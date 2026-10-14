//! Reusable UI chrome primitives for AZRA UI v2.
//!
//! **Rule:** docks, library, floating panes, and any future panel host use these
//! APIs only — no one-off resize/collapse/scroll code in panel bodies.
//!
//! | Module | Owns |
//! |--------|------|
//! | [`collapse`] | Pane body collapse + header collapse (mockup `.collapsed` / `.__headcoll`) |
//! | [`zone`] | Zone hide/show with SLOP (mockup `__lhide` / `__rhide` / `__bhide`) |
//! | [`seam`] | Edge resize handles (H/V) that drive zone size + hide |
//! | [`scroll`] | Panel scrollbar styling (token-driven) |
//! | [`frame`] | Pane surface / head strip paint |
//! | [`float_resize`] | 8-handle resize for floating panels |

pub mod collapse;
pub mod float_resize;
pub mod frame;
pub mod scroll;
pub mod seam;
pub mod zone;

pub use collapse::*;
pub use float_resize::*;
pub use frame::*;
pub use scroll::*;
pub use seam::*;
pub use zone::*;

/// Mockup seam hit width (px) — dock edge grab strip on the viewport side.
pub const SEAM_HIT: f32 = 6.0;
/// Mockup SLOP — drag past min this far before zone-hide (side docks + library).
pub const ZONE_SLOP: f32 = 60.0;
/// Zone-hidden restore strip thickness (mockup 6px).
pub const ZONE_STRIP: f32 = 6.0;
/// Panel header height (mockup `.ptabs` / `.pane-head`).
pub const HEADER_H: f32 = crate::ui_theme::H_PANEL_HEADER; // 34
/// Minimum pane body height when expanded.
pub const PANE_MIN_H: f32 = 64.0;
