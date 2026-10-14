//! Interactive + one-shot **create session** façades over `create_tool` engine.
//!
//! - **Geo kernels** (shape data, ray math): `ops::geo` — pure, no App
//! - **Instant / click create** (App ops for menus & nodes): `instant_origin`, `click_place`, `add_at`
//! - **Footprint drag session** state machine stays in `create_tool.rs` (engine layer — allowed);
//!   UI/input/nodes enter via `session::*` so a future "Create Box (interactive)" node shares one API.

pub mod add_at;
pub mod click_place;
pub mod instant_origin;
pub mod session;
