//! Tools palette — catalog + activation (no dock/shell chrome here).
//!
//! Split for the 500-line rule:
//!   catalog.rs  — category tables + tool_categories / pillar_key
//!   palette.rs  — ui2_tools_pillar + row paint helpers
//!   activate.rs — ui2_tool_activate / ui2_tool_command

mod catalog;
mod palette;
mod activate;

// Submodules reach ui2 shared types (`ToolCatDef`, `ToolItem`, …) via `use super::*`.
#[allow(unused_imports)]
pub(crate) use super::*;

// Re-export free items so `tools::SDF_CATS` / `tools::tool_categories` etc. stay reachable.
// Impl methods on FourthLayerApp need no re-export.
pub(crate) use catalog::*;
#[allow(unused_imports)] // available for external call sites; not all used inside this file
pub(crate) use palette::{tp_sec, tp_tool_row, tp_cmd_row};
