//! AZRA UI v2 — modular presentation layer (`AZRA_UI=v2`).
//!
//! ## Layout (no god file)
//! ```text
//! ui2/
//!   mod.rs              orchestration + re-exports
//!   INTEGRATION_RULES.md  ★ mandatory for agents / new PRs
//!   types.rs            PanelKind, DockSide, PanelGroup, Act, Ui2State
//!   chrome/             REUSABLE: resize, collapse, zone-hide, scroll, frame
//!   draw/               ui2_draw entry + pies / placement / viewport_split
//!   shell/              app bar · ctx bar · shading · sculpt header · status bar
//!   menus.rs            File…Help menu fillers
//!   dock.rs             side docks · floating · actions (uses chrome)
//!   pane.rs             tab strip · fit heights · pane body
//!   persist.rs          ui2_layout.json load / debounced save / reset

//!   tools/              tools palette catalog + activation (split modules)
//!   outliner/           outliner tree UI (split modules)
//! ```
//! Visual bible: `docs/UI/azra_app_mockup.html`
//! Rules: [`INTEGRATION_RULES.md`](INTEGRATION_RULES.md)

pub mod chrome;
mod types;
mod draw;
mod shell;
mod menus;
mod dock;
mod dock_horizontal;
mod pane;
mod idle;
pub(crate) mod persist;
mod tools;
mod outliner;

// Sibling modules `use super::*` — re-export shared items.
pub(crate) use types::*;
pub(crate) use pane::*;

pub(crate) use chrome::{HEADER_H, PANE_MIN_H, SEAM_HIT, ZONE_SLOP, ZONE_STRIP};
pub(crate) use dock::DOCK_EDGE_HIT;
