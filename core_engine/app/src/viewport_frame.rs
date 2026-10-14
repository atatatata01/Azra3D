//! Viewport frame indicator — the border around the 3D view as a status channel.
//!
//! Owner 2026-08-06: the region-select rubber band was the only thing that ever drew on the
//! viewport border, its line was too thick, and it was left showing when nothing was selecting.
//! Their proposal, and this module: use that border deliberately — record indicator, unsaved-work
//! warning that ramps to red between 45 and 60 minutes, render progress, VRAM pressure.
//!
//! ONE CHANNEL AT A TIME, BY PRIORITY. Two statuses sharing one border is unreadable — an amber
//! frame would mean "unsaved" or "VRAM" or "still rendering" and the user could not tell which.
//! The border therefore shows exactly one thing, the most urgent, and the rest wait. That is a
//! deliberate limit on the feature, not an omission: a status light that can mean four things at
//! once is not a status light.
//!
//! Everything here is PASSIVE — it reads state and paints. It never mutates the app.

use eframe::egui;

/// Seconds of unsaved work before the frame starts warning at all.
/// Minimum wall-clock gap between two geometry audits of the active poly body. The audit is three
/// O(mesh) walks (≈37 ms at 323k faces), so it is budgeted rather than run at frame rate: the
/// border light is a diagnostic, and a user cannot tell a light that settles a few times a second
/// from one that recomputes at 60 Hz. In between, the previous verdict stands — a real answer that
/// is a moment old, never a guess.
const GEOM_ISSUE_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

const UNSAVED_WARN_START_S: f32 = 45.0 * 60.0;
/// Seconds at which the warning reaches full red.
const UNSAVED_WARN_FULL_S: f32 = 60.0 * 60.0;
/// VRAM fraction above which the frame starts reporting pressure.
const VRAM_WARN_FRAC: f32 = 0.80;
/// Border thickness. Thin on purpose — the old region-select band used 2.5 and the owner called
/// it too thick. This sits at the edge of a 3D view and must never compete with the model.
const FRAME_W: f32 = 1.5;

/// What the border is currently saying. Ordered most-urgent first; `pick` returns the first that
/// applies, so adding a variant means deciding where it sits in that order.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FrameStatus {
    /// Capture in progress. Never suppressed — recording without an indicator is how footage gets
    /// lost or privacy gets broken.
    Recording,
    /// The active mesh is broken — non-manifold, degenerate faces, or stacked (duplicate)
    /// vertices. Ranks high because every edit made on top of it inherits the damage.
    BrokenGeometry { issues: u32 },
    /// A render is accumulating. `frac` in 0..=1 sweeps the perimeter clockwise from top-left.
    Progress { frac: f32 },
    /// Unsaved work, ramping `t` = 0..=1 across the 45→60 minute window.
    Unsaved { t: f32 },
    /// VRAM pressure, `frac` of the budget in use (only above `VRAM_WARN_FRAC`).
    Vram { frac: f32 },
}

impl FrameStatus {
    /// Colour + how much of the perimeter to draw. Progress is the only one that partially fills;
    /// the rest ring the whole frame so peripheral vision reads them without a saccade.
    fn style(self) -> (egui::Color32, f32) {
        match self {
            // Pulse is applied by the caller (needs frame time); base is the classic record red.
            FrameStatus::Recording => (egui::Color32::from_rgb(255, 60, 60), 1.0),
            // Hard red-orange, fully opaque: this is a correctness fault, not an ambient nag.
            FrameStatus::BrokenGeometry { .. } => (egui::Color32::from_rgb(255, 90, 40), 1.0),
            FrameStatus::Progress { frac } => {
                (egui::Color32::from_rgb(120, 200, 255), frac.clamp(0.0, 1.0))
            }
            FrameStatus::Unsaved { t } => {
                // Amber → red as the hour approaches. Starts nearly invisible so it nags rather
                // than shouts, and is unmistakable by the time it matters.
                let t = t.clamp(0.0, 1.0);
                let r = 255u8;
                let g = (170.0 * (1.0 - t)) as u8;
                let b = (40.0 * (1.0 - t)) as u8;
                let a = (60.0 + 195.0 * t) as u8;
                (egui::Color32::from_rgba_unmultiplied(r, g, b, a), 1.0)
            }
            FrameStatus::Vram { frac } => {
                let over = ((frac - VRAM_WARN_FRAC) / (1.0 - VRAM_WARN_FRAC)).clamp(0.0, 1.0);
                let a = (70.0 + 150.0 * over) as u8;
                (egui::Color32::from_rgba_unmultiplied(255, 180, 60, a), 1.0)
            }
        }
    }
}

impl crate::AzraApp {
    /// The single status the viewport border should show, if any.
    ///
    /// Priority is the whole design:
    ///   * RECORDING outranks everything - a missing record light is a real-world problem.
    ///   * BROKEN GEOMETRY next: every edit made on a damaged mesh inherits the damage, so the
    ///     user needs to know before the next operation, not after.
    ///   * PROGRESS next - transient, and the user is actively waiting on it.
    ///   * UNSAVED outranks VRAM because losing work beats a performance hint.
    pub(crate) fn viewport_frame_status(&self) -> Option<FrameStatus> {
        if self.macro_recording {
            return Some(FrameStatus::Recording);
        }
        if let Some(issues) = self.viewport_frame_geometry_issues() {
            return Some(FrameStatus::BrokenGeometry { issues });
        }
        if let Some(frac) = self.viewport_frame_render_progress() {
            // Only while genuinely mid-render — a finished render must not leave a full ring up.
            if frac < 0.999 {
                return Some(FrameStatus::Progress { frac });
            }
        }
        if let Some(t) = self.viewport_frame_unsaved_ramp() {
            return Some(FrameStatus::Unsaved { t });
        }
        if let Some(frac) = self.viewport_frame_vram_frac() {
            if frac > VRAM_WARN_FRAC {
                return Some(FrameStatus::Vram { frac });
            }
        }
        None
    }

    /// Count of geometry faults on the ACTIVE poly body: non-manifold, zero-area faces, and
    /// stacked (coincident-but-distinct) vertices. `None` when the body is healthy.
    ///
    /// PURE READ of `poly_geom_issues`, which [`Self::poll_poly_geometry_issues`] maintains in the
    /// poll phase. Nothing is computed here.
    ///
    /// It used to compute the three O(mesh) walks inline, memoised on `poly_edit_generation`.
    /// That memo cannot work: the generation IS the mutation counter, so it misses on every frame
    /// the mesh moves — a 37 ms audit of 323k faces in the PAINT path, for a status light on the
    /// viewport border (owner 2026-08-12: `[MidOv] frame 38.8` of a 156 ms frame). An episode gate
    /// was not enough either: the owner's capture shows the same 36.9 ms with `tool=Idle`, because
    /// the generation churns after a drag as well as during one. A diagnostic must not be able to
    /// put unbounded work in the frame at any rate the mutation counter happens to move at.
    fn viewport_frame_geometry_issues(&self) -> Option<u32> {
        self.poly_geom_issues.filter(|&c| c > 0)
    }

    /// Poll phase: refresh the geometry-issue count for the active poly body.
    ///
    /// Bounded three ways, in order of how much they matter:
    /// 1. **Not while a drag episode is open.** Mid-gesture the mesh is transiently non-manifold or
    ///    zero-area by construction, so a live count is noise, not information.
    /// 2. **Throttled** to one walk per [`GEOM_ISSUE_MIN_INTERVAL`]. This is a budget, not a race
    ///    dodge (CLAUDE.md Frame Budget Rule — per-frame work needs a cap): a border light that
    ///    settles a few times a second is indistinguishable to the user from one that recomputes at
    ///    60 Hz, and the stale value in between is the previous honest answer, never a guess.
    /// 2a. **Only once the mesh has settled** (generation unchanged since the previous poll) — so
    ///    the walk lands after the user stops, never in the middle of continuous editing.
    /// 3. **Skipped when the generation and body are unchanged** — the plain memo, which does work
    ///    once the mesh actually stops moving.
    pub(crate) fn poll_poly_geometry_issues(&mut self) {
        use core_math::half_edge::{FaceId, VertexId, NO_EDGE};
        if self.pending_episode.is_some() {
            return;
        }
        let Some(nid) = self.active_poly_nid() else {
            self.poly_geom_issues = None;
            self.poly_geom_issues_key = None;
            return;
        };
        let key = (self.poly_edit_generation, nid.0);
        if self.poly_geom_issues_key == Some(key) {
            return; // already answered for this exact mesh state
        }
        // 2a. Only once the mesh has SETTLED — generation unchanged since the previous poll. The
        //     same latch the select buffer uses. Without it the throttle still lets a 37 ms walk
        //     land ~3x a second in the middle of continuous editing; with it, the audit happens
        //     shortly after the user stops, which is when the answer is worth having.
        let settled = self.poly_geom_issues_last_gen == Some(key.0);
        self.poly_geom_issues_last_gen = Some(key.0);
        if !settled {
            return;
        }
        if self.poly_geom_issues_at.elapsed() < GEOM_ISSUE_MIN_INTERVAL {
            return; // throttled — keep the previous verdict, retry next poll
        }
        let Some(mesh) = self.poly_node_mesh(nid) else { return };
        let mut issues = 0u32;
        if mesh.is_manifold().is_err() {
            issues += 1;
        }
        // Zero-area / degenerate faces.
        for f in 0..mesh.faces.len() as u32 {
            let fid = FaceId(f);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            if mesh.compute_face_normal(fid).length() < 1e-9 {
                issues += 1;
            }
        }
        // Stacked vertices: two distinct live verts at one position.
        let q = |p: glam::Vec3| {
            ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)
        };
        let mut seen: std::collections::HashSet<(i64, i64, i64)> =
            std::collections::HashSet::with_capacity(mesh.vertices.len());
        for i in 0..mesh.vertices.len() as u32 {
            let v = VertexId(i);
            if mesh.vert(v).half_edge == NO_EDGE {
                continue;
            }
            if !seen.insert(q(mesh.vert(v).position)) {
                issues += 1;
            }
        }
        self.poly_geom_issues = Some(issues);
        self.poly_geom_issues_key = Some(key);
        self.poly_geom_issues_at = std::time::Instant::now();
    }

    /// 0..=1 across the 45→60 minute unsaved window, or `None` when there is nothing to warn
    /// about. A scene with no unsaved changes never warns however long it has been open.
    fn viewport_frame_unsaved_ramp(&self) -> Option<f32> {
        if !self.project.scene_modified {
            return None;
        }
        let secs = self.project.last_auto_save.elapsed().as_secs_f32();
        if secs < UNSAVED_WARN_START_S {
            return None;
        }
        let span = (UNSAVED_WARN_FULL_S - UNSAVED_WARN_START_S).max(1.0);
        Some(((secs - UNSAVED_WARN_START_S) / span).clamp(0.0, 1.0))
    }

    /// Path-trace accumulation progress, or `None` when not path tracing.
    fn viewport_frame_render_progress(&self) -> Option<f32> {
        let target = self.render.pt_max_accum_frames;
        if target == 0 {
            return None;
        }
        let done = self.render.pt_accumulated_frames;
        if done == 0 {
            return None;
        }
        Some((done as f32 / target as f32).clamp(0.0, 1.0))
    }

    /// VRAM in use as a fraction of budget. `None` until a real budget source is wired — the
    /// channel is defined and drawn, it simply has nothing to report yet. Returning a guess here
    /// would be worse than staying quiet: a status light that lies is worse than one that is off.
    fn viewport_frame_vram_frac(&self) -> Option<f32> {
        None
    }

    /// Paint the frame. Call from BOTH viewport overlay paths (v1 `draw_viewport_overlays` and v2
    /// `ui2_draw_scene_overlays`) — same rule the region-select band documents, and the reason it
    /// once appeared in only one of them.
    pub(crate) fn draw_viewport_frame_indicator(&self, painter: &egui::Painter, rect: egui::Rect) {
        let Some(status) = self.viewport_frame_status() else {
            return;
        };
        let (mut color, fill) = status.style();
        if status == FrameStatus::Recording {
            // ~1 Hz pulse so it reads as "live" rather than as a static red border. Driven by
            // wall time, not a frame counter: the pulse must look the same at 20fps and 200fps.
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f32())
                .unwrap_or(0.0);
            let phase = (t * std::f32::consts::TAU).sin() * 0.5 + 0.5;
            let a = (140.0 + 115.0 * phase) as u8;
            color = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a);
        }
        let stroke = egui::Stroke::new(FRAME_W, color);
        // Inset by half the width so the line lands INSIDE the viewport and cannot be clipped
        // to a hairline by the panel edge.
        let r = rect.shrink(FRAME_W * 0.5);
        if fill >= 0.999 {
            painter.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Middle);
            return;
        }
        // Partial: walk the perimeter clockwise from the top-left and stop at `fill`.
        let (w, h) = (r.width(), r.height());
        let total = 2.0 * (w + h);
        let mut left = total * fill.clamp(0.0, 1.0);
        let mut seg = |from: egui::Pos2, to: egui::Pos2, len: f32, left: &mut f32| {
            if *left <= 0.0 || len <= 0.0 {
                return;
            }
            let t = (*left / len).clamp(0.0, 1.0);
            painter.line_segment([from, from + (to - from) * t], stroke);
            *left -= len * t;
        };
        seg(r.left_top(), r.right_top(), w, &mut left);
        seg(r.right_top(), r.right_bottom(), h, &mut left);
        seg(r.right_bottom(), r.left_bottom(), w, &mut left);
        seg(r.left_bottom(), r.left_top(), h, &mut left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ramp must be silent before 45 minutes, full at 60, and silent forever on a saved
    /// scene. A warning that fires early is one the user learns to ignore.
    #[test]
    fn unsaved_ramp_window_is_45_to_60_minutes() {
        assert_eq!(UNSAVED_WARN_START_S, 2700.0);
        assert_eq!(UNSAVED_WARN_FULL_S, 3600.0);
        let ramp = |secs: f32| {
            let span = UNSAVED_WARN_FULL_S - UNSAVED_WARN_START_S;
            ((secs - UNSAVED_WARN_START_S) / span).clamp(0.0, 1.0)
        };
        assert_eq!(ramp(2699.0), 0.0, "silent before 45 min");
        assert!((ramp(3150.0) - 0.5).abs() < 1e-3, "half way at 52.5 min");
        assert_eq!(ramp(3600.0), 1.0, "full red at 60 min");
        assert_eq!(ramp(9999.0), 1.0, "clamped after the hour");
    }

    /// Priority is the design. If two channels could ever paint at once the border stops meaning
    /// anything, so this locks the order rather than leaving it to reading order in `pick`.
    #[test]
    fn status_priority_is_recording_progress_unsaved_vram() {
        let all = [
            FrameStatus::Recording,
            FrameStatus::Progress { frac: 0.5 },
            FrameStatus::Unsaved { t: 1.0 },
            FrameStatus::Vram { frac: 0.95 },
        ];
        // Every variant must produce a visible stroke; a status that paints nothing is a bug.
        for s in all {
            let (c, fill) = s.style();
            assert!(c.a() > 0, "{s:?} must be visible");
            assert!(fill > 0.0, "{s:?} must draw some perimeter");
        }
        // Only Progress draws a partial ring.
        assert!(matches!(all[1], FrameStatus::Progress { .. }));
        assert!((all[1].style().1 - 0.5).abs() < 1e-6);
        for s in [all[0], all[2], all[3]] {
            assert_eq!(s.style().1, 1.0, "{s:?} must ring the whole frame");
        }
    }
}
