//! Tool mode transitions, cancel, and drop — shared actions called from
//! keyboard, pie menu, and UI buttons.

use crate::*;

impl AzraApp {
    /// Transition persistent tool mode with mutual exclusion.
    /// Cancels any active transient operation first, then clears mode-specific hover state.
    pub(crate) fn set_mode(&mut self, new_mode: ToolMode) {
        // Switching modes confirms in-flight work (Tool Exit Convention), then switches.
        if self.tool.current.is_busy() {
            self.confirm_operation();
        }
        // BoolCut lives as Option on ToolState (not in ActiveTool)
        if !matches!(new_mode, ToolMode::BoolCut) && self.tool.bool_cut.is_some() {
            self.cancel_bool_cut();
        }
        // Clear mode-specific hover state from outgoing mode
        self.tool.hovered_bevel_edge = None;
        self.tool.selected_bevel_edges.clear();
        self.tool.uniform_bevel_hovered = false;
        self.tool.falloff_hovered = None;
        self.tool.hovered_face = None;

        // Entering Bevel: drop competing tools so B owns the viewport (no gizmo/poly/create overlap).
        if matches!(new_mode, ToolMode::Bevel) {
            self.set_gizmo_armed(false);
            self.poly_tool = PolyTool::Idle;
            self.knife_points.clear();
            self.knife_snap_pos = None;
            if self.tool.work_mode == WorkMode::Face {
                self.tool.work_mode = WorkMode::Object;
                self.sel.edit_target = None;
                self.sel.selected_faces.clear();
            }
            if self.tool.bool_cut.is_some() {
                self.cancel_bool_cut();
            }
        }
        // Leaving Bevel for anything else: selection already cleared above; no sticky edge UI.

        // Stamp the body this mode-tool belongs to (Bevel/Extrude are per-object).
        // A per-frame reconciler (drop_mode_tool_on_body_switch) drops the tool when
        // the active body switches away from this. Non-body-scoped modes clear it.
        // Computed before the move of `new_mode` into `self.tool.mode`.
        self.tool.mode_body_nid = match new_mode {
            ToolMode::Bevel | ToolMode::Extrude => Some(self.scene.active_body_nid),
            _ => None,
        };
        // Set the new mode
        self.tool.mode = new_mode;
    }

    /// Per-frame reconciler: if a body-scoped mode-tool (Bevel/Extrude) is active
    /// and the active body has switched away from the one the tool was armed on,
    /// drop the tool back to Select (DCC "switch object → drop tool" convention,
    /// Tool Exit Convention). Reselecting a DIFFERENT PRIM within the SAME body
    /// does NOT change `active_body_nid`, so the tool is preserved there.
    /// Routes through the existing `set_mode(ToolMode::Select)` exit path.
    pub(crate) fn drop_mode_tool_on_body_switch(&mut self) {
        let Some(armed_body) = self.tool.mode_body_nid else { return; };
        if armed_body == self.scene.active_body_nid { return; }
        // Body changed under an active mode-tool — exit it.
        if self.tool.mode.is_extrude() {
            self.tool.work_mode = WorkMode::Object;
            self.sel.edit_target = None;
            self.sel.selected_faces.clear();
        }
        self.set_mode(ToolMode::Select); // clears mode_body_nid via the match above
        self.toast("Tool dropped (object changed)");
    }

    /// Exit SDF Bevel mode if active (used when arming gizmo / other exclusive tools).
    pub(crate) fn exit_bevel_mode_if_active(&mut self) {
        if self.tool.current.is_bevel() || self.tool.current.is_bool_edge_bevel() {
            let _ = self.cancel_operation();
        }
        if self.tool.mode.is_bevel() {
            self.set_mode(ToolMode::Select);
        }
    }

    /// CONFIRM whichever poly modal is live. Returns true if one was confirmed.
    ///
    /// THE single implementation of CLAUDE.md's Tool Exit Convention — *"Switching tools = confirm
    /// current work, then switch."* Owner 2026-07-30: *"even if I did not accept the cut, changing
    /// for another tool should accept it automatically"* … *"ALL TOOL MUST DO THIS BEHAVIOR NOT JUST
    /// G."*
    ///
    /// Before this, each of the seven poly modals hand-rolled its own exit and only honoured Enter /
    /// Esc, so pressing G (or any other tool key) mid-modal was SWALLOWED — the work was neither
    /// confirmed nor cancelled, and the user was stuck. Adding the switch-confirm to one modal at a
    /// time would have been six more copies of the same rule (the "two code paths for one operation"
    /// trap); this is the one place that knows how to end a poly modal.
    ///
    /// Add new poly modals HERE when they land, not at their key-handling site.
    pub(crate) fn confirm_poly_modal(&mut self) -> bool {
        if self.tool.current.is_poly_loop_cut_slide() {
            self.poly_confirm_loop_cut_slide();
        } else if self.tool.current.is_poly_bevel() {
            self.poly_confirm_bevel();
        } else if self.tool.current.is_poly_face_hinge() {
            self.poly_confirm_face_hinge();
        } else if self.tool.current.is_poly_extrude() {
            self.poly_confirm_extrude_drag();
        } else if self.tool.current.is_poly_extrude_sweep() {
            self.poly_confirm_extrude_sweep();
        } else if self.tool.current.is_poly_edge_extrude() {
            self.poly_confirm_edge_extrude_drag();
        } else if self.tool.current.is_poly_inset() {
            self.poly_confirm_inset_drag();
        } else if self.tool.current.is_poly_shrink_fatten() {
            self.poly_confirm_shrink_fatten();
        } else if self.tool.current.is_poly_offset_loop() {
            self.poly_confirm_offset_loop();
        } else if self.tool.current.is_poly_dice() {
            self.poly_confirm_dice();
        } else if self.tool.current.is_poly_edge_weight() {
            self.poly_confirm_edge_weight_drag();
        } else {
            return false;
        }
        true
    }

    /// Cancel the active transient operation, returning tool.current to Idle.
    /// Returns true if something was cancelled, false if already idle.
    pub(crate) fn cancel_operation(&mut self) -> bool {
        // In-progress spine point drag first (still inside the SpinePointEdit tool).
        if self.spine_pt_drag.is_some() {
            self.spine_pt_cancel_drag();
            self.toast("Spine move cancelled");
            return true;
        }
        // Exclusive spine tool: Esc exits the tool (geometry kept) → object mode.
        if self.tool.current.is_spine_point_edit() {
            self.spine_exit_tool();
            self.toast("Spine tool exited");
            return true;
        }
        if self.tool.current.is_idle() { return false; }

        // Variants with dedicated cancel methods (read tool.current internally)
        if self.tool.current.is_transform()    { self.cancel_modal_transform();   return true; }
        if self.tool.current.is_gizmo()        { self.cancel_gizmo_drag();        return true; }
        if self.tool.current.is_create()       { crate::ops::create::session::cancel(self); return true; }
        if self.tool.current.is_pivot_edit()   { self.cancel_pivot_mode();        return true; }
        if self.tool.current.is_poly_extrude() { self.poly_cancel_extrude_drag(); self.toast("Extrude cancelled"); return true; }
        if self.tool.current.is_poly_extrude_sweep() {
            self.poly_cancel_extrude_sweep();
            self.toast("Extrude Sweep cancelled");
            return true;
        }
        if self.tool.current.is_poly_edge_extrude() { self.poly_cancel_edge_extrude_drag(); self.toast("Edge Extrude cancelled"); return true; }
        if self.tool.current.is_poly_inset()   { self.poly_cancel_inset_drag();   self.toast("Inset cancelled");   return true; }
        if self.tool.current.is_poly_shrink_fatten() { self.poly_cancel_shrink_fatten(); self.toast("Shrink/Fatten cancelled"); return true; }
        if self.tool.current.is_poly_edge_weight() { self.poly_cancel_edge_weight_drag(); self.toast("Edge Weight cancelled"); return true; }
        if self.tool.current.is_poly_bevel()   { self.poly_cancel_bevel_drag();   self.toast("Bevel cancelled");   return true; }
        if self.tool.current.is_poly_offset_loop() { self.poly_cancel_offset_loop(); self.toast("Offset Edge Loop cancelled"); return true; }
        if self.tool.current.is_poly_dice() { self.poly_cancel_dice(); return true; }
        if self.tool.current.is_poly_face_hinge() { self.poly_cancel_face_hinge(); self.toast("Hinge Rotate cancelled"); return true; }
        if self.tool.current.is_poly_loop_cut_slide() { self.poly_cancel_loop_cut_slide(); return true; }
        if self.tool.current.is_poly_slide()   { self.poly_cancel_slide();        self.toast("Slide cancelled");     return true; }
        if self.tool.current.is_nurbs_cv_move(){ self.nurbs_cancel_cv_drag();     self.toast("CV move cancelled"); return true; }
        if self.tool.current.is_nurbs_fillet() { self.nurbs_fillet_cancel(); return true; }
        if self.tool.current.is_poly_transform() { self.poly_cancel_transform();    self.toast("Transform cancelled"); return true; }
        if self.tool.current.is_poly_prop_origin() { self.poly_prop_origin_drag_cancel(); return true; }
        if self.tool.current.is_curve_point_transform() { self.curve_pt_cancel_transform(); self.toast("Transform cancelled"); return true; }
        if self.tool.current.is_curve_bevel() { self.curve_bevel_cancel(); self.toast("Bevel cancelled"); return true; }

        // Take-and-restore: FacePushPull (box half_extents or cylinder radius/height)
        if let Some(fd) = self.tool.current.take_face_push_pull() {
            if fd.prim_idx < self.scene.primitives.len() {
                let (orig_pos, orig_ext) = (fd.original_pos, fd.original_extents);
                self.write_prim(fd.prim_idx, |p| {
                    p.position = orig_pos;
                    match &mut p.shape {
                        SdfShape::Box { half_extents, .. } => { *half_extents = orig_ext; }
                        SdfShape::Cylinder { radius, half_height } => {
                            *radius = orig_ext.x.max(orig_ext.z);
                            *half_height = orig_ext.y;
                        }
                        _ => {}
                    }
                });
                self.pop_last_undo();
                self.rebake_no_undo();
            }
            self.toast("Push/Pull cancelled");
            return true;
        }

        // Simple set-to-Idle with side effects
        if self.tool.current.is_smart_mirror()    { self.tool.current = ActiveTool::Idle; self.toast("Mirror cancelled"); return true; }
        if self.tool.current.is_bevel()           { self.tool.current = ActiveTool::Idle; self.rebake_no_undo(); return true; }
        if self.tool.current.is_falloff_adjust()  { self.tool.current = ActiveTool::Idle; self.rebake_no_undo(); return true; }
        if self.tool.current.is_bool_edge_bevel() { self.tool.current = ActiveTool::Idle; self.bool_edge_dirty = true; self.rebake_no_undo(); return true; }
        if self.tool.current.is_selection_box() {
            self.tool.current = ActiveTool::Idle;
            self.tool.selection_box_secondary = false;
            self.tool.selection_box_subtract = false;
            return true;
        }
        if self.tool.current.is_poly_box_select()    { self.tool.current = ActiveTool::Idle; return true; }
        if self.tool.current.is_poly_circle_select() { self.tool.current = ActiveTool::Idle; return true; }
        if self.tool.current.is_clip_curve_draw()    { self.tool.current = ActiveTool::Idle; self.toast("Clip curve cancelled"); return true; }

        // Fallback — unknown busy variant
        self.tool.current = ActiveTool::Idle;
        true
    }

    /// Confirm the active transient operation on a TOOL SWITCH.
    /// Tool Exit Convention: "Switching tools = confirm current work, then switch."
    /// Mirrors cancel_operation()'s dispatch but routes each poly/modal drag to its
    /// CONFIRM counterpart. Anything without a meaningful confirm-then-switch (pointer-
    /// release-committed drags, modal pickers) falls back to cancel_operation(), so its
    /// behaviour is byte-identical to before. Escape / right-click still call
    /// cancel_operation() directly — only tool switches route here.
    pub(crate) fn confirm_operation(&mut self) -> bool {
        if self.tool.current.is_idle() { return false; }

        if self.tool.current.is_poly_extrude()   { self.poly_confirm_extrude_drag(); return true; }
        if self.tool.current.is_poly_extrude_sweep() {
            self.poly_confirm_extrude_sweep();
            return true;
        }
        if self.tool.current.is_poly_edge_extrude() { self.poly_confirm_edge_extrude_drag(); return true; }
        if self.tool.current.is_poly_inset()     { self.poly_confirm_inset_drag();   return true; }
        if self.tool.current.is_poly_bevel()     { self.poly_confirm_bevel();        return true; }
        if self.tool.current.is_poly_offset_loop() { self.poly_confirm_offset_loop(); return true; }
        if self.tool.current.is_poly_dice() { self.poly_confirm_dice(); return true; }
        if self.tool.current.is_poly_face_hinge() { self.poly_confirm_face_hinge();  return true; }
        if self.tool.current.is_poly_slide()     { self.poly_confirm_slide();        return true; }
        if self.tool.current.is_poly_transform() { self.poly_confirm_transform();    return true; }
        // Switching tools KEEPS the placement — falling through to cancel would silently
        // throw the move away, which is not what "confirm current work, then switch" means.
        if self.tool.current.is_poly_prop_origin() { self.poly_prop_origin_drag_end(); return true; }
        if self.tool.current.is_transform()      { self.confirm_modal_transform();   return true; }
        if self.tool.current.is_spine_point_edit() {
            self.spine_finish_edit();
            return true;
        }

        // No semantic "confirm-then-switch" — behave exactly like cancel.
        self.cancel_operation()
    }

    /// Arm / disarm the G/R/S gizmo.
    ///
    /// Owner 2026-08-07: when GRS is **deactivated**, the Automatic Action Center pivot
    /// resets to its default — **selection center** (`action_automatic_center = None`
    /// so `resolve_action_frame` tracks the selection median again). A Shift+RMB place is
    /// session-local to the armed G/R/S; it must not stick after Space / Esc / tool switch.
    pub(crate) fn set_gizmo_armed(&mut self, armed: bool) {
        if self.tool.gizmo_armed && !armed {
            self.tool.action_automatic_center = None;
        }
        self.tool.gizmo_armed = armed;
    }

    /// Drop armed G/R/S when any other exclusive tool takes the viewport.
    /// Owner 2026-08-06: "activating a tool should always drop G,R,S if grab is active —
    /// changing for knife should drop grab tool."
    ///
    /// Call from knife / loop-cut / bevel / palette / `set_poly_tool` / non-GRS `start_operation`.
    /// Does NOT cancel a live gizmo drag that is itself the G/R/S path (those re-arm after).
    pub(crate) fn drop_grs_for_tool_switch(&mut self) {
        // Mid free-transform or gizmo drag: confirm (Tool Exit Convention), then clear arm.
        if self.tool.current.is_poly_transform()
            || self.tool.current.is_nurbs_cv_move()
            || self.tool.current.is_gizmo()
            || self.tool.current.is_transform()
            || self.tool.current.is_curve_point_transform()
        {
            let _ = self.confirm_operation();
        }
        self.set_gizmo_armed(false);
    }

    /// Set poly persistent tool. Non-Idle always drops G/R/S (one path for K / Ctrl+R / palette).
    pub(crate) fn set_poly_tool(&mut self, tool: PolyTool) {
        if tool != PolyTool::Idle {
            self.drop_grs_for_tool_switch();
        }
        self.poly_tool = tool;
    }

    /// Start a new transient operation.
    /// If another op is already active (e.g. Selection Box + Blend K diamond on same press),
    /// confirm/cancel it first — never panic. Debug builds used to `debug_assert!` idle and
    /// crashed when the user drag-selected over the blend-K handle.
    pub(crate) fn start_operation(&mut self, new_tool: ActiveTool) {
        if self.tool.current.is_busy() {
            #[cfg(debug_assertions)]
            eprintln!(
                "[tool] start_operation({}) while {} active — confirming in-flight op",
                new_tool.variant_name(),
                self.tool.current.variant_name()
            );
            // Confirm (not cancel) the in-flight op when a new tool starts.
            self.confirm_operation();
        }
        // Non-GRS tools always leave Grab. Gizmo / modal transform / poly-transform keep arm —
        // they ARE the G/R/S path (start_gizmo_drag, poly_start_transform).
        let is_grs_path = new_tool.is_gizmo()
            || new_tool.is_transform()
            || new_tool.is_poly_transform()
            || new_tool.is_nurbs_cv_move()
            || new_tool.is_curve_point_transform();
        if !is_grs_path {
            self.set_gizmo_armed(false);
        }
        self.tool.current = new_tool;
    }

    /// Drop the armed GIZMO (G/R/S) and any persistent TOOL MODE (create / bool-cut / bevel /
    /// extrude / poly tool), returning true if one was dropped. Does NOT touch active transient
    /// operations, UI windows, body scope, edit mode, or the selection — those belong to the full
    /// `drop_tool()` cascade. Shared by `drop_tool()` (its L1.5+L2) and the empty-viewport-click
    /// escalation: an LMB click on empty space drops the armed tool but keeps the selection; only a
    /// second empty click (nothing left to drop) falls through to deselect.
    pub(crate) fn drop_active_tool_only(&mut self) -> bool {
        // Shift+D session owns the gizmo — don't silently disarm (Esc cancels session).
        if self.tool.array_dup.is_some() {
            return false;
        }
        // ── L1.5: Dismiss armed gizmo (also resets Automatic pivot → selection center) ──
        if self.tool.gizmo_armed {
            self.set_gizmo_armed(false);
            return true;
        }
        // ── L2: Exit persistent tool mode ──
        if self.tool.mode.is_create_armed() {
            self.set_mode(ToolMode::Select);
            self.toast("Tool dropped");
            return true;
        }
        if self.tool.bool_cut.is_some() {
            self.cancel_bool_cut();
            return true;
        }
        if self.tool.mode.is_bevel() {
            self.set_mode(ToolMode::Select);
            self.toast("Bevel mode OFF");
            return true;
        }
        if self.tool.mode.is_extrude() {
            self.set_mode(ToolMode::Select);
            self.tool.work_mode = WorkMode::Object;
            self.sel.edit_target = None;
            self.sel.selected_faces.clear();
            self.toast("Extrude mode OFF");
            return true;
        }
        if self.poly_tool != PolyTool::Idle || self.knife_shape_kind != 0 {
            // Full knife teardown (shape state + free plane), not a partial clear —
            // same path as Esc / Tab-while-knife.
            if self.poly_tool == PolyTool::Knife || self.knife_shape_kind != 0 {
                self.poly_knife_cancel();
            } else {
                self.poly_tool = PolyTool::Idle;
            }
            self.toast("Tool cancelled");
            return true;
        }
        false
    }

    /// Universal drop-tool / Escape cascade.
    /// Returns `true` if something was dropped/cancelled, `false` if already idle.
    pub(crate) fn drop_tool(&mut self) -> bool {
        // ── Level 0a: Shift+D array session (before gizmo disarm — session owns the episode)
        if self.tool.array_dup.is_some() {
            self.array_dup_cancel();
            return true;
        }
        // ── Level 0: Overlays ──
        if self.curve_copy_angle_arm.is_some() {
            self.curve_copy_angle_arm = None;
            self.toast("Copy angle cancelled");
            return true;
        }
        if self.library.open { self.library.open = false; return true; }
        if let Some(dp) = self.tool.depth_pick.take() {
            // Restore selection from depth pick snapshot
            self.sel.usel = dp.prev_usel;
            self.mark_dirty(DirtyFlags::RENDER);
            return true;
        }

        // ── Level 1: Cancel active transient operation (gizmo stays armed) ──
        if self.cancel_operation() { return true; }

        // ── Levels 1.5 + 2: Dismiss armed gizmo, then exit persistent tool mode ──
        if self.drop_active_tool_only() { return true; }

        // ── Level 3: Close UI elements ──
        if self.ui.z_pie.open { self.ui.z_pie.open = false; self.ui.z_pie.open_time = None; return true; }
        if self.ui.tab_pie.open { self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None; return true; }
        if self.ui.reset_pie.open { self.ui.reset_pie.open = false; self.ui.reset_pie.open_time = None; return true; }
        if self.ui.f_pie.open { self.ui.f_pie.open = false; self.ui.f_pie.open_time = None; return true; }
        if self.ui.q_menu_open { self.ui.q_menu_open = false; return true; }
        if self.ui.ctx_menu_open { self.ui.ctx_menu_open = false; return true; }
        if self.viewport.show_grid_settings { self.viewport.show_grid_settings = false; return true; }
        if self.ui.show_export_window { self.ui.show_export_window = false; return true; }
        if self.ai.show_code_review { self.ai.show_code_review = false; return true; }
        if self.ui.show_node_editor { self.ui.show_node_editor = false; return true; }
        if self.ai.show_ai_tex_window { self.ai.show_ai_tex_window = false; return true; }
        if self.ai.show_beauty_window { self.ai.show_beauty_window = false; return true; }
        if self.ui.show_texgen_window { self.ui.show_texgen_window = false; return true; }
        if self.ai.show_agent_settings { self.ai.show_agent_settings = false; return true; }
        if self.ui.show_keybind_settings { self.ui.show_keybind_settings = false; return true; }
        if self.ui.show_shortcut_editor { self.ui.show_shortcut_editor = false; return true; }
        if self.ui.show_about { self.ui.show_about = false; return true; }
        if self.ai.agent_show_bar { self.ai.agent_show_bar = false; return true; }

        // ── Level 4: Navigate up body hierarchy ──
        if self.scene.active_body_nid != self.scene.sdf_body_nid {
            let parent = self.scene.scene_graph.parent_body(self.scene.active_body_nid)
                .unwrap_or(self.scene.sdf_body_nid);
            self.scene.active_body_nid = parent;
            self.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_)));
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Exited body scope");
            return true;
        }
        // ── Level 5: Work mode → Object ──
        if self.tool.work_mode != WorkMode::Object {
            if matches!(self.tool.work_mode, WorkMode::Vertex | WorkMode::Edge | WorkMode::Face) {
                self.ui.last_edit_mode = Some(self.tool.work_mode);
            }
            self.tool.work_mode = WorkMode::Object;
            self.mark_dirty(DirtyFlags::RENDER);
            self.toast("Object Mode");
            return true;
        }

        // ── Level 6: Deselect ──
        if self.sel.usel.has_prims() {
            self.deselect_all();
            self.mark_dirty(DirtyFlags::RENDER);
            return true;
        }

        false
    }

}
