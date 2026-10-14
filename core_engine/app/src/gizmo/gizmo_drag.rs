//! Pro-gizmo drag: start / update / finish / cancel (multi-target).
//! Pure relocation from gizmo.rs (degod slice 25).

use eframe::egui;
use glam::Vec3;

/// When true, prim.position is body-local; body-level transforms only update
/// body.origin / body_rotation / body_scale — no prim mutations, no rebake.
/// Must stay in sync with the same flag in body_volume_cache.rs, serialize.rs,
/// and project_load.rs.
use crate::BODY_LOCAL_PRIMS;

use crate::{
    FourthLayerApp, ActiveTool, ToolMode, PolyTool, TransformKind, TransformState, AxisConstraint,
    GizmoMode, GizmoComponent, GizmoInteraction, TransformSnapshot, DirtyFlags,
    WorkMode, SceneItemId,
};
use crate::transform::{
    compute_transform, TransformResult, TransformInput, euler_yxz_deg_to_quat, quat_to_euler_yxz_deg,
    apply_orbit_around_pivot,
};
use crate::TransformSpace;

impl FourthLayerApp {
    // ================================================================
    // PRO GIZMO: Start / Update / Finish / Cancel (multi-target)
    // ================================================================
    pub(crate) fn start_gizmo_drag(&mut self, comp: GizmoComponent, mouse_pos: egui::Pos2, shift: bool) {
        // gizmo_armed stays true — gizmo re-appears after drag finishes

        // Light gizmo: Translate + Rotate + Scale (same G/R/S arm path as prims).
        // Scale drives the ATTENUATION RADIUS (owner 2026-08-07) — lights have no
        // spatial scale, but their range is exactly what "make it bigger" means.
        // Gate on BOTH selection forms (`selected_light_ids`) and on "no NON-light node"
        // (`selected_transform_nodes`): a light picked in the outliner is a `SceneItemId::Node`,
        // so `has_lights()` was false and `has_nodes()` true — this arm was skipped and the
        // generic node-gizmo path below wrote `local_scale` / `local_position` on the Light node,
        // fields nothing reads. G/R/S looked armed and did nothing at all.
        let light_ids = self.selected_light_ids();
        if !light_ids.is_empty()
            && !self.sel.usel.has_prims()
            && self.selected_transform_nodes().is_empty()
        {
            if matches!(self.tool.gizmo_mode, GizmoMode::Elongate) {
                self.toast("Elongate not supported for lights");
                return;
            }
            use crate::LightTransformSnapshot;
            let light_snaps: Vec<LightTransformSnapshot> = light_ids.into_iter()
                .filter_map(|lid| self.scene.light_by_id(lid).map(|d| (lid, d.clone())))
                .filter(|(_, d)| !d.locked)
                .map(|(lid, d)| LightTransformSnapshot {
                    light_id: lid,
                    original_pos: d.position,
                    original_azimuth: d.azimuth,
                    original_elevation: d.elevation,
                    original_range: d.range,
                })
                .collect();
            if light_snaps.is_empty() { self.toast("Light(s) locked"); return; }
            let desc = match self.tool.gizmo_mode {
                GizmoMode::Rotate => "Gizmo Rotate Light(s)",
                GizmoMode::Scale => "Gizmo Scale Light Radius",
                _ => "Gizmo Move Light(s)",
            };
            self.begin_episode(desc);
            self.start_operation(ActiveTool::Gizmo(GizmoInteraction {
                component: comp, mode: self.tool.gizmo_mode, space: self.action_frame_space(),
                drag_start_mouse: mouse_pos, median_start_pos: self.resolve_action_frame().center,
                targets: vec![], node_targets: vec![], light_targets: light_snaps,
                pre_drag_aabbs: vec![], shift_duplicate: false,
                elongate_face_negative: self.tool.elongate_face_negative,
                orient: self.action_frame_orient(),
            }));
            return;
        }

        // Poly sub-element gizmo: delegate to poly transform
        if self.modeling_context == crate::ModelingContext::Polygonal
            && self.poly_work_mode != crate::PolyWorkMode::Object
            && self.poly_selection_center().is_some()
        {
            let kind = match self.tool.gizmo_mode {
                GizmoMode::Translate => TransformKind::Grab,
                GizmoMode::Rotate => TransformKind::Rotate,
                GizmoMode::Scale => TransformKind::Scale,
                GizmoMode::Elongate => TransformKind::Elongate,
            };
            self.poly_start_transform(kind, mouse_pos);
            // Set axis constraint from gizmo component
            if let Some(ts) = self.tool.current.as_poly_transform_mut() {
                ts.axis = match comp {
                    GizmoComponent::AxisX => AxisConstraint::X,
                    GizmoComponent::AxisY => AxisConstraint::Y,
                    GizmoComponent::AxisZ => AxisConstraint::Z,
                    GizmoComponent::Center => AxisConstraint::None,
                };
                // Center handle = free Scale/Grab (Shift + mouse dir → axis lock).
                ts.free_mode = matches!(comp, GizmoComponent::Center)
                    && matches!(
                        self.tool.gizmo_mode,
                        GizmoMode::Translate | GizmoMode::Scale
                    );
                ts.confirm_on_release = true;
            }
            return;
        }

        // NURBS sub-element gizmo — same path as poly: start transform + axis from handle
        if self.modeling_context == crate::ModelingContext::Nurbs
            && self.nurbs_selection_center().is_some()
        {
            let kind = match self.tool.gizmo_mode {
                GizmoMode::Translate => TransformKind::Grab,
                GizmoMode::Rotate => TransformKind::Rotate,
                GizmoMode::Scale => TransformKind::Scale,
                GizmoMode::Elongate => TransformKind::Scale,
            };
            self.nurbs_start_transform(kind, mouse_pos);
            if let Some(d) = self.tool.current.as_nurbs_cv_move_mut() {
                d.axis = match comp {
                    GizmoComponent::AxisX => AxisConstraint::X,
                    GizmoComponent::AxisY => AxisConstraint::Y,
                    GizmoComponent::AxisZ => AxisConstraint::Z,
                    GizmoComponent::Center => AxisConstraint::None,
                };
                d.free_mode = matches!(comp, GizmoComponent::Center)
                    && matches!(
                        self.tool.gizmo_mode,
                        GizmoMode::Translate | GizmoMode::Scale
                    );
                d.confirm_on_release = true;
            }
            return;
        }

        // Scene graph node gizmo (single or multi-select). Lights excluded — a mixed
        // light+body selection transforms the body only; the light node's own transform is dead
        // state (`selected_transform_nodes`).
        let sel_nodes: Vec<_> = self.selected_transform_nodes();
        if !sel_nodes.is_empty() {
            // Check if any selected node is an SdfBody — route through prim targets instead
            let has_sdf_body = sel_nodes.iter().any(|&nid| {
                self.scene.scene_graph.get(nid)
                    .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                    .unwrap_or(false)
            });

            if has_sdf_body {
                let desc = match self.tool.gizmo_mode { GizmoMode::Translate=>"Gizmo Move", GizmoMode::Rotate=>"Gizmo Rotate", GizmoMode::Scale=>"Gizmo Scale", GizmoMode::Elongate=>"Gizmo Elongate" };
                // BODY_LOCAL_PRIMS: route through node_targets so apply_to_scene_node
                // updates body.origin / body_rotation / body_scale (no prim mutations, no rebake).
                if BODY_LOCAL_PRIMS {
                    let mut node_targets: Vec<crate::NodeGizmoTarget> = Vec::new();
                    for &nid in &sel_nodes {
                        if let Some(n) = self.scene.scene_graph.get(nid) {
                            if n.locked { continue; }
                            let (pos, rot, scl, orient) = if let core_math::scene_graph::NodePayload::SdfBody { origin, body_rotation, body_scale, .. } = &n.payload {
                                // Convert body_rotation Quat → euler degrees (YXZ) for TransformResult compatibility
                                let (y, x, z) = body_rotation.to_euler(glam::EulerRot::YXZ);
                                let rot_deg = Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                                (*origin, rot_deg, *body_scale, *body_rotation)
                            } else {
                                let r = n.local_rotation * (std::f32::consts::PI / 180.0);
                                let o = glam::Quat::from_euler(glam::EulerRot::YXZ, r.y, r.x, r.z);
                                (n.local_position, n.local_rotation, n.local_scale, o)
                            };
                            node_targets.push(crate::NodeGizmoTarget {
                                node_id: nid,
                                original_pos: pos,
                                original_rot: rot,
                                original_scale: scl,
                                original_orient: orient,
                            });
                        }
                    }
                    if node_targets.is_empty() { self.toast("All selected objects are locked"); return; }
                    self.begin_episode(desc);
                    self.start_operation(ActiveTool::Gizmo(GizmoInteraction{
                        component:comp, mode:self.tool.gizmo_mode, space:self.action_frame_space(),
                        drag_start_mouse:mouse_pos, median_start_pos:self.resolve_action_frame().center,
                        targets: vec![], node_targets, light_targets: vec![],
                        pre_drag_aabbs: vec![], shift_duplicate: false,
                        elongate_face_negative: self.tool.elongate_face_negative,
                        orient: self.action_frame_orient(),
                    }));
                } else {
                    // Prim-based: resolve body prims — selection is NOT mutated.
                    // BODY_LOCAL_PRIMS R/S: snapshots use world positions (see prim_world_pos).
                    let resolved = self.resolve_transform_targets();
                    if resolved.prim_targets.is_empty() { self.toast("All selected objects are locked"); return; }
                    // BODY_LOCAL_PRIMS: convert local positions to world for compute_transform.
                    let prim_targets = if BODY_LOCAL_PRIMS {
                        resolved.prim_targets.into_iter().map(|mut t| {
                            if let Some(flat_idx) = self.prim_index_by_uid(t.prim_uid) {
                                // Convert local prim position to world using body transform.
                                let world_pos = if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(flat_idx) {
                                    let bt = self.scene.scene_graph.body_world_transform(body_nid)
                                        .unwrap_or_default();
                                    bt.local_to_world(self.scene.primitives[flat_idx].position)
                                } else {
                                    self.scene.primitives[flat_idx].position
                                };
                                t.original_pos = world_pos;
                            }
                            t
                        }).collect()
                    } else {
                        resolved.prim_targets
                    };
                    self.begin_episode(desc);
                    self.start_operation(ActiveTool::Gizmo(GizmoInteraction{
                        component:comp, mode:self.tool.gizmo_mode, space:self.action_frame_space(),
                        drag_start_mouse:mouse_pos, median_start_pos:self.resolve_action_frame().center,
                        targets: prim_targets, node_targets: vec![], light_targets: vec![],
                        pre_drag_aabbs: vec![], shift_duplicate: false,
                        elongate_face_negative: self.tool.elongate_face_negative,
                        orient: self.action_frame_orient(),
                    }));
                }
                return;
            } else {
                // Non-SdfBody nodes — existing node_targets path
                let desc = match self.tool.gizmo_mode { GizmoMode::Translate=>"Gizmo Move Node", GizmoMode::Rotate=>"Gizmo Rotate Node", GizmoMode::Scale=>"Gizmo Scale Node", GizmoMode::Elongate=>"Gizmo Elongate Node" };
                let mut node_targets: Vec<crate::NodeGizmoTarget> = Vec::new();
                for &nid in &sel_nodes {
                    if let Some(n) = self.scene.scene_graph.get(nid) {
                        if n.locked { continue; }
                        {
                            let r = n.local_rotation * (std::f32::consts::PI / 180.0);
                            let o = glam::Quat::from_euler(glam::EulerRot::YXZ, r.y, r.x, r.z);
                            node_targets.push(crate::NodeGizmoTarget {
                                node_id: nid,
                                original_pos: n.local_position,
                                original_rot: n.local_rotation,
                                original_scale: n.local_scale,
                                original_orient: o,
                            });
                        }
                    }
                }
                if node_targets.is_empty() { self.toast("All selected nodes are locked"); return; }
                self.begin_episode(desc);
                self.start_operation(ActiveTool::Gizmo(GizmoInteraction{
                    component:comp, mode:self.tool.gizmo_mode, space:self.action_frame_space(),
                    drag_start_mouse:mouse_pos, median_start_pos:self.resolve_action_frame().center,
                    targets: vec![], node_targets, light_targets: vec![],
                    pre_drag_aabbs: vec![], shift_duplicate: false,
                    elongate_face_negative: self.tool.elongate_face_negative,
                    orient: self.action_frame_orient(),
                }));
                return;
            }
        }

        // Multi-prim SDF gizmo
        let indices: Vec<usize> = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        // Shift+D session already duplicated — don't re-dup, don't nest a second episode.
        let in_array_sess = self.tool.array_dup.is_some();
        let shift_dup = !in_array_sess
            && shift
            && indices
                .iter()
                .any(|&i| i < self.scene.primitives.len() && !self.scene.primitives[i].locked);
        let desc = if shift_dup {
            match self.tool.gizmo_mode { GizmoMode::Translate=>"Gizmo Duplicate + Move", GizmoMode::Rotate=>"Gizmo Duplicate + Rotate", GizmoMode::Scale=>"Gizmo Duplicate + Scale", GizmoMode::Elongate=>"Gizmo Duplicate + Elongate" }
        } else {
            match self.tool.gizmo_mode { GizmoMode::Translate=>"Gizmo Move", GizmoMode::Rotate=>"Gizmo Rotate", GizmoMode::Scale=>"Gizmo Scale", GizmoMode::Elongate=>"Gizmo Elongate" }
        };
        // Full undo — skip when Shift+D session already holds the outer episode.
        if !in_array_sess {
            self.begin_episode(desc);
        }

        // Shift+Drag: duplicate selected prims, then drag the duplicates
        if shift_dup {
            let mut new_uids: Vec<u32> = Vec::new();
            for &i in &indices {
                if i < self.scene.primitives.len() && !self.scene.primitives[i].locked {
                    if let Some((_new_idx, uid)) = self.duplicate_prim_for_transform(i) {
                        new_uids.push(uid);
                    }
                }
            }
            // Select the duplicates
            self.clear_selection_unified();
            for &uid in &new_uids {
                self.toggle_select_unified(SceneItemId::Prim(uid));
            }
        }

        // Build targets from current selection (duplicates if shift, originals otherwise)
        let indices: Vec<usize> = self.selected_prim_indices_unified();
        let targets: Vec<TransformSnapshot> = indices.iter()
            .filter(|&&i| i<self.scene.primitives.len() && !self.scene.primitives[i].locked)
            .map(|&i| {
                // Snapshots always store world-space positions so compute_transform math is correct.
                let world_pos = if BODY_LOCAL_PRIMS {
                    if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(i) {
                        let bt = self.scene.scene_graph.body_world_transform(body_nid).unwrap_or_default();
                        bt.local_to_world(self.scene.primitives[i].position)
                    } else {
                        self.scene.primitives[i].position
                    }
                } else {
                    self.scene.primitives[i].position
                };
                TransformSnapshot{prim_uid:self.scene.primitives[i].prim_uid, original_pos:world_pos, original_rot:self.scene.primitives[i].rotation_deg, original_scale:self.scene.primitives[i].scale, original_elongation:self.scene.primitives[i].elongation, original_orient:self.prim_world_orient(i)}
            })
            .collect();
        if targets.is_empty() { self.toast("All selected objects are locked"); return; }
        self.start_operation(ActiveTool::Gizmo(GizmoInteraction{
            component:comp, mode:self.tool.gizmo_mode, space:self.action_frame_space(),
            drag_start_mouse:mouse_pos, median_start_pos:self.resolve_action_frame().center, targets,
            node_targets: vec![], light_targets: vec![],
            pre_drag_aabbs: vec![], shift_duplicate: shift_dup,
            elongate_face_negative: self.tool.elongate_face_negative,
            orient: self.action_frame_orient(),
        }));
    }

    pub(crate) fn update_gizmo_drag(&mut self, cur: egui::Pos2, _rect: egui::Rect, ctrl: bool, shift: bool, alt: bool) {
        // =====================================================================
        // WARNING: ALL transform math lives in compute_transform() in transform.rs.
        // Do NOT add grab/rotate/scale calculations here.
        // To support a new target: add handling below. The math is SHARED.
        // If you duplicate the math, future you will hate past you.
        // =====================================================================
        let gi = match self.tool.current.as_gizmo() { Some(g)=>g, None=>return };
        let comp = gi.component;
        let mode = gi.mode;
        let space = gi.space; // frozen at drag start
        let start = gi.drag_start_mouse;
        let median_pos = gi.median_start_pos;
        let targets = gi.targets.clone();
        let node_targets = gi.node_targets.clone();
        let light_targets = gi.light_targets.clone();
        let elongate_neg = gi.elongate_face_negative;
        // Alt+Elongate: opposite face moves at same world rate as grab (symmetric).
        let elongate_sym = alt && mode == GizmoMode::Elongate;
        // Shift = slower drag; Ctrl+Shift = fine precision (held while dragging).
        let precision = crate::transform::drag_precision_scale(shift, ctrl);

        // Map gizmo component to axis constraint
        let axis = match comp {
            GizmoComponent::AxisX => AxisConstraint::X,
            GizmoComponent::AxisY => AxisConstraint::Y,
            GizmoComponent::AxisZ => AxisConstraint::Z,
            GizmoComponent::Center => AxisConstraint::None,
        };

        // Map gizmo mode to transform kind
        let kind = match mode {
            GizmoMode::Translate => TransformKind::Grab,
            GizmoMode::Rotate => TransformKind::Rotate,
            GizmoMode::Scale => TransformKind::Scale,
            GizmoMode::Elongate => TransformKind::Elongate,
        };

        // Center handle = free transform: Grab plane / Scale uniform / Rotate view-axis spin.
        // free_mode on Rotate is intentional free-aim (cam-forward spin), not a bug.
        let free_mode = matches!(comp, GizmoComponent::Center)
            && matches!(
                mode,
                GizmoMode::Translate | GizmoMode::Scale | GizmoMode::Rotate
            );

        let cam = self.transform_camera();
        let settings = self.transform_settings();
        let snap_active = self.is_snap_active(ctrl);
        // Live geometry snap under the cursor while dragging (Ctrl or magnet). Same as poly
        // modal G: do not rely on the previous overlay frame's result only.
        // Exclude nothing by default — user aims at a vertex/edge to land the constrained move.
        let snap_pos = if snap_active && matches!(kind, TransformKind::Grab) {
            let rect = cam.viewport_rect;
            if rect.width() > 1.0 {
                let hit = self.find_smart_snap(cur, rect, None);
                self.viewport.smart_snap_result = hit.clone();
                hit.map(|h| h.world_pos)
            } else {
                self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos)
            }
        } else {
            self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos)
        };
        let (snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize, snap_angle_lock) =
            self.snap_input_flags();

        // --- Apply to light targets (Translate + Rotate) ---
        // Directional (sun): R only changes aim (az/el) — position stays fixed (Blender sun).
        // G may still re-place the viewport marker; lighting ignores sun position.
        if !light_targets.is_empty() {
            match mode {
                GizmoMode::Scale => {
                    // Scale = attenuation radius (owner 2026-08-07: "S should modify
                    // the radius of the light"). Drag right = grow, left = shrink —
                    // 100px per 1×, precision-scaled like the other modes. The
                    // selected-light range sphere gizmo gives live feedback.
                    let cur_p = crate::transform::precision_scale_mouse(start, cur, precision);
                    let factor = (1.0 + (cur_p.x - start.x) * 0.01).max(0.01);
                    for snap in &light_targets {
                        let lid = snap.light_id;
                        let r = (snap.original_range * factor).clamp(0.05, 500.0);
                        self.write_light(lid, |l| l.range = r);
                    }
                }
                GizmoMode::Rotate => {
                    let cur = crate::transform::precision_scale_mouse(start, cur, precision);
                    let dx = cur.x - start.x;
                    let dy = cur.y - start.y;
                    for snap in &light_targets {
                        let lid = snap.light_id;
                        let az = snap.original_azimuth + dx * self.tweak.light_azimuth_speed;
                        let el = (snap.original_elevation - dy * self.tweak.light_elevation_speed)
                            .clamp(-90.0, 90.0);
                        // Never write position on rotate — sun icon stays put.
                        self.write_light(lid, |l| {
                            l.azimuth = az;
                            l.elevation = el;
                        });
                    }
                }
                _ => {
                    // Translate: free/axis move via compute_transform.
                    let first = &light_targets[0];
                    let input = TransformInput {
                        kind: TransformKind::Grab,
                        axis,
                        original_pos: first.original_pos,
                        original_rot: Vec3::ZERO,
                        original_scale: Vec3::ONE,
                        original_elongation: Vec3::ZERO,
                        mouse_start: start,
                        mouse_current: cur,
                        numeric_value: None,
                        snap_active,
                        snap_grid: self.effective_snap_grid(),
                        snap_rotation_deg: self.viewport.snap_rotation_deg,
                        snap_rotation_active: shift,
                        snap_pos,
                        snap_affect_move,
                        snap_affect_rotate,
                        snap_affect_scale,
                        snap_grid_quantize,
                        snap_angle_lock,
                        free_mode,
                        elongate_face_negative: false,
                        elongate_symmetric: false,
                        space: TransformSpace::Global,
                        object_rot: glam::Quat::IDENTITY,
                        precision_scale: precision,
                    };
                    let result = compute_transform(&input, &cam, &settings);
                    let delta = result.position - first.original_pos;
                    for snap in &light_targets {
                        let lid = snap.light_id;
                        let pos = snap.original_pos + delta;
                        self.write_light(lid, |l| l.position = pos);
                    }
                }
            }
            self.mark_dirty(DirtyFlags::LIGHT);
            return;
        }

        // --- Apply to scene graph node targets ---
        if !node_targets.is_empty() {
            let multi_body = node_targets.len() > 1;
            // Multi-object R: ONE shared rotation delta about the selection pivot.
            // Per-target compute_transform used each origin as the mouse-angle pivot and each
            // orient for Local axes → different angles / opposite turns (owner: two poly
            // objects rotate different directions).
            let shared_rotate_dq = if multi_body && kind == TransformKind::Rotate {
                let first = &node_targets[0];
                let input = TransformInput {
                    kind,
                    axis,
                    original_pos: median_pos, // shared pivot for angular measure
                    original_rot: first.original_rot,
                    original_scale: first.original_scale,
                    original_elongation: Vec3::ZERO,
                    mouse_start: start,
                    mouse_current: cur,
                    numeric_value: None,
                    snap_active,
                    snap_grid: self.effective_snap_grid(),
                    snap_rotation_deg: self.viewport.snap_rotation_deg,
                    snap_rotation_active: shift,
                    snap_pos,
                    snap_affect_move,
                    snap_affect_rotate,
                    snap_affect_scale,
                    snap_grid_quantize,
                    snap_angle_lock,
                    free_mode,
                    elongate_face_negative: false,
                    elongate_symmetric: false,
                    space,
                    // Active/first object axes for Local (same for every target).
                    object_rot: first.original_orient,
                    precision_scale: precision,
                };
                let r = compute_transform(&input, &cam, &settings);
                let q0 = first.original_orient;
                let q1 = euler_yxz_deg_to_quat(r.rotation);
                Some((q1 * q0.inverse()).normalize())
            } else {
                None
            };

            for nt in &node_targets {
                let mut result = if let Some(dq) = shared_rotate_dq {
                    let q1 = (dq * nt.original_orient).normalize();
                    let offset = nt.original_pos - median_pos;
                    TransformResult {
                        position: median_pos + dq * offset,
                        rotation: quat_to_euler_yxz_deg(q1),
                        scale: nt.original_scale,
                        elongation: Vec3::ZERO,
                    }
                } else {
                    let input = TransformInput {
                        kind, axis,
                        original_pos: nt.original_pos, original_rot: nt.original_rot, original_scale: nt.original_scale,
                        original_elongation: Vec3::ZERO,
                        mouse_start: start, mouse_current: cur,
                        numeric_value: None,
                        snap_active, snap_grid: self.effective_snap_grid(), snap_rotation_deg: self.viewport.snap_rotation_deg,
                        snap_rotation_active: shift, snap_pos,
                        snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
                        snap_angle_lock,
                        free_mode,
                        elongate_face_negative: false,
                        elongate_symmetric: false,
                        space,
                        object_rot: nt.original_orient,
                        precision_scale: precision,
                    };
                    let mut result = compute_transform(&input, &cam, &settings);

                    // Orbit origin around the Action Center / Custom pivot (single AND
                    // multi). Without this, T=Pivot + gizmo R spun around the object
                    // origin while the rings sat on the face pivot (owner screenshots).
                    apply_orbit_around_pivot(
                        kind,
                        nt.original_pos,
                        median_pos,
                        nt.original_orient,
                        nt.original_scale,
                        &mut result,
                    );
                    result
                };

                // Align Rotation to Target (Blender): while Grab-snapping onto a surface/face.
                if matches!(kind, TransformKind::Grab) {
                    self.apply_snap_align_rotation(snap_active, &mut result);
                }

                self.apply_to_scene_node(nt.node_id, &result);
            }
        } else {
            // --- Apply to SDF primitive targets ---
            // Get body pivot for orbit/scale if a body is selected
            let body_pivot = self.sel.usel.active_node().and_then(|nid| {
                self.scene.scene_graph.get(nid).and_then(|n| {
                    if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &n.payload {
                        Some(*origin)
                    } else { None }
                })
            });

            // Multi-selection Grab snap: shared world delta from the first target with
            // FULL snap+axis (compute_transform projects geometry snap onto the gizmo lock).
            // Old path used `snap_pos - mouse_pos` unconstrained → Ctrl snap left the axis
            // (owner: snap to a vertex while still constrained by the handle).
            let shared_grab_delta = if matches!(kind, TransformKind::Grab)
                && snap_active
                && snap_affect_move
                && !targets.is_empty()
            {
                let first = &targets[0];
                let first_input = TransformInput {
                    kind,
                    axis,
                    original_pos: first.original_pos,
                    original_rot: first.original_rot,
                    original_scale: first.original_scale,
                    original_elongation: first.original_elongation,
                    mouse_start: start,
                    mouse_current: cur,
                    numeric_value: None,
                    snap_active,
                    snap_grid: self.effective_snap_grid(),
                    snap_rotation_deg: self.viewport.snap_rotation_deg,
                    snap_rotation_active: shift,
                    snap_pos,
                    snap_affect_move,
                    snap_affect_rotate,
                    snap_affect_scale,
                    snap_grid_quantize,
                    snap_angle_lock,
                    free_mode,
                    elongate_face_negative: elongate_neg,
                    elongate_symmetric: elongate_sym,
                    space,
                    object_rot: first.original_orient,
                    precision_scale: precision,
                };
                let first_result = compute_transform(&first_input, &cam, &settings);
                Some(first_result.position - first.original_pos)
            } else {
                None
            };

            for snap in &targets {
                let idx = match self.prim_index_by_uid(snap.prim_uid) { Some(i) => i, None => continue };
                let input = TransformInput {
                    kind, axis,
                    original_pos: snap.original_pos, original_rot: snap.original_rot, original_scale: snap.original_scale,
                    original_elongation: snap.original_elongation,
                    mouse_start: start, mouse_current: cur,
                    numeric_value: None,
                    // Per-target: snap already baked into shared_grab_delta for Grab.
                    // Non-Grab still uses Shift rotation snap only here.
                    snap_active: false, snap_grid: 0.0, snap_rotation_deg: self.viewport.snap_rotation_deg,
                    snap_rotation_active: shift, snap_pos: None,
                    snap_affect_move: true, snap_affect_rotate: true, snap_affect_scale: true, snap_grid_quantize: false,
                    snap_angle_lock: false,
                    free_mode,
                    elongate_face_negative: elongate_neg,
                    elongate_symmetric: elongate_sym,
                    space,
                    object_rot: snap.original_orient,
                    precision_scale: precision,
                };
                let mut result = compute_transform(&input, &cam, &settings);
                if let Some(delta) = shared_grab_delta {
                    // Axis-aware world delta from first target (includes constrained geometry snap).
                    result.position = snap.original_pos + delta;
                }

                // Align Rotation to Target while Grab-snapping (shared normal from smart_snap_result).
                if matches!(kind, TransformKind::Grab) {
                    self.apply_snap_align_rotation(snap_active, &mut result);
                }

                // Body selected: orbit/pivot-scale handles position; keep per-prim rotation unchanged
                if let Some(pivot) = body_pivot {
                    let offset = snap.original_pos - pivot;
                    if kind == TransformKind::Rotate {
                        let q0 = snap.original_orient;
                        let q1 = euler_yxz_deg_to_quat(result.rotation);
                        let dq = (q1 * q0.inverse()).normalize();
                        result.position = pivot + dq * offset;
                    } else if kind == TransformKind::Scale {
                        const EPS: f32 = 1e-7;
                        let safe_scale = snap.original_scale.abs().max(Vec3::splat(EPS));
                        let safe_result = result.scale.abs().max(Vec3::splat(EPS)) * result.scale.signum();
                        let scale_ratio = safe_result / safe_scale;
                        if scale_ratio.is_finite() {
                            result.position = pivot + offset * scale_ratio;
                        } else {
                            eprintln!("[gizmo] scale_ratio NaN/Inf — skipping pivot transform");
                        }
                    }
                }

                // Elongate (asymmetric face extrude): compute_transform already applied the recentering
                // position offset so the OPPOSITE face stays fixed — keep result.position as computed.
                // (Resetting it to original here would re-center and make the extrude symmetric again.)
                self.apply_to_sdf_prim(idx, &result);
            }
            // Shift+D array: keep intermediate copies evenly spaced while dragging.
            if self.tool.array_dup.is_some() {
                self.array_dup_update_positions();
            }
            // Body origin update deferred to finish_gizmo_drag (same reason as single-target path).
        }
        // Prim buffer re-upload for real-time SDF surface update during drag.
        // SVO bake is already skipped during active drags (process_dirty guard).
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn finish_gizmo_drag(&mut self) {
        // No-op gizmo release (grab axis then release without moving) cancels the episode so
        // Ctrl+Z is not wasted — same honesty class as confirm_modal_transform / poly G/R/S.
        if self.gizmo_drag_is_noop() {
            self.cancel_gizmo_drag();
            return;
        }
        // Array session holds the outer episode until Enter/M/Esc — don't commit here.
        if self.tool.array_dup.is_none() {
            self.commit_episode();
        }
        // Light gizmo: mark LIGHT dirty instead of rebaking SDF
        if let Some(gi) = self.tool.current.as_gizmo() {
            if !gi.light_targets.is_empty() {
                self.tool.current = ActiveTool::Idle;
                self.mark_dirty(DirtyFlags::LIGHT);
                return;
            }
        }
        // Body-root-only move (node_targets, no prim targets):
        // SDF is unchanged in body-local space — skip rebake entirely.
        // The raster mesh is valid, just repositioned via instance transform.
        // Don't update body origin from prim centroid (origin was already set
        // correctly by apply_to_scene_node to the gizmo destination).
        let node_only = if let Some(gi) = self.tool.current.as_gizmo() {
            !gi.node_targets.is_empty() && gi.targets.is_empty()
        } else { false };
        if node_only {
            self.tool.current = ActiveTool::Idle;
            self.viewport.snap_indicator = None;
            self.mark_dirty(DirtyFlags::RENDER);
            return;
        }
        // Prim targets: rebake at current positions. Body origin stays where it was —
        // origin only changes on membership change (add/remove/reparent), not prim move.
        self.tool.current = ActiveTool::Idle; self.viewport.snap_indicator=None; self.rebake_no_undo();
    }

    pub(crate) fn cancel_gizmo_drag(&mut self) {
        // Shift+D array session: cancel whole session (not just this drag).
        if self.tool.array_dup.is_some() {
            self.tool.current = ActiveTool::Idle;
            self.array_dup_cancel();
            return;
        }
        // Shift-duplicate cancel: restore full scene graph from undo (removes all duplicates)
        if let Some(gi) = self.tool.current.as_gizmo() {
            if gi.shift_duplicate {
                self.restore_undo_no_redo();
                self.tool.current = ActiveTool::Idle;
                self.rebake_no_undo();
                return;
            }
        }
        if let Some(gi)=self.tool.current.as_gizmo() {
            // Clone targets before mutable borrow
            let node_targets_copy = gi.node_targets.clone();
            let prim_targets_copy = gi.targets.clone();
            let light_targets_copy = gi.light_targets.clone();
            // Restore light targets
            for snap in &light_targets_copy {
                let lid = snap.light_id;
                let pos = snap.original_pos;
                let az = snap.original_azimuth;
                let el = snap.original_elevation;
                let rg = snap.original_range; // S-scale cancel restores the radius
                self.write_light(lid, |l| { l.position = pos; l.azimuth = az; l.elevation = el; l.range = rg; });
            }
            if !light_targets_copy.is_empty() {
                self.mark_dirty(DirtyFlags::LIGHT);
                self.tool.current = ActiveTool::Idle; self.cancel_episode();
                return;
            }
            // Restore node targets (SdfBody-aware: restores origin + prim positions)
            for nt in &node_targets_copy {
                let result = TransformResult { position: nt.original_pos, rotation: nt.original_rot, scale: nt.original_scale, elongation: Vec3::ZERO };
                self.apply_to_scene_node(nt.node_id, &result);
            }
            // Restore primitive targets (flat cache + graph sync).
            // prim_targets use world-space original_pos (converted in start_gizmo_drag).
            // Must convert back world→local before restoring.
            for s in &prim_targets_copy {
                if let Some(idx) = self.prim_index_by_uid(s.prim_uid) {
                    let local_pos = if BODY_LOCAL_PRIMS {
                        if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                            let bt = self.scene.scene_graph.body_world_transform(body_nid)
                                .unwrap_or_default();
                            bt.world_to_local(s.original_pos)
                        } else {
                            s.original_pos
                        }
                    } else {
                        s.original_pos
                    };
                    self.write_prim_transform(idx, local_pos, s.original_rot, s.original_scale);
                }
            }
        }
        self.tool.current = ActiveTool::Idle; self.viewport.snap_indicator=None; self.cancel_episode(); self.rebake_no_undo();
    }

    /// Recompute the active body's origin from the centroid of prims.
    /// Works for both direct prim selection AND body-node selection.
    /// NOTE: No longer called from finish_gizmo_drag — origin only changes on
    /// membership change (add/remove/reparent). Kept for potential future use
    /// (e.g. explicit "Recenter Pivot" command).
    #[allow(dead_code)]
    fn update_body_origin_from_selection(&mut self) {
        // Try direct prim selection first
        let mut sel_indices = self.selected_prim_indices_unified();
        let mut body_nid = None;

        if sel_indices.is_empty() {
            // Body-node selection: resolve all prims in the selected body
            if let Some(nid) = self.sel.usel.active_node() {
                let is_sdf = self.scene.scene_graph.get(nid)
                    .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                    .unwrap_or(false);
                if is_sdf {
                    body_nid = Some(nid);
                    for (flat_idx, &(bnid, _)) in self.scene.prim_body_map.iter().enumerate() {
                        if bnid == nid && flat_idx < self.scene.primitives.len() {
                            sel_indices.push(flat_idx);
                        }
                    }
                }
            }
        }

        if sel_indices.is_empty() { return; }

        // Find which body owns the prims
        let target_body = body_nid.unwrap_or_else(|| {
            let first_idx = sel_indices[0];
            if first_idx < self.scene.prim_body_map.len() { self.scene.prim_body_map[first_idx].0 }
            else { self.scene.active_body_nid }
        });

        if BODY_LOCAL_PRIMS {
            // BODY_LOCAL_PRIMS: prim.position is body-local.
            // Compute the centroid of local positions, then convert to world-space
            // to get the new body.origin. Then adjust local positions so that prim
            // world positions remain unchanged after the origin shift.
            let transform = match self.scene.scene_graph.body_world_transform(target_body) {
                Some(t) => t,
                None => return,
            };
            let old_origin = transform.origin;

            let mut local_centroid = Vec3::ZERO;
            let mut count = 0;
            for &i in &sel_indices {
                if i < self.scene.primitives.len() {
                    local_centroid += self.scene.primitives[i].position;
                    count += 1;
                }
            }
            if count == 0 { return; }
            local_centroid /= count as f32;

            // New origin is the world-space centroid
            let new_origin = transform.local_to_world(local_centroid);
            let origin_shift_world = new_origin - old_origin;

            if origin_shift_world.length() < 1e-6 { return; }

            // Shift all prims in this body by -origin_shift in local space so their
            // world positions remain unchanged.
            // For simplicity (rotation=identity, scale=1 common case) and correctness:
            // local_shift = rotation.inverse() * (-origin_shift_world) / scale
            let local_shift = transform.rotation.inverse() * (-origin_shift_world) / transform.scale;

            // Collect all prim flat indices for this body
            let all_body_indices: Vec<usize> = self.scene.prim_body_map.iter().enumerate()
                .filter(|(_, &(bnid, _))| bnid == target_body)
                .map(|(fi, _)| fi)
                .filter(|&fi| fi < self.scene.primitives.len())
                .collect();

            // Apply shift to graph + flat cache for each prim
            eprintln!("[ConfirmRebase] body={:?} reason=update_body_origin_from_selection prim_count={}",
                target_body, all_body_indices.len());
            for flat_idx in all_body_indices {
                let new_pos = self.scene.primitives[flat_idx].position + local_shift;
                let rot = self.scene.primitives[flat_idx].rotation_deg;
                let scl = self.scene.primitives[flat_idx].scale;
                self.write_prim_transform(flat_idx, new_pos, rot, scl);
            }

            // Update body origin in graph
            if let Some(node) = self.scene.scene_graph.get_mut(target_body) {
                if let core_math::scene_graph::NodePayload::SdfBody { ref mut origin, .. } = node.payload {
                    *origin = new_origin;
                }
            }

            // Translate cached DC mesh vertices + grid bounds by local_shift so
            // the stale mesh stays at the correct world position until the next
            // re-extraction.  Without this, the old vertices rendered with the
            // new origin instance matrix shift by origin_shift_world.
            self.cache.body_vol_cache.translate_body_cache(target_body, local_shift);

            self.cache.prim_buf_stale = true;
        } else {
            // Legacy world-space prim mode: centroid IS the world-space origin.
            let mut centroid = Vec3::ZERO;
            let mut count = 0;
            for &i in &sel_indices {
                if i < self.scene.primitives.len() {
                    centroid += self.scene.primitives[i].position;
                    count += 1;
                }
            }
            if count == 0 { return; }
            centroid /= count as f32;

            // Update body origin
            if let Some(node) = self.scene.scene_graph.get_mut(target_body) {
                if let core_math::scene_graph::NodePayload::SdfBody { ref mut origin, .. } = node.payload {
                    *origin = centroid;
                }
            }
        }
    }
}
