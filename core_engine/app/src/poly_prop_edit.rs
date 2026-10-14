// poly_prop_edit.rs — Proportional Edit ("soft select") viewport affordances.
//
// Two things live here and nothing else:
//   * the FALLOFF RING, drawn whenever Proportional Edit is ON in a poly sub-object mode
//     (owner 2026-08-09: "always when prop edit is ON", not only during a G/R/S drag), and
//   * the ORIGIN DIAMOND, a draggable handle that places the point influence is measured
//     FROM (owner 2026-08-09: "viewport proper handle, to move, a move in position").
//
// The deformation maths stays in `poly_drag_tools/transform_drag.rs`. What this file owns
// is the ORIGIN RESOLVER that both sides call. That split is deliberate: if the overlay
// re-derived the origin itself, the ring could show one centre while the transform used
// another, and the handle would become a decoration that lies about the result.
//
// Not scene data: the origin is tool state in the same class as `poly_prop_radius`, so it
// takes no undo entry and is not written to the `.fl4` (it is cleared on new/load, since a
// world-space point from the outgoing scene means nothing in the incoming one).

use eframe::egui;
use glam::{Mat4, Vec3};
use crate::*;

/// Screen radius (px) within which the origin diamond claims the press. Same 16px
/// convention as the blend-k / curve-bevel diamonds — every viewport handle grabs alike.
const PROP_HANDLE_GRAB_PX: f32 = 16.0;

/// Normalised distance at which `f` first drops to half weight, if it has such a point.
///
/// Drives the faint inner ring — the thing that makes Sharp / Smooth / Linear read as three
/// different modes instead of three identical circles. `Constant` (weight 1 all the way to
/// the rim) and `Random` (noise, not a monotonic curve) genuinely have no half-weight
/// radius, so they return `None` and the inner ring is skipped rather than drawn at an
/// arbitrary place that would misreport the falloff.
///
/// Free function, not a method: it depends on nothing but the falloff, which keeps it
/// unit-testable without standing up an `AzraApp`.
fn prop_half_weight_t(f: PropFalloff) -> Option<f32> {
    if matches!(f, PropFalloff::Constant | PropFalloff::Random) {
        return None;
    }
    const STEPS: u32 = 64;
    (1..=STEPS)
        .map(|i| i as f32 / STEPS as f32)
        .find(|&t| f.weight(t) < 0.5)
}

impl AzraApp {
    // ------------------------------------------------------------------
    // Resolver — ONE definition of "where does influence come from"
    // ------------------------------------------------------------------

    /// Pivot for a poly G/R/S, resolved from the Action Centre.
    ///
    /// Extracted from `poly_start_transform` so the overlay can show the AUTO origin
    /// without duplicating the match. Two copies of this would let the ring drift away
    /// from the vertices that actually move.
    pub(crate) fn poly_transform_pivot(&self, frame_center: Vec3) -> Vec3 {
        match self.tool.action_center {
            crate::ActionCenter::Automatic => {
                if self.tool.action_automatic_center.is_some() {
                    frame_center
                } else {
                    self.poly_selection_center().unwrap_or(frame_center)
                }
            }
            crate::ActionCenter::Selection
            | crate::ActionCenter::SelectionCenterAutoAxis
            | crate::ActionCenter::None => self.poly_selection_center().unwrap_or(frame_center),
            crate::ActionCenter::Origin
            | crate::ActionCenter::Screen
            | crate::ActionCenter::Pivot
            | crate::ActionCenter::Element
            | crate::ActionCenter::Workplane => frame_center,
        }
    }

    /// The USER-PLACED proportional origin, or `None` while it is still AUTO.
    /// `transform_drag` applies this as an override on the pivot it already resolved.
    pub(crate) fn poly_prop_falloff_origin(&self) -> Option<Vec3> {
        self.poly_prop_origin
    }

    /// Where the ring is drawn: the placed origin, else the auto pivot. `None` when there
    /// is no poly body to anchor to.
    pub(crate) fn poly_prop_origin_display(&self) -> Option<Vec3> {
        if let Some(p) = self.poly_prop_origin {
            return Some(p);
        }
        self.poly_active_body()?;
        Some(self.poly_transform_pivot(self.resolve_action_frame().center))
    }

    /// Ring + handle visible: poly sub-object mode with Proportional Edit ON.
    /// Same gate as the header pill (`ui2_show_prop_pill`) plus the toggle itself.
    pub(crate) fn poly_prop_overlay_visible(&self) -> bool {
        self.poly_proportional
            && self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
    }

    /// Push the resolved origin into a LIVE `PolyTransform` so a change made while a G/R/S
    /// is in flight takes effect this frame instead of next drag — the same courtesy the
    /// radius slider and the scroll wheel already do for `FalloffKind::radius`.
    fn poly_prop_sync_live_transform(&mut self) {
        let placed = self.poly_prop_origin;
        if let Some(ts) = self.tool.current.as_poly_transform_mut() {
            ts.falloff_center = placed.unwrap_or(ts.center);
        }
    }

    /// Return the origin to AUTO (follow the Action Centre).
    pub(crate) fn poly_prop_origin_reset(&mut self) {
        self.poly_prop_origin = None;
        self.poly_prop_sync_live_transform();
        self.toast("Proportional origin: Auto");
    }

    // ------------------------------------------------------------------
    // Overlay
    // ------------------------------------------------------------------

    /// Draw the falloff ring + origin diamond, and record whether the cursor is on the
    /// handle. Hover is stamped here (not in input) because this is where the handle is
    /// already projected — one hit-test, read by `input_viewport`, exactly like
    /// `tool.blend_k_hovered`.
    pub(crate) fn draw_poly_prop_overlay(
        &mut self, painter: &egui::Painter, rect: egui::Rect, mouse: egui::Pos2,
    ) {
        self.poly_prop_handle_hovered = false;
        if !self.poly_prop_overlay_visible() {
            return;
        }
        let Some(origin) = self.poly_prop_origin_display() else { return };
        let radius = self.poly_prop_radius;

        // Camera-facing disc. The ring communicates EXTENT, so billboarding it (rather than
        // laying it on a mesh plane) keeps it a circle at grazing angles instead of
        // collapsing to a line — same choice Blender makes for its proportional circle.
        let view_n = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();

        let dragging = self.tool.current.is_poly_prop_origin();
        let placed = self.poly_prop_origin.is_some();

        // Amber once the user has placed it, cyan while it is still tracking the Action
        // Centre — the colour IS the "auto vs placed" readout, so the state is legible
        // without opening the popover.
        let (r, g, b) = if placed { (255, 190, 90) } else { (120, 200, 255) };
        let ring_col = egui::Color32::from_rgba_unmultiplied(r, g, b, if dragging { 230 } else { 170 });
        let inner_col = egui::Color32::from_rgba_unmultiplied(r, g, b, 80);

        if let Some((pts, _)) = self.project_world_ring(rect, Mat4::IDENTITY, origin, view_n, radius) {
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.5, ring_col)));
        }
        if let Some(t50) = prop_half_weight_t(self.poly_prop_falloff) {
            if let Some((pts, _)) =
                self.project_world_ring(rect, Mat4::IDENTITY, origin, view_n, radius * t50)
            {
                painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.0, inner_col)));
            }
        }

        // Handle. Projected independently of the rings: a ring can fail to project (mostly
        // off-frustum) while its centre is still on screen, and the handle must stay
        // grabbable in that case.
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let Some(sp) = world_to_screen(
            origin, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg,
        ) else { return };

        let hovered = rect.contains(mouse) && (mouse - sp).length() < PROP_HANDLE_GRAB_PX;
        // Only CLAIM the press when idle — during a G/R/S the same pixels belong to the
        // transform, and stealing the drag there would cancel the user's edit.
        self.poly_prop_handle_hovered = hovered && self.tool.current.is_idle();

        let label = format!(
            "{} · r {:.3} · {}",
            self.poly_prop_falloff.label(),
            radius,
            if placed { "placed" } else { "auto" },
        );
        self.draw_diamond_handle(painter, sp, (r, g, b), &label, hovered, dragging);
    }

    // ------------------------------------------------------------------
    // Handle drag
    // ------------------------------------------------------------------

    /// Begin moving the origin. No `begin_episode`: this mutates tool state, not the scene,
    /// so it takes no undo entry (same as radius). Escape still restores, via `prev_origin`.
    pub(crate) fn poly_prop_origin_drag_start(&mut self, mouse: egui::Pos2) {
        let Some(origin) = self.poly_prop_origin_display() else { return };
        let prev_origin = self.poly_prop_origin;
        self.start_operation(ActiveTool::PolyPropOrigin(PropOriginDrag {
            start_origin: origin,
            start_mouse: mouse,
            prev_origin,
        }));
    }

    /// Slide the origin on the view plane through its drag-start position.
    ///
    /// The DIFFERENCE between the two ray/plane hits is applied rather than the raw hit, so
    /// the grab offset survives: clicking the edge of the diamond does not teleport its
    /// centre under the cursor on the first pixel of movement.
    pub(crate) fn poly_prop_origin_drag_update(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let Some(d) = self.tool.current.as_poly_prop_origin() else { return };
        let (start_origin, start_mouse) = (d.start_origin, d.start_mouse);
        let n = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
        let Some(a) = self.ray_plane_hit(start_mouse, rect, start_origin, n) else { return };
        let Some(b) = self.ray_plane_hit(mouse, rect, start_origin, n) else { return };
        self.poly_prop_origin = Some(start_origin + (b - a));
        self.poly_prop_sync_live_transform();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Pointer released — keep the placement.
    pub(crate) fn poly_prop_origin_drag_end(&mut self) {
        if !self.tool.current.is_poly_prop_origin() {
            return;
        }
        self.tool.current = ActiveTool::Idle;
        self.toast("Proportional origin placed");
    }

    /// Escape / right-click — restore whatever the origin was before the drag, including
    /// restoring AUTO when it had never been placed.
    pub(crate) fn poly_prop_origin_drag_cancel(&mut self) {
        let Some(d) = self.tool.current.as_poly_prop_origin() else { return };
        self.poly_prop_origin = d.prev_origin;
        self.tool.current = ActiveTool::Idle;
        self.poly_prop_sync_live_transform();
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Proportional origin move cancelled");
    }
}

#[cfg(test)]
mod tests {
    use super::prop_half_weight_t;
    use app_types::PropFalloff;

    /// Every curved falloff must yield an inner ring strictly INSIDE the radius ring, or the
    /// two would overdraw and the mode would look identical to Constant.
    #[test]
    fn curved_falloffs_have_a_half_weight_radius_inside_the_rim() {
        for f in [
            PropFalloff::Smooth,
            PropFalloff::Sphere,
            PropFalloff::Root,
            PropFalloff::InverseSquare,
            PropFalloff::Sharp,
            PropFalloff::Linear,
        ] {
            let t = prop_half_weight_t(f)
                .unwrap_or_else(|| panic!("{f:?} has no half-weight radius"));
            assert!(t > 0.0 && t < 1.0, "{f:?} half-weight t = {t} is not inside the ring");
            assert!(f.weight(t) < 0.5, "{f:?} weight at t={t} is not past half");
        }
    }

    /// Constant never drops below half before the rim and Random is not a curve — both must
    /// SKIP the inner ring rather than report a made-up radius.
    #[test]
    fn constant_and_random_report_no_half_weight_radius() {
        assert!(prop_half_weight_t(PropFalloff::Constant).is_none());
        assert!(prop_half_weight_t(PropFalloff::Random).is_none());
    }

    /// Sharp concentrates influence near the centre, Root spreads it wide — so Sharp's
    /// half-weight ring must sit strictly inside Root's. This is the property that makes the
    /// inner ring worth drawing at all; if it ever inverts, the overlay is lying about the mode.
    #[test]
    fn sharp_half_weight_is_tighter_than_root() {
        let sharp = prop_half_weight_t(PropFalloff::Sharp).unwrap();
        let root = prop_half_weight_t(PropFalloff::Root).unwrap();
        assert!(sharp < root, "Sharp t={sharp} should be tighter than Root t={root}");
    }
}
