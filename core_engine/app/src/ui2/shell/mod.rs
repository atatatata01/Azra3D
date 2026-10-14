//! App bar · ctx bar · status bar (no full menu tree).
//!
//! Split for the 500-line rule: app_bar / ctxbar / shading / status.

mod app_bar;
mod ctxbar;
mod render_settings;
mod sculpt_header;
mod sculpt_popovers;
mod shading;
mod status;

// Submodules reach ui2 shared types (`WorkspaceTab`, `PanelKind`, …) via `use super::*`.
// Inherent methods on FourthLayerApp live in the child files; no method re-exports needed.
#[allow(unused_imports)]
pub(crate) use super::*;
