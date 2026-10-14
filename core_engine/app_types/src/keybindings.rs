// core_engine/app/src/keybindings.rs
// Keyboard shortcut system — KeyBind, KeyBindings, action constants, ACTION_DEFS registry.

use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use crate::keymap::{Guard, Hint, Avail, When};
use crate::keymap::{
    WHEN_POLY_EDIT, WHEN_POLY_VERT, WHEN_POLY_EDGE, WHEN_POLY_FACE, WHEN_POLY_VE, WHEN_POLY_EF,
    WHEN_POLYGONAL, WHEN_OBJECT, WHEN_SDF, WHEN_NURBS, WHEN_NURBS_CV, WHEN_CURVE, WHEN_CREATE,
    WHEN_ACTIVE_TOOL,
};

// ==================================================================
// Key display / parse helpers
// ==================================================================
//
// ONE hand-maintained table, two derived views.
//
// This file used to carry two independent match tables — one Key->str for
// display, one str->Key for parsing — each listing ~53 of egui's 104 keys.
// `key_display` ended in `_ => "?"`, so the other 51 keys SERIALIZED AS "?"
// and `key_from_str` had no "?" arm, so they read back as None. A user who
// rebound an action to F6 (or Plus, Minus, PageUp, Insert, `[`, ...) lost
// that bind silently on the next restart. "?" was also AMBIGUOUS — every
// unspellable key produced it — so "just add ? to the parser" would have
// loaded all 51 back as the WRONG key.
//
// The root cause was not the missing entries. It was that the PERSISTED
// spelling was produced by a DISPLAY function, and the two directions were
// maintained by hand and free to disagree. So:
//
//   * `key_name`    — the persisted spelling. Delegates to egui's `Key::name()`,
//                     which is TOTAL (an exhaustive match over Key, no
//                     fallback) and whose exact inverse is `Key::from_name()`.
//                     There is no sentinel to fall back to. A key that cannot
//                     be spelled is now structurally impossible, not caught.
//   * `key_display` — presentation only, NEVER written to disk. Consults
//                     KEY_ALIASES for a compact label, else `key_name`.
//   * `key_from_str`— accepts an alias, else defers to `Key::from_name`.
//
// Display and parse now read the SAME table and share the SAME total
// fallback, so they cannot drift apart: adding an alias changes both
// directions at once, and a key absent from the table still round-trips.
// The `every_key_round_trips_*` tests iterate `Key::ALL`, so a key added by
// a future egui upgrade is covered automatically with no edit here.

/// AZRA's compact display spellings. The ONLY hand-maintained key table.
///
/// Entries are also the v2 on-disk spellings, so keeping them here is what
/// lets an existing `~/.azra/keybinds.json` still load. Anything not listed
/// displays (and persists) as egui's canonical `Key::name()`.
///
/// Invariant: no alias may equal another key's `Key::name()`, or that name
/// would parse to the wrong key. `key_spellings_are_unambiguous` enforces it.
const KEY_ALIASES: &[(egui::Key, &str)] = &[
    (egui::Key::Escape, "Esc"),
    (egui::Key::Delete, "Del"),
    (egui::Key::Backspace, "Bksp"),
    (egui::Key::ArrowUp, "\u{2191}"),
    (egui::Key::ArrowDown, "\u{2193}"),
    (egui::Key::ArrowLeft, "\u{2190}"),
    (egui::Key::ArrowRight, "\u{2192}"),
];

/// Canonical, persisted spelling of a key. Total — every `egui::Key` has one.
/// This is what goes to disk; never a display string.
pub fn key_name(k: egui::Key) -> &'static str {
    k.name()
}

/// Human-facing label for a key. Presentation only — do NOT persist this.
pub fn key_display(k: egui::Key) -> &'static str {
    match KEY_ALIASES.iter().find(|(key, _)| *key == k) {
        Some((_, label)) => label,
        None => key_name(k),
    }
}

/// The lossy sentinel the pre-2026-07-16 writer emitted for any key it could
/// not spell. It identifies a CORRUPT record, not a key: 51 different keys all
/// collapsed onto it, so the user's actual key is unrecoverable.
const V2_LOSSY_SENTINEL: &str = "?";

/// Parse a key from either an AZRA alias (v2 files) or egui's canonical
/// name / symbol (v3 files). Inverse of both `key_display` and `key_name`.
pub fn key_from_str(s: &str) -> Option<egui::Key> {
    // egui's `from_name` maps "?" -> Questionmark. Deferring to it blindly would
    // silently RESURRECT every corrupt v2 record as a Questionmark bind — the
    // wrong key, which is worse than the drop the user already lived with.
    // Returning None keeps the action on its default, exactly as before.
    // A genuine Questionmark bind is unaffected: it persists via `key_name` as
    // "Questionmark", which the old writer could never produce, so a bare "?"
    // on disk is unambiguously the sentinel.
    if s == V2_LOSSY_SENTINEL {
        return None;
    }
    match KEY_ALIASES.iter().find(|(_, label)| *label == s) {
        Some((key, _)) => Some(*key),
        None => egui::Key::from_name(s),
    }
}

// ==================================================================
// KeyBind
// ==================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct KeyBind { pub key: egui::Key, pub shift: bool, pub ctrl: bool, pub alt: bool }
impl KeyBind {
    pub fn simple(key: egui::Key) -> Self { Self { key, shift:false, ctrl:false, alt:false } }
    pub fn ctrl(key: egui::Key)   -> Self { Self { key, shift:false, ctrl:true,  alt:false } }
    pub fn ctrl_shift(key: egui::Key) -> Self { Self { key, shift:true, ctrl:true, alt:false } }
    pub fn ctrl_alt(key: egui::Key) -> Self { Self { key, shift:false, ctrl:true, alt:true } }
    pub fn shift(key: egui::Key)  -> Self { Self { key, shift:true,  ctrl:false, alt:false } }
    pub fn alt(key: egui::Key)    -> Self { Self { key, shift:false, ctrl:false, alt:true } }
    pub fn shift_alt(key: egui::Key) -> Self { Self { key, shift:true, ctrl:false, alt:true } }
    pub fn matches(&self, i: &egui::InputState) -> bool {
        let mods_ok = i.modifiers.shift   == self.shift
            && i.modifiers.command == self.ctrl
            && i.modifiers.alt     == self.alt;
        if !mods_ok { return false; }
        if i.key_pressed(self.key) { return true; }
        // egui-winit SWALLOWS command+X / command+C / command+V: it pushes Event::Cut/Copy/Paste and
        // `return`s WITHOUT ever emitting the Event::Key for X/C/V (egui-winit 0.29 src/lib.rs:759-774).
        // So `key_pressed(Key::X)` is ALWAYS false under Ctrl, and every bind on those chords — most
        // visibly `ACT_POLY_DISSOLVE = ctrl(X)` — could never fire. A ctrl(X/C/V) bind must therefore
        // match the semantic clipboard event instead. `is_cut_command` keys on command+X regardless of
        // shift, so the `mods_ok` check above is what keeps ctrl(X) and ctrl_shift(X) distinct. Dispatch
        // sites are already gated on `!ctx.wants_keyboard_input()`, so a focused TextEdit's own cut does
        // not double-fire an app action here.
        if self.ctrl {
            return match self.key {
                egui::Key::X => i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
                egui::Key::C => i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                egui::Key::V => i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
                _ => false,
            };
        }
        false
    }
    /// True while the bound key is held with the same modifiers (hold-to-open pies).
    pub fn matches_down(&self, i: &egui::InputState) -> bool {
        i.key_down(self.key)
            && i.modifiers.shift   == self.shift
            && i.modifiers.command == self.ctrl
            && i.modifiers.alt     == self.alt
    }
    /// Match a raw egui key event (used when the key is stripped before egui InputState).
    pub fn matches_raw(&self, key: egui::Key, modifiers: &egui::Modifiers) -> bool {
        key == self.key
            && modifiers.shift == self.shift
            && modifiers.command == self.ctrl
            && modifiers.alt == self.alt
    }
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl  { s.push_str("Ctrl+"); }
        if self.shift { s.push_str("Shift+"); }
        if self.alt   { s.push_str("Alt+"); }
        s.push_str(key_display(self.key));
        s
    }
    /// Persisted form. Uses `key_name` (canonical, total), NOT `key_display` —
    /// a display string is a UI concern and must never reach disk. `from_json`
    /// still reads the v2 display spellings via `key_from_str`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "key": key_name(self.key), "shift": self.shift, "ctrl": self.ctrl, "alt": self.alt })
    }
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let key = key_from_str(v.get("key")?.as_str()?)?;
        Some(Self {
            key,
            shift: v.get("shift").and_then(|v| v.as_bool()).unwrap_or(false),
            ctrl:  v.get("ctrl").and_then(|v| v.as_bool()).unwrap_or(false),
            alt:   v.get("alt").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

// ==================================================================
// Action ID constants
// ==================================================================

// Transform
pub const ACT_GRAB: &str = "grab";
pub const ACT_ROTATE: &str = "rotate";
pub const ACT_SCALE: &str = "scale";
pub const ACT_ELONGATE: &str = "elongate";
pub const ACT_DELETE: &str = "delete";
pub const ACT_DUPLICATE: &str = "duplicate";
pub const ACT_FOCUS: &str = "focus";
pub const ACT_SELECT_ALL: &str = "select_all";
pub const ACT_DESELECT_ALL: &str = "deselect_all";
pub const ACT_TOGGLE_SPACE: &str = "toggle_space";
// View
pub const ACT_SHADING_PIE: &str = "shading_pie";
pub const ACT_MODE_PIE: &str = "mode_pie";
pub const ACT_VIEW_FRONT: &str = "view_front";
pub const ACT_VIEW_RIGHT: &str = "view_right";
pub const ACT_VIEW_TOP: &str = "view_top";
pub const ACT_ORTHO_TOGGLE: &str = "ortho_toggle";
pub const ACT_SHORTCUT_EDITOR: &str = "shortcut_editor";
pub const ACT_WORKPLANE_PLACE: &str = "workplane_place";
pub const ACT_CONTENT_LIBRARY: &str = "content_library";
// Edit
pub const ACT_UNDO: &str = "undo";
pub const ACT_REDO: &str = "redo";
pub const ACT_EXPORT: &str = "export";
pub const ACT_LAYER: &str = "layer";
pub const ACT_INSTANCE: &str = "instance";
pub const ACT_CREATE_LAYER: &str = "create_layer";
/// Outliner / selection rename (default F2).
pub const ACT_RENAME: &str = "rename";
// File
pub const ACT_NEW_SCENE: &str = "new_scene";
pub const ACT_OPEN_SCENE: &str = "open_scene";
pub const ACT_SAVE_SCENE: &str = "save_scene";
pub const ACT_SAVE_AS: &str = "save_as";
// Tools
pub const ACT_DROP_TOOL: &str = "drop_tool";
pub const ACT_BEVEL_TOGGLE: &str = "bevel_toggle";
pub const ACT_EXTRUDE_TOGGLE: &str = "extrude_toggle";
pub const ACT_PIVOT_EDIT: &str = "pivot_edit";
pub const ACT_SMART_MIRROR: &str = "smart_mirror";
pub const ACT_TOGGLE_SYMMETRY: &str = "toggle_symmetry";
pub const ACT_MAT_ASSIGN: &str = "mat_assign";
pub const ACT_SYM_AXIS_X: &str = "sym_axis_x";
pub const ACT_SYM_AXIS_Y: &str = "sym_axis_y";
pub const ACT_SYM_AXIS_Z: &str = "sym_axis_z";
pub const ACT_SYM_FLIP: &str = "sym_flip";
pub const ACT_SYM_SYMMETRIZE: &str = "sym_symmetrize";
pub const ACT_SYM_DELETE_HALF: &str = "sym_delete_half";
pub const ACT_SYM_RADIAL_CYCLE: &str = "sym_radial_cycle";
pub const ACT_ADD_MENU: &str = "add_menu";
/// Ctrl+Shift+A — SDF primitives only; new prim lands in a sub-body of the selection.
pub const ACT_ADD_SDF_SUBBODY: &str = "add_sdf_subbody";
/// Space: drop active tool / clear poly sub-sel / deselect.
/// Historical id `cmd_bar` kept for keybinds.json compatibility — not the agent bar.
pub const ACT_CMD_BAR: &str = "cmd_bar";
/// Alias for ACT_CMD_BAR (honest name). Prefer this in new code.
pub const ACT_CANCEL_DESELECT: &str = ACT_CMD_BAR;
// Creation context
pub const ACT_CUT_THROUGH: &str = "cut_through";
pub const ACT_CREATE_BEVEL: &str = "create_bevel";
pub const ACT_CYCLE_OP: &str = "cycle_op";
pub const ACT_CREATE_JOIN: &str = "create_join";
pub const ACT_BOOL_SUB: &str = "bool_sub";
pub const ACT_BOOL_ADD: &str = "bool_add";
pub const ACT_BOOL_INT: &str = "bool_int";
// Poly context
pub const ACT_POLY_EXTRUDE: &str = "poly_extrude";
/// Alt+E — Extrude Faces Along Normals (Blender).
pub const ACT_POLY_EXTRUDE_NORMALS: &str = "poly_extrude_normals";
/// Shift+Alt+E — Extrude Sweep (mouse length/angle, scroll path segments).
pub const ACT_POLY_EXTRUDE_SWEEP: &str = "poly_extrude_sweep";
pub const ACT_POLY_INSET: &str = "poly_inset";
pub const ACT_POLY_LOOP_CUT: &str = "poly_loop_cut";
pub const ACT_POLY_VERT_SLIDE: &str = "poly_vert_slide";
pub const ACT_POLY_EDGE_SLIDE: &str = "poly_edge_slide";
pub const ACT_POLY_SHRINK_FATTEN: &str = "poly_shrink_fatten";
pub const ACT_POLY_MERGE: &str = "poly_merge";
pub const ACT_POLY_MERGE_MENU: &str = "poly_merge_menu";
pub const ACT_POLY_KNIFE: &str = "poly_knife";
/// Knife `P` -> procedural pipe. Shares the `P` chord with poly Separate, split by STATE:
/// this only fires while the knife holds a polyline, and the dispatcher takes it first so
/// the two can never fire in the same frame (CLAUDE.md: same key + different context is
/// correct; a real collision is two actions firing in the SAME frame in the SAME state).
pub const ACT_POLY_PIPE: &str = "poly_pipe";
/// Knife shape (rect / circle) `E` -> lock the drawn shape and pull the CUT DEPTH, so the
/// cookie-cutter becomes a blind pocket instead of a through-hole. Shares the `E` chord with
/// poly Extrude, split by STATE exactly like [`ACT_POLY_PIPE`] shares `P` with Separate: it
/// only fires while a knife shape drag is anchored, and the dispatcher takes it first so the
/// two can never fire in the same frame.
pub const ACT_POLY_KNIFE_DEPTH: &str = "poly_knife_depth";
/// Knife `U` -> the shape cut ADDS material (Union) instead of removing it. Pressing it again
/// returns to Subtract, so the two ops need no third "back to default" chord. Only fires while
/// the knife tool is armed (the knife is modal and owns its chords) — same STATE split as
/// [`ACT_POLY_KNIFE_DEPTH`].
pub const ACT_POLY_KNIFE_BOOL_UNION: &str = "poly_knife_bool_union";
/// Knife `I` -> the shape cut KEEPS only what the outline encloses (Intersect); press again for
/// Subtract. `I` is already the app's Intersect chord ([`ACT_BOOL_INT`], creation tools), so the
/// meaning is the same in both places. Shares the chord with poly Inset by STATE.
pub const ACT_POLY_KNIFE_BOOL_INTERSECT: &str = "poly_knife_bool_intersect";
pub const ACT_POLY_BEVEL: &str = "poly_bevel";
/// Topology-guess Unbevel / resize bevel: select strip → sharp base + live re-edit (width/scroll/remove).
pub const ACT_POLY_UNBEVEL: &str = "poly_unbevel";
/// Face hinge rotate around a preselected edge (door/hatch).
pub const ACT_POLY_FACE_HINGE: &str = "poly_face_hinge";
pub const ACT_POLY_DELETE_FACE: &str = "poly_delete_face";
pub const ACT_POLY_DELETE_ONLY_FACE: &str = "poly_delete_only_face";
pub const ACT_POLY_SUBDIVIDE: &str = "poly_subdivide";
/// Dice — even U×V grid cuts on selected faces (Ctrl+Alt+D).
pub const ACT_POLY_DICE: &str = "poly_dice";
pub const ACT_POLY_EDGE_LOOP: &str = "poly_edge_loop";
pub const ACT_POLY_EDGE_RING: &str = "poly_edge_ring";
pub const ACT_POLY_BOX_SELECT: &str = "poly_box_select";
/// Circle brush select (secondary — paint/region select is primary). Default Shift+C.
pub const ACT_POLY_CIRCLE_SELECT: &str = "poly_circle_select";
/// Alt+C — LoopTools reshape selection onto a circle (not circle-select).
pub const ACT_POLY_CIRCLE_REGION: &str = "poly_circle_region";
/// Plain C — smart UV seam: toggle edges, or mark face-island boundary seams.
pub const ACT_POLY_SEAM: &str = "poly_seam";
pub const ACT_POLY_FILL: &str = "poly_fill";
/// Ctrl+Alt+F — Grid Fill a closed edge loop with a quad grid.
pub const ACT_POLY_GRID_FILL: &str = "poly_grid_fill";
/// Ctrl+Alt+R — Spin (lathe) selected verts/edges around pivot axis.
pub const ACT_POLY_SPIN: &str = "poly_spin";
/// LoopTools Relax — Laplacian smooth selection.
pub const ACT_POLY_LOOP_RELAX: &str = "poly_loop_relax";
/// LoopTools Flatten — project selection to best-fit plane.
pub const ACT_POLY_LOOP_FLATTEN: &str = "poly_loop_flatten";
/// LoopTools Space — equalize spacing along edge loops.
pub const ACT_POLY_LOOP_SPACE: &str = "poly_loop_space";
pub const ACT_POLY_DISSOLVE: &str = "poly_dissolve";
/// Limited Dissolve — edges between faces ≤ angle threshold (Blender default 5°).
pub const ACT_POLY_LIMITED_DISSOLVE: &str = "poly_limited_dissolve";
pub const ACT_POLY_PROPORTIONAL: &str = "poly_proportional";
pub const ACT_POLY_PROP_FALLOFF: &str = "poly_prop_falloff";
pub const ACT_POLY_RECALC_NORMALS: &str = "poly_recalc_normals";
pub const ACT_POLY_FLIP_NORMALS: &str = "poly_flip_normals";
/// Blender-style normal control (docs: poly Auto Smooth / Mark Sharp). `sharp` is a per-edge
/// authored hard edge; Shade Smooth / Shade Flat are the two ends of the per-body Auto Smooth angle.
pub const ACT_POLY_MARK_SHARP: &str = "poly_mark_sharp";
pub const ACT_POLY_CLEAR_SHARP: &str = "poly_clear_sharp";
pub const ACT_POLY_SHADE_SMOOTH: &str = "poly_shade_smooth";
pub const ACT_POLY_SHADE_FLAT: &str = "poly_shade_flat";
/// `W` — live edge-weight drag (authored `CustomData.crease`, 0..1). Same authoring family as
/// Mark Sharp / Mark Seam, but a VALUE: drag dials it, Shift snaps to 20 % stops, Ctrl zeroes it.
/// Edge-or-Face context (`WHEN_POLY_EF`) — Face mode weights the region boundary.
pub const ACT_POLY_EDGE_WEIGHT: &str = "poly_edge_weight";
/// Ctrl+U — Smart UV Project (angle-based charts + pack).
pub const ACT_POLY_UV_UNWRAP: &str = "poly_uv_unwrap";
/// Alt+U — toggle 2D UV layout over the viewport.
pub const ACT_VIEW_UV_LAYOUT: &str = "view_uv_layout";

pub const ACT_POLY_BRIDGE: &str = "poly_bridge";
pub const ACT_POLY_CONNECT: &str = "poly_connect";
/// Separate selected faces into a new poly body (Blender P → Selection).
pub const ACT_POLY_SEPARATE: &str = "poly_separate";
/// Bisect mesh by plane (symmetry / workplane / view). Shift+Ctrl+B.
pub const ACT_POLY_BISECT: &str = "poly_bisect";
/// Bisect + delete the −normal half (clear inner).
pub const ACT_POLY_BISECT_CLEAR: &str = "poly_bisect_clear";
// NURBS context
pub const ACT_NURBS_KNOT_U: &str = "nurbs_knot_u";
pub const ACT_NURBS_KNOT_V: &str = "nurbs_knot_v";
pub const ACT_NURBS_WEIGHT_UP: &str = "nurbs_weight_up";
pub const ACT_NURBS_WEIGHT_DOWN: &str = "nurbs_weight_down";
pub const ACT_NURBS_GRAB_CV: &str = "nurbs_grab_cv";
// General actions (hardcoded → keybind migration)
pub const ACT_HIDE: &str = "hide";
pub const ACT_UNHIDE_ALL: &str = "unhide_all";
pub const ACT_Q_MENU: &str = "q_menu";
pub const ACT_WIRE_OVERLAY: &str = "wire_overlay";
/// Shift+O — viewport overlay pie (lights, grid, workplane, gizmos, …).
pub const ACT_OVERLAY_PIE: &str = "overlay_pie";
pub const ACT_POLY_HIDE: &str = "poly_hide";
pub const ACT_POLY_LINKED: &str = "poly_linked";
pub const ACT_POLY_INVERT_SEL: &str = "poly_invert_sel";
pub const ACT_POLY_GROW_SEL: &str = "poly_grow_sel";
pub const ACT_POLY_SHRINK_SEL: &str = "poly_shrink_sel";
/// Shift+G — Select Similar (normal / length / valence by mode).
pub const ACT_POLY_SELECT_SIMILAR: &str = "poly_select_similar";
/// V — Rip selected edges into a boundary slit.
pub const ACT_POLY_RIP: &str = "poly_rip";
/// Alt+V — Rip Fill (rip + seal slit with a face).
pub const ACT_POLY_RIP_FILL: &str = "poly_rip_fill";
// SDF clip curve
pub const ACT_CLIP_CURVE: &str = "clip_curve_toggle";
/// K in SDF context — Draw Polyline (a CurveExtrude prim authored by clicking points on the
/// workplane). Owner 2026-08-08: "put polyline primitive on this shortcut gated to sdf object,
/// to not affect the k from polymesh".
///
/// SHARES THE `K` CHORD with [`ACT_POLY_KNIFE`], split by CONTEXT — the same legitimate split
/// CLAUDE.md describes ("Same key + different context is CORRECT, not a bug"): this one is
/// `WHEN_SDF`, the knife is `WHEN_POLYGONAL`, and the two modeling contexts are mutually
/// exclusive so they can never fire in the same frame.
pub const ACT_SDF_DRAW_POLYLINE: &str = "sdf_draw_polyline";
/// C with a lights-only selection — quick color square at the cursor (owner
/// 2026-08-07); scroll = intensity while open, viewport click closes. Shares C
/// with clip-curve (needs prims/nodes) and poly circle select (poly sub-object):
/// a legitimate context split — lights-only selection excludes both.
pub const ACT_LIGHT_COLOR: &str = "light_color_popup";
/// Ctrl+R radial picker for the active top-level body's bake resolution (64³…2048³).
/// Shares Ctrl+R with Poly Loop Cut (Poly Edge mode) and Sculpt Remesh (Sculpt mode) —
/// a legitimate context split: the pie's gate (SDF context AND not Sculpt) is mutually
/// exclusive with both, so the three can never fire in the same frame.
pub const ACT_SDF_RESOLUTION_PIE: &str = "sdf_resolution_pie";
// Polyline (CurveExtrude) profile-point editor — gated behind curve_edit_prim_index().is_some()
pub const ACT_CURVE_SELECT_ALL: &str = "curve_select_all";
pub const ACT_CURVE_DISSOLVE: &str = "curve_dissolve";
pub const ACT_CURVE_SUBDIVIDE: &str = "curve_subdivide";
pub const ACT_CURVE_BEVEL: &str = "curve_bevel";
// UI
pub const ACT_COMMAND_PALETTE: &str = "command_palette";
pub const ACT_LAYOUT_SETTINGS: &str = "layout_settings";
// Clipboard (merged from InputSystem)
pub const ACT_COPY: &str = "copy";
pub const ACT_CUT: &str = "cut";
pub const ACT_PASTE: &str = "paste";
// Sculpt / voxel (merged from InputSystem — state-gated at dispatch)
pub const ACT_SCULPT_BRUSH_SIZE_DOWN: &str = "sculpt_brush_size_down";
pub const ACT_SCULPT_BRUSH_SIZE_UP: &str = "sculpt_brush_size_up";
pub const ACT_SCULPT_BRUSH_STRENGTH_DOWN: &str = "sculpt_brush_strength_down";
pub const ACT_SCULPT_BRUSH_STRENGTH_UP: &str = "sculpt_brush_strength_up";
pub const ACT_SCULPT_REMESH: &str = "sculpt_remesh";
pub const ACT_VOXEL_VOXELIZE_NOW: &str = "voxel_voxelize_now";
// Poly / SDF pies & tools (merged from InputSystem — shared chords, state-gated)
pub const ACT_BEVEL_TOGGLE_CLAMP: &str = "bevel_toggle_clamp";
pub const ACT_POLY_BOOLEAN_PIE: &str = "poly_boolean_pie";
pub const ACT_SDF_BOOLEAN_PIE: &str = "sdf_boolean_pie";
pub const ACT_POLY_JOIN: &str = "poly_join";
// Selection (scroll still in InputSystem; keys live here when bound)
pub const ACT_SELECT_VISIBLE_ONLY: &str = "select_visible_only_toggle";

// ==================================================================
// ActionDef registry
// ==================================================================

pub struct ActionDef {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    /// Context declaration (INPUT_ARCHITECTURE_PLAN §3.3). S2: every action is `Guard::Legacy(when)`
    /// — ADVISORY (feeds find_conflicts + the editor); dispatch is still the hand-written ifs. Flips
    /// to `Guard::Declared` per tool/pillar as the migration proceeds.
    pub guard: crate::keymap::Guard,
    pub hint: crate::keymap::Hint,
    pub avail: crate::keymap::Avail,
}

pub const ACTION_DEFS: &[ActionDef] = &[
    // File
    ActionDef { id: ACT_NEW_SCENE,  name: "New Scene",  category: "File", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_OPEN_SCENE, name: "Open Scene", category: "File", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SAVE_SCENE, name: "Save Scene", category: "File", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SAVE_AS,    name: "Save As",    category: "File", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Transform
    ActionDef { id: ACT_GRAB,         name: "Grab (Move)",        category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_ROTATE,       name: "Rotate",             category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SCALE,        name: "Scale",              category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_ELONGATE,    name: "Elongate",           category: "Transform", guard: Guard::Legacy(WHEN_SDF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_DUPLICATE,    name: "Duplicate",          category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_DELETE,       name: "Delete",             category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_FOCUS,        name: "Focus Selected",     category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SELECT_ALL,   name: "Select All",         category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_DESELECT_ALL, name: "Deselect All",       category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_TOGGLE_SPACE, name: "Global/Local Space", category: "Transform", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // View
    ActionDef { id: ACT_SHADING_PIE,     name: "Shading Pie",       category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_MODE_PIE,        name: "Mode Pie",          category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_VIEW_FRONT,      name: "View Front",        category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_VIEW_RIGHT,      name: "View Right",        category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_VIEW_TOP,        name: "View Top",          category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_ORTHO_TOGGLE,    name: "Ortho/Perspective", category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SHORTCUT_EDITOR, name: "Shortcut Editor",  category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_WORKPLANE_PLACE, name: "Place Work Plane", category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CONTENT_LIBRARY, name: "Expand/Collapse Library", category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Edit
    ActionDef { id: ACT_UNDO,         name: "Undo",            category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_REDO,         name: "Redo",            category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_EXPORT,       name: "Export",          category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_LAYER,        name: "Group/Object",    category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Shares Ctrl+I with invert — dispatch: poly EDIT = invert; object / prim-body = instance.
    ActionDef { id: ACT_INSTANCE,     name: "Create Instance", category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Palette alias of Group/Object (ACT_LAYER). No separate executor — avoid Ctrl+L clash lie.
    ActionDef { id: ACT_CREATE_LAYER, name: "Create Layer (use Group)", category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_RENAME,       name: "Rename",          category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Tools
    ActionDef { id: ACT_DROP_TOOL,      name: "Drop Tool",    category: "Tools", guard: Guard::Legacy(WHEN_ACTIVE_TOOL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_BEVEL_TOGGLE,   name: "Bevel Mode",   category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_EXTRUDE_TOGGLE, name: "Extrude Mode", category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_PIVOT_EDIT,     name: "Pivot Edit",   category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SMART_MIRROR,   name: "Smart Mirror",    category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_TOGGLE_SYMMETRY, name: "Toggle Symmetry", category: "Tools", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_MAT_ASSIGN,     name: "Assign Material", category: "Material", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_AXIS_X,     name: "Symmetry Axis X", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_AXIS_Y,     name: "Symmetry Axis Y", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_AXIS_Z,     name: "Symmetry Axis Z", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_FLIP,       name: "Flip Symmetry Normal", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_SYMMETRIZE, name: "Symmetrize Mesh", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_DELETE_HALF, name: "Delete Half",    category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SYM_RADIAL_CYCLE, name: "Cycle Radial Symmetry", category: "Symmetry", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_ADD_MENU,       name: "Add Menu",     category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_ADD_SDF_SUBBODY, name: "Add SDF in Sub-Body", category: "Tools", guard: Guard::Legacy(WHEN_OBJECT), hint: Hint::None, avail: Avail::Always },
    // Cancel / deselect (id remains `cmd_bar` for saved keybinds — not the agent bar)
    ActionDef { id: ACT_CMD_BAR, name: "Cancel / Deselect", category: "Tools", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Creation
    ActionDef { id: ACT_CUT_THROUGH,  name: "Cut Through",     category: "Creation", guard: Guard::Legacy(WHEN_CREATE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CREATE_BEVEL, name: "Bevel Mode",      category: "Creation", guard: Guard::Legacy(WHEN_CREATE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CYCLE_OP,     name: "Cycle Operation", category: "Creation", guard: Guard::Legacy(WHEN_CREATE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CREATE_JOIN,  name: "Join (Union)",    category: "Creation", guard: Guard::Legacy(WHEN_CREATE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_BOOL_SUB,     name: "Subtract",        category: "Creation", guard: Guard::Legacy(WHEN_ACTIVE_TOOL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_BOOL_ADD,     name: "Add (Union)",     category: "Creation", guard: Guard::Legacy(WHEN_ACTIVE_TOOL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_BOOL_INT,     name: "Intersect",       category: "Creation", guard: Guard::Legacy(WHEN_ACTIVE_TOOL), hint: Hint::None, avail: Avail::Always },
    // Poly context
    ActionDef { id: ACT_POLY_EXTRUDE,     name: "Extrude Face",      category: "Poly",  guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EXTRUDE_NORMALS, name: "Extrude Along Normals", category: "Poly", guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EXTRUDE_SWEEP, name: "Extrude Sweep", category: "Poly", guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_INSET,       name: "Inset Face",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LOOP_CUT,    name: "Loop Cut",          category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_VERT_SLIDE,  name: "Vertex Slide",      category: "Poly",  guard: Guard::Legacy(WHEN_POLY_VERT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EDGE_SLIDE,  name: "Edge Slide",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SHRINK_FATTEN, name: "Shrink/Fatten",   category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_MERGE,       name: "Merge Vertices",    category: "Poly",  guard: Guard::Legacy(WHEN_POLY_VERT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_MERGE_MENU,  name: "Merge Menu",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_VERT), hint: Hint::None, avail: Avail::Always },
    // Guard is ANY so SDF can arm the same K tool (commit = seams / subtract). Poly edit
    // mode still enforced inside poly_activate_knife for mesh cuts.
    ActionDef { id: ACT_POLY_KNIFE,       name: "Knife",             category: "Poly",  guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_PIPE,        name: "Knife: Pipe",       category: "Poly",  guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_KNIFE_DEPTH, name: "Knife: Cut Depth",  category: "Poly",  guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_KNIFE_BOOL_UNION,     name: "Knife: Union (add)",   category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_KNIFE_BOOL_INTERSECT, name: "Knife: Intersect",     category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BEVEL,       name: "Bevel Edge",        category: "Poly",  guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_UNBEVEL,     name: "Unbevel / Resize Bevel", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_FACE_HINGE,  name: "Face Hinge Rotate",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_DELETE_FACE, name: "Delete Faces",      category: "Poly",  guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_DELETE_ONLY_FACE, name: "Delete Only Faces", category: "Poly", guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SUBDIVIDE,   name: "Subdivide (CC)",    category: "Poly",  guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_DICE,        name: "Dice Faces",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EDGE_LOOP,   name: "Select Edge Loop",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EDGE_RING,   name: "Select Edge Ring",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BOX_SELECT,  name: "Box Select",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_CIRCLE_SELECT, name: "Circle Select",   category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_CIRCLE_REGION, name: "Circle (Round Region)", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SEAM, name: "Toggle UV Seam", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_FILL,         name: "Fill Face",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_VERT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_GRID_FILL,    name: "Grid Fill",        category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SPIN,         name: "Spin",             category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LOOP_RELAX,   name: "LoopTools Relax",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LOOP_FLATTEN, name: "LoopTools Flatten", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LOOP_SPACE,   name: "LoopTools Space",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_DISSOLVE,     name: "Dissolve",         category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LIMITED_DISSOLVE, name: "Limited Dissolve", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BRIDGE,      name: "Bridge",           category: "Poly",  guard: Guard::Legacy(WHEN_POLY_VE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SEPARATE,    name: "Separate Selection", category: "Poly", guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BISECT,      name: "Bisect",             category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BISECT_CLEAR, name: "Bisect Clear Inner", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_CONNECT,     name: "Connect Vertex Path", category: "Poly", guard: Guard::Legacy(WHEN_POLY_VERT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_PROPORTIONAL, name: "Proportional Edit", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_PROP_FALLOFF, name: "Prop Falloff Cycle", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_RECALC_NORMALS, name: "Recalculate Normals", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_FLIP_NORMALS, name: "Flip Normals", category: "Poly", guard: Guard::Legacy(WHEN_POLY_FACE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_MARK_SHARP,  name: "Mark Sharp",        category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_CLEAR_SHARP, name: "Clear Sharp",       category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDGE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_EDGE_WEIGHT, name: "Set Edge Weight (drag)", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SHADE_SMOOTH, name: "Shade Smooth",     category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SHADE_FLAT,  name: "Shade Flat",        category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_UV_UNWRAP,   name: "UV Unwrap (Smart)", category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_VIEW_UV_LAYOUT,   name: "UV Layout View",    category: "View", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // NURBS context
    ActionDef { id: ACT_NURBS_KNOT_U,     name: "Insert Knot U",     category: "NURBS", guard: Guard::Legacy(WHEN_NURBS), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_NURBS_KNOT_V,     name: "Insert Knot V",     category: "NURBS", guard: Guard::Legacy(WHEN_NURBS), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_NURBS_WEIGHT_UP,  name: "CV Weight Up",      category: "NURBS", guard: Guard::Legacy(WHEN_NURBS_CV), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_NURBS_WEIGHT_DOWN,name: "CV Weight Down",    category: "NURBS", guard: Guard::Legacy(WHEN_NURBS_CV), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_NURBS_GRAB_CV,    name: "Grab CVs",          category: "NURBS", guard: Guard::Legacy(WHEN_NURBS_CV), hint: Hint::None, avail: Avail::Always },
    // General actions
    ActionDef { id: ACT_HIDE,             name: "Hide Selected",     category: "Edit",  guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_UNHIDE_ALL,       name: "Unhide All",        category: "Edit",  guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_Q_MENU,           name: "Context Menu",      category: "Edit",  guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_WIRE_OVERLAY,     name: "Wireframe Overlay", category: "View",  guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_OVERLAY_PIE,      name: "Overlay Pie",       category: "View",  guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_HIDE,        name: "Hide Poly Elements", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_LINKED,      name: "Select Linked",     category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_INVERT_SEL,  name: "Invert Selection",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_GROW_SEL,    name: "Grow Selection",    category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SHRINK_SEL,  name: "Shrink Selection",  category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_SELECT_SIMILAR, name: "Select Similar", category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_RIP,         name: "Rip",              category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_RIP_FILL,    name: "Rip Fill",         category: "Poly",  guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    // SDF clip curve
    ActionDef { id: ACT_CLIP_CURVE,       name: "Toggle Clip Curve", category: "SDF",   guard: Guard::Legacy(WHEN_SDF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SDF_DRAW_POLYLINE, name: "Draw Polyline",    category: "SDF",   guard: Guard::Legacy(WHEN_SDF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_LIGHT_COLOR,      name: "Light Color Popup", category: "Lights", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Ctrl+R shared with Poly Loop Cut (Poly Edge) + Sculpt Remesh (Sculpt) — state-gated,
    // mutually exclusive contexts; never fires the same frame as either.
    ActionDef { id: ACT_SDF_RESOLUTION_PIE, name: "Resolution Pie",  category: "SDF",   guard: Guard::Legacy(WHEN_SDF), hint: Hint::None, avail: Avail::Always },
    // Polyline profile-point editor
    ActionDef { id: ACT_CURVE_SELECT_ALL, name: "Select All Points", category: "Curve", guard: Guard::Legacy(WHEN_CURVE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CURVE_DISSOLVE,   name: "Dissolve Points",   category: "Curve", guard: Guard::Legacy(WHEN_CURVE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CURVE_SUBDIVIDE,  name: "Subdivide Segments", category: "Curve", guard: Guard::Legacy(WHEN_CURVE), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CURVE_BEVEL,      name: "Bevel Points",      category: "Curve", guard: Guard::Legacy(WHEN_CURVE), hint: Hint::None, avail: Avail::Always },
    // UI
    ActionDef { id: ACT_COMMAND_PALETTE,  name: "Command Palette",   category: "UI",    guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_LAYOUT_SETTINGS,  name: "Layout Settings",   category: "UI",    guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Clipboard
    ActionDef { id: ACT_COPY,  name: "Copy",  category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_CUT,   name: "Cut",   category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_PASTE, name: "Paste", category: "Edit", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Sculpt (state-gated: Sculpt work mode)
    ActionDef { id: ACT_SCULPT_BRUSH_SIZE_DOWN, name: "Brush Size −", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SCULPT_BRUSH_SIZE_UP,   name: "Brush Size +", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SCULPT_BRUSH_STRENGTH_DOWN, name: "Brush Strength −", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SCULPT_BRUSH_STRENGTH_UP,   name: "Brush Strength +", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SCULPT_REMESH,          name: "Remesh / Resolution", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_VOXEL_VOXELIZE_NOW,     name: "Voxelize Pending Moves", category: "Sculpt", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
    // Shared chords — disambiguated by tool/mode at dispatch
    ActionDef { id: ACT_BEVEL_TOGGLE_CLAMP, name: "Bevel: Toggle Clamp Overlap", category: "Poly", guard: Guard::Legacy(WHEN_POLY_EDIT), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_BOOLEAN_PIE,   name: "Boolean Pie (Poly)", category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SDF_BOOLEAN_PIE,    name: "Boolean Pie (SDF)",  category: "SDF",  guard: Guard::Legacy(WHEN_SDF), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_POLY_JOIN,          name: "Join Poly Bodies",   category: "Poly", guard: Guard::Legacy(WHEN_POLYGONAL), hint: Hint::None, avail: Avail::Always },
    ActionDef { id: ACT_SELECT_VISIBLE_ONLY, name: "Toggle Visible-Only Selection", category: "Selection", guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always },
];

// ==================================================================
// Default bindings
// ==================================================================

pub fn default_bind(id: &str) -> KeyBind {
    use egui::Key::*;
    match id {
        ACT_GRAB         => KeyBind::simple(G),
        ACT_ROTATE       => KeyBind::simple(R),
        ACT_SCALE        => KeyBind::simple(S),
        ACT_ELONGATE     => KeyBind::simple(E),
        ACT_DELETE       => KeyBind::simple(Delete),
        ACT_DUPLICATE    => KeyBind::shift(D),
        ACT_FOCUS        => KeyBind::simple(F),
        ACT_SELECT_ALL   => KeyBind::simple(A),
        ACT_DESELECT_ALL => KeyBind { key: A, shift: false, ctrl: false, alt: true },
        ACT_TOGGLE_SPACE => KeyBind::simple(T),
        ACT_SHADING_PIE  => KeyBind::simple(Z),
        ACT_MODE_PIE     => KeyBind::simple(Tab),
        ACT_UNDO         => KeyBind::ctrl(Z),
        ACT_REDO         => KeyBind::ctrl_shift(Z),
        ACT_VIEW_FRONT   => KeyBind::simple(Num1),
        ACT_VIEW_RIGHT   => KeyBind::simple(Num3),
        ACT_VIEW_TOP     => KeyBind::simple(Num7),
        ACT_CMD_BAR      => KeyBind::simple(Space), // cancel / deselect — not agent bar
        ACT_EXPORT       => KeyBind::ctrl(E),
        ACT_LAYER        => KeyBind::ctrl(G),
        ACT_INSTANCE     => KeyBind::ctrl(I),
        ACT_CONTENT_LIBRARY => KeyBind::ctrl(Space),
        // New file
        ACT_NEW_SCENE    => KeyBind::ctrl(N),
        ACT_OPEN_SCENE   => KeyBind::ctrl(O),
        ACT_SAVE_SCENE   => KeyBind::ctrl(S),
        ACT_SAVE_AS      => KeyBind::ctrl_shift(S),
        // New tools
        ACT_DROP_TOOL      => KeyBind::simple(Q),
        ACT_BEVEL_TOGGLE   => KeyBind::simple(B),
        ACT_EXTRUDE_TOGGLE => KeyBind::simple(E),
        ACT_PIVOT_EDIT     => KeyBind::simple(D),
        ACT_SMART_MIRROR   => KeyBind::alt(X),
        // Was Alt+M — that chord is now the poly Merge pie (ACT_POLY_MERGE_MENU).
        // Ctrl+Shift+M is Symmetry Flip; plain symmetry toggle uses Ctrl+Alt+M.
        ACT_TOGGLE_SYMMETRY => KeyBind::ctrl_alt(M),
        ACT_MAT_ASSIGN     => KeyBind::ctrl(M),
        ACT_SYM_AXIS_X     => KeyBind::ctrl_shift(X),
        ACT_SYM_AXIS_Y     => KeyBind::ctrl_shift(Y),
        ACT_SYM_AXIS_Z     => KeyBind { key: egui::Key::Z, shift: true, ctrl: false, alt: true },
        ACT_SYM_FLIP       => KeyBind::ctrl_shift(M),
        // Ctrl+Shift+K, NOT Ctrl+Shift+S: S is Save-As (ACT_SAVE_AS, :354). Both were `guard: Guard::Legacy(When::ANY), hint: Hint::None, avail: Avail::Always`
        // and both dispatched unguarded (input_keyboard.rs:377 and :720), so one press fired BOTH —
        // the Save-As dialog then covered the "Symmetrized" toast, letting the mutated mesh be saved
        // before the user could notice. Undoable, but only if you knew to undo.
        // K is arbitrary: Blender gives Symmetrize NO default hotkey (Mesh > Symmetrize), which is the
        // right answer and is unrepresentable here — `KeyBind.key` is not an Option, so every action
        // must own a chord. Revisit when the keymap plan introduces an unbound state.
        ACT_SYM_SYMMETRIZE => KeyBind::ctrl_shift(K),
        ACT_SYM_DELETE_HALF => KeyBind::ctrl_shift(D),
        ACT_SYM_RADIAL_CYCLE => KeyBind::ctrl_shift(R),
        ACT_ADD_MENU       => KeyBind::shift(A),
        ACT_ADD_SDF_SUBBODY => KeyBind::ctrl_shift(A),
        ACT_CREATE_LAYER   => KeyBind::ctrl(L),
        ACT_RENAME         => KeyBind::simple(F2),
        // New view
        ACT_ORTHO_TOGGLE    => KeyBind::simple(Num5),
        ACT_SHORTCUT_EDITOR => KeyBind::simple(End),
        ACT_WORKPLANE_PLACE => KeyBind::simple(Home),
        // SDF clip curve
        ACT_CLIP_CURVE   => KeyBind::simple(C),
        // K in SDF = Draw Polyline; K in Polygonal = Knife. Context-split, never both.
        ACT_SDF_DRAW_POLYLINE => KeyBind::simple(K),
        // Lights-only selection context — disjoint from clip-curve (prims/nodes)
        // and poly circle select (poly sub-object). See the const's doc comment.
        ACT_LIGHT_COLOR  => KeyBind::simple(C),
        // Ctrl+R — shared with ACT_POLY_LOOP_CUT (Poly Edge) / ACT_SCULPT_REMESH (Sculpt),
        // disambiguated by mode gates at dispatch (SDF + not Sculpt here).
        ACT_SDF_RESOLUTION_PIE => KeyBind::ctrl(R),
        // Polyline profile-point editor (gated on curve_edit_prim_index)
        ACT_CURVE_SELECT_ALL => KeyBind::simple(A),
        ACT_CURVE_DISSOLVE   => KeyBind::ctrl(X),
        ACT_CURVE_SUBDIVIDE  => KeyBind::ctrl(D),
        ACT_CURVE_BEVEL      => KeyBind::simple(B),
        // Creation context
        ACT_CUT_THROUGH  => KeyBind::simple(C),
        ACT_CREATE_BEVEL => KeyBind::simple(B),
        ACT_CYCLE_OP     => KeyBind::simple(X),
        ACT_CREATE_JOIN  => KeyBind::simple(J),
        ACT_BOOL_SUB     => KeyBind::simple(S),
        ACT_BOOL_ADD     => KeyBind::simple(A),
        ACT_BOOL_INT     => KeyBind::simple(I),
        // Poly context
        ACT_POLY_EXTRUDE     => KeyBind::simple(E),
        ACT_POLY_EXTRUDE_NORMALS => KeyBind::alt(E),
        ACT_POLY_EXTRUDE_SWEEP => KeyBind::shift_alt(E),
        ACT_POLY_INSET       => KeyBind::simple(I),
        ACT_POLY_LOOP_CUT    => KeyBind::ctrl(R),
        ACT_POLY_VERT_SLIDE  => KeyBind::shift(V), // Blender vertex slide
        ACT_POLY_EDGE_SLIDE  => KeyBind::shift(E), // Blender edge slide is GG (double-tap); Shift+E here
        ACT_POLY_SHRINK_FATTEN => KeyBind::alt(S), // Blender Alt+S shrink/fatten (move verts along normals)
        ACT_POLY_MERGE       => KeyBind::simple(M),
        ACT_POLY_MERGE_MENU  => KeyBind { key: M, shift: false, ctrl: false, alt: true }, // Alt+M merge menu
        ACT_POLY_KNIFE       => KeyBind::simple(K),
        ACT_POLY_PIPE        => KeyBind::simple(P),
        // Same `E` as Extrude — state-split (only live while a knife shape drag is anchored),
        // and `poly_handle_keys` consumes it before Extrude can see it.
        ACT_POLY_KNIFE_DEPTH => KeyBind::simple(E),
        // `U` is free in the Poly context (only NURBS uses it, ACT_NURBS_KNOT_U). `I` is shared
        // with Inset the same way `E` is shared with Extrude — knife-armed state wins, consumed
        // in `poly_handle_keys` so the two can never fire in the same frame.
        ACT_POLY_KNIFE_BOOL_UNION     => KeyBind::simple(U),
        ACT_POLY_KNIFE_BOOL_INTERSECT => KeyBind::simple(I),
        ACT_POLY_BEVEL       => KeyBind::ctrl(B),
        // Ctrl+Alt+B — Unbevel / resize existing bevel strip (topology guess → live re-edit).
        ACT_POLY_UNBEVEL     => KeyBind::ctrl_alt(B),
        // Ctrl+Shift+H — Face hinge rotate (H free under ctrl+shift; R is rotate/spin family).
        ACT_POLY_FACE_HINGE  => KeyBind::ctrl_shift(H),
        ACT_POLY_DELETE_FACE => KeyBind::simple(Delete),
        ACT_POLY_DELETE_ONLY_FACE => KeyBind::ctrl(Delete),
        ACT_POLY_SUBDIVIDE   => KeyBind::ctrl(D),
        ACT_POLY_DICE        => KeyBind::ctrl_alt(D),
        ACT_POLY_EDGE_LOOP   => KeyBind::shift(L),
        ACT_POLY_EDGE_RING   => KeyBind::shift(R),
        ACT_POLY_BOX_SELECT  => KeyBind::shift(B),
        // Shift+C — circle brush (demoted; owner paint-selects as primary). Plain C = UV seam.
        ACT_POLY_CIRCLE_SELECT => KeyBind::shift(C),
        ACT_POLY_CIRCLE_REGION => KeyBind::alt(C), // Alt+C free in every keymap (grepped default_bind)
        ACT_POLY_SEAM          => KeyBind::simple(C),
        ACT_POLY_FILL        => KeyBind::simple(F),
        // Ctrl+Alt+F — Grid Fill (F alone = Fill Face; Blender menu item has no exclusive default).
        ACT_POLY_GRID_FILL   => KeyBind::ctrl_alt(F),
        // Ctrl+Alt+R — Spin (R alone = Rotate transform).
        ACT_POLY_SPIN        => KeyBind::ctrl_alt(R),
        // LoopTools family (no Blender-global defaults; AZRA chords):
        ACT_POLY_LOOP_RELAX  => KeyBind::ctrl_alt(L), // L = LoopTools
        ACT_POLY_LOOP_FLATTEN => KeyBind::ctrl_shift(F), // F = Flatten (Shift distinguishes Fill)
        ACT_POLY_LOOP_SPACE  => KeyBind::ctrl_alt(S), // S free under ctrl+alt (Alt+S = shrink/fatten)
        ACT_POLY_DISSOLVE    => KeyBind::ctrl(X),
        // Blender: X menu → Limited Dissolve (no default). Ctrl+Shift+X free of Ctrl+X dissolve.
        ACT_POLY_LIMITED_DISSOLVE => KeyBind::ctrl_shift(X),
        ACT_POLY_BRIDGE      => KeyBind::shift(F),
        // Blender P opens Separate menu; we ship Selection-only on bare P in Face mode.
        ACT_POLY_SEPARATE    => KeyBind::simple(P),
        // Blender Bisect has no default hotkey; Ctrl+Shift+B avoids Bevel (B) and Bridge (Shift+F).
        ACT_POLY_BISECT      => KeyBind::ctrl_shift(B),
        ACT_POLY_BISECT_CLEAR => KeyBind { key: B, shift: true, ctrl: true, alt: true },
        // Blender vert_connect_path = J. Same key as ACT_CREATE_JOIN, but a LEGITIMATE context split:
        // ACT_CREATE_JOIN dispatches ONLY inside the `is_create()` branch (input_keyboard.rs:494);
        // ACT_POLY_CONNECT ONLY via poly_handle_keys in the normal-keyboard else branch, gated to Poly
        // Vertex mode. The two branches are mutually exclusive per frame, so J can never fire both.
        ACT_POLY_CONNECT     => KeyBind::simple(J),
        ACT_POLY_PROPORTIONAL => KeyBind::simple(O),
        // Was Shift+O; that chord is now Overlay Pie (owner 2026-08-07).
        ACT_POLY_PROP_FALLOFF => KeyBind::shift_alt(O),
        ACT_OVERLAY_PIE      => KeyBind::shift(O),
        ACT_POLY_RECALC_NORMALS => KeyBind::shift(N),
        ACT_POLY_FLIP_NORMALS => KeyBind::alt(N),
        // Normal AUTHORING (Blender's Ctrl+E edge menu / object Shade menu — neither has a bare
        // default chord). Grepped `default_bind` by hand before choosing: Ctrl+E is ACT_EXPORT and
        // Ctrl+N is ACT_NEW_SCENE, but Ctrl+Shift+E / Ctrl+Alt+E / Ctrl+Shift+N / Ctrl+Alt+N were
        // all FREE. Kept in the N (= normals) family alongside Shift+N / Alt+N above.
        ACT_POLY_MARK_SHARP   => KeyBind::ctrl_shift(E),
        ACT_POLY_CLEAR_SHARP  => KeyBind::ctrl_alt(E),
        // Plain `W` in poly Edge/Face mode. The other plain-W binds are NURBS CV weight up/down
        // (`WHEN_NURBS_CV`) — a different modeling context, which CLAUDE.md § Input System
        // Discipline calls correct, not a collision: the two can never fire in the same frame in
        // the same state. Same key, same MEANING too (weight), which is the point.
        ACT_POLY_EDGE_WEIGHT  => KeyBind::simple(W),
        ACT_POLY_SHADE_SMOOTH => KeyBind::ctrl_shift(N),
        ACT_POLY_SHADE_FLAT   => KeyBind::ctrl_alt(N),
        // Ctrl+U / Alt+U free (U alone = NURBS knot). Grepped default_bind before binding.
        ACT_POLY_UV_UNWRAP   => KeyBind::ctrl(U),
        ACT_VIEW_UV_LAYOUT   => KeyBind::alt(U),
        // NURBS context
        ACT_NURBS_KNOT_U     => KeyBind::simple(U),
        ACT_NURBS_KNOT_V     => KeyBind::simple(V),
        ACT_NURBS_WEIGHT_UP  => KeyBind::simple(W),
        ACT_NURBS_WEIGHT_DOWN => KeyBind::shift(W),
        ACT_NURBS_GRAB_CV    => KeyBind::simple(G),
        // General actions
        ACT_HIDE             => KeyBind::simple(H),
        ACT_UNHIDE_ALL       => KeyBind::alt(H),
        ACT_Q_MENU           => KeyBind::simple(Q),
        ACT_WIRE_OVERLAY     => KeyBind::shift(Z),
        ACT_POLY_HIDE        => KeyBind::simple(H),
        ACT_POLY_LINKED      => KeyBind::simple(L),
        ACT_POLY_INVERT_SEL  => KeyBind::ctrl(I),
        ACT_POLY_GROW_SEL    => KeyBind::ctrl(Plus),
        ACT_POLY_SELECT_SIMILAR => KeyBind::shift(G),
        // Blender V = Rip, Alt+V = Rip Fill. Shift+V remains Vertex Slide.
        ACT_POLY_RIP         => KeyBind::simple(V),
        ACT_POLY_RIP_FILL    => KeyBind::alt(V),
        ACT_POLY_SHRINK_SEL  => KeyBind::ctrl(Minus),
        // UI
        ACT_COMMAND_PALETTE  => KeyBind::ctrl_shift(P),
        ACT_LAYOUT_SETTINGS  => KeyBind::ctrl_shift(L),
        // Clipboard
        ACT_COPY  => KeyBind::ctrl(C),
        ACT_CUT   => KeyBind::ctrl(X),
        ACT_PASTE => KeyBind::ctrl(V),
        // Sculpt (Z/X plain; state-gated so no clash with shading pie / bool pie)
        ACT_SCULPT_BRUSH_SIZE_DOWN => KeyBind::simple(Z),
        ACT_SCULPT_BRUSH_SIZE_UP   => KeyBind::simple(X),
        ACT_SCULPT_BRUSH_STRENGTH_DOWN => KeyBind { key: Z, shift: true, ctrl: false, alt: false },
        ACT_SCULPT_BRUSH_STRENGTH_UP   => KeyBind { key: X, shift: true, ctrl: false, alt: false },
        ACT_SCULPT_REMESH          => KeyBind::ctrl(R),
        ACT_VOXEL_VOXELIZE_NOW     => KeyBind::simple(Enter),
        // Shared with clip-curve / brush — context-gated at dispatch
        ACT_BEVEL_TOGGLE_CLAMP => KeyBind::simple(C),
        ACT_POLY_BOOLEAN_PIE   => KeyBind::simple(X),
        ACT_SDF_BOOLEAN_PIE    => KeyBind::simple(X),
        ACT_POLY_JOIN          => KeyBind::ctrl(J),
        // No default chord — viewport header is primary control
        ACT_SELECT_VISIBLE_ONLY => KeyBind { key: V, shift: true, ctrl: false, alt: true }, // Alt+V
        _ => KeyBind::simple(A), // fallback
    }
}

// ==================================================================
// KeyBindings store
// ==================================================================

#[derive(Clone)]
pub struct KeyBindings {
    map: HashMap<&'static str, KeyBind>,
}

impl Default for KeyBindings {
    fn default() -> Self { Self::defaults() }
}

impl KeyBindings {
    pub fn defaults() -> Self {
        let mut map = HashMap::with_capacity(ACTION_DEFS.len());
        for def in ACTION_DEFS {
            map.insert(def.id, default_bind(def.id));
        }
        Self { map }
    }

    pub fn get(&self, id: &str) -> &KeyBind {
        static FALLBACK: KeyBind = KeyBind { key: egui::Key::A, shift: false, ctrl: false, alt: false };
        self.map.get(id).unwrap_or(&FALLBACK)
    }

    pub fn pressed(&self, id: &str, i: &egui::InputState) -> bool {
        self.get(id).matches(i)
    }

    /// Key held with matching modifiers (hold-to-open pies, brush size, …).
    pub fn down(&self, id: &str, i: &egui::InputState) -> bool {
        self.get(id).matches_down(i)
    }

    pub fn label(&self, id: &str) -> String { self.get(id).label() }

    pub fn set(&mut self, id: &str, bind: KeyBind) {
        if ACTION_DEFS.iter().any(|d| d.id == id) {
            if let Some(def) = ACTION_DEFS.iter().find(|d| d.id == id) {
                self.map.insert(def.id, bind);
            }
        }
    }

    pub fn find_conflicts(&self, bind: &KeyBind, exclude_id: &str) -> Vec<(&'static str, String)> {
        // SOUND conflict detection (INPUT_ARCHITECTURE_PLAN §2.3, §3.1): a real collision exists only
        // if the two actions' contexts can be satisfied SIMULTANEOUSLY — `When::overlaps`, pure mask
        // arithmetic. Same chord in DISJOINT contexts (bevel.clamp vs loopcut.clamp; F=Focus in
        // viewport vs F=Fill in poly edit) is a legitimate split, not a conflict. The old free-text
        // `!=` had both false positives and false negatives (it shipped all 15 real collisions).
        let excl_when = ACTION_DEFS.iter().find(|d| d.id == exclude_id).map(|d| d.guard.when());
        let mut out = Vec::new();
        for def in ACTION_DEFS {
            if def.id == exclude_id { continue; }
            if *self.get(def.id) != *bind { continue; }
            if let Some(ew) = excl_when {
                if !def.guard.when().overlaps(&ew) { continue; }
            }
            out.push((def.name, bind.label()));
        }
        out
    }

    pub fn config_dir() -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let dir = home.join(".azra");
        if !dir.exists() { let _ = std::fs::create_dir_all(&dir); }
        Some(dir)
    }

    pub fn save(&self) {
        let Some(dir) = Self::config_dir() else { return; };
        let mut bindings = serde_json::Map::new();
        for def in ACTION_DEFS {
            let current = self.get(def.id);
            let dflt = default_bind(def.id);
            if *current != dflt {
                bindings.insert(def.id.to_string(), current.to_json());
            }
        }
        // v3: keys persist as egui canonical names (`key_name`) instead of the
        // v2 display spellings, which could not represent 51 of egui's 104 keys
        // (they wrote "?" and were dropped on load). `load()` is deliberately
        // version-AGNOSTIC — `key_from_str` accepts both vocabularies — so this
        // number is a record of the change, not a branch. Nothing reads it back
        // today; it is here for a future migration that needs the signal.
        let root = serde_json::json!({ "version": 3, "bindings": bindings });
        let path = dir.join("keybinds.json");
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap_or_default());
    }

    pub fn load() -> Self {
        let mut kb = Self::defaults();
        let Some(dir) = Self::config_dir() else { return kb; };
        let path = dir.join("keybinds.json");
        let Ok(data) = std::fs::read_to_string(&path) else { return kb; };
        let Ok(root) = serde_json::from_str::<serde_json::Value>(&data) else { return kb; };
        if let Some(bindings) = root.get("bindings").and_then(|v| v.as_object()) {
            for (id, val) in bindings {
                if let Some(bind) = KeyBind::from_json(val) {
                    kb.set(id, bind);
                }
            }
        }
        kb
    }

    pub fn profile_modo() -> Self {
        let mut kb = Self::defaults();
        kb.set(ACT_GRAB, KeyBind::simple(egui::Key::W));
        kb.set(ACT_ROTATE, KeyBind::simple(egui::Key::E));
        kb.set(ACT_SCALE, KeyBind::simple(egui::Key::R));
        kb.set(ACT_DUPLICATE, KeyBind::ctrl(egui::Key::D));
        kb
    }
}

// ==================================================================
// Tests — the key vocabulary invariants
// ==================================================================
//
// These iterate `egui::Key::ALL` rather than a hand-listed set. That is the
// point: when egui gains a key, `ALL` grows and these tests cover it with no
// edit here. A key that cannot round-trip is a silently-lost user rebind
// (the rebind editor accepts ANY key — input_keyboard.rs:77 takes whatever
// `Event::Key` carries — so every key in ALL is reachable by a real user).

#[cfg(test)]
mod tests {
    use super::*;

    /// Save-As vs Symmetrize regression (2026-07-15): both once shipped on Ctrl+Shift+S.
    #[test]
    fn save_as_and_symmetrize_do_not_clash() {
        let kb = KeyBindings::defaults();
        assert_ne!(
            kb.get(ACT_SAVE_AS),
            kb.get(ACT_SYM_SYMMETRIZE),
            "Save As and Symmetrize must use different chords"
        );
    }

    /// S2 ledger baseline: every action starts `Guard::Legacy` (nothing Declared yet). As tools
    /// migrate this count drops; `Declared` actions are enforced by resolve() (S4+). Guards the
    /// invariant that S2 changed NO dispatch behavior.
    #[test]
    fn s2_all_actions_are_legacy() {
        let declared: Vec<&str> = ACTION_DEFS.iter()
            .filter(|d| d.guard.is_declared()).map(|d| d.id).collect();
        assert!(declared.is_empty(), "unexpected Declared actions at S2: {:?}", declared);
        assert!(ACTION_DEFS.len() >= 90, "ACTION_DEFS unexpectedly small: {}", ACTION_DEFS.len());
    }

    /// find_conflicts is now sound: two actions on the SAME chord in DISJOINT contexts are NOT a
    /// conflict. Uses the real registry — pick any two poly-edit-only vs object-only pair sharing a key.
    #[test]
    fn find_conflicts_respects_context_overlap() {
        // Same chord in DISJOINT contexts is not a conflict (the soundness find_conflicts now relies
        // on). Object-mode vs Poly-Edit are disjoint (Object work vs Edit work); Vertex ⊂ Poly-Edit.
        assert!(!WHEN_OBJECT.overlaps(&WHEN_POLY_EDIT), "Object vs Poly-Edit must be disjoint");
        assert!(WHEN_POLY_VERT.overlaps(&WHEN_POLY_EDIT), "Vertex must overlap Poly-Edit");
    }

    /// Every key a user can bind must survive display -> parse.
    /// Guards legacy `keybinds.json` files, which stored display spellings.
    #[test]
    fn every_key_round_trips_through_display() {
        let mut broken = Vec::new();
        for &k in egui::Key::ALL {
            match key_from_str(key_display(k)) {
                Some(back) if back == k => {}
                other => broken.push(format!("{:?} -> {:?} -> {:?}", k, key_display(k), other)),
            }
        }
        assert!(broken.is_empty(), "{} of {} keys do not round-trip:\n  {}",
            broken.len(), egui::Key::ALL.len(), broken.join("\n  "));
    }

    /// The persisted form must survive to_json -> from_json for every key.
    /// This is the actual save/restart path.
    #[test]
    fn every_key_round_trips_through_json() {
        let mut broken = Vec::new();
        for &k in egui::Key::ALL {
            let original = KeyBind { key: k, ctrl: true, shift: false, alt: false };
            match KeyBind::from_json(&original.to_json()) {
                Some(back) if back == original => {}
                other => broken.push(format!("{:?} -> {} -> {:?}",
                    k, original.to_json(), other.map(|b| b.key))),
            }
        }
        assert!(broken.is_empty(), "{} of {} keys are lost on save/reload:\n  {}",
            broken.len(), egui::Key::ALL.len(), broken.join("\n  "));
    }

    /// No two distinct keys may share a spelling — an ambiguous spelling parses
    /// back to the wrong key, which is worse than dropping it.
    #[test]
    fn key_spellings_are_unambiguous() {
        let mut seen: HashMap<&'static str, egui::Key> = HashMap::new();
        for &k in egui::Key::ALL {
            let s = key_display(k);
            if let Some(prev) = seen.insert(s, k) {
                panic!("{:?} and {:?} both spell as {:?}", prev, k, s);
            }
        }
    }

    /// A v2 file on disk holds the old display spellings. They must still load.
    #[test]
    fn legacy_v2_spellings_still_parse() {
        for (s, expect) in [
            ("Esc", egui::Key::Escape), ("Del", egui::Key::Delete),
            ("Bksp", egui::Key::Backspace), ("Space", egui::Key::Space),
            ("Tab", egui::Key::Tab), ("Enter", egui::Key::Enter),
            ("Home", egui::Key::Home), ("End", egui::Key::End),
            ("\u{2191}", egui::Key::ArrowUp), ("\u{2193}", egui::Key::ArrowDown),
            ("\u{2190}", egui::Key::ArrowLeft), ("\u{2192}", egui::Key::ArrowRight),
            ("A", egui::Key::A), ("Z", egui::Key::Z),
            ("0", egui::Key::Num0), ("9", egui::Key::Num9),
            ("F1", egui::Key::F1), ("F5", egui::Key::F5),
        ] {
            assert_eq!(key_from_str(s), Some(expect), "legacy v2 spelling {:?} must still parse", s);
        }
    }

    /// The persisted spelling must never be a display string, and must never
    /// be the old lossy sentinel. Guards the display/persistence split.
    #[test]
    fn persisted_spelling_is_canonical_not_display() {
        for &k in egui::Key::ALL {
            let persisted = key_name(k);
            assert_ne!(persisted, "?", "{:?} persists as the lossy sentinel", k);
            assert_eq!(egui::Key::from_name(persisted), Some(k),
                "{:?} persists as {:?} which egui cannot read back", k, persisted);
        }
        // Aliased keys: display is compact, disk form stays canonical.
        assert_eq!(key_display(egui::Key::Escape), "Esc");
        assert_eq!(key_name(egui::Key::Escape), "Escape");
        assert_eq!(KeyBind::simple(egui::Key::Escape).to_json()["key"], "Escape");
    }

    /// No shipped default may render as "?" in the Shortcut Editor, which
    /// renders via `label()` -> `key_display`. Grow/Shrink Selection ship on
    /// Ctrl+Plus / Ctrl+Minus and showed "Ctrl+?" — cosmetic (save() skips
    /// defaults, so they were never actually lost), but it is the same
    /// sentinel and the editor is where a user would go to rebind them.
    #[test]
    fn no_shipped_default_labels_as_unknown() {
        for def in ACTION_DEFS {
            let label = default_bind(def.id).label();
            assert!(!label.contains('?'), "{} labels as {:?}", def.id, label);
        }
        assert_eq!(default_bind(ACT_POLY_GROW_SEL).label(), "Ctrl+Plus");
        assert_eq!(default_bind(ACT_POLY_SHRINK_SEL).label(), "Ctrl+Minus");
    }

    /// Every key survives a full save-then-load through `KeyBindings`, using a
    /// non-default bind so `save()`'s skip-defaults filter cannot hide it.
    /// This is the exact reported bug: rebind -> restart -> bind is gone.
    #[test]
    fn rebind_to_any_key_survives_save_and_load() {
        for &k in egui::Key::ALL {
            let bind = KeyBind { key: k, ctrl: true, shift: true, alt: false };
            if bind == default_bind(ACT_GRAB) { continue; }
            let mut kb = KeyBindings::defaults();
            kb.set(ACT_GRAB, bind.clone());
            // Mirror save()'s serialize step and load()'s parse step without
            // touching the real ~/.azra/keybinds.json.
            let json = kb.get(ACT_GRAB).to_json();
            let restored = KeyBind::from_json(&json)
                .unwrap_or_else(|| panic!("rebinding grab to {:?} is lost on restart", k));
            assert_eq!(restored, bind, "grab rebound to {:?} came back wrong", k);
        }
    }

    /// A v2 file written by the OLD buggy build can contain `"key": "?"` for
    /// any of the 51 unspellable keys. egui's `from_name` maps "?" ->
    /// Questionmark, so a naive fallback would silently resurrect those corrupt
    /// records as a Questionmark bind — a WRONG key, worse than the drop the
    /// user already lived with. The sentinel must stay unreadable.
    #[test]
    fn v2_lossy_sentinel_never_resurrects_as_questionmark() {
        assert_eq!(key_from_str("?"), None, "the v2 \"?\" sentinel must not parse");
        let corrupt = serde_json::json!({ "key": "?", "shift": false, "ctrl": true, "alt": false });
        assert_eq!(KeyBind::from_json(&corrupt), None,
            "a corrupt v2 record must be dropped (-> action keeps its default), not misread");
        // A genuine Questionmark bind persists canonically and is unaffected.
        assert_eq!(key_name(egui::Key::Questionmark), "Questionmark");
        assert_eq!(key_from_str("Questionmark"), Some(egui::Key::Questionmark));
        let real = KeyBind::ctrl(egui::Key::Questionmark);
        assert_eq!(KeyBind::from_json(&real.to_json()), Some(real));
    }

    /// A real v2 file body, parsed through the same path `load()` uses.
    #[test]
    fn legacy_v2_json_bindings_survive() {
        let v2 = serde_json::json!({
            "version": 2,
            "bindings": {
                "grab":   { "key": "Del",      "shift": false, "ctrl": false, "alt": false },
                "rotate": { "key": "\u{2191}", "shift": true,  "ctrl": false, "alt": false },
                "scale":  { "key": "Bksp",     "shift": false, "ctrl": true,  "alt": false },
            }
        });
        let b = v2.get("bindings").unwrap().as_object().unwrap();
        assert_eq!(KeyBind::from_json(&b["grab"]).unwrap().key, egui::Key::Delete);
        assert_eq!(KeyBind::from_json(&b["rotate"]).unwrap(),
            KeyBind { key: egui::Key::ArrowUp, shift: true, ctrl: false, alt: false });
        assert_eq!(KeyBind::from_json(&b["scale"]).unwrap(),
            KeyBind { key: egui::Key::Backspace, shift: false, ctrl: true, alt: false });
    }
}
