//! Input System V1 — context-aware action dispatch.
//!
//! Every NEW keyboard/scroll/mouse action should be registered here.
//! Old actions in handle_keyboard() coexist and migrate gradually.
//!
//! Usage:
//!   1. Register: `input.register("my_action", "My Action", "Category");`
//!   2. Bind:     `input.bind_default(Key(G), ctrl, shift, Viewport, "my_action");`
//!   3. Dispatch: `if let Some(id) = input.resolve(...) { match id { ... } }`

use eframe::egui;

// ============================================================
// Context — where the input event happens
// ============================================================

/// Panel-level context determined by mouse hover position.
/// Start small — add Mode/Tool contexts when first needed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum InputContext {
    /// Fallback — matches if no more specific context does.
    Global,
    /// Mouse is over the 3D viewport.
    Viewport,
    /// Mouse is over the outliner panel.
    Outliner,
    /// Mouse is over the properties panel.
    Properties,
    /// Mouse is over the content library.
    Library,
}

// ============================================================
// Event — what input happened
// ============================================================

/// Input events that can be bound to actions.
/// Keys, scroll, and (future) mouse buttons.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum InputEvent {
    Key(egui::Key),
    ScrollUp,
    ScrollDown,
}

// ============================================================
// Action ID constants (stable string IDs for rebindable actions)
// ============================================================

/// Poly bevel "Clamp Overlap" toggle (C by default). Fired contextually inside the bevel modal.
pub(crate) const ACTION_BEVEL_TOGGLE_CLAMP: &str = "bevel_toggle_clamp";

/// Poly mesh boolean pie (X by default, hold-to-open). Fired contextually in the Polygonal
/// modeling context with an idle tool — never collides with the Sculpt-mode brush-size X,
/// the SDF boolean-pie X, or the transform-modal axis-constraint X (all state-gated).
pub(crate) const ACTION_POLY_BOOLEAN_PIE: &str = "poly_boolean_pie";

/// SDF boolean pie (X by default, hold-to-open). Fired contextually in the SDF modeling
/// context with an idle tool — replaces the old blind Union→Subtract→Intersect cycle.
/// Same shared-X disambiguation as ACTION_POLY_BOOLEAN_PIE (all state-gated).
pub(crate) const ACTION_SDF_BOOLEAN_PIE: &str = "sdf_boolean_pie";

/// Ctrl+J — Join selected poly bodies into the active one (Blender parity). Object-mode poly only.
pub(crate) const ACTION_POLY_JOIN: &str = "poly_join";

// Alt+C Circle-region was migrated to ACTION_DEFS (`ACT_POLY_CIRCLE_REGION`, keybindings.rs) — the
// deprecated InputSystem registration was removed with bug #6. See poly/key_dispatch.rs.

/// Modifier state for a binding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct InputMods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl InputMods {
    pub const NONE: Self = Self { ctrl: false, shift: false, alt: false };
    pub const CTRL: Self = Self { ctrl: true, shift: false, alt: false };
    pub const SHIFT: Self = Self { ctrl: false, shift: true, alt: false };
    pub const CTRL_SHIFT: Self = Self { ctrl: true, shift: true, alt: false };
    pub const ALT: Self = Self { ctrl: false, shift: false, alt: true };

    /// Build from egui's current input state.
    pub fn from_egui(ctx: &egui::Context) -> Self {
        ctx.input(|i| Self {
            ctrl: i.modifiers.command,
            shift: i.modifiers.shift,
            alt: i.modifiers.alt,
        })
    }
}

// ============================================================
// Action registry
// ============================================================

/// Metadata for a registered action.
#[derive(Clone, Debug)]
pub(crate) struct ActionEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
}

// ============================================================
// Binding — maps event+mods+context → action
// ============================================================

/// A single input binding.
#[derive(Clone, Debug)]
pub(crate) struct InputBinding {
    pub event: InputEvent,
    pub mods: InputMods,
    pub context: InputContext,
    pub action_id: &'static str,
    /// True if this is the default binding (not user-customized).
    pub is_default: bool,
}

// ============================================================
// InputSystem — the dispatcher
// ============================================================

/// Context-aware input dispatch system.
/// Register actions, bind them to events+contexts, resolve at runtime.
pub(crate) struct InputSystem {
    pub actions: Vec<ActionEntry>,
    pub bindings: Vec<InputBinding>,
    /// Mode-pie key events stripped in `raw_input_hook` (key = ACT_MODE_PIE bind).
    pub raw_mode_pie_events: Vec<egui::Event>,
    /// Physical hold of the Mode Pie key (from raw events — egui never sees it).
    pub mode_pie_held: bool,
    pub any_text_focused: bool,
}

impl Default for InputSystem {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            bindings: Vec::new(),
            raw_mode_pie_events: Vec::new(),
            mode_pie_held: false,
            any_text_focused: false,
        }
    }
}

impl InputSystem {
    /// Register a new action. Idempotent — skips if ID already exists.
    pub fn register(&mut self, id: &'static str, name: &'static str, category: &'static str) {
        if self.actions.iter().any(|a| a.id == id) { return; }
        self.actions.push(ActionEntry { id, name, category });
    }

    /// Add a default binding. Won't override user customizations.
    pub fn bind_default(
        &mut self,
        event: InputEvent,
        mods: InputMods,
        context: InputContext,
        action_id: &'static str,
    ) {
        // Don't duplicate
        let exists = self.bindings.iter().any(|b| {
            b.event == event && b.mods == mods && b.context == context
        });
        if exists { return; }
        self.bindings.push(InputBinding {
            event, mods, context, action_id, is_default: true,
        });
    }

    /// Resolve an input event in the given context.
    /// Checks the specific context first, then falls through to Global.
    /// Returns the action ID if a binding matches.
    pub fn resolve(
        &self,
        event: InputEvent,
        mods: InputMods,
        context: InputContext,
    ) -> Option<&'static str> {
        // Check specific context first
        if context != InputContext::Global {
            for b in &self.bindings {
                if b.event == event && b.mods == mods && b.context == context {
                    return Some(b.action_id);
                }
            }
        }
        // Fall through to Global
        for b in &self.bindings {
            if b.event == event && b.mods == mods && b.context == InputContext::Global {
                return Some(b.action_id);
            }
        }
        None
    }

    /// List all actions in a category (for UI display).
    pub fn actions_in_category(&self, category: &str) -> Vec<&ActionEntry> {
        self.actions.iter().filter(|a| a.category == category).collect()
    }

    /// Find the binding for an action in a given context (for UI display).
    pub fn binding_for(&self, action_id: &str, context: InputContext) -> Option<&InputBinding> {
        self.bindings.iter().find(|b| b.action_id == action_id && b.context == context)
    }

    /// All unique categories (for UI grouping).
    pub fn categories(&self) -> Vec<&'static str> {
        let mut cats: Vec<&str> = self.actions.iter().map(|a| a.category).collect();
        cats.sort_unstable();
        cats.dedup();
        cats
    }
}

// ============================================================
// Active context detection
// ============================================================

impl crate::FourthLayerApp {
    /// Determine which panel the mouse is currently over.
    pub(crate) fn active_input_context(&self, ctx: &egui::Context) -> InputContext {
        let mouse = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(egui::pos2(0.0, 0.0));

        // Check viewport first (largest area, most common)
        if self.viewport.viewport_rect.contains(mouse) {
            return InputContext::Viewport;
        }

        // Outliner is on the right side — check if mouse is in that region
        // (approximate: right 25% of screen, top half)
        let screen = ctx.screen_rect();
        if mouse.x > screen.width() * 0.75 && mouse.y < screen.height() * 0.5 {
            return InputContext::Outliner;
        }

        // Properties panel — right side, bottom half
        if mouse.x > screen.width() * 0.75 && mouse.y >= screen.height() * 0.5 {
            return InputContext::Properties;
        }

        // Library — bottom strip
        if mouse.y > screen.height() * 0.85 {
            return InputContext::Library;
        }

        InputContext::Global
    }

    /// Register all built-in actions and default bindings.
    /// Called once at startup.
    pub(crate) fn register_input_actions(&mut self) {
        let input = &mut self.input_system;

        // -- Hierarchy ordering --
        input.register("hierarchy_up", "Move Up in Hierarchy", "Hierarchy");
        input.register("hierarchy_down", "Move Down in Hierarchy", "Hierarchy");
        input.register("hierarchy_first", "Move to Top of Hierarchy", "Hierarchy");
        input.register("hierarchy_last", "Move to Bottom of Hierarchy", "Hierarchy");

        // Ctrl+Scroll in viewport = hierarchy reorder (when idle)
        input.bind_default(InputEvent::ScrollUp, InputMods::CTRL, InputContext::Global, "hierarchy_up");
        input.bind_default(InputEvent::ScrollDown, InputMods::CTRL, InputContext::Global, "hierarchy_down");
        input.bind_default(InputEvent::ScrollUp, InputMods::CTRL_SHIFT, InputContext::Global, "hierarchy_first");
        input.bind_default(InputEvent::ScrollDown, InputMods::CTRL_SHIFT, InputContext::Global, "hierarchy_last");

        // -- Clipboard --
        input.register("copy", "Copy Selection", "Edit");
        input.register("cut", "Cut Selection", "Edit");
        input.register("paste", "Paste", "Edit");

        input.bind_default(InputEvent::Key(egui::Key::C), InputMods::CTRL, InputContext::Global, "copy");
        input.bind_default(InputEvent::Key(egui::Key::X), InputMods::CTRL, InputContext::Global, "cut");
        input.bind_default(InputEvent::Key(egui::Key::V), InputMods::CTRL, InputContext::Global, "paste");

        // -- Clip curve --
        input.register("clip_curve_toggle", "Toggle Clip Curve", "SDF");
        input.bind_default(InputEvent::Key(egui::Key::C), InputMods::NONE, InputContext::Viewport, "clip_curve_toggle");

        // -- Voxel sculpt brush (rebindable; act only in Sculpt work mode) --
        input.register("sculpt_brush_size_down", "Brush Size −", "Sculpt");
        input.register("sculpt_brush_size_up", "Brush Size +", "Sculpt");
        input.bind_default(InputEvent::Key(egui::Key::Z), InputMods::NONE, InputContext::Viewport, "sculpt_brush_size_down");
        input.bind_default(InputEvent::Key(egui::Key::X), InputMods::NONE, InputContext::Viewport, "sculpt_brush_size_up");
        input.bind_default(InputEvent::ScrollUp, InputMods::NONE, InputContext::Viewport, "sculpt_brush_size_up");
        input.bind_default(InputEvent::ScrollDown, InputMods::NONE, InputContext::Viewport, "sculpt_brush_size_down");
        // Strength: Shift+Z / Shift+X (size stays plain Z/X). Dispatched in input_keyboard Sculpt arm.
        input.register("sculpt_brush_strength_down", "Brush Strength −", "Sculpt");
        input.register("sculpt_brush_strength_up", "Brush Strength +", "Sculpt");
        input.bind_default(InputEvent::Key(egui::Key::Z), InputMods::SHIFT, InputContext::Viewport, "sculpt_brush_strength_down");
        input.bind_default(InputEvent::Key(egui::Key::X), InputMods::SHIFT, InputContext::Viewport, "sculpt_brush_strength_up");

        // -- Voxel remesh / resolution (rebindable; acts only in Sculpt work mode) --
        input.register("sculpt_remesh", "Remesh / Resolution", "Sculpt");
        input.bind_default(InputEvent::Key(egui::Key::R), InputMods::CTRL, InputContext::Viewport, "sculpt_remesh");

        // -- Voxel surface-mode "Voxelize now" (deferred Move flush; rebindable, Sculpt mode only).
        // Enter = confirm the accumulated surface-mode moves into the density grid (Tool Exit Convention:
        // Enter confirms). Also fired by the Voxelize button / the 2s idle timer / accumulation overflow.
        input.register("voxel_voxelize_now", "Voxelize Pending Moves", "Sculpt");
        input.bind_default(InputEvent::Key(egui::Key::Enter), InputMods::NONE, InputContext::Viewport, "voxel_voxelize_now");
        // -- Poly bevel "Clamp Overlap" toggle (rebindable; acts only while the bevel modal owns the drag).
        // C toggles Blender's Clamp Overlap: OFF lets the bevel go wider than the geometry would allow.
        // Dispatched contextually in the is_poly_bevel() branch, so it can't collide with the SDF
        // clip-curve C binding (which is checked outside any active poly modal).
        input.register(ACTION_BEVEL_TOGGLE_CLAMP, "Bevel: Toggle Clamp Overlap", "Poly");
        // NOTE: C+NONE+Viewport is already bound to "clip_curve_toggle" above. bind_default() dedups by
        // (event, mods, context) and would DROP this binding, leaving binding_for(bevel_toggle_clamp)
        // empty. The two actions are disambiguated by tool state at dispatch time (clip-curve fires
        // outside any poly modal; this fires only inside the bevel modal), so they legitimately share
        // the panel context. Push the binding directly (guarded against a true duplicate of itself) so
        // binding_for() can resolve it and honor a later user remap.
        let already = input.bindings.iter().any(|b| b.action_id == ACTION_BEVEL_TOGGLE_CLAMP);
        if !already {
            input.bindings.push(InputBinding {
                event: InputEvent::Key(egui::Key::C),
                mods: InputMods::NONE,
                context: InputContext::Viewport,
                action_id: ACTION_BEVEL_TOGGLE_CLAMP,
                is_default: true,
            });
        }

        // -- Poly boolean pie (hold X → Union / Difference / Intersect, cutter → targets) --
        // NOTE: X+NONE+Viewport is already bound to "sculpt_brush_size_up" above, and
        // bind_default() dedups by (event, mods, context) — it would silently DROP this
        // binding. Same shared-binding situation as ACTION_BEVEL_TOGGLE_CLAMP: the actions
        // are disambiguated by mode at dispatch time (brush size fires only in Sculpt work
        // mode; this pie only in the Polygonal modeling context with an idle tool), so they
        // legitimately share the panel context. Push directly so binding_for() resolves it
        // and honors a later user remap.
        input.register(ACTION_POLY_BOOLEAN_PIE, "Boolean Pie (Cutter \u{2192} Targets)", "Poly");
        let already = input.bindings.iter().any(|b| b.action_id == ACTION_POLY_BOOLEAN_PIE);
        if !already {
            input.bindings.push(InputBinding {
                event: InputEvent::Key(egui::Key::X),
                mods: InputMods::NONE,
                context: InputContext::Viewport,
                action_id: ACTION_POLY_BOOLEAN_PIE,
                is_default: true,
            });
        }

        // -- SDF boolean pie (hold X → Union / Subtract / Intersect / Seam on the selection) --
        // Same shared-binding situation as ACTION_POLY_BOOLEAN_PIE: X+NONE+Viewport is already
        // bound, and bind_default() dedups by (event, mods, context) — it would silently DROP
        // this binding. The actions are disambiguated by modeling context at dispatch time
        // (this pie fires ONLY in the SDF modeling context with an idle tool). Push directly
        // so binding_for() resolves it and honors a later user remap.
        input.register(ACTION_SDF_BOOLEAN_PIE, "Boolean Pie (SDF Op Type)", "SDF");
        let already = input.bindings.iter().any(|b| b.action_id == ACTION_SDF_BOOLEAN_PIE);
        if !already {
            input.bindings.push(InputBinding {
                event: InputEvent::Key(egui::Key::X),
                mods: InputMods::NONE,
                context: InputContext::Viewport,
                action_id: ACTION_SDF_BOOLEAN_PIE,
                is_default: true,
            });
        }

        // -- Poly Join (Ctrl+J) — merge selected poly bodies into the active one (Blender parity) --
        input.register(ACTION_POLY_JOIN, "Join Selected Poly Bodies", "Poly");
        input.bind_default(InputEvent::Key(egui::Key::J), InputMods::CTRL, InputContext::Viewport, ACTION_POLY_JOIN);

        // Poly Circle (Alt+C) migrated to ACTION_DEFS (ACT_POLY_CIRCLE_REGION) with bug #6 — the
        // registration here was removed; it now round-trips to keybinds.json and the rebind editor.

        // -- Poly edge-loop selection walk (SHIFT+Scroll in Edge mode) --
        // Extends/retracts the edge selection ONE edge along the loop from the hovered edge — partial
        // loop selection without clicking each segment. Dispatched from the viewport scroll chain
        // (input_viewport.rs) via resolve(); acts only in Poly Edge work mode with an idle tool.
        input.register("loop_extend_next", "Extend Selection Along Edge Loop", "Selection");
        input.register("loop_extend_prev", "Retract Selection Along Edge Loop", "Selection");
        input.bind_default(InputEvent::ScrollUp, InputMods::SHIFT, InputContext::Viewport, "loop_extend_next");
        input.bind_default(InputEvent::ScrollDown, InputMods::SHIFT, InputContext::Viewport, "loop_extend_prev");

        // Occlusion-aware ("visible only" vs X-ray) selection toggle. Registered for the
        // shortcut editor / rebinding UI. No default key is bound: resolve()-based dispatch
        // is not yet wired into handle_keyboard(), and the CLAUDE.md rule forbids adding a
        // new hardcoded key_pressed() block — so the primary control is the viewport-header
        // toggle. A key can be bound here once resolve() dispatch lands, with zero behavior
        // change (the toggle already flips self.sel.visible_only).
        input.register("select_visible_only_toggle", "Toggle Visible-Only Selection", "Selection");
    }
}
