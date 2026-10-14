//! Scene / modeling operations — **one unit = one file** (node-ready).
//!
//! | Layer | Module | For |
//! |-------|--------|-----|
//! | Pure geo kernels | `geo` | Shape presets, ray/plane math — **no App** |
//! | Create sessions | `create` | Instant / click / footprint façade → create_tool engine |
//! | Domain AppOps | `sdf`, `poly`, `instance`, `edit`, `bool_op` | Menu / tool / shortcut / future nodes |
//!
//! See `ui2/INTEGRATION_RULES.md` §0.

pub mod geo;
pub mod create;
pub mod sdf;
pub mod poly;
pub mod instance;
pub mod edit;
pub mod bool_op;
