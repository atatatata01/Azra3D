//! SculptState — the voxel-sculpt runtime cluster grouped off the FourthLayerApp god-struct
//! (de-God-Object Phase 1). All fields are `pub(crate)`; access them as `self.sculpt.<field>`.
//! Pure regrouping — no behaviour change, no field renames.

use std::collections::{HashMap, VecDeque};
use crate::{VoxelUndoStack, VoxelColorUndoStack, VoxelGridUndoStack, VoxelSurfaceUndoStack, UndoKind, VoxelObject};

/// Blender `brush.stroke_method` (`properties_paint_common.StrokePanel`).
///
/// Engine today is Space (radius-fraction interpolator). Dots skips the interpolator
/// (one dab per pointer sample). Airbrush keeps Space and adds timed dabs while still.
/// Anchored / Line / Curve are listed to match the Blender menu; they fall back to Space.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SculptStrokeMethod {
    Dots,
    DragDot,
    #[default]
    Space,
    Airbrush,
    Anchored,
    Line,
    Curve,
}

impl SculptStrokeMethod {
    pub const ALL: [SculptStrokeMethod; 7] = [
        Self::Dots,
        Self::DragDot,
        Self::Space,
        Self::Airbrush,
        Self::Anchored,
        Self::Line,
        Self::Curve,
    ];

    /// Blender RNA labels (`Brush.stroke_method`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Dots => "Dots",
            Self::DragDot => "Drag Dot",
            Self::Space => "Space",
            Self::Airbrush => "Airbrush",
            Self::Anchored => "Anchored",
            Self::Line => "Line",
            Self::Curve => "Curve",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Self::Dots => "One stamp per pointer sample — no chord fill (Blender Dots).",
            Self::DragDot => "Single stamp that follows the cursor (Blender Drag Dot). Applied as Dots until undo-reapply lands.",
            Self::Space => "Evenly spaced dabs along the path (Blender Space — our default interpolator).",
            Self::Airbrush => "Space plus timed dabs while the cursor is held still (Blender Airbrush).",
            Self::Anchored => "Click sets centre, drag sets radius (not wired — uses Space).",
            Self::Line => "Two-click line of stamps (not wired — uses Space).",
            Self::Curve => "Paint-curve stroke (not wired — uses Space).",
        }
    }

    pub fn implemented(self) -> bool {
        matches!(self, Self::Dots | Self::DragDot | Self::Space | Self::Airbrush)
    }

    /// Method the interpolator actually runs.
    pub fn effective(self) -> Self {
        if self.implemented() {
            self
        } else {
            Self::Space
        }
    }

    pub fn interpolates(self) -> bool {
        matches!(self.effective(), Self::Space | Self::Airbrush)
    }
}

/// Frozen state of one anchored Grab stroke (Blender `SCULPT_do_grab_brush` semantics).
///
/// Everything here is captured at stroke START and never recomputed, which is the whole point:
/// Blender grabs the surface it touched, not whatever happens to be under the cursor later.
pub(crate) struct PolyGrabAnchor {
    pub(crate) nid: core_math::scene_graph::NodeId,
    /// Affected verts, frozen at the anchor.
    pub(crate) verts: Vec<u32>,
    /// Their positions BEFORE the stroke, parallel to `verts` — the base every frame adds to.
    pub(crate) orig: Vec<glam::Vec3>,
    /// Per-AXIS signed falloff weight per vert, frozen with the set. Without symmetry this
    /// is `w·(1,1,1)` (w = scalar falloff: 1 at the anchor → 0 at the rim) — identical to
    /// the old scalar. With mirror axes enabled, each mirrored anchor instance contributes
    /// `w_i·mask_i` (mask = ±1 per axis), normalized by `max_w/Σw_i`: mirrored verts move
    /// with the mirrored offset, and a vert ON the mirror plane gets a ZERO cross-plane
    /// component (it stays on the plane instead of splitting the seam). The update rule
    /// stays ONE line either way: `new = orig + offset_local * weight` (component-wise).
    pub(crate) weight: Vec<glam::Vec3>,
    /// Grab point in WORLD space; the drag plane passes through it, facing the camera.
    pub(crate) anchor_world: glam::Vec3,
}

/// PR-0 (ZB_RESPONSIVE_SCULPT_PLAN §11.2): per-stamp PolySculpt stroke costs.
/// Fields may read 0 until the matching P0 slice lands its instrumentation.
/// `normals_mode`: 0 = none yet, 1 = region pass, 2 = full recompute.
#[derive(Default, Clone, Copy)]
pub(crate) struct PolySculptProfile {
    pub pick_ms: f32,
    pub query_ms: f32,
    pub kernel_ms: f32,
    pub normals_ms: f32,
    pub upload_ms: f32,
    pub upload_bytes: u64,
    pub seed_count: u32,
    pub corner_count: u32,
    pub undo_bytes: u64,
    pub normals_mode: u8,
}

#[derive(Default)]
pub(crate) struct SculptState {
    /// PR-0: live PolySculpt stroke costs (ZB plan §11.2) — HUD + AZRA_POLYSCULPT_PERF.
    pub(crate) poly_profile: PolySculptProfile,
    pub(crate) voxel_undo_stack: VoxelUndoStack,
    pub(crate) voxel_redo_stack: VoxelUndoStack, // VS4: symmetric redo
    /// Surface-mode mesh-stroke undo/redo (3D-Coat Surface mode — `UndoKind::Surface`).
    pub(crate) voxel_surface_undo_stack: VoxelSurfaceUndoStack,
    pub(crate) voxel_surface_redo_stack: VoxelSurfaceUndoStack,
    pub(crate) voxel_color_undo_stack: VoxelColorUndoStack,
    pub(crate) voxel_color_redo_stack: VoxelColorUndoStack,
    /// Full-grid snapshots for Move grow / Ctrl+R remesh (restores res + density).
    pub(crate) voxel_grid_undo_stack: VoxelGridUndoStack,
    pub(crate) voxel_grid_redo_stack: VoxelGridUndoStack,
    /// Interleaved undo-kind order: `Cpu` → density delta; `Atlas` → GPU brick snapshot;
    /// `CpuColor` → paint; `Grid` → full `VoxelGridRecord`.
    pub(crate) voxel_undo_order: VecDeque<UndoKind>,
    pub(crate) voxel_redo_order: VecDeque<UndoKind>,
    pub(crate) pending_atlas_restore: VecDeque<crate::voxel_gpu_mesh::AtlasUndoEntry>,
    pub(crate) voxel: Option<VoxelObject>, // NEW voxel sculpt room (density grid + Surface Nets mesh)
    pub(crate) current_stroke: HashMap<u32, f32>, // LEGACY cpu_volume sculpt: idx → pre-stroke value
    pub(crate) voxel_stroke: core_math::voxel_sculpt::StrokeRecorder, // voxel room: dense-mask undo capture
    pub(crate) stroke_active: bool,
    /// The voxel stroke's RESOLVED polarity (Shift/Ctrl/Alt/brush_add folded), latched every
    /// stamp frame at the single invert resolve. The pen-up release tails MUST use this —
    /// recomputing from raw `brush_add` there dropped Ctrl/Alt, so the deferred dabs flushed
    /// at the end of a Ctrl-carve stroke DEPOSITED (polarity flip at pen-up, 2026-08-18).
    pub(crate) voxel_stroke_invert: bool,
    /// PolySculpt stroke last hit (local). `None` between strokes.
    pub(crate) poly_stroke_last: Option<glam::Vec3>,
    /// PolySculpt brush when no voxel object is attached. `None` = Draw.
    pub(crate) poly_brush: Option<core_math::voxel_sculpt::VoxelBrush>,
    /// Which `(body, stack slot)` the modifier Edit button armed. `None` = not editing.
    pub(crate) poly_edit: Option<(core_math::scene_graph::NodeId, usize)>,
    /// Brush radius / strength for PolySculpt (0 = use 0.15 / 1.0 defaults).
    pub(crate) poly_radius: f32,
    pub(crate) poly_strength: f32,
    /// GLOBAL sculpt falloff hardness (owner 2026-08-14: symmetry / size / strength / falloff /
    /// dynamesh are SCULPT settings, not per-brush ones). 0 = soft round, 1 = hard core.
    pub(crate) poly_hardness: f32,
    /// Full Blender header knobs for PolySculpt (no VoxelObject). Size/strength/hardness
    /// stay on `poly_radius` / `poly_strength` / `poly_hardness` so existing strokes + tests
    /// keep working; everything else lives here.
    pub(crate) poly_tool: crate::VoxelBrushSettings,
    /// One-shot seed guard for poly_tool defaults that DIVERGE from
    /// `VoxelBrushSettings::default()` (which serves the density path). Seeded by
    /// `poly_ensure_defaults()`; the UI owns the values afterwards.
    pub(crate) poly_tool_seeded: bool,
    pub(crate) poly_symmetry: [bool; 3],
    pub(crate) poly_pressure_to_strength: bool,
    pub(crate) poly_pressure_to_radius: bool,
    /// Brush tip alpha for PolySculpt (Photoshop/ZBrush brush shape) — the poly twin of
    /// `VoxelObject::brush_tip` (runtime tool state, never serialized). Multiplies the stamp
    /// falloff in `displace_verts_for_dabs`; `None` = radial falloff only.
    pub(crate) poly_tip: Option<core_math::voxel_sculpt::BrushTipAlpha>,
    pub(crate) poly_tip_name: String,
    pub(crate) poly_tip_path: Option<String>,
    /// Per-stroke random stream for tip orientation / rotation dynamics (poly twin of
    /// `VoxelObject::dyn_stroke_seed` / `dyn_dab_index`): seed bumps at stroke start, the
    /// counter tracks the stroke's absolute dab index so one gesture keeps one continuous
    /// stream no matter how its samples split across frames.
    pub(crate) poly_stroke_seed: u32,
    pub(crate) poly_dab_counter: u32,
    /// Live RMB-drag brush adjust (ZBrush-style, owner 2026-08-19): drag RIGHT = radius up,
    /// drag UP = strength up. Latched at drag start as `(anchor px, radius0, strength0)`;
    /// `None` between gestures. Works on whichever sculpt target is live (voxel or poly).
    pub(crate) rmb_adjust: Option<(glam::Vec2, f32, f32)>,
    /// PolySculpt cursor surface hit `(local pos, local normal)` — refreshed every hover
    /// frame by the SAME budgeted pick the stroke uses (Blender paint_cursor parity:
    /// `SCULPT_cursor_geometry_info_update` raycasts per mouse move). `None` = ray missed
    /// the mesh → the overlay falls back to the flat 2D circle, exactly like Blender.
    pub(crate) poly_cursor_hit: Option<(glam::Vec3, glam::Vec3)>,
    /// Ctrl+R Sculpt Remesh popup for a POLY body (the voxel twin lives on VoxelObject).
    pub(crate) poly_remesh_open: bool,
    /// Target voxel-remesh resolution for the poly popup (0 = unseeded → 128). Also the
    /// resolution the header's Dyntopo toggle seeds its live Remesh modifier with —
    /// Dyntopo itself is STACK state (`poly_dyntopo_enabled`/`poly_dyntopo_set`), never a
    /// flag here that could lie after a body switch.
    pub(crate) poly_remesh_res: u32,
    /// Dyntopo master toggle — GLOBAL sculpt tool state (owner: "global sculpt setting"),
    /// like symmetry. Blender-tech since DYNTOPO_PLAN Phase B: local edge
    /// subdivide/collapse on the WORKING mesh per stamp (`poly_dyntopo_refine_for_dabs`),
    /// never a global remesh.
    pub(crate) dyn_topo: bool,
    /// Dyntopo detailing mode (Blender's Detailing): false = Constant (fixed edge length,
    /// `poly_dyntopo_const_detail`), true = Brush — target edge length tracks the brush
    /// (`brush diameter / poly_dyntopo_detail`; smaller brush = finer topology).
    pub(crate) poly_dyntopo_brush_detail: bool,
    /// Brush-detail density: how many edges span the brush DIAMETER (Blender's Detail
    /// Percentage analog). 0 = unseeded → 12.
    pub(crate) poly_dyntopo_detail: f32,
    /// Constant-detail target edge length in scene units. 0 = unseeded → 0.05.
    pub(crate) poly_dyntopo_const_detail: f32,
    /// Refine method (Blender): true = Subdivide Collapse (default), false = Subdivide
    /// Edges only. Seeded by `poly_ensure_defaults`.
    pub(crate) poly_dyntopo_collapse: bool,
    pub(crate) poly_lazy_pos: Option<glam::Vec2>,
    /// ANCHORED GRAB state, captured ONCE at stroke start and frozen for the whole drag
    /// (`None` between strokes). This is what makes Grab feel like Blender's:
    ///
    /// * the affected vertex set and their falloff weights are decided at the anchor and NEVER
    ///   re-gathered — re-querying at the moving centre lets verts enter and leave the brush
    ///   mid-drag, which smears the surface like taffy instead of grabbing it;
    /// * displacement is ABSOLUTE from the captured originals (`orig + offset * weight`), not an
    ///   accumulation of per-frame deltas — so pulling out and coming back lands exactly where it
    ///   started, and a dropped frame cannot bend the result;
    /// * the drag is read in the VIEW PLANE at the anchor's depth, not from re-raycasting the
    ///   mesh — the mesh is moving under the cursor, so a per-frame ray hits a different place
    ///   each time (or misses entirely and silently kills the stroke).
    pub(crate) poly_grab: Option<PolyGrabAnchor>,
    /// Live SURFACE-mode stroke: pre-stroke vertex arrays of each chunk touched so far (keyed by
    /// linear chunk index), captured lazily on first touch by the dab / Snake Hook / Move-commit
    /// paths. Drained into ONE `UndoKind::Surface` entry by `surface_end_stroke` — the ONE stroke
    /// lifecycle exit. Lives HERE (per-stroke state, next to `voxel_stroke` — its density analog),
    /// NOT on `VoxelObject`: a stale entry surviving into the next stroke is the "vertices reverse
    /// to an older stroke's positions" class (Snake Hook re-derives from this map as PRISTINE).
    pub(crate) surface_stroke_snap: HashMap<u32, Vec<[f32; 8]>>,
    /// Accumulate-OFF (layer) ledger for the live SURFACE-mode stroke: per chunk, per vert row,
    /// the signed deposit already applied THIS stroke — the voxel-surface twin of the poly
    /// `deposit_applied` ledger (one offset layer per stroke, Blender BRUSH_ACCUMULATE unset).
    /// Vert-row keys are stable for the stroke's lifetime (same identity contract as
    /// `surface_stroke_snap`). MUST be cleared wherever the snapshot is cleared/drained — a
    /// stale entry would cap the NEXT stroke's deposit at zero.
    pub(crate) surface_stroke_ledger: HashMap<u32, HashMap<u32, f32>>,
    /// Blender stroke method — tool-global (header Stroke popover). Default Space.
    pub(crate) stroke_method: SculptStrokeMethod,
    /// Seconds between still-cursor Airbrush dabs. 0 = 0.09 (Blender-ish rate).
    pub(crate) airbrush_rate: f32,
    /// Last Airbrush dab instant (runtime-only).
    pub(crate) airbrush_last: Option<std::time::Instant>,
}

impl SculptState {
    pub(crate) fn poly_ensure_defaults(&mut self) {
        if self.poly_radius <= 1e-4 {
            self.poly_radius = 0.15;
        }
        if self.poly_strength <= 1e-4 {
            // Blender's default brush strength (owner 2026-08-19: 1.0 default was far too hot).
            self.poly_strength = 0.5;
        }
        if !self.poly_tool_seeded {
            self.poly_tool_seeded = true;
            // Blender parity: normal-deposit brushes default to Accumulate OFF (one offset
            // layer per stroke — the deposit ledger in poly_sculpt_apply_dabs).
            // `VoxelBrushSettings::default()` says true because the DENSITY path reads the
            // same field for its PlaneFill blend, so the poly copy diverges here ONCE; the
            // Brush Settings checkbox is authoritative afterwards.
            self.poly_tool.accumulate = false;
            // Dyntopo defaults (Blender: Subdivide Collapse, moderate detail).
            self.poly_dyntopo_collapse = true;
            // Blender's default stroke spacing = 10% of DIAMETER (DNA spacing 10) =
            // 0.2 × radius. The 0.10 seed dabbed twice as densely as Blender — with
            // overlap attenuation the deposit matches either way, but the dab cadence
            // (and CPU per stroke) now matches 1:1.
            self.poly_tool.stroke_spacing = 0.20;
        }
        if self.poly_dyntopo_const_detail <= 0.0 {
            self.poly_dyntopo_const_detail = 0.05;
        }
        if self.poly_dyntopo_detail <= 0.0 {
            self.poly_dyntopo_detail = 12.0;
        }
    }

    pub(crate) fn airbrush_interval(&self) -> std::time::Duration {
        let s = if self.airbrush_rate > 1e-4 {
            self.airbrush_rate
        } else {
            0.09
        };
        std::time::Duration::from_secs_f32(s.clamp(0.016, 1.0))
    }
}

impl SculptState {
    /// THE one surface-stroke lifecycle exit (structural fix for the cross-stroke snapshot-leak
    /// class, 2026-07-23). Drains the per-chunk first-touch snapshots into ONE
    /// `UndoKind::Surface` entry and truncates redo across all kinds (a stroke is a new edit).
    /// Called from EVERY way a stroke can end:
    ///   • pointer release (`handle_sculpt_input` surface release branch),
    ///   • stroke START (`handle_voxel_sculpt_input` drag_started — the self-heal: any exit path
    ///     missed above commits here, so the next stroke can NEVER inherit a stale snapshot),
    ///   • `undo()` / `redo()` (an in-progress stroke commits first, then pops — density parity),
    ///   • `process_dirty` SCENE force-clear of `stroke_active` (dirty.rs — previously swallowed
    ///     the release branch and leaked the snapshot).
    /// No-op (false) when no stroke is pending. Discard barriers (revoxelize / remesh swap /
    /// grid-record restore / leave-edit) still `clear()` directly — there the snapshots reference
    /// dead topology and must NOT become an undo entry.
    pub(crate) fn surface_end_stroke(&mut self) -> bool {
        // The Accumulate-OFF ledger shares the snapshot's stroke lifetime exactly.
        self.surface_stroke_ledger.clear();
        let snap = std::mem::take(&mut self.surface_stroke_snap);
        if snap.is_empty() { return false; }
        let delta: crate::SurfaceMeshDelta = snap.into_iter().collect();
        let evicted = self.voxel_surface_undo_stack.push(delta);
        self.voxel_undo_order.push_back(UndoKind::Surface);
        crate::input_sculpt::prune_front_undo_markers(
            &mut self.voxel_undo_order, UndoKind::Surface, evicted);
        // A stroke truncates redo across all kinds (matching density-stroke behaviour).
        self.voxel_surface_redo_stack.clear();
        self.voxel_redo_stack.clear();
        self.voxel_color_redo_stack.clear();
        self.voxel_grid_redo_stack.clear();
        self.voxel_redo_order.clear();
        true
    }
}

// ============================================================================
// Stroke-lifecycle regression tests — the cross-stroke stale-snapshot class
// (owner-reported 2026-07-23: "caused by older stroke / vertices reversing /
// brush stops registering"). These drive the REAL production lifecycle fn in
// the exact sequences the app runs.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn vtx(x: f32) -> Vec<[f32; 8]> { vec![[x, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]] }

    /// Stroke A touches chunk 3, but its release is MISSED (at HEAD: `process_dirty(SCENE)`
    /// force-cleared `stroke_active` → the release branch never ran → the snapshot leaked into
    /// stroke B, whose Snake Hook then used it as "pristine" and REWOUND chunk 3 to stroke A's
    /// start — the owner's "geometry reversing / caused by older stroke"). The fix: stroke B's
    /// START runs the one lifecycle exit as a self-heal.
    #[test]
    fn missed_release_snapshot_commits_at_next_stroke_start_never_leaks_into_it() {
        let mut s = SculptState::default();
        // Stroke A: first-touch snapshot captured (exactly what voxel_surface_apply_dabs /
        // apply_snake_hook / commit_move_grab insert). Release missed — NO end_stroke.
        s.surface_stroke_snap.insert(3, vtx(1.0));
        // Stroke B start → self-heal (the drag_started call site).
        assert!(s.surface_end_stroke(), "a leaked snapshot must be committed, not inherited");
        // The missed stroke became its own undo entry (edits preserved, undo history intact)…
        assert_eq!(s.voxel_undo_order.back(), Some(&UndoKind::Surface));
        assert_eq!(s.voxel_surface_undo_stack.pop_back(), Some(vec![(3u32, vtx(1.0))]));
        // …and stroke B begins with NO stale pristine source: `or_insert_with` in the dab /
        // Snake Hook paths can only capture CURRENT verts — the rewind class is impossible.
        assert!(s.surface_stroke_snap.is_empty());
    }

    /// Owner Ctrl+Z's mid-stroke (pen still down): the in-progress stroke must commit FIRST so
    /// the Surface undo arm pops exactly it — matching the density path's "revert in-progress
    /// stroke first". At HEAD the undo popped the PREVIOUS stroke's entry while the live
    /// snapshot kept pre-undo verts: the ongoing Snake Hook then rewrote the restored chunks
    /// from that snapshot, resurrecting the undone stroke ("caused by older stroke").
    #[test]
    fn mid_stroke_undo_commits_in_progress_stroke_and_pops_it_first() {
        let mut s = SculptState::default();
        // Stroke A committed normally.
        s.surface_stroke_snap.insert(7, vtx(1.0));
        assert!(s.surface_end_stroke());
        // Stroke B in progress (snapshot live), Ctrl+Z arrives → heal runs first:
        s.surface_stroke_snap.insert(7, vtx(2.0));
        s.surface_end_stroke();
        // The undo arm now pops B's entry (pre-B verts), NOT A's.
        assert_eq!(s.voxel_undo_order.pop_back(), Some(UndoKind::Surface));
        assert_eq!(s.voxel_surface_undo_stack.pop_back(), Some(vec![(7u32, vtx(2.0))]));
        // A's entry is intact beneath — history stays strictly ordered.
        assert_eq!(s.voxel_undo_order.pop_back(), Some(UndoKind::Surface));
        assert_eq!(s.voxel_surface_undo_stack.pop_back(), Some(vec![(7u32, vtx(1.0))]));
        assert!(s.surface_stroke_snap.is_empty());
    }

    /// The lifecycle exit is a strict no-op with no pending stroke (release after heal, double
    /// release, SCENE-dirty with no stroke) and truncates redo when a stroke IS pending.
    #[test]
    fn end_stroke_noop_when_empty_and_truncates_redo_when_pending() {
        let mut s = SculptState::default();
        assert!(!s.surface_end_stroke(), "no pending stroke → no-op");
        assert!(s.voxel_undo_order.is_empty());
        // Seed redo state (as after an undo), then a new in-progress stroke ends:
        s.voxel_surface_redo_stack.push(vec![(1u32, vtx(9.0))]);
        s.voxel_redo_order.push_back(UndoKind::Surface);
        s.surface_stroke_snap.insert(1, vtx(4.0));
        assert!(s.surface_end_stroke());
        assert!(s.voxel_redo_order.is_empty(), "a stroke truncates redo across all kinds");
        assert!(s.voxel_surface_redo_stack.pop_back().is_none());
    }

    #[test]
    fn stroke_method_blender_labels_and_fallback() {
        assert_eq!(SculptStrokeMethod::Dots.label(), "Dots");
        assert_eq!(SculptStrokeMethod::DragDot.label(), "Drag Dot");
        assert_eq!(SculptStrokeMethod::Space.label(), "Space");
        assert_eq!(SculptStrokeMethod::Airbrush.label(), "Airbrush");
        assert_eq!(SculptStrokeMethod::Anchored.label(), "Anchored");
        assert_eq!(SculptStrokeMethod::Line.label(), "Line");
        assert_eq!(SculptStrokeMethod::Curve.label(), "Curve");
        assert!(SculptStrokeMethod::Space.implemented());
        assert!(SculptStrokeMethod::Dots.interpolates() == false);
        assert!(SculptStrokeMethod::Space.interpolates());
        assert_eq!(SculptStrokeMethod::Line.effective(), SculptStrokeMethod::Space);
        assert_eq!(SculptStrokeMethod::Curve.effective(), SculptStrokeMethod::Space);
        assert_eq!(SculptStrokeMethod::Anchored.effective(), SculptStrokeMethod::Space);
    }
}
