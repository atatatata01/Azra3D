//! Right-panel helpers, Agent command bar, and voxel sculpt panel
//! (pure-relocated from ui_panels.rs).

mod shared;
mod modifiers;
mod agent_bar;
mod sculpt_panel;

pub(crate) use shared::*;
pub(crate) use modifiers::*;
// agent_bar / sculpt_panel are impl AzraApp methods — no re-export needed
