use eframe::egui;
use crate::{
    AzraApp, DirtyFlags, ModelingContext, PolyWorkMode, PolyTool,
    ActiveTool, PolyBoxSelectState, PolyCircleSelectState, PolyKeyFlags,
};

impl AzraApp {
    /// Dispatch poly tool keys from keybind flags (Phase 3 contextual keybinds).
    pub(crate) fn poly_handle_keys(&mut self, kf: &PolyKeyFlags) {
        if self.modeling_context != ModelingContext::Polygonal { return; }
        // `E` while a knife rect/circle drag is anchored = CUT DEPTH, never Extrude. Consumes
        // the chord so the two can never fire in the same frame — the same state split
        // `pipe_took_p` uses for `P` below (CLAUDE.md: same key + different state is correct;
        // a real collision is two actions firing in the SAME frame in the SAME state).
        let depth_took_e = kf.knife_depth
            && self.poly_tool == PolyTool::Knife
            && self.knife_shape_kind != 0
            && self.knife_shape_anchor.is_some();
        if depth_took_e {
            self.poly_knife_shape_depth_arm();
        }
        // `U`/`I` while the knife is armed pick the BOOLEAN TYPE of the next shape cut (owner
        // 2026-08-11). Same STATE split as `E` above: the knife is modal and owns its chords, so
        // `I` cannot also reach Inset in the same frame. `U` has no other poly binding.
        let knife_armed = self.poly_tool == PolyTool::Knife;
        let bool_took_i = kf.knife_bool_intersect && knife_armed;
        if kf.knife_bool_union && knife_armed {
            self.poly_knife_set_bool_op(crate::poly_knife::KnifeBoolOp::Union);
        }
        if bool_took_i {
            self.poly_knife_set_bool_op(crate::poly_knife::KnifeBoolOp::Intersect);
        }
        if kf.extrude && !depth_took_e {
            // P0-2: route by work mode. Face / Edge / Vertex each have a real extrude path.
            // Never arm ExtrudeFace latently when selection is invalid (was: E in Vertex mode
            // set the tool only — later Face click fired a surprise extrude).
            let mouse = self.viewport.viewport_mouse;
            match self.poly_work_mode {
                PolyWorkMode::Face | PolyWorkMode::Group => {
                    // Empty face selection is OK — start uses poly_faces_or_all (Modo full).
                    // Start itself refuses when there is no poly body.
                    self.poly_start_extrude_drag(mouse);
                }
                PolyWorkMode::Edge if !self.poly_sel.selected_edges.is_empty() => {
                    self.poly_start_extrude_drag(mouse);
                }
                PolyWorkMode::Edge => {
                    self.toast("Extrude: select edges first");
                }
                PolyWorkMode::Vertex if !self.poly_sel.selected_vertices.is_empty() => {
                    self.poly_start_extrude_drag(mouse);
                }
                PolyWorkMode::Vertex => {
                    self.toast("Extrude: select vertices first");
                }
                _ => {
                    self.toast("Extrude: enter Face / Edge / Vertex mode");
                }
            }
        }
        // Alt+E — Extrude Along Normals (Blender): region topology, per-vertex cap normals.
        if kf.extrude_normals {
            if self.poly_work_mode == PolyWorkMode::Face && !self.poly_sel.selected_faces.is_empty()
                && self.tool.current.is_idle()
            {
                let mouse = self.viewport.viewport_mouse;
                self.poly_start_extrude_along_normals_drag(mouse);
                self.set_poly_tool(PolyTool::ExtrudeFace);
                self.toast("Extrude Along Normals: drag offset, LMB/Enter confirm, Esc cancel");
            }
        }
        // Shift+Alt+E — Extrude Sweep (duct turn: mouse length/angle, scroll segments).
        if kf.extrude_sweep && self.tool.current.is_idle() {
            let mouse = self.viewport.viewport_mouse;
            self.poly_start_extrude_sweep(mouse);
        }
        if kf.inset && !bool_took_i {
            if self.poly_work_mode == PolyWorkMode::Face && !self.poly_sel.selected_faces.is_empty() {
                let mouse = self.viewport.viewport_mouse;
                self.poly_start_inset_drag(mouse);
                self.set_poly_tool(PolyTool::InsetFace);
                self.toast("Inset: drag R=inward L=outward, Alt=depth, LMB/Enter confirm, Esc cancel");
            } else if self.poly_work_mode == PolyWorkMode::Edge
                && !self.poly_sel.selected_edges.is_empty()
            {
                // OFFSET EDGE LOOP — `I` in Edge mode (Maya's Offset Edge Loop Tool / Blender's
                // Offset Edge Loop Cut). Plan `docs/planning/poly/OFFSET_EDGE_LOOP_PLAN.md` §1:
                // this slot was a DEAD key (every non-Face branch fell to `set_poly_tool` and
                // nothing happened), and no new `ActionDef` is wanted — CLAUDE.md § Input System
                // Discipline: *"Same key + different context is CORRECT, not a bug"* (F = Focus and
                // Poly Fill; E = Elongate / Extrude / Poly Extrude). Face mode above is untouched,
                // and Edge mode with an EMPTY selection still falls to the old `else`.
                self.poly_activate_offset_edge_loop();
            } else {
                self.set_poly_tool(PolyTool::InsetFace);
            }
        }
        if kf.loop_cut {
            // A live PARAMETRIC primitive has no edge loops to cut — its divisions ARE spec
            // segment counts (owner 2026-08-04: "for the segment ctrl+r, same as loop cut, so we
            // stay consistent"). Same key, same meaning ("add divisions"); the representation is
            // chosen by what is selected. Declines for authored meshes, so real Loop Cut is
            // untouched.
            if self.poly_work_mode == PolyWorkMode::Object && self.primitive_segments_modal_begin()
            {
                // Segments modal entered — nothing else to do this frame.
            } else if self.poly_tool == PolyTool::LoopCut {
                self.poly_tool = PolyTool::Idle;
                self.toast("Loop Cut cancelled");
            } else {
                self.set_poly_tool(PolyTool::LoopCut);
                self.loop_cut_count = 1;
                self.toast(
                    "Loop Cut: hover edge · move=slide · scroll=cuts · R=direction · K=through-cut · click/Enter accept",
                );
            }
        }
        if kf.vert_slide && self.poly_work_mode == PolyWorkMode::Vertex {
            self.poly_start_vert_slide();
        }
        if kf.edge_slide && self.poly_work_mode == PolyWorkMode::Edge {
            self.poly_start_edge_slide();
        }
        if kf.shrink_fatten && self.poly_work_mode != PolyWorkMode::Object {
            // Alt+S:
            //   Edge mode  → Edge Slide (along connected-face rails — not averaged normals)
            //   Vertex     → multi-vertex slide along incident edges (mouse picks direction)
            //   Face       → classic shrink/fatten along normals
            // Owner 2026-07-30: "Alt+S weird average — should slide on connect edge, mouse driven".
            match self.poly_work_mode {
                PolyWorkMode::Edge => self.poly_start_edge_slide(),
                PolyWorkMode::Vertex => self.poly_start_multi_vert_edge_slide(),
                _ => {
                    let mouse = self.viewport.viewport_mouse;
                    self.poly_start_shrink_fatten(mouse);
                }
            }
        }
        // Plain M: quick merge (Last if active target in selection, else Average/Center).
        // Alt+M opens the Merge pie (Average / Last / By Distance) — handled in handle.rs
        // so the pie can hold-to-pick like X boolean. Old PolyTool::MergeVertices mode removed.
        if kf.merge {
            if self.poly_work_mode == PolyWorkMode::Vertex
                && self.poly_sel.selected_vertices.len() >= 2
            {
                if self
                    .poly_active_vertex
                    .map_or(false, |v| self.poly_sel.selected_vertices.contains(&v))
                {
                    self.poly_merge_vertices_at_last();
                } else {
                    self.poly_merge_vertices();
                }
            } else {
                self.toast("Merge: select 2+ vertices first (Alt+M for options pie)");
            }
        }
        // merge_menu (Alt+M) — open handled in input_keyboard/handle.rs (pie path)
        // K must not arm Knife while Ctrl+R loop cut owns the session (owner 2026-08-06:
        // loop-cut K is a one-shot through-cut; it must not drop loop cut into the knife tool).
        if kf.knife && !self.poly_loop_cut_is_active() {
            self.poly_activate_knife();
        }
        if kf.bevel {
            self.poly_activate_bevel();
        }
        if kf.unbevel {
            self.poly_activate_unbevel();
        }
        if kf.face_hinge {
            self.poly_activate_face_hinge();
        }
        // Delete key routes by work mode (Blender X-menu: Delete Vertices / Edges / Faces). Vertex and
        // Edge delete were previously dead keys (audit #4/#5).
        if kf.delete_face {
            match self.poly_work_mode {
                PolyWorkMode::Face if !self.poly_sel.selected_faces.is_empty() => self.poly_delete_faces(),
                PolyWorkMode::Edge if !self.poly_sel.selected_edges.is_empty() => self.poly_delete_edges(),
                PolyWorkMode::Vertex if !self.poly_sel.selected_vertices.is_empty() => self.poly_delete_vertices(),
                _ => {}
            }
        }
        // Ctrl+Delete = Delete ONLY Faces (Blender ONLY_FACE): drop the face(s) but KEEP every edge as
        // a wire edge. Mirrors Delete (=Delete Faces) with the Ctrl modifier. Face mode only.
        if kf.delete_only_face
            && self.poly_work_mode == PolyWorkMode::Face
            && !self.poly_sel.selected_faces.is_empty()
        {
            self.poly_delete_faces_only();
        }
        if kf.subdivide { self.poly_subdivide(); }
        if kf.dice && self.tool.current.is_idle() {
            self.poly_activate_dice();
        }
        if kf.edge_loop && self.poly_work_mode == PolyWorkMode::Edge { self.poly_select_edge_loop(); }
        if kf.edge_ring && self.poly_work_mode == PolyWorkMode::Edge { self.poly_select_edge_ring(); }
        // Vertex OR Edge — `poly_fill_face` derives the ring from either selection (owner: "I did
        // not see the command to create a connected poly from selection"). Blender's F does both.
        if kf.fill
            && matches!(self.poly_work_mode, PolyWorkMode::Vertex | PolyWorkMode::Edge)
        {
            self.poly_fill_face();
        }
        // Ctrl+Alt+F before bare F paths — more specific chord.
        if kf.grid_fill && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_grid_fill();
        }
        if kf.spin && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_spin();
        }
        if self.tool.current.is_idle() && self.poly_work_mode != PolyWorkMode::Object {
            if kf.loop_relax {
                self.poly_loop_relax();
            } else if kf.loop_flatten {
                self.poly_loop_flatten();
            } else if kf.loop_space {
                self.poly_loop_space();
            }
        }
        if kf.bridge && (self.poly_work_mode == PolyWorkMode::Vertex || self.poly_work_mode == PolyWorkMode::Edge) {
            self.poly_bridge();
        }
        // P — Separate selected faces into a new body (Blender Separate by Selection).
        // `P` with a live knife polyline = PIPE. Consumes the chord so Separate (same key)
        // cannot also fire this frame -- the two are split by state, never both live.
        let pipe_took_p = kf.pipe && self.poly_tool == PolyTool::Knife && self.knife_points.len() >= 2;
        if pipe_took_p {
            self.poly_knife_make_pipe();
        }
        if kf.separate && !pipe_took_p && self.poly_work_mode == PolyWorkMode::Face && self.tool.current.is_idle() {
            self.poly_separate_selection();
        }
        // Ctrl+Shift+B — Bisect (plane: symmetry → workplane → view). Clear = +Alt.
        if self.tool.current.is_idle() && self.poly_work_mode != PolyWorkMode::Object {
            if kf.bisect_clear {
                self.poly_bisect_clear_inner();
            } else if kf.bisect {
                self.poly_bisect();
            }
        }
        // Connect Vertex Path (J) — Vertex mode only. Runs ONLY here (poly edit, normal-keyboard branch),
        // never colliding with ACT_CREATE_JOIN (J), which dispatches only in the is_create() branch.
        if kf.connect && self.poly_work_mode == PolyWorkMode::Vertex {
            self.poly_connect_vertices();
        }
        // Ctrl+X (ACT_POLY_DISSOLVE): same Event::Cut as ACT_CUT.
        // ONLY cut_selection() mutates — dual fire used to race (dissolve then body-cut).
        // Log so live console shows whether this chord was seen in poly_handle_keys.
        if kf.dissolve {
            Self::ctrx_diag(&format!(
                "poly_handle_keys ACT_POLY_DISSOLVE seen mode={:?} faces={} edges={} verts={} \
                 (no-op here; cut_selection owns the work)",
                self.poly_work_mode,
                self.poly_sel.selected_faces.len(),
                self.poly_sel.selected_edges.len(),
                self.poly_sel.selected_vertices.len(),
            ));
        }
        if kf.limited_dissolve && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_limited_dissolve();
        }
        if kf.select_similar && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_select_similar();
        }
        // Alt+V before V so the more specific bind wins when both fire.
        if kf.rip_fill && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_rip_fill();
        } else if kf.rip && self.poly_work_mode != PolyWorkMode::Object && self.tool.current.is_idle() {
            self.poly_rip();
        }
        // B / C are box & circle SELECT tools — while Knife is modal they must not steal
        // the frame (C is also knife-circle shape via raw key in handle.rs).
        if kf.box_select
            && self.poly_tool != PolyTool::Knife
            && self.poly_work_mode != PolyWorkMode::Object
            && self.tool.current.is_idle()
        {
            self.tool.current = ActiveTool::PolyBoxSelect(PolyBoxSelectState {
                start: egui::pos2(0.0, 0.0),
                current: egui::pos2(0.0, 0.0),
                shift: false,
            });
            self.toast("Box Select: drag to select");
        }
        if kf.recalc_normals {
            self.poly_recalculate_normals();
        }
        if kf.flip_normals && self.poly_work_mode == PolyWorkMode::Face {
            self.poly_flip_normals();
        }
        // Blender-style normal AUTHORING. Mark/Clear Sharp act on the EDGE selection (Edge mode);
        // Shade Smooth/Flat are body-level and work in any poly mode, Object included.
        if kf.mark_sharp && self.poly_work_mode == PolyWorkMode::Edge && self.tool.current.is_idle() {
            self.poly_set_sharp(true);
        }
        if kf.clear_sharp && self.poly_work_mode == PolyWorkMode::Edge && self.tool.current.is_idle() {
            self.poly_set_sharp(false);
        }
        // `W` — live edge WEIGHT drag (authored crease). Same authoring family as Mark Sharp above,
        // but a value rather than a flag, so it arms a modal instead of writing once. Edge mode
        // weights the selected edges; Face/Group weights the selected region's boundary (owner:
        // "if poly is selected boundery get weited edge"). `is_idle` so `W` cannot re-arm on top of
        // an in-flight modal — a live tool switch goes through `confirm_poly_modal` instead.
        if kf.edge_weight
            && self.poly_work_mode != PolyWorkMode::Object
            && self.tool.current.is_idle()
        {
            let mouse = self.viewport.viewport_mouse;
            self.poly_start_edge_weight_drag(mouse);
        }
        if kf.shade_smooth && self.tool.current.is_idle() {
            self.poly_shade_smooth();
        }
        if kf.shade_flat && self.tool.current.is_idle() {
            self.poly_shade_flat();
        }
        if kf.uv_unwrap && self.tool.current.is_idle() {
            self.poly_auto_uv_unwrap();
        }
        if kf.uv_layout {
            self.ui.show_uv_layout = !self.ui.show_uv_layout;
            self.toast(if self.ui.show_uv_layout {
                "UV Layout ON"
            } else {
                "UV Layout OFF"
            });
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if kf.proportional {
            self.poly_proportional = !self.poly_proportional;
            self.toast(if self.poly_proportional { "Proportional Edit ON" } else { "Proportional Edit OFF" });
        }
        if kf.prop_falloff {
            self.poly_prop_falloff = self.poly_prop_falloff.next();
            self.toast(&format!("Falloff: {}", self.poly_prop_falloff.label()));
        }
        // Plain C — smart UV seam (no armed tool): faces → island boundary; edges/hover → toggle.
        // Circle brush select moved to Shift+C (paint/region select is the primary path).
        if kf.seam
            && self.poly_tool != PolyTool::Knife
            && self.poly_work_mode != PolyWorkMode::Object
            && self.tool.current.is_idle()
        {
            self.poly_mark_seam_apply();
        }
        if kf.circle_select
            && self.poly_tool != PolyTool::Knife
            && self.poly_work_mode != PolyWorkMode::Object
            && self.tool.current.is_idle()
        {
            self.tool.current = ActiveTool::PolyCircleSelect(PolyCircleSelectState {
                radius: 40.0,
                active: false,
            });
            self.toast("Circle Select: LMB=paint, Scroll=resize, Esc=exit");
        }
        // Alt+C — reshape a selected loop onto a circle (LoopTools "Circle"). Face mode rounds the face
        // region's boundary; Edge mode rounds the selected EDGE LOOP directly (both share the one reshape
        // core in core_math). Vertex/Object mode TOASTS instead of silently doing nothing (bug #6: the
        // "sometimes triggers, sometimes not" was the user landing in a non-Face/Edge mode). Empty or
        // non-loop selections are toasted inside poly_circle_edges / poly_circle_faces.
        if kf.circle_region && self.tool.current.is_idle() {
            self.poly_circle();
        }
    }
}
