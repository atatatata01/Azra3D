// transform_drag.rs — pure relocation from poly_drag_tools.rs
// G/R/S poly sub-element transform.

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
    // Poly Transform (G/R/S on sub-elements)
    // ============================================================

    /// Start a poly sub-element transform (G/R/S).
    pub(crate) fn poly_start_transform(&mut self, kind: TransformKind, mouse_pos: egui::Pos2) {
        // Elongate is SDF-only (parametric prim elongation). On poly it used to silently alias Scale
        // and would translate nonsense if ever routed through compute_transform's Elongate branch
        // (GRS_PARITY_DECISIONS D7 / WORK_ORDER #17).
        if matches!(kind, TransformKind::Elongate) {
            self.toast("Elongate is SDF-only — use Scale (S) on poly");
            return;
        }

        let affected = self.poly_gather_affected_vertices();
        if affected.is_empty() { return; }

        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        self.begin_episode(match kind {
            TransformKind::Grab => "Poly Grab",
            TransformKind::Rotate => "Poly Rotate",
            TransformKind::Scale => "Poly Scale",
            TransformKind::Elongate => "Poly Elongate",
        });

        // Mesh verts are BODY-LOCAL. Camera / compute_transform / gizmo are WORLD.
        // Pivot = poly_selection_center() (WORLD) — ONE function shared with gizmo placement
        // (GRS S1 REUSE; do not hand-roll a second centroid).
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let mut original_positions = Vec::new(); // LOCAL positions (written back via inverse xform)
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                for &vid in &affected {
                    let pos = mesh.vert(VertexId(vid)).position;
                    original_positions.push((vid, pos));
                }
            }
        }
        // Action Center (T pie) pivot for G/R/S.
        // Automatic: user-placed world pivot (Shift+RMB) MUST win when set — do not replace it
        // with the selection centroid (that made placed pivots look "ignored" / tools feel broken).
        // Selection / None: prefer poly selection centroid when sub-elements are selected.
        let frame = self.resolve_action_frame();
        // Action-Centre pivot. The match moved to `poly_prop_edit::poly_transform_pivot` so
        // the proportional-edit overlay can draw the AUTO falloff origin from the SAME
        // resolution — a second copy here would let the ring drift off the deformation.
        let center = self.poly_transform_pivot(frame.center);
        let frozen_orient = if frame.use_orient {
            frame.orient
        } else {
            self.active_rotation_quat()
        };
        let frozen_space = if frame.use_orient {
            crate::TransformSpace::Local
        } else {
            self.tool.transform_space
        };

        let falloff = if self.poly_proportional {
            FalloffKind::Proportional { falloff: self.poly_prop_falloff, radius: self.poly_prop_radius }
        } else {
            FalloffKind::Uniform
        };
        // Proportional influence is measured from the USER-PLACED origin when the viewport
        // handle has been moved, else from the Action Centre (the pre-handle behaviour).
        // Kept apart from `center` on purpose: `center` is the rotate/scale pivot, and the
        // ring must never move what R and S orbit around. `poly_prop_falloff_origin` is the
        // ONE resolver — the overlay draws from it too, so the ring cannot disagree with the
        // maths that actually deforms the mesh.
        let falloff_center = self.poly_prop_falloff_origin().unwrap_or(center);

        let mut all_vert_originals = Vec::new();
        if matches!(falloff, FalloffKind::Proportional { .. }) {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    for i in 0..mesh.vertices.len() {
                        let vid = i as u32;
                        if affected.contains(&vid) { continue; }
                        if mesh.vert(VertexId(vid)).half_edge == NO_EDGE { continue; }
                        all_vert_originals.push((vid, mesh.vert(VertexId(vid)).position));
                    }
                }
            }
        }

        // Build mirror correspondence map when global symmetry is enabled.
        // Plane + verts: reflect in WORLD (plane is world-axis; mesh verts are body-local) — GRS S1 co-fix.
        let mut mirror_pairs = Vec::new();
        let mut mirror_originals = Vec::new();
        let sym = &self.viewport.symmetry;
        if sym.enabled {
            let affected_set: HashSet<u32> = original_positions.iter().map(|&(v, _)| v).collect();
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    let threshold = sym.merge_threshold;
                    for &(vid, pos_local) in &original_positions {
                        let pos_w = world_xform.transform_point3(pos_local);
                        if sym.is_on_seam(pos_w) { continue; }
                        let reflected_w = sym.reflect_point(pos_w);
                        let mut best_dist = threshold * 10.0;
                        let mut best_vid: Option<u32> = None;
                        for i in 0..mesh.vertices.len() {
                            let mv = i as u32;
                            if affected_set.contains(&mv) { continue; }
                            if mesh.vert(VertexId(mv)).half_edge == NO_EDGE { continue; }
                            let mpos_w = world_xform.transform_point3(mesh.vert(VertexId(mv)).position);
                            let d = (mpos_w - reflected_w).length();
                            if d < best_dist {
                                best_dist = d;
                                best_vid = Some(mv);
                            }
                        }
                        if let Some(mv) = best_vid {
                            mirror_pairs.push((vid, mv));
                            mirror_originals.push((mv, mesh.vert(VertexId(mv)).position));
                        }
                    }
                }
            }
        }

        self.start_operation(ActiveTool::PolyTransform(PolyTransformState {
            kind,
            axis: AxisConstraint::None,
            original_positions,
            center,
            falloff_center,
            start_mouse: mouse_pos,
            num_buf: String::new(),
            falloff,
            all_vert_originals,
            confirm_on_release: false,
            mirror_pairs,
            mirror_originals,
            free_mode: false,
            frozen_orient,
            frozen_space,
        }));
    }

    /// Update poly transform from mouse position.
    /// `ctrl` = increment/grid snap (same as SDF gizmo); `shift` = free-move axis lock OR
    /// rotation-angle snap — NOT precision-slow (owner: Shift must not reduce poly edit speed).
    pub(crate) fn poly_update_transform(&mut self, cur: egui::Pos2, shift: bool, ctrl: bool) {
        // [XformPhase] — owner 2026-08-13: 46 ms per frame dragging ONE polygon, with no
        // [XformSplit] line, i.e. writes+normals are under 10 ms and the cost is somewhere ELSE in
        // this function. Clones / snap query / transform maths / post-edit sync are the candidates;
        // time all of them rather than guess a fourth time.
        let _t_fn = std::time::Instant::now();
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };

        let (kind, axis, center, falloff_center, start, originals, all_vert_originals, falloff,
             num_buf, mirror_pairs, free_mode, frozen_orient, frozen_space) = {
            let ts = match self.tool.current.as_poly_transform() { Some(t) => t, None => return };
            (ts.kind, ts.axis, ts.center, ts.falloff_center, ts.start_mouse,
             ts.original_positions.clone(),
             ts.all_vert_originals.clone(), ts.falloff, ts.num_buf.clone(),
             ts.mirror_pairs.clone(), ts.free_mode, ts.frozen_orient, ts.frozen_space)
        };

        // No Shift/Ctrl precision scale on poly edit — full mouse delta always.
        // (SDF gizmo still uses drag_precision_scale; poly Shift is axis-lock only.)
        let dx = cur.x - start.x;
        let num_val: Option<f32> = if num_buf.is_empty() { None } else { num_buf.parse().ok() };

        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();

        // Body transform: math in WORLD, write LOCAL (GRS D6).
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let inv_xform = world_xform.inverse();
        let to_world = |p: Vec3| world_xform.transform_point3(p);
        let to_local = |p: Vec3| inv_xform.transform_point3(p);

        // Snap: Ctrl or magnet toggle = active. Grid = increment when no geometry target.
        // Geometry V/E/F targets = poly mesh + SDF candidates (M9 / GRS D2).
        let snap_active = self.is_snap_active(ctrl);
        let snap_grid = self.effective_snap_grid();
        let snap_rot = self.viewport.snap_rotation_deg.max(0.0);
        let (snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize, snap_angle_lock) =
            self.snap_input_flags();

        // Use unified compute_transform for grab delta (identical feel to SDF/node).
        // `center` is WORLD (set at drag start).
        let cam = self.transform_camera();
        let settings = self.transform_settings();
        // Frozen Action Center frame (T pie) — do not re-read live tool space mid-drag.
        let space = frozen_space;
        let object_rot = frozen_orient;

        // Grab only: teleport selection median onto the nearest V/E/F (or SDF/grid) target.
        // Live query with drag cursor — hover_pos is often None mid-drag (GRS D1).
        // R/S keep increment snap only (angle / scale steps).
        let _ms_setup = _t_fn.elapsed().as_secs_f32() * 1000.0;
        let _t_snap = std::time::Instant::now();
        let snap_pos = if snap_active && matches!(kind, TransformKind::Grab) && snap_affect_move {
            let rect = cam.viewport_rect;
            if rect.width() > 1.0 {
                let hit = self.find_smart_snap(cur, rect, None);
                self.viewport.smart_snap_result = hit.clone();
                hit.map(|h| h.world_pos)
            } else {
                self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos)
            }
        } else {
            None
        };

        let _ms_snap = _t_snap.elapsed().as_secs_f32() * 1000.0;
        let _t_math = std::time::Instant::now();
        let grab_input = crate::transform::TransformInput {
            kind: TransformKind::Grab, axis,
            original_pos: center, original_rot: Vec3::ZERO, original_scale: Vec3::ONE,
            original_elongation: Vec3::ZERO,
            mouse_start: start, mouse_current: cur,
            numeric_value: num_val,
            snap_active, snap_grid, snap_rotation_deg: snap_rot,
            // Shift on free grab = dominant-axis lock (handled below); for Grab, snap_rotation_active
            // also drives free-plane axis clamp inside compute_transform when shift is held.
            snap_rotation_active: shift, snap_pos,
            snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
            snap_angle_lock,
            free_mode,
            elongate_face_negative: false,
            elongate_symmetric: false,
            space,
            object_rot,
            precision_scale: 1.0, // poly: never Shift-slow (axis lock uses `shift` separately)
        };
        let grab_result = crate::transform::compute_transform(&grab_input, &cam, &settings);
        let mut grab_delta = grab_result.position - center; // WORLD delta
        // Shift while free-moving = LOCK to the single world axis the drag points most along.
        if free_mode && shift && axis == AxisConstraint::None {
            let (ax, ay, az) = (grab_delta.x.abs(), grab_delta.y.abs(), grab_delta.z.abs());
            grab_delta = if ax >= ay && ax >= az { Vec3::new(grab_delta.x, 0.0, 0.0) }
                else if ay >= az { Vec3::new(0.0, grab_delta.y, 0.0) }
                else { Vec3::new(0.0, 0.0, grab_delta.z) };
        }
        // Snap SOURCE is the closest selected vertex, not the Action Center. Project
        // that vertex onto the gizmo / Shift lock so X-drag + vertex target aligns
        // the vertex the user is moving, not the median (owner screenshot).
        if let Some(sp) = snap_pos {
            let source = originals
                .iter()
                .map(|&(_, loc)| to_world(loc))
                .min_by(|a, b| (a - sp).length_squared().total_cmp(&(b - sp).length_squared()))
                .unwrap_or(center);
            let (lock, lock_space) = if !matches!(axis, AxisConstraint::None) {
                (axis, space)
            } else if shift && grab_delta.length_squared() > 1e-12 {
                let (ax, ay, az) = (grab_delta.x.abs(), grab_delta.y.abs(), grab_delta.z.abs());
                let a = if ax >= ay && ax >= az {
                    AxisConstraint::X
                } else if ay >= az {
                    AxisConstraint::Y
                } else {
                    AxisConstraint::Z
                };
                (a, crate::TransformSpace::Global)
            } else {
                (AxisConstraint::None, space)
            };
            let snapped = crate::transform::project_snap_along_constraint(
                source,
                sp,
                lock,
                lock_space,
                object_rot,
            );
            grab_delta = snapped - source;
        }

        // Rotation about WORLD axes (view-aware free axis, constraint_axis_world for locked).
        let rot_quat = {
            let mut angle_deg = num_val.unwrap_or(dx * settings.rotate_speed);
            // Ctrl (or Shift as rotation-snap, Blender-style) snaps angle to snap_rotation_deg.
            if (snap_active || shift) && snap_rot > 0.0 && num_val.is_none() {
                angle_deg = (angle_deg / snap_rot).round() * snap_rot;
            }
            let angle_rad = angle_deg.to_radians();
            let rot_axis = match axis {
                AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z
                | AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
                    crate::transform::constraint_axis_world(axis, space, object_rot)
                        .unwrap_or(Vec3::Y)
                }
                AxisConstraint::None => {
                    let ax = cam_fwd.x.abs(); let ay = cam_fwd.y.abs(); let az = cam_fwd.z.abs();
                    if ax >= ay && ax >= az { Vec3::X * cam_fwd.x.signum() }
                    else if ay >= ax && ay >= az { Vec3::Y * cam_fwd.y.signum() }
                    else { Vec3::Z * cam_fwd.z.signum() }
                }
            };
            glam::Quat::from_axis_angle(rot_axis, angle_rad)
        };

        let mut scale_factor = num_val.unwrap_or_else(|| (1.0 + dx * settings.scale_speed).max(0.01));
        if snap_active && num_val.is_none() {
            // Snap scale factor to 0.1 increments when Ctrl held (Blender-ish precision).
            scale_factor = (scale_factor * 10.0).round() / 10.0;
            scale_factor = scale_factor.max(0.01);
        }
        // Plane scale only from explicit Shift+X/Y/Z key constraints (PlaneX/Y/Z).
        let scale_plane_normal: Option<Vec3> = match axis {
            AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
                crate::transform::constraint_axis_world(axis, space, object_rot)
            }
            _ => None,
        };
        // Single-axis scale: X/Y/Z keys, OR free scale + Shift + mouse direction
        // (owner 2026-08-07: Alt free-scale / S free + Shift locks to axis of motion).
        let scale_axis_world: Option<Vec3> = match axis {
            AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z => {
                crate::transform::constraint_axis_world(axis, space, object_rot)
            }
            AxisConstraint::None
                if shift
                    && matches!(kind, TransformKind::Scale | TransformKind::Elongate)
                    && num_val.is_none() =>
            {
                let lock = crate::transform::dominant_axis_constraint_from_mouse(
                    center,
                    start,
                    cur,
                    &cam,
                    space,
                    object_rot,
                );
                crate::transform::constraint_axis_world(lock, space, object_rot)
            }
            _ => None,
        };

        // Apply WORLD transform of one original LOCAL position → new LOCAL.
        let map_vert = |orig_local: Vec3| -> Vec3 {
            let w0 = to_world(orig_local);
            let w1 = match kind {
                TransformKind::Grab => w0 + grab_delta,
                TransformKind::Rotate => center + rot_quat * (w0 - center),
                TransformKind::Scale | TransformKind::Elongate => {
                    let rel = w0 - center;
                    let scaled = if let Some(n) = scale_plane_normal {
                        // 2-axis plane scale: keep component along n, scale in the plane.
                        let n = n.normalize_or_zero();
                        let along = rel.dot(n) * n;
                        let in_plane = rel - along;
                        along + in_plane * scale_factor
                    } else if let Some(aw) = scale_axis_world {
                        // 1-axis scale along aw.
                        let aw = aw.normalize_or_zero();
                        let along = rel.dot(aw) * aw;
                        let perp = rel - along;
                        along * scale_factor + perp
                    } else {
                        // Free uniform scale.
                        rel * scale_factor
                    };
                    center + scaled
                }
            };
            to_local(w1)
        };

        // Symmetry plane is WORLD (axis-set); reflect absolute world positions of moved verts.
        let sym = self.viewport.symmetry.clone();
        let do_mirror = sym.enabled && !mirror_pairs.is_empty();

        // [XformSplit] — `poly_update_transform` measured 477–577 ms for a 161k-vertex grab
        // (owner: 50 ms on a smaller selection, `input=50` with `mesh=0.0`). That is ~3 µs per
        // vertex for what should be "add a delta to a position", so the write loops are almost
        // certainly not the cost. Split writes vs normals rather than guess again.
        let mut _ms_write = 0.0f32;
        let mut _ms_norm = 0.0f32;
        let _ms_math = _t_math.elapsed().as_secs_f32() * 1000.0;
        // P3: single chokepoint (release + make_mut) so Escape / display sync stay honest.
        self.poly_with_base_mesh_mut(nid, |mesh| {
            let _t_w = std::time::Instant::now();
            for &(vid, orig) in &originals {
                mesh.vert_mut(VertexId(vid)).position = map_vert(orig);
            }
            for &(vid, orig) in &all_vert_originals {
                // Falloff radius is in WORLD units (matches gizmo / poly_selection_center).
                // Measured from `falloff_center` (user-placed ring origin, else Action Centre)
                // — NOT from `center`, which stays the rotate/scale pivot.
                let w = falloff.weight(to_world(orig), falloff_center);
                if w < 1e-6 {
                    mesh.vert_mut(VertexId(vid)).position = orig;
                    continue;
                }
                let target = map_vert(orig);
                mesh.vert_mut(VertexId(vid)).position = orig.lerp(target, w);
            }
            // Mirror: WORLD reflect of the source's NEW world position → LOCAL write (GRS S1).
            if do_mirror {
                let src_map: std::collections::HashMap<u32, Vec3> = originals
                    .iter()
                    .map(|&(v, _)| (v, mesh.vert(VertexId(v)).position))
                    .collect();
                for &(src_vid, mir_vid) in &mirror_pairs {
                    if let Some(&new_local) = src_map.get(&src_vid) {
                        let new_w = to_world(new_local);
                        let mir_w = sym.reflect_point(new_w);
                        mesh.vert_mut(VertexId(mir_vid)).position = to_local(mir_w);
                    }
                }
            }
            // Locality spike: normals only for faces incident to moved verts (not full mesh).
            let mut seed: Vec<VertexId> = originals.iter().map(|&(v, _)| VertexId(v)).collect();
            seed.extend(all_vert_originals.iter().map(|&(v, _)| VertexId(v)));
            if do_mirror {
                seed.extend(mirror_pairs.iter().map(|&(_, mir)| VertexId(mir)));
            }
            _ms_write = _t_w.elapsed().as_secs_f32() * 1000.0;
            let _t_n = std::time::Instant::now();
            let n_seed = seed.len();
            mesh.recompute_normals_region(seed);
            _ms_norm = _t_n.elapsed().as_secs_f32() * 1000.0;
            n_seed
        });
        if _ms_write + _ms_norm > 10.0 || crate::perf_spam_on() {
            eprintln!(
                "[XformSplit] {:.1}ms = writes {:.1} + normals {:.1} | moved={} falloff={}",
                _ms_write + _ms_norm, _ms_write, _ms_norm,
                originals.len(), all_vert_originals.len()
            );
        }
        // P3: topology-stable → skip geometry modifier re-eval when stack is empty/normal-only.
        let _t_sync = std::time::Instant::now();
        self.poly_sync_after_position_edit(nid);
        let _ms_sync = _t_sync.elapsed().as_secs_f32() * 1000.0;
        let _total = _t_fn.elapsed().as_secs_f32() * 1000.0;
        if _total > 8.0 || crate::perf_spam_on() {
            eprintln!(
                "[XformPhase] {:.1}ms = setup {:.1} + snap {:.1} + math {:.1} + mutate {:.1}                  + sync {:.1} | moved={} falloff={}",
                _total, _ms_setup, _ms_snap, _ms_math,
                _ms_write + _ms_norm, _ms_sync, originals.len(), all_vert_originals.len()
            );
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Cancel poly transform — restore original vertex positions from snapshot.
    pub(crate) fn poly_cancel_transform(&mut self) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => {
                self.tool.current = ActiveTool::Idle;
                return;
            }
        };
        if let Some(ts) = self.tool.current.as_poly_transform() {
            let originals = ts.original_positions.clone();
            let all_vert_originals = ts.all_vert_originals.clone();
            let mirror_originals = ts.mirror_originals.clone();
            // MUST release display arcs first — with a Mirror (etc.) modifier the base Arc can
            // stay unique and the poly_sync early-out would leave the viewport on the dragged shape.
            self.poly_with_base_mesh_mut(nid, |mesh| {
                for (vid, orig) in &originals {
                    mesh.vert_mut(VertexId(*vid)).position = *orig;
                }
                for (vid, orig) in &all_vert_originals {
                    mesh.vert_mut(VertexId(*vid)).position = *orig;
                }
                for (vid, orig) in &mirror_originals {
                    mesh.vert_mut(VertexId(*vid)).position = *orig;
                }
                let mut seed: Vec<VertexId> = originals.iter().map(|(v, _)| VertexId(*v)).collect();
                seed.extend(all_vert_originals.iter().map(|(v, _)| VertexId(*v)));
                seed.extend(mirror_originals.iter().map(|(v, _)| VertexId(*v)));
                mesh.recompute_normals_region(seed);
            });
        }
        self.cancel_episode();
        self.tool.current = ActiveTool::Idle;
        self.poly_sync_mesh_cache(nid);
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Confirm poly transform — keep vertex positions, clear tool state.
    ///
    /// No-op confirm (G/R/S then Enter without moving, or values that leave verts at originals)
    /// cancels the episode so Ctrl+Z is not wasted (same honesty as zero extrude/inset/slide).
    pub(crate) fn poly_confirm_transform(&mut self) {
        let no_op = match self.tool.current.as_poly_transform() {
            None => true,
            Some(ts) => {
                let Some(nid) = self.sel.usel.active_node() else {
                    // Selection lost mid-modal — cancel, don't commit a stray episode.
                    self.poly_cancel_transform();
                    return;
                };
                match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::PolygonalBody { mesh, .. }) => {
                        let eps2 = 1e-12f32;
                        let same = |vid: u32, orig: Vec3| {
                            (vid as usize) < mesh.vertices.len()
                                && (mesh.vert(VertexId(vid)).position - orig).length_squared() < eps2
                        };
                        ts.original_positions.iter().all(|&(v, o)| same(v, o))
                            && ts.all_vert_originals.iter().all(|&(v, o)| same(v, o))
                            && ts.mirror_originals.iter().all(|&(v, o)| same(v, o))
                    }
                    _ => true,
                }
            }
        };
        if no_op {
            self.poly_cancel_transform();
            return;
        }
        self.commit_episode();
        self.poly_flush_mesh_dirty();
        self.tool.current = ActiveTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Set axis constraint on active poly transform.
    pub(crate) fn poly_transform_set_axis(&mut self, axis: AxisConstraint) {
        if let Some(ts) = self.tool.current.as_poly_transform_mut() {
            ts.axis = axis;
        }
    }

    /// Append a character to the numeric input buffer of the active poly transform.
    pub(crate) fn poly_transform_num_input(&mut self, ch: char) {
        if let Some(ts) = self.tool.current.as_poly_transform_mut() {
            if ch == '\x08' {
                ts.num_buf.pop();
            } else {
                ts.num_buf.push(ch);
            }
        }
    }
}
