//! Command queue — execute_and_log (zero-latency) + process_commands (deferred).

use crate::*;

impl FourthLayerApp {
    /// Queue a command for deferred processing at end of frame.
    #[allow(dead_code)]
    pub(crate) fn push_cmd(&mut self, cmd: AppCommand) { self.commands.push(cmd); }

    /// Resolve the body NodeId that owns a given prim (by flat index).
    fn body_nid_for_prim(&self, idx: usize) -> Option<core_math::scene_graph::NodeId> {
        self.scene.prim_body_map.get(idx).map(|(nid, _)| *nid)
    }

    /// Convert a world-space position to body-local space for the body owning `prim_idx`.
    /// Uses BodyTransform::world_to_local to handle rotation and scale correctly.
    /// Falls back to world-space unchanged when no body is found.
    fn world_to_local_for_prim(&self, prim_idx: usize, world_pos: Vec3) -> Vec3 {
        if let Some(nid) = self.body_nid_for_prim(prim_idx) {
            if let Some(bt) = self.scene.scene_graph.body_world_transform(nid) {
                return bt.world_to_local(world_pos);
            }
        }
        world_pos
    }

    /// Execute a transform command immediately (zero latency) and log it for macro recording.
    /// Same-value sets skip undo (no empty history entry).
    #[allow(dead_code)]
    pub(crate) fn execute_and_log(&mut self, cmd: AppCommand) {
        let mut applied = false;
        match &cmd {
            AppCommand::SetPosition(uid, pos) => {
                if let Some(idx) = self.prim_index_by_uid(*uid) {
                    // SetPosition receives world-space coordinates from callers (AI, macros).
                    // With BODY_LOCAL_PRIMS=true, convert to body-local before writing.
                    let local_pos = if Self::BODY_LOCAL_PRIMS {
                        self.world_to_local_for_prim(idx, *pos)
                    } else {
                        *pos
                    };
                    if (self.scene.primitives[idx].position - local_pos).length_squared() >= 1e-12 {
                        self.transaction_snapshot_only("AI/macro transform");
                        self.write_prim(idx, |pr| pr.position = local_pos);
                        applied = true;
                    }
                }
            }
            AppCommand::SetRotation(uid, rot) => {
                if let Some(idx) = self.prim_index_by_uid(*uid) {
                    let r = *rot;
                    if (self.scene.primitives[idx].rotation_deg - r).length_squared() >= 1e-10 {
                        self.transaction_snapshot_only("AI/macro transform");
                        self.write_prim(idx, |pr| pr.rotation_deg = r);
                        applied = true;
                    }
                }
            }
            AppCommand::SetScale(uid, sc) => {
                if let Some(idx) = self.prim_index_by_uid(*uid) {
                    let s = *sc;
                    if (self.scene.primitives[idx].scale - s).length_squared() >= 1e-12 {
                        self.transaction_snapshot_only("AI/macro transform");
                        self.write_prim(idx, |pr| pr.scale = s);
                        applied = true;
                    }
                }
            }
            _ => { self.commands.push(cmd); return; }
        }
        if applied {
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if self.macro_recording { self.macro_buffer.push(cmd.clone()); }
        self.command_log.push(cmd);
        if self.command_log.len() > 1_000 { self.command_log.drain(..500); }
    }

    pub(crate) fn process_commands(&mut self) {
        let cmds = std::mem::take(&mut self.commands);
        for cmd in cmds {
            self.command_log.push(cmd.clone());
            if self.macro_recording { self.macro_buffer.push(cmd.clone()); }
            match cmd {
                AppCommand::DeleteSelected => {
                    self.delete_selected();
                    if self.scene.primitives.iter().any(|p| p.node_graph.is_some()) { self.shader_dirty = true; }
                }
                AppCommand::DuplicateSelected => {
                    self.duplicate_selected();
                    if self.scene.primitives.iter().any(|p| p.node_graph.is_some()) { self.shader_dirty = true; }
                }
                AppCommand::SelectSingle(uid) => {
                    self.select_single_unified(SceneItemId::Prim(uid));
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                AppCommand::DeselectAll => { self.deselect_all(); self.mark_dirty(DirtyFlags::RENDER); }
                AppCommand::SelectAll => {
                    let ids: Vec<SceneItemId> = self.scene.primitives.iter()
                        .map(|p| SceneItemId::Prim(p.prim_uid)).collect();
                    self.select_after_create(&ids);
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                AppCommand::SetBooleanOp(uid, op) => {
                    // Single shared mutation path (also used by the X boolean pie).
                    self.set_prims_boolean_op(&[(uid, op)]);
                }
                AppCommand::SetVisible(uid, vis) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        // Already at target visibility → no empty undo.
                        if self.scene.primitives[idx].visible == vis { continue; }
                        self.transaction("Toggle visibility", |s| {
                            s.write_prim(idx, |p| p.visible = vis);
                            s.rebake_no_undo();
                            s.mark_visibility_dirty();
                            s.mark_outliner_dirty();
                        });
                    }
                }
                AppCommand::SetLocked(uid, locked) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        if self.scene.primitives[idx].locked == locked { continue; }
                        self.transaction("Toggle lock", |s| {
                            s.write_prim(idx, |p| p.locked = locked);
                            s.mark_outliner_dirty();
                        });
                    }
                }
                AppCommand::CreateInstance => { self.create_instance(); }
                AppCommand::InstanceIntoAllBodies => { self.instance_into_all_bodies(); }
                AppCommand::MakeInstanceUnique(uid) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        self.make_instance_unique(idx);
                    }
                }
                AppCommand::MakeSelectedInstancesUnique => {
                    self.make_selected_instances_unique();
                }
                AppCommand::Toast(msg) => { self.toast(&msg); }
                AppCommand::Undo => { self.undo(); }
                AppCommand::Redo => { self.redo(); }
                // Poly commands
                AppCommand::PolyExtrude => { self.poly_extrude_faces(0.3); }
                AppCommand::PolyInset => { self.poly_inset_faces(0.1); }
                AppCommand::PolyCircle => { self.poly_circle(); }
                AppCommand::PolyLoopCut => { self.poly_loop_cut(); }
                AppCommand::PolyMerge => { self.poly_merge_vertices(); }
                AppCommand::PolyDeleteFaces => { self.poly_delete_faces(); }
                AppCommand::PolySubdivide => { self.poly_subdivide(); }
                AppCommand::PolyDice => { self.poly_activate_dice(); }
                AppCommand::PolySolidify => { self.poly_solidify(0.1); }
                AppCommand::PolyKnife => { self.poly_activate_knife(); }
                AppCommand::PolyBevelEdge => { self.poly_activate_bevel(); }
                AppCommand::PolyUnbevel => { self.poly_activate_unbevel(); }
                AppCommand::PolyFaceHinge => { self.poly_activate_face_hinge(); }
                AppCommand::PolyEdgeLoop => { self.poly_select_edge_loop(); }
                AppCommand::PolyEdgeRing => { self.poly_select_edge_ring(); }
                AppCommand::PolyBridge => { self.poly_bridge(); }
                AppCommand::PolyPoke => { self.poly_poke_faces(); }
                AppCommand::PolyConnect => { self.poly_connect_vertices(); }
                AppCommand::PolyToSphere => { self.poly_to_sphere(); }
                // NURBS commands
                AppCommand::NurbsKnotU => { self.nurbs_insert_knot_u(); }
                AppCommand::NurbsKnotV => { self.nurbs_insert_knot_v(); }
                AppCommand::NurbsWeightUp => { self.nurbs_adjust_weight(0.1); }
                AppCommand::NurbsWeightDown => { self.nurbs_adjust_weight(-0.1); }
                AppCommand::NurbsGrabCv => { self.nurbs_tool = NurbsTool::MoveCv; }
                // UI commands
                AppCommand::ToggleWindow(name) => {
                    match name {
                        "tweak" => self.ui.show_tweak_panel = !self.ui.show_tweak_panel,
                        "shortcuts" => self.ui.show_shortcut_editor = !self.ui.show_shortcut_editor,
                        "export" => self.ui.show_export_window = !self.ui.show_export_window,
                        "grid" => self.viewport.show_grid_settings = !self.viewport.show_grid_settings,
                        "layout" => self.ui.show_layout_settings = !self.ui.show_layout_settings,
                        _ => {}
                    }
                }
                AppCommand::SetWorkMode(wm) => {
                    if wm == WorkMode::Sculpt {
                        self.enter_sculpt_mode();
                    } else {
                        if wm == WorkMode::Object {
                            self.poly_sculpt_leave_edit();
                        }
                        self.tool.work_mode = wm;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                AppCommand::OpenCommandPalette => { self.ui.command_palette.open = !self.ui.command_palette.open; }
                AppCommand::OpenLayoutSettings => { self.ui.show_layout_settings = !self.ui.show_layout_settings; }
                AppCommand::SetCreateOp(op) => { self.set_create_op(op); }
                AppCommand::Screenshot => { self.screenshot_requested = true; }
                // Transform commands: also handled by execute_and_log() for zero-latency path
                AppCommand::SetPosition(uid, pos) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        let local_pos = if Self::BODY_LOCAL_PRIMS {
                            self.world_to_local_for_prim(idx, pos)
                        } else {
                            pos
                        };
                        // Same position → no empty undo (API / macro re-set of current value).
                        if (self.scene.primitives[idx].position - local_pos).length_squared() < 1e-12 {
                            continue;
                        }
                        self.transaction("Set position", |s| {
                            s.write_prim(idx, |p| p.position = local_pos);
                            s.mark_dirty(DirtyFlags::RENDER);
                        });
                    }
                }
                AppCommand::SetRotation(uid, rot) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        if (self.scene.primitives[idx].rotation_deg - rot).length_squared() < 1e-10 {
                            continue;
                        }
                        self.transaction("Set rotation", |s| {
                            s.write_prim(idx, |p| p.rotation_deg = rot);
                            s.mark_dirty(DirtyFlags::RENDER);
                        });
                    }
                }
                AppCommand::SetScale(uid, sc) => {
                    if let Some(idx) = self.prim_index_by_uid(uid) {
                        if (self.scene.primitives[idx].scale - sc).length_squared() < 1e-12 {
                            continue;
                        }
                        self.transaction("Set scale", |s| {
                            s.write_prim(idx, |p| p.scale = sc);
                            s.mark_dirty(DirtyFlags::RENDER);
                        });
                    }
                }
            }
        }
        if self.command_log.len() > 1_000 { self.command_log.drain(..500); }
    }
}
