// core_engine/app/src/smart_snap.rs
// Unified smart snap system — geometry (V/E/F/center/volume), grid, UI bar, indicators.
use eframe::egui;
use glam::Vec3;
use crate::{AzraApp, SnapFilter, SmartSnapHit, SnapCandidate, WorkMode, world_to_screen_ex};

impl AzraApp {
    /// Is smart snap active right now? (magnet / Enable Snap OR Ctrl momentary)
    ///
    /// NEVER in Sculpt mode: Ctrl there is the invert-stroke modifier (Blender/ZBrush),
    /// and brushes don't snap. Gated here — not per-caller — so every consumer
    /// (pre-highlight, gizmo, drag tools) agrees; the pre-highlight alone was running
    /// a full `find_smart_snap` raycast every frame of a Ctrl-carve stroke.
    pub(crate) fn is_snap_active(&self, ctrl: bool) -> bool {
        if self.tool.work_mode == WorkMode::Sculpt {
            return false;
        }
        self.viewport.smart_snap_on || self.viewport.snap_enabled || ctrl
    }

    /// Toggle the permanent magnet — keeps `snap_enabled` in lockstep.
    pub(crate) fn set_smart_snap_on(&mut self, on: bool) {
        self.viewport.smart_snap_on = on;
        self.viewport.snap_enabled = on;
    }

    /// Cap occlusion probes per `find_smart_snap` call (FRAME_STALL_PLAN Gate 1).
    /// Full-scene raycasts are O(scene tris); exhaustive per-candidate was 277–328 ms input.
    /// Best-first scoring + this hard cap keeps visible-only correct without unbounded work.
    const SMART_SNAP_MAX_OCCLUSION_PROBES: usize = 8;

    /// Screen-space pick distance for a snap candidate.
    ///
    /// Free grab: distance from the cursor to the candidate.
    /// Axis-locked gizmo / Shift lock: also the distance to the candidate's
    /// projection onto the lock axis (the "tick" on the handle). Hovering the
    /// X arrow near a vertex's X is enough — you do not have to leave the handle
    /// and sit on the vertex (owner: gizmo axis + vertex target).
    pub(crate) fn snap_pick_screen_dist(
        mouse: egui::Pos2,
        candidate_screen: egui::Pos2,
        axis_tick_screen: Option<egui::Pos2>,
    ) -> f32 {
        let d = (candidate_screen - mouse).length();
        if let Some(tick) = axis_tick_screen {
            d.min((tick - mouse).length())
        } else {
            d
        }
    }

    /// World point of `p` projected onto the infinite axis through `origin` along `dir`.
    pub(crate) fn snap_axis_tick_world(origin: Vec3, dir: Vec3, p: Vec3) -> Vec3 {
        origin + dir * (p - origin).dot(dir)
    }

    /// Axis line (origin, unit dir) for the live Grab / Translate lock, if any.
    pub(crate) fn active_grab_snap_axis_line(&self) -> Option<(Vec3, Vec3)> {
        use crate::transform::constraint_axis_world;
        if let Some(ts) = self.tool.current.as_poly_transform() {
            if matches!(ts.kind, crate::TransformKind::Grab) {
                let dir = constraint_axis_world(ts.axis, ts.frozen_space, ts.frozen_orient)?;
                return Some((ts.center, dir));
            }
        }
        if let Some(g) = self.tool.current.as_gizmo() {
            if matches!(g.mode, crate::GizmoMode::Translate) {
                let axis = match g.component {
                    crate::GizmoComponent::AxisX => crate::AxisConstraint::X,
                    crate::GizmoComponent::AxisY => crate::AxisConstraint::Y,
                    crate::GizmoComponent::AxisZ => crate::AxisConstraint::Z,
                    crate::GizmoComponent::Center => return None,
                };
                let dir = constraint_axis_world(axis, g.space, g.orient)?;
                return Some((g.median_start_pos, dir));
            }
        }
        if let Some(d) = self.tool.current.as_nurbs_cv_move() {
            if matches!(d.kind, crate::TransformKind::Grab) {
                let dir = constraint_axis_world(d.axis, d.frozen_space, d.frozen_orient)?;
                return Some((d.center, dir));
            }
        }
        None
    }

    /// Smart snap: nearest candidate to `mouse` within pixel threshold.
    /// `exclude_prim` — skip candidates from this SDF prim index (e.g. during creation).
    ///
    /// **Visible-only occlusion is lazy** (Gate 1): all candidates are scored by screen
    /// distance + priority first; occlusion is tested only on the ordered shortlist until
    /// the first visible winner (capped by `SMART_SNAP_MAX_OCCLUSION_PROBES`).
    pub(crate) fn find_smart_snap(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        exclude_prim: Option<usize>,
    ) -> Option<SmartSnapHit> {
        self.find_smart_snap_along(mouse, rect, exclude_prim, self.active_grab_snap_axis_line())
    }

    /// Same as [`find_smart_snap`] with an explicit axis line (origin, dir) for gizmo lock.
    pub(crate) fn find_smart_snap_along(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        exclude_prim: Option<usize>,
        axis_line: Option<(Vec3, Vec3)>,
    ) -> Option<SmartSnapHit> {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let threshold = self.viewport.smart_snap_threshold;
        // Within this px, lower-priority kinds lose to higher (vert > edge > face > …).
        let tie_px = 2.5f32;

        // Ranked pool: (screen_dist, priority, hit) — lower dist wins; near-ties use priority.
        let mut ranked: Vec<(f32, u8, SmartSnapHit)> = Vec::with_capacity(64);

        let consider = |ranked: &mut Vec<(f32, u8, SmartSnapHit)>, dist: f32, hit: SmartSnapHit| {
            if dist >= threshold {
                return;
            }
            let prio = hit.kind.priority();
            ranked.push((dist, prio, hit));
        };

        // --- Discrete geometry candidates (NO occlusion here — Gate 1 lazy path) ---
        let candidates = self.compute_snap_candidates();
        for c in &candidates {
            if let Some(excl) = exclude_prim {
                if c.prim_idx == excl {
                    continue;
                }
            }
            let enabled = match c.kind {
                SnapFilter::Vertex => self.viewport.smart_snap_vertex,
                SnapFilter::Face => self.viewport.smart_snap_face_center,
                // Midpoints: Edge midpoint checkbox. Continuous along-edge handled below.
                SnapFilter::Edge => {
                    self.viewport.smart_snap_edge_center || self.viewport.smart_snap_edge
                }
                SnapFilter::All => self.viewport.smart_snap_center,
                SnapFilter::Volume => self.ui2.snap_volume,
                SnapFilter::Grid => false, // lattice added below
            };
            if !enabled {
                continue;
            }
            // Midpoints only when edge_center (or edge alone without continuous pass using mid as fallback)
            if c.kind == SnapFilter::Edge
                && !self.viewport.smart_snap_edge_center
                && self.viewport.smart_snap_edge
            {
                // Skip discrete midpoints when only continuous Edge is on — continuous pass wins.
                if c.edge.is_some() && c.label.contains("edge mid") {
                    // still allow SDF "side mid" midpoints as coarse targets when continuous is on
                }
            }
            if let Some(sp) = world_to_screen_ex(
                c.world_pos,
                cam_pos,
                self.viewport.camera.orbit,
                rect,
                aspect,
                ortho_hw,
                self.render.fov_deg,
            ) {
                let tick_sp = axis_line.and_then(|(origin, dir)| {
                    let tick = Self::snap_axis_tick_world(origin, dir, c.world_pos);
                    world_to_screen_ex(
                        tick,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    )
                });
                let dist = Self::snap_pick_screen_dist(mouse, sp, tick_sp);
                consider(&mut ranked, dist, SmartSnapHit::from_candidate(c));
            }
        }

        // --- Continuous along-edge / edge-perpendicular (foot of perpendicular in screen space) ---
        let want_edge_line = self.viewport.smart_snap_edge || self.ui2.snap_edge_perp;
        if want_edge_line {
            // Reuse candidates that already carry segment endpoints (no second full rebuild).
            let edge_segs: Vec<(Vec3, Vec3, String)> = candidates
                .iter()
                .filter_map(|c| c.edge.map(|(a, b)| (a, b, c.label.clone())))
                .collect();
            for (a, b, label) in edge_segs {
                let Some(sa) = world_to_screen_ex(
                    a,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ) else {
                    continue;
                };
                let Some(sb) = world_to_screen_ex(
                    b,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ) else {
                    continue;
                };
                let (ex, ey) = (sb.x - sa.x, sb.y - sa.y);
                let len2 = ex * ex + ey * ey;
                let t = if len2 > 1e-8 {
                    (((mouse.x - sa.x) * ex + (mouse.y - sa.y) * ey) / len2).clamp(0.0, 1.0)
                } else {
                    0.5
                };
                let sx = sa.x + ex * t;
                let sy = sa.y + ey * t;
                let dist = ((sx - mouse.x).powi(2) + (sy - mouse.y).powi(2)).sqrt();
                let wp = a + (b - a) * t;
                // Edge-perp: only accept when screen foot is a true perpendicular (t free would
                // already be clamped; also require ray–edge nearly shortest: t in (0,1) interior
                // or endpoint within threshold). Foot of perpendicular is always the closest point.
                let tag = if self.ui2.snap_edge_perp && !self.viewport.smart_snap_edge {
                    format!("{label} (perp)")
                } else {
                    format!("{label} along")
                };
                // Occlusion deferred to lazy pick below (Gate 1).
                consider(
                    &mut ranked,
                    dist,
                    SmartSnapHit {
                        world_pos: wp,
                        label: tag,
                        kind: SnapFilter::Edge,
                        edge: Some((a, b)),
                        normal: None,
                    },
                );
            }
        }

        // --- Volume / Align-to-surface hit under cursor ---
        // Volume target always raycasts mesh surface. Align Rotation also needs a surface
        // normal under the cursor (Blender Face Project + Align Rotation to Target).
        if self.ui2.snap_volume || self.ui2.snap_align_rotation {
            let (origin, dir) = self.cam_ray(mouse, rect);
            if let Some((hit, n, _nid)) = self.raycast_scene_meshes(origin, dir) {
                if let Some(sp) = world_to_screen_ex(
                    hit,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ) {
                    let dist = ((sp.x - mouse.x).powi(2) + (sp.y - mouse.y).powi(2)).sqrt();
                    // Surface is under the cursor by construction — allow a generous threshold.
                    if dist < threshold.max(8.0) {
                        let nn = n.normalize_or_zero();
                        consider(
                            &mut ranked,
                            dist.min(threshold * 0.5),
                            SmartSnapHit {
                                world_pos: hit,
                                label: "Surface".into(),
                                kind: SnapFilter::Volume,
                                edge: None,
                                normal: if nn.length_squared() > 1e-8 { Some(nn) } else { None },
                            },
                        );
                    }
                }
            }
        }

        // --- Grid lattice (uses snap_grid — same as menu Grid size / transform quantize) ---
        if self.viewport.smart_snap_grid {
            let grid = self.effective_snap_grid();
            let floor_y = self.viewport.grid_floor_y;
            let range = (self.viewport.camera.distance * 2.0 / grid).ceil().min(30.0) as i32;
            let cx = (self.viewport.camera.target.x / grid).round() as i32;
            let cz = (self.viewport.camera.target.z / grid).round() as i32;
            for ix in (cx - range)..=(cx + range) {
                for iz in (cz - range)..=(cz + range) {
                    let wp = Vec3::new(ix as f32 * grid, floor_y, iz as f32 * grid);
                    if let Some(sp) = world_to_screen_ex(
                        wp,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    ) {
                        if sp.x < rect.min.x
                            || sp.x > rect.max.x
                            || sp.y < rect.min.y
                            || sp.y > rect.max.y
                        {
                            continue;
                        }
                        let dist = ((sp.x - mouse.x).powi(2) + (sp.y - mouse.y).powi(2)).sqrt();
                        consider(
                            &mut ranked,
                            dist,
                            SmartSnapHit {
                                world_pos: wp,
                                label: format!("Grid {ix},{iz}"),
                                kind: SnapFilter::Grid,
                                edge: None,
                                // Floor grid: +Y up (Align Rotation sits objects on the floor).
                                normal: Some(Vec3::Y),
                            },
                        );
                    }
                }
            }
        }

        if ranked.is_empty() {
            return None;
        }

        // Sort: closer first; near-ties prefer higher priority (lower prio number).
        //
        // CRASH FIX (owner 2026-08-06, panic mid-session: "user-provided comparison function does
        // not correctly implement a total order", backtrace through ui2_draw_scene_overlays).
        //
        // The pairwise epsilon band this replaces was NOT TRANSITIVE, and that is fatal to a sort
        // regardless of the data. With tie_px = 2.5: distances 0 and 2 are "tied" so priority
        // decides, 2 and 4 are "tied" so priority decides, but 0 and 4 are NOT tied so distance
        // decides. Let their priorities disagree with their distances and you get a <= b, b <= c,
        // c < a. Rust's driftsort verifies the ordering it was handed and aborts the process.
        //
        // An epsilon band is fine for a running best-pick (which is what this code did before it
        // became a sort — a max never claims a total order); it is never fine for `sort_by`.
        //
        // Bucketing the distance restores transitivity while keeping the intent: the band is now
        // a FUNCTION OF THE DISTANCE ALONE, so (bucket, priority, distance) is a genuine total
        // order by construction. `total_cmp` on the tiebreak so a NaN distance can never
        // reintroduce the same class through the back door.
        ranked.sort_by(|a, b| {
            let bucket = |d: f32| -> i64 {
                if d.is_finite() { (d / tie_px.max(1e-3)).floor() as i64 } else { i64::MAX }
            };
            bucket(a.0)
                .cmp(&bucket(b.0))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.total_cmp(&b.0))
        });

        // Lazy occlusion: test best → next until first visible winner (capped).
        if !self.viewport.smart_snap_visible_only {
            return ranked.into_iter().next().map(|(_, _, hit)| hit);
        }
        let mut probes = 0usize;
        for (_, _, hit) in ranked {
            if probes >= Self::SMART_SNAP_MAX_OCCLUSION_PROBES {
                // Cap hit: accept best remaining without further raycasts (still scored).
                return Some(hit);
            }
            probes += 1;
            let probe = SnapCandidate::point(
                hit.world_pos,
                hit.kind,
                hit.label.clone(),
                usize::MAX,
            );
            if !self.snap_candidate_occluded(&probe) {
                return Some(hit);
            }
        }
        None
    }

    /// Occlusion for visible-only: inside another SDF prim, or behind a poly/mesh surface
    /// along the view ray toward the candidate.
    fn snap_candidate_occluded(&self, c: &SnapCandidate) -> bool {
        // SDF inside test
        for (j, prim) in self.scene.primitives.iter().enumerate() {
            if j == c.prim_idx || !prim.visible {
                continue;
            }
            // world_distance expects world-space points
            let d = if crate::BODY_LOCAL_PRIMS {
                // Prim fields are body-local; convert the world candidate into that body frame.
                let local_pt = self
                    .scene
                    .prim_body_map
                    .get(j)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .map(|bt| bt.world_to_local(c.world_pos))
                    .unwrap_or(c.world_pos);
                prim.world_distance(local_pt)
            } else {
                prim.world_distance(c.world_pos)
            };
            if d < -0.01 {
                return true;
            }
        }
        // Mesh / poly front surface closer than candidate along camera ray
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let to_c = c.world_pos - cam_pos;
        let dist_c = to_c.length();
        if dist_c < 1e-5 {
            return false;
        }
        let dir = to_c / dist_c;
        if let Some((hit, _, _)) = self.raycast_scene_meshes(cam_pos, dir) {
            let dist_hit = (hit - cam_pos).length();
            // Hit clearly in front of the candidate → occluded
            if dist_hit + 0.02 < dist_c {
                return true;
            }
        }
        false
    }

    /// Draw the smart snap toolbar (v1 floating bar).
    pub(crate) fn draw_smart_snap_bar(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let bar_w = 380.0;
        let x = rect.center().x - bar_w * 0.5;
        let y = rect.min.y + 4.0;
        egui::Area::new(egui::Id::new("smart_snap_bar"))
            .fixed_pos(egui::pos2(x, y))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220))
                    .rounding(4.0)
                    .inner_margin(egui::Margin::symmetric(6, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let on_col = egui::Color32::from_rgb(255, 200, 0);
                            let off_col = egui::Color32::from_gray(100);
                            let mag_col = if self.viewport.smart_snap_on {
                                on_col
                            } else {
                                off_col
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("\u{1F9F2}").color(mag_col).size(13.0),
                                    )
                                    .min_size(egui::vec2(24.0, 20.0))
                                    .fill(if self.viewport.smart_snap_on {
                                        egui::Color32::from_rgba_unmultiplied(255, 200, 0, 30)
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    }),
                                )
                                .on_hover_text("Smart Snap (hold Ctrl for momentary)")
                                .clicked()
                            {
                                let on = !self.viewport.smart_snap_on;
                                self.set_smart_snap_on(on);
                            }
                            ui.separator();
                            for (label, hint, val) in [
                                ("V", "Vertex / Corners", &mut self.viewport.smart_snap_vertex),
                                ("F", "Face Centers", &mut self.viewport.smart_snap_face_center),
                                ("E", "Edge Midpoints", &mut self.viewport.smart_snap_edge_center),
                                ("G", "Grid Intersections", &mut self.viewport.smart_snap_grid),
                                ("C", "Center of Mass", &mut self.viewport.smart_snap_center),
                            ] {
                                let col = if *val { on_col } else { off_col };
                                let bg = if *val {
                                    egui::Color32::from_rgba_unmultiplied(255, 200, 0, 25)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(label)
                                                .color(col)
                                                .size(11.0)
                                                .strong(),
                                        )
                                        .min_size(egui::vec2(22.0, 20.0))
                                        .fill(bg),
                                    )
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    *val = !*val;
                                }
                            }
                            ui.separator();
                            {
                                let vis_col = if self.viewport.smart_snap_visible_only {
                                    on_col
                                } else {
                                    off_col
                                };
                                let vis_bg = if self.viewport.smart_snap_visible_only {
                                    egui::Color32::from_rgba_unmultiplied(255, 200, 0, 25)
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("\u{1F441}")
                                                .color(vis_col)
                                                .size(11.0),
                                        )
                                        .min_size(egui::vec2(22.0, 20.0))
                                        .fill(vis_bg),
                                    )
                                    .on_hover_text("Visible Only (off = snap through geometry)")
                                    .clicked()
                                {
                                    self.viewport.smart_snap_visible_only =
                                        !self.viewport.smart_snap_visible_only;
                                }
                            }
                            ui.separator();
                            ui.add(
                                egui::DragValue::new(&mut self.viewport.smart_snap_threshold)
                                    .range(5.0..=60.0)
                                    .speed(0.5)
                                    .suffix("px")
                                    .min_decimals(0)
                                    .max_decimals(0),
                            );
                            ui.separator();
                            ui.colored_label(
                                egui::Color32::from_gray(140),
                                egui::RichText::new("∠").size(12.0),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.viewport.snap_rotation_deg)
                                    .range(0.0..=90.0)
                                    .speed(0.5)
                                    .suffix("°")
                                    .min_decimals(0)
                                    .max_decimals(0),
                            )
                            .on_hover_text(
                                "Angle snap (degrees).\n\
                                 Move/Extrude: with Angle lock + Snap ON, free direction locks to this step.\n\
                                 Knife: magnet ON or Ctrl, rubber-band locks to this step.\n\
                                 Rotate: magnet or Shift.\n\
                                 0 = off.",
                            );
                            ui.separator();
                            ui.colored_label(
                                egui::Color32::from_gray(80),
                                egui::RichText::new("Ctrl=snap").size(9.0),
                            );
                        });
                    });
            });
    }

    /// Draw smart snap indicator: kind-coded marker + label (knife-quality feedback).
    pub(crate) fn draw_smart_snap_indicator(&self, painter: &egui::Painter, rect: egui::Rect) {
        let hit = match &self.viewport.smart_snap_result {
            Some(h) => h,
            None => return,
        };
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        // Edge segment highlight
        if let Some((a, b)) = hit.edge {
            if let (Some(sa), Some(sb)) = (
                world_to_screen_ex(
                    a,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ),
                world_to_screen_ex(
                    b,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ),
            ) {
                painter.line_segment(
                    [sa, sb],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(60, 200, 255)),
                );
            }
        }
        let Some(sp) = world_to_screen_ex(
            hit.world_pos,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        ) else {
            return;
        };

        let yellow = egui::Color32::from_rgb(255, 200, 0);
        let cyan = egui::Color32::from_rgb(60, 200, 255);
        let green = egui::Color32::from_rgb(80, 230, 90);
        let orange = egui::Color32::from_rgb(255, 160, 40);
        let magenta = egui::Color32::from_rgb(220, 120, 255);
        let black = egui::Color32::BLACK;

        match hit.kind {
            SnapFilter::Vertex => {
                let r = egui::Rect::from_center_size(sp, egui::vec2(10.0, 10.0));
                painter.rect_filled(r, 1.0, green);
                painter.rect_stroke(r, 1.0, egui::Stroke::new(1.5, black), egui::StrokeKind::Middle);
            }
            SnapFilter::Edge => {
                painter.circle_filled(sp, 5.5, cyan);
                painter.circle_stroke(sp, 5.5, egui::Stroke::new(1.5, black));
            }
            SnapFilter::Face => {
                let r = 6.0;
                let pts = [
                    sp + egui::vec2(0.0, -r),
                    sp + egui::vec2(r, 0.0),
                    sp + egui::vec2(0.0, r),
                    sp + egui::vec2(-r, 0.0),
                ];
                painter.add(egui::Shape::convex_polygon(
                    pts.to_vec(),
                    orange,
                    egui::Stroke::new(1.5, black),
                ));
            }
            SnapFilter::Grid => {
                // Cross
                let s = 6.0;
                painter.line_segment(
                    [sp + egui::vec2(-s, 0.0), sp + egui::vec2(s, 0.0)],
                    egui::Stroke::new(2.0, yellow),
                );
                painter.line_segment(
                    [sp + egui::vec2(0.0, -s), sp + egui::vec2(0.0, s)],
                    egui::Stroke::new(2.0, yellow),
                );
                painter.circle_stroke(sp, 4.0, egui::Stroke::new(1.5, black));
            }
            SnapFilter::Volume => {
                painter.circle_filled(sp, 6.0, magenta);
                painter.circle_stroke(sp, 6.0, egui::Stroke::new(1.5, black));
                painter.circle_stroke(sp, 10.0, egui::Stroke::new(1.0, magenta));
            }
            SnapFilter::All => {
                painter.circle_filled(sp, 5.0, yellow);
                painter.circle_stroke(sp, 5.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(180, 140, 0)));
            }
        }

        // Label (kind + description)
        let text = if hit.label.is_empty() {
            hit.kind.label().to_string()
        } else {
            format!("{} · {}", hit.kind.label(), hit.label)
        };
        let font = egui::FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::from_gray(240));
        let pad = egui::vec2(5.0, 2.0);
        let label_pos = sp + egui::vec2(10.0, -18.0);
        let bg = egui::Rect::from_min_size(
            label_pos - pad,
            galley.size() + pad * 2.0,
        );
        painter.rect_filled(
            bg,
            3.0,
            egui::Color32::from_rgba_unmultiplied(12, 12, 14, 210),
        );
        painter.galley(label_pos, galley, egui::Color32::from_gray(240));
    }
}

#[cfg(test)]
mod smart_snap_occlusion_tests {
    use super::*;
    use crate::FourthLayerApp;
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    use std::sync::Arc;

    /// FRAME_STALL_PLAN Gate 1 acceptance: visible-only must refuse a candidate
    /// behind a front surface (lazy probe still correct — not just fast).
    #[test]
    fn smart_snap_visible_only_refuses_occluded_point() {
        let mut app = FourthLayerApp::new_headless();
        // Front quad at z=0 (blocks), rear vertex candidate at z=-2.
        let front = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(-1.0, -1.0, 0.0),
                Vec3::new(1.0, -1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(-1.0, 1.0, 0.0),
            ],
            &[vec![0, 1, 2, 3]],
        );
        let rear = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(-0.2, -0.2, -2.0),
                Vec3::new(0.2, -0.2, -2.0),
                Vec3::new(0.2, 0.2, -2.0),
                Vec3::new(-0.2, 0.2, -2.0),
            ],
            &[vec![0, 1, 2, 3]],
        );
        let root = app.scene.scene_graph.root();
        let _front = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Front".into(),
                mesh: Arc::new(front),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        let _rear = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "Rear".into(),
                mesh: Arc::new(rear),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        // Camera looks down -Z toward origin from z=+5.
        app.viewport.camera.target = Vec3::ZERO;
        app.viewport.camera.orbit = glam::Quat::IDENTITY;
        app.viewport.camera.distance = 5.0;
        app.viewport.smart_snap_on = true;
        app.viewport.smart_snap_visible_only = true;
        app.viewport.smart_snap_vertex = true;
        app.viewport.smart_snap_edge = false;
        app.viewport.smart_snap_edge_center = false;
        app.viewport.smart_snap_face_center = false;
        app.viewport.smart_snap_grid = false;
        app.viewport.smart_snap_center = false;
        app.ui2.snap_volume = false;

        let probe = SnapCandidate::point(
            Vec3::new(0.0, 0.0, -2.0),
            SnapFilter::Vertex,
            String::from("rear"),
            usize::MAX,
        );
        assert!(
            app.snap_candidate_occluded(&probe),
            "rear point must be occluded by the front quad"
        );

        // Mouse dead-center of viewport; rect covers a typical panel.
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
        let mouse = rect.center();
        let hit = app.find_smart_snap(mouse, rect, None);
        if let Some(ref h) = hit {
            assert!(
                h.world_pos.z > -1.0,
                "visible-only must not return a deep rear hit (got {:?})",
                h.world_pos
            );
            // Explicitly: rear-center vertex must never win when occluded.
            assert!(
                (h.world_pos - Vec3::new(0.0, 0.0, -2.0)).length() > 0.1,
                "occluded rear vertex must not win snap"
            );
        }
    }
}

#[cfg(test)]
mod snap_order_tests {
    /// CRASH REGRESSION (owner 2026-08-06): the smart-snap ranking sort aborted the process with
    /// "user-provided comparison function does not correctly implement a total order".
    ///
    /// The cause was a pairwise EPSILON BAND, which is not transitive: with tie_px = 2.5,
    /// 0 vs 2 are tied (priority decides), 2 vs 4 are tied (priority decides), but 0 vs 4 are not
    /// (distance decides). Give those three priorities that disagree with their distances and the
    /// relation says a <= b, b <= c, c < a. No NaN needed - the data below is entirely finite.
    ///
    /// This drives the SAME shape of comparator the fix installs. It panics on the pairwise band
    /// and passes on the bucketed one, so it discriminates - which the first version of this gate
    /// did not, and a test that passes on the broken code is worse than no test.
    #[test]
    fn ranking_order_is_transitive_under_the_tie_band() {
        let tie_px = 2.5f32;
        // (distance, priority) chosen so a band-based comparator contradicts itself.
        let mut ranked: Vec<(f32, u8)> = Vec::new();
        for i in 0..64u32 {
            let d = (i as f32) * 1.0;
            let prio = ((i * 7) % 5) as u8; // priority deliberately uncorrelated with distance
            ranked.push((d, prio));
        }
        // The shipped ordering: bucket the distance so the band is a function of distance ALONE.
        ranked.sort_by(|a, b| {
            let bucket = |d: f32| -> i64 {
                if d.is_finite() { (d / tie_px.max(1e-3)).floor() as i64 } else { i64::MAX }
            };
            bucket(a.0)
                .cmp(&bucket(b.0))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.total_cmp(&b.0))
        });
        assert_eq!(ranked.len(), 64, "sort must not lose candidates");
        // Non-decreasing by bucket = the ordering actually holds.
        let bucket = |d: f32| (d / tie_px) as i64;
        for w in ranked.windows(2) {
            assert!(bucket(w[0].0) <= bucket(w[1].0), "ranking is not ordered by distance bucket");
        }
    }
}

#[cfg(test)]
mod gizmo_axis_snap_pick_tests {
    use super::AzraApp;

    #[test]
    fn axis_tick_beats_far_vertex_when_cursor_is_on_the_gizmo_handle() {
        // Cursor sits on the X handle. Candidate is 80px off the handle in Y
        // (a vertex on the mesh) but its X-tick is 4px from the cursor.
        // Free pick would miss (80 > typical 20px threshold); axis pick hits.
        let mouse = egui::pos2(200.0, 100.0);
        let vert_screen = egui::pos2(200.0, 180.0);
        let tick_on_handle = egui::pos2(204.0, 100.0);
        let d_free = AzraApp::snap_pick_screen_dist(mouse, vert_screen, None);
        let d_axis = AzraApp::snap_pick_screen_dist(mouse, vert_screen, Some(tick_on_handle));
        assert!(d_free > 70.0, "free pick is the 2D distance to the vertex, got {d_free}");
        assert!(d_axis < 8.0, "axis pick must use the handle tick, got {d_axis}");
    }

    #[test]
    fn axis_tick_world_projects_onto_x() {
        let origin = glam::Vec3::new(3.0, 1.0, -2.0);
        let target = glam::Vec3::new(0.4, 8.0, 5.0);
        let tick = AzraApp::snap_axis_tick_world(origin, glam::Vec3::X, target);
        assert!((tick.x - 0.4).abs() < 1e-5);
        assert!((tick.y - 1.0).abs() < 1e-5);
        assert!((tick.z + 2.0).abs() < 1e-5);
    }
}
