//! Keyboard input handler — moved from input.rs.
//!
//! Pure-relocation layout: the single mega-method `handle_keyboard` lives WHOLE in
//! `handle.rs` (dispatch order is behavior — never carve its interior into new
//! signatures). Same rule as `update()` / `dispatch_gpu_render`.

mod handle;
