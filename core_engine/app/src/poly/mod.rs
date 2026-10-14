//! Polygon editing tools — one file per tool for modularity.

pub(crate) mod primitive_params_ui;
pub(crate) mod primitive_source; // MeshSource on PolygonalBody: live_spec / set_primitive_spec / make_editable
pub(crate) mod primitive_drag;   // viewport chords: Alt+drag segments, Shift+drag round width, Shift+scroll round segs
mod extrude;
mod inset;
mod circle;
mod loop_cut;
mod slide;
mod bevel;
mod merge;
mod bridge;
mod dissolve;
mod subdivide;
mod duplicate;
mod normals;
mod fill;
mod grid_fill;
mod spin;
mod loop_tools;
mod poke;
mod connect;
mod tosphere;
mod selection;
pub(crate) mod mesh_sync;
pub(crate) mod remesh;
pub(crate) mod sculpt;
pub(crate) mod sculpt_pick;
pub(crate) mod sculpt_undo;
pub(crate) mod sculpt_pick_gpu;
pub(crate) mod sculpt_pbvh;
pub(crate) mod uv;
pub(crate) mod lod;
mod dice;
mod key_dispatch;
mod delete;
pub(crate) mod boolean;
pub(crate) mod bool_material; // per-face materials through a boolean + its redo panel
#[cfg(test)]
mod diag_material_class; // APP-PATH proof that Ctrl+X / Alt+X keep per-face materials
#[cfg(test)]
mod diag_bool_material; // APP-PATH proof that the X-pie boolean keeps per-face materials
#[cfg(test)]
mod diag_bool_island; // APP-PATH proof that an island boolean ignores unreachable islands
#[cfg(test)]
mod diag_bool_ngon_bevel; // owner bollean.fl4: rounded-bevel cutter subtract on an n-gon arch
mod separate;
mod bisect;
mod rip;
mod hinge_rotate;
pub(crate) mod outcome;
pub(crate) mod op_tags;
