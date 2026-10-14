//! Modal G/R/S transform + array helpers + noop snapshot matchers.
//! Pure relocation from gizmo.rs (degod slices 24 + shared noop helpers).

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
use crate::transform::{compute_transform, TransformResult, TransformInput, euler_yxz_deg_to_quat};
use crate::TransformSpace;

impl FourthLayerApp {
    // ---- Modal Transform (G/R/S) ----
    pub(crate) fn start_modal_transform(&mut self, kind:TransformKind, mouse_pos:egui::Pos2, shift:bool) {
        if let Some(idx)=self.active_prim_index_unified() { if idx<self.scene.primitives.len() {
            if self.scene.primitives[idx].locked { self.toast("Object is locked"); return; }

            // Shift+Transform: duplicate selected prims first, then transform the duplicates
            // (same grab/gizmo math path as G — ActiveTool::Transform + compute_transform).
            let shift_dup = shift && !self.scene.primitives[idx].locked;
            let dup_origin_local = self.scene.primitives[idx].position; // body-local pos of original
            let array_source_uid = if shift_dup {
                Some(self.scene.primitives[idx].prim_uid)
            } else {
                None
            };

            let desc = if shift_dup {
                match kind { TransformKind::Grab=>format!("Duplicate + Move"),TransformKind::Rotate=>format!("Duplicate + Rotate"),TransformKind::Scale=>format!("Duplicate + Scale"),TransformKind::Elongate=>format!("Duplicate + Elongate") }
            } else {
                let n=self.scene.primitives[idx].name.clone();
                match kind{TransformKind::Grab=>format!("Move {}",n),TransformKind::Rotate=>format!("Rotate {}",n),TransformKind::Scale=>format!("Scale {}",n),TransformKind::Elongate=>format!("Elongate {}",n)}
            };

            // Full undo — captures scene graph (body.origin) + prims atomically.
            // Must be saved BEFORE duplicating so cancel can restore original state.
            self.begin_episode(&desc);

            // If shift-duplicate: create duplicates and select them instead
            if shift_dup {
                let indices: Vec<usize> = self.selected_prim_indices_unified();
                let mut new_uids: Vec<u32> = Vec::new();
                for &i in &indices {
                    if i < self.scene.primitives.len() && !self.scene.primitives[i].locked {
                        if let Some((_new_idx, uid)) = self.duplicate_prim_for_transform(i) {
                            new_uids.push(uid);
                        }
                    }
                }
                // Select the duplicates (original prims stay in place, unselected)
                self.clear_selection_unified();
                for &uid in &new_uids {
                    self.toggle_select_unified(SceneItemId::Prim(uid));
                }
            }

            // Build targets from current selection (duplicates if shift, originals otherwise)
            let indices: Vec<usize> = self.selected_prim_indices_unified();
            let idx = if let Some(i) = self.active_prim_index_unified() { i } else { return; };
            if idx >= self.scene.primitives.len() { return; }
            let r=self.scene.primitives[idx].rotation_deg; let s=self.scene.primitives[idx].scale;
            let p = if BODY_LOCAL_PRIMS {
                if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                    let bt = self.scene.scene_graph.body_world_transform(body_nid).unwrap_or_default();
                    bt.local_to_world(self.scene.primitives[idx].position)
                } else { self.scene.primitives[idx].position }
            } else { self.scene.primitives[idx].position };
            let multi: Vec<TransformSnapshot> = indices.iter()
                .filter(|&&i| i < self.scene.primitives.len() && !self.scene.primitives[i].locked)
                .map(|&i| {
                    let world_pos = if BODY_LOCAL_PRIMS {
                        if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(i) {
                            let bt = self.scene.scene_graph.body_world_transform(body_nid).unwrap_or_default();
                            bt.local_to_world(self.scene.primitives[i].position)
                        } else { self.scene.primitives[i].position }
                    } else { self.scene.primitives[i].position };
                    TransformSnapshot { prim_uid: self.scene.primitives[i].prim_uid, original_pos: world_pos, original_rot: self.scene.primitives[i].rotation_deg, original_scale: self.scene.primitives[i].scale, original_elongation: self.scene.primitives[i].elongation, original_orient: self.prim_world_orient(i) }
                })
                .collect();
            let frame = self.resolve_action_frame();
            let frozen_space = self.action_frame_space();
            let frozen_orient = self.action_frame_orient();
            // Action Center pivot for R/S; Grab still uses prim position as start for delta.
            let pivot = if matches!(kind, TransformKind::Rotate | TransformKind::Scale) {
                frame.center
            } else {
                p
            };
            // Linear array (Ctrl+Shift+D / shift_dup Grab): free workplane move until click.
            // free_mode → workplane plane; Shift → lock to dominant of the two plane axes.
            let free_mode = shift_dup && kind == TransformKind::Grab;
            self.start_operation(ActiveTool::Transform(TransformState{
                kind, axis: AxisConstraint::None,
                original_pos: pivot, original_rot: r, original_scale: s,
                start_mouse: mouse_pos, num_buf: String::new(),
                node_target: None, confirm_on_release: false, free_mode,
                multi_targets: multi, light_targets: Vec::new(),
                space: frozen_space, object_orient: frozen_orient,
                shift_duplicate: shift_dup, array_count: 1, array_prim_uids: Vec::new(),
                dup_origin_local,
                array_source_uid,
                array_scroll_accum: 0.0, array_circle: false,
            }));
            if free_mode {
                self.toast(
                    "Array: workplane move · Shift=axis lock · scroll/Shift+scroll=count · C=circle · LMB confirm",
                );
            }
        }}
        // Scene graph node modal transform (poly/NURBS/imported bodies).
        // A Light node is NOT one of these: its transform lives in `LightData`, so it must fall
        // through to the light arm below. Without the filter an outliner-selected light started a
        // node transform that wrote `local_position` / `local_scale` — fields no renderer, packer
        // or gizmo reads, so G/R/S appeared armed and moved nothing.
        else if let Some(nid) = self.sel.usel.active_node().filter(|&n| !self.is_light_node(n)) {
            let node_data = self.scene.scene_graph.get(nid).map(|n| {
                let (pos, body_rot, body_scl, orient) = if let core_math::scene_graph::NodePayload::SdfBody { origin, body_rotation, body_scale, .. } = &n.payload {
                    // Convert body_rotation Quat → euler degrees (YXZ) for TransformResult compatibility
                    let (y, x, z) = body_rotation.to_euler(glam::EulerRot::YXZ);
                    (*origin, Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees()), *body_scale, *body_rotation)
                } else {
                    let r = n.local_rotation * (std::f32::consts::PI / 180.0);
                    let o = glam::Quat::from_euler(glam::EulerRot::YXZ, r.y, r.x, r.z);
                    (n.local_position, n.local_rotation, n.local_scale, o)
                };
                (n.locked, pos, body_rot, body_scl, orient,
                 matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
            });
            if let Some((locked, pos, rot, scale, orient, is_sdf_body)) = node_data {
                if locked { self.toast("Node is locked"); return; }
                let desc = match kind { TransformKind::Grab=>"Move Body", TransformKind::Rotate=>"Rotate Body", TransformKind::Scale=>"Scale Body", TransformKind::Elongate=>"Elongate Body" };
                let frozen_space = self.action_frame_space();

                if is_sdf_body {
                    if BODY_LOCAL_PRIMS {
                        // BODY_LOCAL_PRIMS: update body.origin / body_rotation / body_scale
                        // via node_target. No prim mutations, no rebake needed.
                        self.begin_episode(desc);
                        self.start_operation(ActiveTool::Transform(TransformState{
                            kind, axis: AxisConstraint::None,
                            original_pos: pos, original_rot: rot, original_scale: scale,
                            start_mouse: mouse_pos, num_buf: String::new(),
                            node_target: Some(nid),
                            confirm_on_release: false, free_mode: false,
                            multi_targets: Vec::new(), light_targets: Vec::new(),
                            space: frozen_space, object_orient: orient,
                            shift_duplicate:false,array_count:1,array_prim_uids:Vec::new(),dup_origin_local:Vec3::ZERO,
                            array_source_uid: None, array_scroll_accum: 0.0, array_circle: false,
                        }));
                    } else {
                        // Legacy prim-based path: resolve body prims as multi_targets.
                        let resolved = self.resolve_transform_targets();
                        let prim_targets = resolved.prim_targets;
                        self.begin_episode(desc);
                        // node_target: None — let body pivot code handle R/S via multi_targets
                        self.start_operation(ActiveTool::Transform(TransformState{
                            kind, axis: AxisConstraint::None,
                            original_pos: pos, original_rot: rot, original_scale: scale,
                            start_mouse: mouse_pos, num_buf: String::new(),
                            node_target: None, confirm_on_release: false, free_mode: false,
                            multi_targets: prim_targets, light_targets: Vec::new(),
                            space: frozen_space, object_orient: orient,
                            shift_duplicate: false, array_count: 1, array_prim_uids: Vec::new(),
                            dup_origin_local: Vec3::ZERO,
                            array_source_uid: None, array_scroll_accum: 0.0, array_circle: false,
                        }));
                    }
                } else {
                    // Non-SdfBody (poly / NURBS / import): freeze Action Center pivot for R/S
                    // so Object-mode rotation orbits geometry center / custom pivot — not only
                    // the transform origin (owner: "pivot Geometry still uses origin").
                    let frame = self.resolve_action_frame();
                    let pivot = if matches!(kind, TransformKind::Rotate | TransformKind::Scale) {
                        frame.center
                    } else {
                        pos
                    };
                    self.begin_episode(desc);
                    self.start_operation(ActiveTool::Transform(TransformState{
                        kind, axis: AxisConstraint::None,
                        // Object origin at drag start (written back for Grab; orbit base for R/S).
                        original_pos: pos, original_rot: rot, original_scale: scale,
                        start_mouse: mouse_pos, num_buf: String::new(),
                        node_target: Some(nid), confirm_on_release: false, free_mode: false,
                        multi_targets: Vec::new(), light_targets: Vec::new(),
                        space: frozen_space, object_orient: orient,
                        shift_duplicate: false, array_count: 1, array_prim_uids: Vec::new(),
                        // Frozen R/S pivot (geometry / Action Center). Unused for Grab.
                        dup_origin_local: pivot,
                        array_source_uid: None, array_scroll_accum: 0.0, array_circle: false,
                    }));
                }
            }
        }
        // Light modal transform (G/R/S — S scales the ATTENUATION RADIUS, owner 2026-08-07).
        // Both selection forms: viewport pick = `SceneItemId::Light`, outliner row = Node.
        else if !self.selected_light_ids().is_empty() {
            if kind == TransformKind::Elongate {
                self.toast("Elongate not supported for lights");
                return;
            }
            use crate::LightTransformSnapshot;
            let light_snaps: Vec<LightTransformSnapshot> = self.selected_light_ids().into_iter()
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
            // Use first snapshot for TransformState originals (pivot for single light)
            let first = &light_snaps[0];
            let pos = first.original_pos;
            let rot = glam::Vec3::new(first.original_azimuth, first.original_elevation, 0.0);
            let desc = match kind {
                TransformKind::Grab => "Move Light(s)",
                TransformKind::Rotate => "Rotate Light(s)",
                TransformKind::Scale => "Scale Light(s)",
                TransformKind::Elongate => "Elongate Light(s)",
            };
            self.begin_episode(desc);
            // Grab = free view-plane move (follows mouse). Rotate keeps free_mode false
            // so azimuth/elevation drag from mouse delta works as before.
            let free = kind == TransformKind::Grab;
            self.start_operation(ActiveTool::Transform(TransformState {
                kind,
                axis: AxisConstraint::None,
                original_pos: pos,
                original_rot: rot,
                original_scale: glam::Vec3::ONE,
                start_mouse: mouse_pos,
                num_buf: String::new(),
                node_target: None,
                confirm_on_release: false,
                free_mode: free,
                multi_targets: Vec::new(),
                light_targets: light_snaps,
                space: TransformSpace::Global,
                object_orient: glam::Quat::IDENTITY,
                shift_duplicate: false, array_count: 1, array_prim_uids: Vec::new(), dup_origin_local: Vec3::ZERO,
                array_source_uid: None, array_scroll_accum: 0.0, array_circle: false,
            }));
        }
    }
    pub(crate) fn cancel_modal_transform(&mut self) {
        // Linear-array / shift-duplicate cancel: restore pre-dup snapshot.
        if let Some(ts) = self.tool.current.as_transform() {
            if ts.shift_duplicate {
                // Unhide source if circle mode hid it (episode restore also covers this).
                if let Some(suid) = ts.array_source_uid {
                    if let Some(sidx) = self.prim_index_by_uid(suid) {
                        self.scene.primitives[sidx].visible = true;
                        self.mirror_prim_to_graph(sidx);
                    }
                }
                self.tool.current = ActiveTool::Idle;
                self.viewport.snap_indicator = None;
                // Prefer pending episode (held open while array modal is live).
                if let Some(entry) = self.pending_episode.take() {
                    if let app_types::UndoPayload::Full {
                        scene_graph_snapshot: Some(sg),
                        ..
                    } = entry.payload
                    {
                        self.scene.scene_graph = sg;
                        self.sync_flat_cache();
                    }
                    self.scene.active_body_nid = entry.active_body_nid;
                    self.sel.usel = entry.usel;
                    self.history.mark_prims_dirty();
                    if self.cache.use_body_vol_cache {
                        self.cache
                            .body_vol_cache
                            .invalidate_for_undo(&self.scene.scene_graph);
                    }
                    self.cache.body_vol_cache.suppress_bake_dispatch = false;
                    self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH);
                } else {
                    self.restore_undo_no_redo();
                }
                self.rebake_no_undo();
                self.restore_pre_grab_state();
                self.toast("Linear array cancelled");
                return;
            }
        }
        // Light cancel (multi-light aware)
        if let Some(ts) = self.tool.current.as_transform() {
            if !ts.light_targets.is_empty() {
                let snaps = ts.light_targets.clone();
                for snap in &snaps {
                    let lid = snap.light_id;
                    let pos = snap.original_pos;
                    let az = snap.original_azimuth;
                    let el = snap.original_elevation;
                    let rg = snap.original_range; // S-scale cancel restores the radius
                    self.write_light(lid, |l| { l.position = pos; l.azimuth = az; l.elevation = el; l.range = rg; });
                }
                self.mark_dirty(DirtyFlags::LIGHT);
                self.cancel_episode(); self.tool.current = ActiveTool::Idle; return;
            }
        }
        // Scene graph node cancel (SdfBody-aware: restores origin + prim positions)
        if let Some(ts) = self.tool.current.as_transform() {
            if let Some(nid) = ts.node_target {
                let result = TransformResult { position: ts.original_pos, rotation: ts.original_rot, scale: ts.original_scale, elongation: Vec3::ZERO };
                self.apply_to_scene_node(nid, &result);
                self.cancel_episode(); self.tool.current = ActiveTool::Idle; self.mark_dirty(DirtyFlags::RENDER); return;
            }
        }
        if let Some(ts) = self.tool.current.as_transform() {
            if !ts.multi_targets.is_empty() {
                // multi_targets use world-space original_pos (we converted local→world in start).
                // Must convert back world→local before restoring.
                for snap in &ts.multi_targets.clone() {
                    if let Some(idx) = self.prim_index_by_uid(snap.prim_uid) {
                        let local_pos = if BODY_LOCAL_PRIMS {
                            if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                                let bt = self.scene.scene_graph.body_world_transform(body_nid)
                                    .unwrap_or_default();
                                bt.world_to_local(snap.original_pos)
                            } else {
                                snap.original_pos
                            }
                        } else {
                            snap.original_pos
                        };
                        self.write_prim_transform(idx, local_pos, snap.original_rot, snap.original_scale);
                    }
                }
                self.rebake_no_undo();
            } else if let Some(idx) = self.active_prim_index_unified() {
                if idx < self.scene.primitives.len() {
                    // original_pos is world-space (converted in start_modal_transform).
                    // Must convert back to body-local before restoring.
                    let local_pos = if BODY_LOCAL_PRIMS {
                        if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                            let bt = self.scene.scene_graph.body_world_transform(body_nid)
                                .unwrap_or_default();
                            bt.world_to_local(ts.original_pos)
                        } else { ts.original_pos }
                    } else { ts.original_pos };
                    self.write_prim_transform(idx, local_pos, ts.original_rot, ts.original_scale);
                    self.rebake_no_undo();
                }
            }
        }
        self.cancel_episode(); self.tool.current = ActiveTool::Idle; self.viewport.snap_indicator=None;
        self.restore_pre_grab_state();
    }
    /// True when the modal G/R/S left lights/prims/nodes at their episode originals.
    /// Shift-duplicate always creates geometry → never treated as a no-op.
    fn modal_transform_is_noop(&self) -> bool {
        let Some(ts) = self.tool.current.as_transform() else { return true };
        if ts.shift_duplicate { return false; }
        const EPS2: f32 = 1e-10;

        if !ts.light_targets.is_empty() {
            return ts.light_targets.iter().all(|snap| self.light_snapshot_matches(snap));
        }

        if let Some(nid) = ts.node_target {
            return self.node_transform_matches_original(nid, ts.original_pos, ts.original_rot, ts.original_scale);
        }

        if !ts.multi_targets.is_empty() {
            return ts.multi_targets.iter().all(|snap| self.prim_snapshot_matches(snap));
        }

        // Single-prim fallback (empty multi_targets, no node/light).
        if let Some(idx) = self.active_prim_index_unified() {
            if idx < self.scene.primitives.len() {
                let world = self.prim_world_position(idx);
                let p = &self.scene.primitives[idx];
                return (world - ts.original_pos).length_squared() < EPS2
                    && (p.rotation_deg - ts.original_rot).length_squared() < EPS2
                    && (p.scale - ts.original_scale).length_squared() < EPS2;
            }
        }
        true
    }

    fn prim_world_position(&self, idx: usize) -> Vec3 {
        if BODY_LOCAL_PRIMS {
            if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                let bt = self.scene.scene_graph.body_world_transform(body_nid).unwrap_or_default();
                return bt.local_to_world(self.scene.primitives[idx].position);
            }
        }
        self.scene.primitives[idx].position
    }

    /// One light vs its pre-drag snapshot — THE light no-op test, shared by
    /// `modal_transform_is_noop` (G/R/S + Enter) and `gizmo_drag_is_noop` (handle release).
    /// Both used to inline their own copy and both compared position / azimuth / elevation ONLY.
    ///
    /// `range` MUST be part of it: S on a light scales the ATTENUATION RADIUS and touches
    /// nothing else, so a pure radius change read as "nothing moved" → the confirm path called
    /// `cancel_*`, and cancel restores `original_range`. The radius therefore snapped back to its
    /// pre-drag value the instant the mouse released — live during the drag, gone on commit.
    /// A no-op test that ignores a field the drag writes cancels real work; keeping ONE matcher
    /// means the next light field added to `LightTransformSnapshot` is fixed in one place.
    fn light_snapshot_matches(&self, snap: &crate::LightTransformSnapshot) -> bool {
        const EPS: f32 = 1e-5;
        const EPS2: f32 = 1e-10;
        self.scene.light_by_id(snap.light_id).map(|d| {
            (d.position - snap.original_pos).length_squared() < EPS2
                && (d.azimuth - snap.original_azimuth).abs() < EPS
                && (d.elevation - snap.original_elevation).abs() < EPS
                && (d.range - snap.original_range).abs() < EPS
        }).unwrap_or(true)
    }

    fn prim_snapshot_matches(&self, snap: &TransformSnapshot) -> bool {
        const EPS2: f32 = 1e-10;
        let Some(idx) = self.prim_index_by_uid(snap.prim_uid) else { return true };
        if idx >= self.scene.primitives.len() { return true; }
        let p = &self.scene.primitives[idx];
        let world = self.prim_world_position(idx);
        (world - snap.original_pos).length_squared() < EPS2
            && (p.rotation_deg - snap.original_rot).length_squared() < EPS2
            && (p.scale - snap.original_scale).length_squared() < EPS2
            && (p.elongation - snap.original_elongation).length_squared() < EPS2
    }

    fn node_transform_matches_original(
        &self,
        nid: core_math::scene_graph::NodeId,
        pos: Vec3,
        rot: Vec3,
        scale: Vec3,
    ) -> bool {
        const EPS2: f32 = 1e-10;
        let Some(n) = self.scene.scene_graph.get(nid) else { return true };
        let (cpos, crot, cscl) = match &n.payload {
            core_math::scene_graph::NodePayload::SdfBody {
                origin, body_rotation, body_scale, ..
            } => {
                let (y, x, z) = body_rotation.to_euler(glam::EulerRot::YXZ);
                (
                    *origin,
                    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees()),
                    *body_scale,
                )
            }
            _ => (n.local_position, n.local_rotation, n.local_scale),
        };
        (cpos - pos).length_squared() < EPS2
            && (crot - rot).length_squared() < EPS2
            && (cscl - scale).length_squared() < EPS2
    }

    /// True when the gizmo drag left all targets at their pre-drag snapshots.
    /// Shift-duplicate always creates geometry → never a no-op.
    pub(crate) fn gizmo_drag_is_noop(&self) -> bool {
        let Some(gi) = self.tool.current.as_gizmo() else { return true };
        if gi.shift_duplicate { return false; }

        if !gi.light_targets.is_empty() {
            return gi.light_targets.iter().all(|snap| self.light_snapshot_matches(snap));
        }

        let nodes_same = gi.node_targets.iter().all(|nt| {
            self.node_transform_matches_original(nt.node_id, nt.original_pos, nt.original_rot, nt.original_scale)
        });
        let prims_same = gi.targets.iter().all(|snap| self.prim_snapshot_matches(snap));
        nodes_same && prims_same
    }

    pub(crate) fn confirm_modal_transform(&mut self) {
        // No-op confirm (G/R/S then Enter without moving) cancels the episode so Ctrl+Z is not
        // wasted — same honesty class as poly_confirm_transform / zero extrude.
        if self.modal_transform_is_noop() {
            self.cancel_modal_transform();
            return;
        }
        let was_shift_dup = self.tool.current.as_transform()
            .map_or(false, |ts| ts.shift_duplicate);
        // Circle array: keep source hidden at center (empty center). Episode commits as-is.
        // Linear: source stays visible at origin.
        self.commit_episode();
        // Light confirm (multi-light aware)
        if let Some(ts) = self.tool.current.as_transform() {
            if !ts.light_targets.is_empty() {
                self.tool.current = ActiveTool::Idle; self.mark_dirty(DirtyFlags::LIGHT); return;
            }
        }
        // A node_target transform only edits the node's transform fields
        // (SdfBody origin/rotation/scale, or local_position/rotation/scale for
        // poly/NURBS/ImportedMesh). It NEVER mutates SDF prims, so the SDF
        // rebake path below must be skipped. This MUST mirror finish_gizmo_drag's
        // node_only branch — otherwise the gizmo and modal paths rot independently
        // (CLAUDE.md: two code paths for the same operation must share behaviour).
        //
        // Critically: the node-transform branch must mark DirtyFlags::RENDER so the
        // viewport re-renders the moved mesh on the confirm frame. Without it, an
        // ImportedMesh (voxel) node transform left the viewport frozen until the
        // next camera move re-armed the render gate (the "surface disappears until
        // I move the camera" bug).
        let is_node_transform = self.tool.current.as_transform()
            .map_or(false, |ts| ts.node_target.is_some());

        self.tool.current = ActiveTool::Idle; self.viewport.snap_indicator=None;
        if is_node_transform {
            // Node moved: mesh is valid in body-local space, only the instance
            // matrix changed. Re-render, no rebake.
            self.mark_dirty(DirtyFlags::RENDER);
        } else {
            // Recalculate body origin after prim move to keep AABB tight.
            // Without this, moving a prim expands the body-local AABB because
            // the origin stays at the old centroid. Recentering compensates all
            // prim positions so their world positions are unchanged, but the
            // body-local AABB stays tight around the geometry.
            // Note: the episode was already committed above. The recentering
            // changes body-local positions without changing world positions —
            // this is a derived recalculation, not a user mutation. The undo
            // entry captures the original drag delta; recentering is recomputed
            // on undo/redo via the next rebake cycle.
            let active = self.scene.active_body_nid;
            self.recalculate_body_origin(active);
            self.sync_flat_cache();
            self.rebake_no_undo();
        }
        // After Shift+D confirm: arm translate gizmo so fine placement reuses the
        // same gizmo path as a normal grab (axis handles, not only free-mouse modal).
        if was_shift_dup {
            self.tool.gizmo_mode = GizmoMode::Translate;
            self.tool.gizmo_armed = true;
        }
        self.restore_pre_grab_state();
    }

    /// M during array modal: collapse live copies into a non-destructive Array modifier
    /// on the source. Linear or circular (if C was pressed).
    pub(crate) fn transform_array_make_modifier(&mut self) {
        let (source_uid, array_uids, multi_uids, origin_local, array_count, circle) = {
            let Some(ts) = self.tool.current.as_transform() else { return };
            if !ts.shift_duplicate {
                return;
            }
            let Some(src) = ts.array_source_uid else {
                self.toast("No source prim for array → modifier");
                return;
            };
            let multi: Vec<u32> = ts.multi_targets.iter().map(|t| t.prim_uid).collect();
            (
                src,
                ts.array_prim_uids.clone(),
                multi,
                ts.dup_origin_local,
                ts.array_count.max(1),
                ts.array_circle,
            )
        };
        let Some(&primary_uid) = multi_uids.first() else { return };
        let Some(pidx) = self.prim_index_by_uid(primary_uid) else { return };
        let dragged = self.scene.primitives[pidx].position;

        // Remove temporary live copies (primary + intermediates).
        let mut to_remove: Vec<u32> = array_uids;
        to_remove.extend_from_slice(&multi_uids);
        let mut idxs: Vec<usize> = to_remove
            .iter()
            .filter_map(|u| self.prim_index_by_uid(*u))
            .collect();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        for idx in idxs {
            self.remove_prim_from_graph(idx);
        }

        let Some(src_idx) = self.prim_index_by_uid(source_uid) else {
            self.cancel_modal_transform();
            return;
        };

        // Ensure source is at the array center/origin and visible.
        self.scene.primitives[src_idx].position = origin_local;
        self.scene.primitives[src_idx].visible = true;

        let arr = if circle {
            // Circular: empty center at origin; radius_vec = first copy offset on workplane.
            let plane_n = self.viewport.wp_resolved.normal.normalize_or_zero();
            let mut radius_vec = dragged - origin_local;
            radius_vec = radius_vec - plane_n * radius_vec.dot(plane_n);
            if radius_vec.length() < 0.5 {
                // Default ring radius if user didn't drag far.
                let u = self.viewport.wp_resolved.tangent_u.normalize_or_zero();
                radius_vec = u * 1.5;
            }
            // Rotation axis = workplane normal → dominant world axis.
            let an = plane_n.abs();
            let axis = if an.y >= an.x && an.y >= an.z {
                0u8 // Y
            } else if an.x >= an.z {
                1u8 // X
            } else {
                2u8 // Z
            };
            let count = array_count.max(2);
            core_math::modifiers::Modifier::array_circular(count, radius_vec, axis)
        } else {
            // Linear: count includes original + end + intermediates ≈ array_count + 1.
            let total = (array_count + 1).max(2);
            let step = (dragged - origin_local) / array_count.max(1) as f32;
            core_math::modifiers::Modifier::array_linear(total, step)
        };

        let count_show = arr.strength.round() as u32;
        let mods = &mut self.scene.primitives[src_idx].modifiers;
        if let Some(existing) = mods
            .iter_mut()
            .find(|m| m.mod_type == core_math::primitives::ModifierType::Array)
        {
            *existing = arr;
        } else {
            mods.push(arr);
        }
        self.scene.primitives[src_idx].sync_modifiers_to_gpu_fields();
        self.mirror_prim_to_graph(src_idx);

        self.commit_episode();
        self.tool.current = ActiveTool::Idle;
        self.viewport.snap_indicator = None;
        self.select_single_unified(SceneItemId::Prim(source_uid));
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.rebake_no_undo();
        if circle {
            self.toast(&format!(
                "Circle Array modifier ×{count_show} · edit in Modifiers panel"
            ));
        } else {
            self.toast(&format!(
                "Linear Array modifier ×{count_show} · edit in Modifiers panel"
            ));
        }
        self.restore_pre_grab_state();
    }

    /// Scroll wheel during linear/circle array: adjust count.
    /// Works with or without Shift held (Shift is axis-lock for free grab, not scroll).
    /// Pure accumulation only — one physical wheel notch ≈ one step (no discrete fast-path;
    /// that was ~10× too sensitive with Shift+scroll / raw deltas).
    pub(crate) fn transform_array_scroll(&mut self, sc: f32) {
        let ts = match self.tool.current.as_transform_mut() {
            Some(t) if t.shift_duplicate => t,
            _ => return,
        };
        // One mouse-wheel notch is ~40–50 on Windows smooth_scroll; require a full notch.
        const NOTCH: f32 = 55.0;
        ts.array_scroll_accum += sc;
        let steps = (ts.array_scroll_accum / NOTCH).trunc() as i32;
        if steps == 0 {
            return;
        }
        ts.array_scroll_accum -= steps as f32 * NOTCH;
        // Cap to ±1 step per event so a huge delta never jumps by 10.
        let steps = steps.clamp(-1, 1);
        let old_count = ts.array_count;
        if steps > 0 {
            ts.array_count = (ts.array_count + 1).min(50);
        } else {
            ts.array_count = ts.array_count.saturating_sub(1).max(1);
        }
        if ts.array_count == old_count {
            return;
        }

        self.transform_array_rebuild_copies();
        let (count, circle) = self
            .tool
            .current
            .as_transform()
            .map(|t| (t.array_count, t.array_circle))
            .unwrap_or((1, false));
        let mode = if circle { "Circle" } else { "Linear" };
        self.toast(&format!(
            "{mode} array ×{count}  (scroll=count · C=circle · LMB confirm)"
        ));
        self.mark_dirty(DirtyFlags::SCENE);
    }

    /// Grow/shrink intermediate prim list to match array_count, then re-layout.
    fn transform_array_rebuild_copies(&mut self) {
        let (array_count, dragged_uid) = {
            let Some(ts) = self.tool.current.as_transform() else { return };
            if !ts.shift_duplicate {
                return;
            }
            let uid = ts.multi_targets.first().map(|t| t.prim_uid);
            (ts.array_count.max(1), uid)
        };
        let Some(dragged_uid) = dragged_uid else { return };
        let Some(source_idx) = self.prim_index_by_uid(dragged_uid) else { return };

        let mut array_uids = self
            .tool
            .current
            .as_transform_mut()
            .map(|t| std::mem::take(&mut t.array_prim_uids))
            .unwrap_or_default();
        let needed = (array_count as usize).saturating_sub(1);

        while array_uids.len() < needed {
            if let Some((_new_idx, uid)) = self.duplicate_prim_for_transform(source_idx) {
                array_uids.push(uid);
            } else {
                break;
            }
        }
        while array_uids.len() > needed {
            if let Some(uid) = array_uids.pop() {
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    self.remove_prim_from_graph(idx);
                }
            }
        }
        if let Some(ts) = self.tool.current.as_transform_mut() {
            ts.array_prim_uids = array_uids;
        }
        self.update_array_positions();
    }

    /// Toggle circle vs linear array (C key). Center = original position (empty — no prim there).
    pub(crate) fn transform_array_toggle_circle(&mut self) {
        let Some(ts) = self.tool.current.as_transform_mut() else { return };
        if !ts.shift_duplicate {
            return;
        }
        ts.array_circle = !ts.array_circle;
        let circle = ts.array_circle;
        let source_uid = ts.array_source_uid;
        // Circle needs ≥2 copies on the ring.
        if circle && ts.array_count < 2 {
            ts.array_count = 3;
        }
        // Hide the source prim so nothing sits at the circle center.
        if let Some(suid) = source_uid {
            if let Some(sidx) = self.prim_index_by_uid(suid) {
                self.scene.primitives[sidx].visible = !circle;
                self.mirror_prim_to_graph(sidx);
            }
        }
        self.transform_array_rebuild_copies();
        self.toast(if circle {
            "Circle array · empty center · scroll=count · C=linear"
        } else {
            "Linear array · scroll=count · C=circle"
        });
    }

    /// Update intermediate array positions after the dragged prim moves.
    fn update_array_positions(&mut self) {
        let ts = match self.tool.current.as_transform() {
            Some(t) if t.shift_duplicate => t,
            _ => return,
        };
        let origin_local = ts.dup_origin_local;
        let array_count = ts.array_count.max(1);
        let circle = ts.array_circle;
        let dragged_uid = if let Some(t) = ts.multi_targets.first() {
            t.prim_uid
        } else {
            return;
        };
        let array_uids = ts.array_prim_uids.clone();
        let primary_rot = ts.multi_targets.first().map(|t| t.original_rot);
        let primary_scale = ts.multi_targets.first().map(|t| t.original_scale);

        let dragged_local = if let Some(idx) = self.prim_index_by_uid(dragged_uid) {
            self.scene.primitives[idx].position
        } else {
            return;
        };

        if circle {
            // Empty center at original position. All copies (primary + intermediates)
            // sit evenly on a circle in the workplane.
            let plane_n = self.viewport.wp_resolved.normal.normalize_or_zero();
            let u = self.viewport.wp_resolved.tangent_u.normalize_or_zero();
            let v = self.viewport.wp_resolved.tangent_v.normalize_or_zero();
            let offset = dragged_local - origin_local;
            let flat = offset - plane_n * offset.dot(plane_n);
            // Min radius so switching to C with no drag still shows a ring.
            let radius = flat.length().max(0.5);
            let angle0 = {
                let x = flat.dot(u);
                let y = flat.dot(v);
                if x * x + y * y < 1e-8 {
                    0.0
                } else {
                    y.atan2(x)
                }
            };
            let n = array_count.max(1) as f32;
            // Primary at angle0 on the ring (not at center).
            let primary_pos = origin_local + u * (radius * angle0.cos()) + v * (radius * angle0.sin());
            if let Some(idx) = self.prim_index_by_uid(dragged_uid) {
                self.scene.primitives[idx].position = primary_pos;
                self.mirror_prim_to_graph(idx);
            }
            // Intermediates evenly around the rest of the circle.
            for (i, &uid) in array_uids.iter().enumerate() {
                let a = angle0 + std::f32::consts::TAU * (i + 1) as f32 / n;
                let pos = origin_local + u * (radius * a.cos()) + v * (radius * a.sin());
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    self.scene.primitives[idx].position = pos;
                    if let Some(r) = primary_rot {
                        self.scene.primitives[idx].rotation_deg = r;
                    }
                    if let Some(s) = primary_scale {
                        self.scene.primitives[idx].scale = s;
                    }
                    self.mirror_prim_to_graph(idx);
                }
            }
        } else {
            // Linear: even spacing origin → primary (source stays at origin, visible).
            let offset = dragged_local - origin_local;
            for (i, &uid) in array_uids.iter().enumerate() {
                let t = (i + 1) as f32 / array_count as f32;
                let pos = origin_local + offset * t;
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    self.scene.primitives[idx].position = pos;
                    self.mirror_prim_to_graph(idx);
                }
            }
        }
    }

    /// Restore tool state saved before G grab (bevel/extrude mode).
    pub(crate) fn restore_pre_grab_state(&mut self) {
        if let Some(work_mode) = self.tool.pre_grab_work_mode.take() {
            let saved = std::mem::replace(&mut self.tool.pre_grab_mode, ToolMode::Select);
            if saved.is_extrude() {
                self.tool.work_mode = work_mode;
                self.sel.edit_target = self.active_prim_index_unified();
            }
            self.set_mode(saved);
        }
    }

    // Capture digit/minus/dot/backspace into the numeric buffer during modal transform
    pub(crate) fn capture_transform_num_input(&mut self, ctx: &egui::Context) {
        let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
        for evt in events {
            match evt {
                egui::Event::Text(t) => {
                    for ch in t.chars() {
                        if ch.is_ascii_digit() || ch == '.' || ch == '-' {
                            if let Some(ts) = self.tool.current.as_transform_mut() { ts.num_buf.push(ch); }
                        }
                    }
                }
                egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                    if let Some(ts) = self.tool.current.as_transform_mut() { ts.num_buf.pop(); }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn update_modal_transform(&mut self, cur: egui::Pos2, ctrl: bool, shift: bool) {
        // Light transform: multi-light aware
        if let Some(ts) = self.tool.current.as_transform() {
            if !ts.light_targets.is_empty() {
                let light_snaps = ts.light_targets.clone();
                let kind = ts.kind;
                let start = ts.start_mouse;
                let axis = ts.axis;
                let fm = ts.free_mode;
                let num_val: Option<f32> = if ts.num_buf.is_empty() { None } else { ts.num_buf.parse().ok() };
                match kind {
                    TransformKind::Rotate => {
                        let prec = crate::transform::drag_precision_scale(shift, ctrl);
                        let cur = crate::transform::precision_scale_mouse(start, cur, prec);
                        let dx = cur.x - start.x;
                        let dy = cur.y - start.y;
                        for snap in &light_snaps {
                            let lid = snap.light_id;
                            let az = snap.original_azimuth + dx * self.tweak.light_azimuth_speed;
                            let el = (snap.original_elevation - dy * self.tweak.light_elevation_speed).clamp(-90.0, 90.0);
                            self.write_light(lid, |l| { l.azimuth = az; l.elevation = el; });
                        }
                    }
                    TransformKind::Grab => {
                        let cam = self.transform_camera();
                        let settings = self.transform_settings();
                        let snap_active = self.is_snap_active(ctrl);
                        let snap_pos = self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos);
        let (snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize, snap_angle_lock) =
            self.snap_input_flags();
                        // Compute delta from first light's original position
                        let first = &light_snaps[0];
                        let input = TransformInput {
                            kind, axis,
                            original_pos: first.original_pos, original_rot: Vec3::ZERO, original_scale: Vec3::ONE,
                            original_elongation: Vec3::ZERO,
                            mouse_start: start, mouse_current: cur,
                            numeric_value: num_val,
                            snap_active, snap_grid: self.effective_snap_grid(), snap_rotation_deg: self.viewport.snap_rotation_deg,
                            snap_rotation_active: shift, snap_pos,
                            snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
                            snap_angle_lock,
                            free_mode: fm,
                            elongate_face_negative: false,
                            elongate_symmetric: false,
                            space: TransformSpace::Global,
                            object_rot: glam::Quat::IDENTITY,
                            precision_scale: crate::transform::drag_precision_scale(shift, ctrl),
                        };
                        let result = compute_transform(&input, &cam, &settings);
                        let delta = result.position - first.original_pos;
                        for snap in &light_snaps {
                            let lid = snap.light_id;
                            let pos = snap.original_pos + delta;
                            self.write_light(lid, |l| l.position = pos);
                        }
                    }
                    TransformKind::Scale => {
                        // S on a light scales its ATTENUATION RADIUS (owner
                        // 2026-08-07). Numeric entry types an absolute factor
                        // ("S 2 Enter" = double the radius), matching prim scale.
                        let prec = crate::transform::drag_precision_scale(shift, ctrl);
                        let cur = crate::transform::precision_scale_mouse(start, cur, prec);
                        let factor = num_val
                            .unwrap_or_else(|| (1.0 + (cur.x - start.x) * 0.01))
                            .max(0.01);
                        for snap in &light_snaps {
                            let lid = snap.light_id;
                            let r = (snap.original_range * factor).clamp(0.05, 500.0);
                            self.write_light(lid, |l| l.range = r);
                        }
                    }
                    _ => {}
                }
                self.mark_dirty(DirtyFlags::LIGHT);
                return;
            }
        }

        let (kind, axis, start, op, or, os, num_buf, node_target, fm, multi, space, object_orient, pivot_frozen) = {
            let ts = match self.tool.current.as_transform() { Some(t) => t, None => return };
            (ts.kind, ts.axis, ts.start_mouse, ts.original_pos, ts.original_rot, ts.original_scale,
             ts.num_buf.clone(), ts.node_target, ts.free_mode, ts.multi_targets.clone(),
             ts.space, ts.object_orient, ts.dup_origin_local)
        };

        let cam = self.transform_camera();
        let settings = self.transform_settings();
        let snap_active = self.is_snap_active(ctrl);
        let snap_pos = self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos);
        let (snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize, snap_angle_lock) =
            self.snap_input_flags();
        let num_val: Option<f32> = if num_buf.is_empty() { None } else { num_buf.parse().ok() };

        // Apply to target — dispatch only, no math
        if let Some(nid) = node_target {
            // R/S: mouse measures around the frozen Action Center / geometry pivot; object
            // origin orbits that pivot (same as multi-body gizmo). Grab uses object origin.
            let use_pivot = matches!(kind, TransformKind::Rotate | TransformKind::Scale)
                && !self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |t| t.shift_duplicate);
            let pivot = if use_pivot { pivot_frozen } else { op };
            let input = TransformInput {
                kind, axis,
                original_pos: pivot, original_rot: or, original_scale: os,
                original_elongation: Vec3::ZERO,
                mouse_start: start, mouse_current: cur,
                numeric_value: num_val,
                snap_active, snap_grid: self.effective_snap_grid(), snap_rotation_deg: self.viewport.snap_rotation_deg,
                snap_rotation_active: shift, snap_pos,
                snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
                snap_angle_lock,
                free_mode: fm,
                elongate_face_negative: false,
                elongate_symmetric: false,
                space,
                object_rot: object_orient, // frozen at modal start — never re-read live orientation
                precision_scale: crate::transform::drag_precision_scale(shift, ctrl),
            };
            let mut result = compute_transform(&input, &cam, &settings);
            if use_pivot {
                let offset = op - pivot;
                if kind == TransformKind::Rotate {
                    let q0 = object_orient;
                    let q1 = euler_yxz_deg_to_quat(result.rotation);
                    let dq = (q1 * q0.inverse()).normalize();
                    result.position = pivot + dq * offset;
                } else if kind == TransformKind::Scale {
                    const EPS: f32 = 1e-7;
                    let safe_scale = os.abs().max(Vec3::splat(EPS));
                    let safe_result = result.scale.abs().max(Vec3::splat(EPS)) * result.scale.signum();
                    let scale_ratio = safe_result / safe_scale;
                    if scale_ratio.is_finite() {
                        result.position = pivot + offset * scale_ratio;
                    }
                }
            }
            if matches!(kind, TransformKind::Grab) {
                self.apply_snap_align_rotation(snap_active, &mut result);
            }
            self.apply_to_scene_node(nid, &result);
            self.cache.prim_buf_stale = true;
            self.mark_dirty(DirtyFlags::RENDER);
        } else if !multi.is_empty() {
            // Multi-prim modal transform: compute per-prim from each original
            // Get body pivot for orbit/scale if a body is selected
            let body_pivot = self.sel.usel.active_node().and_then(|nid| {
                self.scene.scene_graph.get(nid).and_then(|n| {
                    if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &n.payload {
                        Some(*origin)
                    } else { None }
                })
            });

            for snap in &multi {
                let input = TransformInput {
                    kind, axis,
                    original_pos: snap.original_pos, original_rot: snap.original_rot, original_scale: snap.original_scale,
                    original_elongation: snap.original_elongation,
                    mouse_start: start, mouse_current: cur,
                    numeric_value: num_val,
                    snap_active, snap_grid: self.effective_snap_grid(), snap_rotation_deg: self.viewport.snap_rotation_deg,
                    snap_rotation_active: shift, snap_pos,
                    snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
                    snap_angle_lock,
                    free_mode: fm,
                    elongate_face_negative: false,
                    elongate_symmetric: false,
                    space,
                    // Per-prim world orient at drag start (body * prim), not live active_rotation_quat.
                    object_rot: snap.original_orient,
                    precision_scale: crate::transform::drag_precision_scale(shift, ctrl),
                };
                let mut result = compute_transform(&input, &cam, &settings);

                // Body selected: orbit/pivot-scale handles position; keep per-prim rotation unchanged
                if let Some(pivot) = body_pivot {
                    let offset = snap.original_pos - pivot;
                    if kind == TransformKind::Rotate {
                        let q0 = snap.original_orient;
                        let q1 = euler_yxz_deg_to_quat(result.rotation);
                        let dq = (q1 * q0.inverse()).normalize();
                        result.position = pivot + dq * offset;
                        // Prims rotate with the orbit so they stay oriented to the group
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

                if matches!(kind, TransformKind::Grab) {
                    self.apply_snap_align_rotation(snap_active, &mut result);
                }

                if let Some(idx) = self.prim_index_by_uid(snap.prim_uid) {
                    self.apply_to_sdf_prim(idx, &result);
                }
            }
            // Body origin update deferred to finish_gizmo_drag — updating it during
            // drag would shift the raster mesh instance transform (body origin mismatch
            // with the mesh that was baked at the old origin).
            // SCENE dirty re-uploads prim buffer so the SDF surface updates in real-time.
            self.update_array_positions();
            self.cache.prim_buf_stale = true;
            self.mark_dirty(DirtyFlags::RENDER);
        } else if let Some(idx) = self.active_prim_index_unified() {
            let input = TransformInput {
                kind, axis,
                original_pos: op, original_rot: or, original_scale: os,
                original_elongation: Vec3::ZERO,
                mouse_start: start, mouse_current: cur,
                numeric_value: num_val,
                snap_active, snap_grid: self.effective_snap_grid(), snap_rotation_deg: self.viewport.snap_rotation_deg,
                snap_rotation_active: shift, snap_pos,
                snap_affect_move, snap_affect_rotate, snap_affect_scale, snap_grid_quantize,
                snap_angle_lock,
                free_mode: fm,
                elongate_face_negative: false,
                elongate_symmetric: false,
                space,
                object_rot: object_orient,
                precision_scale: crate::transform::drag_precision_scale(shift, ctrl),
            };
            let mut result = compute_transform(&input, &cam, &settings);
            if matches!(kind, TransformKind::Grab) {
                self.apply_snap_align_rotation(snap_active, &mut result);
            }
            self.apply_to_sdf_prim(idx, &result);
            self.update_array_positions();
            // Prim buffer re-upload for real-time SDF surface update during drag.
            self.cache.prim_buf_stale = true;
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

}
