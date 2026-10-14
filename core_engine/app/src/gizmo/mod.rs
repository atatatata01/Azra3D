//! Gizmo / modal transform / scene-edit commands (pure-relocated from gizmo.rs).
//!
//! Transform *math* lives in `transform.rs` (`compute_transform`).
//! Transform *write-back* lives in [`apply`] — do not re-derive apply paths.

mod apply;
mod scene_edit;
mod transform_modal;
mod gizmo_drag;
