//! Viewport mouse/drag input — moved from input.rs.

use eframe::egui;
use glam::{Quat, EulerRot, Vec3};
use core_math::primitives::{SdfShape, BevelMode};
use crate::*;

use crate::BODY_LOCAL_PRIMS;

/// Sensitivity boost for the 2-axis Seam/Extrude diamond drag relative to the
/// base `bevel_drag_speed` (0.002 units/px). Both axes span 0..=0.5, so at the
/// base rate the full range needs a ~250px drag — unusably fine (owner
/// feedback 2026-07-14). 4× reaches the full range in a comfortable ~60px.
/// Blend-k and bevel drags keep the base rate.
const SEAM_EXTRUDE_DRAG_BOOST: f32 = 4.0;

/// Poly LMB paint-select: pixels of movement from the press point before a click becomes a paint drag
/// (click-vs-drag split, movement-threshold not a timer). Matches the box tool's ~4px tiny-box gate.
const POLY_PAINT_DRAG_THRESHOLD: f32 = 4.0;
/// Alt+LMB while G/R/S armed: free-center transform only after this drag distance.
/// Smaller Alt+LMB motion / click / double-click stay loop/ring (owner: free must not
/// steal Alt+double ring when Grab is armed).
const POLY_ALT_FREE_DRAG_THRESHOLD: f32 = 6.0;
/// Screen-space radius (px) of the LMB paint-select cursor brush — elements under it are added each frame.

/// Smooth-scroll units in ONE physical wheel notch on this platform.
///
/// Every discrete-step wheel consumer (transform array count, poly bevel segments, extrude
/// sweep segments, knife shape segments) measures the wheel in `smooth_scroll_delta` units and
/// divides by this. Decoding `Event::MouseWheel` line units separately is what made knife
/// segments step 3× per notch — a notch is ~3 line units, not one.
pub(crate) const WHEEL_UNITS_PER_NOTCH: f32 = 40.0;

impl AzraApp {
    /// True while any radial pie owns the mouse — viewport must not select / deselect / box /
    /// context-menu under it.
    pub(crate) fn pie_menu_blocks_viewport(&self) -> bool {
        self.ui.sym_pie.open
            || self.ui.align_pie.open
            || self.ui.merge_pie.open
            || self.ui.sdf_special_pie.open
            || self.ui.sdf_bool_pie.open
            || self.ui.bool_pie.open
            || self.ui.z_pie.open
            || self.ui.tab_pie.open
            || self.ui.view_pie.open
            || self.ui.f_pie.open
            || self.ui.overlay_pie.open
            || self.ui.d_pie.open
            || self.ui.t_pie.open
            || self.ui.reset_pie.open
            || self.ui.spine_corner_pie.open
            || self.ui.res_pie.open
            || self.ui.shape_swap_pie_open
    }

    /// Floating popups drawn AFTER the viewport central panel (Ctrl+M assign, RMB context,
    /// Shift+A add). Without this gate the selection ray / LMB pick still runs under the
    /// menu because viewport input is processed earlier in the frame (owner 2026-08-06:
    /// "selection ray goes through the Ctrl+M material menu").
    pub(crate) fn floating_popup_blocks_viewport(&self) -> bool {
        self.ui.show_mat_assign || self.ui.ctx_menu_open || self.ui.show_add_popup
    }

    pub(crate) fn handle_viewport_input(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, rect: egui::Rect, response: &egui::Response) {
        // Any pie menu is modal for mouse: LMB must not deselect / box-select / open context menus
        // under the pie. Hold-to-open pies (X, Tab, …) and sticky click pies (special / reset /
        // symmetrize) all share this gate.
        // `pie_eat_click` swallows the confirming LMB on the frame the pie closed (keyboard runs
        // first and clears `open`, so the raw click would otherwise leak here the same frame).
        // Floating popups (Ctrl+M / RMB / Shift+A) same class — block pick ray under the menu.
        if self.pie_menu_blocks_viewport()
            || self.floating_popup_blocks_viewport()
            || std::mem::take(&mut self.ui.pie_eat_click)
        {
            return;
        }

        // UV split owns the left pane: LMB select / double-click island live there,
        // and must not orbit or 3D-pick through the overlay.
        if self.ui.show_uv_layout {
            if let Some(pos) = response.hover_pos().or_else(|| response.interact_pointer_pos()) {
                if self.uv_layout_left_pane().contains(pos) {
                    self.handle_uv_layout_input(ctx, pos, response);
                    return;
                }
            }
        }

        // Library place session (drag body into scene): move / Ctrl surface / scroll resize.
        // Active session owns LMB (confirm) + wheel (resize) but MMB orbit still runs below.
        let library_placing = self.tick_library_place_session(ctx, response);

        // ── AI render region draw (Neural Beauty crop) ──
        // Modal: LMB drag defines the box; Esc / right-click cancels draw mode.
        if self.ai.beauty_region_draw {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape))
                || response.clicked_by(egui::PointerButton::Secondary)
            {
                self.ai.beauty_region_draw = false;
                self.ai.beauty_region_drag_start = None;
                self.ai.beauty_region_drag_cur = None;
                self.toast("AI region draw cancelled");
                return;
            }
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(mp) = response.interact_pointer_pos() {
                    let local = egui::pos2(mp.x - rect.min.x, mp.y - rect.min.y);
                    self.ai.beauty_region_drag_start = Some(local);
                    self.ai.beauty_region_drag_cur = Some(local);
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(mp) = response.interact_pointer_pos() {
                    let local = egui::pos2(
                        (mp.x - rect.min.x).clamp(0.0, rect.width()),
                        (mp.y - rect.min.y).clamp(0.0, rect.height()),
                    );
                    self.ai.beauty_region_drag_cur = Some(local);
                    if self.ai.beauty_region_drag_start.is_none() {
                        self.ai.beauty_region_drag_start = Some(local);
                    }
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary)
                || (response.clicked_by(egui::PointerButton::Primary)
                    && self.ai.beauty_region_drag_start.is_some()
                    && !response.dragged_by(egui::PointerButton::Primary))
            {
                if let (Some(a), Some(b)) = (
                    self.ai.beauty_region_drag_start.take(),
                    self.ai.beauty_region_drag_cur.take(),
                ) {
                    let w = rect.width().max(1.0);
                    let h = rect.height().max(1.0);
                    let u0 = (a.x / w).clamp(0.0, 1.0);
                    let v0 = (a.y / h).clamp(0.0, 1.0);
                    let u1 = (b.x / w).clamp(0.0, 1.0);
                    let v1 = (b.y / h).clamp(0.0, 1.0);
                    let pw = ((u0 - u1).abs() * w) as i32;
                    let ph = ((v0 - v1).abs() * h) as i32;
                    if pw >= 32 && ph >= 32 {
                        self.ai.beauty_region_uv = Some([u0, v0, u1, v1]);
                        self.toast(&format!(
                            "AI region set: {pw}\u{00d7}{ph} px  (resolution budget applies here)"
                        ));
                    } else {
                        self.toast("AI region too small (min 32\u{00d7}32) \u{2014} try again");
                    }
                }
                self.ai.beauty_region_draw = false;
            }
            // Consume viewport interaction while drawing the region.
            return;
        }
        // Crash diagnostic: log first interaction after startup / project load
        if self.diag_frame <= 5 {
            let pointer_down = ctx.input(|i| i.pointer.any_down());
            let any_click = response.clicked() || response.secondary_clicked() || response.middle_clicked();
            if pointer_down || any_click {
                eprintln!("[Interact] frame={} active_body={:?} gpu={} prims={} tool_idle={}",
                    self.diag_frame, self.scene.active_body_nid, self.gpu.is_some(),
                    self.scene.primitives.len(), self.tool.current.is_idle());
            }
        }
        let shift=ctx.input(|i|i.modifiers.shift); let alt=ctx.input(|i|i.modifiers.alt); let ctrl=ctx.input(|i|i.modifiers.command);
        // Object-mode long-press LMB → Parent / Match TRS menu (before box-select arms).
        let object_link_owns = self.object_link_tick(ctx, response, rect);
        // Clear one-frame gizmo drag suppression (set by click-to-select)
        if self.tool.suppress_gizmo_drag && !ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            self.tool.suppress_gizmo_drag = false;
        }
        // Stale depth pick guard: clear if tool is no longer idle
        if self.tool.depth_pick.is_some() && !self.tool.current.is_idle() {
            self.tool.depth_pick = None;
        }
        // Close floating popups on any viewport click and surrender text focus
        if response.clicked() || response.dragged_by(egui::PointerButton::Primary) {
            self.viewport.show_grid_settings = false;
            self.ui.ctx_menu_open = false;
            self.ui.shape_swap_pie_open = false;
            // Surrender any UI widget focus so keyboard shortcuts work
            // (but not if a text edit is active — let it handle lost_focus naturally)
            if !ctx.wants_keyboard_input() {
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }
        // Smart Mirror: LMB click = place plane or confirm
        if self.tool.current.is_smart_mirror() && response.clicked_by(egui::PointerButton::Primary) {
            let phase = self.tool.current.as_smart_mirror().unwrap().phase;
            match phase {
                SmartMirrorPhase::Placing => {
                    if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                        st.phase = SmartMirrorPhase::Adjusting;
                        st.base_normal = st.plane_normal;
                        st.drag_start_origin = st.plane_origin;
                    }
                    self.toast("Plane placed \u{2014} Drag=Move, Scroll=Rotate, LMB=Confirm");
                }
                SmartMirrorPhase::Adjusting => {
                    self.confirm_smart_mirror();
                }
            }
        }
        // Smart Mirror: RMB cancels
        if self.tool.current.is_smart_mirror() && response.clicked_by(egui::PointerButton::Secondary) {
            self.tool.current = ActiveTool::Idle;
            self.toast("Mirror cancelled");
        }
        // Clip Curve / Draw Polyline: LMB = add vertex (Ctrl=H/V, Shift=angle snap), RMB = cancel
        if self.tool.current.is_clip_curve_draw() {
            let (ctrl, shift) = ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
            if let Some(mouse) = ctx.input(|i| i.pointer.hover_pos()) {
                self.clip_curve_update_cursor(mouse, rect, ctrl, shift);
                if response.clicked_by(egui::PointerButton::Primary) {
                    self.clip_curve_add_vertex(mouse, rect, ctrl, shift);
                }
            }
            if response.clicked_by(egui::PointerButton::Secondary) {
                let create = self
                    .tool
                    .current
                    .as_clip_curve_draw()
                    .map_or(false, |s| s.create_prim);
                self.tool.current = ActiveTool::Idle;
                self.toast(if create {
                    "Polyline cancelled"
                } else {
                    "Clip curve cancelled"
                });
            }
        }
        // A click that CONFIRMS a modal tool must be CONSUMED. The poly box-select below starts on the
        // LMB PRESS and only checks `is_idle()` — and these confirms just MADE it idle. On a fast click
        // egui reports `button_pressed` AND `clicked_by` in the SAME frame, so the accepting click also
        // started a box-select, whose tiny-box release ran `poly_click_select` => it DESELECTED too.
        // (That is the "inset confirm drops the selection, so E won't extrude" bug.) Owner's model:
        // 1st click accepts the tool, 2nd click deselects, 3rd goes to root — so one click, one action.
        // Library place owns LMB confirm while active (MMB orbit still works).
        let mut tool_consumed_click = library_placing;
        // Set when Alt+LMB region select release already handled click (block double handle_click_select).
        let mut poly_region_click_done = false;
        // RMB that CONFIRMS/CANCELS a modal tool (loop cut, knife, …) must not also open the
        // viewport context menu (owner 2026-08-06). Early confirm paths leave the tool Idle before
        // the shared RMB arm, so without this flag the menu branch still fires.
        let mut rmb_consumed = false;
        // A fresh RMB press starts a new gesture: any leftover swallow intent from a previous
        // gesture (e.g. press-confirm that turned into an orbit, so no click ever fired) is stale.
        // The loop-cut confirm below re-sets it on this same frame when it applies.
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            self.tool.rmb_swallow_click = false;
        }

        // Poly Bevel: LMB click = confirm
        if self.tool.current.is_poly_bevel() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_bevel();
            tool_consumed_click = true;
        }
        // Offset Edge Loop (`I` in Edge mode): LMB click = confirm. Consumes the click here, with
        // the other click-owning modals, so the confirm cannot also fall through to click-select.
        if self.tool.current.is_poly_offset_loop() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_offset_loop();
            tool_consumed_click = true;
        }
        // Curve bevel (persistent-diamond press-drag): the radius follows the horizontal drag
        // each frame while LMB is held; the gesture resolves on RELEASE via
        // curve_bevel_release_direct (drag = commit, unmoved click = corner-type cycle).
        // RMB / Esc cancel (Esc via drop_tool→cancel_operation).
        if self.tool.current.is_curve_bevel() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.curve_bevel_update(mp);
            }
            if response.clicked_by(egui::PointerButton::Secondary) { self.curve_bevel_cancel(); }
        }
        // Curve "Copy angle" pick (Tier 3): the next LMB picks a reference segment. Consumes the
        // click so it doesn't also select/move points. RMB/Esc cancels the pick.
        else if self.curve_copy_angle_arm.is_some() {
            if let Some(mp) = response.interact_pointer_pos() {
                if response.clicked_by(egui::PointerButton::Primary) {
                    self.curve_copy_angle_pick(mp, rect);
                }
            }
            if response.clicked_by(egui::PointerButton::Secondary) {
                self.curve_copy_angle_arm = None;
                self.toast("Copy angle cancelled");
            }
        }
        // Poly Loop Cut: RIGHT-click confirms the cut (owner's choice — a Knife-style exception to the
        // Tool Exit Convention). The session auto-enters on hover, then the BARE CURSOR slides the loop
        // (handled in the session update below); Enter also confirms; Esc cancels.
        // Mark rmb_consumed so the context-menu arm later this frame cannot open.
        if self.tool.current.is_poly_loop_cut_slide() && response.clicked_by(egui::PointerButton::Secondary) {
            self.poly_confirm_loop_cut_slide();
            rmb_consumed = true;
        }
        // Poly Loop Cut: LEFT-click ACCEPTS (owner 2026-08-11 — the click no longer begins the slide,
        // so it is free to accept). It MUST live here rather than in the session block below: this is
        // the only place that can set `tool_consumed_click`, without which the accepting click falls
        // through to `poly_click_select` on the same frame and DESELECTS the loop it just made (the
        // trap documented at the click-select site).
        if self.tool.current.is_poly_loop_cut_slide() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_loop_cut_slide();
            tool_consumed_click = true;
        }
        // Poly Extrude: LMB click = confirm
        if self.tool.current.is_poly_extrude() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_extrude_drag();
            tool_consumed_click = true;
        }
        // Extrude Sweep: LMB = confirm + exit (one-shot).
        if self.tool.current.is_poly_extrude_sweep()
            && response.clicked_by(egui::PointerButton::Primary)
        {
            self.poly_confirm_extrude_sweep();
            tool_consumed_click = true;
        }
        if self.tool.current.is_poly_edge_extrude() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_edge_extrude_drag();
            tool_consumed_click = true;
        }
        // Poly Inset: LMB click = confirm
        if self.tool.current.is_poly_inset() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_inset_drag();
            tool_consumed_click = true;
        }
        // Poly Shrink/Fatten: LMB click = confirm
        if self.tool.current.is_poly_shrink_fatten() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_shrink_fatten();
            tool_consumed_click = true;
        }
        // Edge Weight: a LMB *click* (press+release without motion) confirms; a press that turns into
        // a DRAG is the strength gesture and is handled by the update path — egui only reports
        // `clicked_by` when the pointer did not travel, so the two cannot both fire. Consume it here
        // with the other click-owning modals so the confirm never falls through to click-select.
        if self.tool.current.is_poly_edge_weight() && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_edge_weight_drag();
            tool_consumed_click = true;
        }
        // Face hinge rotate: LMB = pick hinge / gizmo grab / confirm
        if self.tool.current.is_poly_face_hinge() {
            let mp = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or(self.viewport.viewport_mouse);
            let alt_h = ctx.input(|i| i.modifiers.alt);
            if response.clicked_by(egui::PointerButton::Primary)
                || response.drag_started_by(egui::PointerButton::Primary)
            {
                self.poly_face_hinge_click(mp, alt_h);
                tool_consumed_click = true;
            }
            if response.drag_stopped_by(egui::PointerButton::Primary)
                || ctx.input(|i| i.pointer.primary_released())
            {
                self.poly_face_hinge_release();
            }
        }
        // Circle radius handle (S4) while Adjust Circle is open — before general select/gizmo.
        if self.ui.poly_circle_adjust.is_some() && self.tool.current.is_idle() {
            let mp = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(self.viewport.viewport_mouse);
            let primary_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
            let primary_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
            let primary_released = ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
            if self.poly_circle_handle_input(mp, rect, primary_down, primary_pressed, primary_released) {
                tool_consumed_click = true;
            }
        }
        // Poly Transform: LMB click = confirm (keyboard-initiated G/R/S)
        if self.tool.current.is_poly_transform()
            && !self.tool.current.as_poly_transform().map_or(false, |t| t.confirm_on_release)
            && response.clicked_by(egui::PointerButton::Primary) {
            self.poly_confirm_transform();
            tool_consumed_click = true;
        }
        // PARAMETRIC PRIMITIVE chords (owner 2026-08-04): modifier + mouse MOVE, NO button held —
        // the same hover shape the Pipe adjust uses. Alt = segment counts on the two visible
        // screen axes, Alt+Shift = all counts, and the `B` round modal (drag = width, scroll =
        // segments). Runs before the pipe/selection handlers; when it owns the pointer it
        // consumes the click so the gesture can never also select or box-select.
        {
            let mp = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or(self.viewport.viewport_mouse);
            if self.primitive_chords_frame(ctx, mp) {
                tool_consumed_click = true;
            }
        }

        // PIPE live adjust (knife `P`): horizontal drag = radius, LMB/Enter confirm, Esc
        // cancel. Consumes the click so it can never also select or start a paint stroke.
        if self.pipe_adjust_active() {
            let mp = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or(self.viewport.viewport_mouse);
            self.pipe_adjust_drag(mp);
            // RMB CONFIRMS (owner 2026-08-04: "right click lets you confirm the pipe like
            // the knife") -- the same exception to the Tool Exit Convention the knife has.
            // Esc remains the cancel. Consume RMB so the context menu never opens on accept.
            if response.clicked_by(egui::PointerButton::Primary)
                || response.clicked_by(egui::PointerButton::Secondary)
            {
                if response.clicked_by(egui::PointerButton::Secondary) {
                    rmb_consumed = true;
                }
                self.pipe_adjust_confirm();
                tool_consumed_click = true;
            }
        }
        // Knife tool: double-click first point = close shape + cut; LMB = place point.
        // Handle double-click BEFORE single click (egui reports both on the second press).
        //
        // Knife SHAPE (R/C or Tool Params): EXCLUSIVE primary-drag owner — press anchors, drag
        // sizes, release cuts. Must suppress paint select, region select, and free-line placement
        // on the same LMB (owner 2026-08-06: "rectangular shape still paint select").
        //
        // State machine:
        //   • press  = button_pressed OR drag_started (not only click)
        //   • drag   = every frame while primary is down with an anchor
        //   • release = button_released OR drag_stopped only — NEVER bare `!primary_down`
        let mut shape_owned_lmb = false;
        let knife_shape_armed =
            self.poly_tool == PolyTool::Knife && self.knife_shape_kind != 0;
        if knife_shape_armed {
            // Kick any paint/region that leaked under the knife (stuck ActiveTool would make
            // is_idle() false and silently disable the shape path — that was the regression).
            if self.tool.current.is_poly_paint_select()
                || self.tool.current.is_poly_region_select()
                || self.tool.current.is_poly_box_select()
                || self.tool.current.is_poly_circle_select()
            {
                self.tool.current = ActiveTool::Idle;
            }
            shape_owned_lmb = true;
            tool_consumed_click = true;
            let mp = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or(self.viewport.viewport_mouse);
            let (primary_pressed, primary_down, primary_released) = ctx.input(|i| {
                (
                    i.pointer.button_pressed(egui::PointerButton::Primary),
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.button_released(egui::PointerButton::Primary),
                )
            });
            let drag_started = response.drag_started_by(egui::PointerButton::Primary);
            let dragging = response.dragged_by(egui::PointerButton::Primary);
            let drag_stopped = response.drag_stopped_by(egui::PointerButton::Primary);

            // Alt = draw shape from center (press = center, drag = half-extent / radius).
            // Space mid-drag = move whole shape; hold R mid-drag (rect) = rotate;
            // Ctrl while rotating = snap to snap_rotation_deg.
            let from_center = alt;
            let move_held = ctx.input(|i| i.key_down(egui::Key::Space));
            let rotate_held = ctx.input(|i| i.key_down(egui::Key::R));
            let snap_angle = ctrl;
            // Ctrl+D mid-drag rect → linear array (one duplicate, then scroll count).
            if self.knife_shape_anchor.is_some()
                && self.knife_shape_kind == 1
                && (primary_down || dragging)
                && ctx.input(|i| {
                    i.modifiers.command
                        && !i.modifiers.alt
                        && i.key_pressed(egui::Key::D)
                })
            {
                let _ = self.poly_knife_shape_array_ctrl_d();
            }
            // ── DEPTH PHASE (E pressed mid-drag, owner 2026-08-09) ────────────────────────
            // The silhouette is locked; LMB now pulls how deep the cut goes and its RELEASE
            // commits. MMB-orbit is first-class here — that is the whole point of the phase
            // (you cannot judge a depth from the face you drew on) — so orbit FREEZES depth
            // accumulation and un-freezing re-seeds the reference, exactly as the create
            // tool's extrude phase does. Without the freeze the depth would jump by the full
            // reprojection the moment the camera moved.
            let depth_phase = self.knife_shape_depth_mode && self.knife_shape_anchor.is_some();
            if depth_phase {
                let mmb_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
                // The cut extrudes TO the pointer, every frame, button or no button — that is
                // what makes `E` visibly do something. Only an MMB orbit suspends it: during
                // the orbit gesture the pointer sweeps the screen and the depth would swing
                // wildly with it; tracking resumes from wherever the cursor lands after.
                self.knife_shape_depth_frozen = mmb_down;
                if !mmb_down {
                    self.poly_knife_shape_depth_track(mp, response.rect);
                    // Press EDGE only: the sizing drag's still-held LMB never armed a confirm,
                    // so its release ends sizing instead of cutting.
                    if primary_pressed || drag_started {
                        self.poly_knife_shape_depth_press();
                    }
                    if primary_released || drag_stopped {
                        self.poly_knife_shape_depth_release();
                    }
                }
            } else if self.knife_shape_anchor.is_none() && (primary_pressed || drag_started) {
                self.poly_knife_shape_press(mp, response.rect, from_center);
                // Seed cursor on the press frame so a short drag still sizes.
                self.poly_knife_shape_drag(mp, response.rect, from_center, false, false, false);
            } else if self.knife_shape_anchor.is_some() && (primary_down || dragging) {
                self.poly_knife_shape_drag(
                    mp,
                    response.rect,
                    from_center,
                    move_held,
                    rotate_held,
                    snap_angle,
                );
            } else {
                self.knife_shape_rot_last_mouse = None;
                self.knife_shape_move_last_mouse = None;
            }

            // Release only on an explicit up-edge — not "button not held" while idle-armed.
            // Skipped in the depth phase: that branch owns both the sizing release (ends
            // sizing, no cut) and the depth release (commits).
            if !depth_phase && self.knife_shape_anchor.is_some() && (primary_released || drag_stopped)
            {
                self.poly_knife_shape_release();
            }
        }
        if self.poly_tool == PolyTool::Knife
            && self.knife_shape_kind == 0
            && !shape_owned_lmb
            && self.tool.current.is_idle()
        {
            if response.double_clicked_by(egui::PointerButton::Primary) {
                let mp = response.interact_pointer_pos().unwrap_or(response.rect.center());
                if self.poly_knife_try_close(mp, response.rect) {
                    tool_consumed_click = true;
                } else if response.clicked_by(egui::PointerButton::Primary) {
                    self.poly_knife_click();
                }
            } else if response.clicked_by(egui::PointerButton::Primary) {
                self.poly_knife_click();
            }
        }
        // Scroll / Ctrl+Scroll / Alt+Scroll during creation; else camera zoom.
        // Modal poly-bevel also consumes scroll when the pointer is not over the viewport
        // response (e.g. slightly off the mesh / over empty chrome) so segments still change.
        //
        // DRAG-SELECT PROTECTION (owner 2026-08-06): while any rubber-band / paint select
        // owns the pointer, scroll-bevel / knife-shape segment wheels must not run — they
        // share the wheel channel and one leak mutates geometry mid-selection.
        let drag_select_active = self.tool.current.is_drag_select_tool();
        if response.hovered() || self.tool.current.is_poly_bevel() {
            // Use dominant axis of scroll: Windows + Shift often remaps vertical wheel → X.
            let (sc, raw_sc) = ctx.input(|i| {
                let s = i.smooth_scroll_delta;
                let r = i.raw_scroll_delta;
                let sc = if s.y.abs() >= s.x.abs() { s.y } else { s.x };
                let raw = if r.y.abs() >= r.x.abs() { r.y } else { r.x };
                (sc, raw)
            });
            // egui converts Ctrl+Scroll into zoom_delta on Windows, so read that too
            let zoom = ctx.input(|i| i.zoom_delta());
            let ctrl_scroll = ctrl && (zoom - 1.0).abs() > 0.001;

            // PARAMETRIC ROUND MODAL (`B` on a primitive): bare scroll = segments, matching the
            // poly bevel modal. Runs before every other scroll consumer and returns, so the
            // camera can never also zoom on the same event. Declines unless the modal is live.
            //
            // Deliberately NOT gated on `!ctrl`: both modals are ENTERED with Ctrl held
            // (`Ctrl+B` / `Ctrl+R`), so the user is still holding it when they scroll — that guard
            // sent the wheel straight to Ctrl+Scroll dolly zoom (owner: "bevel scroll still
            // registers camera movement"). A live modal owns the wheel regardless of modifiers;
            // Shift still means all-orientations inside the segments modal.
            //
            // Hard-gated off during drag-select so a stuck modal can never steal the wheel
            // mid rubber-band.
            if !drag_select_active {
                // DISCRETE wheel notches, from egui's `Event::MouseWheel` stream.
                //
                // Every delta-based source lies here. `smooth_scroll_delta` interpolates one
                // physical notch across many frames and `raw_scroll_delta` is repeated while that
                // decays, so accumulating either counted the SAME notch dozens of times — one tick
                // was landing 25 segments. The event stream is emitted once per physical notch,
                // which is the only source that can honour "one segment per tick".
                let ticks: f32 = ctx.input(|i| {
                    i.events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { unit, delta, .. } => {
                                // Shift remaps vertical wheel to X on Windows, so fold both axes.
                                let d = if delta.y.abs() >= delta.x.abs() { delta.y } else { delta.x };
                                Some(match unit {
                                    egui::MouseWheelUnit::Line => d,
                                    // Trackpads report points; ~50pt ≈ one notch of intent.
                                    egui::MouseWheelUnit::Point => d / 50.0,
                                    egui::MouseWheelUnit::Page => d * 10.0,
                                })
                            }
                            _ => None,
                        })
                        .sum()
                });
                if ticks != 0.0 && self.primitive_modal_scroll(ticks, ctrl || alt) {
                    return; // consumed
                }
                // Swallow the LEFTOVER smooth-scroll decay that follows a notch, so the camera
                // does not zoom underneath the modal on the frames after a wheel event.
                //
                // This must only fire when there is actually residual wheel to eat. It was first
                // written as an unconditional `return`, which exited `handle_viewport_input`
                // outright on every frame the modal was open — taking MMB camera orbit (further
                // down this same function) with it (owner: "I still cannot rotate after adding
                // segments"). Never blanket-return from a shared input function to protect one
                // consumer.
                let residual = sc.abs() > 0.01 || raw_sc.abs() > 0.01 || ctrl_scroll;
                if residual && self.primitive_modal_scroll_active() {
                    return;
                }
            }

            // PIPE: plain scroll changes the segment count while the live adjust runs.
            if self.pipe_adjust_active() && !ctrl && !shift {
                let wheel = if sc.abs() > 0.01 { sc } else { raw_sc };
                if wheel.abs() > 0.01 {
                    self.pipe_adjust_scroll(wheel);
                    return; // consumed: never also zoom the camera on this event
                }
            }

            // ── Ctrl+Shift+Scroll = GROW / SHRINK the poly selection (owner request) ──
            // Runs before every other scroll consumer and then ZEROES the wheel, so the camera
            // never also zooms on the same event. One BFS step per notch, same ops the menu and
            // the Ctrl+Plus/Minus binds call. Only meaningful inside a poly sub-mode.
            let mut sel_scroll_done = false;
            if ctrl
                && shift
                && self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode != PolyWorkMode::Object
            {
                // Windows delivers Ctrl+wheel as zoom_delta rather than scroll, so accept either.
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc
                } else if (zoom - 1.0).abs() > 0.001 {
                    if zoom > 1.0 { 1.0 } else { -1.0 }
                } else {
                    0.0
                };
                if wheel > 0.0 {
                    self.poly_grow_selection();
                    sel_scroll_done = true;
                } else if wheel < 0.0 {
                    self.poly_shrink_selection();
                    sel_scroll_done = true;
                }
            }
            // Consume: shadow every wheel input the downstream handlers read.
            let sc = if sel_scroll_done { 0.0 } else { sc };
            let raw_sc = if sel_scroll_done { 0.0 } else { raw_sc };
            let zoom = if sel_scroll_done { 1.0 } else { zoom };
            let ctrl_scroll = ctrl_scroll && !sel_scroll_done;

            // Polyline / Spine point editors: scroll over a handle cycles corner TYPE
            // (Sharp→Round→Chamfer). Contextual — only when idle, hovering a handle; consumes the
            // scroll so the camera doesn't also zoom. Computed BEFORE the do_zoom closure (which
            // immutably borrows self for the rest of the block).
            let mut curve_scroll_done = false;
            if self.tool.current.is_idle() && self.spine_pt_drag.is_none() && sc.abs() > 0.1 {
                if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                    if let Some(cidx) = self.curve_edit_prim_index() {
                        curve_scroll_done = self.curve_pt_scroll_cycle(cidx, mp, rect, sc > 0.0);
                    } else if let Some(sidx) = self.spine_edit_prim_index() {
                        curve_scroll_done = self.spine_pt_scroll_cycle(sidx, mp, rect, sc > 0.0);
                    }
                }
            }
            // Spine editor (exclusive tool — NOT idle): scroll over a point cycles its corner
            // type; consumes the scroll so the camera doesn't also zoom. Off-point scrolls
            // fall through to normal camera zoom.
            else if self.spine_pt_drag.is_none() && sc.abs() > 0.1 {
                if let (Some(sidx), Some(mp)) = (self.spine_edit_prim_index(), ctx.input(|i| i.pointer.hover_pos())) {
                    curve_scroll_done = self.spine_pt_scroll_cycle(sidx, mp, rect, sc > 0.0);
                }
            }

            // Camera zoom helper (multiplicative for consistent feel)
            let do_zoom = |cam_dist: &mut f32, sc: f32, dirty: &mut bool| {
                let factor = (1.0 - sc * self.tweak.cam_zoom_speed).clamp(0.8, 1.25);
                *cam_dist = (*cam_dist * factor).clamp(self.tweak.cam_zoom_min, self.tweak.cam_zoom_max);
                *dirty = true;
            };
            // Zoom toward cursor: keep world point under mouse stable (follows view angle).
            let do_zoom_to_cursor = |app: &mut Self, sc: f32, mouse: Option<egui::Pos2>, r: egui::Rect| {
                let old_dist = app.viewport.camera.distance;
                let factor = (1.0 - sc * app.tweak.cam_zoom_speed).clamp(0.8, 1.25);
                let new_dist = (old_dist * factor).clamp(app.tweak.cam_zoom_min, app.tweak.cam_zoom_max);
                if (new_dist - old_dist).abs() < 1e-8 {
                    return;
                }
                if let Some(mp) = mouse {
                    if r.contains(mp) {
                        // World point under cursor before zoom (ray vs plane at orbit target depth).
                        let (origin, dir) = app.cam_ray(mp, r);
                        let target = app.viewport.camera.target;
                        let cam_pos = target
                            + app.viewport.camera.orbit * Vec3::new(0.0, 0.0, old_dist);
                        let view = (target - cam_pos).normalize_or_zero();
                        // Intersect ray with plane through target, facing camera
                        let denom = dir.dot(view);
                        let hit = if denom.abs() > 1e-5 {
                            let t = (target - origin).dot(view) / denom;
                            if t.is_finite() && t > 0.0 {
                                Some(origin + dir * t)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(p) = hit {
                            // After zoom, cam moves along orbit; shift target so p stays under cursor.
                            app.viewport.camera.distance = new_dist;
                            let cam_pos2 = target
                                + app.viewport.camera.orbit * Vec3::new(0.0, 0.0, new_dist);
                            let (o2, d2) = app.cam_ray(mp, r);
                            let view2 = (app.viewport.camera.target - cam_pos2).normalize_or_zero();
                            let denom2 = d2.dot(view2);
                            if denom2.abs() > 1e-5 {
                                let t2 = (app.viewport.camera.target - o2).dot(view2) / denom2;
                                if t2.is_finite() && t2 > 0.0 {
                                    let p2 = o2 + d2 * t2;
                                    app.viewport.camera.target += p - p2;
                                }
                            }
                            app.is_dirty = true;
                            app.mark_dirty(DirtyFlags::RENDER);
                            return;
                        }
                    }
                }
                app.viewport.camera.distance = new_dist;
                app.is_dirty = true;
                app.mark_dirty(DirtyFlags::RENDER);
            };

            // Linear/circle array modal + Shift+D gizmo array session: consume scroll FIRST.
            // Shift is axis-lock for free grab — must NOT block count scroll.
            // Shift+wheel on Windows often arrives as horizontal (X) — handled above via dominant axis.
            let array_modal = self.tool.current.is_transform()
                && self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |ts| ts.shift_duplicate);
            let array_dup_sess = self.array_dup_active();
            if array_modal || array_dup_sess {
                // Prefer smooth; if zero (some Shift remaps), use raw as one notch.
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc.signum() * 55.0
                } else if ctrl_scroll {
                    if zoom > 1.0 { 55.0 } else if zoom < 1.0 { -55.0 } else { 0.0 }
                } else {
                    0.0
                };
                if wheel != 0.0 {
                    if array_dup_sess {
                        self.array_dup_scroll(wheel);
                    } else {
                        self.transform_array_scroll(wheel);
                    }
                }
            } else if curve_scroll_done {
                // handled — corner type cycled
            }
            // Circle / paint select: scroll resizes the brush — intentional select UX,
            // NOT scroll-bevel. Allowed during drag_select_active.
            else if self.tool.current.is_poly_circle_select() && sc.abs() > 0.1 {
                // Circle select: scroll = resize radius
                if let ActiveTool::PolyCircleSelect(ref mut cs) = self.tool.current {
                    cs.radius = (cs.radius + sc * 2.0).clamp(8.0, 200.0);
                }
            } else if self.tool.current.is_poly_paint_select() && sc.abs() > 0.1 {
                // Paint select: scroll = resize the brush (owner request). Persists between strokes so
                // the chosen size sticks. Consumes the scroll so the camera doesn't also zoom. `sc`
                // (smooth_scroll_delta) is large per notch on this system — a gentle 0.13 factor + a
                // per-event clamp keeps one notch to a few px (was ~15x too sensitive at *2.0).
                let d = (sc * 0.13).clamp(-8.0, 8.0);
                self.poly_paint_radius = (self.poly_paint_radius + d).clamp(2.0, 200.0);
            }
            // ── Scroll-bevel / segment consumers: never while drag-select owns the pointer ──
            else if !drag_select_active && self.tool.current.is_create() {
                // Bevel sensitivity proportional to box size (consistent feel at any scale)
                let bevel_scale = if let Some(cs) = self.tool.current.as_create() {
                    let box_size = cs.local_u.abs().max(cs.local_v.abs()).max(cs.depth.abs()).max(0.2);
                    box_size * self.tweak.bevel_scroll_speed
                } else { self.tweak.bevel_drag_speed };
                let max_r = if let Some(cs) = self.tool.current.as_create() {
                    let min_he = cs.local_u.abs().min(cs.local_v.abs()).max(0.01) * 0.5;
                    min_he
                } else { 2.0 };

                if ctrl_scroll {
                    // Ctrl+Scroll = bevel top + bottom edges (0-7)
                    let zsc = (zoom - 1.0) * self.tweak.bevel_scroll_mult;
                    if let Some(cs) = self.tool.current.as_create_mut() {
                        cs.corner_radius_tb = (cs.corner_radius_tb + zsc * bevel_scale).clamp(0.0, max_r);
                    }
                    self.update_create_bevel();
                } else if alt && sc.abs() > 0.1 {
                    // Alt+Scroll = blend_k
                    if let Some(cs) = self.tool.current.as_create_mut() {
                        cs.blend_k = (cs.blend_k + sc * self.tweak.blend_k_speed)
                            .clamp(0.0, self.tweak.blend_k_max.max(0.0));
                        let prim_idx = cs.prim_idx;
                        let blend_k = cs.blend_k;
                        let op = cs.op;
                        {
                            let op_val = op.to_boolean_op(blend_k);
                            self.write_prim(prim_idx, |p| p.operation = op_val);
                        }
                    }
                    self.tool.create_blend_k = self.tool.current.as_create().map(|cs| cs.blend_k).unwrap_or(0.0);
                    self.rebake_no_undo();
                } else if sc.abs() > 0.1 {
                    // Scroll = bevel vertical edges (8-11)
                    if let Some(cs) = self.tool.current.as_create_mut() {
                        cs.corner_radius = (cs.corner_radius + sc * bevel_scale).clamp(0.0, max_r);
                    }
                    self.update_create_bevel();
                }
            } else if !drag_select_active
                && self.poly_tool == PolyTool::Knife
                && self.knife_shape_scroll_armed()
            {
                // Knife shape mid-drag only.
                // Rect: plain scroll = corner bevel amount; Shift+scroll = bevel segments.
                // Circle: scroll = segments.
                //
                // ONE WHEEL CONVENTION. This branch used to decode `Event::MouseWheel` itself
                // and treat one Line unit as one notch. A physical notch arrives here as ~3
                // Line units, so every tick stepped THREE segments (owner: "circle segment
                // scroll is ~3x too fast"). Reading the wheel a second, different way is the
                // bug — the array modal, poly bevel and extrude sweep all take smooth-scroll
                // units and divide by `WHEEL_UNITS_PER_NOTCH`, and so does this now.
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc.signum() * WHEEL_UNITS_PER_NOTCH
                } else if ctrl_scroll {
                    if zoom > 1.0 {
                        WHEEL_UNITS_PER_NOTCH
                    } else if zoom < 1.0 {
                        -WHEEL_UNITS_PER_NOTCH
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                // `poly_knife_shape_scroll` counts in NOTCHES (its accumulator threshold is 1.0
                // and its bevel step is per-notch), so convert here rather than there.
                let notches = wheel / WHEEL_UNITS_PER_NOTCH;
                if notches.abs() > 1e-6 {
                    let _ = self.poly_knife_shape_scroll(notches, shift);
                }
            } else if !drag_select_active && self.tool.current.is_poly_bevel() {
                // Poly Bevel: scroll = segments. Same robust wheel path as array-count scroll —
                // Windows+Shift remaps vertical wheel to X (handled above); Ctrl+Scroll becomes
                // zoom_delta with smooth_scroll≈0 (owner: "scroll / accel segments does nothing").
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc.signum() * 55.0
                } else if ctrl_scroll {
                    if zoom > 1.0 { 55.0 } else if zoom < 1.0 { -55.0 } else { 0.0 }
                } else {
                    0.0
                };
                if wheel != 0.0 {
                    self.poly_bevel_scroll_segments(wheel);
                }
            } else if !drag_select_active && self.tool.current.is_poly_extrude_sweep() {
                // Extrude Sweep: scroll = path segments for smoother duct turn.
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc.signum() * 55.0
                } else {
                    0.0
                };
                if wheel.abs() > 0.01 {
                    self.poly_extrude_sweep_scroll(wheel);
                }
            } else if self.tool.current.is_poly_loop_cut_slide() && sc.abs() > 0.01 {
                // Loop Cut slide: scroll = number of evenly-spaced cuts
                self.poly_loop_cut_scroll_count(sc);
            } else if shift
                && !ctrl
                && !alt
                && self.tool.current.is_idle()
                && self.modeling_context == ModelingContext::Polygonal
                && matches!(
                    self.poly_work_mode,
                    // Vertex included (owner 2026-08-08): green vert chain + Shift+scroll was a
                    // no-op because this match omitted Vertex — Blender Next Active works in all
                    // three element modes.
                    PolyWorkMode::Vertex
                        | PolyWorkMode::Edge
                        | PolyWorkMode::Face
                        | PolyWorkMode::Group
                )
            {
                // SHIFT+Scroll = Blender "Next/Prev Active": walk one element along the
                // direction defined by selection history (last two selected).
                //
                // Wheel robustness (same contract as array / bevel / knife segments):
                // Windows+Shift remaps vertical wheel → X (dominant-axis fold above). Prefer
                // smooth_scroll; if zero, one physical notch of raw_scroll; never require the
                // resolve() binding alone as the only gate when the default is live — still
                // honour a user unbind (resolve returns None for loop_extend_*).
                let wheel = if sc.abs() > 0.01 {
                    sc
                } else if raw_sc.abs() > 0.01 {
                    raw_sc.signum() * WHEEL_UNITS_PER_NOTCH
                } else {
                    0.0
                };
                if wheel != 0.0 {
                    let grow = wheel > 0.0;
                    let bound = self.input_system.resolve(
                        if grow {
                            crate::input_system::InputEvent::ScrollUp
                        } else {
                            crate::input_system::InputEvent::ScrollDown
                        },
                        crate::input_system::InputMods::SHIFT,
                        crate::input_system::InputContext::Viewport,
                    );
                    // Fire when bound to loop_extend_*, OR when no Viewport Shift+scroll binding
                    // exists at all (defaults not loaded yet). Skip only if remapped away.
                    let allow = match bound {
                        Some(id) => id.starts_with("loop_extend"),
                        None => true, // no competing bind → use built-in Next Active
                    };
                    if allow {
                        self.tool.loop_walk_scroll_accum += wheel;
                        // One element per physical notch (same unit as WHEEL_UNITS_PER_NOTCH).
                        const LOOP_WALK_STEP: f32 = WHEEL_UNITS_PER_NOTCH;
                        while self.tool.loop_walk_scroll_accum >= LOOP_WALK_STEP {
                            self.tool.loop_walk_scroll_accum -= LOOP_WALK_STEP;
                            self.poly_loop_extend(true);
                        }
                        while self.tool.loop_walk_scroll_accum <= -LOOP_WALK_STEP {
                            self.tool.loop_walk_scroll_accum += LOOP_WALK_STEP;
                            self.poly_loop_extend(false);
                        }
                    }
                }
            } else if self.tool.current.is_poly_transform() && self.poly_proportional && sc.abs() > 0.1 {
                // Proportional editing: scroll = adjust influence radius (via FalloffKind)
                self.poly_prop_radius = (self.poly_prop_radius + sc * 0.05).clamp(0.05, 50.0);
                if let Some(ts) = self.tool.current.as_poly_transform_mut() {
                    if let FalloffKind::Proportional { radius, .. } = &mut ts.falloff {
                        *radius = self.poly_prop_radius;
                    }
                }
            } else if self.tool.current.is_smart_mirror() && sc.abs() > 0.1 {
                // Smart Mirror: scroll = rotate plane
                let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
                if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                    if st.phase == SmartMirrorPhase::Adjusting {
                        st.rotation_angle += sc * self.tweak.mirror_rotate_speed;
                        if ctrl { st.rotation_angle = snap_angle(st.rotation_angle, self.tweak.mirror_snap_angle); }
                        let rot = Quat::from_axis_angle(cam_fwd, st.rotation_angle);
                        st.plane_normal = (rot * st.base_normal).normalize();
                        let sn = st.surface_normal;
                        let (tu, tv) = crate::mirror_tangent_frame(st.plane_normal, sn);
                        st.tangent_u = tu; st.tangent_v = tv;
                        let abs_n = st.plane_normal.abs();
                        st.snapped_axis = if abs_n.x >= abs_n.y && abs_n.x >= abs_n.z { 0 }
                                          else if abs_n.y >= abs_n.z { 1 } else { 2 };
                    }
                }
            } else if self.ui.sdf_cycle_mode && sc.abs() > 0.1
                      && self.tool.current.is_idle()
            {
                // Cycle mode: scroll steps through prims in active body (outliner order)
                if let Some(prims) = self.scene.scene_graph.body_primitives(self.scene.active_body_nid) {
                    let body_uids: Vec<u32> = prims.iter().map(|p| p.prim_uid).collect();
                    if !body_uids.is_empty() {
                        let current_uid = self.sel.usel.active_prim_uid();
                        let current_idx = current_uid
                            .and_then(|uid| body_uids.iter().position(|u| *u == uid))
                            .unwrap_or(0);
                        let next_idx = if sc > 0.0 {
                            // Scroll up = next prim
                            (current_idx + 1) % body_uids.len()
                        } else {
                            // Scroll down = previous prim
                            current_idx.checked_sub(1).unwrap_or(body_uids.len() - 1)
                        };
                        self.select_single_unified(SceneItemId::Prim(body_uids[next_idx]));
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            } else if alt && sc.abs() > 0.1
                      && self.tool.work_mode == WorkMode::Object
                      && self.tool.current.is_idle()
                      && self.sel.usel.has_nodes() && !self.sel.usel.has_prims()
            {
                // Body-level Alt+Scroll: cycle root SDF bodies (threshold-accumulated)
                self.tool.body_cycle_scroll_accum += sc;
                let threshold = 50.0;
                let root_bodies: Vec<core_math::scene_graph::NodeId> = self.scene.scene_graph
                    .children(self.scene.scene_graph.root())
                    .iter()
                    .copied()
                    .filter(|&nid| self.scene.scene_graph.get(nid)
                        .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                        .unwrap_or(false))
                    .collect();
                if root_bodies.len() > 1 {
                    let mut current_idx = root_bodies.iter().position(|&nid| nid == self.scene.active_body_nid).unwrap_or(0);
                    let len = root_bodies.len();
                    let mut stepped = false;
                    while self.tool.body_cycle_scroll_accum > threshold {
                        self.tool.body_cycle_scroll_accum -= threshold;
                        current_idx = (current_idx + 1) % len;
                        stepped = true;
                    }
                    while self.tool.body_cycle_scroll_accum < -threshold {
                        self.tool.body_cycle_scroll_accum += threshold;
                        current_idx = current_idx.checked_sub(1).unwrap_or(len - 1);
                        stepped = true;
                    }
                    if stepped {
                        let next_nid = root_bodies[current_idx];
                        self.scene.active_body_nid = next_nid;
                        self.select_single_unified(SceneItemId::Node(next_nid));
                        self.outliner.outliner_dirty = true;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            } else if alt && sc.abs() > 0.1
                      && self.tool.work_mode == WorkMode::Object
                      && self.tool.current.is_idle()
            {
                // Depth scroll pick: Alt+Scroll cycles overlapping shapes under cursor
                if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                    if let Some(ref mut dp) = self.tool.depth_pick {
                        // Accumulate smooth scroll; step only on full notch (~50 units)
                        dp.scroll_accum += sc;
                        let threshold = 50.0;
                        let mut stepped = false;
                        let len = dp.candidates.len();
                        while dp.scroll_accum > threshold {
                            dp.scroll_accum -= threshold;
                            dp.index = dp.index.checked_sub(1).unwrap_or(len - 1);
                            stepped = true;
                        }
                        while dp.scroll_accum < -threshold {
                            dp.scroll_accum += threshold;
                            dp.index = (dp.index + 1) % len;
                            stepped = true;
                        }
                        if stepped {
                            let uid = dp.candidates[dp.index];
                            dp.origin_mouse = mp; // reset drift anchor on each cycle
                            self.select_single_unified(SceneItemId::Prim(uid));
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    } else {
                        let all = self.raycast_select_all(mp, rect);
                        if !all.is_empty() {
                            let prev_usel = self.sel.usel.clone();
                            let candidates: Vec<u32> = all.iter()
                                .filter_map(|(i,_)| self.scene.primitives.get(*i).map(|p| p.prim_uid))
                                .collect();
                            if let Some(&first_uid) = candidates.first() {
                                self.tool.depth_pick = Some(DepthPickState {
                                    candidates,
                                    index: 0,
                                    origin_mouse: mp,
                                    prev_usel,
                                    scroll_accum: 0.0,
                                });
                                self.select_single_unified(SceneItemId::Prim(first_uid));
                                self.toast("Depth pick: scroll to cycle, click to confirm");
                            }
                        }
                    }
                }
            } else if self.poly_tool == PolyTool::LoopCut && sc.abs() > 0.1 {
                // Loop cut: scroll adjusts cut count
                if sc > 0.0 { self.loop_cut_count = (self.loop_cut_count + 1).min(10); }
                else { self.loop_cut_count = self.loop_cut_count.saturating_sub(1).max(1); }
            } else if ctrl_scroll && self.tool.current.is_idle() {
                // Ctrl+Scroll = reorder selection in hierarchy (up/down)
                // Shift+Ctrl+Scroll = move to first/last
                let shift = ctx.input(|i| i.modifiers.shift);
                let dir = if zoom > 1.0 {
                    if shift { crate::hierarchy_order::ReorderDirection::First }
                    else { crate::hierarchy_order::ReorderDirection::Up }
                } else {
                    if shift { crate::hierarchy_order::ReorderDirection::Last }
                    else { crate::hierarchy_order::ReorderDirection::Down }
                };
                self.reorder_selection(dir);
            } else if self.ui.light_color_popup.is_some() && sc.abs() > 0.1 {
                // Quick light popup open (C): scroll = INTENSITY, camera zoom
                // suppressed (owner 2026-08-07). ~15% per wheel notch, multiplicative
                // so it feels uniform across magnitudes; rides the popup's undo episode.
                if let Some((lid, _)) = self.ui.light_color_popup {
                    if let Some(l) = self.scene.light_by_id(lid) {
                        let ni = (l.intensity * (1.15f32).powf(sc / 50.0)).clamp(0.001, 10000.0);
                        self.write_light(lid, |l| l.intensity = ni);
                        self.mark_dirty(DirtyFlags::LIGHT);
                    }
                }
            } else if library_placing {
                // Library place owns the wheel for part resize — do not also camera-zoom.
            } else if sc.abs() > 0.1 {
                // Zoom toward mouse along camera view (keeps angle + cursor point stable)
                let mp = ctx.input(|i| i.pointer.hover_pos());
                do_zoom_to_cursor(self, sc, mp, rect);
            }
        }
        // Edge bevel hover (when bevel mode active OR box/cylinder with bevel selected).
        // Suppress while Elongate gizmo is armed: uniform-bevel edges/yellow square were
        // clearing gizmo_hovered so face-dot resize never started on rounded cubes.
        self.tool.hovered_bevel_edge = None;
        // Keep yellow-square hover while dragging it so release still sees the tool.
        if !self.tool.current.as_bevel().map_or(false, |b| b.uniform_all) {
            self.tool.uniform_bevel_hovered = false;
        }
        // Bevel edge hover / handles: ONLY while ToolMode::Bevel is on.
        // Previously also showed for any prim that already had bevel radii — so after
        // switching tools the B chrome stayed visible ("B UI does not go away").
        let sel_prim_indices = self.selected_prim_indices_unified();
        let show_bevel_edges = self.tool.mode.is_bevel();
        if show_bevel_edges && response.hovered() && self.tool.work_mode==WorkMode::Object && sel_prim_indices.len()==1
           && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
           && !self.tool.current.is_selection_box() {
            let idx = sel_prim_indices[0];
            if let Some(prim) = self.scene.primitives.get(idx) {
                let bt = self.scene.prim_body_map.get(idx)
                    .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
                    .unwrap_or_default();
                if let SdfShape::Box{half_extents, bevel} = &prim.shape {
                    let he = *half_extents;
                    let edges = Self::box_bevel_edges(prim.position, prim.rotation_quat(), prim.scale, he, bevel.up_axis, &bt);
                    let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0,0.0,self.viewport.camera.distance);
                    if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
                        let aspect = rect.width() / rect.height().max(1.0);
                        // Yellow Uniform square — same placement as draw (center + off silhouette).
                        let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
                        let world_sq = Self::uniform_bevel_handle_world(
                            prim.position,
                            prim.rotation_quat(),
                            prim.scale,
                            he,
                            &bt,
                            cam_right,
                        );
                        if let Some(sq_sp) = world_to_screen(
                            world_sq, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg,
                        ) {
                            let sq_rect = egui::Rect::from_center_size(sq_sp, egui::vec2(26.0, 26.0));
                            if sq_rect.contains(mp) {
                                self.tool.uniform_bevel_hovered = true;
                            }
                        }
                        if !self.tool.uniform_bevel_hovered {
                            self.tool.hovered_bevel_edge = Self::detect_bevel_hover(&edges, mp, rect, cam_pos, self.viewport.camera.orbit, ortho_hw, self.tweak.bevel_hover_px, self.render.fov_deg);
                        }
                    }
                } else if let SdfShape::Cylinder{radius, half_height} = &prim.shape {
                    let segs = Self::cylinder_bevel_segments(prim.position, prim.rotation_quat(), prim.scale, *radius, *half_height, &bt);
                    let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0,0.0,self.viewport.camera.distance);
                    if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
                        self.tool.hovered_bevel_edge = Self::detect_cyl_bevel_hover(&segs, mp, rect, cam_pos, self.viewport.camera.orbit, ortho_hw, self.tweak.bevel_hover_px, self.render.fov_deg);
                    }
                }
            }
        }
        // Face mode hover detection (isolated to edit_target when in edit mode)
        self.tool.hovered_face = None;
        if response.hovered() && self.tool.work_mode == WorkMode::Face
           && !self.tool.current.is_face_push_pull() && !self.tool.current.is_gizmo()
           && !self.tool.current.is_transform() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.tool.hovered_face = if let Some(target) = self.sel.edit_target {
                    self.detect_hovered_face_single(mp, rect, target)
                } else {
                    self.detect_hovered_face(mp, rect)
                };
            }
        }
        // Primitive hover detection (for viewport highlight)
        // Block SDF hover while in poly/nurbs sub-element edit mode
        let in_sub_edit = (self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object)
            || (self.modeling_context == ModelingContext::Nurbs);
        self.tool.hovered_prim = None;
        if response.hovered() && self.tool.work_mode == WorkMode::Object && !in_sub_edit
           && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
           && !self.tool.current.is_selection_box() && !self.tool.current.is_create() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.tool.hovered_prim = self.raycast_select(mp, rect, false);
            }
        }
        // Polygon sub-element hover picking.
        //
        // UNCONDITIONAL CLEAR FIRST — the same shape as `tool.hovered_face` (line ~1165) and
        // `tool.hovered_prim` (line ~1181) just above. `poly_hover_pick` is the ONLY writer that
        // resets these four fields, and it runs solely while the pointer is over the viewport, so
        // moving the cursor onto the properties panel / outliner / menu bar / another window FROZE
        // the last hovered element with its overlay still painted (owner 2026-08-11: "I still have
        // a pale highlight sticking even if the poly is no longer selected" — a hovered edge draws
        // in `hover_color` at 0.7x width, i.e. a pale version of the selection line, and it is
        // independent of `selected_edges`, so deselecting does not remove it).
        //
        // A frozen hover is not merely cosmetic: `poly_click_select` selects
        // `poly_sel.hovered_edge`/`_face`/`_vertex`, so a stale value is a click that lands on the
        // element the cursor last touched instead of the one under it.
        self.poly_sel.hovered_vertex = None;
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_face = None;
        self.poly_sel.hovered_face_other = None;
        if self.modeling_context == ModelingContext::Polygonal
           && self.poly_work_mode != PolyWorkMode::Object
           && response.hovered() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.poly_hover_pick(mp, rect);
                // Knife tool snap preview
                if self.poly_tool == PolyTool::Knife {
                    self.poly_knife_hover(mp, rect, ctrl, shift);
                }
            }
        }
        // NURBS hover (modeless Object = vert>edge>face; locked modes filter)
        if self.modeling_context == ModelingContext::Nurbs
           && self.nurbs_solid_root().is_some()
           && response.hovered() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.nurbs_hover_pick(mp, rect);
            }
        }
        // Polyline (CurveExtrude) profile-point editor: owner-sync + hover refresh each frame.
        // Skipped while a drag tool owns the pointer (transform/gizmo/box) to avoid stale picks.
        if !self.tool.current.is_curve_point_transform() && !self.tool.current.is_gizmo() {
            let hover = if response.hovered() { ctx.input(|i| i.pointer.hover_pos()) } else { None };
            self.curve_pt_tick(hover, rect);
            // Spine point editor: owner-sync + hover refresh (skipped mid-drag so the
            // hover pick doesn't fight the in-flight point drag).
            if self.spine_pt_drag.is_none() {
                self.spine_pt_tick(hover, rect);
            }
        }
        // BoolCut depth phase: continuous mouse-Y depth (no LMB required)
        if self.tool.bool_cut.as_ref().map_or(false, |bc| bc.phase == BoolCutPhase::Depth) {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()).filter(|mp| rect.contains(*mp)) {
                self.update_bool_cut_depth(mp, rect);
            }
        }
        // Depth pick cancellation: drift >20px restores, Alt release confirms current pick
        if self.tool.depth_pick.is_some() {
            let should_cancel = if let Some(ref dp) = self.tool.depth_pick {
                ctx.input(|i| i.pointer.hover_pos())
                    .map_or(false, |mp| (mp - dp.origin_mouse).length() > self.tweak.depth_pick_drift_px)
            } else { false };
            if should_cancel {
                let restored = self.tool.depth_pick.as_ref().unwrap().prev_usel.clone();
                self.tool.depth_pick = None;
                self.sel.usel = restored;
                self.mark_dirty(DirtyFlags::RENDER);
            } else if !alt {
                // Alt released: confirm current pick (keep selection, clear state)
                self.tool.depth_pick = None;
            }
        }
        // Reset body cycle scroll accumulator when Alt released
        if !alt { self.tool.body_cycle_scroll_accum = 0.0; }
        // Smart Mirror: update plane each frame (use rect.contains instead of response.hovered
        // to avoid issues with floating egui elements stealing hover)
        if self.tool.current.is_smart_mirror() {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()).filter(|mp| rect.contains(*mp)) {
                // Read phase + from_workplane before mutable borrow
                let phase = self.tool.current.as_smart_mirror().unwrap().phase;
                let _from_wp = self.tool.current.as_smart_mirror().unwrap().from_workplane;
                // Update action from current modifiers
                if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                    st.action = if shift && alt { SmartMirrorAction::DeleteHalf }
                                else if shift { SmartMirrorAction::Instance }
                                else { SmartMirrorAction::Mirror };
                }
                match phase {
                    SmartMirrorPhase::Placing => {
                        // Compute new plane position (need &self methods before &mut self)
                        let (origin, dir) = self.cam_ray(mp, rect);
                        let wp_origin = self.viewport.wp_resolved.origin;
                        let wp_normal = self.viewport.wp_resolved.normal;
                        let wp_locked = self.viewport.wp_mode == WorkPlaneMode::Locked;
                        let cam_orbit = self.viewport.camera.orbit;
                        let snap_grid = self.viewport.snap_grid;
                        if wp_locked {
                            if let Some(hit) = self.ray_plane_hit(mp, rect, wp_origin, wp_normal) {
                                if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                    st.plane_origin = if ctrl { crate::snap_to_grid(hit, snap_grid) } else { hit };
                                }
                            }
                        } else if let Some((hit_pos, hit_normal)) = self.raycast_scene(origin, dir) {
                            let cam_fwd = (cam_orbit * Vec3::NEG_Z).normalize();
                            let proj = cam_fwd - hit_normal * cam_fwd.dot(hit_normal);
                            let pn = if proj.length_squared() > 0.001 {
                                proj.normalize()
                            } else {
                                let cam_right = (cam_orbit * Vec3::X).normalize();
                                (cam_right - hit_normal * cam_right.dot(hit_normal)).normalize()
                            };
                            if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                st.plane_origin = if ctrl { crate::snap_to_grid(hit_pos, snap_grid) } else { hit_pos };
                                st.plane_normal = pn;
                                st.base_normal = pn;
                                st.surface_normal = hit_normal;
                                let (tu, tv) = crate::mirror_tangent_frame(pn, hit_normal);
                                st.tangent_u = tu; st.tangent_v = tv;
                                let abs_n = pn.abs();
                                st.snapped_axis = if abs_n.x >= abs_n.y && abs_n.x >= abs_n.z { 0 }
                                                  else if abs_n.y >= abs_n.z { 1 } else { 2 };
                            }
                        } else {
                            // Fallback: project onto workplane so plane always tracks mouse
                            if let Some(hit) = self.ray_plane_hit(mp, rect, wp_origin, wp_normal) {
                                if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                    st.plane_origin = if ctrl { crate::snap_to_grid(hit, snap_grid) } else { hit };
                                }
                            }
                        }
                    }
                    SmartMirrorPhase::Adjusting => {
                        // Drag to translate plane
                        let pd = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                        if pd {
                            let snap_grid = self.viewport.snap_grid;
                            let drag_start = self.tool.current.as_smart_mirror().unwrap().drag_start_mouse;
                            let drag_origin = self.tool.current.as_smart_mirror().unwrap().drag_start_origin;
                            let plane_n = self.tool.current.as_smart_mirror().unwrap().plane_normal;
                            if drag_start.is_none() {
                                if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                    st.drag_start_mouse = Some(mp);
                                    st.drag_start_origin = st.plane_origin;
                                }
                            } else {
                                let ds = drag_start.unwrap();
                                let start_hit = self.ray_plane_hit(ds, rect, drag_origin, plane_n);
                                let cur_hit = self.ray_plane_hit(mp, rect, drag_origin, plane_n);
                                if let (Some(sh), Some(ch)) = (start_hit, cur_hit) {
                                    let delta = ch - sh;
                                    let new_origin = drag_origin + delta;
                                    if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                        st.plane_origin = if ctrl { crate::snap_to_grid(new_origin, snap_grid) } else { new_origin };
                                    }
                                }
                            }
                        } else {
                            if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                                st.drag_start_mouse = None;
                            }
                        }
                        // Compute delete-half preview
                        let targets = self.smart_mirror_targets();
                        if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                            if st.action == SmartMirrorAction::DeleteHalf {
                                let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
                                let cam_side = (cam_pos - st.plane_origin).dot(st.plane_normal);
                                st.half_delete_indices.clear();
                                for idx in &targets {
                                    if *idx >= self.scene.primitives.len() { continue; }
                                    let side = (self.scene.primitives[*idx].position - st.plane_origin).dot(st.plane_normal);
                                    if side * cam_side > 0.0 { st.half_delete_indices.push(*idx); }
                                }
                            } else {
                                st.half_delete_indices.clear();
                            }
                        }
                    }
                }
            }
        }
        // Falloff gizmo hover detection
        self.tool.falloff_hovered = None;
        if response.hovered() && !self.tool.current.is_falloff_adjust()
           && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
           && !self.tool.current.is_bevel() && !self.tool.current.is_selection_box()
           && self.tool.hovered_bevel_edge.is_none()
           && !self.tool.uniform_bevel_hovered {
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                self.tool.falloff_hovered = self.detect_falloff_hover(mp, rect);
            }
        }
        // Light zone/softness/range handle hover (selected light only)
        if self.tool.light_handle_drag.is_none() {
            self.tool.light_handle_hovered = None;
            if response.hovered()
                && !self.tool.current.is_gizmo()
                && !self.tool.current.is_transform()
                && !self.tool.current.is_selection_box()
                && self.tool.falloff_hovered.is_none()
            {
                if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                    self.tool.light_handle_hovered = self.light_handle_hit_test(mp, rect);
                }
            }
        }
        // Gizmo hover — suppress while paint/region select owns the pointer so the
        // G/R/S handles never steal a click-drag select (owner 2026-08-06).
        let select_owns_pointer = self.tool.current.is_poly_paint_select()
            || self.tool.current.is_poly_region_select()
            || self.tool.current.is_poly_box_select()
            || self.tool.current.is_poly_circle_select()
            || self.tool.current.is_selection_box();
        if response.hovered()&&!self.tool.current.is_gizmo()&&!self.tool.current.is_transform()&&!select_owns_pointer&&self.tool.hovered_bevel_edge.is_none()&&!self.tool.uniform_bevel_hovered&&self.tool.falloff_hovered.is_none()&&!self.tool.current.is_face_push_pull(){
            let m=ctx.input(|i|i.pointer.hover_pos().unwrap_or(egui::pos2(0.0,0.0)));
            // In Elongate mode, use face hover to track which face (pos/neg) is hovered
            if self.tool.gizmo_mode == GizmoMode::Elongate {
                if let Some((comp, neg)) = self.elongate_face_hover(m, rect) {
                    self.tool.gizmo_hovered = Some(comp);
                    self.tool.elongate_face_negative = neg;
                } else {
                    self.tool.gizmo_hovered = None;
                }
            } else {
                self.tool.gizmo_hovered=self.gizmo_hit_test(m,rect);
            }
        } else if self.tool.current.is_gizmo()||self.tool.current.is_transform()||select_owns_pointer||self.tool.hovered_bevel_edge.is_some()||self.tool.uniform_bevel_hovered||self.tool.falloff_hovered.is_some()||self.tool.current.is_face_push_pull(){self.tool.gizmo_hovered=None;}
        // MMB Navigation
        if response.dragged_by(egui::PointerButton::Middle) {
            let dx = response.drag_delta().x;
            let dy = response.drag_delta().y;
            if ctrl {
                // Ctrl+MMB = dolly zoom (same as scroll wheel)
                let factor = (1.0 + dy * self.tweak.cam_zoom_speed).clamp(0.8, 1.25);
                self.viewport.camera.distance = (self.viewport.camera.distance * factor)
                    .clamp(self.tweak.cam_zoom_min, self.tweak.cam_zoom_max);
                self.viewport.orbit_alt_snap_latched = false;
            } else if shift {
                let r = (self.viewport.camera.orbit * Vec3::X).normalize();
                let u = (self.viewport.camera.orbit * Vec3::Y).normalize();
                let s = self.viewport.camera.distance * self.tweak.cam_pan_speed;
                self.viewport.camera.target -= r * dx * s;
                self.viewport.camera.target += u * dy * s;
                self.viewport.orbit_alt_snap_latched = false;
            } else if alt {
                // Alt while MMB: snap ONCE per Alt-down edge (was every frame → toast/dirty spam).
                if !self.viewport.orbit_alt_snap_latched {
                    self.snap_orbit_to_nearest_view();
                    self.viewport.orbit_alt_snap_latched = true;
                }
                // Hold Alt: stay locked on that view (no free orbit this frame).
            } else {
                // Released Alt mid-drag → allow another snap if they press Alt again.
                self.viewport.orbit_alt_snap_latched = false;
                // Pure trackball around the CURRENT look-at (`camera.target`) only.
                //
                // Do NOT swing the eye around the selection median here. Selecting a face must
                // not change where the camera sits OR how the next MMB drag behaves — otherwise
                // the cube appears to "jump" as soon as you orbit after a face pick (owner
                // 2026-08-06). Blender: selection does not re-aim the view; orbit keeps the
                // same view center. Frame Selected (F / `.`) is the intentional re-center.
                let speed = self.tweak.cam_orbit_speed;
                let orbit0 = self.viewport.camera.orbit;
                let mut right = (orbit0 * Vec3::X).normalize_or_zero();
                if right.cross(Vec3::Y).length_squared() < 1e-4 {
                    right = (orbit0 * Vec3::NEG_Z).normalize_or_zero();
                }
                let yaw_q = Quat::from_axis_angle(Vec3::Y, -dx * speed);
                let pitch_q = if right.length_squared() > 1e-8 {
                    Quat::from_axis_angle(right, -dy * speed)
                } else {
                    Quat::IDENTITY
                };
                self.viewport.camera.orbit = (yaw_q * pitch_q * orbit0).normalize();
                // Perspective: temp ortho from Alt+MMB axis snap ends when you free-orbit away.
                // User Orthographic mode is permanent (ortho_from_view_snap == false).
                self.maybe_restore_perspective_after_orbit();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        } else {
            // End of MMB drag — clear latch for next orbit.
            self.viewport.orbit_alt_snap_latched = false;
        }
        // Create tool: extrude mode (mouse Y → depth, click = confirm)
        // Track if creation was active this frame (prevents click passthrough to selection)
        let had_create = self.tool.current.is_create();
        let mmb_held = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        let is_extrude = self.tool.current.as_create().map_or(false, |cs| cs.phase == CreatePhase::Extrude);
        if is_extrude {
            if mmb_held {
                // Freeze depth accumulation while camera orbits
                if let Some(cs) = self.tool.current.as_create_mut() {
                    cs.depth_orbit_frozen = true;
                }
            } else {
                // Just came out of orbit — re-seed prev_extrude_mouse
                let was_frozen = self.tool.current.as_create()
                    .map_or(false, |cs| cs.depth_orbit_frozen);
                if was_frozen {
                    if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                        self.unfreeze_depth_orbit(mp, rect);
                    }
                }
                // On the commit frame, skip depth recomputation so the committed
                // box matches exactly what the user saw on the previous frame.
                // Recomputing from the click position can introduce jitter because
                // the mouse may shift 1-2px during the click, and at long extrudes
                // or grazing camera angles that maps to a visible depth change.
                let will_commit = response.clicked_by(egui::PointerButton::Primary);
                if !will_commit {
                    if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                        crate::ops::create::session::update_extrude(self, mp, rect, shift, alt);
                    }
                }
            }
            if response.clicked_by(egui::PointerButton::Primary) {
                crate::ops::create::session::commit(self, false);
            }
        }
        // Primary drag
        let pd = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        let lmb_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        // ── Poly / free / select ownership (owner 2026-08-07) ─────────────────
        //   1) plain LMB drag  → PAINT select
        //   2) RMB drag        → SHAPE / region select (Box/Circle/Lasso)
        //   3) Alt+LMB         → free-center G/R/S when armed (drag past threshold);
        //                        tiny Alt+LMB click / Alt+double = loop / ring
        // Paint must NEVER start while Alt is held.
        // G/R/S armed → Alt+LMB free-center (object gizmo + poly free transform).
        let free_tool_active = self.tool.gizmo_armed
            && matches!(
                self.tool.gizmo_mode,
                GizmoMode::Translate | GizmoMode::Rotate | GizmoMode::Scale | GizmoMode::Elongate
            );
        let poly_edit = self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.poly_tool == PolyTool::Idle;
        let sd = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));

        // (1) Shape / region select — RMB drag (not Alt+LMB; free center is Alt+LMB).
        // Ctrl+RMB or Alt+RMB = through (x-ray); plain RMB = front/visible.
        // Shift+RMB drag = DESELECT (remove hits). Short RMB = context menu / tool cancel.
        // Never under knife free or R/C cut-shape.
        // Never while gizmo is highlighted — highlight owns the pointer (owner: gizmo click
        // was lost to region/paint while the handle still lit up).
        if response.drag_started_by(egui::PointerButton::Secondary)
            && !tool_consumed_click
            && poly_edit
            && self.poly_tool == PolyTool::Idle
            && self.knife_shape_kind == 0
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
        {
            if let Some(mp) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.press_origin()))
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            {
                let start = ctx
                    .input(|i| i.pointer.press_origin())
                    .unwrap_or(mp);
                self.tool.current = ActiveTool::PolyRegionSelect(PolyRegionSelectState {
                    shape: self.poly_sel_shape,
                    front_only: !(ctrl || alt), // Ctrl/Alt+RMB = through; plain RMB = front
                    subtract: shift,            // Shift+RMB = deselect
                    start,
                    current: mp,
                    path: vec![start, mp],
                });
                ctx.request_repaint();
            }
        }
        // Live rubber-band while RMB held (region shape select).
        if self.tool.current.is_poly_region_select() && sd {
            if let Some(mp) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            {
                if let ActiveTool::PolyRegionSelect(ref mut rs) = self.tool.current {
                    rs.current = mp;
                    if rs.path.last().map_or(true, |p| (*p - mp).length() > 2.0) {
                        rs.path.push(mp);
                    }
                }
                ctx.request_repaint();
            }
        }
        // Free center (poly): G/R/S armed + Alt+LMB *drag past threshold* only.
        // NOT on press — that stole Alt+double-click ring while Grab was armed.
        // NOT bare LMB (owner: bare LMB is paint select).
        if free_tool_active
            && alt
            && !tool_consumed_click
            && poly_edit
            && self.tool.current.is_idle()
            && self.poly_selection_center().is_some()
            && self.tool.gizmo_hovered.is_none()
            && pd
        {
            if let (Some(start), Some(mp)) = (
                ctx.input(|i| i.pointer.press_origin()),
                response
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.hover_pos())),
            ) {
                if (mp - start).length() > POLY_ALT_FREE_DRAG_THRESHOLD {
                    let kind = match self.tool.gizmo_mode {
                        GizmoMode::Translate => TransformKind::Grab,
                        GizmoMode::Rotate => TransformKind::Rotate,
                        GizmoMode::Scale => TransformKind::Scale,
                        GizmoMode::Elongate => TransformKind::Elongate,
                    };
                    self.poly_start_transform(kind, start);
                    if let Some(ts) = self.tool.current.as_poly_transform_mut() {
                        ts.free_mode = true;
                        ts.confirm_on_release = true;
                    }
                    if self.tool.current.is_poly_transform() {
                        time_block!("poly_update_transform", self.poly_update_transform(mp, shift, ctrl));
                    }
                }
            }
        }
        // Free center (SDF / object / lights): SAME Alt+LMB ownership as poly.
        // G/R/S arm the gizmo; Alt+LMB drag past threshold = free view-plane Grab /
        // free Scale / free Rotate (cam-forward). Bare LMB stays select/box.
        // Owner 2026-08-08: SDF GRS lost free mode — the old path only fired on
        // egui `dragged_by` + WorkMode::Object, so SDF Vertex (prim edit) and
        // threshold-short Alt drags never started free transform.
        let sdf_or_object_sel = self.active_prim_index_unified().is_some()
            || self.sel.usel.active_node().is_some()
            || self.sel.usel.has_lights()
            || !self.selected_prim_indices_unified().is_empty();
        let sdf_free_context = !poly_edit
            && self.modeling_context != ModelingContext::Nurbs
            && (self.tool.work_mode == WorkMode::Object
                || (self.modeling_context == ModelingContext::Sdf
                    && self.tool.work_mode == WorkMode::Vertex));
        if free_tool_active
            && alt
            && !tool_consumed_click
            && sdf_free_context
            && sdf_or_object_sel
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
            && !self.tool.suppress_gizmo_drag
            && !self.tool.mode.is_bevel()
            && !self.tool.mode.is_extrude()
            && pd
        {
            if let (Some(start), Some(mp)) = (
                ctx.input(|i| i.pointer.press_origin()),
                response
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.hover_pos())),
            ) {
                if (mp - start).length() > POLY_ALT_FREE_DRAG_THRESHOLD {
                    // Center handle = free mode (same as poly free_mode + gizmo Center).
                    self.start_gizmo_drag(GizmoComponent::Center, start, shift);
                    if self.tool.current.is_gizmo() {
                        self.update_gizmo_drag(mp, rect, ctrl, shift, alt);
                    }
                }
            }
        }
        // NURBS free G/R/S: same rule as poly — drag past threshold, not press (parity).
        if free_tool_active
            && alt
            && !tool_consumed_click
            && self.modeling_context == ModelingContext::Nurbs
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
            && self.nurbs_selection_center().is_some()
            && pd
        {
            if let (Some(start), Some(mp)) = (
                ctx.input(|i| i.pointer.press_origin()),
                response
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.hover_pos())),
            ) {
                if (mp - start).length() > POLY_ALT_FREE_DRAG_THRESHOLD {
                    let kind = match self.tool.gizmo_mode {
                        GizmoMode::Translate => TransformKind::Grab,
                        GizmoMode::Rotate => TransformKind::Rotate,
                        GizmoMode::Scale => TransformKind::Scale,
                        GizmoMode::Elongate => TransformKind::Scale,
                    };
                    self.nurbs_start_transform(kind, start);
                    if let Some(d) = self.tool.current.as_nurbs_cv_move_mut() {
                        d.free_mode = true;
                        d.confirm_on_release = true;
                    }
                    if self.tool.current.is_nurbs_cv_move() {
                        self.nurbs_update_cv_drag(mp, shift, ctrl);
                    }
                }
            }
        }
        // Live paint stroke while LMB held — same primary-down gate as region select.
        // Must NOT depend on egui `dragged_by` alone: shape-select made small LMB moves
        // often miss the drag classifier, so paint armed on press never dabbed until release
        // (which then ran click-select and looked "broken").
        // NEVER while knife (free or shape) owns the tool — paint must not stroke under a cut.
        if self.tool.current.is_poly_paint_select()
            && pd
            && !self.sculpt_brush_owns_lmb()
            && self.poly_tool != PolyTool::Knife
            && self.knife_shape_kind == 0
        {
            if let Some(mp) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            {
                let paint_seg = if let ActiveTool::PolyPaintSelect(ref mut ps) = self.tool.current {
                    if !ps.painted && (mp - ps.start).length() > POLY_PAINT_DRAG_THRESHOLD {
                        ps.painted = true;
                    }
                    let prev = ps.last;
                    ps.last = mp;
                    if ps.painted {
                        Some((prev, ps.shift))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((prev, erase)) = paint_seg {
                    let front = self.sel.visible_only;
                    self.poly_paint_stroke_apply(
                        prev,
                        mp,
                        self.poly_paint_radius,
                        front,
                        rect,
                        erase,
                    );
                }
                ctx.request_repaint();
            }
        }
        // Region release (RMB): real drag → apply shape select / Shift-deselect; tiny → drop.
        // Must finish on Secondary-up — Primary is never down for RMB region.
        if self.tool.current.is_poly_region_select() && !sd {
            let (moved, release_pos) = match &self.tool.current {
                ActiveTool::PolyRegionSelect(rs) => {
                    let dist = (rs.current - rs.start).length() > 4.0;
                    let path_drag = rs.path.len() > 3;
                    let lasso =
                        matches!(rs.shape, SelectionShape::Lasso) && rs.path.len() > 2;
                    (dist || path_drag || lasso, rs.current)
                }
                _ => (false, egui::pos2(0.0, 0.0)),
            };
            if moved {
                self.poly_apply_region_select(rect);
                rmb_consumed = true; // do not open context menu on marquee release
            } else if shift {
                // Short Shift+RMB often arms region (drag_started) so egui never fires
                // Secondary `clicked` — still run Action Center place (T-mode place).
                if ctrl || alt {
                    self.place_workplane_on_surface(release_pos, rect);
                } else if self.tool.action_center == crate::ActionCenter::Automatic {
                    let _ = self.action_center_place_automatic(release_pos, rect, false);
                } else if self.tool.action_center == crate::ActionCenter::Element {
                    let _ = self.action_center_place_element(release_pos, rect);
                } else {
                    self.place_workplane_on_surface(release_pos, rect);
                }
                rmb_consumed = true;
            }
            // Tiny RMB "drag": drop tool; loop/ring is Alt+LMB, not RMB.
            poly_region_click_done = true;
            self.tool.current = ActiveTool::Idle;
        }
        // (2) Gizmo highlight owns LMB — start on press (not only after drag threshold).
        //
        // Owner: "gizmo highlight but click does not register — drag select registers first."
        // Paint used to arm on `lmb_pressed` while gizmo drag waited for `dragged_by`, and only
        // AxisX/Y/Z blocked paint. Center (hit-tested first at the pivot) still painted, and once
        // paint latched it cleared `gizmo_hovered` via select_owns_pointer so the handle never won.
        // Rule: if the gizmo is highlighted, that click/drag is the gizmo — never selection.
        let drag_started_lmb = response.drag_started_by(egui::PointerButton::Primary);
        let gizmo_highlight = self.tool.gizmo_hovered;
        if (lmb_pressed || drag_started_lmb)
            && !tool_consumed_click
            && !shape_owned_lmb
            && self.tool.current.is_idle()
            && !self.tool.suppress_gizmo_drag
            && gizmo_highlight.is_some()
            && !self.tool.current.is_face_push_pull()
            && self.poly_tool == PolyTool::Idle
            && self.knife_shape_kind == 0
        {
            if let Some(comp) = gizmo_highlight {
                let mp = response
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.press_origin()))
                    .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                    .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                    .unwrap_or(self.viewport.viewport_mouse);
                if !self.try_start_box_face_extrude(comp, mp) {
                    self.start_gizmo_drag(comp, mp, shift);
                }
            }
        }
        // (3) Paint select — plain LMB only (never while Alt held, never under knife).
        // Free-center is Alt+LMB when G/R/S armed (and not on a highlighted handle).
        // Gizmo highlight (any component, including Center) blocks paint — see (2).
        // Knife free/shape owns all LMB while poly_tool == Knife.
        // Also arm on drag_started if press was missed (egui sometimes reports drag without a
        // button_pressed on the viewport response — same class as knife shape ownership).
        if (lmb_pressed || drag_started_lmb)
            && !tool_consumed_click
            && !alt
            && !shape_owned_lmb
            && poly_edit
            && !self.sculpt_brush_owns_lmb()
            && self.poly_tool == PolyTool::Idle
            && self.knife_shape_kind == 0
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
        {
            let mp = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or(self.viewport.viewport_mouse);
            self.tool.current = ActiveTool::PolyPaintSelect(PolyPaintSelectState {
                start: mp,
                last: mp,
                painted: false,
                shift,
            });
        }
        // Polyline profile-point editor — SINGLE press owner. `curve_press_dispatch` resolves every
        // curve-prim press state atomically (whole-prim `Transform` modal → confirm + grab; armed
        // gizmo over a handle → disarm + grab; idle over a handle → grab; empty → box-select), so the
        // press is never split across racing arms / two files. This replaces the former pair of arms
        // (`is_transform()` + `is_idle && !gizmo_armed`), which between them left the armed-gizmo case
        // (G with the prim selected but no point selected) falling through to a whole-prim gizmo drag
        // — the "clicking a curve point moves the whole prim" bug. Skipped during a copy-angle pick
        // (that gesture consumes the LMB itself).
        // Set when the curve point editor consumed the LMB press as a shift-toggle / grab that
        // started NO tool (pure selection change). egui still reports a `clicked_by(Primary)` for
        // that same press+release, so without this flag `handle_click_select` (line ~1964) would run
        // and toggle the WHOLE prim out of the unified selection — collapsing `sel.len()==1`, which
        // deactivates the point editor and clears `curve_pt_sel` in `curve_pt_tick`. That was the
        // "shift+click can't accumulate 2+ profile points" bug (and, downstream, "Flatten does
        // nothing" — Flatten needs exactly 2 adjacent selected points, unreachable without it).
        let mut curve_press_consumed = false;
        // The hold flag survives past this frame: egui can classify a jittery shift+click as a
        // DRAG, and the drag block below would start a viewport SelectionBox on the very press the
        // point editor consumed (tool stays Idle after a pure toggle), whose release re-runs object
        // selection with shift and toggles the whole prim out — wiping curve_pt_sel. Held until the
        // primary button releases; cleared here so the release frame itself is still protected.
        if !ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)
            || i.pointer.button_released(egui::PointerButton::Primary))
        {
            self.tool.curve_press_hold = false;
        }
        // Spine exclusive tool owns the LMB press (tool is busy → every other press arm is
        // suppressed by its own tool.current checks). Ctrl+click = add point, click point =
        // select/drag, Shift+click = multi-select, empty = clear point selection.
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            && self.tool.current.is_spine_point_edit()
            && self.spine_pt_drag.is_none()
            && !self.ui.spine_corner_pie.open
        {
            if let Some(sidx) = self.spine_edit_prim_index() {
                if let Some(mp) = response.interact_pointer_pos() {
                    self.set_gizmo_armed(false);
                    let _ = self.spine_pt_press(sidx, mp, rect, shift, ctrl);
                }
            }
        }
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            && self.curve_copy_angle_arm.is_none()
        {
            if let Some(mp) = response.interact_pointer_pos() {
                match self.curve_press_dispatch(mp, rect, shift) {
                    crate::curve_point_edit::CurvePressOutcome::Grab => {
                        curve_press_consumed = true;
                        // Only a no-tool consume needs the cross-frame guard (a real grab/tool
                        // already suppresses the other arms via tool.current checks).
                        if self.tool.current.is_idle() { self.tool.curve_press_hold = true; }
                    }
                    crate::curve_point_edit::CurvePressOutcome::Empty => {
                        // Editor active, press missed all handles → point-editor box-select
                        // (release routes tiny-box → click-deselect, real drag → box-select).
                        self.tool.selection_box_secondary = false;
                        self.tool.selection_box_subtract = false;
                        self.tool.current = ActiveTool::SelectionBox(SelectionDrag::Box { start: mp });
                    }
                    crate::curve_point_edit::CurvePressOutcome::No => {}
                }
            }
        }
        // Spine drag update:
        // - LMB move/add/handle: every frame while primary is *down* (not only after drag
        //   threshold) so yellow Bezier handles track the workplane under the cursor immediately
        // - Modal G/S/R: every frame from hover (no LMB required)
        // Shift = angle snap; Alt = free camera plane (points) / unlink (handles)
        // Ctrl = snap to visible SDF/mesh surface while dragging a point
        if self.spine_pt_drag.is_some() {
            let modal = self.spine_pt_drag.as_ref().map(|d| d.modal).unwrap_or(false);
            let primary_down = ctx.input(|i| i.pointer.primary_down());
            let follow = modal || primary_down || response.dragged_by(egui::PointerButton::Primary);
            if follow {
                let mp = if modal {
                    ctx.input(|i| i.pointer.hover_pos()).or_else(|| response.interact_pointer_pos())
                } else {
                    response.interact_pointer_pos()
                        .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                };
                if let Some(mp) = mp {
                    self.spine_pt_update_drag(mp, rect, shift, alt, ctrl);
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Primary){if let Some(mp)=response.interact_pointer_pos(){
            // Light zone/softness/range handle drag (viewport Unreal-style controls)
            if let Some(drag) = self.tool.light_handle_drag.clone() {
                if let Some(light) = self.scene.light_by_id(drag.light_id).cloned() {
                    let pos = light.position;
                    match drag.kind {
                        app_types::LightHandleKind::Zone => {
                            if let Some(r) = self.light_handle_world_radius(mp, rect, pos) {
                                let r = r.clamp(0.05, 500.0);
                                self.write_light(drag.light_id, |l| l.volumetric.zone = r);
                                self.mark_dirty(DirtyFlags::LIGHT);
                            }
                        }
                        app_types::LightHandleKind::Softness => {
                            // Softness from world radius of inner ring: soft = 1 - (r / zone)
                            if let Some(r) = self.light_handle_world_radius(mp, rect, pos) {
                                let z = drag.start_zone.max(0.05);
                                let soft = (1.0 - (r / z)).clamp(0.0, 1.0);
                                self.write_light(drag.light_id, |l| l.volumetric.softness = soft);
                                self.mark_dirty(DirtyFlags::LIGHT);
                            }
                        }
                        app_types::LightHandleKind::Range => {
                            if let Some(r) = self.light_handle_world_radius(mp, rect, pos) {
                                let r = r.clamp(0.1, 500.0);
                                self.write_light(drag.light_id, |l| l.range = r);
                                self.mark_dirty(DirtyFlags::LIGHT);
                            }
                        }
                    }
                }
            }
            // Sculpt mode: voxel sculpt handles LMB drag via handle_sculpt_input() below
            else if self.tool.work_mode == WorkMode::Sculpt && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
            {
                // Handled by handle_sculpt_input() below — skip all other drag handlers
            }
            // Pivot edit drag
            else if self.tool.current.is_pivot_edit() {
                self.update_pivot_drag(mp, rect, ctrl, shift);
            }
            // Direct curve profile-point drag (started on LMB press over a handle/edge). Keyboard
            // G/R/S modals don't hold LMB, so this arm only fires for pointer-initiated drags.
            else if self.tool.current.as_curve_point_transform().map_or(false, |d| d.direct) {
                self.curve_pt_update_transform(mp, rect, ctrl, shift);
            }
            else if self.tool.current.is_create() && self.tool.current.as_create().unwrap().phase == CreatePhase::Footprint {
                let snap_on = self.viewport.smart_snap_on || ctrl;
                let snap_wp = if snap_on {
                    self.viewport.smart_snap_result.as_ref().map(|h| h.world_pos)
                } else {
                    None
                };
                crate::ops::create::session::update_footprint(self, mp, rect, shift, alt, snap_wp);
            }
            // BoolCut draw phase: resize footprint while LMB held
            else if self.tool.bool_cut.as_ref().map_or(false, |bc| bc.phase == BoolCutPhase::Drawing) {
                self.update_bool_cut_draw(mp, rect, shift, alt);
            }
            else if self.tool.mode.is_create_armed() && !self.tool.current.is_create()
                && self.tool.gizmo_hovered.is_none()
                && self.tool.hovered_bevel_edge.is_none() && self.tool.falloff_hovered.is_none()
                && !self.tool.current.is_gizmo() && !self.tool.current.is_transform()
                && self.tool.work_mode == WorkMode::Object
            {
                // Start drag-to-create (only when a tool is armed)
                if let Some(mut state) = crate::ops::create::session::start(self, mp, rect) {
                    // Smart snap: override anchor if snap result is active
                    let snap_on = self.viewport.smart_snap_on || ctrl;
                    if snap_on {
                        if let Some(hit) = &self.viewport.smart_snap_result {
                            let sp = hit.world_pos;
                            state.anchor = sp;
                            state.hit_point = sp;
                            // Reposition the nascent primitive
                            self.write_prim(state.prim_idx, |p| p.position = sp);
                        }
                    }
                    self.start_operation(ActiveTool::Create(state));
                }
            }
            // BoolCut: first LMB frame while armed → start drawing a surface cutter
            else if self.tool.mode.is_bool_cut()
                && self.tool.bool_cut.is_none()
                && self.tool.current.is_idle()
                && self.tool.gizmo_hovered.is_none()
                && self.tool.work_mode == WorkMode::Object
                && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            {
                if let Some(state) = self.start_bool_cut(mp, rect) {
                    self.tool.bool_cut = Some(state);
                }
            }
            // BoolCut depth phase: LMB press commits (once per click)
            else if self.tool.bool_cut.as_ref().map_or(false, |bc| bc.phase == BoolCutPhase::Depth)
                && self.tool.current.is_idle()
                && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
            {
                self.commit_bool_cut(false);
            }
            else if self.tool.current.is_falloff_adjust() {
                let fd = self.tool.current.as_falloff_adjust().unwrap();
                let (prim_idx, handle, start_mouse, start_offset, start_length, start_k, start_dir) =
                    (fd.prim_idx, fd.handle, fd.start_mouse, fd.start_offset, fd.start_length, fd.start_k, fd.start_direction);
                if prim_idx < self.scene.primitives.len() {
                    let prim = &self.scene.primitives[prim_idx];
                    let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
                    let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize();
                    let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize();
                    let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
                    let prim_center = if BODY_LOCAL_PRIMS {
                        self.scene.prim_body_map.get(prim_idx)
                            .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                            .unwrap_or_default()
                            .local_to_world(prim.position)
                    } else { prim.position };
                    match handle {
                        FalloffHandle::End => {
                            let cur_dir = start_dir.normalize_or_zero();
                            let origin_world = prim_center + cur_dir * start_offset;
                            let start_end = origin_world + cur_dir * start_length;
                            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
                            let depth = (start_end - cam_pos).dot(cam_fwd).max(0.1);
                            let px_to_world = depth / (rect.height() * 0.5);
                            let mouse_dx = mp.x - start_mouse.x;
                            let mouse_dy = mp.y - start_mouse.y;
                            let world_delta = cam_right * mouse_dx * px_to_world - cam_up * mouse_dy * px_to_world;
                            let new_end = start_end + world_delta;
                            let new_vec = new_end - origin_world;
                            let new_len = new_vec.length();
                            if new_len > 0.01 {
                                let dir = new_vec / new_len;
                                self.write_prim(prim_idx, |p| {
                                    p.blend_falloff.direction = dir;
                                    p.blend_falloff.length = new_len;
                                });
                            }
                        }
                        FalloffHandle::Origin => {
                            let cur_dir = start_dir.normalize_or_zero();
                            let start_origin = prim_center + cur_dir * start_offset;
                            let end_world = start_origin + cur_dir * start_length;
                            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
                            let depth = (start_origin - cam_pos).dot(cam_fwd).max(0.1);
                            let px_to_world = depth / (rect.height() * 0.5);
                            let mouse_dx = mp.x - start_mouse.x;
                            let mouse_dy = mp.y - start_mouse.y;
                            let world_delta = cam_right * mouse_dx * px_to_world - cam_up * mouse_dy * px_to_world;
                            let new_origin = start_origin + world_delta;
                            let new_vec = end_world - new_origin;
                            let new_len = new_vec.length();
                            if new_len > 0.01 {
                                let new_dir = new_vec / new_len;
                                let new_offset = (new_origin - prim_center).dot(new_dir);
                                self.write_prim(prim_idx, |p| {
                                    p.blend_falloff.direction = new_dir;
                                    p.blend_falloff.length = new_len;
                                    p.blend_falloff.origin_offset = new_offset;
                                });
                            }
                        }
                        FalloffHandle::Radius => {
                            let axis_dir = start_dir.normalize_or_zero();
                            let s0 = world_to_screen(prim_center, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
                            let s1 = world_to_screen(prim_center + axis_dir, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
                            if let (Some(sp0), Some(sp1)) = (s0, s1) {
                                let screen_axis = egui::vec2(sp1.x - sp0.x, sp1.y - sp0.y);
                                let sal = screen_axis.length();
                                if sal > 0.1 {
                                    let san = screen_axis / sal;
                                    let perp_n = egui::vec2(-san.y, san.x);
                                    let mdelta = egui::vec2(mp.x - start_mouse.x, mp.y - start_mouse.y);
                                    let perp_proj = mdelta.x * perp_n.x + mdelta.y * perp_n.y;
                                    let perp_delta = perp_proj / sal;
                                    let new_k = (start_k + perp_delta).max(0.01);
                                    self.scene.primitives[prim_idx].operation.set_blend_k(new_k);
                                    self.mirror_prim_to_graph(prim_idx);
                                }
                            }
                        }
                    }
                    self.rebake_no_undo();
                }
            }
            // Blend-k drag update (unified for prim and body). The handle mode decides
            // the axes (edge_detect::BlendHandleMode):
            //   BlendK          → HORIZONTAL k (unsigned, clamps at 0) — unchanged.
            //   SeamDepth       → 2-AXIS: VERTICAL signed depth (down = groove deeper (+),
            //                     up through zero = raised bead (−) — up raises material;
            //                     replaces the old right=deeper horizontal mapping),
            //                     HORIZONTAL width (clamped to the card slider range).
            //   ExtrudeStrength → 2-AXIS: VERTICAL signed strength (up = raise/out (+),
            //                     down through zero = recess/in (−)), HORIZONTAL width.
            // Both axes mutate inside the ONE episode opened at drag start (one undo).
            // Mode is re-resolved from live state each frame so mid-drag polarity flips
            // keep working.
            else if self.tool.current.is_poly_prop_origin() {
                self.poly_prop_origin_drag_update(mp, rect);
            }
            else if self.tool.current.is_blend_k_drag() {
                use crate::edge_detect::BlendHandleMode;
                let (target, start_x, start_y, start_k, start_w) = {
                    let bd = self.tool.current.as_blend_k_drag().unwrap();
                    (bd.target.clone(), bd.start_mouse_x, bd.start_mouse_y, bd.start_k, bd.start_w)
                };
                let speed = self.tweak.bevel_drag_speed;
                let dx = (mp.x - start_x) * speed;
                let dy = (mp.y - start_y) * speed; // screen down = positive
                // Seam/Extrude axes span a 0..=0.5 value range — at the base
                // blend-k rate the full range needs a ~250px drag (owner:
                // "too small, hard to use"). Boost both axes so the range is
                // reachable in a comfortable ~60px.
                let bdx = dx * SEAM_EXTRUDE_DRAG_BOOST;
                let bdy = dy * SEAM_EXTRUDE_DRAG_BOOST;
                match target {
                    BlendKTarget::Prims(indices) => {
                        let k_max = self.tweak.blend_k_max.max(0.0);
                        let new_k = (start_k + dx).clamp(0.0, k_max);
                        let mut any = false;
                        for &idx in &indices {
                            if idx >= self.scene.primitives.len() { continue; }
                            match crate::edge_detect::prim_handle_mode(&self.scene.primitives[idx]) {
                                BlendHandleMode::SeamDepth => {
                                    let op = &mut self.scene.primitives[idx].operation;
                                    crate::edge_detect::seam_set_signed_depth(op, start_k + bdy);
                                    crate::edge_detect::op_set_width_clamped(op, start_w + bdx);
                                }
                                BlendHandleMode::ExtrudeStrength => {
                                    let op = &mut self.scene.primitives[idx].operation;
                                    crate::edge_detect::extrude_set_signed_strength(op, start_k - bdy);
                                    crate::edge_detect::op_set_width_clamped(op, start_w + bdx);
                                }
                                BlendHandleMode::BlendK => {
                                    crate::edge_detect::op_set_k_clamped(
                                        &mut self.scene.primitives[idx].operation, new_k, k_max);
                                }
                            }
                            self.mirror_prim_to_graph(idx);
                            any = true;
                        }
                        if any { self.rebake_no_undo(); }
                    }
                    BlendKTarget::Body(body_nid) => {
                        if let Some(old_op) = self.scene.scene_graph.body_op(body_nid).cloned() {
                            let mut new_op = old_op;
                            let k_max = self.tweak.blend_k_max.max(0.0);
                            match crate::edge_detect::op_handle_mode(&new_op) {
                                BlendHandleMode::SeamDepth => {
                                    crate::edge_detect::seam_set_signed_depth(&mut new_op, start_k + bdy);
                                    crate::edge_detect::op_set_width_clamped(&mut new_op, start_w + bdx);
                                }
                                BlendHandleMode::ExtrudeStrength => {
                                    crate::edge_detect::extrude_set_signed_strength(&mut new_op, start_k - bdy);
                                    crate::edge_detect::op_set_width_clamped(&mut new_op, start_w + bdx);
                                }
                                BlendHandleMode::BlendK => {
                                    crate::edge_detect::op_set_k_clamped(
                                        &mut new_op, start_k + dx, k_max);
                                }
                            }
                            self.scene.scene_graph.set_body_op(body_nid, new_op);
                            if let Some(ref mut gpu) = self.gpu {
                                if let Some(ref mut atlas) = gpu.volume_atlas {
                                    atlas.rebuild_params(&self.scene.scene_graph);
                                }
                            }
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }
                }
            }
            // Boolean edge bevel drag (ongoing)
            else if self.tool.current.is_bool_edge_bevel() {
                let (prim_idx, start_x, orig_k) = {
                    let bd = self.tool.current.as_bool_edge_bevel().unwrap();
                    (bd.prim_idx, bd.start_mouse_x, bd.original_k)
                };
                let dx = mp.x - start_x;
                let k_max = self.tweak.blend_k_max.max(0.0);
                let new_k = (orig_k + dx * self.tweak.bevel_drag_speed).clamp(0.0, k_max);
                if prim_idx < self.scene.primitives.len() {
                    crate::edge_detect::op_set_k_clamped(
                        &mut self.scene.primitives[prim_idx].operation, new_k, k_max);
                    self.mirror_prim_to_graph(prim_idx);
                    self.rebake_no_undo();
                }
            }
            else if self.tool.current.is_bevel() {
                let (prim_idx, start_x, target_edges, sa_bot, sa_top, sa_vert, sp_bot, sp_top, sp_vert, uniform_all) = {
                    let bd = self.tool.current.as_bevel().unwrap();
                    (bd.prim_idx, bd.start_mouse_x, bd.target_edges.clone(),
                     bd.start_chamfer_angle_bot, bd.start_chamfer_angle_top, bd.start_chamfer_angle_vert,
                     bd.start_bevel_profile_bot, bd.start_bevel_profile_top, bd.start_bevel_profile_vert,
                     bd.uniform_all)
                };
                let dx = mp.x - start_x;
                let ctrl = ctx.input(|i| i.modifiers.ctrl);
                let shift = ctx.input(|i| i.modifiers.shift);
                if prim_idx < self.scene.primitives.len() {
                    // Yellow-square Uniform drag: all 12 edges + auto Uniform algo (Round fillet).
                    if uniform_all && !ctrl && !shift {
                        if let SdfShape::Box { half_extents, bevel } = &mut self.scene.primitives[prim_idx].shape {
                            use core_math::sdf_shapes::BoxBevelAlgo;
                            let max_r = half_extents.x.min(half_extents.y).min(half_extents.z);
                            let radius = (dx.abs() * self.tweak.bevel_drag_speed).min(max_r);
                            bevel.algo = BoxBevelAlgo::Uniform;
                            for i in 0..12 {
                                bevel.radii[i] = radius;
                                bevel.modes[i] = if radius > 0.001 { BevelMode::Round } else { BevelMode::None };
                            }
                        }
                        self.mirror_prim_to_graph(prim_idx);
                        self.sync_instances_if_master(prim_idx);
                        self.rebake_no_undo();
                    } else {
                    let group = {
                        let mut has_bot = false;
                        let mut has_top = false;
                        let mut has_vert = false;
                        for &e in &target_edges {
                            if e < 4 { has_bot = true; }
                            else if e < 8 { has_top = true; }
                            else { has_vert = true; }
                        }
                        if has_bot && !has_top && !has_vert { "bot" }
                        else if !has_bot && has_top && !has_vert { "top" }
                        else if !has_bot && !has_top && has_vert { "vert" }
                        else { "all" }
                    };
                    if !ctrl && !shift {
                        // Adjust radius
                        if let SdfShape::Box{half_extents, bevel} = &mut self.scene.primitives[prim_idx].shape {
                            let max_r = half_extents.x.min(half_extents.y).min(half_extents.z);
                            let radius = (dx.abs() * self.tweak.bevel_drag_speed).min(max_r);
                            let mode = if dx < 0.0 { BevelMode::Round } else { BevelMode::Chamfer };
                            for &ei in &target_edges {
                                bevel.radii[ei as usize] = radius;
                                bevel.modes[ei as usize] = mode;
                            }
                        } else if matches!(&self.scene.primitives[prim_idx].shape, SdfShape::Cylinder{..}) {
                            let max_r = if let SdfShape::Cylinder{radius, half_height} = &self.scene.primitives[prim_idx].shape {
                                radius.min(*half_height)
                            } else { 1.0 };
                            let radius = (dx.abs() * self.tweak.bevel_drag_speed).min(max_r);
                            let mode = if dx < 0.0 { BevelMode::Round } else { BevelMode::Chamfer };
                            for &ei in &target_edges {
                                match ei {
                                    0 => { self.scene.primitives[prim_idx].cyl_bevel.top = radius; self.scene.primitives[prim_idx].cyl_bevel.top_mode = mode; }
                                    1 => { self.scene.primitives[prim_idx].cyl_bevel.bot = radius; self.scene.primitives[prim_idx].cyl_bevel.bot_mode = mode; }
                                    _ => {}
                                }
                            }
                        }
                    } else if ctrl {
                        // Adjust chamfer angle — absolute from start value, not accumulative
                        let offset = dx * self.tweak.bevel_drag_speed * 0.3;
                        let p = &mut self.scene.primitives[prim_idx];
                        match group {
                            "bot" => p.chamfer_angle_bot = (sa_bot + offset).clamp(1.0, 89.0),
                            "top" => p.chamfer_angle_top = (sa_top + offset).clamp(1.0, 89.0),
                            "vert" => p.chamfer_angle_vert = (sa_vert + offset).clamp(1.0, 89.0),
                            "all" => {
                                p.chamfer_angle_bot = (sa_bot + offset).clamp(1.0, 89.0);
                                p.chamfer_angle_top = (sa_top + offset).clamp(1.0, 89.0);
                                p.chamfer_angle_vert = (sa_vert + offset).clamp(1.0, 89.0);
                            }
                            _ => {}
                        }
                    } else if shift {
                        // Adjust profile (only for Box) — absolute from start value
                        if matches!(&self.scene.primitives[prim_idx].shape, SdfShape::Box{..}) {
                            let offset = dx * self.tweak.bevel_drag_speed * 0.15;
                            let p = &mut self.scene.primitives[prim_idx];
                            match group {
                                "bot" => p.bevel_profile_bot = (sp_bot + offset).clamp(-1.0, 1.0),
                                "top" => p.bevel_profile_top = (sp_top + offset).clamp(-1.0, 1.0),
                                "vert" => p.bevel_profile_vert = (sp_vert + offset).clamp(-1.0, 1.0),
                                "all" => {
                                    p.bevel_profile_bot = (sp_bot + offset).clamp(-1.0, 1.0);
                                    p.bevel_profile_top = (sp_top + offset).clamp(-1.0, 1.0);
                                    p.bevel_profile_vert = (sp_vert + offset).clamp(-1.0, 1.0);
                                }
                                _ => {}
                            }
                        }
                    }
                    self.mirror_prim_to_graph(prim_idx);
                    // Bevel drag mutates shape in place (not write_prim) — push per-edge
                    // radii/modes to linked instances.
                    self.sync_instances_if_master(prim_idx);
                    self.rebake_no_undo();
                    } // end non-uniform_all branch
                }
            }
            // Yellow Uniform-bevel square drag start (before per-edge — square wins when both hover)
            else if self.tool.uniform_bevel_hovered && !self.tool.current.is_busy() {
                if let Some(idx) = self.active_prim_index_unified() {
                    if idx < self.scene.primitives.len()
                        && matches!(&self.scene.primitives[idx].shape, SdfShape::Box { .. })
                    {
                        let idx = self.resolve_content_edit_idx(idx);
                        let snap = Self::bevel_drag_start_snapshot(&self.scene.primitives[idx]);
                        self.begin_episode("Uniform bevel");
                        self.start_operation(ActiveTool::Bevel(BevelDrag {
                            prim_idx: idx,
                            target_edges: (0u8..12).collect(),
                            start_mouse_x: mp.x,
                            start_mouse_y: mp.y,
                            start_chamfer_angle_bot: snap.ca_bot,
                            start_chamfer_angle_top: snap.ca_top,
                            start_chamfer_angle_vert: snap.ca_vert,
                            start_bevel_profile_bot: snap.bp_bot,
                            start_bevel_profile_top: snap.bp_top,
                            start_bevel_profile_vert: snap.bp_vert,
                            start_box_radii: snap.box_radii,
                            start_cyl_top: snap.cyl_top,
                            start_cyl_bot: snap.cyl_bot,
                            uniform_all: true,
                        }));
                    }
                }
            }
            // Bevel drag start
            // Guard `!is_busy()` (not just gizmo/create): a NEW transient drag must only start from an
            // idle tool. The old guard predated the E-tool's FacePushPull (ELONG2) — during a Push/Pull
            // drag the cursor can cross a bevel edge (sets hovered_bevel_edge) and this `else if` would
            // fire start_operation(Bevel) while busy → debug_assert panic (tool_mode.rs:96). is_busy()
            // covers every active op, fixing the whole class. FacePushPull clears on pointer-up (:1228),
            // so a genuine fresh press is idle here and bevel-start still works.
            else if self.tool.hovered_bevel_edge.is_some() && !self.tool.current.is_busy() {
                let edge = self.tool.hovered_bevel_edge.unwrap();
                let target_edges = if self.tool.selected_bevel_edges.contains(&edge) {
                    self.tool.selected_bevel_edges.clone()
                } else {
                    self.tool.selected_bevel_edges = vec![edge];
                    vec![edge]
                };
                if let Some(idx) = self.active_prim_index_unified() {
                    if idx >= self.scene.primitives.len() { return; }
                    // Instances share shape with master — bevel the master so all copies update.
                    let idx = self.resolve_content_edit_idx(idx);
                    let snap = Self::bevel_drag_start_snapshot(&self.scene.primitives[idx]);
                    self.begin_episode("Bevel drag");
                    self.start_operation(ActiveTool::Bevel(BevelDrag {
                        prim_idx: idx,
                        target_edges,
                        start_mouse_x: mp.x,
                        start_mouse_y: mp.y,
                        start_chamfer_angle_bot: snap.ca_bot,
                        start_chamfer_angle_top: snap.ca_top,
                        start_chamfer_angle_vert: snap.ca_vert,
                        start_bevel_profile_bot: snap.bp_bot,
                        start_bevel_profile_top: snap.bp_top,
                        start_bevel_profile_vert: snap.bp_vert,
                        start_box_radii: snap.box_radii,
                        start_cyl_top: snap.cyl_top,
                        start_cyl_bot: snap.cyl_bot,
                        uniform_all: false,
                    }));
                }
            }
            // Falloff drag start
            // Same class fix as bevel-start above: only start the falloff drag from an idle tool, so a
            // cursor crossing a falloff handle mid-Push/Pull (or any active op) can't start_operation while busy.
            else if self.tool.falloff_hovered.is_some() && !self.tool.current.is_busy() {
                let handle = self.tool.falloff_hovered.unwrap();
                if let Some(idx) = self.active_prim_index_unified() {
                    if idx < self.scene.primitives.len() {
                        self.begin_episode("Falloff drag");
                        let prim = &self.scene.primitives[idx];
                        let k_val = prim.operation.blend_k();
                        self.start_operation(ActiveTool::FalloffAdjust(FalloffDrag {
                            prim_idx: idx, handle,
                            start_mouse: mp,
                            start_offset: prim.blend_falloff.origin_offset,
                            start_length: prim.blend_falloff.length,
                            start_k: k_val,
                            start_direction: prim.blend_falloff.direction,
                        }));
                    }
                }
            }
            // Light handle drag start (zone / softness / range) — does not steal ActiveTool.
            else if self.tool.light_handle_hovered.is_some()
                && self.tool.light_handle_drag.is_none()
                && !self.tool.current.is_busy()
            {
                let (lid, kind) = self.tool.light_handle_hovered.unwrap();
                if let Some(light) = self.scene.light_by_id(lid) {
                    let zone = if light.volumetric.zone > 0.0 { light.volumetric.zone } else { light.range };
                    let start_value = match kind {
                        app_types::LightHandleKind::Zone => zone,
                        app_types::LightHandleKind::Softness => light.volumetric.softness,
                        app_types::LightHandleKind::Range => light.range,
                    };
                    let ep = match kind {
                        app_types::LightHandleKind::Zone => "Edit volumetric zone",
                        app_types::LightHandleKind::Softness => "Edit volumetric softness",
                        app_types::LightHandleKind::Range => "Edit light range",
                    };
                    self.begin_episode(ep);
                    self.tool.light_handle_drag = Some(app_types::LightHandleDrag {
                        light_id: lid,
                        kind,
                        start_mouse: mp,
                        start_value,
                        start_zone: zone,
                    });
                }
            }
            // Proportional-Edit ORIGIN diamond drag start. Claims the press before poly
            // click/box select so grabbing the handle never also re-selects through it.
            // `poly_prop_handle_hovered` was stamped by the overlay draw, which already
            // projected the diamond — one hit-test, same contract as `blend_k_hovered`.
            else if self.poly_prop_handle_hovered
                   && self.tool.current.is_idle()
                   && !self.tool.current.is_gizmo()
                   && !self.tool.current.is_create() {
                self.poly_prop_origin_drag_start(mp);
            }
            // Blend-k handle drag start (unified for prim and body).
            // `start_k` is the handle's SIGNED start value in the target's mode
            // (blend k / seam depth / extrude strength); `start_w` is the width
            // (2nd axis) for Seam/Extrude — see edge_detect::BlendHandleMode.
            // Suppressed in Bevel mode — diamond is hidden (same screen slot as Uniform square).
            else if self.tool.blend_k_hovered
                   && self.tool.current.is_idle()
                   && !self.tool.mode.is_bevel()
                   && !self.tool.current.is_gizmo()
                   && !self.tool.current.is_create() {
                use crate::edge_detect::BlendHandleMode;
                // Shared start-value resolution: (primary signed value, width).
                let start_vals = |op: &core_math::primitives::BooleanOp| -> (f32, f32) {
                    match crate::edge_detect::op_handle_mode(op) {
                        BlendHandleMode::SeamDepth =>
                            (crate::edge_detect::seam_signed_depth(op), op.blend_k()),
                        BlendHandleMode::ExtrudeStrength =>
                            (crate::edge_detect::extrude_signed_strength(op), op.blend_k()),
                        BlendHandleMode::BlendK => (op.blend_k(), 0.0),
                    }
                };
                // Resolve target: all valid selected prims (multi-edit), then body.
                // Anchor (first) drives start_k and handle mode; only prims matching that mode are included.
                let target = {
                    let sel = self.selected_prim_indices_unified();
                    let valid: Vec<usize> = sel.into_iter()
                        .filter(|&i| crate::edge_detect::prim_accepts_blend_handle(&self.scene, i))
                        .collect();
                    if !valid.is_empty() {
                        let anchor = self.active_prim_index_unified()
                            .filter(|a| valid.contains(a))
                            .unwrap_or(valid[0]);
                        let mode = crate::edge_detect::prim_handle_mode(&self.scene.primitives[anchor]);
                        let mut prims: Vec<usize> = valid.into_iter()
                            .filter(|&i| crate::edge_detect::prim_handle_mode(&self.scene.primitives[i]) == mode)
                            .collect();
                        // Anchor first so start_k is unambiguous.
                        if let Some(pos) = prims.iter().position(|&i| i == anchor) {
                            prims.swap(0, pos);
                        }
                        let (v, w) = start_vals(&self.scene.primitives[anchor].operation);
                        Some((BlendKTarget::Prims(prims), v, w))
                    } else { None }
                }
                .or_else(|| {
                    let nid = self.sel.usel.selected.iter().find_map(|item| {
                        if let SceneItemId::Node(n) = item { Some(*n) } else { None }
                    })?;
                    let op = self.scene.scene_graph.body_op(nid)?;
                    let (v, w) = start_vals(op);
                    Some((BlendKTarget::Body(nid), v, w))
                });
                if let Some((target, orig_k, orig_w)) = target {
                    let n = match &target { BlendKTarget::Prims(v) => v.len(), _ => 1 };
                    self.begin_episode(if n > 1 { "Blend K drag (multi)" } else { "Blend K drag" });
                    self.start_operation(ActiveTool::BlendKDrag(BlendKDrag {
                        target, start_mouse_x: mp.x, start_mouse_y: mp.y,
                        start_k: orig_k, start_w: orig_w,
                    }));
                }
            }
            // Boolean edge bevel drag start
            else if self.bool_edge_hovered.is_some()
                   && self.tool.current.is_idle()
                   && !self.tool.current.is_bool_edge_bevel()
                   && !self.tool.current.is_gizmo() && !self.tool.current.is_create() {
                let ci = self.bool_edge_hovered.unwrap();
                if let Some(curve) = self.bool_edges.get(ci) {
                    let prim_idx = curve.prim_idx;
                    if prim_idx < self.scene.primitives.len() {
                        let orig_k = crate::edge_detect::op_get_k(&self.scene.primitives[prim_idx].operation);
                        self.begin_episode("Bool edge bevel");
                        self.select_single(prim_idx);
                        self.start_operation(ActiveTool::BoolEdgeBevel(BoolEdgeBevelDrag {
                            prim_idx,
                            start_mouse_x: mp.x,
                            original_k: orig_k,
                        }));
                    }
                }
            }
            // Face drag update (Alt = both faces; Shift / Ctrl+Shift = precision)
            else if self.tool.current.is_face_push_pull() {
                self.apply_face_drag(mp, rect, ctrl, alt, shift);
            }
            // Poly extrude drag update (Shift = world-axis lock, Alt = free-grab hand-off —
            // must live here, not only keyboard: bare modifiers make handle_keyboard
            // early-return as "idle").
            else if self.tool.current.is_poly_extrude() {
                self.poly_update_extrude_drag(mp, rect, ctrl, shift, alt);
            }
            else if self.tool.current.is_poly_extrude_sweep() {
                self.poly_update_extrude_sweep(mp, rect, ctrl);
            }
            else if self.tool.current.is_poly_edge_extrude() {
                self.poly_update_edge_extrude_drag(mp, rect, ctrl);
            }
            // Poly inset drag update (Alt = depth axis)
            else if self.tool.current.is_poly_inset() {
                let alt = ctx.input(|i| i.modifiers.alt);
                self.poly_update_inset_drag(mp, rect, ctrl, alt);
            }
            // Poly shrink/fatten drag update
            else if self.tool.current.is_poly_shrink_fatten() {
                self.poly_update_shrink_fatten(mp, ctrl);
            }
            // Edge Weight is a FREE modal (see the `!pd` arm below for the no-button case) — but keep
            // tracking while a button happens to be held, so holding LMB does not freeze the value.
            else if self.tool.current.is_poly_edge_weight() {
                self.poly_update_edge_weight_drag(mp, shift, ctrl);
            }
            else if self.tool.current.is_poly_face_hinge() {
                let (sh, al) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                // Alt+primary held = free drag; also keep updating while gizmo dragging.
                let primary = ctx.input(|i| i.pointer.primary_down());
                if al && primary {
                    if let Some(d) = self.tool.current.as_poly_face_hinge_mut() {
                        if d.phase == crate::PolyFaceHingePhase::Rotate && !d.gizmo_dragging {
                            // Free mode: treat as drag from first press.
                            d.gizmo_dragging = false; // free path uses alt branch
                        }
                    }
                }
                self.poly_update_face_hinge(mp, sh, al && primary);
            }
            // Poly G/R/S sub-element transform — MUST update from the viewport path (not only
            // handle_keyboard). Keyboard-only updates miss frames where hover_pos() is None mid-drag
            // → choppy movement (GRS §1 fossil; same class as the old start_modal_transform bug).
            else if self.tool.current.is_poly_transform() {
                let (sh, ct) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                time_block!("poly_update_transform", self.poly_update_transform(mp, sh, ct));
            }
            // Poly bevel interactive drag update — ALT gates the profile-shape (mouse-Y) adjust
            // (SHIFT is the Consume Geometry toggle, handled in the keyboard modal).
            else if self.tool.current.is_poly_bevel() {
                let alt_held = ctx.input(|i| i.modifiers.alt);
                self.poly_update_bevel_drag(mp, alt_held);
            }
            // NURBS G/R/S — same as poly: update from viewport path (not keyboard-only)
            else if self.tool.current.is_nurbs_cv_move() {
                let (sh, ct) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                self.nurbs_update_cv_drag(mp, sh, ct);
            }
            // NURBS fillet radius drag
            else if self.tool.current.is_nurbs_fillet() {
                self.nurbs_fillet_update(mp);
            }
            // Poly PAINT select (LMB drag): once past the click threshold, add every element under a
            // small cursor brush each frame (additive — never clears; paint ADDS per the owner's model).
            // Sub-threshold releases fall through to click-select in the paint-release handler below.
            // Skip under knife free/shape — paint must never stroke while a cut owns LMB.
            else if self.tool.current.is_poly_paint_select()
                && !self.sculpt_brush_owns_lmb()
                && self.poly_tool != PolyTool::Knife
                && self.knife_shape_kind == 0
            {
                // Sweep the brush from the PREVIOUS mouse position to the current one (capsule), so a fast
                // drag never leaves gaps between per-frame samples ("paint select not reliable").
                // Carry the stroke's SHIFT flag out with the segment: it is latched at stroke start
                // (`PolyPaintSelectState.shift`) so releasing Shift mid-drag does not flip the brush
                // from erase to add halfway through — the same latch-at-start rule the region select
                // uses for front/through.
                let paint_seg = if let ActiveTool::PolyPaintSelect(ref mut ps) = self.tool.current {
                    if !ps.painted && (mp - ps.start).length() > POLY_PAINT_DRAG_THRESHOLD {
                        ps.painted = true;
                    }
                    let prev = ps.last;
                    ps.last = mp;
                    if ps.painted { Some((prev, ps.shift)) } else { None }
                } else { None };
                if let Some((prev, erase)) = paint_seg {
                    // Paint what you see: front-only follows the global visible_only toggle.
                    let front = self.sel.visible_only;
                    self.poly_paint_stroke_apply(prev, mp, self.poly_paint_radius, front, rect, erase);
                }
            }
            // Poly box select: start drag on first LMB press, update rect on drag
            else if self.tool.current.is_poly_box_select() {
                if let ActiveTool::PolyBoxSelect(ref mut bs) = self.tool.current {
                    if bs.start == egui::pos2(0.0, 0.0) {
                        // First frame of drag — set start
                        bs.start = mp;
                        bs.current = mp;
                        bs.shift = shift;
                    } else {
                        bs.current = mp;
                    }
                }
            }
            // Poly circle select: paint selection while LMB held
            else if self.tool.current.is_poly_circle_select() {
                if let ActiveTool::PolyCircleSelect(ref mut cs) = self.tool.current {
                    cs.active = true;
                }
                let radius = if let ActiveTool::PolyCircleSelect(ref cs) = self.tool.current { cs.radius } else { 40.0 };
                self.poly_circle_select_apply(mp, radius, false, rect);
            }
            // Face drag start
            else if self.tool.work_mode == WorkMode::Face && self.tool.hovered_face.is_some()
                   && !self.tool.current.is_gizmo() && !self.tool.current.is_create()
                   && !self.tool.current.is_bevel() {
                let (idx, local_n) = self.tool.hovered_face.unwrap();
                if idx < self.scene.primitives.len() && self.scene.primitives[idx].locked {
                    self.toast("Object is locked");
                } else {
                    let face_data = if idx < self.scene.primitives.len() {
                        let p = &self.scene.primitives[idx];
                        crate::face_ops::shape_face_size_and_curve_uv(&p.shape)
                            .map(|(ext, uv)| (p.position, ext, p.scale, uv))
                    } else { None };
                    if let Some((pos, ext, scale, curve_uv)) = face_data {
                        self.begin_episode("Push/Pull Face");
                        self.start_operation(ActiveTool::FacePushPull(FaceDrag {
                            prim_idx: idx, local_normal: local_n,
                            start_mouse: mp,
                            original_pos: pos, original_extents: ext,
                            original_scale: scale,
                            accumulated_delta: 0.0,
                            original_curve_uv: curve_uv,
                        }));
                        self.select_single(idx);
                    }
                }
            }
            // Poly/NURBS tool drag start (armed tool + LMB press)
            else if self.tool.current.is_idle() && self.poly_tool == PolyTool::ExtrudeFace
                && !self.poly_sel.selected_faces.is_empty()
                && self.modeling_context == ModelingContext::Polygonal {
                self.poly_start_extrude_drag(mp);
            }
            else if self.tool.current.is_idle() && self.poly_tool == PolyTool::InsetFace
                && !self.poly_sel.selected_faces.is_empty()
                && self.modeling_context == ModelingContext::Polygonal {
                self.poly_start_inset_drag(mp);
            }
            else if self.tool.current.is_idle() && self.poly_tool == PolyTool::LoopCut
                && self.modeling_context == ModelingContext::Polygonal {
                // Loop cut starts on the CLICK (press+release) handled at the top of
                // handle_viewport_input via clicked_by → poly_start_loop_cut_slide. Consume the press
                // here so a loop-cut-mode click never falls through to box-select.
            }
            else if self.tool.current.is_idle() && self.nurbs_tool == NurbsTool::MoveCv
                && !self.nurbs_sel.selected_cvs.is_empty()
                && self.modeling_context == ModelingContext::Nurbs {
                self.nurbs_start_cv_drag(mp);
            }
            else if self.tool.current.is_selection_box(){/* ongoing */}
            // CurveBevelTransform (diamond press-drag) OWNS the LMB for its whole gesture — the
            // radius update already ran at the top of this fn. Without this exclusion the drag
            // fell through to the arms below and start_operation(SelectionBox) fired mid-bevel:
            // debug_assert panic in tool_mode.rs::start_operation → app death that presented as
            // "[GPU-LOST] reason=Dropped msg=Device is dying." (wgpu teardown during unwind).
            else if self.tool.current.is_curve_bevel(){/* ongoing — updated at is_curve_bevel arm above */}
            else if !self.tool.current.is_gizmo()&&!self.tool.current.is_transform()&&!self.tool.current.is_face_push_pull()
                && !self.tool.current.is_poly_extrude()&&!self.tool.current.is_poly_extrude_sweep()&&!self.tool.current.is_poly_edge_extrude()&&!self.tool.current.is_poly_inset()&&!self.tool.current.is_poly_bevel()&&!self.tool.current.is_poly_loop_cut_slide()&&!self.tool.current.is_poly_shrink_fatten()
                && !self.tool.current.is_poly_face_hinge()
                && !self.tool.current.is_poly_box_select()&&!self.tool.current.is_poly_circle_select()&&!self.tool.current.is_poly_paint_select()&&!self.tool.current.is_poly_region_select()
                && !self.tool.current.is_nurbs_cv_move()&&!self.tool.current.is_nurbs_fillet()&&!self.tool.current.is_poly_transform()
                && !self.tool.current.is_poly_offset_loop()
                // Edge Weight owns the LMB for its whole gesture (drag = strength) — without this the
                // press falls through and arms box/paint select on top of the live modal.
                && !self.tool.current.is_poly_edge_weight(){
                // Gizmo HANDLE drag — any highlighted component wins over box/paint select.
                // Backup if the press-path arm above missed (egui drag without button_pressed).
                let gizmo_hit = self.tool.gizmo_hovered;
                let has_obj_sel = self.active_prim_index_unified().is_some()
                    || self.sel.usel.active_node().is_some()
                    || self.sel.usel.has_lights();
                if let Some(comp) = gizmo_hit {
                    if !self.tool.suppress_gizmo_drag {
                        if !self.try_start_box_face_extrude(comp, mp) {
                            self.start_gizmo_drag(comp, mp, shift);
                        }
                    }
                }
                // Free center (object / SDF prim): Alt+LMB only when G/R/S armed — NEVER bare LMB.
                // Primary free-start is the threshold path above (poly parity); this is a
                // backup if that arm missed. Allow SDF Vertex (prim edit) as well as Object.
                else if alt
                    && !in_sub_edit
                    && !self.tool.suppress_gizmo_drag
                    && free_tool_active
                    && has_obj_sel
                    && gizmo_hit.is_none()
                    && (self.tool.work_mode == WorkMode::Object
                        || (self.modeling_context == ModelingContext::Sdf
                            && self.tool.work_mode == WorkMode::Vertex))
                    && !self.tool.current.is_gizmo()
                {
                    self.start_gizmo_drag(GizmoComponent::Center, mp, shift);
                }
                // Spine point drag lives outside ActiveTool — must not arm the scene rect-select
                // while a spine handle is being moved/created (was stealing the gesture).
                else if self.spine_pt_drag.is_some() { /* spine drag owns LMB */ }
                // Object-mode LMB marquee (when free is Alt, bare LMB can box even if GRS armed).
                // Poly edit uses paint (below); RMB shape select is the preferred marquee path.
                // Never while gizmo is highlighted (handle owns the gesture).
                // Never while SDF Bevel/Extrude mode owns the viewport — empty LMB must go through
                // click-select's drop-then-deselect ladder (not arm a rubber-band that races Hybrid
                // pick → deselect on the same gesture as the tool drop).
                else if self.tool.current.is_idle()
                    && (self.tool.work_mode==WorkMode::Object || (self.tool.work_mode==WorkMode::Vertex && self.modeling_context==ModelingContext::Sdf))
                    && !in_sub_edit
                    && !self.sdf_sculpt_active
                    && !self.tool.curve_press_hold
                    && self.spine_edit_prim_index().is_none()
                    && !self.tool.mode.is_bevel()
                    && !self.tool.mode.is_extrude()
                    && !alt
                    && gizmo_hit.is_none()
                    // Long-press link/match owns the pointer once armed (Parent / Match TRS).
                    && !object_link_owns
                {
                    self.tool.selection_box_secondary = false;
                    self.tool.selection_box_subtract = false;
                    self.start_operation(ActiveTool::SelectionBox(SelectionDrag::Box{start:mp}));
                }
                // Click empty space in SDF extrude mode → exit
                else if self.tool.work_mode==WorkMode::Face && self.tool.mode.is_extrude() && self.tool.hovered_face.is_none() {
                    self.set_mode(ToolMode::Select);
                    self.tool.work_mode = WorkMode::Object;
                    self.sel.edit_target = None;
                    self.sel.selected_faces.clear();
                    self.toast("Extrude mode OFF");
                }
                // Poly edit: auto PAINT-select on LMB drag if press path missed.
                // Never while Alt held (Alt+LMB = free center / loop-ring). Never under knife.
                // Never while gizmo is highlighted (any component). Bare LMB = paint, never free G/R/S.
                else if self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode != PolyWorkMode::Object
                    && !self.sculpt_brush_owns_lmb()
                    && self.tool.current.is_idle()
                    && self.poly_tool == PolyTool::Idle
                    && self.knife_shape_kind == 0
                    && !alt
                    && gizmo_hit.is_none()
                {
                    self.tool.current = ActiveTool::PolyPaintSelect(PolyPaintSelectState {
                        start: mp, last: mp, painted: true, shift,
                    });
                    // Immediate first dab so the arm frame paints under the cursor.
                    let front = self.sel.visible_only;
                    self.poly_paint_stroke_apply(mp, mp, self.poly_paint_radius, front, rect, shift);
                }
            }
            if self.tool.current.is_gizmo()&&!self.tool.current.is_face_push_pull(){self.update_gizmo_drag(mp,rect,ctrl,shift,alt);}
        }}
        // Object-mode shape/rectangular select on RMB drag (same gesture language as poly
        // region: LMB paint-or-click, RMB marquee, Alt+LMB free). Poly sub-edit arms
        // PolyRegionSelect above; this arm is object / SDF-edit only.
        // Short RMB cancels tools; Ctrl+RMB click = context menu; Shift+RMB = Action Center.
        if response.drag_started_by(egui::PointerButton::Secondary)
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
            && (self.tool.work_mode == WorkMode::Object
                || (self.tool.work_mode == WorkMode::Vertex
                    && self.modeling_context == ModelingContext::Sdf))
            && !in_sub_edit
            && !self.sdf_sculpt_active
            && !self.tool.curve_press_hold
            && self.spine_edit_prim_index().is_none()
            && self.poly_tool == PolyTool::Idle
            && self.nurbs_tool == NurbsTool::Idle
            && !self.tool.mode.is_bevel()
            && !self.tool.mode.is_extrude()
            && self.tool.bool_cut.is_none()
            && self.curve_copy_angle_arm.is_none()
        {
            if let Some(mp) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.press_origin()))
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            {
                let start = ctx
                    .input(|i| i.pointer.press_origin())
                    .unwrap_or(mp);
                self.tool.selection_box_secondary = true;
                self.tool.selection_box_subtract = shift; // Shift+RMB = deselect
                self.start_operation(ActiveTool::SelectionBox(SelectionDrag::Box { start }));
            }
        }
        // Poly tools: update on mouse move even when LMB not held (keyboard-initiated G/R/S, E, I, …).
        // Prefer interact_pointer_pos when LMB is down (above); here hover_pos covers modal tools.
        if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
            if self.tool.current.is_poly_extrude() && !pd {
                let alt = ctx.input(|i| i.modifiers.alt);
                self.poly_update_extrude_drag(mp, rect, ctrl, shift, alt);
            }
            if self.tool.current.is_poly_extrude_sweep() && !pd {
                self.poly_update_extrude_sweep(mp, rect, ctrl);
            }
            if self.tool.current.is_poly_edge_extrude() && !pd {
                self.poly_update_edge_extrude_drag(mp, rect, ctrl);
            }
            if self.tool.current.is_poly_inset() && !pd {
                let alt = ctx.input(|i| i.modifiers.alt);
                self.poly_update_inset_drag(mp, rect, ctrl, alt);
            }
            if self.tool.current.is_poly_shrink_fatten() && !pd {
                self.poly_update_shrink_fatten(mp, ctrl);
            }
            // Edge Weight: the PRIMARY path — a free modal tracks the bare cursor, no button held, so
            // that a click is left free to accept. Also carries the live Shift/Ctrl reads, which
            // handle_keyboard cannot (it early-returns as "idle" while a bare modifier is down).
            if self.tool.current.is_poly_edge_weight() && !pd {
                self.poly_update_edge_weight_drag(mp, shift, ctrl);
            }
            if self.tool.current.is_poly_face_hinge() {
                let (sh, al) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                let primary = ctx.input(|i| i.pointer.primary_down());
                // Always run for hover; angle updates when alt+drag or gizmo drag.
                self.poly_update_face_hinge(mp, sh, al && primary);
                let _ = pd;
            }
            // Keyboard G/R/S: no LMB held — must still track mouse every frame (was keyboard-only → choppy).
            if self.tool.current.is_poly_transform() && !pd {
                let (sh, ct) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                time_block!("poly_update_transform", self.poly_update_transform(mp, sh, ct));
            }
            if self.tool.current.is_poly_bevel() && !pd {
                let alt_held = ctx.input(|i| i.modifiers.alt);
                self.poly_update_bevel_drag(mp, alt_held);
            }
            // Offset Edge Loop is a Blender-style modal (no button held) — track the mouse from the
            // viewport path too, so a frame where handle_keyboard early-returns cannot stall it.
            if self.tool.current.is_poly_offset_loop() && !pd {
                self.poly_update_offset_loop_drag(mp, rect);
            }
        }
        // Loop Cut session (Ctrl+R): auto-enter on hover + follow the ring; the BARE CURSOR slides the
        // loop once it engages (no button held — owner 2026-08-11 "make it just drag, so clic will
        // accept"). The LMB accept lives with the other modal confirms at the top of this function,
        // where `tool_consumed_click` can stop the accepting click from also running click-select.
        if self.poly_tool == PolyTool::LoopCut {
            self.poly_loop_cut_session_tick(rect);
            // RIGHT-click confirms the cut. Read the raw press (not response.clicked_by, which fails
            // when the click carries any movement / gets read as a camera orbit) so it fires reliably.
            // Always consume RMB — even when already confirmed above — so the context menu never opens.
            if self.tool.current.is_poly_loop_cut_slide()
                && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)
                    || i.pointer.button_released(egui::PointerButton::Secondary)
                    || i.pointer.button_clicked(egui::PointerButton::Secondary))
            {
                if self.tool.current.is_poly_loop_cut_slide() {
                    self.poly_confirm_loop_cut_slide();
                }
                rmb_consumed = true;
                // The confirm fires on the PRESS frame; `clicked_by` (context-menu arm) fires on
                // the RELEASE frame, when the tool state is already Idle. Persist the swallow.
                self.tool.rmb_swallow_click = true;
            } else if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                // No press/release handling: the update itself engages the slide on cursor travel
                // (`SLIDE_ENGAGE_PX`), so the button never has to mean "begin adjusting".
                self.poly_update_loop_cut_slide(mp, rect);
            }
        }
        // Dice session: LMB-drag adjusts segs; Enter confirms (keyboard); Esc/RMB cancels.
        if self.tool.current.is_poly_dice() {
            let shift = ctx.input(|i| i.modifiers.shift);
            if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)
                || i.pointer.button_released(egui::PointerButton::Secondary)
                || i.pointer.button_clicked(egui::PointerButton::Secondary))
            {
                self.poly_cancel_dice();
                rmb_consumed = true;
                self.tool.rmb_swallow_click = true;
            } else if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                    self.poly_dice_begin_drag(mp);
                }
                if ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                    self.poly_dice_end_drag();
                }
                self.poly_update_dice(mp, shift);
            }
        }
        // Vertex / Edge slide modal (Shift+V / Shift+E): mouse drag drives the factor; LMB/Enter confirm,
        // RMB/Esc cancel (Tool Exit Convention), C toggles clamp, ALT (held) = slide BEYOND the shape
        // (Blender's Alt-unclamp). Runs while a slide tool is active.
        if self.tool.current.is_poly_slide() {
            let alt_held = ctx.input(|i| i.modifiers.alt);
            if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                if self.tool.current.is_poly_vert_slide() { self.poly_update_vert_slide(mp, rect, alt_held); }
                else { self.poly_update_edge_slide(mp, rect, alt_held); }
            }
            let cancel = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary) || i.key_pressed(egui::Key::Escape));
            let confirm = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary) || i.key_pressed(egui::Key::Enter));
            let clamp = ctx.input(|i| i.key_pressed(egui::Key::C));
            if cancel { self.poly_cancel_slide(); }
            else if confirm { self.poly_confirm_slide(); }
            else if clamp { self.poly_slide_toggle_clamp(); }
        }
        // Poly box select: release = apply selection, single-shot exit
        let mut poly_click_done = poly_region_click_done; // Alt+LMB region already owned this LMB
        // Poly PAINT select release:
        //   past-threshold → strokes already applied per frame (just exit)
        //   sub-threshold  → click-select (same as box tiny-drag)
        // ALWAYS set poly_click_done: painted strokes must not fall through to
        // handle_click_select on the same primary-up (that re-ran click-select and
        // wiped / replaced the paint — "paint select is broken since shape select").
        // If knife/shape is active, drop paint silently — never click-select under a cut.
        if self.tool.current.is_poly_paint_select() && !pd {
            let knife_owns = self.poly_tool == PolyTool::Knife || self.knife_shape_kind != 0;
            let (painted, release_pos, additive) = if let ActiveTool::PolyPaintSelect(ref ps) = self.tool.current {
                (ps.painted, ps.last, ps.shift)
            } else { (false, egui::pos2(0.0, 0.0), false) };
            if !painted && !knife_owns {
                // Plain LMB click (no Alt — paint never arms while Alt held).
                let now = ctx.input(|i| i.time);
                self.poly_release_click_select(release_pos, additive, ctrl, false, now);
            }
            poly_click_done = true;
            self.tool.current = ActiveTool::Idle;
        }
        if self.tool.current.is_poly_box_select() && !pd {
            let (sr, additive, release_pos) = if let ActiveTool::PolyBoxSelect(ref bs) = self.tool.current {
                let r = egui::Rect::from_two_pos(bs.start, bs.current);
                (r, bs.shift, bs.current)
            } else { (egui::Rect::NOTHING, false, egui::pos2(0.0, 0.0)) };
            if sr.width() > 4.0 || sr.height() > 4.0 {
                self.poly_box_select_apply(sr, rect, additive);
            } else {
                // Tiny box = click. Shared click-select path (Alt=loop/ring, manual double-click =
                // loop/linked, else single) — see poly_release_click_select.
                let now = ctx.input(|i| i.time);
                self.poly_release_click_select(release_pos, additive, ctrl, alt, now);
                poly_click_done = true;
            }
            self.tool.current = ActiveTool::Idle;
        }
        // Poly circle select: deactivate painting on LMB release (stay in circle mode)
        if self.tool.current.is_poly_circle_select() && !pd {
            if let ActiveTool::PolyCircleSelect(ref mut cs) = self.tool.current {
                cs.active = false;
            }
        }
        // Pivot edit: release = confirm
        if self.tool.current.is_pivot_edit() && !pd {
            self.confirm_pivot_mode();
        }
        // Spine LMB move/add: release = confirm. Modal G/S/R confirm on click/Enter (keyboard).
        if self.spine_pt_drag.as_ref().map(|d| !d.modal).unwrap_or(false) && !pd {
            self.spine_pt_confirm_drag();
        }
        // Modal spine G/S/R: LMB click confirms
        if self.spine_pt_drag.as_ref().map(|d| d.modal).unwrap_or(false)
            && ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
        {
            self.spine_pt_confirm_drag();
        }
        // BoolCut: Drawing → Depth on LMB release (if footprint has size)
        if self.tool.bool_cut.as_ref().map_or(false, |bc| bc.phase == BoolCutPhase::Drawing) && !pd {
            let mouse_y = ctx.input(|i| i.pointer.hover_pos().map(|p| p.y).unwrap_or(0.0));
            let (ok, tiny) = self.tool.bool_cut.as_ref().map_or((false, true), |bc| {
                let tiny = bc.local_u.abs() <= 0.01 && bc.local_v.abs() <= 0.01;
                (true, tiny)
            });
            if ok {
                if tiny {
                    self.cancel_bool_cut();
                } else if let Some(ref mut bc) = self.tool.bool_cut {
                    bc.phase = BoolCutPhase::Depth;
                    bc.depth_start_mouse_y = mouse_y;
                    self.toast("Bool Cut depth — move mouse up/down, LMB confirm, RMB cancel");
                }
            }
        }
        // Create tool: Footprint → Extrude on LMB release
        if self.tool.current.is_create() && !pd {
            let mouse_pos = ctx.input(|i| i.pointer.hover_pos().unwrap_or(egui::pos2(0.0, 0.0)));
            let (ray_o, ray_d) = self.cam_ray(mouse_pos, rect);
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * glam::Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let cam_up = (self.viewport.camera.orbit * glam::Vec3::Y).normalize();
            let transition = {
                if let Some(cs) = self.tool.current.as_create_mut() {
                    if cs.phase == CreatePhase::Footprint {
                        if cs.local_u.abs() > 0.01 || cs.local_v.abs() > 0.01 {
                            cs.phase = CreatePhase::Extrude;
                            // Fence-plane: capture t₀ so depth starts at 0
                            let draw_center = cs.anchor + cs.tangent_u * (cs.local_u * 0.5)
                                                        + cs.tangent_v * (cs.local_v * 0.5);
                            cs.depth_start_t = crate::create_tool::compute_extrude_depth(
                                ray_o, ray_d, draw_center, cs.hit_normal, cam_pos, cam_up
                            ).unwrap_or(0.0);
                            cs.depth = 0.0;
                            1 // transition to extrude
                        } else { 2 } // cancel — footprint too small
                    } else { 0 } // already in extrude
                } else { 0 }
            };
            if transition == 1 {
                crate::ops::create::session::finalize_footprint(self, ctrl && shift);
                self.toast("Drag to set height");
                // Immediately compute first depth so the prim isn't stuck
                // at init_he_y (0.001) for one frame.
                crate::ops::create::session::update_extrude(self, mouse_pos, rect, shift, alt);
            }
            if transition == 2 {
                // Footprint too small — user just clicked without dragging.
                // Place a default-sized shape at the click point instead of cancelling.
                crate::ops::create::session::cancel(self);
                crate::ops::create::click_place::run(self, mouse_pos, rect);
            }
        }
        let mut gizmo_just_finished = false;
        if self.tool.current.is_gizmo()&&!pd{
            // A gizmo press released with NO movement is a CLICK, not a transform. In poly edit mode the
            // gizmo sits ON the selection, so clicks meant for select/double-click landed on it and were
            // eaten — the "double-click select-all works 1 in 3 when G/R/S is active" bug. Detect the
            // no-move click and route it through the SAME poly click path so it participates in the
            // double-click sequence (a real drag moves >4px and is unaffected).
            let gizmo_click_pos = if self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode != PolyWorkMode::Object {
                self.tool.current.as_gizmo().map(|gi| gi.drag_start_mouse).and_then(|s| {
                    let cur = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(s);
                    if (cur - s).length() < 4.0 { Some(s) } else { None }
                })
            } else { None };
            self.finish_gizmo_drag();
            gizmo_just_finished=true;
            if let Some(pos) = gizmo_click_pos {
                let now = ctx.input(|i| i.time);
                self.poly_release_click_select(pos, shift, ctrl, alt, now);
                poly_click_done = true;
            }
        }
        // Confirm-on-release transforms (Alt+LMB drag / free gizmo click)
        if !pd {
            if self.tool.current.as_transform().map_or(false, |t| t.confirm_on_release) { self.confirm_modal_transform(); }
            if self.tool.current.as_poly_transform().map_or(false, |t| t.confirm_on_release) { self.poly_confirm_transform(); }
            if self.tool.current.as_nurbs_cv_move().map_or(false, |t| t.confirm_on_release) {
                self.nurbs_confirm_cv_drag();
            }
        }
        let mut drag_just_finished = false;
        if self.tool.current.is_falloff_adjust()&&!pd{
            // Click-release without moving falloff params → cancel empty episode.
            let noop = self.tool.current.as_falloff_adjust().map(|fd| {
                if fd.prim_idx >= self.scene.primitives.len() { return true; }
                let p = &self.scene.primitives[fd.prim_idx];
                (p.blend_falloff.origin_offset - fd.start_offset).abs() < 1e-8
                    && (p.blend_falloff.length - fd.start_length).abs() < 1e-8
                    && (p.operation.blend_k() - fd.start_k).abs() < 1e-8
                    && (p.blend_falloff.direction - fd.start_direction).length_squared() < 1e-12
            }).unwrap_or(true);
            if noop { self.cancel_episode(); } else { self.commit_episode(); }
            self.tool.current = ActiveTool::Idle;self.rebake_no_undo();drag_just_finished=true;
        }
        if self.tool.light_handle_drag.is_some() && !pd {
            self.commit_episode();
            self.tool.light_handle_drag = None;
            drag_just_finished = true;
        }
        if self.tool.current.is_poly_prop_origin() && !pd {
            self.poly_prop_origin_drag_end();
            drag_just_finished = true;
        }
        if self.tool.current.is_blend_k_drag()&&!pd{
            // If mouse barely moved, treat as click → cycle (profile for prim, op for body)
            let mut discrete_change = false;
            if let Some(bd) = self.tool.current.as_blend_k_drag() {
                let (cur_x, cur_y) = ctx.input(|i| i.pointer.hover_pos()
                    .map(|p| (p.x, p.y)).unwrap_or((bd.start_mouse_x, bd.start_mouse_y)));
                let dx = (cur_x - bd.start_mouse_x).abs();
                let dy = (cur_y - bd.start_mouse_y).abs();
                if dx < 4.0 && dy < 4.0 {
                    // The diamond CLICK cycles the target's discrete profile; a >4px DRAG
                    // (either axis — the handle is 2-axis for Seam/Extrude) already adjusted
                    // the continuous values each frame. `orig_k`/`orig_w` restore the
                    // drag-start values so a click is a pure profile toggle (the tiny sub-4px
                    // drag may have nudged them).
                    let orig_k = bd.start_k;
                    let orig_w = bd.start_w;
                    match &bd.target {
                        BlendKTarget::Prims(indices) => {
                            let indices = indices.clone();
                            if let Some(&idx) = indices.first() {
                            if idx < self.scene.primitives.len()
                                && crate::edge_detect::prim_accepts_blend_handle(&self.scene, idx)
                            {
                                use crate::edge_detect::BlendHandleMode;
                                match crate::edge_detect::prim_handle_mode(&self.scene.primitives[idx]) {
                                    // Blend-k target: cycle Smooth <-> Chamfer on ALL multi targets.
                                    BlendHandleMode::BlendK => {
                                        use core_math::primitives::BlendProfile;
                                        // Cycle Smooth ↔ Chamfer only (curve feature removed).
                                        let next = match self.scene.primitives[idx].blend_profile {
                                            BlendProfile::Chamfer => BlendProfile::Polynomial,
                                            _ => BlendProfile::Chamfer,
                                        };
                                        let label = crate::properties_sdf::blend_profile_label(next);
                                        for &i in &indices {
                                            if i < self.scene.primitives.len() {
                                                self.write_prim(i, |p| {
                                                    crate::edge_detect::op_set_k(&mut p.operation, orig_k);
                                                    p.blend_profile = next;
                                                });
                                            }
                                        }
                                        if indices.len() > 1 {
                                            self.toast(&format!("Blend: {} ({} prims)", label, indices.len()));
                                        } else {
                                            self.toast(&format!("Blend: {}", label));
                                        }
                                        discrete_change = true;
                                    }
                                    // Seam target: cycle the seam PROFILE V→U→Square (drag = depth).
                                    BlendHandleMode::SeamDepth => {
                                        use core_math::primitives::BooleanOp;
                                        let cur = match &self.scene.primitives[idx].operation {
                                            BooleanOp::Seam { profile, .. } => *profile,
                                            _ => 0,
                                        };
                                        let next = crate::edge_detect::seam_next_profile(cur);
                                        let label = crate::edge_detect::seam_profile_label(next);
                                        for &i in &indices {
                                            if i >= self.scene.primitives.len() { continue; }
                                            self.write_prim(i, |p| {
                                                crate::edge_detect::seam_set_signed_depth(&mut p.operation, orig_k);
                                                p.operation.set_blend_k(orig_w);
                                                if let BooleanOp::Seam { profile, .. } = &mut p.operation {
                                                    *profile = next;
                                                }
                                            });
                                        }
                                        if indices.len() > 1 {
                                            self.toast(&format!("Seam: {} ({} prims)", label, indices.len()));
                                        } else {
                                            self.toast(&format!("Seam: {}", label));
                                        }
                                        discrete_change = true;
                                    }
                                    // Extrude: no click cycle — restore drag-start on all targets.
                                    BlendHandleMode::ExtrudeStrength => {
                                        for &i in &indices {
                                            if i >= self.scene.primitives.len() { continue; }
                                            self.write_prim(i, |p| {
                                                crate::edge_detect::extrude_set_signed_strength(&mut p.operation, orig_k);
                                                p.operation.set_blend_k(orig_w);
                                            });
                                        }
                                    }
                                }
                            }
                            }
                        }
                        BlendKTarget::Body(body_nid) => {
                            use core_math::primitives::BooleanOp;
                            let body_nid = *body_nid;
                            if let Some(old_op) = self.scene.scene_graph.body_op(body_nid).cloned() {
                                if matches!(old_op, BooleanOp::Extrude { .. }) {
                                    // Extrude sub-body: no click cycle — restore drag-start values.
                                    let mut new_op = old_op;
                                    crate::edge_detect::extrude_set_signed_strength(&mut new_op, orig_k);
                                    new_op.set_blend_k(orig_w);
                                    self.scene.scene_graph.set_body_op(body_nid, new_op);
                                } else if matches!(old_op, BooleanOp::Seam { .. }) {
                                    // Seam sub-body: click cycles the seam PROFILE (drag = depth/width).
                                    let cur = if let BooleanOp::Seam { profile, .. } = old_op { profile } else { 0 };
                                    let next = crate::edge_detect::seam_next_profile(cur);
                                    let label = crate::edge_detect::seam_profile_label(next);
                                    let mut new_op = old_op;
                                    crate::edge_detect::seam_set_signed_depth(&mut new_op, orig_k);
                                    new_op.set_blend_k(orig_w); // restore width (2nd axis)
                                    if let BooleanOp::Seam { profile, .. } = &mut new_op { *profile = next; }
                                    self.scene.scene_graph.set_body_op(body_nid, new_op);
                                    if let Some(ref mut gpu) = self.gpu {
                                        if let Some(ref mut atlas) = gpu.volume_atlas {
                                            atlas.rebuild_params(&self.scene.scene_graph);
                                        }
                                    }
                                    self.toast(&format!("Seam: {}", label));
                                    discrete_change = true;
                                } else {
                                    let k = old_op.blend_k();
                                    let (new_op, label) = match old_op {
                                        BooleanOp::Union { .. } => (BooleanOp::Subtract { k }, "Subtract"),
                                        BooleanOp::Subtract { .. } => (BooleanOp::Intersect, "Intersect"),
                                        BooleanOp::Intersect => (BooleanOp::Union { k: 0.0 }, "Union"),
                                        BooleanOp::SmoothIntersect { .. } => (BooleanOp::Union { k: 0.0 }, "Union"),
                                        BooleanOp::ChamferUnion { .. } => (BooleanOp::ChamferSubtract { k }, "Chamfer Sub"),
                                        BooleanOp::ChamferSubtract { .. } => (BooleanOp::Union { k: 0.0 }, "Union"),
                                        BooleanOp::Seam { .. } | BooleanOp::Extrude { .. } => unreachable!(),
                                    };
                                    self.scene.scene_graph.set_body_op(body_nid, new_op);
                                    if let Some(ref mut gpu) = self.gpu {
                                        if let Some(ref mut atlas) = gpu.volume_atlas {
                                            atlas.rebuild_params(&self.scene.scene_graph);
                                        }
                                    }
                                    self.toast(&format!("Body: {}", label));
                                    discrete_change = true;
                                }
                            }
                        }
                    }
                    // Click path: only commit when a discrete cycle actually mutated.
                    // ExtrudeStrength restore-only would otherwise leave an empty undo.
                    if discrete_change { self.commit_episode(); } else { self.cancel_episode(); }
                } else {
                    // Drag path: cancel if continuous values still match drag-start.
                    let noop = {
                        use crate::edge_detect::BlendHandleMode;
                        let start_vals = |op: &core_math::primitives::BooleanOp| -> (f32, f32) {
                            match crate::edge_detect::op_handle_mode(op) {
                                BlendHandleMode::SeamDepth =>
                                    (crate::edge_detect::seam_signed_depth(op), op.blend_k()),
                                BlendHandleMode::ExtrudeStrength =>
                                    (crate::edge_detect::extrude_signed_strength(op), op.blend_k()),
                                BlendHandleMode::BlendK => (op.blend_k(), 0.0),
                            }
                        };
                        match &bd.target {
                            BlendKTarget::Prims(indices) => {
                                match indices.first() {
                                    Some(&idx) if idx < self.scene.primitives.len() => {
                                        let (v, w) = start_vals(&self.scene.primitives[idx].operation);
                                        (v - bd.start_k).abs() < 1e-8 && (w - bd.start_w).abs() < 1e-8
                                    }
                                    _ => true,
                                }
                            }
                            BlendKTarget::Body(nid) => {
                                match self.scene.scene_graph.body_op(*nid) {
                                    Some(op) => {
                                        let (v, w) = start_vals(op);
                                        (v - bd.start_k).abs() < 1e-8 && (w - bd.start_w).abs() < 1e-8
                                    }
                                    None => true,
                                }
                            }
                        }
                    };
                    if noop { self.cancel_episode(); } else { self.commit_episode(); }
                }
            } else {
                self.commit_episode();
            }
            self.tool.current = ActiveTool::Idle;self.rebake_no_undo();drag_just_finished=true;
        }
        if self.tool.current.is_bool_edge_bevel()&&!pd{
            // No k change → cancel empty episode (click without drag).
            let noop = self.tool.current.as_bool_edge_bevel().map(|bd| {
                if bd.prim_idx >= self.scene.primitives.len() { return true; }
                let k = crate::edge_detect::op_get_k(&self.scene.primitives[bd.prim_idx].operation);
                (k - bd.original_k).abs() < 1e-8
            }).unwrap_or(true);
            if noop { self.cancel_episode(); } else { self.commit_episode(); }
            self.tool.current = ActiveTool::Idle;self.bool_edge_dirty=true;self.rebake_no_undo();drag_just_finished=true;
        }
        if self.tool.current.is_bevel()&&!pd{
            // Yellow-square click (no drag) → reset ALL bevel. Drag → commit uniform/edge edit.
            let mut did_reset = false;
            if let Some(bd) = self.tool.current.as_bevel() {
                if bd.uniform_all {
                    let (cur_x, cur_y) = ctx.input(|i| {
                        i.pointer.hover_pos().map(|p| (p.x, p.y)).unwrap_or((bd.start_mouse_x, bd.start_mouse_y))
                    });
                    let moved = (cur_x - bd.start_mouse_x).abs() > 4.0
                        || (cur_y - bd.start_mouse_y).abs() > 4.0;
                    if !moved {
                        let idx = bd.prim_idx;
                        // Cancel the drag episode (no radius change), then undoable reset.
                        self.cancel_episode();
                        self.tool.current = ActiveTool::Idle;
                        if self.reset_prim_corner_bevel_undoable(idx) {
                            self.toast("Bevel reset");
                        }
                        did_reset = true;
                        drag_just_finished = true;
                    }
                }
            }
            if !did_reset {
                // No-op only if radii AND angles/profiles match drag-start.
                // (Old code ignored radii → radius-only bevels never entered the undo stack.)
                let noop = self.tool.current.as_bevel().map(|bd| {
                    Self::bevel_drag_is_noop(bd, &self.scene.primitives)
                }).unwrap_or(true);
                if noop { self.cancel_episode(); } else { self.commit_episode(); }
                self.tool.current = ActiveTool::Idle;self.rebake_no_undo();drag_just_finished=true;
            }
        }
        if self.tool.current.is_face_push_pull()&&!pd{
            // Zero-delta face push (click without drag) → cancel empty episode.
            let noop = self.tool.current.as_face_push_pull()
                .map(|fd| fd.accumulated_delta.abs() < 1e-8)
                .unwrap_or(true);
            if noop { self.cancel_episode(); } else { self.commit_episode(); }
            self.tool.current = ActiveTool::Idle;self.rebake_no_undo();drag_just_finished=true;
        }
        // Direct curve point/edge/add drag: commit on release (or drop an unmoved click).
        if self.tool.current.is_curve_point_transform()&&!pd{ if self.curve_pt_release_drag(){drag_just_finished=true;} }
        // Direct bevel-diamond gesture: release commits the radius drag OR (unmoved click) cycles
        // the corner type. Keyboard-B modal returns false here and confirms via clicked_by above.
        if self.tool.current.is_curve_bevel()&&!pd{ if self.curve_bevel_release_direct(){drag_just_finished=true;} }
        // Poly extrude/inset confirm on LMB release only if started via LMB drag
        // (keyboard-initiated tools confirm via LMB click below)
        if self.tool.current.is_nurbs_cv_move()&&!pd{ self.nurbs_confirm_cv_drag(); }
        // NURBS fillet: LMB release confirms (same as poly bevel Enter/LMB)
        let mut sel_box_just_finished = false;
        // Finish SelectionBox when the arming button is released (LMB or RMB).
        // RMB marquee must NOT finish on `!pd` alone — primary is never down, so the rubber-band
        // would collapse on the arm frame. `sd` defined with poly free/region ownership above.
        let sel_box_button_up = if self.tool.selection_box_secondary { !sd } else { !pd };
        if self.tool.current.is_selection_box() && sel_box_button_up {
            let m=ctx.input(|i|i.pointer.hover_pos().unwrap_or(egui::pos2(0.0,0.0)));
            let rmb_box = self.tool.selection_box_secondary;
            // Check if this was a real drag (>3px) or just a point-click
            let is_real_drag = if let ActiveTool::SelectionBox(SelectionDrag::Box{start}) = &self.tool.current {
                (m.x - start.x).abs() > 4.0 || (m.y - start.y).abs() > 4.0
            } else { false };
            // ── Polyline / Spine point-editor interception ──
            // A selection box over an active CurveExtrude / SpinePipe prim edits POINTS, not scene
            // objects. Consumed only when the editor handled it; otherwise fall through.
            // RMB marquee is object-mode only (never armed while point editor is exclusive).
            let mut curve_consumed = false;
            if !rmb_box {
            if let Some(cidx) = self.curve_edit_prim_index() {
                let start = if let ActiveTool::SelectionBox(SelectionDrag::Box{start}) = &self.tool.current { *start } else { m };
                if is_real_drag {
                    let _ = self.tool.current.take_selection_box();
                    self.curve_pt_box_select(cidx, start, m, rect, shift);
                    self.mark_dirty(DirtyFlags::RENDER);
                    sel_box_just_finished = true;
                    curve_consumed = true;
                } else if self.curve_pt_click(cidx, m, rect, shift) {
                    let _ = self.tool.current.take_selection_box();
                    self.mark_dirty(DirtyFlags::RENDER);
                    sel_box_just_finished = true;
                    curve_consumed = true;
                }
            } else if let Some(sidx) = self.spine_edit_prim_index() {
                let start = if let ActiveTool::SelectionBox(SelectionDrag::Box{start}) = &self.tool.current { *start } else { m };
                if is_real_drag {
                    let _ = self.tool.current.take_selection_box();
                    self.spine_pt_box_select(sidx, start, m, rect, shift);
                    self.mark_dirty(DirtyFlags::RENDER);
                    sel_box_just_finished = true;
                    curve_consumed = true;
                } else {
                    // Tiny box = click empty while shift-box was armed: clear point sel only.
                    let _ = self.tool.current.take_selection_box();
                    if !shift { self.spine_pt_sel.clear(); }
                    self.mark_dirty(DirtyFlags::RENDER);
                    sel_box_just_finished = true;
                    curve_consumed = true;
                }
            }
            } // !rmb_box curve/spine intercept
            if curve_consumed {
                // handled by the point editor
            } else if is_real_drag {
                // Real box drag: LMB Shift/Ctrl = add; RMB always ADDS (owner 2026-08-07),
                // Shift+RMB = deselect; plain LMB = replace.
                let subtract = rmb_box && self.tool.selection_box_subtract;
                let add = if rmb_box { !subtract } else { ctrl || shift };
                self.finish_selection_drag(m, rect, add, alt, subtract);
                // Real RMB marquee must not also open the context menu on release.
                if rmb_box {
                    rmb_consumed = true;
                }
            } else if rmb_box {
                // Tiny RMB "drag" — drop rubber-band. Plain = no menu.
                // Shift+RMB short (misclassified as box arm) → Action Center place.
                // Ctrl+RMB short → context menu via Secondary `clicked` below.
                let _ = self.tool.current.take_selection_box();
                sel_box_just_finished = true;
                if shift {
                    if ctrl || alt {
                        self.place_workplane_on_surface(m, rect);
                    } else if self.tool.action_center == crate::ActionCenter::Automatic {
                        let _ = self.action_center_place_automatic(m, rect, false);
                    } else if self.tool.action_center == crate::ActionCenter::Element {
                        let _ = self.action_center_place_element(m, rect);
                    } else {
                        self.place_workplane_on_surface(m, rect);
                    }
                    rmb_consumed = true;
                }
            } else {
                // Point-click (LMB): use raycast click-select instead
                let _ = self.tool.current.take_selection_box(); // consume the selection box
                let multi = ctrl || shift;
                // Try scene node pick first (Hybrid mode).
                // GATE: node/body picking is an OBJECT-mode concern. In SDF Vertex (Edit)
                // mode the user is selecting individual prims — running pick_node_at here
                // let a GPU-picked body node (SdfBody rendered resident, or an overlapping
                // ImportedMesh/Voxel/Poly/Nurbs body) win over the CPU prim raycast, so
                // clicking prim B intermittently selected B's whole body instead of prim B
                // ("sometimes selects the body when picking prims one after the other").
                // In Vertex mode, skip node picking entirely → always resolve to Prim(uid).
                let mut node_hit = false;
                let allow_node_pick = self.tool.work_mode == WorkMode::Object;
                if allow_node_pick && self.render.viewport_mode == ViewportRenderMode::Hybrid {
                    let px = (m.x - rect.min.x) as u32;
                    let py = (m.y - rect.min.y) as u32;
                    let w = rect.width() as u32;
                    let h = rect.height() as u32;
                    if let Some(nid) = time_block!("pick_node_at", self.pick_node_at(px, py, w, h, ctrl, shift)) {
                        if let Some(node) = self.scene.scene_graph.get(nid) {
                            match &node.payload {
                                core_math::scene_graph::NodePayload::ImportedMesh { .. }
                                | core_math::scene_graph::NodePayload::VoxelBody { .. }
                                | core_math::scene_graph::NodePayload::PolygonalBody { .. }
                                | core_math::scene_graph::NodePayload::NurbsBody { .. } => {
                                    if multi {
                                        self.toggle_select_unified(SceneItemId::Node(nid));
                                    } else {
                                        self.select_scene_node(nid);
                                    }
                                    node_hit = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if !node_hit {
                    // SDF prim raycast
                    let mut hit = self.raycast_select(m, rect, alt);
                    if let Some(h) = hit {
                        // Check if prim belongs to a different body than active
                        let clicked_body = if h < self.scene.prim_body_map.len() {
                            self.scene.prim_body_map[h].0
                        } else {
                            self.scene.active_body_nid
                        };
                        // Auto-enter the clicked prim's body if different from active
                        if clicked_body != self.scene.active_body_nid {
                            self.scene.active_body_nid = clicked_body;
                        }
                        self.deselect_scene_node();
                        if multi { self.toggle_select(h); } else { self.select_single(h); }
                    } else if !multi {
                        // Empty-click tool-drop escalation (mirrors poly_release_click_select): a click on
                        // empty space drops the armed tool (G/R/S gizmo, Bevel/Extrude, poly tool) first
                        // and keeps the selection; only a second empty click falls through to deselect.
                        if !self.drop_active_tool_only() {
                            self.deselect_scene_node();
                            self.deselect_all();
                        }
                    }
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
            self.tool.selection_box_secondary = false;
            self.tool.selection_box_subtract = false;
            sel_box_just_finished=true;
        }
        // ── CONTEXTUAL mouse shortcut (not a global rebind) ─────────────────────
        // Chord: Shift+RMB. Meaning depends on Action Center mode (T pie):
        //   • Automatic → place G/R/S pivot (snaps to face/edge centers when close)
        //   • Element   → pick face/edge/vert under cursor as Action frame (no select change)
        //   • else      → place workplane (snap-aware: edge mid / face center / surface)
        // Ctrl+Shift+RMB always = lock workplane (same snap) — works even when AC is Automatic
        //   so "set workplane to edge center" is never blocked by the pivot shortcut.
        // Alt+Shift+RMB also = workplane lock (escape hatch when AC is Automatic/Element).
        // NEVER steal Shift+RMB while a modal poly tool owns the session (Bevel/Extrude/G/R/S…) —
        // that broke RMB-cancel and made Bevel feel dead when Shift was held for profile drag.
        let modal_blocks_ac_rmb = self.tool.current.is_poly_bevel()
            || self.tool.current.is_poly_offset_loop()
            || self.tool.current.is_poly_extrude()
            || self.tool.current.is_poly_extrude_sweep()
            || self.tool.current.is_poly_edge_extrude()
            || self.tool.current.is_poly_inset()
            || self.tool.current.is_poly_transform()
            || self.tool.current.is_poly_shrink_fatten()
            || self.tool.current.is_poly_face_hinge()
            || self.tool.current.is_poly_loop_cut_slide()
            || self.tool.current.is_transform()
            || self.tool.current.is_gizmo();
        if shift && response.clicked_by(egui::PointerButton::Secondary) && !modal_blocks_ac_rmb {
            if let Some(mp) = response.interact_pointer_pos() {
                if ctrl || alt {
                    // Always lock construction plane (snap edge mid / face center / surface).
                    self.place_workplane_on_surface(mp, rect);
                } else if self.tool.action_center == crate::ActionCenter::Automatic {
                    let _ = self.action_center_place_automatic(mp, rect, false);
                } else if self.tool.action_center == crate::ActionCenter::Element {
                    let _ = self.action_center_place_element(mp, rect);
                } else {
                    self.place_workplane_on_surface(mp, rect);
                }
            }
        }
        // RMB on blend-k diamond → open popup (blend profile for prims, boolean type for bodies)
        else if response.clicked_by(egui::PointerButton::Secondary)
            && self.tool.blend_k_hovered
        {
            // Resolve target: all valid selected prims (multi), then body
            let target = {
                let valid: Vec<usize> = self.selected_prim_indices_unified().into_iter()
                    .filter(|&i| crate::edge_detect::prim_accepts_blend_handle(&self.scene, i))
                    .collect();
                if !valid.is_empty() {
                    let anchor = self.active_prim_index_unified()
                        .filter(|a| valid.contains(a))
                        .unwrap_or(valid[0]);
                    let mode = crate::edge_detect::prim_handle_mode(&self.scene.primitives[anchor]);
                    let mut prims: Vec<usize> = valid.into_iter()
                        .filter(|&i| crate::edge_detect::prim_handle_mode(&self.scene.primitives[i]) == mode)
                        .collect();
                    if let Some(pos) = prims.iter().position(|&i| i == anchor) {
                        prims.swap(0, pos);
                    }
                    Some(BlendKTarget::Prims(prims))
                } else { None }
            }
            .or_else(|| {
                let nid = self.sel.usel.selected.iter().find_map(|item| {
                    if let SceneItemId::Node(n) = item { Some(*n) } else { None }
                })?;
                self.scene.scene_graph.body_op(nid)?; // verify it's a body
                Some(BlendKTarget::Body(nid))
            });
            if let Some(t) = target {
                self.tool.blend_k_popup = Some(t);
            }
        }
        // RMB: cancel/confirm active operations.
        // Context menu is NO LONGER plain RMB — it steals Shift+RMB Action Center place
        // and region-select tiny releases (owner: menu often in the way).
        // Menu chord: Ctrl+RMB short-click (Ctrl+RMB *drag* stays through/x-ray marquee).
        // Skip entirely when an earlier path already used this RMB to accept a tool
        // (loop cut / knife / pipe) — otherwise cancel runs on top of the accept.
        else if response.clicked_by(egui::PointerButton::Secondary) && !rmb_consumed
            && !std::mem::take(&mut self.tool.rmb_swallow_click) {
            if self.tool.current.is_face_push_pull() {
                if let Some(fd) = self.tool.current.take_face_push_pull() {
                    {
                        let orig_pos = fd.original_pos;
                        let orig_ext = fd.original_extents;
                        self.write_prim(fd.prim_idx, |p| {
                            p.position = orig_pos;
                            if let SdfShape::Box { half_extents, .. } = &mut p.shape {
                                *half_extents = orig_ext;
                            }
                        });
                        self.cancel_episode();
                        self.rebake_no_undo();
                        self.toast("Push/Pull cancelled");
                    }
                }
            }
            else if self.tool.current.is_poly_bevel() { self.poly_cancel_bevel_drag(); self.toast("Bevel cancelled"); }
            // Tool Exit Convention: RMB = cancel + exit (knife is the ONLY RMB-confirms exception).
            else if self.tool.current.is_poly_offset_loop() { self.poly_cancel_offset_loop(); self.toast("Offset Edge Loop cancelled"); }
            // Edge Weight: RMB CANCELS (owner 2026-08-11 — the plain convention). Placed with the
            // ActiveTool modals and BEFORE the `poly_tool ==` mode arms below: an armed tool MODE
            // (Ctrl+R loop cut, knife) can coexist with this live modal, and whoever is actually
            // holding the gesture must win the button. No `rmb_consumed = true` here — this arm IS
            // inside the context-menu branch (its guard read the flag already), so handling the click
            // here is what stops the menu; assigning the flag now would be dead (as it already is in
            // five sibling arms below, which is why the compiler flags them).
            else if self.tool.current.is_poly_edge_weight() {
                self.poly_cancel_edge_weight_drag();
                self.toast("Edge Weight cancelled");
            }
            else if self.tool.current.is_poly_dice() {
                self.poly_cancel_dice();
                rmb_consumed = true;
            }
            else if self.tool.current.is_poly_loop_cut_slide() {
                self.poly_confirm_loop_cut_slide();
                rmb_consumed = true;
            }
            // Armed loop-cut tool (no session yet): swallow RMB — do not open context menu mid-tool.
            else if self.poly_tool == PolyTool::LoopCut {
                self.poly_tool = PolyTool::Idle;
                self.toast("Loop Cut cancelled");
                rmb_consumed = true;
            }
            else if self.poly_tool == PolyTool::Dice {
                self.poly_cancel_dice();
                rmb_consumed = true;
            }
            else if self.tool.current.is_poly_extrude() { self.poly_cancel_extrude_drag(); self.toast("Extrude cancelled"); }
            else if self.tool.current.is_poly_extrude_sweep() {
                self.poly_cancel_extrude_sweep();
                self.toast("Extrude Sweep cancelled");
            }
            else if self.tool.current.is_poly_edge_extrude() { self.poly_cancel_edge_extrude_drag(); self.toast("Edge Extrude cancelled"); }
            else if self.tool.current.is_poly_inset() { self.poly_cancel_inset_drag(); self.toast("Inset cancelled"); }
            else if self.tool.current.is_poly_shrink_fatten() { self.poly_cancel_shrink_fatten(); self.toast("Shrink/Fatten cancelled"); }
            else if self.tool.current.is_poly_face_hinge() { self.poly_cancel_face_hinge(); self.toast("Hinge Rotate cancelled"); }
            else if self.tool.current.is_poly_box_select() { self.tool.current = ActiveTool::Idle; self.toast("Box select cancelled"); }
            else if self.tool.current.is_poly_circle_select() { self.tool.current = ActiveTool::Idle; self.toast("Circle select off"); }
            // Knife: RMB confirms (Tool Exit Convention exception) — commit_hover=false: RMB is a
            // pure confirm/exit gesture, the cursor position is NOT a cut point (see poly_knife.rs).
            // Must NOT fall through to context menu after accept.
            else if self.poly_tool == PolyTool::Knife {
                self.poly_knife_confirm(false);
                rmb_consumed = true;
            }
            else if self.tool.current.is_nurbs_cv_move() { self.nurbs_cancel_cv_drag(); self.toast("CV move cancelled"); }
            else if self.tool.current.is_nurbs_fillet() { self.nurbs_fillet_cancel(); }
            else if self.tool.current.is_pivot_edit() { self.cancel_pivot_mode(); }
            else if self.tool.current.is_create() { crate::ops::create::session::cancel(self); }
            else if self.tool.bool_cut.is_some() { self.cancel_bool_cut(); }
            else if self.tool.current.is_selection_box() {
                self.tool.current = ActiveTool::Idle;
                self.tool.selection_box_secondary = false;
                self.tool.selection_box_subtract = false;
            }
            else if self.tool.current.is_gizmo(){self.cancel_gizmo_drag();}
            else if self.tool.current.is_transform(){self.cancel_modal_transform();}
            else if self.tool.current.is_poly_transform(){self.poly_cancel_transform();}
            else if self.tool.current.is_curve_point_transform(){self.curve_pt_cancel_transform();self.toast("Cancelled");}
            // RMB on a profile point (editor active, idle) → delete it (≥3 guard). Falls through to
            // the normal RMB path when the cursor isn't over a handle.
            else if self.curve_edit_prim_index().is_some()
                && response.interact_pointer_pos().map_or(false, |mp| self.curve_pt_rmb_delete(mp, rect)) {}
            // Spine tool: RMB on point → delete (selection if multi). No context menu.
            else if self.tool.current.is_spine_point_edit()
                && self.spine_pt_drag.is_none()
                && response.interact_pointer_pos().map_or(false, |mp| self.spine_pt_rmb_delete(mp, rect)) {}
            // Spine tool RMB not on a point: cancel an in-flight drag, else swallow the RMB so it
            // does not open the object context menu mid-tool (Space/Enter exits the tool).
            else if self.tool.current.is_spine_point_edit() {
                if self.spine_pt_drag.is_some() {
                    self.spine_pt_cancel_drag();
                    self.toast("Cancelled");
                }
                // swallow RMB so it does not open the object context menu mid-tool
            }
            else if self.tool.mode.is_bevel() { self.set_mode(ToolMode::Select); self.toast("Bevel mode OFF"); }
            else if self.tool.mode.is_extrude() {
                self.set_mode(ToolMode::Select); self.tool.work_mode = WorkMode::Object;
                self.sel.edit_target = None; self.sel.selected_faces.clear(); self.toast("Extrude mode OFF");
            }
            else if let Some(mp) = response.interact_pointer_pos() {
                let hit = self.raycast_select(mp, rect, alt);
                if let Some(idx) = hit {
                    if idx < self.scene.primitives.len() {
                        let uid = self.scene.primitives[idx].prim_uid;
                        if !self.sel.usel.selected.contains(&SceneItemId::Prim(uid)) {
                            self.select_single(idx);
                        }
                    }
                }
                // Ctrl+RMB short-click = viewport context menu (plain RMB is cancel/region only).
                // Ctrl+Shift+RMB is workplane lock (handled above with Shift). Require !shift.
                if ctrl && !shift {
                    self.ui.ctx_menu_open = true;
                    self.ui.ctx_menu_pos = mp;
                    self.ui.ctx_menu_prim = hit;
                    self.ui.ctx_menu_search.clear();
                    self.ui.ctx_menu_focus_search = true;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Plain / Shift RMB when idle: no menu (Shift = Action Center place above).
            }
        }
        // Bevel edge click — same modifiers as the rest of the app:
        //   plain LMB     → replace selection with that edge
        //   Shift+LMB     → add / remove from selection (was Ctrl; app-wide additive is Shift)
        //   Alt+LMB       → select all edges of the same parameter group (bot / top / vert)
        //   double-click  → same as Alt (group select)
        if self.tool.hovered_bevel_edge.is_some()
           && !self.tool.current.is_gizmo() && !self.tool.current.is_selection_box()
           && !self.tool.current.is_create() && !self.tool.current.is_bevel() {
            let edge = self.tool.hovered_bevel_edge.unwrap();
            let is_cyl = self.active_prim_index_unified().map_or(false, |i|
                i < self.scene.primitives.len()
                    && matches!(&self.scene.primitives[i].shape, SdfShape::Cylinder { .. }));
            let select_group = |edge: u8, is_cyl: bool| -> Vec<u8> {
                if is_cyl {
                    // Both rings share "rim" parameter space when selecting either.
                    vec![0, 1]
                } else {
                    // Box: 0–3 bottom, 4–7 top, 8–11 vertical — same chamfer/profile param group.
                    let g_start = (edge / 4) * 4;
                    (g_start..g_start + 4).map(|e| e as u8).collect()
                }
            };
            if response.double_clicked_by(egui::PointerButton::Primary)
                || (response.clicked_by(egui::PointerButton::Primary) && alt)
            {
                self.tool.selected_bevel_edges = select_group(edge, is_cyl);
            } else if response.clicked_by(egui::PointerButton::Primary) {
                if shift {
                    if self.tool.selected_bevel_edges.contains(&edge) {
                        self.tool.selected_bevel_edges.retain(|&e| e != edge);
                    } else {
                        self.tool.selected_bevel_edges.push(edge);
                    }
                } else {
                    self.tool.selected_bevel_edges = vec![edge];
                }
            }
        }
        // LMB double-click: enter child body scope if a child body is selected
        if response.double_clicked_by(egui::PointerButton::Primary)
            && self.modeling_context == ModelingContext::Sdf
        {
            if let Some(sel_nid) = self.sel.usel.active_node() {
                if self.scene.scene_graph.child_bodies(self.scene.active_body_nid).contains(&sel_nid) {
                    self.scene.active_body_nid = sel_nid;
                    // Clear node/prim selection but preserve layer/object/light across scope change.
                    self.remove_from_selection(|id| matches!(id, SceneItemId::Node(_) | SceneItemId::Prim(_)));
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        }
        // LMB double-click in Body mode: enter edit mode on the clicked body,
        // select the first prim in that body's tree.
        // Exception: double-click on a SpinePipe enters exclusive point-edit (below).
        let mut spine_dbl = false;
        if response.double_clicked_by(egui::PointerButton::Primary)
            && self.modeling_context == ModelingContext::Sdf
            && !self.tool.current.is_spine_point_edit()
            && self.tool.current.is_idle()
        {
            if let Some(mp) = response.interact_pointer_pos() {
                let hit = self.raycast_select(mp, rect, false);
                // Prefer the raycast hit; if miss, sole selected Spine still opens edit
                // (double-click on already-selected prim / gizmo area).
                let idx = hit.filter(|&h| h < self.scene.primitives.len()).or_else(|| {
                    let sel = self.selected_prim_indices_unified();
                    if sel.len() == 1 && sel[0] < self.scene.primitives.len() {
                        Some(sel[0])
                    } else {
                        None
                    }
                });
                if let Some(i) = idx {
                    if matches!(self.scene.primitives[i].shape, SdfShape::SpinePipe { .. }) {
                        self.spine_enter_edit(i);
                        spine_dbl = true;
                    }
                }
            }
        }
        if !spine_dbl && response.double_clicked_by(egui::PointerButton::Primary)
            && self.tool.work_mode == WorkMode::Object
            && self.modeling_context == ModelingContext::Sdf
            && self.tool.current.is_idle()
        {
            if let Some(mp) = response.interact_pointer_pos() {
                let hit = self.raycast_select(mp, rect, false);
                if let Some(h) = hit {
                    if h < self.scene.primitives.len() {
                        // Spine handled above — body-enter for other shapes only
                        if !matches!(self.scene.primitives[h].shape, SdfShape::SpinePipe { .. }) {
                            let clicked_body = if h < self.scene.prim_body_map.len() {
                                self.scene.prim_body_map[h].0
                            } else {
                                self.scene.active_body_nid
                            };
                            self.scene.active_body_nid = clicked_body;
                            self.switch_work_mode(WorkMode::Vertex);
                            // Select the first prim in the body
                            let first_uid = self.scene.prim_body_map.iter()
                                .find(|(nid, _)| *nid == clicked_body)
                                .and_then(|_| {
                                    self.scene.primitives.iter().enumerate()
                                        .find(|(i, _)| *i < self.scene.prim_body_map.len() && self.scene.prim_body_map[*i].0 == clicked_body)
                                        .map(|(_, p)| p.prim_uid)
                                });
                            if let Some(uid) = first_uid {
                                self.select_single_unified(SceneItemId::Prim(uid));
                            }
                        }
                    }
                }
            }
        }
        // LMB double-click fallback (poly Edge mode). Primary path is poly_release_click_select
        // (Alt+double = RING, plain double = island / Shift+double = loop). This egui path
        // covers the rare case the paint/region release path did not run.
        // Face mode: face LOOP (Shift keeps/adds; two selected faces orient the loop 1→2→3…).
        // Other modes: select connected. Polygonal sub-element only.
        let mut was_double = spine_dbl;
        // Fallback only: the manual time-based detector in the box-release path above handles this now
        // (and sets poly_click_done). This egui path stays for the rare case the box-select didn't start.
        // Skip when Alt is held — Alt+LMB is region-select owned; Alt+double ring is handled there
        // via poly_release_click_select (owner 2026-08-06: Alt+double-click edge = ring).
        if !poly_click_done
            && !poly_region_click_done
            && !alt
            && response.double_clicked_by(egui::PointerButton::Primary)
            && self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.tool.gizmo_hovered.is_none()
        {
            match self.poly_work_mode {
                PolyWorkMode::Edge => {
                    if !shift {
                        self.poly_sel.selected_edges.clear();
                    } // replace (unless Shift = add)
                    // Plain double-click fallback → loop (Alt+double ring is on the region path).
                    if let Some(e) = self.poly_sel.hovered_edge {
                        self.poly_sel.selected_edges.insert(e);
                    }
                    self.poly_select_edge_loop();
                }
                PolyWorkMode::Face | PolyWorkMode::Group => {
                    // Shift+double-click: keep existing faces and extend the loop (owner: 1,2 → 3…).
                    // Plain double-click: replace with the loop through the hovered face.
                    if !shift {
                        self.poly_sel.selected_faces.clear();
                    }
                    self.poly_select_face_loop();
                }
                _ => {
                    self.poly_select_linked_hover(shift);
                }
            }
            was_double = true;
        }
        // Gizmo reset handle click: LMB click on center in Rotate/Scale mode → reset transform
        let mut reset_handled = false;
        if !was_double && response.clicked_by(egui::PointerButton::Primary)
            && self.viewport.show_reset_handle
            && self.tool.gizmo_hovered == Some(GizmoComponent::Center)
            && !self.tool.current.is_gizmo()
            && self.tool.gizmo_mode != GizmoMode::Translate
        {
            match self.tool.gizmo_mode {
                GizmoMode::Rotate   => { self.reset_rotation();   self.toast("Rotation reset"); }
                GizmoMode::Scale    => { self.reset_scale();      self.toast("Scale reset"); }
                GizmoMode::Elongate => { self.reset_elongation(); self.toast("Elongation reset"); }
                _ => {}
            }
            reset_handled = true;
        }
        // LMB click on blend-k diamond → cycle (blend profile for prims, body_op for bodies)
        let mut blend_k_clicked = false;
        if !reset_handled && !was_double && !sel_box_just_finished && !gizmo_just_finished && !drag_just_finished
            && response.clicked_by(egui::PointerButton::Primary)
            && self.tool.blend_k_hovered
            && !self.tool.current.is_gizmo() && !self.tool.current.is_create()
        {
            // Try prim first. Same click-cycle semantics as the drag-release twin
            // above (kept in lock-step): blend-k → Smooth↔Chamfer, seam → V→U→Square
            // profile, relief → no cycle (the diamond just CONSUMES the click, no
            // select-through). No `orig_k` restore here — this branch never entered a
            // drag so the continuous value did not drift.
            if let Some(idx) = self.active_prim_index_unified() {
                if idx < self.scene.primitives.len() && idx > 0 {
                    use crate::edge_detect::BlendHandleMode;
                    match crate::edge_detect::prim_handle_mode(&self.scene.primitives[idx]) {
                        BlendHandleMode::BlendK => {
                            use core_math::primitives::BlendProfile;
                            // Cycle Smooth ↔ Chamfer only (curve feature removed).
                            let next = match self.scene.primitives[idx].blend_profile {
                                BlendProfile::Chamfer => BlendProfile::Polynomial,
                                _ => BlendProfile::Chamfer,
                            };
                            let label = crate::properties_sdf::blend_profile_label(next);
                            self.transaction("Cycle blend profile", |s| {
                                s.write_prim(idx, |p| p.blend_profile = next);
                                s.rebake_no_undo();
                            });
                            self.toast(&format!("Blend: {}", label));
                        }
                        BlendHandleMode::SeamDepth => {
                            use core_math::primitives::BooleanOp;
                            let uid = self.scene.primitives[idx].prim_uid;
                            let mut new_op = self.scene.primitives[idx].operation.clone();
                            let cur = if let BooleanOp::Seam { profile, .. } = new_op { profile } else { 0 };
                            let next = crate::edge_detect::seam_next_profile(cur);
                            let label = crate::edge_detect::seam_profile_label(next);
                            if let BooleanOp::Seam { profile, .. } = &mut new_op { *profile = next; }
                            // Same mutation path as the pie (set_prims_boolean_op = one
                            // transaction, write_prim → graph mirror, undoable).
                            self.set_prims_boolean_op(&[(uid, new_op)]);
                            self.toast(&format!("Seam: {}", label));
                        }
                        // Extrude target: no click cycle — polarity/width live on the
                        // 2-axis drag. The diamond just CONSUMES the click.
                        BlendHandleMode::ExtrudeStrength => {}
                    }
                    blend_k_clicked = true;
                }
            }
            // Then try body
            if !blend_k_clicked {
                use core_math::primitives::BooleanOp;
                let sel_nid = self.sel.usel.selected.iter().find_map(|item| {
                    if let SceneItemId::Node(nid) = item { Some(*nid) } else { None }
                });
                if let Some(body_nid) = sel_nid {
                    if let Some(old_op) = self.scene.scene_graph.body_op(body_nid).cloned() {
                        // Extrude sub-body: no click cycle (polarity/width live on the
                        // 2-axis drag) — consume the click without an op change.
                        if matches!(old_op, BooleanOp::Extrude { .. }) {
                            blend_k_clicked = true;
                        } else {
                        let (new_op, label) = if matches!(old_op, BooleanOp::Seam { .. }) {
                            // Seam sub-body: click cycles the seam PROFILE (drag = depth).
                            let cur = if let BooleanOp::Seam { profile, .. } = old_op { profile } else { 0 };
                            let next = crate::edge_detect::seam_next_profile(cur);
                            let mut seam = old_op;
                            if let BooleanOp::Seam { profile, .. } = &mut seam { *profile = next; }
                            (seam, format!("Seam: {}", crate::edge_detect::seam_profile_label(next)))
                        } else {
                            let k = old_op.blend_k();
                            let (op, name) = match old_op {
                                BooleanOp::Union { .. } => (BooleanOp::Subtract { k }, "Subtract"),
                                BooleanOp::Subtract { .. } => (BooleanOp::Intersect, "Intersect"),
                                BooleanOp::Intersect => (BooleanOp::Union { k: 0.0 }, "Union"),
                                BooleanOp::SmoothIntersect { .. } => (BooleanOp::Union { k: 0.0 }, "Union"),
                                BooleanOp::ChamferUnion { .. } => (BooleanOp::ChamferSubtract { k }, "Chamfer Sub"),
                                BooleanOp::ChamferSubtract { .. } => (BooleanOp::Union { k: 0.0 }, "Union"),
                                BooleanOp::Seam { .. } | BooleanOp::Extrude { .. } => unreachable!(),
                            };
                            (op, format!("Body: {}", name))
                        };
                        self.transaction("Cycle body boolean", |s| {
                            s.scene.scene_graph.set_body_op(body_nid, new_op);
                            s.mark_dirty(DirtyFlags::SCENE);
                        });
                        if let Some(ref mut gpu) = self.gpu {
                            if let Some(ref mut atlas) = gpu.volume_atlas {
                                atlas.rebuild_params(&self.scene.scene_graph);
                            }
                        }
                        self.toast(&label);
                        blend_k_clicked = true;
                        }
                    }
                }
            }
        }
        // LMB click = confirm / select (blocked during creation, including frame of commit)
        // `!tool_consumed_click`: this same click ALREADY accepted a modal tool (inset/extrude/bevel/
        // shrink-fatten/transform confirm, above). Those confirms set the tool to Idle, so every
        // `is_*()` guard here passes and the click fell through to handle_click_select ->
        // poly_click_select -> DESELECT — the accepting click also cleared the selection, which is why
        // "E won't extrude after confirming an inset". One click = one action; the NEXT click deselects.
        // Alt+LMB click in poly edit = edge loop / ring (free-center is Alt+LMB *drag* only).
        // Must run before handle_click_select, which would plain-select without Alt loop semantics.
        if !tool_consumed_click
            && !blend_k_clicked
            && !reset_handled
            && !was_double
            && !sel_box_just_finished
            && !poly_click_done
            && !curve_press_consumed
            && !self.tool.curve_press_hold
            && !gizmo_just_finished
            && !drag_just_finished
            && alt
            && response.clicked_by(egui::PointerButton::Primary)
            && self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.poly_tool == PolyTool::Idle
            && self.tool.current.is_idle()
            && self.tool.gizmo_hovered.is_none()
        {
            if let Some(mp) = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
            {
                let now = ctx.input(|i| i.time);
                self.poly_release_click_select(mp, shift, ctrl, true, now);
                poly_click_done = true;
            }
        }
        // `!is_spine_point_edit()`: the exclusive spine tool owns EVERY LMB click
        // (spine_pt_press — empty click = clear point selection). Without this, the same
        // click fell through here → handle_sdf_edit_click → deselect_all, deselecting the
        // spine prim mid-tool (same click-ownership class as `curve_press_consumed`).
        if !tool_consumed_click && !blend_k_clicked && !reset_handled && !was_double && !sel_box_just_finished && !poly_click_done && !curve_press_consumed && !self.tool.curve_press_hold && !gizmo_just_finished && !drag_just_finished && response.clicked_by(egui::PointerButton::Primary)&&!self.tool.current.is_gizmo()&&!self.tool.current.is_selection_box()&&!self.tool.current.is_create()&&!had_create&&!self.tool.current.is_smart_mirror()&&!self.tool.current.is_poly_bevel()&&!self.tool.current.is_clip_curve_draw()&&!self.tool.current.is_spine_point_edit(){
            self.handle_click_select(response, rect, shift, ctrl, alt);
        }
        // Sculpt drag + stroke finalization
        time_block!("handle_sculpt_input", self.handle_sculpt_input(response, rect, shift, ctrl, alt));
        // SDF sculpt brush (adds Sculpt modifiers to selected prim)
        self.handle_sdf_sculpt_input(response, rect, shift, ctrl);

        // MMB press on bool edge → reset bevel to 0
        {
            let mmb_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Middle));
            if mmb_pressed {
                if let Some(ci) = self.bool_edge_hovered {
                    if let Some(curve) = self.bool_edges.get(ci) {
                        let prim_idx = curve.prim_idx;
                        // Skip empty undo when blend-k / seam width is already ~0.
                        if self.reset_prim_blend_k_undoable(prim_idx) {
                            self.toast("Bevel reset");
                        }
                    }
                }
            }
        }

        // ---- OS file drop onto viewport ----
        // Gated on the pointer actually being over the VIEWPORT. `dropped_files` is a
        // global, window-wide event, so without this an image dropped on a material
        // panel channel row (which loads it into that channel) ALSO landed here and got
        // assigned to the hovered prim — one gesture, two different actions.
        // Position-based rather than a consumed-flag so it holds regardless of whether
        // panels draw before or after this runs.
        let pointer_in_viewport = ctx
            .input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()))
            .map(|p| rect.contains(p))
            .unwrap_or(false);
        let viewport_drops: Vec<std::path::PathBuf> = if pointer_in_viewport {
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect())
        } else {
            Vec::new()
        };
        for path in &viewport_drops {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            match ext.as_str() {
                "hdr" | "exr" => {
                    // Worker-thread decode + poll install (env_ibl.rs) — no frame stall.
                    self.request_env_load(path.clone());
                    self.toast("Loading HDR environment…");
                }
                "png" | "jpg" | "jpeg" | "tga" | "bmp" => {
                    if let Some(prim_idx) = self.tool.hovered_prim.or(self.active_prim_index_unified()) {
                        if prim_idx < self.scene.primitives.len() {
                            self.apply_dropped_image(path, prim_idx);
                        }
                    } else {
                        // No hovered prim — create billboard image plane
                        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                        // Get image dimensions for aspect ratio
                        if let Ok(img) = image::open(path) {
                            let (iw, ih) = (img.width() as f32, img.height() as f32);
                            let aspect = iw / ih.max(1.0);
                            let h = 1.0f32;
                            let w = h * aspect;
                            // Place at workplane center or origin
                            let pos = self.ray_workplane_hit(
                                response.rect.center(), response.rect
                            ).unwrap_or(glam::Vec3::new(0.0, h * 0.5, 0.0));
                            let fname2 = fname.clone();
                            self.transaction("Create Billboard", |s| {
                                // Allocate uid then advance — pre-increment reused next_prim_id
                                // as the next free id (collision with subsequent creates).
                                let uid = s.scene.next_prim_id;
                                s.scene.next_prim_id += 1;
                                let mut prim = core_math::primitives::SdfPrimitive::with_uid(
                                    fname2,
                                    core_math::primitives::SdfShape::Billboard { width: w, height: h, face_camera: false },
                                    pos, uid,
                                );
                                // Load texture into atlas
                                if let Some(tex_id) = s.atlas_add_texture(path) {
                                    prim.texture_id = tex_id;
                                }
                                let new_idx = s.add_prim_to_graph(prim);
                                let uid = s.scene.primitives[new_idx].prim_uid;
                                s.select_single_unified(SceneItemId::Prim(uid));
                                s.mark_dirty(DirtyFlags::RENDER);
                            });
                            self.toast(&format!("Billboard: {}", fname));
                        } else {
                            self.toast(&format!("Failed to load image: {}", fname));
                        }
                    }
                }
                _ => {}
            }
        }

        // ---- Drop hover overlay ----
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering_files && response.hovered() {
            let is_hdr = ctx.input(|i| i.raw.hovered_files.iter().any(|f| {
                f.path.as_ref().map_or(false, |p| {
                    p.extension().and_then(|e| e.to_str()).map_or(false, |e| e.eq_ignore_ascii_case("hdr"))
                })
            }));
            let (label, col) = if is_hdr {
                ("Drop to set environment map", egui::Color32::from_rgba_unmultiplied(40, 80, 120, 60))
            } else {
                ("Drop to apply texture", egui::Color32::from_rgba_unmultiplied(40, 120, 80, 60))
            };
            ui.painter().rect_filled(rect, 0.0, col);
            ui.painter().text(
                rect.center(), egui::Align2::CENTER_CENTER,
                label, egui::FontId::proportional(18.0),
                egui::Color32::from_white_alpha(200),
            );
        }
    }

}

