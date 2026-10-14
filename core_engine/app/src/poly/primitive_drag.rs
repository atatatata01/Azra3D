//! Viewport modals for tuning a selected parametric primitive — owner-specified 2026-08-04.
//!
//! | key | modal | mouse (NO button) | scroll | `Shift`+scroll |
//! |---|---|---|---|---|
//! | `B` | round / bevel | X = width | segments | — |
//! | `Ctrl+R` | segments | previews the orientation | count **horizontally** | `Ctrl`/`Alt`+scroll = **vertically** |
//!
//! `Enter` confirms and `Esc`/`RMB` cancels. In the segments modal a middle-click (the orbit
//! gesture) SETS the current count and keeps the modal open, so you can bank one orientation and
//! carry straight on in the next. LMB does not confirm — it used to, and that made the modal a
//! one-shot you could not rotate out of.
//!
//! # Why keys, not chords
//!
//! The viewport is saturated: `Shift`/`Alt`+MMB are camera pan and view-snap, `Shift`/`Ctrl`+LMB
//! are additive-select, `Alt`+LMB is gizmo center-drag, and every bare letter is bound. Rather
//! than displace any of them, both modals hang off the key that ALREADY means the same thing —
//! `B` (bevel) and `Ctrl+R` (loop cut) — and pick their representation from what is selected:
//! authored topology gets the real edge op, a live parametric primitive gets the spec parameter.
//! CLAUDE.md explicitly allows one key in several contexts; it forbids only two actions that can
//! fire in the SAME frame in the SAME state, and a body either has a live spec or it does not.
//!
//! # Loop-Cut semantics for `Ctrl+R`
//!
//! Mouse movement does NOT change the count — it chooses which orientation the count applies to,
//! exactly as Loop Cut previews a direction before you commit. Scroll then sets the number, and a
//! click sets it. The orientation is measured against the **workplane** when one is locked, so the
//! choice stays put while the camera orbits; otherwise against the camera.

use core_math::primitive_spec::{Axis3, PrimitiveSpec, SegField};
use core_math::scene_graph::NodeId;
use glam::Vec3;

/// Live state for a primitive-parameter drag.
#[derive(Clone)]
pub(crate) struct PrimitiveDrag {
    pub(crate) nid: NodeId,
    /// Spec as it was when the drag started — all deltas are applied to THIS, never accumulated
    /// onto the running value, so the result depends only on total mouse travel and a drag that
    /// returns to its origin restores the original value exactly.
    pub(crate) start_spec: PrimitiveSpec,
    pub(crate) start_mouse: egui::Pos2,
    /// Which count the horizontal / vertical mouse axis edits. Snapped at drag start.
    pub(crate) field_h: Option<SegField>,
    pub(crate) field_v: Option<SegField>,
    /// The orientation the scroll currently edits. Chosen by mouse movement, Loop-Cut style.
    pub(crate) active_field: Option<SegField>,
    /// Alt+Shift: drive every count from one axis instead.
    pub(crate) all_fields: bool,
    /// Camera distance at drag start — width sensitivity scales with it so the feel is the same
    /// zoomed in or out (same trick as `poly_update_bevel_drag`).
    pub(crate) start_cam_dist: f32,
    /// Wheel accumulator. One segment per ~40 units, matching `poly_bevel_scroll_segments` —
    /// without it a single physical notch (which arrives as several events) stepped many segments
    /// at once (owner: "scroll too fast").
    pub(crate) scroll_accum: f32,
    /// True once a parameter actually changed, so a click-without-motion opens no undo entry.
    pub(crate) needs_settle: bool,
}

/// What the drag is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrimitiveDragKind {
    /// Alt held + mouse move (no button) — integer segment counts.
    Segments,
    /// `B` modal on a parametric primitive — continuous round/bevel width.
    /// Bare drag = width, bare scroll = segments, exactly like the poly bevel modal.
    RoundWidth,
}

impl crate::AzraApp {
    /// The (right, up) basis that axis picking is measured against.
    ///
    /// A **locked workplane wins** — the user set it precisely so "horizontal" and "vertical" mean
    /// something stable in the model, not something that changes as the camera swings. With no
    /// workplane the camera basis is the only sensible reference.
    fn primitive_reference_frame(&self) -> (Vec3, Vec3) {
        if self.poly_workplane_is_active_set() {
            let wp = &self.viewport.wp_resolved;
            let u = wp.tangent_u.normalize_or_zero();
            let v = wp.tangent_v.normalize_or_zero();
            if u.length_squared() > 0.25 && v.length_squared() > 0.25 {
                return (u, v);
            }
        }
        (
            (self.viewport.camera.orbit * Vec3::X).normalize_or_zero(),
            (self.viewport.camera.orbit * Vec3::Y).normalize_or_zero(),
        )
    }

    /// The selected body, if it is a live parametric primitive. The gate for every chord here.
    ///
    /// Resolves the node the SAME way `draw_props_poly` does — `active_node()` falling back to
    /// `last_clicked`. `active_node()` alone is legitimately `None` in common states, which is
    /// precisely why the properties panel has that fallback; without it the chords saw nothing
    /// while the panel was happily showing sliders for the same object.
    pub(crate) fn primitive_chord_target(&self) -> Option<(NodeId, PrimitiveSpec)> {
        let last = match self.sel.usel.last_clicked {
            Some(crate::types::SceneItemId::Node(nid)) => Some(nid),
            _ => None,
        };
        let nid = self.sel.usel.active_node().or(last)?;
        let spec = self.poly_live_spec(nid)?;
        Some((nid, spec))
    }

    /// Local axes laid out against the reference frame, most-horizontal first.
    ///
    /// The frame is the **workplane** when one is locked (owner 2026-08-04: "use workplane
    /// orientation"), falling back to the camera otherwise. Using the workplane means the axis you
    /// pick stays put while you orbit — which is the whole point of having set a workplane.
    fn visible_local_axes(&self, nid: NodeId) -> (Axis3, Axis3) {
        let (cam_right, cam_up) = self.primitive_reference_frame();

        // The body's local axes in WORLD space, so a rotated object maps its OWN axes to the
        // screen rather than the world's.
        let xf = self.scene.scene_graph.world_transform(nid);
        let world_axis = |a: Axis3| -> Vec3 {
            let v = match a {
                Axis3::X => Vec3::X,
                Axis3::Y => Vec3::Y,
                Axis3::Z => Vec3::Z,
            };
            xf.transform_vector3(v).normalize_or_zero()
        };

        const ALL: [Axis3; 3] = [Axis3::X, Axis3::Y, Axis3::Z];
        let score = |a: Axis3, basis: Vec3| world_axis(a).dot(basis).abs();

        // Horizontal = the local axis most aligned with screen-right; vertical = the most
        // screen-up of the two that remain. Picking both by their own basis vector (rather than
        // "drop the depth axis") is what makes this correct for an arbitrarily rotated body.
        let h = ALL
            .into_iter()
            .max_by(|a, b| {
                score(*a, cam_right)
                    .partial_cmp(&score(*b, cam_right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Axis3::X);
        let v = ALL
            .into_iter()
            .filter(|a| *a != h)
            .max_by(|a, b| {
                score(*a, cam_up)
                    .partial_cmp(&score(*b, cam_up))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(Axis3::Y);
        (h, v)
    }

    /// Apply the current mouse position. Called every frame while a modal is live.
    pub(crate) fn primitive_drag_update(&mut self, mouse: egui::Pos2, orbiting: bool) {
        let Some(d) = self.ui.primitive_drag.clone() else {
            return;
        };
        let Some(kind) = self.ui.primitive_drag_kind else {
            return;
        };
        // The body may have been deleted or baked mid-drag — stand down rather than write to it.
        if self.poly_live_spec(d.nid).is_none() {
            self.primitive_drag_cancel();
            return;
        }

        let dx = mouse.x - d.start_mouse.x;
        let dy = mouse.y - d.start_mouse.y;
        let mut spec = d.start_spec;

        match kind {
            PrimitiveDragKind::Segments => {
                // The screen↔axis mapping is recomputed EVERY FRAME from the current view (or the
                // locked workplane). Snapping it once at entry meant orbiting left the mapping
                // stale and the tool had to be restarted to work from the new angle (owner: "I
                // need to reset the tool each time I change view"). Recomputing is safe here
                // precisely because mouse movement only RETARGETS: no value can jump.
                let (ah, av) = self.visible_local_axes(d.nid);
                let (fh, fv) = (spec.seg_for_axis(ah), spec.seg_for_axis(av));

                // ORBITING (MMB held) re-aims the tool instead of driving it.
                //
                // The counts already set stay set — they live in the spec — and the target
                // follows the view, so releasing the orbit leaves the next scroll pointed at
                // whichever plane now faces you (owner: "let us rotate, and when we rotate it
                // sets the number of segments, letting us change view then scroll to add
                // segments in another plane"). Orbit mouse travel must NOT be read as a
                // retarget gesture, which is why `start_mouse` is re-anchored every frame here.
                if orbiting {
                    if let Some(a) = self.ui.primitive_drag.as_mut() {
                        a.field_h = fh;
                        a.field_v = fv;
                        a.active_field = fh.or(fv);
                        a.start_mouse = mouse;
                    }
                    return;
                }

                // Mouse movement PICKS THE ORIENTATION — it does not change the count. Same shape
                // as Loop Cut: move to choose the direction, scroll for how many, click to set.
                // A dead zone stops a jittery pointer from flipping the target every frame.
                const DEAD_ZONE: f32 = 6.0;
                let moved = dx.abs() > DEAD_ZONE || dy.abs() > DEAD_ZONE;
                let want_h = dx.abs() >= dy.abs();
                let mut announce = None;
                if let Some(a) = self.ui.primitive_drag.as_mut() {
                    a.field_h = fh;
                    a.field_v = fv;
                    if moved {
                        let next = if want_h { fh } else { fv };
                        if next.is_some() && next != a.active_field {
                            a.active_field = next;
                            announce = next;
                        }
                        // Re-anchor so the NEXT movement is measured from here; otherwise the
                        // target would latch on the first flick and never move again.
                        a.start_mouse = mouse;
                    } else if a.active_field.is_none() {
                        // Nothing targetable on entry (or the view just made one valid) — adopt
                        // whichever orientation exists so scroll is never a dead control.
                        a.active_field = fh.or(fv);
                    }
                }
                if announce.is_some() {
                    self.toast(&format!("Segments: {}", seg_field_label(announce)));
                }
                return; // counts change on scroll only
            }
            PrimitiveDragKind::RoundWidth => {
                let Some(base) = d.start_spec.round() else {
                    return;
                };
                // Camera-distance-scaled like `poly_update_bevel_drag`, but 4× gentler: at the
                // bevel's 0.002 the round went from nothing to fully clamped in a few pixels,
                // because a primitive's round range is a fraction of its size rather than a free
                // offset (owner: "is too strong").
                let delta = dx * d.start_cam_dist * 0.0005;
                let max = spec.round_max_radius();
                let r = (base.radius + delta).clamp(0.0, max);
                if let Some(round) = spec.round_mut() {
                    round.radius = r;
                }
            }
        }

        if spec != d.start_spec || d.needs_settle {
            if spec != self.poly_live_spec(d.nid).unwrap_or(spec) {
                self.set_primitive_spec(d.nid, spec);
                if let Some(a) = self.ui.primitive_drag.as_mut() {
                    a.needs_settle = true;
                }
            }
        }
    }

    /// `B` on a parametric primitive in Object mode — enter the round/bevel modal.
    ///
    /// Called from `poly_activate_bevel`, the single entry point the poly bevel already uses, so
    /// `B` keeps ONE meaning ("bevel this") and just picks the representation that fits what is
    /// selected: edge bevel on authored topology, parametric round on a live primitive.
    /// Returns false when the selection is not a live parametric primitive, so the normal poly
    /// bevel proceeds untouched.
    pub(crate) fn primitive_round_modal_begin(&mut self) -> bool {
        let Some((nid, spec)) = self.primitive_chord_target() else {
            return false;
        };
        if spec.round().is_none() {
            // Sphere/torus/cone have no round — decline so the user gets the real bevel (or its
            // "select a poly object" message) rather than a modal that can do nothing.
            return false;
        }
        let mouse = self.viewport.viewport_mouse;
        self.ui.primitive_drag = Some(PrimitiveDrag {
            nid,
            start_spec: spec,
            start_mouse: mouse,
            field_h: None,
            field_v: None,
            active_field: None,
            all_fields: false,
            start_cam_dist: self.viewport.camera.distance.max(0.01),
            scroll_accum: 0.0,
            needs_settle: false,
        });
        self.ui.primitive_drag_kind = Some(PrimitiveDragKind::RoundWidth);
        self.toast("Round: drag = width, scroll = segments, LMB/Enter = confirm, Esc = cancel");
        true
    }

    pub(crate) fn primitive_round_modal_active(&self) -> bool {
        self.ui.primitive_drag_kind == Some(PrimitiveDragKind::RoundWidth)
    }

    /// `Ctrl+R` on a parametric primitive in Object mode — enter the segments modal.
    ///
    /// Same key as Loop Cut and the same idea ("add divisions"), with the representation chosen by
    /// what is selected: real edge loops on authored topology, spec segment counts on a live
    /// primitive (owner 2026-08-04). Mouse X/Y edit the two counts facing the camera; scroll edits
    /// them all at once, matching Loop Cut's scroll-for-count.
    pub(crate) fn primitive_segments_modal_begin(&mut self) -> bool {
        let Some((nid, spec)) = self.primitive_chord_target() else {
            return false;
        };
        let (ah, av) = self.visible_local_axes(nid);
        let (field_h, field_v) = (spec.seg_for_axis(ah), spec.seg_for_axis(av));
        if field_h.is_none() && field_v.is_none() {
            return false;
        }
        self.ui.primitive_drag = Some(PrimitiveDrag {
            nid,
            start_spec: spec,
            start_mouse: self.viewport.viewport_mouse,
            field_h,
            field_v,
            active_field: field_h.or(field_v),
            all_fields: false,
            start_cam_dist: self.viewport.camera.distance.max(0.01),
            scroll_accum: 0.0,
            needs_settle: false,
        });
        self.ui.primitive_drag_kind = Some(PrimitiveDragKind::Segments);
        self.toast("Segments: move mouse X/Y, scroll = all, LMB/Enter = confirm, Esc = cancel");
        true
    }

    pub(crate) fn primitive_segments_modal_active(&self) -> bool {
        self.ui.primitive_drag_kind == Some(PrimitiveDragKind::Segments)
    }

    /// Scroll inside the segments modal — the count, on the orientation the mouse picked.
    ///
    /// `Shift` widens it to EVERY orientation at once (owner: "shift drag all orientation").
    fn primitive_scroll_all_segments(&mut self, delta: f32, swap_dir: bool) -> bool {
        if delta.abs() < 0.5 {
            return false;
        }
        let Some(d) = self.ui.primitive_drag.clone() else {
            return false;
        };
        let Some(spec_now) = self.poly_live_spec(d.nid) else {
            return false;
        };
        let step: i64 = if delta > 0.0 { 1 } else { -1 };
        let mut spec = spec_now;
        // A MODIFIER AT SCROLL TIME picks the direction (owner 2026-08-04: "pressing shift when
        // scrolling could change the direction... shift does not work, it needs to be ctrl, or
        // alt"). Shift genuinely cannot serve here: Windows remaps Shift+wheel onto the
        // HORIZONTAL axis before egui sees it, so the modifier and the signal collide. Ctrl and
        // Alt both pass through untouched, and both are accepted so either reflex works.
        //
        // Deterministic and instant — no mouse gesture needed to aim. Falls back to the other
        // axis when this shape cannot subdivide in the requested direction, so a wheel click is
        // never silently dead.
        let target = if swap_dir {
            d.field_v.or(d.field_h)
        } else {
            d.field_h.or(d.field_v)
        };
        let targets: Vec<SegField> = target.into_iter().collect();
        if targets.is_empty() {
            return false;
        }
        for f in &targets {
            if let Some(base) = spec_now.seg_get(*f) {
                spec.seg_set(*f, add_clamped(base, step));
            }
        }
        if spec != spec_now {
            self.set_primitive_spec(d.nid, spec);
            if let Some(a) = self.ui.primitive_drag.as_mut() {
                a.needs_settle = true;
                a.active_field = target;
                // Re-anchor so pointer movement (which only RETARGETS) does not fight the scroll.
                a.start_spec = spec;
                a.start_mouse = self.viewport.viewport_mouse;
            }
            let label = seg_field_label(target);
            let shown = targets
                .first()
                .and_then(|f| spec.seg_get(*f))
                .unwrap_or_default();
            self.toast(&format!("Segments [{label}]: {shown}"));
        }
        true
    }

    /// Scroll wheel — round/bevel segments. Bare scroll inside the `B` modal (matching the poly
    /// bevel modal); ignored otherwise so the camera keeps its wheel.
    /// Returns true if it consumed the event.
    pub(crate) fn primitive_scroll_round_segments(&mut self, delta: f32) -> bool {
        if delta.abs() < 0.5 || !self.primitive_round_modal_active() {
            return false;
        }
        let Some((nid, spec)) = self.primitive_chord_target() else {
            return false;
        };
        let Some(round) = spec.round() else {
            return false; // no rounding on this shape — let normal handling have the scroll
        };
        let step = if delta > 0.0 { 1i64 } else { -1 };
        let next = (round.segments as i64 + step)
            .clamp(1, core_math::primitive_spec::RoundSpec::MAX_SEGMENTS as i64)
            as u32;
        if next == round.segments {
            return true; // at the clamp — still consumed, so the camera does not also zoom
        }
        let mut spec = spec;
        // Read the seed BEFORE taking the mutable borrow (read → decide → write, rather than
        // cloning to escape the borrow checker).
        let seed = spec_round_seed(&spec).max(1e-4);
        if let Some(r) = spec.round_mut() {
            r.segments = next;
            // Scrolling segments on a primitive with no round yet is a dead control; seed a
            // visible radius so the gesture does something instead of silently no-op'ing.
            if r.radius <= 1e-6 {
                r.radius = seed;
            }
        }
        self.set_primitive_spec(nid, spec);
        // NO per-notch transaction. Committing here opened one undo entry per wheel click, so
        // undoing a few scrolls took a few Ctrl+Z — the modal commits ONCE at confirm, like every
        // other AZRA modal. (owner: "the bevel scroll bug")
        if let Some(a) = self.ui.primitive_drag.as_mut() {
            a.needs_settle = true;
            a.start_spec = spec; // re-anchor: mouse deltas measure from the new value
        }
        self.toast(&format!("Round segments: {next}"));
        true
    }

    /// Per-frame chord handling. Called once from `handle_viewport_input`.
    ///
    /// Both chords are **modals entered by a key, then driven by bare mouse movement with NO
    /// button held** (owner 2026-08-04) — the same hover shape the Pipe adjust uses, and the same
    /// confirm/cancel contract as the poly bevel modal. Building them as click-drags was the bug:
    /// with no click they sat in the LMB-press chain and could never be reached.
    ///
    /// | key | modal | mouse | scroll |
    /// |---|---|---|---|
    /// | `B` | round/bevel | X = width | segments |
    /// | `Ctrl+R` | segments | X/Y = the two visible axes | all counts |
    ///
    /// Returns true if a modal owns the pointer this frame (caller must consume the click).
    pub(crate) fn primitive_chords_frame(&mut self, ctx: &egui::Context, mouse: egui::Pos2) -> bool {
        if self.ui.primitive_drag_kind.is_none() {
            return false;
        }
        // MMB = camera orbit. The modal stays open through it and re-aims itself; it must never
        // read orbit travel as a parameter gesture.
        let orbiting = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));

        // STARTING AN ORBIT SETS the work done so far (owner 2026-08-04: "just middle click
        // rotate should also set it"). Each orientation you tune becomes its own undo step, so
        // the natural rhythm — scroll, rotate, scroll, rotate — banks progress as you go instead
        // of collapsing into one entry at the very end. The modal stays open; only the undo
        // boundary is drawn here.
        if ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Middle)) {
            let settle = self
                .ui
                .primitive_drag
                .as_ref()
                .map(|d| (d.needs_settle, d.start_spec.label()));
            if let Some((true, label)) = settle {
                self.transaction_snapshot_only(&format!("Adjust {label}"));
                if let Some(a) = self.ui.primitive_drag.as_mut() {
                    a.needs_settle = false;
                }
            }
        }

        self.primitive_drag_update(mouse, orbiting);

        // Enter confirms, Esc/RMB cancels. LMB deliberately does NOT confirm.
        //
        // It used to, following the poly bevel modal — but that made the modal a trap: the first
        // click ended it, so there was no way to keep working across several orientations (owner
        // 2026-08-04: "I cannot rotate before clicking to set — remove the set by left clicking,
        // just use rotation middle click as the trigger to set"). MMB (above) is now the
        // set-and-continue action, which is the gesture the workflow already uses to change angle.
        let confirm = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
        if cancel {
            self.primitive_drag_cancel();
            self.toast("Cancelled");
        } else if confirm {
            self.primitive_drag_confirm();
        } else if self
            .primitive_chord_target()
            .map(|(nid, _)| nid)
            .map_or(true, |nid| Some(nid) != self.ui.primitive_drag.as_ref().map(|d| d.nid))
        {
            // The selection moved to a different object (or none). With LMB no longer confirming,
            // this is what stops the modal outliving what it was editing — commit and stand down
            // rather than silently keep aiming at the old body.
            self.primitive_drag_confirm();
        }
        true
    }

    /// True while a modal owns the wheel, so decay frames never reach the camera.
    pub(crate) fn primitive_modal_scroll_active(&self) -> bool {
        self.ui.primitive_drag_kind.is_some()
    }

    /// Scroll while either modal is live. `ticks` is in wheel NOTCHES (±1 per physical click).
    ///
    /// **Always consumes when a modal is open**, even at a clamp or with nothing to change.
    /// Returning false there let the camera zoom underneath the modal — which is what happened at
    /// a segment count's minimum (owner: "when segment count is 0 zoom scroll kicks in").
    /// A modal owns the wheel for as long as it is open, full stop.
    pub(crate) fn primitive_modal_scroll(&mut self, ticks: f32, swap_dir: bool) -> bool {
        if self.ui.primitive_drag_kind.is_none() {
            return false;
        }
        // `ticks` is already in WHEEL NOTCHES (see the caller): ±1 per physical click. The
        // accumulator only exists to gather sub-notch trackpad motion into whole steps.
        let mut steps: i64 = 0;
        {
            let Some(d) = self.ui.primitive_drag.as_mut() else {
                return true;
            };
            d.scroll_accum += ticks;
            while d.scroll_accum >= 1.0 {
                d.scroll_accum -= 1.0;
                steps += 1;
            }
            while d.scroll_accum <= -1.0 {
                d.scroll_accum += 1.0;
                steps -= 1;
            }
        }
        if steps == 0 {
            return true; // consumed, just not a full notch yet
        }
        let dir = if steps > 0 { 1.0 } else { -1.0 };
        for _ in 0..steps.unsigned_abs().min(8) {
            match self.ui.primitive_drag_kind {
                Some(PrimitiveDragKind::RoundWidth) => {
                    self.primitive_scroll_round_segments(dir);
                }
                Some(PrimitiveDragKind::Segments) => {
                    self.primitive_scroll_all_segments(dir, swap_dir);
                }
                None => break,
            }
        }
        true
    }

    /// Commit the drag as ONE undo entry.
    pub(crate) fn primitive_drag_confirm(&mut self) {
        let Some(d) = self.ui.primitive_drag.take() else {
            return;
        };
        self.ui.primitive_drag_kind = None;
        if d.needs_settle {
            self.poly_request_display_sync(d.nid);
            self.transaction_snapshot_only(&format!("Adjust {}", d.start_spec.label()));
        }
    }

    /// Abandon the drag and restore the spec it started from.
    pub(crate) fn primitive_drag_cancel(&mut self) {
        let Some(d) = self.ui.primitive_drag.take() else {
            return;
        };
        self.ui.primitive_drag_kind = None;
        if d.needs_settle && self.poly_live_spec(d.nid).is_some() {
            self.set_primitive_spec(d.nid, d.start_spec);
        }
    }
}

fn add_clamped(base: u32, step: i64) -> u32 {
    (base as i64 + step).clamp(0, u32::MAX as i64) as u32
}

/// A sensible first radius when the user scrolls segments on an un-rounded primitive: 10% of the
/// safe maximum, so the round is visible but nowhere near self-intersecting.
fn spec_round_seed(spec: &PrimitiveSpec) -> f32 {
    spec.round_max_radius() * 0.1
}

/// Human label for the orientation the segments modal is targeting.
fn seg_field_label(f: Option<SegField>) -> String {
    match f {
        Some(SegField::SegX) => "X",
        Some(SegField::SegY) => "Y",
        Some(SegField::SegZ) => "Z",
        Some(SegField::SubdivW) => "width",
        Some(SegField::SubdivD) => "depth",
        Some(SegField::Radial) => "around",
        Some(SegField::Axial) => "height",
        Some(SegField::Rings) => "rings",
        Some(SegField::MinorRing) => "tube",
        Some(SegField::IcoSubdiv) => "subdiv",
        None => "-",
    }
    .to_string()
}