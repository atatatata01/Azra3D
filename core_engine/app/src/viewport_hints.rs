// core_engine/app/src/viewport_hints.rs
// THE single source of truth for the contextual viewport hint row.
//
// Why this module exists: the hint row used to be built by `status_line_hint()` in ui_panels.rs, which
// returned a pre-joined String. Every consumer therefore got prose, not data, and the per-work-mode
// fallback rows advertised keys that a LIVE modal tool had already reassigned (e.g. the Face row's
// "G: Push/Pull" while an inset drag is running, where G actually means Group/Region). The owner's
// strategy is "hotkeys everywhere, discoverable via the hint row" — that only works if the row is
// contextual AND if there is exactly ONE place that decides what it says.
//
// `viewport_hints()` is that place. It returns structured (key, action) pairs so a future bottom bar can
// render chips/kbd elements from the SAME data the current single-line row uses — no second table to
// drift out of sync. `status_line_hint()` is now a thin join over it and keeps the row's exact current
// look/placement (viewport_overlays.rs paints it unchanged).
//
// INVARIANT: every key listed here is verified against the real dispatch site. A wrong hint is worse
// than none. When you add/rebind a key for a modal tool, update the matching arm here in the same task.
// Dispatch sites are cited per-arm below so the next dev can re-verify without spelunking.

use crate::{AzraApp, ModelingContext, WorkMode, PolyWorkMode, NurbsWorkMode, PolyTool};

/// Build a `(key, action)` row from a static table — keeps the arms below terse and uniform.
fn row(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect()
}

impl AzraApp {
    /// Contextual hints for the CURRENT state — THE single source of truth for the hint row.
    ///
    /// Priority: an ACTIVE modal tool owns the row (its keys are the ones that actually fire); only an
    /// idle viewport falls back to the work-mode row. This mirrors the real input precedence in
    /// `handle_keyboard` (input_keyboard.rs) / `handle_viewport_input` (input_viewport.rs), where the
    /// modal branches short-circuit before the generic per-context key handling.
    ///
    /// Returns `(key, action)` — e.g. `("Ctrl", "Snap")`. Renderers decide the separator/format.
    pub(crate) fn viewport_hints(&self) -> Vec<(String, String)> {
        // ── ACTIVE modal tools (tool.current) ──
        // These branches are ordered to mirror input dispatch; ActiveTool variants are mutually
        // exclusive, so only the poly_tool session checks further down depend on ordering.

        // create_tool.rs / draw_create.rs — footprint→extrude phases.
        if self.tool.current.is_create() {
            return row(&[("LMB Drag", "Footprint"), ("Release", "Extrude phase"),
                         ("Scroll", "Bevel"), ("RMB/Esc", "Cancel")]);
        }
        if self.tool.mode.is_create_armed() {
            return row(&[("LMB Drag", "Start creating shape"), ("RMB", "Cancel")]);
        }
        // input_keyboard.rs:400-418 — X/Y/Z axis, numeric buffer, Enter/LMB confirm.
        if self.tool.current.is_transform() {
            return row(&[("Drag", "Transform"), ("X/Y/Z", "Axis lock"),
                         ("Shift", "Slow"), ("Ctrl+Shift", "Fine"),
                         ("Ctrl", "Snap"), ("RMB/Esc", "Cancel"), ("Enter", "Confirm")]);
        }
        if self.tool.current.is_gizmo() {
            return row(&[("Drag", "Along axis"), ("Shift", "Slow"),
                         ("Ctrl+Shift", "Fine"), ("Ctrl", "Snap"),
                         ("Release", "Confirm")]);
        }
        // SDF bevel edge drag (ActiveTool::Bevel) — distinct from the poly bevel modal below.
        if self.tool.current.is_bevel() {
            return row(&[("Drag", "Radius ←fillet / chamfer→"), ("Ctrl+Drag", "Chamfer angle"),
                         ("Shift+Drag", "Profile"), ("RMB/Esc", "Cancel")]);
        }
        // SDF Bevel mode armed (ToolMode::Bevel) — edge select before drag.
        if self.tool.mode.is_bevel() {
            return row(&[("LMB", "Select edge"), ("Shift+LMB", "Add/remove"),
                         ("Alt+LMB", "Same group (top/bot/vert)"), ("Drag edge", "Bevel"),
                         ("Empty click", "Drop tool"), ("B/Esc", "Exit")]);
        }
        if self.tool.current.is_face_push_pull() {
            return row(&[("Drag", "Push/Pull"), ("Shift", "Slow"), ("Ctrl+Shift", "Fine"),
                         ("Ctrl", "Snap"), ("Alt", "Both faces"), ("Esc", "Cancel")]);
        }
        // Poly Extrude modal — input_keyboard.rs (Ctrl snap via poly_update_extrude_drag,
        // N = Region ⇄ Along Normals toggle, Enter confirm), input_viewport.rs (LMB confirm,
        // RMB cancel), tool_mode.rs:37 (Esc → cancel_operation → poly_cancel_extrude_drag).
        if self.tool.current.is_poly_extrude() {
            return row(&[("Drag", "Extrude offset"), ("Shift", "World axis lock"), ("Alt", "Free grab"),
                         ("N", "Along normals"), ("Ctrl", "Snap"), ("LMB/Enter", "Confirm"),
                         ("RMB/Esc", "Cancel")]);
        }
        if self.tool.current.is_poly_extrude_sweep() {
            return row(&[("Move", "Length+angle"), ("Scroll", "Path segments"), ("Type", "Lock angle"),
                         ("LMB", "Confirm+exit"), ("RMB/Esc", "Cancel")]);
        }
        // Poly Inset modal — input_keyboard.rs:622-637: G = Region/group, I = Individual (both
        // bare, no modifiers), Ctrl = snap (poly_update_inset_drag → is_snap_active), Enter confirm.
        // input_viewport.rs:136 (LMB confirm), :1720 (RMB cancel), tool_mode.rs:38 (Esc cancel).
        // NOTE: G here is Region/Group — not gizmo Move. Modal tools must own the hint row.
        if self.tool.current.is_poly_inset() {
            return row(&[("Drag", "Thickness"), ("Alt+Drag", "Depth"), ("G", "Region"), ("I", "Individual"),
                         ("Ctrl", "Snap"), ("LMB/Enter", "Confirm"), ("RMB/Esc", "Cancel")]);
        }
        // Offset Edge Loop (`I` in Edge mode) — one rail per adjacent n-gon; Shift mirrors both.
        if self.tool.current.is_poly_offset_loop() {
            return row(&[("Drag", "Offset"), ("Shift", "Both planes"),
                         ("LMB/Enter", "Confirm"), ("RMB/Esc", "Cancel")]);
        }
        // Poly Bevel modal — input_keyboard.rs:576-593: C = clamp (rebindable via
        // ACTION_BEVEL_TOGGLE_CLAMP, default C), SHIFT gates the profile-shape axis, Enter confirm.
        // input_viewport.rs:229-231 (scroll = segments), :95 (LMB confirm), :1717 (RMB cancel),
        // tool_mode.rs:40 (Esc cancel). Matches the tool's own start toast (poly_drag_tools.rs:926).
        if self.tool.current.is_poly_bevel() {
            return row(&[("Drag", "Width"), ("Shift+Drag", "Shape"), ("Scroll", "Segments"),
                         ("C", "Clamp"), ("LMB/Enter", "Confirm"), ("RMB/Esc", "Cancel")]);
        }
        // Loop Cut session — input_keyboard.rs:597-612: C = clamp (same rebindable action as bevel),
        // S = paired support loops, Enter confirm. input_viewport.rs (scroll = cut count; RMB confirms
        // — the documented Knife-style exception to the Tool Exit Convention; LMB CLICK confirms since
        // 2026-08-11, because the bare cursor now slides and no longer needs the button),
        // tool_mode.rs (Esc cancel).
        if self.tool.current.is_poly_loop_cut_slide() || self.poly_tool == PolyTool::LoopCut {
            return row(&[("Move", "Slide"), ("Scroll", "Cuts"), ("R", "Rotate dir"),
                         ("K", "Through-cut"), ("S", "Paired"), ("C", "Clamp"),
                         ("Tab", "Accept+mode"), ("Click/RMB/Enter", "Confirm"), ("Esc", "Cancel")]);
        }
        // Edge Weight modal (`W`) — poly_drag_tools/edge_weight.rs. Free cursor sets the strength;
        // typed digits are an exact percent; LMB click / Enter accept, RMB / Esc cancel.
        if self.tool.current.is_poly_edge_weight() {
            return row(&[("Move", "Strength"), ("0-9", "Exact %"), ("Shift", "20% steps"),
                         ("Ctrl", "Reset 0"), ("LMB/Enter", "Accept"), ("RMB/Esc", "Cancel")]);
        }
        // Poly Shrink/Fatten modal — input_keyboard.rs:639-642 (Ctrl snap, Enter confirm),
        // input_viewport.rs:141 (LMB confirm), :1721 (RMB cancel), tool_mode.rs:39 (Esc cancel).
        if self.tool.current.is_poly_shrink_fatten() {
            return row(&[("Drag", "Offset"), ("Ctrl", "Snap"),
                         ("LMB/Enter", "Confirm"), ("RMB/Esc", "Cancel")]);
        }
        // Vertex / Edge slide (poly/slide.rs) — pure position move; C toggles clamp.
        // Confirm: tool_mode.rs Enter / LMB; Esc cancel. Update: input_viewport while drag.
        if self.tool.current.is_poly_slide() {
            return row(&[("Drag", "Slide factor"), ("C", "Toggle clamp"),
                         ("LMB/Enter", "Confirm"), ("Esc", "Cancel")]);
        }
        // Paint select (LMB press→stroke): input_viewport paint arm. Scroll resizes brush.
        if self.tool.current.is_poly_paint_select() {
            return row(&[("Drag", "Paint select"), ("Scroll", "Brush size"),
                         ("Release", "Finish")]);
        }
        // Region select (RMB / Alt+RMB drag): input_viewport region arm.
        if self.tool.current.is_poly_region_select() {
            return row(&[("Drag", "Region"), ("Release", "Select"),
                         ("Alt", "Through (if started with Alt)")]);
        }
        if self.tool.current.is_selection_box() || self.tool.current.is_poly_box_select() {
            return row(&[("Drag", "Selection box"), ("Release", "Confirm")]);
        }
        if self.tool.current.is_poly_circle_select() {
            return row(&[("LMB", "Add to selection"), ("MMB", "Remove"),
                         ("Scroll", "Radius"), ("Esc", "Exit")]);
        }
        if self.tool.current.is_smart_mirror() {
            return row(&[("Drag", "Position mirror plane"), ("X/Y/Z", "Axis"),
                         ("Enter", "Confirm"), ("Esc", "Cancel")]);
        }
        if self.tool.current.is_pivot_edit() {
            return row(&[
                ("Drag / G", "Move pivot"),
                ("Ctrl", "Snap"),
                ("Enter", "Confirm"),
                ("Esc", "Cancel"),
            ]);
        }
        if self.tool.current.is_falloff_adjust() {
            return row(&[("Drag", "Adjust falloff"), ("Release", "Confirm")]);
        }
        // Poly Transform (G/R/S on sub-elements): X/Y/Z axis, Shift+X/Y/Z plane, Ctrl = grid/angle snap
        // (GRS D2 — was hardcoded snap_active:false), numeric buffer, LMB/Enter confirm.
        if self.tool.current.is_poly_transform() {
            return row(&[("Drag", "Transform"), ("X/Y/Z", "Axis"), ("Shift+X/Y/Z", "Plane"),
                         ("Ctrl", "Snap"), ("0-9", "Numeric"), ("LMB/Enter", "Confirm"), ("RMB/Esc", "Cancel")]);
        }
        if self.tool.current.is_nurbs_cv_move() {
            return row(&[("Drag", "Move CV"), ("Esc", "Cancel")]);
        }

        // ── Armed poly session tools (poly_tool, no ActiveTool yet) ──
        // Loop Cut before an edge is hovered: poly_loop_cut_session_tick (poly/loop_cut.rs:68-78)
        // auto-enters the slide session off the hovered/selected edge, so this is the pre-hover state.
        if self.poly_tool == PolyTool::LoopCut {
            return row(&[("Hover edge", "Preview ring"), ("Esc", "Cancel")]);
        }
        // Knife — input_viewport.rs:153 (LMB places a point), :1724 (RMB confirms),
        // input_keyboard.rs:651 (Enter confirms), :342-345 (Ctrl+Z pops the last point — tool-local
        // undo, not scene undo), tool_mode.rs:163-169 (Esc → drop_tool clears knife_points).
        if self.poly_tool == PolyTool::Knife {
            // The shape personalities (R/C) can run three booleans — advertise the two chords
            // that switch them, else U/I are dead keys nobody finds.
            return row(&[
                ("LMB", "Place point"),
                ("Ctrl/magnet", "Straight + angle"),
                ("U/I", "Union / Intersect"),
                ("Ctrl+Z", "Undo point"),
                ("RMB/Enter", "Confirm"),
                ("Esc", "Cancel"),
            ]);
        }

        // ── Idle: per-context work-mode rows ──
        // Contextual mouse: Action Center Automatic (T pie) reassigns Shift+RMB from workplane
        // place → temporary G/R/S pivot (input_viewport.rs). List it only in that mode.
        let ac_auto = self.tool.action_center == crate::ActionCenter::Automatic;
        let ac_elem = self.tool.action_center == crate::ActionCenter::Element;
        let mut hints = match self.modeling_context {
            ModelingContext::Sdf => {
                if self.tool.work_mode == WorkMode::Sculpt {
                    // Z/X, not "[/]" — OpenBracket/CloseBracket appear NOWHERE in the codebase, so this
                    // row advertised keys that do not exist. Real keys: input_keyboard.rs:23-31.
                    row(&[("LMB", "Sculpt stroke"), ("Ctrl", "Invert"), ("Shift", "Smooth"),
                          ("Z/X", "Brush size"), ("Tab", "Exit sculpt")])
                } else {
                    // X opens the boolean PIE (input_keyboard.rs:1238 sdf_bool_pie.open); the cycle it
                    // replaced is called "the old cycle" by the code itself (scene_actions.rs:68).
                    row(&[("LMB", "Select"), ("G", "Move"), ("R", "Rotate"), ("S", "Scale"),
                          ("X", "Boolean pie"), ("Shift+D", "Duplicate"), ("Del", "Delete")])
                }
            }
            ModelingContext::Polygonal => match self.poly_work_mode {
                PolyWorkMode::Object => row(&[("LMB", "Select"), ("G", "Move"), ("R", "Rotate"),
                                              ("S", "Scale"), ("Tab", "Edit mode"), ("Shift+D", "Duplicate")]),
                PolyWorkMode::Vertex => row(&[("LMB", "Select vertex"), ("Shift+LMB", "Multi"),
                                              ("G", "Move"), ("M", "Merge"), ("F", "Fill"),
                                              ("Tab", "Object mode")]),
                PolyWorkMode::Edge   => row(&[("LMB", "Select edge"), ("DblClick", "Loop"),
                                              ("Ctrl+R", "Loop Cut"), ("B", "Bevel"),
                                              ("Shift+E", "Edge Slide"), ("Tab", "Object mode")]),
                // G arms the Move gizmo (same as Vertex/Object) — NOT SDF Face "Push/Pull"
                // (input_keyboard G/R/S → gizmo_armed). Push/Pull is SDF-only.
                // Alt+C = ACT_POLY_CIRCLE_REGION.
                PolyWorkMode::Face   => row(&[("LMB", "Select face"), ("Shift+LMB", "Multi"),
                                              ("E", "Extrude"), ("I", "Inset"), ("G", "Move"),
                                              ("Alt+C", "Circle"), ("Tab", "Object mode")]),
                PolyWorkMode::Group  => row(&[("LMB", "Select polygroup"), ("Shift+LMB", "Add group"),
                                              ("E", "Extrude"), ("I", "Inset"), ("G", "Move"),
                                              ("Tab", "Object mode")]),
            },
            ModelingContext::Nurbs => match self.nurbs_work_mode {
                NurbsWorkMode::Object => row(&[("LMB", "Select"), ("G", "Move"), ("R", "Rotate"),
                                                     ("S", "Scale"), ("Tab", "Vertex/Edge/Face")]),
                NurbsWorkMode::Vertex => row(&[("LMB", "Select CV"), ("Shift+LMB", "Multi"),
                                                     ("G", "Move"), ("W", "Weight"), ("Tab", "Object mode")]),
                NurbsWorkMode::Edge   => row(&[("LMB", "Select edge chain"), ("Tab", "Object mode")]),
                NurbsWorkMode::Face   => row(&[("LMB", "Select surface"), ("Tab", "Object mode")]),
            },
            ModelingContext::Light        => row(&[("LMB", "Select"), ("G", "Move"),
                                                   ("Properties", "Adjust light settings")]),
            ModelingContext::Camera       => row(&[("LMB", "Select"), ("G", "Move"),
                                                   ("Properties", "Adjust camera settings")]),
            ModelingContext::ImportedMesh => row(&[("LMB", "Select"), ("G", "Move"),
                                                   ("Properties", "Convert to Poly")]),
            ModelingContext::None         => row(&[("LMB", "Select"), ("Shift+A", "Add object"),
                                                   ("F3", "Command palette"), ("MMB", "Orbit"),
                                                   ("Scroll", "Zoom")]),
        };
        if ac_auto {
            // Prepend so the contextual rebind is visible while Automatic is live.
            hints.insert(0, ("Shift+RMB".into(), "Place pivot (face/edge snap)".into()));
        } else if ac_elem {
            hints.insert(0, ("Shift+RMB".into(), "Pick Element target".into()));
        }
        hints
    }

    /// Context-sensitive status line hint for the viewport bottom — the single-line rendering of
    /// `viewport_hints()`. Kept as a `String` so viewport_overlays.rs paints the row exactly as before;
    /// new consumers (e.g. a chip-style bottom bar) should call `viewport_hints()` and format the pairs
    /// themselves rather than re-parsing this string.
    pub(crate) fn status_line_hint(&self) -> String {
        self.viewport_hints()
            .iter()
            .map(|(k, a)| format!("{}: {}", k, a))
            .collect::<Vec<_>>()
            .join(" | ")
    }
}
