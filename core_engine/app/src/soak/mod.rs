//! AZRA_SOAK — self-driving console harness (sibling of AZRA_VISDIFF).
//!
//! Run: `AZRA_SOAK=<out_dir> cargo run -p app` (optionally with `AZRA_UI=v2`).
//! The app drives ITSELF from `update()` via production APIs, captures a viewport
//! shot after each step (same `visdiff_shot` → screenshot readback path as VISDIFF),
//! and prints one console line per step:
//!
//! ```text
//! [SOAK] 007 sdf.boolean_subtract     48ms  prims=2 vcap=9436   px=18.4%  OK
//!
//! COLUMN MEANINGS — read these before drawing conclusions from a run:
//!   ms    SYNCHRONOUS call time only. Most SDF actions ENQUEUE async work (bake, re-mesh) that
//!         completes during the step's `settle` frames, so a 0.1ms `boolean_subtract` is the
//!         enqueue, NOT the bake. Steps whose work is synchronous (the modifier-stack fold) DO
//!         report real cost — those are the ones `SLOW` is meaningful for.
//!   vcap  GPU-resident VERTEX CAPACITY, summed over bodies. Capacity, not an exact count: the
//!         true count is GPU-only ("NO mesh readback" — resident.rs). Capacity is reallocated on
//!         every re-mesh, so it tracks topology changes, which is what this column is for.
//!   px    % of viewport pixels changed vs the previous step. THE load-bearing column: `OK` with
//!         px=0.0% means the action did nothing.
//! [SOAK] 015 curv.vertex_rung          2ms  prims=2 verts=9436  px= 0.0%  NO-OP: sparse not quiet
//! ```
//!
//! Columns: wall ms · post-step state · % pixels changed vs previous shot · verdict.
//! `px` is computed in-process (no external diff script). A step that reports OK with
//! 0% pixel change is the bug class this harness is built to catch.
//!
//! Zero cost when `AZRA_SOAK` is unset — one env check at the call site (app_update).

use std::path::PathBuf;
use std::time::Instant;

mod scenarios;
mod report;
mod drivers;
mod meshmap;

// ─── Public harness state on AzraApp ─────────────────────────────────────────

/// Per-session state for AZRA_SOAK. Lives on `AzraApp` so the screenshot path can
/// hand decoded RGBA into the harness without a second capture pipeline.
#[derive(Default)]
pub(crate) struct SoakHarness {
    pub active: bool,
    pub out_dir: PathBuf,
    pub frame: u32,
    pub step_i: usize,
    pub phase: SoakPhase,
    pub step_t0: Option<Instant>,
    pub step_ms: f32,
    pub step_note: Option<String>,
    pub step_fail: Option<String>,
    pub step_unwired: Option<String>,
    /// Full-frame RGBA8 from the last completed screenshot decode (viewport_render hook).
    pub last_rgba: Option<(u32, u32, Vec<u8>)>,
    /// Previous step's RGBA for pixel-diff.
    pub prev_rgba: Option<(u32, u32, Vec<u8>)>,
    pub waiting_shot: bool,
    pub settle_left: u32,
    pub wait_shot_frames: u32,
    // Summary counters
    pub n_ok: u32,
    pub n_noop: u32,
    pub n_slow: u32,
    pub n_fail: u32,
    pub n_unwired: u32,
    /// Cached material id for texturing section (set by tex.add_material).
    pub mat_id: Option<core_math::primitives::MaterialId>,
    /// Active poly body for phase-2 poly steps.
    pub poly_nid: Option<core_math::scene_graph::NodeId>,
    /// Snapshot of (prims, vcap, nodes) before save/load round-trip.
    pub pre_save_counts: Option<(usize, usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SoakPhase {
    #[default]
    Boot,
    /// Run the step action this frame.
    Action,
    /// Wait for bake/render to settle before capture.
    Settle,
    /// Request screenshot via `visdiff_shot`.
    RequestShot,
    /// Wait for `soak.last_rgba` from the screenshot completion path.
    WaitShot,
    Done,
}
