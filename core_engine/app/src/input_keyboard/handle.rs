//! Keyboard input handler — moved from input.rs.

use eframe::egui;
use glam::{Quat, EulerRot, Vec3};
use core_math::primitives::{SdfShape, BooleanOp, BevelMode};
use crate::*;

impl AzraApp {
    pub(crate) fn handle_keyboard(&mut self, ctx: &egui::Context, mode_pie_pressed: bool) {
        // If mouse is over the viewport (not over any UI panel), surrender widget focus
        // so keyboard shortcuts work without requiring a click first.
        let pointer_over_ui = ctx.is_pointer_over_area();
        if !pointer_over_ui && !ctx.wants_keyboard_input() {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        let any_text = ctx.wants_keyboard_input();
        // Alt+X Symmetry pie: MODAL while open, but ONLY while the pointer is over the viewport. It
        // owns the keyboard there so its tool-state keys (o/p/c/i/m) don't leak to other viewport
        // shortcuts. The instant the pointer is over a PANEL — or a text field wants keys — the pie
        // yields (closes) so the panel keeps the keyboard. This is the "recognize viewport vs panel"
        // context the input layer is supposed to honor, not a blanket keyboard grab.
        if self.ui.sym_pie.open {
            if pointer_over_ui || any_text {
                self.ui.sym_pie.open = false;
            } else {
                let mp = ctx.input(|i| i.pointer.latest_pos()).unwrap_or(self.viewport.viewport_mouse);
                self.tick_sym_pie(ctx, mp);
                return;
            }
        }
        // FAST PATH: no keys, no text focus, no modal pie — skip the 2k-line bind scan.
        // Viewport-only frames (orbit / idle) were paying 0.5–1.5 ms here every frame for nothing.
        // Held keys (sculpt size, axis constraints) keep keys_down non-empty so we stay live.
        if self.ui2_keyboard_idle(ctx, mode_pie_pressed) {
            return;
        }
        // Mode Pie key is stripped in raw_input_hook — use held flag from raw events.
        let mode_pie_held = self.input_system.mode_pie_held;

        // Voxel sculpt brush size/strength — ACT_SCULPT_BRUSH_* (state-gated Sculpt mode).
        // Strength (Shift+Z/X) ported from fix/sculpt-feel-perf as registered actions.
        if !any_text && self.tool.work_mode == WorkMode::Sculpt
            && (self.sculpt.voxel.is_some() || self.sculpt.poly_edit.is_some())
        {
            let (size_dn, size_up, str_dn, str_up) = ctx.input(|i| (
                self.keybindings.pressed(ACT_SCULPT_BRUSH_SIZE_DOWN, i),
                self.keybindings.pressed(ACT_SCULPT_BRUSH_SIZE_UP, i),
                self.keybindings.pressed(ACT_SCULPT_BRUSH_STRENGTH_DOWN, i),
                self.keybindings.pressed(ACT_SCULPT_BRUSH_STRENGTH_UP, i),
            ));
            if size_dn || size_up || str_dn || str_up {
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    // Relative steps feel better than a fixed 0.03 across the 0.05..1.0 range.
                    if size_dn { v.radius = (v.radius * 0.88).clamp(0.05, 1.0); }
                    if size_up { v.radius = (v.radius * 1.14).clamp(0.05, 1.0); }
                    if str_dn { v.strength = (v.strength - 0.05).clamp(0.05, 1.0); }
                    if str_up { v.strength = (v.strength + 0.05).clamp(0.05, 1.0); }
                } else if self.sculpt.poly_edit.is_some() {
                    if self.sculpt.poly_radius <= 1e-4 { self.sculpt.poly_radius = 0.15; }
                    if self.sculpt.poly_strength <= 1e-4 { self.sculpt.poly_strength = 0.5; }
                    if size_dn { self.sculpt.poly_radius = (self.sculpt.poly_radius * 0.88).clamp(0.01, 2.0); }
                    if size_up { self.sculpt.poly_radius = (self.sculpt.poly_radius * 1.14).clamp(0.01, 2.0); }
                    if str_dn { self.sculpt.poly_strength = (self.sculpt.poly_strength - 0.05).clamp(0.05, 2.0); }
                    if str_up { self.sculpt.poly_strength = (self.sculpt.poly_strength + 0.05).clamp(0.05, 2.0); }
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
        }
        // Voxel remesh / resolution — ACT_SCULPT_REMESH
        if !any_text && self.tool.work_mode == WorkMode::Sculpt && self.sculpt.voxel.is_some() {
            if ctx.input(|i| self.keybindings.pressed(ACT_SCULPT_REMESH, i)) {
                self.voxel_open_remesh();
            }
        }
        // Same key on a POLY sculpt target → the poly Sculpt Remesh popup (owner 2026-08-19:
        // "remesh does not work for poly object"). Same state-gated Ctrl+R split as voxel.
        else if !any_text
            && self.tool.work_mode == WorkMode::Sculpt
            && (self.sculpt.poly_edit.is_some() || self.poly_sculpt_target().is_some())
        {
            if ctx.input(|i| self.keybindings.pressed(ACT_SCULPT_REMESH, i)) {
                self.poly_sculpt_open_remesh();
            }
        }
        // Voxelize pending Move grabs — ACT_VOXEL_VOXELIZE_NOW (only when deferred moves pending).
        if !any_text && self.tool.work_mode == WorkMode::Sculpt
            && self.sculpt.voxel.as_ref().map_or(false, |v| !v.pending_move_grabs.is_empty())
            && ctx.input(|i| self.keybindings.pressed(ACT_VOXEL_VOXELIZE_NOW, i))
        {
            self.voxel_flush_pending_move();
        }
        // Shortcut editor rebinding: capture next key press
        if let Some(action_id) = self.ui.rebinding_action {
            if !any_text {
                let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                for evt in &events {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = evt {
                        let new_bind = KeyBind { key: *key, shift: modifiers.shift, ctrl: modifiers.command, alt: modifiers.alt };
                        self.ui.keybind_conflicts = self.keybindings.find_conflicts(&new_bind, action_id)
                            .iter().map(|(n,l)| (n.to_string(), l.clone())).collect();
                        self.keybindings.set(action_id, new_bind);
                        self.keybindings.save();
                        self.ui.rebinding_action = None;
                        break;
                    }
                }
            }
        }
        if !any_text {
            let kb = &self.keybindings;
            let p = ctx.input(|i|(
                kb.pressed(ACT_GRAB, i), kb.pressed(ACT_ROTATE, i), kb.pressed(ACT_SCALE, i),
                i.key_pressed(egui::Key::X), i.key_pressed(egui::Key::Y),
                // Z AXIS CONSTRAINT — was `kb.pressed(ACT_SHADING_PIE, i)`.
                //
                // Owner 2026-07-30: "could not change direction on extrude." The destructuring below
                // names this slot `z` and every axis-constraint site reads it as the Z key, but it
                // was wired to the SHADING PIE action. So X and Y constrained correctly and Z never
                // did — pressing Z during a transform opened the shading pie instead. A positional
                // tuple with 24 elements is what let the name and the value drift apart silently.
                //
                // The shading pie is dispatched separately below via `shading_pie_pressed`, so it
                // does not lose its binding.
                i.key_pressed(egui::Key::Z),
                kb.pressed(ACT_DELETE, i),
                false, // Mode Pie: handled via mode_pie_pressed (raw strip)
                kb.pressed(ACT_FOCUS, i), i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Enter),
                kb.pressed(ACT_DUPLICATE, i),
                kb.pressed(ACT_VIEW_FRONT, i), kb.pressed(ACT_VIEW_RIGHT, i), kb.pressed(ACT_VIEW_TOP, i),
                kb.pressed(ACT_UNDO, i), kb.pressed(ACT_REDO, i),
                kb.pressed(ACT_SELECT_ALL, i) && !i.modifiers.command,
                kb.pressed(ACT_TOGGLE_SPACE, i),
                kb.pressed(ACT_CMD_BAR, i),
                kb.pressed(ACT_EXPORT, i),
                kb.pressed(ACT_SHORTCUT_EDITOR, i),
                kb.pressed(ACT_WORKPLANE_PLACE, i),
                kb.pressed(ACT_CONTENT_LIBRARY, i),
            ));
            let (g,r,s,x,y,z,del,_tab,f,esc,enter,shift_d,v1,v3,v7,ctrl_z,ctrl_shift_z,a_key,t_key,space,ctrl_e,end_key,home_key,content_lib)=p;
            // Evaluate all tool keybinds upfront to avoid borrow conflicts
            let q2 = ctx.input(|i|(
                kb.pressed(ACT_ADD_MENU, i),
                kb.pressed(ACT_ADD_SDF_SUBBODY, i),
                kb.pressed(ACT_SAVE_AS, i), kb.pressed(ACT_SAVE_SCENE, i),
                kb.pressed(ACT_OPEN_SCENE, i), kb.pressed(ACT_NEW_SCENE, i),
                kb.pressed(ACT_DROP_TOOL, i),
                kb.pressed(ACT_BOOL_SUB, i), kb.pressed(ACT_BOOL_ADD, i), kb.pressed(ACT_BOOL_INT, i),
                kb.pressed(ACT_SMART_MIRROR, i),
                kb.pressed(ACT_BEVEL_TOGGLE, i), kb.pressed(ACT_EXTRUDE_TOGGLE, i),
                kb.pressed(ACT_PIVOT_EDIT, i),
                kb.pressed(ACT_LAYER, i), kb.pressed(ACT_INSTANCE, i),
                kb.pressed(ACT_ORTHO_TOGGLE, i),
                kb.pressed(ACT_CUT_THROUGH, i), kb.pressed(ACT_CREATE_BEVEL, i),
                kb.pressed(ACT_CYCLE_OP, i), kb.pressed(ACT_CREATE_JOIN, i),
                kb.pressed(ACT_COMMAND_PALETTE, i), kb.pressed(ACT_LAYOUT_SETTINGS, i),
            ));
            let (add_menu, add_sdf_subbody, save_as, save, open, new_sc, drop_tool,
                 bool_sub, bool_add, bool_int, smart_mirror,
                 bevel_tog, extrude_tog, pivot_ed,
                 layer_grp, instance_key, ortho_tog,
                 cut_through, create_bevel, cycle_op, create_join,
                 cmd_palette, layout_settings) = q2;
            // SDF elongation (E key in SDF context)
            let elongate_key = ctx.input(|i| kb.pressed(ACT_ELONGATE, i));
            // SDF clip curve
            let clip_curve_key = ctx.input(|i| kb.pressed(ACT_CLIP_CURVE, i));
            // K in SDF context = Draw Polyline (the poly Knife owns K in Polygonal context).
            let sdf_polyline_key = ctx.input(|i| kb.pressed(ACT_SDF_DRAW_POLYLINE, i));
            // Quick light color popup (C, lights-only selection — context-split from
            // clip curve / poly circle select, which both need non-light selections).
            // Evaluated here with the other binds; ACTED ON below after the `kb`
            // borrow ends (mutating self while kb borrows self is a borrow error).
            let light_color_key = ctx.input(|i| kb.pressed(ACT_LIGHT_COLOR, i));
            // Pre-evaluate poly/NURBS contextual keybinds to avoid borrow conflicts
            let pkf = ctx.input(|i| PolyKeyFlags {
                extrude: kb.pressed(ACT_POLY_EXTRUDE, i),
                extrude_normals: kb.pressed(ACT_POLY_EXTRUDE_NORMALS, i),
                extrude_sweep: kb.pressed(ACT_POLY_EXTRUDE_SWEEP, i),
                inset: kb.pressed(ACT_POLY_INSET, i),
                loop_cut: kb.pressed(ACT_POLY_LOOP_CUT, i),
                vert_slide: kb.pressed(ACT_POLY_VERT_SLIDE, i),
                edge_slide: kb.pressed(ACT_POLY_EDGE_SLIDE, i),
                shrink_fatten: kb.pressed(ACT_POLY_SHRINK_FATTEN, i),
                merge: kb.pressed(ACT_POLY_MERGE, i),
                merge_menu: kb.pressed(ACT_POLY_MERGE_MENU, i),
                knife: kb.pressed(ACT_POLY_KNIFE, i),
                bevel: kb.pressed(ACT_POLY_BEVEL, i),
                unbevel: kb.pressed(ACT_POLY_UNBEVEL, i),
                face_hinge: kb.pressed(ACT_POLY_FACE_HINGE, i),
                delete_face: kb.pressed(ACT_POLY_DELETE_FACE, i),
                delete_only_face: kb.pressed(ACT_POLY_DELETE_ONLY_FACE, i),
                subdivide: kb.pressed(ACT_POLY_SUBDIVIDE, i),
                dice: kb.pressed(ACT_POLY_DICE, i),
                edge_loop: kb.pressed(ACT_POLY_EDGE_LOOP, i),
                edge_ring: kb.pressed(ACT_POLY_EDGE_RING, i),
                box_select: kb.pressed(ACT_POLY_BOX_SELECT, i),
                circle_select: kb.pressed(ACT_POLY_CIRCLE_SELECT, i),
                seam: kb.pressed(ACT_POLY_SEAM, i),
                circle_region: kb.pressed(ACT_POLY_CIRCLE_REGION, i),
                fill: kb.pressed(ACT_POLY_FILL, i),
                grid_fill: kb.pressed(ACT_POLY_GRID_FILL, i),
                spin: kb.pressed(ACT_POLY_SPIN, i),
                loop_relax: kb.pressed(ACT_POLY_LOOP_RELAX, i),
                loop_flatten: kb.pressed(ACT_POLY_LOOP_FLATTEN, i),
                loop_space: kb.pressed(ACT_POLY_LOOP_SPACE, i),
                dissolve: kb.pressed(ACT_POLY_DISSOLVE, i),
                limited_dissolve: kb.pressed(ACT_POLY_LIMITED_DISSOLVE, i),
                select_similar: kb.pressed(ACT_POLY_SELECT_SIMILAR, i),
                rip: kb.pressed(ACT_POLY_RIP, i),
                rip_fill: kb.pressed(ACT_POLY_RIP_FILL, i),
                proportional: kb.pressed(ACT_POLY_PROPORTIONAL, i),
                prop_falloff: kb.pressed(ACT_POLY_PROP_FALLOFF, i),
                recalc_normals: kb.pressed(ACT_POLY_RECALC_NORMALS, i),
                flip_normals: kb.pressed(ACT_POLY_FLIP_NORMALS, i),
                confirm_bevel: i.key_pressed(egui::Key::Enter),
                cancel_bevel: i.key_pressed(egui::Key::Escape),
                bridge: kb.pressed(ACT_POLY_BRIDGE, i),
                connect: kb.pressed(ACT_POLY_CONNECT, i),
                separate: kb.pressed(ACT_POLY_SEPARATE, i),
                pipe: kb.pressed(ACT_POLY_PIPE, i),
                knife_depth: kb.pressed(ACT_POLY_KNIFE_DEPTH, i),
                knife_bool_union: kb.pressed(ACT_POLY_KNIFE_BOOL_UNION, i),
                knife_bool_intersect: kb.pressed(ACT_POLY_KNIFE_BOOL_INTERSECT, i),
                bisect: kb.pressed(ACT_POLY_BISECT, i),
                bisect_clear: kb.pressed(ACT_POLY_BISECT_CLEAR, i),
                mark_sharp: kb.pressed(ACT_POLY_MARK_SHARP, i),
                clear_sharp: kb.pressed(ACT_POLY_CLEAR_SHARP, i),
                edge_weight: kb.pressed(ACT_POLY_EDGE_WEIGHT, i),
                shade_smooth: kb.pressed(ACT_POLY_SHADE_SMOOTH, i),
                shade_flat: kb.pressed(ACT_POLY_SHADE_FLAT, i),
                uv_unwrap: kb.pressed(ACT_POLY_UV_UNWRAP, i),
                uv_layout: kb.pressed(ACT_VIEW_UV_LAYOUT, i),
            });
            // Polyline profile-point editor contextual keys (gated on curve_edit_prim_index below).
            let (curve_select_all, curve_dissolve, curve_subdivide, curve_bevel_key) = ctx.input(|i| (
                kb.pressed(ACT_CURVE_SELECT_ALL, i),
                kb.pressed(ACT_CURVE_DISSOLVE, i),
                kb.pressed(ACT_CURVE_SUBDIVIDE, i),
                kb.pressed(ACT_CURVE_BEVEL, i),
            ));
            let nkf = ctx.input(|i| NurbsKeyFlags {
                knot_u: kb.pressed(ACT_NURBS_KNOT_U, i),
                knot_v: kb.pressed(ACT_NURBS_KNOT_V, i),
                weight_up: kb.pressed(ACT_NURBS_WEIGHT_UP, i),
                weight_down: kb.pressed(ACT_NURBS_WEIGHT_DOWN, i),
                grab_cv: kb.pressed(ACT_NURBS_GRAB_CV, i),
            });
            // On a keyboard-only frame `latest_pos()` is None; falling back to (0,0) put pie menus
            // (Alt+X symmetry, etc.) in the top-left corner. Retain the last-known pointer instead.
            let mpos=ctx.input(|i|i.pointer.latest_pos()).unwrap_or(self.viewport.viewport_mouse);
            self.viewport.viewport_mouse = mpos;
            let alt_a = ctx.input(|i| kb.pressed(ACT_DESELECT_ALL, i));
            // Pre-evaluate general action keybinds (migrated from hardcoded key checks)
            let (hide_key, unhide_key, q_menu_key, wire_key,
                 poly_hide_key, poly_linked_key, poly_invert_key,
                 poly_grow_key, poly_shrink_key, toggle_symmetry,
                 sym_axis_x, sym_axis_y, sym_axis_z,
                 sym_flip, sym_symmetrize, sym_delete_half, sym_radial_cycle,
                 mat_assign) = ctx.input(|i| (
                kb.pressed(ACT_HIDE, i),
                kb.pressed(ACT_UNHIDE_ALL, i),
                kb.pressed(ACT_Q_MENU, i),
                kb.pressed(ACT_WIRE_OVERLAY, i),
                kb.pressed(ACT_POLY_HIDE, i),
                kb.pressed(ACT_POLY_LINKED, i),
                kb.pressed(ACT_POLY_INVERT_SEL, i),
                kb.pressed(ACT_POLY_GROW_SEL, i),
                kb.pressed(ACT_POLY_SHRINK_SEL, i),
                kb.pressed(ACT_TOGGLE_SYMMETRY, i),
                kb.pressed(ACT_SYM_AXIS_X, i),
                kb.pressed(ACT_SYM_AXIS_Y, i),
                kb.pressed(ACT_SYM_AXIS_Z, i),
                kb.pressed(ACT_SYM_FLIP, i),
                kb.pressed(ACT_SYM_SYMMETRIZE, i),
                kb.pressed(ACT_SYM_DELETE_HALF, i),
                kb.pressed(ACT_SYM_RADIAL_CYCLE, i),
                kb.pressed(ACT_MAT_ASSIGN, i),
            ));
            // Quick light color popup (C, lights-only selection): dispatched HERE, after
            // every kb-batch closure above, so mutating self doesn't fight the kb borrow.
            // Both selection forms (`selected_light_ids` / `selected_transform_nodes`): a light
            // picked in the outliner is a `SceneItemId::Node`, which made `has_lights()` false
            // and `has_nodes()` true — C did nothing on exactly the row the user had clicked.
            if light_color_key
                && matches!(self.tool.current, ActiveTool::Idle)
                && !self.sel.usel.has_prims()
                && self.selected_transform_nodes().is_empty()
                && !self.selected_light_ids().is_empty()
            {
                if self.ui.light_color_popup.is_some() {
                    self.ui.light_color_popup = None;
                    self.commit_episode();
                } else {
                    // Bind the id first — the selection iterator's borrow must end
                    // before begin_episode takes &mut self.
                    let first_light = self.selected_light_ids().first().copied();
                    if let Some(lid) = first_light {
                        let anchor = ctx
                            .input(|i| i.pointer.hover_pos())
                            .unwrap_or(egui::pos2(240.0, 240.0));
                        self.begin_episode("Light color popup");
                        self.ui.light_color_popup = Some((lid, anchor));
                    }
                }
            }
            // Alt+X opens the Symmetrize pie for the active poly body — evaluated BEFORE the tool-modal
            // if/else chain below so it fires even mid-tool (a live bevel/inset drag routes into its own
            // modal branch and would otherwise swallow Alt+X). Cancel the active op first so a stuck
            // drag can't trap it, then open + return (the pie owns input via the modal guard up top).
            // Context-split: SDF Alt+X is NOT poly, so it falls through to Smart Mirror further down.
            if smart_mirror && !self.ui.sym_pie.open && self.active_poly_nid().is_some() {
                self.cancel_operation();
                self.open_sym_pie(mpos);
                return;
            }
            // Ctrl+Shift+A takes priority over Shift+A (both contain A+Shift).
            if add_sdf_subbody && !self.tool.current.is_transform() && !self.tool.current.is_create() {
                // SDF-only menu: new prim is created inside a sub-body of the selection.
                self.ui.show_add_popup = true;
                self.ui.add_popup_sdf_subbody = true;
                self.ui.add_popup_pos = mpos;
                self.ui.add_search.clear();
                self.ui.add_search_focus = true;
                self.ui.add_sec_user_open = 0; // only primary family expanded
            } else if add_menu && !self.tool.current.is_transform() && !self.tool.current.is_create() {
                // Blender-style: Shift+A inside the Shader Node Editor adds a *shader node*,
                // not a primitive. Primitive Add Menu only when the node editor is closed.
                if self.ui.show_node_editor {
                    self.ui.node_drag_state.add_node_menu =
                        Some(crate::shader_nodes::AddNodeMenu {
                            screen_pos: [mpos.x, mpos.y],
                            // place_pos refined when the editor draws (canvas origin known).
                            place_pos: [150.0, 150.0],
                            search: String::new(),
                        });
                    // Don't also open the primitive creation popup.
                    self.ui.show_add_popup = false;
                    self.ui.add_popup_sdf_subbody = false;
                } else {
                    self.ui.show_add_popup = true;
                    self.ui.add_popup_sdf_subbody = false;
                    self.ui.add_popup_pos = mpos;
                    self.ui.add_search.clear();
                    self.ui.add_search_focus = true;
                    // Workflow: only the context-primary group is open; others start collapsed.
                    self.ui.add_sec_user_open = 0;
                }
            }
            if end_key { self.ui.show_shortcut_editor = !self.ui.show_shortcut_editor; }
            if cmd_palette { self.ui.command_palette.open = !self.ui.command_palette.open; self.ui.command_palette.search.clear(); self.ui.command_palette.selected_idx = 0; }
            if layout_settings { self.ui.show_layout_settings = !self.ui.show_layout_settings; }
            // Workspace switching: Ctrl+1/2/3/4
            let ws_key = ctx.input(|i| {
                if !i.modifiers.command { return None; }
                if i.key_pressed(egui::Key::Num1) { Some(0usize) }
                else if i.key_pressed(egui::Key::Num2) { Some(1) }
                else if i.key_pressed(egui::Key::Num3) { Some(2) }
                else if i.key_pressed(egui::Key::Num4) { Some(3) }
                else { None }
            });
            if let Some(idx) = ws_key {
                if idx < self.workspace_mgr.workspaces.len() {
                    self.workspace_mgr.active_idx = idx;
                    self.workspace_mgr.save();
                    let name = self.workspace_mgr.active().name.clone();
                    self.toast(&format!("Workspace: {}", name));
                }
            }
            // Bare 1/2/3 keys for poly sub-mode switching (only in edit mode, no Ctrl — Ctrl+N = workspace)
            if self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode != PolyWorkMode::Object
                && !any_text
            {
                let poly_mode_key = ctx.input(|i| {
                    if i.modifiers.command { return None; }
                    if i.key_pressed(egui::Key::Num1) { Some(PolyWorkMode::Vertex) }
                    else if i.key_pressed(egui::Key::Num2) { Some(PolyWorkMode::Edge) }
                    else if i.key_pressed(egui::Key::Num3) { Some(PolyWorkMode::Face) }
                    else { None }
                });
                if let Some(pm) = poly_mode_key {
                    // Mode switch accepts a live loop cut (same rule as Tab).
                    let _ = self.poly_loop_cut_accept_on_mode_switch();
                    self.poly_switch_work_mode(pm);
                }
                // Ctrl+I in poly EDIT (vert/edge/face): invert only — instance is handled below
                // exclusively when NOT in poly edit (shared chord with ACT_INSTANCE).
                // Ctrl+Plus / Ctrl+Minus = grow/shrink poly selection (via keybind)
                if self.poly_work_mode != PolyWorkMode::Object {
                    if poly_grow_key { self.poly_grow_selection(); }
                    if poly_shrink_key { self.poly_shrink_selection(); }
                }
                // H / Alt+H = hide/unhide poly elements (via keybind)
                if self.poly_work_mode != PolyWorkMode::Object && poly_hide_key {
                    self.poly_hide_selected();
                }
                if self.poly_work_mode != PolyWorkMode::Object && unhide_key {
                    self.poly_unhide_all();
                }
                // Shift+I = toggle vertex-index DEBUG overlay (cross-reference vertex positions vs Blender)
                if self.poly_work_mode != PolyWorkMode::Object
                    && ctx.input(|i| i.modifiers.shift && !i.modifiers.command && !i.modifiers.alt && i.key_pressed(egui::Key::I)) {
                    self.poly_vertex_debug = !self.poly_vertex_debug;
                    let on = self.poly_vertex_debug;
                    self.toast(if on { "Vertex debug ON (Shift+I)" } else { "Vertex debug OFF" });
                }
                // (Shift+O is Overlay Pie globally — see ACT_OVERLAY_PIE below.)
                // L = select linked (via keybind)
                if self.poly_work_mode != PolyWorkMode::Object && poly_linked_key {
                    let shift = ctx.input(|i| i.modifiers.shift);
                    if ctx.input(|i| i.modifiers.command) {
                        self.poly_select_linked_from_sel();
                    } else {
                        self.poly_select_linked_hover(shift);
                    }
                }
            }
            // H / Alt+H in Object mode = hide/unhide selected objects (via keybind)
            // (skip when poly sub-mode handler already covers it)
            let poly_sub_mode = self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object;
            if unhide_key && self.tool.work_mode == WorkMode::Object && !poly_sub_mode {
                // Alt+H: Unhide all — skip empty undo when everything is already visible.
                let any_hidden_prim = self.scene.primitives.iter().any(|p| !p.visible);
                let nids: Vec<_> = self.scene.scene_graph.children(self.scene.scene_graph.root()).to_vec();
                let any_hidden_node = nids.iter().any(|&nid| {
                    self.scene.scene_graph.get(nid).map(|n| !n.visible).unwrap_or(false)
                });
                if any_hidden_prim || any_hidden_node {
                    self.transaction("Unhide All", |s| {
                        for i in 0..s.scene.primitives.len() { s.write_prim(i, |p| p.visible = true); }
                        let nids: Vec<_> = s.scene.scene_graph.children(s.scene.scene_graph.root()).to_vec();
                        for nid in nids {
                            if let Some(n) = s.scene.scene_graph.get_mut(nid) { n.visible = true; }
                            s.mark_node_body_vol_dirty(nid);
                        }
                        s.mark_visibility_dirty();
                        s.outliner.outliner_dirty = true;
                    });
                    self.toast("Unhide All");
                }
            }
            else if hide_key && self.tool.work_mode == WorkMode::Object && !poly_sub_mode {
                // H: Hide selected — only open undo when something is still visible.
                let indices: Vec<usize> = self.selected_prim_indices_unified();
                let active_node = self.sel.usel.active_node();
                let any_visible_prim = indices.iter().any(|&idx| {
                    idx < self.scene.primitives.len() && self.scene.primitives[idx].visible
                });
                let any_visible_node = active_node
                    .and_then(|nid| self.scene.scene_graph.get(nid))
                    .map(|n| n.visible)
                    .unwrap_or(false);
                if any_visible_prim || any_visible_node {
                    let mut count = 0u32;
                    self.transaction("Hide Selected", |s| {
                        for idx in &indices {
                            if *idx < s.scene.primitives.len() && s.scene.primitives[*idx].visible {
                                s.write_prim(*idx, |p| p.visible = false);
                                count += 1;
                            }
                        }
                        if let Some(nid) = active_node {
                            if let Some(n) = s.scene.scene_graph.get_mut(nid) {
                                if n.visible {
                                    n.visible = false;
                                    count += 1;
                                }
                            }
                            s.mark_node_body_vol_dirty(nid);
                        }
                        s.mark_visibility_dirty();
                        s.outliner.outliner_dirty = true;
                    });
                    if count > 0 {
                        // Clear prim + node selection for hidden items; light/layer/object preserved.
                        self.remove_from_selection(|id| matches!(id, SceneItemId::Prim(_) | SceneItemId::Node(_)));
                        self.toast(&format!("Hidden {}", count));
                    }
                }
            }
            // Q key = context-sensitive operations menu (via keybind)
            if q_menu_key && !any_text {
                self.ui.q_menu_open = !self.ui.q_menu_open;
                self.ui.q_menu_pos = mpos;
            }
            if home_key {
                let rect = self.viewport.viewport_rect;
                self.place_workplane_on_surface(mpos, rect);
            }
            if ctrl_z {
                // Blender parity: while the knife is placing points, Ctrl+Z removes the LAST placed point
                // (tool-local undo), NOT the scene. Without this it fell through to self.undo() and undid the
                // object itself (the reported "Ctrl+Z undoes the cube, not the point"). Empty knife → scene undo.
                if self.poly_tool == PolyTool::Knife && !self.knife_points.is_empty() {
                    self.knife_points.pop();
                    self.knife_snap_pos = None; // recomputed by poly_knife_hover next frame
                    self.mark_dirty(DirtyFlags::RENDER);
                } else {
                    self.undo();
                }
            }
            if ctrl_shift_z{self.redo();}
            if z && !ctx.input(|i| i.modifiers.any()) && self.tool.current.is_bevel() {
                let prim_idx = self.tool.current.as_bevel().unwrap().prim_idx;
                // Skip empty undo when corner bevels are already at defaults.
                let _ = self.reset_prim_corner_bevel_undoable(prim_idx);
            }
            if ctrl_e{self.ui.show_export_window=!self.ui.show_export_window;}
            // Ctrl+Space: expand / collapse the library pane (ui2). Zone-hidden → reopen
            // expanded. Expanded ⇄ collapsed body (header stays). Legacy bottom panel
            // follows open-and-not-collapsed so non-ui2 layout still toggles visibility.
            if content_lib {
                if !self.ui2.library.open {
                    self.ui2.library.open = true;
                    self.ui2.library.collapsed = false;
                } else {
                    self.ui2.library.collapsed = !self.ui2.library.collapsed;
                }
                self.library.open = self.ui2.library.open && !self.ui2.library.collapsed;
            }
            // File shortcuts
            if save_as { self.save_scene_as(); }
            else if save { self.save_scene(); }
            if open {
                if self.project.scene_modified {
                    self.ui.pending_action_after_save = Some(PendingAction::OpenFile);
                    self.ui.show_unsaved_dialog = true;
                } else {
                    self.open_scene();
                }
            }
            if new_sc {
                if self.project.scene_modified {
                    self.ui.pending_action_after_save = Some(PendingAction::NewScene);
                    self.ui.show_unsaved_dialog = true;
                } else {
                    self.new_scene();
                }
            }
            // PIPE live adjust owns Esc/Enter first (Tool Exit Convention: Escape = cancel,
            // Enter = confirm). Handled before drop_tool so Esc removes the pipe rather than
            // leaving an orphan modifier behind with the tool merely disarmed.
            // Latch BEFORE handling: cancel/confirm clears the state, so re-reading it below
            // would let drop_tool() fire on the same Esc that the pipe already consumed.
            let pipe_took_key = self.pipe_adjust_active();
            if pipe_took_key {
                if esc {
                    self.pipe_adjust_cancel();
                } else if enter {
                    self.pipe_adjust_confirm();
                }
            }
            // ── Universal Escape / Space / Drop-Tool for active modals ──
            if esc && !pipe_took_key {
                self.drop_tool();
            }
            // Space / Q drop while linear-array free-grab is live (before other handlers).
            if (space || drop_tool)
                && self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |t| t.shift_duplicate)
            {
                self.cancel_modal_transform();
            }
            // ── Modal transform (non-Esc keys) ──
            if self.tool.current.is_transform() {
                let is_linear = self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |t| t.shift_duplicate && t.free_mode);
                let shift_held = ctx.input(|i| i.modifiers.shift);
                // Free linear array: do NOT hard-bind X/Y/Z (workplane free + Shift lock).
                // Explicit G/R/S grab still uses X/Y/Z axis keys.
                if !is_linear {
                    if x { if let Some(ts) = self.tool.current.as_transform_mut() {
                        ts.axis = if shift_held { AxisConstraint::PlaneX } else { AxisConstraint::X };
                        ts.num_buf.clear();
                    }}
                    if y { if let Some(ts) = self.tool.current.as_transform_mut() {
                        ts.axis = if shift_held { AxisConstraint::PlaneY } else { AxisConstraint::Y };
                        ts.num_buf.clear();
                    }}
                    if z { if let Some(ts) = self.tool.current.as_transform_mut() {
                        ts.axis = if shift_held { AxisConstraint::PlaneZ } else { AxisConstraint::Z };
                        ts.num_buf.clear();
                    }}
                }
                self.capture_transform_num_input(ctx);
                // M → Array modifier; C → toggle circle array (center = original prim).
                let m_key = ctx.input(|i| {
                    i.key_pressed(egui::Key::M)
                        && !i.modifiers.command
                        && !i.modifiers.shift
                        && !i.modifiers.alt
                });
                let c_key = ctx.input(|i| {
                    i.key_pressed(egui::Key::C)
                        && !i.modifiers.command
                        && !i.modifiers.shift
                        && !i.modifiers.alt
                });
                let is_array = self
                    .tool
                    .current
                    .as_transform()
                    .map_or(false, |t| t.shift_duplicate);
                if m_key && is_array {
                    self.transform_array_make_modifier();
                } else if c_key && is_array {
                    self.transform_array_toggle_circle();
                }
                let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                // Modal G/R/S over a curve profile point: FREEZE the cursor-follow so the whole prim
                // stops tracking the mouse while the user reaches a point (else it slides toward the
                // click). The LMB press -> confirm-modal + point-grab is handled in input_viewport.rs;
                // here we skip the follow AND do not confirm on that click (input_viewport already does,
                // which would otherwise commit at the nudged position and eat the grab click).
                let rect = self.viewport.viewport_rect;
                let over_curve_pt = self.curve_edit_prim_index()
                    .zip(ctx.input(|i| i.pointer.hover_pos()))
                    .map_or(false, |(cidx, hp)| {
                        self.curve_pt_pick(cidx, hp, rect).is_some()
                            || self.curve_pt_edge_pick(cidx, hp, rect).is_some()
                    });
                if enter || (lmb_clicked && !over_curve_pt) { self.confirm_modal_transform(); }
                else if !esc && !over_curve_pt && self.tool.current.is_transform() {
                    let c=ctx.input(|i|i.modifiers.command); let sh=ctx.input(|i|i.modifiers.shift);
                    self.update_modal_transform(mpos,c,sh);
                }
            }
            // ── TOOL SWITCH CONFIRMS THE LIVE POLY MODAL (CLAUDE.md Tool Exit Convention) ──
            //
            // Owner 2026-07-30: "even if I did not accept the cut, changing for another tool should
            // accept it automatically … ALL TOOL MUST DO THIS BEHAVIOR NOT JUST G."
            //
            // Every poly modal (loop cut, bevel, hinge, extrude, edge extrude, inset, shrink/fatten)
            // used to honour ONLY Enter/Esc, so a tool key pressed mid-modal was swallowed: the work
            // was neither confirmed nor cancelled and the user was stuck. This ONE branch, ahead of
            // the per-modal chain, ends whichever modal is live and then starts the requested tool —
            // so Ctrl+R → G confirms the cut (leaving the new loop selected) and grabs it.
            //
            // EXCEPTIONS — modal-owned mode keys (must NOT confirm + arm Grab):
            //   • Extrude Along Normals: G = Grouped, I = Individual (owner 2026-08-06)
            //   • Inset: G = Group/Region, I = Individual
            // Bare G on those modals falls through to the per-modal handlers below.
            // Loop-cut owns R (rotate cut direction) and S (paired toggle) — those must NOT
            // confirm+arm Gizmo. G still confirms the cut then arms Translate (Ctrl+R → G).
            // Owner 2026-08-06: R = re-aim loop strip 90° on the face; K = knife through.
            else if (g || r || s)
                && self.tool.current.is_poly_modal()
                && !(g
                    && (self.tool.current.is_poly_extrude()
                        || self.tool.current.is_poly_inset()))
                && !(self.tool.current.is_poly_loop_cut_slide() && (r || s))
                // Dice owns R = rotate grid 15° — must not confirm + arm Rotate gizmo.
                && !(self.tool.current.is_poly_dice() && r)
            {
                // Confirm the modal, then ARM THE GIZMO — do NOT start a Blender-style modal drag.
                //
                // Owner 2026-07-30: "the G after the loop cut did not bring the proper drag tool
                // (it brings the Blender version, this is not the one from AZRA)." Correct: AZRA's
                // G/R/S set `gizmo_mode` + `gizmo_armed` (:1233-1236) and the user then drags a
                // VISIBLE gizmo handle; `poly_start_transform` is the modal path the gizmo delegates
                // INTO on handle-grab (gizmo.rs:1722), not the thing a bare G should launch.
                // My first version called the modal directly, which is why it felt like Blender.
                //
                // S is safe here for non-loop-cut modals: this branch runs only AFTER the modal is
                // confirmed and gone. Loop-cut S is handled in the loop-cut branch below.
                self.confirm_poly_modal();
                self.tool.gizmo_mode = if g {
                    GizmoMode::Translate
                } else if r {
                    GizmoMode::Rotate
                } else {
                    GizmoMode::Scale
                };
                self.tool.gizmo_armed = true;
            }
            // ── Poly Transform (G/R/S on sub-elements) ──
            else if self.tool.current.is_poly_transform() {
                let shift_held = ctx.input(|i| i.modifiers.shift);
                if x { self.poly_transform_set_axis(if shift_held { AxisConstraint::PlaneX } else { AxisConstraint::X }); }
                if y { self.poly_transform_set_axis(if shift_held { AxisConstraint::PlaneY } else { AxisConstraint::Y }); }
                if z { self.poly_transform_set_axis(if shift_held { AxisConstraint::PlaneZ } else { AxisConstraint::Z }); }
                // Numeric input
                for ch in ['0','1','2','3','4','5','6','7','8','9','.','-'] {
                    let key = match ch {
                        '0' => egui::Key::Num0, '1' => egui::Key::Num1, '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3, '4' => egui::Key::Num4, '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6, '7' => egui::Key::Num7, '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9, '.' => egui::Key::Period, '-' => egui::Key::Minus,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) {
                        self.poly_transform_num_input(ch);
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    self.poly_transform_num_input('\x08');
                }
                let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                if enter || lmb_clicked { self.poly_confirm_transform(); }
                else if !esc {
                    let (shift, ctrl) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                    self.poly_update_transform(mpos, shift, ctrl);
                }
            }
            // ── NURBS Fillet (Ctrl+B) modal ──
            else if self.tool.current.is_nurbs_fillet() {
                // V — cycle Const → Linear → Law → Const
                if ctx.input(|i| i.key_pressed(egui::Key::V) && !i.modifiers.command) {
                    self.nurbs_fillet_toggle_linear();
                }
                // N — Law station count 2/3/4 (or enter Law)
                if ctx.input(|i| i.key_pressed(egui::Key::N) && !i.modifiers.command) {
                    self.nurbs_fillet_cycle_law_stations();
                }
                // Numeric peak-radius entry (poly G/R/S parity)
                for ch in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.'] {
                    let key = match ch {
                        '0' => egui::Key::Num0,
                        '1' => egui::Key::Num1,
                        '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3,
                        '4' => egui::Key::Num4,
                        '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6,
                        '7' => egui::Key::Num7,
                        '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9,
                        '.' => egui::Key::Period,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) {
                        self.nurbs_fillet_num_input(ch);
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    self.nurbs_fillet_num_input('\x08');
                }
                let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                if enter || lmb_clicked {
                    self.nurbs_fillet_confirm();
                } else if esc {
                    self.nurbs_fillet_cancel();
                } else {
                    self.nurbs_fillet_update(mpos);
                }
            }
            // ── NURBS Transform (G/R/S) — axis / numeric / confirm parity with poly ──
            else if self.tool.current.is_nurbs_cv_move() {
                let shift_held = ctx.input(|i| i.modifiers.shift);
                if x { self.nurbs_transform_set_axis(if shift_held { AxisConstraint::PlaneX } else { AxisConstraint::X }); }
                if y { self.nurbs_transform_set_axis(if shift_held { AxisConstraint::PlaneY } else { AxisConstraint::Y }); }
                if z { self.nurbs_transform_set_axis(if shift_held { AxisConstraint::PlaneZ } else { AxisConstraint::Z }); }
                for ch in ['0','1','2','3','4','5','6','7','8','9','.','-'] {
                    let key = match ch {
                        '0' => egui::Key::Num0, '1' => egui::Key::Num1, '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3, '4' => egui::Key::Num4, '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6, '7' => egui::Key::Num7, '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9, '.' => egui::Key::Period, '-' => egui::Key::Minus,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) {
                        self.nurbs_transform_num_input(ch);
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    self.nurbs_transform_num_input('\x08');
                }
                let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                if enter || lmb_clicked { self.nurbs_confirm_cv_drag(); }
                else if esc { self.nurbs_cancel_cv_drag(); }
                else {
                    let (shift, ctrl) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                    self.nurbs_update_cv_drag(mpos, shift, ctrl);
                }
            }
            // ── Curve Point Transform (G/R/S on CurveExtrude profile points) ──
            else if self.tool.current.is_curve_point_transform()
                    && !self.tool.current.as_curve_point_transform().map_or(false, |d| d.direct) {
                let shift_held = ctx.input(|i| i.modifiers.shift);
                // In-plane axis lock: X → UV.x, Y → UV.y, Z → free (reset). (No PlaneN variants — the
                // whole transform is already confined to the profile plane.)
                if x { self.curve_pt_transform_set_axis(if shift_held { AxisConstraint::None } else { AxisConstraint::X }); }
                if y { self.curve_pt_transform_set_axis(if shift_held { AxisConstraint::None } else { AxisConstraint::Y }); }
                if z { self.curve_pt_transform_set_axis(AxisConstraint::None); }
                for ch in ['0','1','2','3','4','5','6','7','8','9','.','-'] {
                    let key = match ch {
                        '0' => egui::Key::Num0, '1' => egui::Key::Num1, '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3, '4' => egui::Key::Num4, '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6, '7' => egui::Key::Num7, '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9, '.' => egui::Key::Period, '-' => egui::Key::Minus,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) { self.curve_pt_transform_num_input(ch); }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) { self.curve_pt_transform_num_input('\x08'); }
                let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                let vp_rect = self.viewport.viewport_rect;
                let (ctrl_held, shift_snap) = ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
                if enter || lmb_clicked { self.curve_pt_confirm_transform(); }
                else if !esc { self.curve_pt_update_transform(mpos, vp_rect, ctrl_held, shift_snap); }
            }
            // ── Creation tool keys (non-Esc) ──
            else if self.tool.current.is_create() {
                // Cut through during footprint
                if cut_through
                    && self.tool.current.as_create().map(|cs| cs.phase == CreatePhase::Footprint).unwrap_or(false)
                {
                    crate::ops::create::session::commit(self, true);
                }
                // Toggle bevel Round/Chamfer
                else if create_bevel {
                    if let Some(cs) = self.tool.current.as_create_mut() {
                        cs.corner_chamfer = !cs.corner_chamfer;
                    }
                    let is_chamfer = self.tool.current.as_create().map(|cs| cs.corner_chamfer).unwrap_or(false);
                    self.update_create_bevel();
                    self.toast(if is_chamfer { "Chamfer corners" } else { "Round corners" });
                }
                // Cycle op / Join
                else if cycle_op {
                    crate::ops::create::session::cycle_op(self);
                }
                else if create_join {
                    self.push_cmd(AppCommand::SetCreateOp(CreateOp::Union));
                }
                // Numeric depth input (during Extrude phase)
                if self.tool.current.as_create().map(|cs| cs.phase == CreatePhase::Extrude).unwrap_or(false) {
                    let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
                    for ev in &events {
                        if let egui::Event::Text(t) = ev {
                            for ch in t.chars() {
                                if ch.is_ascii_digit() || ch == '.' || ch == '-' {
                                    if let Some(cs) = self.tool.current.as_create_mut() {
                                        cs.num_buf.push(ch);
                                    }
                                }
                            }
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                        if let Some(cs) = self.tool.current.as_create_mut() { cs.num_buf.pop(); }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let parsed = self.tool.current.as_create().and_then(|cs| cs.num_buf.parse::<f32>().ok());
                        if let Some(v) = parsed {
                            if let Some(cs) = self.tool.current.as_create_mut() {
                                cs.depth = -v.abs();
                                cs.num_buf.clear();
                                let prim_idx = cs.prim_idx;
                                let he_y = v.abs().max(0.001) * 0.5;
                                let hit_normal = cs.hit_normal;
                                let draw_center = if cs.anchor == cs.hit_point {
                                    cs.anchor
                                } else {
                                    cs.anchor + cs.tangent_u * (cs.local_u * 0.5) + cs.tangent_v * (cs.local_v * 0.5)
                                };
                                if prim_idx < self.scene.primitives.len() {
                                    let pos = draw_center + hit_normal * (cs.depth * 0.5);
                                    self.write_prim(prim_idx, |p| {
                                        p.position = pos;
                                        if let SdfShape::Box { ref mut half_extents, .. } = p.shape {
                                            half_extents.y = he_y;
                                        }
                                    });
                                }
                            }
                            self.rebake_no_undo();
                            crate::ops::create::session::commit(self, false);
                        } else {
                            if let Some(cs) = self.tool.current.as_create_mut() { cs.num_buf.clear(); }
                        }
                    }
                }
            }
            // ── Smart Mirror tool keys ──
            else if self.tool.current.is_smart_mirror() {
                if x && !ctx.input(|i| i.modifiers.alt) {
                    if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                        st.plane_normal = Vec3::X; st.base_normal = Vec3::X;
                        st.snapped_axis = 0; st.rotation_angle = 0.0;
                        let (tu, tv) = crate::mirror_tangent_frame(Vec3::X, st.surface_normal);
                        st.tangent_u = tu; st.tangent_v = tv;
                        self.toast("Mirror axis: X");
                    }
                }
                if y {
                    if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                        st.plane_normal = Vec3::Y; st.base_normal = Vec3::Y;
                        st.snapped_axis = 1; st.rotation_angle = 0.0;
                        let (tu, tv) = crate::mirror_tangent_frame(Vec3::Y, st.surface_normal);
                        st.tangent_u = tu; st.tangent_v = tv;
                        self.toast("Mirror axis: Y");
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Z) && !i.modifiers.command) {
                    if let Some(st) = self.tool.current.as_smart_mirror_mut() {
                        st.plane_normal = Vec3::Z; st.base_normal = Vec3::Z;
                        st.snapped_axis = 2; st.rotation_angle = 0.0;
                        let (tu, tv) = crate::mirror_tangent_frame(Vec3::Z, st.surface_normal);
                        st.tangent_u = tu; st.tangent_v = tv;
                        self.toast("Mirror axis: Z");
                    }
                }
                if enter { self.confirm_smart_mirror(); }
            }
            // ── Poly Bevel modal (mouse move = width, ALT+move = shape, SHIFT = consume,
            //    scroll = segments) ──
            else if self.tool.current.is_poly_bevel() {
                // Unbevel session: Ctrl rising edge = remove full bevel and exit tool.
                // (Latch avoids firing on the same Ctrl held from Ctrl+Alt+B activation.)
                let ctrl_now = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                let (is_unbevel, ctrl_was) = self
                    .tool
                    .current
                    .as_poly_bevel()
                    .map(|d| (d.is_unbevel, d.ctrl_was_down))
                    .unwrap_or((false, false));
                if is_unbevel && ctrl_now && !ctrl_was {
                    self.poly_unbevel_remove_full_and_exit();
                } else {
                    if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                        d.ctrl_was_down = ctrl_now;
                    }
                    // Clamp Overlap — ACT_BEVEL_TOGGLE_CLAMP (modal-only; no clash with clip-curve).
                    if ctx.input(|i| self.keybindings.pressed(ACT_BEVEL_TOGGLE_CLAMP, i)) {
                        self.poly_bevel_toggle_clamp();
                    }
                    // SHIFT tap = toggle Consume Geometry (owner 2026-08-09). Rising edge, same
                    // latch shape as the unbevel Ctrl tap above — SHIFT is a modifier, not an
                    // `egui::Key`, so it has no `ACTION_DEFS` chord to bind; the modal owns it.
                    // This is why the profile shape moved off SHIFT and onto ALT: a held SHIFT
                    // that also adjusted shape would flip consume on every shape tweak.
                    let shift_held = ctx.input(|i| i.modifiers.shift);
                    let shift_was = self
                        .tool
                        .current
                        .as_poly_bevel()
                        .map(|d| d.shift_was_down)
                        .unwrap_or(false);
                    if let Some(d) = self.tool.current.as_poly_bevel_mut() {
                        d.shift_was_down = shift_held;
                    }
                    if shift_held && !shift_was {
                        self.poly_bevel_toggle_consume();
                    }
                    // Update width (mouse-X) every frame; ALT gates the profile-shape (mouse-Y) adjust.
                    let alt_held = ctx.input(|i| i.modifiers.alt);
                    self.poly_update_bevel_drag(mpos, alt_held);
                    if enter {
                        self.poly_confirm_bevel();
                    }
                }
            }
            // ── Face hinge rotate modal ──
            else if self.tool.current.is_poly_face_hinge() {
                let (shift_held, alt_held) =
                    ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
                let primary = ctx.input(|i| i.pointer.primary_down());
                self.poly_update_face_hinge(mpos, shift_held, alt_held && primary);
                if enter {
                    self.poly_confirm_face_hinge();
                }
            }
            // ── Poly Dice session (Enter = confirm, R = rotate grid 15°) ──
            else if self.tool.current.is_poly_dice() || self.poly_tool == PolyTool::Dice {
                if r && !ctx.input(|i| i.modifiers.any()) {
                    self.poly_dice_rotate();
                }
                if enter {
                    self.poly_confirm_dice();
                }
            }
            // ── Poly Loop Cut session (Enter = confirm, C = clamp, S = paired support loops,
            //    R = rotate cut direction, K = knife through-all; hover/slide in viewport) ──
            //
            // This branch OWNS the modal if-else, so Tab pie (in Normal keyboard) would never
            // run — handle Tab/1–3 here: accept cut, then open the mode pie / switch mode.
            else if self.tool.current.is_poly_loop_cut_slide()
                || self.poly_tool == PolyTool::LoopCut
            {
                // Tab → accept cut, then open mode pie (owner 2026-08-06).
                if mode_pie_pressed && !self.ui.tab_pie.open {
                    let _ = self.poly_loop_cut_accept_on_mode_switch();
                    self.ui.z_pie.open = false;
                    self.ui.z_pie.open_time = None;
                    self.ui.view_pie.open = false;
                    self.ui.view_pie.open_time = None;
                    self.ui.f_pie.open = false;
                    self.ui.f_pie.open_time = None;
                    self.ui.pie_mode = crate::pie_menu::PieMode::Poly;
                    let n_items = crate::pie_menu::tab_pie_def_ex(
                        self.ui.pie_mode,
                        self.poly_pie_shows_group(),
                    )
                    .inner
                    .items
                    .len();
                    self.ui.tab_pie.open(mpos, n_items);
                } else if self.tool.current.is_poly_loop_cut_slide() {
                    if ctx.input(|i| self.keybindings.pressed(ACT_BEVEL_TOGGLE_CLAMP, i)) {
                        self.poly_loop_cut_toggle_clamp();
                    }
                    // S toggles paired support loops (no modifiers, only while the session owns the modal).
                    if ctx.input(|i| i.key_pressed(egui::Key::S) && !i.modifiers.any()) {
                        self.poly_loop_cut_toggle_paired();
                    }
                    // R = rotate cut direction on the seed face (vertical ↔ horizontal strip).
                    if r && !ctx.input(|i| i.modifiers.any()) {
                        self.poly_loop_cut_rotate_direction();
                    }
                    // K = one-shot through-cut along the preview (does NOT arm Knife tool).
                    if ctx.input(|i| i.key_pressed(egui::Key::K) && !i.modifiers.any()) {
                        self.poly_loop_cut_project_knife_through();
                    }
                    if enter {
                        self.poly_confirm_loop_cut_slide();
                    }
                    // 1/2/3 mode keys while session is live.
                    let poly_mode_key = ctx.input(|i| {
                        if i.modifiers.command {
                            return None;
                        }
                        if i.key_pressed(egui::Key::Num1) {
                            Some(PolyWorkMode::Vertex)
                        } else if i.key_pressed(egui::Key::Num2) {
                            Some(PolyWorkMode::Edge)
                        } else if i.key_pressed(egui::Key::Num3) {
                            Some(PolyWorkMode::Face)
                        } else {
                            None
                        }
                    });
                    if let Some(pm) = poly_mode_key {
                        let _ = self.poly_loop_cut_accept_on_mode_switch();
                        self.poly_switch_work_mode(pm);
                    }
                } else if r && !ctx.input(|i| i.modifiers.any()) {
                    // Armed (hovering) but not yet in slide session.
                    self.poly_loop_cut_rotate_direction();
                } else if mode_pie_pressed {
                    // Already accepted above when pie was closed; nothing else.
                }
            }
            // ── Poly Extrude modal (mouse move = offset) ──
            else if self.tool.current.is_poly_extrude() {
                // N — toggle Region Normal ⇄ Along Normals mid-drag (context-scoped bare key,
                // same pattern as inset G/I; the global bare-N "new SDF body" is gated on
                // tool.current.is_idle() so it cannot fire while this modal owns the drag).
                if ctx.input(|i| i.key_pressed(egui::Key::N) && !i.modifiers.any()) {
                    self.poly_extrude_toggle_along_normals();
                }
                // G / I — Along Normals Grouped ⇄ Individual (owner: "press G for grouped").
                // Topology differs, so this rebuilds from the pre-extrude snapshot (inset-style).
                // Only meaningful while Along Normals is active; otherwise toasts the N path.
                let (g, i_key) = ctx.input(|i| (
                    i.key_pressed(egui::Key::G) && !i.modifiers.any(),
                    i.key_pressed(egui::Key::I) && !i.modifiers.any(),
                ));
                if g {
                    self.poly_extrude_set_along_group(true);
                } else if i_key {
                    self.poly_extrude_set_along_group(false);
                }
                // D HELD — Punch-It edge pick: hover any edge to take its direction; the
                // last valid pick stays after release. N returns to Region Normal.
                if ctx.input(|i| i.key_down(egui::Key::D) && !i.modifiers.any()) {
                    let rect = self.viewport.viewport_rect;
                    self.poly_extrude_pick_edge_dir(mpos, rect);
                }
                // CONSOLE_AUDIT A2: do NOT call poly_update_extrude_drag here.
                // Viewport path (input_viewport) is the sole update — keyboard ran first
                // every frame and doubled delta+sync (~2×78ms input). Shift/world-axis
                // still applied in the viewport call (modifiers read there).
                if enter { self.poly_confirm_extrude_drag(); }
            }
            // ── Extrude Sweep (Shift+Alt+E): mouse path, scroll segments, type angle ──
            else if self.tool.current.is_poly_extrude_sweep() {
                let rect = self.viewport.viewport_rect;
                let kctrl = ctx.input(|i| i.modifiers.command);
                // Numeric angle entry (45, 90, …).
                for ch in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', '-'] {
                    let key = match ch {
                        '0' => egui::Key::Num0,
                        '1' => egui::Key::Num1,
                        '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3,
                        '4' => egui::Key::Num4,
                        '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6,
                        '7' => egui::Key::Num7,
                        '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9,
                        '.' => egui::Key::Period,
                        '-' => egui::Key::Minus,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) {
                        self.poly_extrude_sweep_num_input(ch);
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    self.poly_extrude_sweep_num_input('\x08');
                }
                self.poly_update_extrude_sweep(mpos, rect, kctrl);
                if enter {
                    self.poly_confirm_extrude_sweep();
                }
            }
            else if self.tool.current.is_poly_edge_extrude() {
                let rect = self.viewport.viewport_rect;
                let kctrl = ctx.input(|i| i.modifiers.command);
                self.poly_update_edge_extrude_drag(mpos, rect, kctrl);
                if enter { self.poly_confirm_edge_extrude_drag(); }
            }
            // ── Poly Inset modal (mouse move = amount) ──
            else if self.tool.current.is_poly_inset() {
                let rect = self.viewport.viewport_rect;
                let kctrl = ctx.input(|i| i.modifiers.command);
                // Blender-style modal mode keys, context-scoped to the live inset drag:
                //   G = Group/Region (the I default)   I = Individual
                // The two modes have different TOPOLOGY, so this rebuilds from the pre-inset snapshot
                // and re-applies the current amount — the drag continues uninterrupted.
                let (g, i_key) = ctx.input(|i| (
                    i.key_pressed(egui::Key::G) && !i.modifiers.any(),
                    i.key_pressed(egui::Key::I) && !i.modifiers.any(),
                ));
                if g { self.poly_inset_set_mode(false); }
                else if i_key { self.poly_inset_set_mode(true); }
                let kalt = ctx.input(|i| i.modifiers.alt);
                self.poly_update_inset_drag(mpos, rect, kctrl, kalt);
                if enter { self.poly_confirm_inset_drag(); }
            }
            // ── Poly Shrink/Fatten modal (mouse move = signed offset) ──
            else if self.tool.current.is_poly_shrink_fatten() {
                let kctrl = ctx.input(|i| i.modifiers.command);
                self.poly_update_shrink_fatten(mpos, kctrl);
                if enter { self.poly_confirm_shrink_fatten(); }
            }
            // ── Poly Edge Weight modal (`W`): free-cursor drag = strength, typed digits = exact %,
            //    Shift = 20 % stops, Ctrl = reset 0. Enter confirms here; the click-accept, the RMB
            //    cancel and the live Shift/Ctrl reads live in input_viewport.rs — bare modifiers make
            //    this handler early-return as "idle", so modifier-driven behaviour CANNOT be
            //    keyboard-only (the same fossil the extrude drag documents).
            else if self.tool.current.is_poly_edge_weight() {
                // Numeric percent entry (40 → 40 %), same key sweep as the Extrude Sweep angle.
                for ch in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.'] {
                    let key = match ch {
                        '0' => egui::Key::Num0,
                        '1' => egui::Key::Num1,
                        '2' => egui::Key::Num2,
                        '3' => egui::Key::Num3,
                        '4' => egui::Key::Num4,
                        '5' => egui::Key::Num5,
                        '6' => egui::Key::Num6,
                        '7' => egui::Key::Num7,
                        '8' => egui::Key::Num8,
                        '9' => egui::Key::Num9,
                        '.' => egui::Key::Period,
                        _ => continue,
                    };
                    if ctx.input(|i| i.key_pressed(key)) {
                        self.poly_edge_weight_num_input(ch);
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    self.poly_edge_weight_num_input('\x08');
                }
                let (ksh, kct) = ctx.input(|i| {
                    (i.modifiers.shift, i.modifiers.command || i.modifiers.ctrl)
                });
                self.poly_update_edge_weight_drag(mpos, ksh, kct);
                if enter { self.poly_confirm_edge_weight_drag(); }
            }
            // ── Offset Edge Loop modal (`I` in Edge mode): mouse move = ABSOLUTE offset distance,
            //    the side chosen by which probe landing the cursor heads toward. Enter confirms;
            //    Esc → drop_tool → cancel_operation; LMB/RMB in input_viewport.rs.
            //    SHIFT tap = both adjacent planes (P3). ALT (proportional) is still inert.
            else if self.tool.current.is_poly_offset_loop() {
                let shift_held = ctx.input(|i| i.modifiers.shift);
                let shift_was = self
                    .tool
                    .current
                    .as_poly_offset_loop()
                    .map(|d| d.shift_was_down)
                    .unwrap_or(false);
                if let Some(d) = self.tool.current.as_poly_offset_loop_mut() {
                    d.shift_was_down = shift_held;
                }
                if shift_held && !shift_was {
                    self.poly_toggle_offset_loop_both_sides();
                }
                let rect = self.viewport.viewport_rect;
                self.poly_update_offset_loop_drag(mpos, rect);
                if enter { self.poly_confirm_offset_loop(); }
            }
            // ── Exclusive Spine point-edit tool (modal) — no other shortcuts ──
            else if self.tool.current.is_spine_point_edit() {
                let b_down = ctx.input(|i| i.key_down(egui::Key::B) && !i.modifiers.any());
                let rect = self.viewport.viewport_rect;
                // Modal G/S/R: update from hover every keyboard frame; Enter confirms.
                // Shift = angle snap; Alt = workplane lock; Ctrl = surface snap (same as LMB drag).
                let (sh, al, ct) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt, i.modifiers.command));
                if let Some(drag) = self.spine_pt_drag.as_ref() {
                    if drag.modal {
                        self.spine_pt_update_drag(mpos, rect, sh, al, ct);
                        if enter {
                            self.spine_pt_confirm_drag();
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                    }
                }
                // B pie: hold B to open, release B to confirm (same as Z/Tab pies).
                if curve_bevel_key && !self.ui.spine_corner_pie.open && self.spine_pt_drag.is_none() {
                    self.ui.spine_corner_pie.open(mpos, 3);
                }
                if self.ui.spine_corner_pie.open {
                    let def = crate::pie_menu::spine_corner_pie_def();
                    self.ui.spine_corner_pie.update_hover(mpos, &def, 1.0);
                    self.ui.spine_corner_pie.tick(ctx.input(|i| i.stable_dt));
                    if !b_down {
                        use crate::pie_menu::PieResult;
                        match self.ui.spine_corner_pie.release() {
                            PieResult::Inner(idx) => self.spine_apply_join_type(idx),
                            PieResult::QuickTap => self.spine_cycle_join_type(),
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                } else if self.spine_pt_drag.as_ref().map(|d| d.modal).unwrap_or(false) {
                    // Modal transform owns keys until confirmed (except Esc via drop_tool).
                } else if (space || enter) && self.spine_pt_drag.is_none() {
                    self.spine_finish_edit();
                } else if (g || r || s) && self.spine_pt_drag.is_none() {
                    if g {
                        self.spine_pt_grab_key(mpos, rect);
                    } else if r {
                        self.spine_pt_rotate_key(mpos, rect);
                    } else {
                        self.spine_pt_scale_key(mpos, rect);
                    }
                } else if del && self.spine_pt_drag.is_none()
                    && !self.spine_pt_sel.is_empty()
                    && self.spine_edit_prim_index().is_some()
                {
                    self.spine_pt_delete();
                }
                // All other shortcuts (Z/Tab/F/G-object/bool/…) are intentionally ignored.
            }
            // ── Normal keyboard ──
            else {
                // Poly editing keys — dispatch via keybind system
                if self.modeling_context == ModelingContext::Polygonal {
                    // Tab is handled by the Tab pie below (poly_switch_work_mode path)
                    self.poly_handle_keys(&pkf);
                }
                // Knife modal keys — POLY ONLY (owner 2026-08-08: the SDF knife on K was
                // reverted; K in SDF context now creates a polyline primitive instead, so the
                // knife modal must never arm outside Polygonal or the two tools fight over the
                // same chord).
                if self.poly_tool == PolyTool::Knife
                    && self.modeling_context == ModelingContext::Polygonal
                {
                    // Enter: commit pending hover point then cut (Blender rubber-band parity).
                    if enter {
                        self.poly_knife_confirm(true);
                    }
                    // S = through-slice of the current polyline / shape.
                    // Shift+S = BOOLEAN (kerf) slice. Ctrl+S stays Save.
                    if ctx.input(|i| {
                        i.key_pressed(egui::Key::S)
                            && !i.modifiers.ctrl
                            && !i.modifiers.command
                    }) {
                        if ctx.input(|i| i.modifiers.shift) {
                            self.poly_knife_slice_kerf_now();
                        } else {
                            self.poly_knife_slice_through_now();
                        }
                    }
                    // R = rectangular cut shape. Mid-drag: R rotates — do not toggle arm off.
                    if ctx.input(|i| i.key_pressed(egui::Key::R) && !i.modifiers.any()) {
                        let rotating =
                            self.knife_shape_kind == 1 && self.knife_shape_anchor.is_some();
                        if !rotating {
                            self.poly_knife_shape_arm(1);
                        }
                    }
                    // C = circular cut shape.
                    if ctx.input(|i| i.key_pressed(egui::Key::C) && !i.modifiers.any()) {
                        self.poly_knife_shape_arm(2);
                    }
                    // L = free polyline.
                    if ctx.input(|i| i.key_pressed(egui::Key::L) && !i.modifiers.any()) {
                        self.poly_knife_shape_to_free_line();
                    }
                }
                // NURBS editing keys — dispatch via keybind system
                if self.modeling_context == ModelingContext::Nurbs {
                    if mode_pie_pressed && !self.ui.tab_pie.open {
                        let _ = self.nurbs_handle_key(egui::Key::Tab, false, false);
                    }
                    self.nurbs_handle_keys(&nkf);
                }
                // Active tool keys: drop, bool ops
                if self.tool.mode.is_create_armed() || self.tool.current.is_create() {
                    if drop_tool {
                        if self.tool.current.is_create() { crate::ops::create::session::cancel(self); }
                        crate::ops::create::session::deactivate(self);
                    }
                    else if bool_sub { self.push_cmd(AppCommand::SetCreateOp(CreateOp::Subtract)); }
                    else if bool_add { self.push_cmd(AppCommand::SetCreateOp(CreateOp::Union)); }
                    else if bool_int { self.push_cmd(AppCommand::SetCreateOp(CreateOp::Intersect)); }
                }
                // Smart mirror tool (SDF only — the poly Symmetrize pie is opened earlier, before the
                // tool-modal chain, so it works mid-tool; here `smart_mirror` can only be the SDF path).
                if smart_mirror && !self.tool.current.is_smart_mirror()
                    && self.tool.work_mode == WorkMode::Object
                    && self.sel.usel.has_prims()
                {
                    let rect = self.viewport.viewport_rect;
                    let (origin, dir) = self.cam_ray(mpos, rect);
                    let (p_origin, p_normal, surf_n, from_wp) =
                        if self.viewport.wp_mode == WorkPlaneMode::Locked {
                            (self.viewport.wp_resolved.origin, self.viewport.wp_resolved.normal, self.viewport.wp_resolved.normal, true)
                        } else if let Some((hit_pos, hit_normal)) = self.raycast_scene(origin, dir) {
                            // Plane perpendicular to surface, facing camera
                            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
                            let proj = cam_fwd - hit_normal * cam_fwd.dot(hit_normal);
                            let pn = if proj.length_squared() > 0.001 {
                                proj.normalize()
                            } else {
                                let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize();
                                (cam_right - hit_normal * cam_right.dot(hit_normal)).normalize()
                            };
                            (hit_pos, pn, hit_normal, false)
                        } else {
                            (self.viewport.wp_resolved.origin, self.viewport.wp_resolved.normal, Vec3::Y, false)
                        };
                    let (tu, tv) = crate::mirror_tangent_frame(p_normal, surf_n);
                    let abs_n = p_normal.abs();
                    let snapped = if abs_n.x >= abs_n.y && abs_n.x >= abs_n.z { 0u8 }
                                  else if abs_n.y >= abs_n.z { 1 } else { 2 };
                    self.start_operation(ActiveTool::SmartMirror(SmartMirrorState {
                        phase: SmartMirrorPhase::Placing,
                        plane_origin: p_origin, plane_normal: p_normal, base_normal: p_normal,
                        surface_normal: surf_n,
                        tangent_u: tu, tangent_v: tv,
                        snapped_axis: snapped, from_workplane: from_wp,
                        rotation_angle: 0.0,
                        drag_start_mouse: None, drag_start_origin: p_origin,
                        action: SmartMirrorAction::Mirror,
                        half_delete_indices: Vec::new(),
                    }));
                    self.toast("Smart Mirror \u{2014} LMB=Place, Scroll=Rotate, X/Y/Z=Axis\nShift=Instance | Shift+Alt=Delete Half | Esc=Cancel");
                }
                // Symmetry toggle (default was Alt+M — now Merge pie owns Alt+M in poly edit).
                // Still fire when the merge pie is not taking the chord.
                if toggle_symmetry
                    && !(self.modeling_context == ModelingContext::Polygonal
                        && self.poly_work_mode == PolyWorkMode::Vertex
                        && self.poly_sel.selected_vertices.len() >= 2)
                {
                    self.toggle_symmetry();
                }
                // Symmetry axis/ops
                if sym_axis_x { self.set_symmetry_axis(0); }
                if sym_axis_y { self.set_symmetry_axis(1); }
                if sym_axis_z { self.set_symmetry_axis(2); }
                if sym_flip { self.flip_symmetry_normal(); }
                if sym_symmetrize { self.symmetry_symmetrize(); }
                // Ctrl+Shift+D is shared: SDF linear array (handled above) vs poly Delete Half.
                if sym_delete_half && self.modeling_context == ModelingContext::Polygonal {
                    self.symmetry_delete_half();
                }
                if sym_radial_cycle { self.symmetry_cycle_radial(); }
                // Ctrl+M: material assignment popup
                if mat_assign {
                    self.ui.show_mat_assign = !self.ui.show_mat_assign;
                    if self.ui.show_mat_assign {
                        self.ui.mat_assign_pos = mpos;
                        self.ui.mat_assign_search.clear();
                        self.ui.mat_assign_selected = None;
                        self.ui.mat_assign_new_name =
                            format!("Material {}", self.scene.scene_graph.material_count() + 1);
                    }
                }
                // Shift+Z: toggle wireframe overlay (via keybind)
                if wire_key {
                    self.render.wire_overlay_enabled = !self.render.wire_overlay_enabled;
                    self.toast(if self.render.wire_overlay_enabled { "Wireframe overlay ON" } else { "Wireframe overlay OFF" });
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Z pie: shading mode — hold uses ACT_SHADING_PIE (rebind-safe)
                let z_down = ctx.input(|i| self.keybindings.down(ACT_SHADING_PIE, i));
                if z && !self.ui.z_pie.open && !self.tool.current.is_transform() && !self.tool.current.is_bevel() && self.tool.work_mode==WorkMode::Object {
                    // Close other pies before opening
                    self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None;
                    self.ui.view_pie.open = false; self.ui.view_pie.open_time = None;
                    self.ui.f_pie.open = false; self.ui.f_pie.open_time = None;
                    self.ui.z_pie.open(mpos, 7);
                }
                if self.ui.z_pie.open {
                    let z_def = crate::pie_menu::z_pie_def();
                    self.ui.z_pie.update_hover(mpos, &z_def, 1.0);
                    self.ui.z_pie.tick(ctx.input(|i| i.stable_dt));
                    if !z_down {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.z_pie.release();
                        if let PieResult::Inner(q) = result {
                            self.render.shading_mode = match q {
                                0=>ShadingMode::Pbr, 1=>ShadingMode::Raytrace, 2=>ShadingMode::PathTrace,
                                3=>ShadingMode::MatCap, 4=>ShadingMode::Lit, 5=>ShadingMode::SolidHdr, _=>ShadingMode::Wireframe,
                            };
                            // Auto-toggle dither per mode
                            match self.render.shading_mode {
                                ShadingMode::MatCap | ShadingMode::Lit
                                | ShadingMode::SolidHdr | ShadingMode::Wireframe => self.render.dither_enabled = false,
                                ShadingMode::Pbr | ShadingMode::Raytrace => {} // keep preference; default off
                                _ => {}
                            }
                            self.toast(match self.render.shading_mode {
                                ShadingMode::Pbr=>"Shading: PBR",
                                ShadingMode::Raytrace=>"Shading: Raytrace",
                                ShadingMode::PathTrace=>"Shading: Path Trace",
                                ShadingMode::MatCap=>"Shading: MatCap",
                                ShadingMode::Lit=>"Shading: Lit",
                                ShadingMode::SolidHdr=>"Shading: Solid+HDR",
                                ShadingMode::Wireframe=>"Shading: Wireframe",
                            });
                            self.render.pt_accumulated_frames = 0;
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // View pie: backtick key opens view direction selector
                let backtick = ctx.input(|i| i.key_pressed(egui::Key::Backtick));
                let backtick_down = ctx.input(|i| i.key_down(egui::Key::Backtick));
                if backtick && !self.ui.view_pie.open {
                    // Close other pies before opening
                    self.ui.z_pie.open = false; self.ui.z_pie.open_time = None;
                    self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None;
                    self.ui.f_pie.open = false; self.ui.f_pie.open_time = None;
                    self.ui.view_pie.open(mpos, 6);
                }
                if self.ui.view_pie.open {
                    let view_def = crate::pie_menu::view_pie_def();
                    self.ui.view_pie.update_hover(mpos, &view_def, 1.0);
                    self.ui.view_pie.tick(ctx.input(|i| i.stable_dt));
                    if !backtick_down {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.view_pie.release();
                        let apply_view = |s: &mut Self, idx: usize| {
                            let hp = std::f32::consts::FRAC_PI_2;
                            // Compass layout: 0=Front, 1=Right, 2=Bottom, 3=Back, 4=Left, 5=Top
                            s.viewport.camera.orbit = match idx {
                                0 => Quat::from_euler(EulerRot::YXZ, 0.0, 0.0, 0.0),              // Front
                                1 => Quat::from_euler(EulerRot::YXZ, -hp, 0.0, 0.0),              // Right
                                2 => Quat::from_euler(EulerRot::YXZ, 0.0, hp, 0.0),               // Bottom
                                3 => Quat::from_euler(EulerRot::YXZ, std::f32::consts::PI, 0.0, 0.0), // Back
                                4 => Quat::from_euler(EulerRot::YXZ, hp, 0.0, 0.0),               // Left
                                _ => Quat::from_euler(EulerRot::YXZ, 0.0, -hp, 0.0),              // Top
                            };
                            // Same as Alt+MMB snap: temp ortho from Perspective; permanent if Ortho mode.
                            if s.render.ortho && !s.viewport.ortho_from_view_snap {
                                s.toast(match idx {
                                    0 => "View: Front (ortho)",
                                    1 => "View: Right (ortho)",
                                    2 => "View: Bottom (ortho)",
                                    3 => "View: Back (ortho)",
                                    4 => "View: Left (ortho)",
                                    _ => "View: Top (ortho)",
                                });
                            } else {
                                s.render.ortho = true;
                                s.viewport.ortho_from_view_snap = true;
                                s.toast(match idx {
                                    0 => "View: Front (temp ortho)",
                                    1 => "View: Right (temp ortho)",
                                    2 => "View: Bottom (temp ortho)",
                                    3 => "View: Back (temp ortho)",
                                    4 => "View: Left (temp ortho)",
                                    _ => "View: Top (temp ortho)",
                                });
                            }
                            s.ui.last_view_idx = Some(idx);
                            s.mark_dirty(DirtyFlags::RENDER);
                        };
                        match result {
                            PieResult::Inner(idx) => apply_view(self, idx),
                            PieResult::QuickTap => {
                                // Quick tap backtick = toggle perspective/ortho (user choice, not temp snap)
                                let next = !self.render.ortho;
                                self.set_user_projection_ortho(next);
                                self.toast(if next { "Orthographic" } else { "Perspective" });
                            }
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // F pie: utility (Focus / Isolate) — hold uses ACT_FOCUS (rebind-safe).
                // Phase 0: in poly Vertex mode bare F is Fill Face (ACT_POLY_FILL) — do NOT also
                // open the Focus pie (was a double-fire: fill + frame/pie on the same key).
                let f_down = ctx.input(|i| self.keybindings.down(ACT_FOCUS, i));
                let poly_vert_fill_owns_f = self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode == PolyWorkMode::Vertex;
                if f && !self.ui.f_pie.open && !self.tool.current.is_transform() && !poly_vert_fill_owns_f {
                    // Close other pies before opening
                    self.ui.z_pie.open = false; self.ui.z_pie.open_time = None;
                    self.ui.view_pie.open = false; self.ui.view_pie.open_time = None;
                    self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None;
                    self.ui.overlay_pie.open = false; self.ui.overlay_pie.open_time = None;
                    self.ui.f_pie.open(mpos, 4);
                }
                if self.ui.f_pie.open {
                    let f_def = crate::pie_menu::f_pie_def(self.scene.isolated_body_nid.is_some(), self.isolate_context_ghost);
                    self.ui.f_pie.update_hover(mpos, &f_def, 1.0);
                    self.ui.f_pie.tick(ctx.input(|i| i.stable_dt));
                    if !f_down {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.f_pie.release();
                        match result {
                            PieResult::Inner(idx) => { self.apply_f_pie(idx); }
                            PieResult::QuickTap => {
                                // Quick tap F = focus camera (legacy shortcut)
                                self.frame_selected();
                            }
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Shift+O: viewport Overlay pie (lights, grid, workplane, gizmo, …).
                // Hold = hover + release confirm; quick tap opens sticky click-mode.
                let (overlay_pressed, overlay_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_OVERLAY_PIE, i),
                    self.keybindings.down(ACT_OVERLAY_PIE, i),
                ));
                if overlay_pressed
                    && !self.ui.overlay_pie.open
                    && !self.tool.current.is_transform()
                    && !self.tool.current.is_create()
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                {
                    self.ui.z_pie.open = false; self.ui.z_pie.open_time = None;
                    self.ui.view_pie.open = false; self.ui.view_pie.open_time = None;
                    self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None;
                    self.ui.f_pie.open = false; self.ui.f_pie.open_time = None;
                    let n = crate::pie_menu::overlay_pie_def(self.viewport.show_overlays)
                        .inner
                        .items
                        .len();
                    self.ui.overlay_pie.open(mpos, n);
                }
                if self.ui.overlay_pie.open {
                    let o_def = crate::pie_menu::overlay_pie_def(self.viewport.show_overlays);
                    self.ui.overlay_pie.update_hover(mpos, &o_def, 1.0);
                    self.ui.overlay_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                    let pie_age_ms = self
                        .ui
                        .overlay_pie
                        .open_time
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(999);
                    // Hold-Shift+O: confirm on key release. Sticky after quick-tap: LMB.
                    let hold_confirm = !overlay_down && !self.ui.overlay_pie.click_mode;
                    let click_confirm = self.ui.overlay_pie.click_mode
                        && ((lmb && pie_age_ms > 80) || rmb || esc);
                    // Quick release without move → sticky click mode (Blender pie style).
                    if !overlay_down && !self.ui.overlay_pie.click_mode && !self.ui.overlay_pie.has_moved {
                        self.ui.overlay_pie.click_mode = true;
                    } else if hold_confirm || click_confirm {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.overlay_pie.release();
                        self.ui.overlay_pie.click_mode = false;
                        if !esc && !rmb {
                            match result {
                                PieResult::Inner(idx) => self.apply_overlay_pie(idx),
                                PieResult::CenterToggle | PieResult::QuickTap => {
                                    self.apply_overlay_pie_center();
                                }
                                _ => {}
                            }
                        }
                        if lmb || rmb {
                            self.ui.pie_eat_click = true;
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Mode Pie (ACT_MODE_PIE, default Tab): selection mode switcher (SDF / Poly / NURBS)
                if mode_pie_pressed && !self.ui.tab_pie.open && !self.tool.current.is_transform() && !self.tool.current.is_create() {
                    // Knife drops on Tab, and the SAME press continues into the mode pie
                    // (owner 2026-08-07: "Tab need to open the selection on the same first
                    // trigger" — supersedes 2026-08-06's cancel-only Tab, which ate the press
                    // and made reaching the selection pie a double-Tab).
                    if self.poly_tool == PolyTool::Knife || self.knife_shape_kind != 0 {
                        self.poly_knife_cancel();
                        self.toast("Knife cancelled");
                    }
                    if self.tool.mode.is_create_armed() {
                        crate::ops::create::session::deactivate(self);
                    }
                    // Ctrl+R loop cut active → ACCEPT the cut before switching selection mode
                    // (owner 2026-08-06: Tab to change selection should accept the cut).
                    let _ = self.poly_loop_cut_accept_on_mode_switch();
                    // Close other pies before opening
                    self.ui.z_pie.open = false;
                    self.ui.z_pie.open_time = None;
                    self.ui.view_pie.open = false;
                    self.ui.view_pie.open_time = None;
                    self.ui.f_pie.open = false;
                    self.ui.f_pie.open_time = None;
                    // Context-sensitive: derive the pie from the selected object's type.
                    // No matching selection (None/Light/Camera/ImportedMesh) → keep last mode.
                    match self.modeling_context {
                        ModelingContext::Sdf => self.ui.pie_mode = crate::pie_menu::PieMode::Sdf,
                        ModelingContext::Polygonal => {
                            self.ui.pie_mode = crate::pie_menu::PieMode::Poly
                        }
                        ModelingContext::Nurbs => {
                            self.ui.pie_mode = crate::pie_menu::PieMode::Nurbs
                        }
                        _ => {}
                    }
                    // Slot count from the active pie's def (Poly has 5 incl. Sculpt; SDF/NURBS 4).
                    let n_items = crate::pie_menu::tab_pie_def_ex(
                        self.ui.pie_mode,
                        self.poly_pie_shows_group(),
                    )
                    .inner
                    .items
                    .len();
                    self.ui.tab_pie.open(mpos, n_items);
                }
                if self.ui.tab_pie.open {
                    let tab_def = crate::pie_menu::tab_pie_def_ex(
                        self.ui.pie_mode,
                        self.poly_pie_shows_group(),
                    );
                    self.ui.tab_pie.update_hover(mpos, &tab_def, 1.0);
                    self.ui.tab_pie.tick(ctx.input(|i| i.stable_dt));
                    // LMB click on center hub while pie is open → cycle mode LIVE (SDF→Poly→NURBS→SDF)
                    // Uses direct distance check so center is always clickable (bypasses has_moved gate)
                    let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let in_center = (mpos - self.ui.tab_pie.center).length() < 22.0;
                    // LMB on yellow pivot dot → open CENTER pie (never OBJECT mode).
                    if lmb_clicked && self.ui.tab_pie.hovered_object_pivot {
                        self.ui.expose_selection_pivot = true;
                        self.ui.tab_pie.pivot_click_consumed = true;
                        self.ui.tab_pie.hovered_object_pivot = false;
                        // Close Tab pie immediately so Tab release cannot select OBJECT.
                        self.ui.tab_pie.force_close();
                        self.ui.pivot_center_menu_open = false;
                        let n = crate::pie_menu::center_pie_def(self.get_active_pivot_persistent())
                            .inner
                            .items
                            .len();
                        self.ui.d_pie.open(mpos, n);
                        self.ui.d_pie_click_mode = true; // click-to-pick, not hold-D
                    } else if lmb_clicked && in_center && !self.ui.tab_pie.pivot_click_consumed {
                        self.ui.pie_mode = self.ui.pie_mode.cycle_forward();
                        self.ui.tab_pie.hovered_center = false;
                        self.ui.tab_pie.has_moved = false;
                        self.ui.tab_pie.open_pos = mpos;
                    }
                    // Mode Pie key release: only if pie still open (yellow LMB already closed it).
                    if !mode_pie_held && self.ui.tab_pie.open {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.tab_pie.release();
                        match result {
                            PieResult::QuickTap | PieResult::Cancel | PieResult::CenterToggle
                            | PieResult::ObjectPivot => {}
                            // Yellow hover alone (no LMB): still must not select OBJECT.
                            PieResult::Inner(idx) => {
                                if self.ui.tab_pie.hovered_object_pivot {
                                    // defensive — hover path clears inner
                                } else {
                                    self.apply_tab_pie_mode(idx);
                                }
                            }
                            PieResult::Outer { inner, outer } => {
                                self.apply_tab_pie_mode(inner);
                                self.apply_tab_pie_tool(inner, outer);
                            }
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Reset pie: click-to-select (opened from context menu)
                if self.ui.reset_pie.open {
                    let reset_def = crate::pie_menu::reset_pie_def();
                    self.ui.reset_pie.update_hover(mpos, &reset_def, 1.0);
                    self.ui.reset_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                    if lmb_clicked || rmb_clicked || esc {
                        if lmb_clicked || rmb_clicked {
                            self.ui.pie_eat_click = true;
                        }
                        use crate::pie_menu::PieResult;
                        let result = self.ui.reset_pie.release();
                        match result {
                            PieResult::Inner(idx) => {
                                match idx {
                                    0 => { self.reset_position(); self.toast("Position reset"); }
                                    1 => { self.reset_rotation(); self.toast("Rotation reset"); }
                                    2 => { self.reset_scale();    self.toast("Scale reset"); }
                                    _ => { self.reset_transform();self.toast("Transform reset"); }
                                }
                            }
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Edit Pivot mode owns G/R/S: move the pivot (yellow marker), not geometry.
                // G = grab plane drag; R/S switch gizmo mode for feedback (position still via drag/G).
                if (g || r || s) && self.tool.current.is_pivot_edit()
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.f_pie.open
                {
                    if g {
                        self.tool.gizmo_mode = GizmoMode::Translate;
                        self.toast("Pivot grab — move mouse · Ctrl=snap · Enter=confirm · Esc=cancel");
                    } else if r {
                        self.tool.gizmo_mode = GizmoMode::Rotate;
                        self.toast("Pivot edit — drag yellow marker to place (R keeps edit mode)");
                    } else {
                        self.tool.gizmo_mode = GizmoMode::Scale;
                        self.toast("Pivot edit — drag yellow marker to place (S keeps edit mode)");
                    }
                    self.tool.gizmo_armed = true;
                }
                // G/R/S on selected Polyline (CurveExtrude) profile points — modal transform in the
                // profile plane. Precedence: when ANY profile point is selected, G/R/S move the
                // POINTS; with none selected it falls through to the normal prim gizmo-arm below.
                else if (g || r || s) && self.tool.current.is_idle()
                    && !self.curve_pt_sel.is_empty() && self.curve_edit_prim_index().is_some()
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.f_pie.open {
                    let kind = if g { TransformKind::Grab } else if r { TransformKind::Rotate } else { TransformKind::Scale };
                    self.curve_pt_start_transform(kind, mpos);
                }
                // ── Unified G/S/R handler (all modes, including lights) ──
                // G/S/R (no Alt): arm gizmo — show it, wait for user to click an axis / Alt free.
                // Lights used to start an immediate free-grab modal on G (felt like "hold G to
                // move"). They now arm the same persistent gizmo as prims/bodies (owner 2026-08-07).
                else if (g || r || s) && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.view_pie.open && !self.ui.f_pie.open
                    && self.tool.current.is_idle()
                    // The armed knife owns the modal: S toggles its subtract/slice mode, and
                    // arming a transform gizmo mid-cut would stack two tools (same class as the
                    // bevel-under-G/R/S bug fixed above). Not a context split — a real collision.
                    && self.poly_tool != PolyTool::Knife
                {
                    // Check if anything is selected (works for SDF, poly object, poly edit, NURBS, lights)
                    let has_selection = self.active_prim_index_unified().is_some()
                        || self.sel.usel.active_node().is_some()
                        || self.sel.usel.active_light().is_some()
                        || self.sel.usel.has_lights()
                        || (self.modeling_context == ModelingContext::Polygonal
                            && self.poly_work_mode != PolyWorkMode::Object
                            && self.poly_selection_center().is_some())
                        || (self.modeling_context == ModelingContext::Nurbs
                            && self.nurbs_selection_center().is_some());

                    if has_selection {
                        // (Lights support S since 2026-08-07: Scale drives the
                        // attenuation radius — same arm path as everything else.)
                        {
                        // LEAVE BEVEL FIRST (owner 2026-07-29: "the SDF bevel tool still does not
                        // deactivate itself when I switch to G/R/S, resulting in overlay viewport
                        // handles, blank, and all bevel").
                        //
                        // The E (Elongate) handler a few lines below already does exactly this, and
                        // its comment states the rule — "switching tools must leave bevel". G/R/S
                        // simply never got the same treatment, so bevel stayed active underneath the
                        // transform gizmo: two tools armed at once, both drawing overlays.
                        //
                        // CLAUDE.md's Tool Exit Convention says "switching tools = confirm current
                        // work, then switch". Bevel's in-flight edge drag is cancelled rather than
                        // confirmed, matching what E does — a half-dragged bevel is not a result the
                        // user asked to keep, and silently committing it on a G press would be worse
                        // than dropping it.
                        if self.tool.current.is_bevel() || self.tool.current.is_bool_edge_bevel() {
                            let _ = self.cancel_operation();
                        }
                        if self.tool.mode.is_bevel() {
                            self.set_mode(ToolMode::Select);
                            self.toast("Bevel mode OFF");
                        }
                        // Set gizmo mode (or switch if already armed)
                        if g { self.tool.gizmo_mode = GizmoMode::Translate; }
                        if r { self.tool.gizmo_mode = GizmoMode::Rotate; }
                        if s { self.tool.gizmo_mode = GizmoMode::Scale; }
                        self.tool.gizmo_armed = true;
                        }
                    }
                }
                // E key: arm Elongate gizmo in SDF context (consumes E so extrude_tog won't fire).
                // If Bevel mode is on, DROP it immediately — switching tools must leave bevel.
                let mut elongate_consumed = false;
                if elongate_key && self.modeling_context == ModelingContext::Sdf
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open
                    && self.active_prim_index_unified().is_some()
                {
                    // Cancel in-flight bevel edge drag, then exit bevel mode.
                    if self.tool.current.is_bevel() || self.tool.current.is_bool_edge_bevel() {
                        let _ = self.cancel_operation();
                    }
                    if self.tool.mode.is_bevel() {
                        self.set_mode(ToolMode::Select);
                        self.toast("Bevel mode OFF");
                    }
                    if self.tool.current.is_idle() {
                        self.tool.gizmo_mode = GizmoMode::Elongate;
                        self.tool.gizmo_armed = true;
                        elongate_consumed = true;
                    }
                }
                // Polyline profile-point BEVEL (B) — gated STRICTLY behind an active point editor
                // WITH a selection, evaluated BEFORE the box/cylinder/poly bevel handlers so those
                // are untouched when the curve editor is not active. Consumes B (curve_bevel_taken).
                //
                // Owner workflow (2026-07-23): the persistent DIAMOND gizmo owns bevel — drag it to
                // change the radius, click it to cycle the corner type. B must NOT start changing
                // the radius with bare cursor movement (the old auto-size modal is deleted); it
                // only points the user at the already-visible diamond for the current selection.
                let mut curve_bevel_taken = false;
                if curve_bevel_key && self.tool.current.is_idle()
                    && self.curve_edit_prim_index().is_some() && !self.curve_pt_sel.is_empty()
                    && !curve_bevel_taken
                {
                    curve_bevel_taken = true;
                    self.toast("Bevel: drag the diamond to size, click it to cycle corner type");
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // NURBS Ctrl+B / B — same shortcut idea as poly/SDF bevel
                if !curve_bevel_taken
                    && (bevel_tog || pkf.bevel)
                    && self.modeling_context == ModelingContext::Nurbs
                    && !self.tool.current.is_create()
                    && !self.tool.current.is_transform()
                {
                    self.nurbs_try_bevel();
                }
                // Bevel toggle — SDF only (B or Ctrl+B, auto-commit active tool first)
                else if !curve_bevel_taken
                    && (bevel_tog || (pkf.bevel && self.modeling_context == ModelingContext::Sdf))
                    && self.modeling_context != ModelingContext::Polygonal
                    && self.modeling_context != ModelingContext::Nurbs
                    && !self.tool.current.is_create() && !self.tool.current.is_transform()
                    && !self.tool.current.is_pivot_edit()
                {
                    // Auto-commit face drag and exit extrude mode if active
                    if self.tool.current.is_face_push_pull() { self.tool.current = ActiveTool::Idle; self.rebake_no_undo(); }
                    if self.tool.mode.is_extrude() {
                        self.tool.work_mode = WorkMode::Object;
                        self.sel.edit_target = None;
                        self.sel.selected_faces.clear();
                    }
                    if self.tool.work_mode != WorkMode::Object { self.tool.work_mode = WorkMode::Object; }
                    // Resolve the prim the user is working on (active / last-clicked /
                    // single selected prim / single prim under body). Stamp it as active
                    // Prim so handles and a single B press never miss after a body-fallback
                    // active (owner 2026-08-08: often needed to press B twice).
                    if let Some(idx) = self.resolved_prim_index_unified() {
                        if let Some(uid) = self.scene.primitives.get(idx).map(|p| p.prim_uid) {
                            // Ensure active is the Prim (not the parent body) so the next
                            // tool / property panel / B toggle sees it without a re-click.
                            if self.sel.usel.active_prim_uid() != Some(uid) {
                                self.sel.usel.active = Some(SceneItemId::Prim(uid));
                                self.sel.usel.selected.insert(SceneItemId::Prim(uid));
                                self.sel.usel.last_clicked = Some(SceneItemId::Prim(uid));
                            }
                        }
                        let entering = !self.tool.mode.is_bevel();
                        if entering {
                            self.set_mode(ToolMode::Bevel);
                            self.toast("Bevel mode ON");
                        } else {
                            self.set_mode(ToolMode::Select);
                            self.toast("Bevel mode OFF");
                        }
                    } else {
                        self.toast("Select an object first");
                    }
                }
                // Extrude toggle — DISABLED in SDF context (E is now Elongate)
                // Kept for polygonal context only.
                if extrude_tog && !elongate_consumed
                    && self.modeling_context == ModelingContext::Polygonal
                    && !self.tool.current.is_create() && !self.tool.current.is_transform()
                    && !self.tool.current.is_pivot_edit()
                {
                    if self.tool.work_mode != WorkMode::Object && self.tool.work_mode != WorkMode::Face {
                        self.tool.work_mode = WorkMode::Object;
                    }
                    if self.active_prim_index_unified().is_some() {
                        let entering = !self.tool.mode.is_extrude();
                        if entering {
                            self.set_mode(ToolMode::Extrude);
                            self.tool.work_mode = WorkMode::Face;
                            self.sel.edit_target = self.active_prim_index_unified();
                            self.toast("Extrude mode — click face to push/pull");
                        } else {
                            self.set_mode(ToolMode::Select);
                            self.tool.work_mode = WorkMode::Object;
                            self.sel.edit_target = None;
                            self.sel.selected_faces.clear();
                            self.toast("Extrude mode OFF");
                        }
                    } else {
                        self.toast("Select an object first");
                    }
                }
                // D-key pivot pie menu (hold-D)
                if pivot_ed && !self.tool.current.is_create() && !self.tool.current.is_transform()
                    && !self.ui.d_pie.open
                    && (self.sel.usel.has_prims() || self.sel.usel.has_nodes())
                {
                    self.ui.z_pie.open = false; self.ui.tab_pie.open = false;
                    self.ui.view_pie.open = false; self.ui.f_pie.open = false;
                    self.ui.d_pie_click_mode = false;
                    let n = crate::pie_menu::d_pie_def(self.get_active_pivot_persistent())
                        .inner.items.len();
                    self.ui.d_pie.open(mpos, n);
                    self.ui.expose_selection_pivot = true;
                }
                // D-pie update + confirm — hold uses ACT_PIVOT_EDIT (rebind-safe)
                let d_key_down = ctx.input(|i| self.keybindings.down(ACT_PIVOT_EDIT, i));
                let d_pie_open = self.ui.d_pie.open;
                if d_pie_open {
                    let persistent = self.get_active_pivot_persistent();
                    let d_def = crate::pie_menu::d_pie_def(persistent);
                    self.ui.d_pie.update_hover(mpos, &d_def, 1.0);
                    self.ui.d_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                    // Ignore the opening LMB frame in click-mode (same click that opened the pie).
                    let pie_age_ms = self
                        .ui
                        .d_pie
                        .open_time
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(999);
                    // Hold-D: confirm on key release. Click-mode (yellow dot): LMB after open.
                    let confirm = if self.ui.d_pie_click_mode {
                        (lmb && pie_age_ms > 80) || rmb || esc
                    } else {
                        !d_key_down
                    };
                    if confirm {
                        use crate::pie_menu::PieResult;
                        let result = self.ui.d_pie.release();
                        self.ui.d_pie_click_mode = false;
                        let vp_rect = self.viewport.viewport_rect;
                        if !esc && !rmb {
                            match result {
                                PieResult::Inner(idx) => {
                                    self.apply_pivot_from_pie(idx, mpos, vp_rect);
                                }
                                PieResult::CenterToggle => self.toggle_pivot_persistent(),
                                _ => {}
                            }
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Join poly bodies — ACT_POLY_JOIN (object-mode poly + idle).
                if ctx.input(|i| self.keybindings.pressed(ACT_POLY_JOIN, i))
                    && self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode == PolyWorkMode::Object
                    && self.tool.current.is_idle()
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                {
                    if self.poly_join_has_candidates() {
                        self.poly_join();
                    } else {
                        self.toast("Join: select 2 or more polygonal objects first");
                    }
                }
                // Alt+C (Circle: reshape the selected face region onto a circle) dispatches
                // through ACTION_DEFS / PolyKeyFlags in poly_handle_keys → key_dispatch.rs, like
                // every other poly edit key — persisted to keybinds.json and rebindable.
                // Poly boolean pie — ACT_POLY_BOOLEAN_PIE (hold → release). Shared X with sculpt/SDF, state-gated.
                let (bool_key_pressed, bool_key_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_POLY_BOOLEAN_PIE, i),
                    self.keybindings.down(ACT_POLY_BOOLEAN_PIE, i),
                ));
                if bool_key_pressed && !self.ui.bool_pie.open
                    && self.modeling_context == ModelingContext::Polygonal
                    && self.tool.work_mode != WorkMode::Sculpt
                    && self.tool.current.is_idle()
                    && self.poly_tool == PolyTool::Idle
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.view_pie.open
                    && !self.ui.f_pie.open && !self.ui.d_pie.open
                {
                    if self.poly_boolean_has_candidates() {
                        self.ui.bool_pie.open(mpos, 8); // 8-sector: Union/Subtract/Intersect/Cut/Keep
                    } else {
                        self.toast("Boolean: select a polygonal object (or poly sub-elements) first");
                    }
                }
                if self.ui.bool_pie.open {
                    let bool_def = crate::pie_menu::boolean_pie_def(self.ui.bool_pie_keep_cutter);
                    self.ui.bool_pie.update_hover(mpos, &bool_def, 1.0);
                    self.ui.bool_pie.tick(ctx.input(|i| i.stable_dt));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    if esc || rmb {
                        let _ = self.ui.bool_pie.release();
                    } else if !bool_key_down {
                        use crate::pie_menu::PieResult;
                        use core_math::mesh_boolean::BoolOp;
                        // 8-sector layout: 0=Union, 2=Subtract, 4=Intersect, 5=Cut, 6=KeepCutter
                        match self.ui.bool_pie.release() {
                            PieResult::Inner(0) => self.poly_boolean_execute(BoolOp::Union),
                            PieResult::Inner(2) => self.poly_boolean_execute(BoolOp::Difference),
                            PieResult::Inner(4) => self.poly_boolean_execute(BoolOp::Intersect),
                            PieResult::Inner(5) => self.poly_boolean_execute_cut(),
                            PieResult::Inner(6) => {
                                self.ui.bool_pie_keep_cutter = !self.ui.bool_pie_keep_cutter;
                                self.toast(if self.ui.bool_pie_keep_cutter {
                                    "Boolean: keep cutter ON"
                                } else {
                                    "Boolean: keep cutter OFF"
                                });
                            }
                            PieResult::CenterToggle => {
                                self.toast("Live boolean \u{2014} coming soon");
                            }
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // ── Alt+M Merge pie — HOLD-PIE TEMPLATE (same as Z / bool / T) ──
                // Open ONLY on key *press* while closed. Holding never re-opens or re-applies.
                // Confirm ONLY when the key goes up (`!merge_key_down` → release()).
                let (merge_key_pressed, merge_key_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_POLY_MERGE_MENU, i),
                    self.keybindings.down(ACT_POLY_MERGE_MENU, i),
                ));
                if merge_key_pressed
                    && !self.ui.merge_pie.open
                    && self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode == PolyWorkMode::Vertex
                    && self.tool.current.is_idle()
                    && self.poly_tool == PolyTool::Idle
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                    && !self.ui.z_pie.open
                    && !self.ui.tab_pie.open
                    && !self.ui.bool_pie.open
                    && !self.ui.t_pie.open
                    && !self.ui.align_pie.open
                {
                    if self.poly_sel.selected_vertices.len() >= 2 {
                        self.ui.sym_pie.force_close();
                        self.ui.align_pie.force_close();
                        self.ui.merge_pie.open(mpos, 3);
                    } else {
                        self.toast("Merge pie: select 2+ vertices first");
                    }
                }
                if self.ui.merge_pie.open {
                    let merge_def = crate::pie_menu::merge_pie_def();
                    self.ui.merge_pie.update_hover(mpos, &merge_def, 1.0);
                    self.ui.merge_pie.tick(ctx.input(|i| i.stable_dt));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    if esc || rmb {
                        let _ = self.ui.merge_pie.release();
                    } else if !merge_key_down {
                        // Key released → confirm once. Holding never reaches here.
                        use crate::pie_menu::PieResult;
                        match self.ui.merge_pie.release() {
                            PieResult::Inner(0) => self.apply_merge_pie(0),
                            PieResult::Inner(1) => self.apply_merge_pie(1),
                            PieResult::Inner(2) => self.apply_merge_pie(2),
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // ── Alt+A Align / Flatten pie (poly edit) — HOLD-PIE TEMPLATE ──
                // Blender MACHIN3 Align Pie: Top / Right / Bottom / Left (view-relative
                // flatten = scale-to-0 on that axis). Owns Alt+A in poly edit so it does
                // NOT also fire ACT_DESELECT_ALL (that was wiping selection the same press).
                //
                // HOLD UX (owner 2026-08-06: "Alt+A pie hard to use"):
                //   • Open on ACT_DESELECT_ALL press (default Alt+A — same bind as deselect).
                //   • Confirm when the **letter key** is released — NOT when Alt drops first.
                //     Requiring full chord hold made releasing Alt mid-aim cancel the pie
                //     (QuickTap/Cancel) before a sector was chosen — conflicted with Alt+LMB
                //     region / Alt+double ring muscle memory (hold Alt, then act).
                //   • Drop region/paint select on open (Alt is shared; leftover PolyRegionSelect
                //     made tool.current busy → pie refused to open).
                let align_bind_key = self.keybindings.get(ACT_DESELECT_ALL).key;
                let (align_chord_pressed, align_letter_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_DESELECT_ALL, i),
                    i.key_down(align_bind_key),
                ));
                let poly_edit = self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode != PolyWorkMode::Object;
                if align_chord_pressed
                    && !self.ui.align_pie.open
                    && poly_edit
                    && self.poly_tool == PolyTool::Idle
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                    && !self.ui.z_pie.open
                    && !self.ui.tab_pie.open
                    && !self.ui.bool_pie.open
                    && !self.ui.merge_pie.open
                    && !self.ui.t_pie.open
                {
                    // Allow open over region/paint (Alt-owned select); not over real modals.
                    let busy_ok = self.tool.current.is_idle()
                        || self.tool.current.is_poly_region_select()
                        || self.tool.current.is_poly_paint_select()
                        || self.tool.current.is_poly_box_select()
                        || self.tool.current.is_poly_circle_select();
                    if busy_ok {
                        if self.tool.current.is_poly_region_select()
                            || self.tool.current.is_poly_paint_select()
                            || self.tool.current.is_poly_box_select()
                            || self.tool.current.is_poly_circle_select()
                        {
                            self.tool.current = ActiveTool::Idle;
                        }
                        if self.align_pie_can_open() {
                            self.ui.sym_pie.force_close();
                            self.ui.merge_pie.force_close();
                            self.ui.align_pie.open(mpos, 4);
                        } else {
                            self.toast("Align: select 2+ verts / edges / faces");
                        }
                    }
                }
                if self.ui.align_pie.open {
                    let align_def = crate::pie_menu::align_pie_def();
                    self.ui.align_pie.update_hover(mpos, &align_def, 1.0);
                    self.ui.align_pie.tick(ctx.input(|i| i.stable_dt));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    // Hold ends when letter (A) is up — Alt may already be released.
                    if esc || rmb {
                        let _ = self.ui.align_pie.release();
                    } else if !align_letter_down {
                        use crate::pie_menu::PieResult;
                        match self.ui.align_pie.release() {
                            PieResult::Inner(idx) => self.apply_align_pie(idx),
                            PieResult::CenterToggle => self.apply_align_workplane(),
                            _ => {}
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // T is context-split (owner 2026-07-24): in the SDF context it toggles
                // Global/Local transform space; in Poly modeling it opens the Action-Center
                // pie (Automatic/Selection/Element/Screen/Origin/Pivot...). Hold-to-pick
                // like Z / Tab pie — confirm on ACT_TOGGLE_SPACE release, not LMB click.
                if t_key {
                    if self.modeling_context == ModelingContext::Polygonal {
                        if !self.ui.t_pie.open {
                            self.ui.z_pie.open = false; self.ui.z_pie.open_time = None;
                            self.ui.tab_pie.open = false; self.ui.tab_pie.open_time = None;
                            self.ui.t_pie.open(mpos, 8);
                        }
                    } else {
                        self.tool.transform_space = match self.tool.transform_space {
                            TransformSpace::Global => TransformSpace::Local,
                            TransformSpace::Local => TransformSpace::Global,
                        };
                        self.toast(&format!("Space: {:?}", self.tool.transform_space));
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                // Action-Center pie: key-up release → apply_action_center_from_pie (z_pie pattern).
                if self.ui.t_pie.open {
                    let t_def = crate::pie_menu::t_pie_def(self.tool.action_center.label());
                    self.ui.t_pie.update_hover(mpos, &t_def, 1.0);
                    self.ui.t_pie.tick(ctx.input(|i| i.stable_dt));
                    let t_down = ctx.input(|i| self.keybindings.down(ACT_TOGGLE_SPACE, i));
                    if !t_down {
                        use crate::pie_menu::PieResult;
                        if let PieResult::Inner(idx) = self.ui.t_pie.release() {
                            self.apply_action_center_from_pie(idx);
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Polyline profile-point editor edit ops (gated on an active editor + idle tool):
                // Ctrl+X = dissolve selected, Ctrl+D = subdivide selected segments.
                if curve_dissolve && self.tool.current.is_idle() && self.curve_edit_prim_index().is_some() {
                    self.curve_pt_dissolve();
                }
                if curve_subdivide && self.tool.current.is_idle() && self.curve_edit_prim_index().is_some() {
                    self.curve_pt_subdivide();
                }
                // Polyline profile-point delete takes precedence over prim/object delete when the
                // point editor is active and points are selected (guards a closed profile at ≥3).
                if del&&!ctx.input(|i|i.modifiers.alt)&&self.tool.current.is_idle()
                   &&!self.curve_pt_sel.is_empty()&&self.curve_edit_prim_index().is_some(){
                    self.curve_pt_delete();
                }
                else if del&&!ctx.input(|i|i.modifiers.alt)&&self.tool.current.is_idle()
                   &&!self.spine_pt_sel.is_empty()&&self.spine_edit_prim_index().is_some(){
                    self.spine_pt_delete();
                }
                else if del
                    && !ctx.input(|i| i.modifiers.alt)
                    && self.tool.current.is_idle()
                    && !self.tool.mode.is_create_armed()
                    && {
                        // Owner 2026-08-07: Del on a poly sphere in Face mode took 2× Ctrl+Z to
                        // restore. `tool.work_mode` often stays Object while `poly_work_mode` is
                        // Face/Edge/Vertex — so the same Del key ran poly component delete AND
                        // object `delete_selected` (two undo entries). Object-level Del only when
                        // not in poly component edit; SDF vertex edit still uses object Del.
                        let poly_component_edit = self.modeling_context
                            == ModelingContext::Polygonal
                            && self.poly_work_mode != PolyWorkMode::Object;
                        !poly_component_edit
                            && (self.tool.work_mode == WorkMode::Object
                                || (self.tool.work_mode == WorkMode::Vertex
                                    && self.modeling_context == ModelingContext::Sdf))
                    }
                {
                    // ui2 outliner selects lights as Node — resolve that before delete_selected
                    // so single-sun Del always works (scene may have zero lights after).
                    if let Some(li) = self.sel.usel.active_light() {
                        self.delete_light(li);
                    } else if let Some(lid) = self.sel.usel.active_node().and_then(|nid| {
                        self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                            core_math::scene_graph::NodePayload::Light(d) => Some(d.id),
                            _ => None,
                        })
                    }) {
                        self.delete_light(lid);
                    } else {
                        // Multi-select / bodies / poly / etc. — shared core.
                        self.delete_selected();
                    }
                }
                // X key: SDF boolean pie (hold X → hover op → release) — ACT_SDF_BOOLEAN_PIE
                // (state-gated SDF idle). Replaces the old blind Union→Subtract→Intersect
                // cycle — the pie shows every op type incl. Seam. X is shared with the poly
                // pie / Sculpt brush-size / transform axis-constraint — disambiguated by state.
                let (sb_pressed, sb_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_SDF_BOOLEAN_PIE, i),
                    self.keybindings.down(ACT_SDF_BOOLEAN_PIE, i),
                ));
                if sb_pressed && !self.ui.sdf_bool_pie.open
                    && !self.ui.sdf_special_pie.open
                    && self.modeling_context == ModelingContext::Sdf
                    && self.tool.current.is_idle() && !self.tool.current.is_transform()
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.view_pie.open
                    && !self.ui.f_pie.open && !self.ui.d_pie.open && !self.ui.bool_pie.open
                {
                        if self.sel.usel.has_prims() {
                            // Prim targets: every selected NON-base prim. The chain base keeps
                            // the old cycle's rule — no boolean op, no pie. (Base = first slot
                            // of resolve_child_order, not flat index 0 — after a reorder the
                            // ex-base is editable and the new base is not.)
                            let any_non_base = self.selected_prim_indices_unified().iter()
                                .any(|&i| i < self.scene.primitives.len()
                                    && !self.scene.is_base_prim_flat(i));
                            if any_non_base {
                                self.ui.sdf_bool_pie.open(mpos, 8);
                            } else {
                                self.toast("Base of chain — no boolean op");
                            }
                        } else if self.sel.usel.has_nodes() {
                            let body_nodes: Vec<core_math::scene_graph::NodeId> =
                                self.sel.usel.selected_nodes()
                                    .filter(|&nid| self.scene.scene_graph.get(nid)
                                        .map(|n| matches!(n.payload,
                                            core_math::scene_graph::NodePayload::SdfBody { .. }))
                                        .unwrap_or(false))
                                    .collect();
                            if body_nodes.len() == 2 {
                                // Two bodies selected: KEEP the X nest gesture (fires on press,
                                // no pie) — nest first into second with Subtract. Routed through
                                // scene_actions::reparent_body_node — THE single nest/un-nest
                                // action (CLAUDE "two code paths" rule). The old inline reparent
                                // here never dirtied the child's OLD parent → ghost volume (the
                                // exact bug the outliner drag path fixed in b18fe74).
                                let child = body_nodes[0];
                                let parent = body_nodes[1];
                                // Same guards reparent_body_node enforces — check up-front so we
                                // don't toast success (or flip the op) on a rejected nest.
                                let old_parent = self.scene.scene_graph.get(child).and_then(|n| n.parent);
                                if child == parent
                                    || old_parent == Some(parent)
                                    || self.scene.scene_graph.is_descendant_of(parent, child)
                                {
                                    self.toast("Cannot nest: already nested or would create a cycle");
                                } else {
                                    self.transaction("Nest body (Subtract)", |s| {
                                        let child_is_union = s.scene.scene_graph.body_op(child)
                                            .map(|op| op.is_additive())
                                            .unwrap_or(false);
                                        s.reparent_body_node(child, parent);
                                        // X-key semantic: an additive child becomes a Subtract operand
                                        // (this is the ACTION's meaning — reparent_body_node itself
                                        // preserves body_op per the owner-directed b18fe74 rule).
                                        if child_is_union {
                                            s.scene.scene_graph.set_body_op(child, BooleanOp::Subtract { k: 0.0 });
                                            s.sync_flat_cache();
                                            s.mark_node_body_vol_dirty(child);
                                        }
                                        s.select_single_unified(SceneItemId::Node(child));
                                        s.rebake_no_undo();
                                    });
                                    self.toast("Nested body (Subtract)");
                                }
                            } else if !body_nodes.is_empty() {
                                // Sub-body selection: pie sets body_op on every selected body.
                                self.ui.sdf_bool_pie.open(mpos, 8);
                            }
                        }
                }
                // Special pie (from MORE…) before bool pie so a live center-switch
                // doesn't also get processed as a bool-pie release the same frame.
                //
                // Interaction matches Tab pie center: LMB on hub switches LIVE while
                // the key is still held. Release X to confirm a hovered sector.
                // Sticky (click-to-select) only if the pie was opened with X already up
                // (e.g. released X on MORE… without a prior LMB).
                if self.ui.sdf_special_pie.open {
                    let def = crate::pie_menu::sdf_special_pie_def();
                    self.ui.sdf_special_pie.update_hover(mpos, &def, 1.0);
                    self.ui.sdf_special_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let sticky = self.ui.sdf_special_pie_sticky;
                    // LIVE BACK: LMB center hub (like Tab pie center cycle).
                    let in_center = (mpos - self.ui.sdf_special_pie.center).length() < 22.0;
                    if lmb && in_center {
                        self.ui.pie_eat_click = true;
                        let _ = self.ui.sdf_special_pie.release();
                        self.ui.sdf_special_pie_sticky = false;
                        self.ui.sdf_bool_pie.open(mpos, 8);
                        // Keep hold-X if still held; sticky only if X is already up.
                        self.ui.sdf_bool_pie_sticky = !sb_down;
                    } else if esc || rmb {
                        if rmb {
                            self.ui.pie_eat_click = true;
                        }
                        let _ = self.ui.sdf_special_pie.release();
                        self.ui.sdf_special_pie_sticky = false;
                    } else if if sticky { lmb } else { !sb_down } {
                        if sticky && lmb {
                            self.ui.pie_eat_click = true;
                        }
                        use crate::pie_menu::PieResult;
                        match self.ui.sdf_special_pie.release() {
                            PieResult::Inner(0) => self.extract_intersection(),
                            PieResult::CenterToggle => {
                                // Released X on BACK hub (no LMB) → return to bool pie.
                                self.ui.sdf_bool_pie.open(mpos, 8);
                                self.ui.sdf_bool_pie_sticky = !sb_down;
                            }
                            _ => {}
                        }
                        self.ui.sdf_special_pie_sticky = false;
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                } else if self.ui.sdf_bool_pie.open {
                    let sdf_bool_def = crate::pie_menu::sdf_boolean_pie_def();
                    self.ui.sdf_bool_pie.update_hover(mpos, &sdf_bool_def, 1.0);
                    self.ui.sdf_bool_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let sticky = self.ui.sdf_bool_pie_sticky;
                    // LIVE MORE…: LMB center hub while pie open (Tab-pie style).
                    let in_center = (mpos - self.ui.sdf_bool_pie.center).length() < 22.0;
                    if lmb && in_center {
                        self.ui.pie_eat_click = true;
                        let _ = self.ui.sdf_bool_pie.release();
                        self.ui.sdf_bool_pie_sticky = false;
                        self.ui.sdf_special_pie.open(mpos, 4);
                        // Still holding X → special pie uses key-release confirm.
                        // X already up (sticky bool) → special pie is click-to-select.
                        self.ui.sdf_special_pie_sticky = !sb_down;
                    } else if esc || rmb {
                        if rmb {
                            self.ui.pie_eat_click = true;
                        }
                        let _ = self.ui.sdf_bool_pie.release();
                        self.ui.sdf_bool_pie_sticky = false;
                    } else if if sticky { lmb } else { !sb_down } {
                        if sticky && lmb {
                            self.ui.pie_eat_click = true;
                        }
                        use crate::pie_menu::PieResult;
                        match self.ui.sdf_bool_pie.release() {
                            PieResult::Inner(slot) => {
                                self.ui.sdf_bool_pie_sticky = false;
                                self.apply_sdf_bool_pie(slot);
                            }
                            // Released X on MORE… hub → open special (sticky, X is up).
                            PieResult::CenterToggle => {
                                self.ui.sdf_bool_pie_sticky = false;
                                self.ui.sdf_special_pie.open(mpos, 4);
                                self.ui.sdf_special_pie_sticky = true;
                            }
                            _ => {
                                self.ui.sdf_bool_pie_sticky = false;
                            }
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // Ctrl+R SDF resolution pie — ACT_SDF_RESOLUTION_PIE. State-gated context
                // split of the Ctrl+R chord: Sculpt = ACT_SCULPT_REMESH (WorkMode::Sculpt,
                // handled at the top of this fn), Poly Edge = ACT_POLY_LOOP_CUT (Polygonal
                // context). This gate (SDF AND not Sculpt) is mutually exclusive with both.
                let (res_pie_pressed, res_pie_down) = ctx.input(|i| (
                    self.keybindings.pressed(ACT_SDF_RESOLUTION_PIE, i),
                    self.keybindings.down(ACT_SDF_RESOLUTION_PIE, i),
                ));
                if res_pie_pressed && !self.ui.res_pie.open
                    && self.modeling_context == ModelingContext::Sdf
                    && self.tool.work_mode != WorkMode::Sculpt
                    && self.tool.current.is_idle() && !self.tool.current.is_transform()
                    && self.active_input_context(ctx) == crate::input_system::InputContext::Viewport
                    && !self.ui.z_pie.open && !self.ui.tab_pie.open && !self.ui.view_pie.open
                    && !self.ui.f_pie.open && !self.ui.d_pie.open && !self.ui.bool_pie.open
                    && !self.ui.sdf_bool_pie.open
                {
                    // Needs an SDF body or prim selected/active — resolution is a property
                    // of the ACTIVE top-level body (same target as the Grid picker).
                    // modeling_context == Sdf already implies an SDF selection; this also
                    // verifies the active body resolves to a real SdfBody node.
                    if self.resolution_target_body().is_some() {
                        self.ui.res_pie_sticky = false;
                        self.ui.res_pie.open(mpos, 6);
                    } else {
                        self.toast("Resolution: select an SDF body or primitive first");
                    }
                }
                if self.ui.res_pie.open {
                    let sel_max = self.resolution_target_body()
                        .map(|b| self.body_grid_res_state(b).1)
                        .unwrap_or_else(crate::gpu_jobs::max_gridbacked_resolution);
                    let res_def = crate::pie_menu::resolution_pie_def(sel_max);
                    self.ui.res_pie.update_hover(mpos, &res_def, 1.0);
                    self.ui.res_pie.tick(ctx.input(|i| i.stable_dt));
                    let lmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
                    let rmb = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                    let pie_age_ms = self.ui.res_pie.open_time
                        .map(|t| t.elapsed().as_millis()).unwrap_or(999);
                    use crate::pie_menu::PieResult;
                    if esc || rmb {
                        // Tool Exit Convention: Esc / RMB = cancel.
                        let _ = self.ui.res_pie.release();
                        self.ui.res_pie_sticky = false;
                    } else if lmb && pie_age_ms > 80 {
                        // Click-select (sticky after a quick tap, but a deliberate LMB
                        // always confirms). Age guard skips the opening-frame click.
                        if let PieResult::Inner(slot) = self.ui.res_pie.release() {
                            self.apply_resolution_pie(slot);
                        }
                        self.ui.res_pie_sticky = false;
                    } else if !res_pie_down && !self.ui.res_pie_sticky {
                        if self.ui.res_pie.has_moved {
                            // Flick / hover + key release.
                            if let PieResult::Inner(slot) = self.ui.res_pie.release() {
                                self.apply_resolution_pie(slot);
                            }
                        } else {
                            // Quick tap: stay open, confirm via LMB (click mode).
                            self.ui.res_pie_sticky = true;
                        }
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                // K key (SDF only): Draw Polyline — click points on the workplane, Enter to
                // close into a CurveExtrude prim (owner 2026-08-08, replacing the reverted SDF
                // knife). Poly-mesh K is untouched: `ACT_POLY_KNIFE` is WHEN_POLYGONAL and the
                // two modeling contexts are mutually exclusive.
                //
                // Only from Idle — re-pressing K mid-draw must not restart the tool and throw
                // away the points already placed.
                if sdf_polyline_key
                    && self.modeling_context == ModelingContext::Sdf
                    && self.tool.current.is_idle()
                {
                    crate::ops::sdf::draw_polyline::run(self);
                }
                // C key: enter clip curve draw mode (or commit if already drawing)
                if clip_curve_key && self.modeling_context == ModelingContext::Sdf {
                    if self.tool.current.is_clip_curve_draw() {
                        // Already drawing — C again commits
                        self.clip_curve_commit();
                    } else if self.tool.current.is_idle() && (self.sel.usel.has_prims() || self.sel.usel.has_nodes()) {
                        self.start_clip_curve_draw();
                    }
                }
                // Enter: commit clip curve polygon
                if enter && self.tool.current.is_clip_curve_draw() {
                    self.clip_curve_commit();
                }
                // Shift+D array session keys (C / I / M / Enter) — before generic handlers.
                if self.array_dup_active() {
                    let bare = |k: egui::Key| {
                        ctx.input(|i| {
                            i.key_pressed(k)
                                && !i.modifiers.command
                                && !i.modifiers.shift
                                && !i.modifiers.alt
                        })
                    };
                    if bare(egui::Key::C) {
                        self.array_dup_toggle_circle();
                    } else if bare(egui::Key::I) {
                        self.array_dup_toggle_instance();
                    } else if bare(egui::Key::M) {
                        self.array_dup_toggle_as_modifier();
                    } else if enter {
                        self.array_dup_confirm();
                    } else if esc || space {
                        self.array_dup_cancel();
                    }
                }
                // Shift+D = poly Object: live array session (scroll count, C/I/M).
                // Poly edit: component duplicate. SDF prims: Ctrl+Shift+D = free-grab array.
                // Ctrl+Shift+D = linear array free-grab on workplane (scroll count, click confirm).
                let ctrl_shift_d = ctx.input(|i| {
                    i.key_pressed(egui::Key::D)
                        && i.modifiers.command
                        && i.modifiers.shift
                        && !i.modifiers.alt
                });
                if ctrl_shift_d && self.active_prim_index_unified().is_some() {
                    // Linear array tool — free workplane grab until LMB (not gizmo).
                    self.start_modal_transform(TransformKind::Grab, mpos, true);
                } else if shift_d && !self.array_dup_active() {
                    if let Some(li) = self.sel.usel.active_light() {
                        self.duplicate_light(li);
                        self.tool.gizmo_mode = GizmoMode::Translate;
                        self.tool.gizmo_armed = true;
                    } else if self.modeling_context == ModelingContext::Polygonal
                        && self.poly_work_mode != PolyWorkMode::Object
                    {
                        self.poly_duplicate_selection();
                        self.tool.gizmo_mode = GizmoMode::Translate;
                        self.tool.gizmo_armed = true;
                    } else if self.modeling_context == ModelingContext::Polygonal
                        && self.poly_work_mode == PolyWorkMode::Object
                    {
                        // Poly Object Shift+D → array session (scroll / C / I / M).
                        self.start_poly_shift_dup_array();
                    } else if self.active_prim_index_unified().is_some() {
                        // SDF prims: same gizmo array session as poly.
                        self.start_shift_dup_array();
                    } else {
                        // Bodies (NURBS/empty/etc.) — simple duplicate.
                        self.duplicate_selected();
                        self.tool.gizmo_mode = GizmoMode::Translate;
                        self.tool.gizmo_armed = true;
                    }
                }
                // Clipboard — ACT_COPY / ACT_CUT / ACT_PASTE (shortcut editor rebindable).
                // The Shader Node Editor owns these shortcuts while hovered; letting the scene
                // handler run too would cut or paste scene objects before the graph sees the key.
                let node_editor_owns_clipboard = self.ui2_pointer_over_node_editor(ctx)
                    || (self.ui.show_node_editor && pointer_over_ui)
                    || self.ui.node_drag_state.owns_clipboard_shortcuts();
                if !node_editor_owns_clipboard {
                    if ctx.input(|i| self.keybindings.pressed(ACT_COPY, i)) {
                        self.copy_selection();
                    }
                    if ctx.input(|i| self.keybindings.pressed(ACT_CUT, i)) {
                        // Ctrl+X = ACT_CUT and ACT_POLY_DISSOLVE (same Event::Cut).
                        // cut_selection owns poly (dissolve / refuse) + non-poly clipboard cut.
                        Self::ctrx_diag("input_keyboard ACT_CUT pressed → cut_selection");
                        self.cut_selection();
                    }
                    if ctx.input(|i| self.keybindings.pressed(ACT_PASTE, i)) {
                        self.paste(crate::clipboard::PastePosition::Original);
                    }
                }
                // Ctrl+Shift+G: dissolve child SdfBody
                if ctx.input(|i| i.key_pressed(egui::Key::G) && i.modifiers.command && i.modifiers.shift) {
                    if self.modeling_context == ModelingContext::Sdf {
                        if let Some(sel_nid) = self.sel.usel.active_node() {
                            if sel_nid != self.scene.sdf_body_nid {
                                self.dissolve_sdf_body(sel_nid);
                            }
                        } else if self.scene.active_body_nid != self.scene.sdf_body_nid {
                            self.dissolve_sdf_body(self.scene.active_body_nid);
                        } else {
                            self.toast("Select a child body or enter a scope to dissolve");
                        }
                    }
                }
                // Ctrl+G: group / layer
                if layer_grp {
                    if self.modeling_context == ModelingContext::Sdf {
                        self.group_selected_into_body(None);
                    }
                }
                // Ctrl+I (ACT_INSTANCE / ACT_POLY_INVERT_SEL share default chord) — exclusive:
                //   · Poly mesh EDIT (Vertex/Edge/Face) → invert component selection
                //   · otherwise → create instance. Do NOT require WorkMode::Object — SDF create
                //     auto-enters Vertex (create_tool/creation), so Object-only made Ctrl+I a
                //     silent no-op right after placing a shape (the common case). Sculpt toasts.
                // Never both in one press.
                if instance_key || poly_invert_key {
                    let poly_edit = self.modeling_context == ModelingContext::Polygonal
                        && self.poly_work_mode != PolyWorkMode::Object;
                    if poly_edit {
                        crate::ops::poly::invert_selection::run(self);
                    } else if self.tool.work_mode == WorkMode::Sculpt {
                        // P3b/P4: Ctrl+I inverts protect-mask when present (dense or sparse).
                        let region = self.sculpt.voxel.as_mut().and_then(|v| {
                            if !v.grid.has_mask() { return None; }
                            v.grid.invert_mask();
                            v.grid.take_dirty().map(|d| [d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]])
                        });
                        if let Some(region) = region {
                            self.voxel_mark_chunks_in_region(region);
                            self.mark_dirty(DirtyFlags::RENDER);
                            self.toast("Mask inverted");
                        } else if self.sculpt.voxel.is_some() {
                            self.toast("No mask to invert — paint a mask first (Mask paint)");
                        } else {
                            self.toast("Exit sculpt mode to create an instance");
                        }
                    } else {
                        crate::ops::instance::create::run(self);
                    }
                }
                // N: new SDF body at root level
                if !any_text
                    && !ctx.wants_keyboard_input()
                    && ctx.input(|i| i.key_pressed(egui::Key::N) && !i.modifiers.command && !i.modifiers.shift && !i.modifiers.alt)
                {
                    if (self.modeling_context == ModelingContext::Sdf
                        || self.modeling_context == ModelingContext::None)
                        && self.tool.current.is_idle()
                    {
                        self.create_new_sdf_body();
                    }
                }
                // F key: utility pie (Focus Camera / Isolate) — handled below with other pies
                // Polyline editor: A = select all profile points (takes precedence over the global
                // scene select-all when a Polyline prim's point editor is active).
                if curve_select_all && self.tool.current.is_idle() && self.curve_edit_prim_index().is_some() {
                    self.curve_pt_select_all();
                }
                else if a_key && self.modeling_context==ModelingContext::Polygonal && self.poly_work_mode!=PolyWorkMode::Object {
                    self.poly_select_all_toggle();
                }
                else if a_key && self.modeling_context==ModelingContext::Polygonal && self.poly_work_mode==PolyWorkMode::Object {
                    // Select all scene-graph nodes in poly Object mode
                    let root = self.scene.scene_graph.root();
                    let children: Vec<_> = self.scene.scene_graph.children(root).to_vec();
                    let all_selected = !children.is_empty() && children.iter().all(|&c| {
                        self.sel.usel.selected.contains(&SceneItemId::Node(c))
                            || self.scene.scene_graph.get(c).map(|n| !n.visible).unwrap_or(true)
                    });
                    if all_selected {
                        // All visible already selected — deselect
                        self.remove_from_selection(|id| matches!(id, SceneItemId::Node(_)));
                    } else {
                        // Select ALL visible children, not just the first
                        for &nid in &children {
                            if self.scene.scene_graph.get(nid).map(|n| n.visible).unwrap_or(false) {
                                self.sel.usel.selected.insert(SceneItemId::Node(nid));
                            }
                        }
                        if self.sel.usel.active.is_none() {
                            if let Some(&first) = children.first() {
                                self.sel.usel.active = Some(SceneItemId::Node(first));
                            }
                        }
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                else if a_key&&self.tool.work_mode==WorkMode::Object&&self.modeling_context!=ModelingContext::Polygonal&&!self.tool.current.is_create(){
                    // INTENTIONAL: additive select-all (prims + optional first node), conditional active — doesn't fit canonical writers
                    for p in &self.scene.primitives {
                        if p.visible && !p.locked {
                            self.sel.usel.selected.insert(SceneItemId::Prim(p.prim_uid));
                        }
                    }
                    if self.sel.usel.active.is_none() {
                        if let Some(p) = self.scene.primitives.first() {
                            self.sel.usel.active = Some(SceneItemId::Prim(p.prim_uid));
                        }
                    }
                    // Also select all visible scene graph nodes
                    {
                        let root = self.scene.scene_graph.root();
                        let children: Vec<_> = self.scene.scene_graph.children(root).to_vec();
                        for &nid in &children {
                            if self.scene.scene_graph.get(nid).map(|n| n.visible).unwrap_or(false) {
                                self.sel.usel.selected.insert(SceneItemId::Node(nid));
                            }
                        }
                        if self.sel.usel.active.is_none() {
                            if let Some(&first) = children.first() {
                                self.sel.usel.active = Some(SceneItemId::Node(first));
                            }
                        }
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Alt+A = deselect all — EXCEPT in poly edit, where Alt+A is the Align /
                // Flatten pie (Blender MACHIN3-style Top/Right/Bottom/Left). Same chord.
                let poly_edit_owns_alt_a = self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode != PolyWorkMode::Object;
                if alt_a && !poly_edit_owns_alt_a && !self.ui.align_pie.open {
                    if self.modeling_context == ModelingContext::Polygonal {
                        self.poly_sel.selected_vertices.clear();
                        self.poly_sel.selected_edges.clear();
                        self.poly_sel.selected_faces.clear();
                    }
                    self.clear_selection_unified();
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                // Guard: don't change camera view when 1/2/3 are used for poly mode switching
                let in_poly_edit = self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object;
                if v1 && !in_poly_edit {self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,0.0,0.0,0.0);self.mark_dirty(DirtyFlags::RENDER);}
                if v3 && !in_poly_edit {self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,-std::f32::consts::FRAC_PI_2,0.0,0.0);self.mark_dirty(DirtyFlags::RENDER);}
                if v7 && !in_poly_edit {self.viewport.camera.orbit=Quat::from_euler(EulerRot::YXZ,0.0,-std::f32::consts::FRAC_PI_2,0.0);self.mark_dirty(DirtyFlags::RENDER);}
                // Ortho/perspective toggle (user choice — clears temp Alt+MMB ortho)
                if ortho_tog {
                    self.set_user_projection_ortho(!self.render.ortho);
                }
                // Space = drop tool / clear sub-selection / exit edit mode / deselect
                // (ACT_CMD_BAR / ACT_CANCEL_DESELECT — not the agent command bar).
                // Same-frame outliner hit (panel rects) — not 1-frame-lag ol_pointer_over alone.
                // (Linear-array Space is handled earlier via cancel_modal_transform.)
                // Spine exclusive tool handles Space/Enter in its own modal branch above.
                // Knife shape mid-drag: Space pans the rect/circle (viewport path) — do NOT drop.
                let knife_shape_space_move = self.poly_tool == PolyTool::Knife
                    && self.knife_shape_kind != 0
                    && self.knife_shape_anchor.is_some();
                if space
                    && !knife_shape_space_move
                    && !self.ui2_pointer_over_outliner(ctx)
                    && !self
                        .tool
                        .current
                        .as_transform()
                        .map_or(false, |t| t.shift_duplicate)
                {
                    if self.drop_tool() {
                        // drop_tool handled something (cancel create, modal, etc.)
                    } else if in_poly_edit {
                        // In poly edit mode: first press clears sub-element selection,
                        // second press (nothing selected) exits to object mode.
                        let has_sub = !self.poly_sel.selected_vertices.is_empty()
                            || !self.poly_sel.selected_edges.is_empty()
                            || !self.poly_sel.selected_faces.is_empty();
                        if has_sub {
                            self.poly_sel.selected_vertices.clear();
                            self.poly_sel.selected_edges.clear();
                            self.poly_sel.selected_faces.clear();
                        } else {
                            self.poly_switch_work_mode(PolyWorkMode::Object);
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    } else {
                        // Nothing active to drop — deselect all
                        self.deselect_all();
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
            }
        }
    }

}
