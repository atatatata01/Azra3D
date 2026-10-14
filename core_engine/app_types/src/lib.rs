// app_types — shared type definitions extracted from the app crate.
// Pure data types with no behavior on AzraApp.

// ==================================================================
// Dirty Flags — centralized change tracking
// ==================================================================

/// Tracks which subsystems need processing at frame end.
#[derive(Clone, Copy, Default)]
pub struct DirtyFlags(u32);

impl DirtyFlags {
    pub const NONE:  u32 = 0;
    pub const SCENE: u32 = 1 << 0;
    pub const RENDER: u32 = 1 << 1;
    pub const MESH:  u32 = 1 << 2;
    pub const LIGHT: u32 = 1 << 3;
    pub const SVO:   u32 = 1 << 4;

    pub fn set(&mut self, flag: u32) { self.0 |= flag; }
    pub fn has(self, flag: u32) -> bool { self.0 & flag != 0 }
    pub fn any(self) -> bool { self.0 != 0 }
    pub fn clear(&mut self) { self.0 = 0; }
    /// Raw bitset (for diagnostics / idle gate).
    pub fn bits(self) -> u32 { self.0 }
    /// Read current flags and clear them. Flags set during processing survive.
    pub fn take(&mut self) -> u32 { let v = self.0; self.0 = 0; v }
}

impl std::fmt::Debug for DirtyFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirtyFlags(0x{:x})", self.0)
    }
}

// ==================================================================
// Type sub-modules
// ==================================================================
mod types_gizmo;               pub use types_gizmo::*;
mod types_light;               pub use types_light::*;
mod types_transform;           pub use types_transform::*;
mod types_poly;                pub use types_poly::*;
mod types_create;              pub use types_create::*;
mod types_mirror;              pub use types_mirror::*;
mod types_library;             pub use types_library::*;
mod types_viewport;            pub use types_viewport::*;
mod types_render;              pub use types_render::*;
mod types_compositor;          pub use types_compositor::*;
mod types_compositor_presets;
mod types_tool;                pub use types_tool::*;
mod types_selection;           pub use types_selection::*;
// types_ui stays in app (depends on pie_menu, shader_nodes)
mod types_tweaks;              pub use types_tweaks::*;
mod types_commands;            pub use types_commands::*;
mod types_layout;              pub use types_layout::*;
mod types_workspace;           pub use types_workspace::*;

pub mod keybindings;           pub use keybindings::*;
pub mod user_defaults;
pub mod keymap;                // INPUT_ARCHITECTURE_PLAN — S1: types only, inert (no `pub use` glob to avoid name clashes)
