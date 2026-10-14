// draw_poly_wire.rs — Wireframe overlay, box/circle select overlays, NURBS overlays
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use core_math::scene_graph::NodePayload;
use crate::*;

impl AzraApp {
    // The object-mode WIREFRAME OVERLAY (`draw_wireframe_overlay`) was DELETED here and is
    // now a depth-tested GPU line pass: segments gathered in overlay_lines.rs
    // (`append_object_wire`), drawn in viewport_render.rs against the raster pass's depth
    // buffer.
    //
    // What used to live here: the same back-face-cull heuristic as the edit wireframe, and a
    // worse version of it — the face normal came from the FIRST 3 VERTICES, which returns
    // garbage for a concave or knife-cut n-gon (near-collinear leading verts => ~zero normal
    // => face misclassified as front-facing => its back edges drew through the surface). Even
    // with a correct normal the cull cannot hide a far island behind a near one. The hardware
    // depth test replaces both, so the heuristic is gone rather than kept as a second path.
    //
    // `render.wire_overlay_backface` ("show back-facing edges too") is preserved: it now
    // selects the depth_compare=Always pipeline instead of skipping the CPU cull.

    /// Draw box select rectangle overlay.
    pub(crate) fn draw_poly_box_select_overlay(&self, painter: &egui::Painter) {
        if let ActiveTool::PolyBoxSelect(ref bs) = self.tool.current {
            let r = egui::Rect::from_two_pos(bs.start, bs.current);
            let fill = egui::Color32::from_rgba_unmultiplied(80, 160, 255, 30);
            let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 160, 255));
            painter.rect(r, 0.0, fill, stroke, egui::StrokeKind::Middle);
        }
    }

    /// Knife R/C shape rubber-band — same visual language as region select (green box / circle).
    ///
    /// While dragging a **rectangle**, scroll corner-bevel rewrites `knife_points` as a rounded
    /// polyline. That path (and its yellow corner/arc points) is drawn here ungated so it always
    /// paints even when poly overlays are off — same rule as region-select rubber-band.
    ///
    /// The transparent green **fill** must follow the live cut polyline (bevel arcs + hold-R
    /// rotation). A screen AABB was used as a shortcut and stayed axis-aligned while the yellow
    /// points rotated/rounded — owner: overlay does not follow bevel or rotation.
    pub(crate) fn draw_knife_shape_select_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.poly_tool != PolyTool::Knife || self.knife_shape_kind == 0 {
            return;
        }
        let (Some(a), Some(b)) = (self.knife_shape_screen_a, self.knife_shape_screen_b) else {
            return;
        };
        // Same green as front region-select so the gesture reads as "shape under the cursor".
        // ORANGE once `E` locks it for the depth phase: the silhouette stops responding to the
        // mouse there, and an unchanged overlay made a live phase look like a stuck rectangle
        // (owner 2026-08-09: "E don't work" / "the rectangular UI stays behind").
        let base = if self.knife_shape_depth_mode {
            egui::Color32::from_rgb(255, 160, 40)
        } else {
            egui::Color32::from_rgb(80, 255, 120)
        };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 55);
        let stroke = egui::Stroke::new(1.5, base);
        let min_extent = 3.0;
        let has_poly = self.knife_points.len() >= 2;

        // Project live cut polyline once — fill + outline + yellow dots all share it so rotation
        // and corner bevel never desync from the transparent overlay.
        let screen_poly: Vec<egui::Pos2> = if has_poly {
            let cam_pos = self.viewport.camera.target
                + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
            let ortho_hw = if self.render.ortho {
                self.viewport.camera.distance
            } else {
                0.0
            };
            self.knife_points
                .iter()
                .filter_map(|&pt| {
                    world_to_screen(
                        pt,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        // Drop the closed-ring duplicate (last == first) for fill; keep stroke over full ring.
        let fill_pts: Vec<egui::Pos2> = if screen_poly.len() >= 3 {
            let mut pts = screen_poly.clone();
            if pts.len() >= 2 {
                let first = pts[0];
                let last = *pts.last().unwrap();
                if (first - last).length_sq() < 1.0 {
                    pts.pop();
                }
            }
            pts
        } else {
            Vec::new()
        };

        if fill_pts.len() >= 3 {
            // Authoritative fill: follows bevel arcs + R-rotate (same verts as the cut).
            painter.add(egui::Shape::convex_polygon(
                fill_pts,
                egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 40),
                egui::Stroke::NONE,
            ));
        } else {
            // No polyline yet (first frame of drag): fall back to screen rubber-band.
            match self.knife_shape_kind {
                1 => {
                    let mut r = if self.knife_shape_from_center {
                        let hx = (b.x - a.x).abs().max(min_extent * 0.5);
                        let hy = (b.y - a.y).abs().max(min_extent * 0.5);
                        egui::Rect::from_center_size(a, egui::vec2(hx * 2.0, hy * 2.0))
                    } else {
                        egui::Rect::from_two_pos(a, b)
                    };
                    if r.width() < min_extent && r.height() < min_extent {
                        r = egui::Rect::from_center_size(a, egui::vec2(min_extent, min_extent));
                    }
                    painter.rect(r, 0.0, fill, stroke, egui::StrokeKind::Middle);
                }
                2 => {
                    let radius = (b - a).length().max(min_extent);
                    painter.circle_filled(
                        a,
                        radius,
                        egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 35),
                    );
                    painter.circle_stroke(a, radius, stroke);
                }
                _ => {}
            }
        }

        // Outline + yellow vertices (the cut will use these points).
        if screen_poly.len() >= 2 {
            let yellow = egui::Color32::from_rgb(255, 230, 50);
            let poly_stroke = egui::Stroke::new(2.0, base);
            for w in screen_poly.windows(2) {
                painter.line_segment([w[0], w[1]], poly_stroke);
            }
            // Skip the closed-ring duplicate for dots (last ≈ first).
            let n_dots = if screen_poly.len() >= 2
                && (screen_poly[0] - *screen_poly.last().unwrap()).length_sq() < 1.0
            {
                screen_poly.len() - 1
            } else {
                screen_poly.len()
            };
            for sp in screen_poly.iter().take(n_dots) {
                painter.circle_filled(*sp, 3.0, yellow);
                painter.circle_stroke(*sp, 3.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
            }
        }

        // ── Cut-depth extrusion preview (E) ─────────────────────────────────────────────
        // Owner 2026-08-09: *"i need to see the shape extrude, otherwise user think nothing
        // append."* The depth phase moves nothing on screen by itself — the silhouette is
        // frozen by design — so the ONLY way it reads as alive is drawing the solid that is
        // about to be removed: the ring swept along −n to the current depth.
        //
        // This lives HERE and not in `draw_knife_overlay` because that function early-returns
        // to a HUD line whenever a shape is anchored (i.e. for the whole depth phase), and it
        // is also the ungated path, so the preview survives "poly overlays off" exactly like
        // the shape outline it belongs to.
        if self.knife_shape_depth_mode && self.knife_points.len() >= 2 {
            if let Some((_o, _u, _v, n)) = self.knife_free_plane {
                let cam_pos = self.viewport.camera.target
                    + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
                let aspect = rect.width() / rect.height().max(1.0);
                let ortho_hw = if self.render.ortho {
                    self.viewport.camera.distance
                } else {
                    0.0
                };
                let depth = self.knife_shape_depth;
                // Project BOTH rings here, keeping `None` in place. `screen_poly` above is a
                // `filter_map` — it silently drops points that fail to project, so its indices
                // do NOT line up with `knife_points`, and pairing rim[i] with floor[i] off it
                // would sweep the wrong corners together the moment one point went off-screen.
                let project = |p: Vec3| {
                    world_to_screen(
                        p,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    )
                };
                let rim: Vec<Option<egui::Pos2>> =
                    self.knife_points.iter().map(|&p| project(p)).collect();
                let floor: Vec<Option<egui::Pos2>> = self
                    .knife_points
                    .iter()
                    .map(|&p| project(p - n * depth))
                    .collect();
                let wall_fill = egui::Color32::from_rgba_unmultiplied(255, 160, 40, 46);
                let floor_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 160, 40));
                let rib_stroke =
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 160, 40, 130));
                // One shaded quad per ring segment: rim edge, its two side ribs, floor edge.
                // Quads (not one big polygon) so a concave ring — a rect array, a bevelled
                // corner — still reads correctly; `convex_polygon` would fill the concavity in.
                for i in 0..self.knife_points.len() - 1 {
                    let (Some(t0), Some(t1), Some(f0), Some(f1)) =
                        (rim[i], rim[i + 1], floor[i], floor[i + 1])
                    else {
                        continue;
                    };
                    painter.add(egui::Shape::convex_polygon(
                        vec![t0, t1, f1, f0],
                        wall_fill,
                        egui::Stroke::NONE,
                    ));
                    painter.line_segment([f0, f1], floor_stroke);
                    painter.line_segment([t0, f0], rib_stroke);
                }
                if let (Some(Some(t)), Some(Some(f))) = (rim.last(), floor.last()) {
                    painter.line_segment([*t, *f], rib_stroke); // closing rib
                }
                // Depth readout ON the floor, so the number sits where the cut actually ends.
                if let Some(c) = self.poly_knife_shape_center() {
                    if let Some(csp) = world_to_screen(
                        c - n * depth,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    ) {
                        painter.text(
                            csp,
                            egui::Align2::CENTER_CENTER,
                            format!("{depth:.3}"),
                            egui::FontId::proportional(13.0),
                            egui::Color32::from_rgb(255, 200, 120),
                        );
                    }
                }
            }
        }
    }

    /// Draw the RMB region-select rubber band (Box / Circle / Lasso).
    /// Green = front/visible, amber = through/x-ray (Ctrl/Alt+RMB), red = Shift deselect.
    ///
    /// MUST be called from **both** `draw_viewport_overlays` (v1) and `ui2_draw_scene_overlays` (v2).
    /// Coordinates are absolute screen points (same as pointer / press_origin).
    pub(crate) fn draw_poly_region_select_overlay(&self, painter: &egui::Painter) {
        let ActiveTool::PolyRegionSelect(ref rs) = self.tool.current else {
            return;
        };
        let base = if rs.subtract {
            egui::Color32::from_rgb(255, 90, 90) // Shift+RMB deselect
        } else if rs.front_only {
            egui::Color32::from_rgb(80, 255, 120) // front / visible — high contrast
        } else {
            egui::Color32::from_rgb(255, 200, 40) // through / x-ray
        };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 55);
        // 1.5, not 2.5 - owner 2026-08-06 "the line is too thick". Matches the viewport
        // frame indicator so the two never look like different weights of the same thing.
        let stroke = egui::Stroke::new(1.5, base);
        // Even a zero-size start still shows a crosshair so the user knows the gesture armed.
        let min_extent = 3.0;
        match rs.shape {
            SelectionShape::Box => {
                let mut r = egui::Rect::from_two_pos(rs.start, rs.current);
                if r.width() < min_extent && r.height() < min_extent {
                    r = egui::Rect::from_center_size(rs.start, egui::vec2(min_extent, min_extent));
                }
                painter.rect(r, 0.0, fill, stroke, egui::StrokeKind::Middle);
            }
            SelectionShape::Circle => {
                let radius = (rs.current - rs.start).length().max(min_extent);
                painter.circle_filled(
                    rs.start,
                    radius,
                    egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 35),
                );
                painter.circle_stroke(rs.start, radius, stroke);
            }
            SelectionShape::Lasso => {
                if rs.path.len() >= 2 {
                    let mut pts = rs.path.clone();
                    pts.push(rs.path[0]);
                    painter.add(egui::Shape::line(pts, stroke));
                } else {
                    // Armed but only one sample — show start mark
                    painter.circle_stroke(rs.start, min_extent, stroke);
                }
            }
        }
    }

    /// Draw the LMB paint-select cursor brush (only while actively painting past the click threshold).
    pub(crate) fn draw_poly_paint_select_overlay(&self, painter: &egui::Painter, mouse: egui::Pos2, radius: f32) {
        if let ActiveTool::PolyPaintSelect(ref ps) = self.tool.current {
            if ps.painted {
                // Stroke-only green brush ring (no transparent fill — owner request).
                let base = egui::Color32::from_rgb(90, 200, 120);
                painter.circle_stroke(mouse, radius, egui::Stroke::new(1.5, base));
            }
        }
    }

    /// Draw circle select brush overlay.
    pub(crate) fn draw_poly_circle_select_overlay(&self, painter: &egui::Painter, mouse: egui::Pos2) {
        if let ActiveTool::PolyCircleSelect(ref cs) = self.tool.current {
            // Stroke-only ring; amber while actively painting, white otherwise (no transparent fill).
            let ring = if cs.active {
                egui::Color32::from_rgb(255, 200, 80)
            } else {
                egui::Color32::from_rgb(255, 255, 255)
            };
            painter.circle_stroke(mouse, cs.radius, egui::Stroke::new(1.5, ring));
        }
    }

    // ================================================================
    // NURBS editing overlays (Plasticity-simple face / edge / vertex)
    // ================================================================

    /// Selection highlight: whole-face yellow outline, edge stroke, or CV dots.
    /// Works for `NurbsSolid` (face index) and legacy single `NurbsBody`.
    pub(crate) fn draw_nurbs_overlays(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.modeling_context != ModelingContext::Nurbs { return; }
        let solid = match self.nurbs_solid_root() { Some(s) => s, None => return };
        let n_faces = self.nurbs_face_count(solid);
        if n_faces == 0 { return; }

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        let sel_col = egui::Color32::from_rgb(255, 220, 40);
        let hover_col = egui::Color32::from_rgb(255, 255, 160);
        let cv_col = egui::Color32::from_rgb(200, 140, 255);
        let cv_sel = egui::Color32::from_rgb(255, 180, 50);
        // Wire: black main edges + black bevel silhouette only (no interior isoparms)
        let wire_black = egui::Color32::from_rgb(20, 20, 20);

        let project = |p: Vec3| world_to_screen(p, cam_pos, orbit, rect, aspect, ortho_hw, fov);

        // ── NURBS wire: face outline + fillet arcs only, black ──
        {
            let stroke_edge = egui::Stroke::new(1.35, wire_black);
            let stroke_bevel = egui::Stroke::new(1.5, wire_black);
            for fi in 0..n_faces {
                let fi = fi as u32;
                if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                    continue;
                }
                let xform = self.nurbs_face_world_xform(solid, fi);
                let _ = self.nurbs_with_face(solid, fi, |face| {
                    let surface = face.surface.as_ref();
                    let (u0, u1) = surface.domain_u();
                    let (v0, v1) = surface.domain_v();
                    let eval_sp = |u: f32, v: f32| -> Option<egui::Pos2> {
                        project(xform.transform_point3(surface.evaluate(u, v)))
                    };
                    let curve = |painter: &egui::Painter,
                                 ua: f32,
                                 va: f32,
                                 ub: f32,
                                 vb: f32,
                                 steps: u32,
                                 stroke: egui::Stroke| {
                        let mut prev = None;
                        for i in 0..=steps {
                            let t = i as f32 / steps as f32;
                            let u = ua + (ub - ua) * t;
                            let v = va + (vb - va) * t;
                            // Follow kept region (trim-aware outline)
                            if !face.uv_inside(u, v) {
                                prev = None;
                                continue;
                            }
                            if let Some(sp) = eval_sp(u, v) {
                                if let Some(pp) = prev {
                                    painter.line_segment([pp, sp], stroke);
                                }
                                prev = Some(sp);
                            } else {
                                prev = None;
                            }
                        }
                    };
                    // Plasticity-style wire: planar = full outline; Fillet/Corner = seam
                    // rails only (full UV dump on spheres = clover isoparm clutter).
                    let name = surface.name.as_str();
                    match name {
                        "Fillet" => {
                            // Quarter-cylinder: u=0/u=1 = planar seams; v=0/v=1 = free-end
                            // quarter-circles (owner free-end silhouette — must not omit).
                            curve(painter, u0, v0, u0, v1, 32, stroke_bevel);
                            curve(painter, u1, v0, u1, v1, 32, stroke_bevel);
                            curve(painter, u0, v0, u1, v0, 28, stroke_bevel);
                            curve(painter, u0, v1, u1, v1, 28, stroke_bevel);
                        }
                        "Corner" => {
                            // Spherical octant: three boundary arcs (u=0, u=1, v=0).
                            // Skip degenerate pole at v=1.
                            curve(painter, u0, v0, u0, v1, 28, stroke_bevel);
                            curve(painter, u1, v0, u1, v1, 28, stroke_bevel);
                            curve(painter, u0, v0, u1, v0, 28, stroke_bevel);
                        }
                        _ => {
                            // Planar: UV boundary, but **trim-aware** (uv_inside). Never draw
                            // domain edges through free-end sector holes — that painted the
                            // ghost yellow triangle on owner free-end screenshots.
                            curve(painter, u0, v0, u0, v1, 28, stroke_edge);
                            curve(painter, u0, v1, u1, v1, 28, stroke_edge);
                            curve(painter, u1, v1, u1, v0, 28, stroke_edge);
                            curve(painter, u1, v0, u0, v0, 28, stroke_edge);
                        }
                    }
                    // Bevel silhouette = Arc trims only (free-end quarter-circles on side faces).
                    // Do NOT draw hole radial Line segments (those made the triangle legs).
                    for loop_ in &face.trims {
                        for c in &loop_.curves {
                            if let core_math::nurbs::TrimCurve2d::Arc {
                                centre,
                                radius,
                                t0,
                                t1,
                            } = c
                            {
                                let mut d = t1 - t0;
                                while d > std::f32::consts::PI {
                                    d -= 2.0 * std::f32::consts::PI;
                                }
                                while d < -std::f32::consts::PI {
                                    d += 2.0 * std::f32::consts::PI;
                                }
                                let steps = 32u32;
                                let mut prev = None;
                                for i in 0..=steps {
                                    let t = t0 + d * (i as f32 / steps as f32);
                                    let uv =
                                        *centre + glam::Vec2::new(t.cos(), t.sin()) * *radius;
                                    if let Some(sp) = eval_sp(uv.x, uv.y) {
                                        if let Some(pp) = prev {
                                            painter.line_segment([pp, sp], stroke_bevel);
                                        }
                                        prev = Some(sp);
                                    } else {
                                        prev = None;
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }

        // Selection boundary: trim-aware (skip UV samples outside kept region).
        let draw_boundary = |painter: &egui::Painter, solid: core_math::scene_graph::NodeId, fi: u32, stroke: egui::Stroke, steps: u32| {
            let xform = self.nurbs_face_world_xform(solid, fi);
            let _ = self.nurbs_with_face(solid, fi, |face| {
                let surface = face.surface.as_ref();
                let (u0, u1) = surface.domain_u();
                let (v0, v1) = surface.domain_v();
                let edges = [(u0, v0, u0, v1), (u0, v1, u1, v1), (u1, v1, u1, v0), (u1, v0, u0, v0)];
                for (ua, va, ub, vb) in edges {
                    let mut prev: Option<egui::Pos2> = None;
                    for i in 0..=steps {
                        let t = i as f32 / steps as f32;
                        let u = ua + (ub - ua) * t;
                        let v = va + (vb - va) * t;
                        if !face.uv_inside(u, v) {
                            prev = None;
                            continue;
                        }
                        let p = xform.transform_point3(surface.evaluate(u, v));
                        if let Some(sp) = project(p) {
                            if let Some(pp) = prev {
                                painter.line_segment([pp, sp], stroke);
                            }
                            prev = Some(sp);
                        } else {
                            prev = None;
                        }
                    }
                }
                // Free-end Arc trims as selection outline (not radial lines)
                for loop_ in &face.trims {
                    for c in &loop_.curves {
                        if let core_math::nurbs::TrimCurve2d::Arc {
                            centre,
                            radius,
                            t0,
                            t1,
                        } = c
                        {
                            let mut d = t1 - t0;
                            while d > std::f32::consts::PI {
                                d -= 2.0 * std::f32::consts::PI;
                            }
                            while d < -std::f32::consts::PI {
                                d += 2.0 * std::f32::consts::PI;
                            }
                            let mut prev = None;
                            for i in 0..=steps {
                                let t = t0 + d * (i as f32 / steps as f32);
                                let uv = *centre + glam::Vec2::new(t.cos(), t.sin()) * *radius;
                                let p = xform.transform_point3(surface.evaluate(uv.x, uv.y));
                                if let Some(sp) = project(p) {
                                    if let Some(pp) = prev {
                                        painter.line_segment([pp, sp], stroke);
                                    }
                                    prev = Some(sp);
                                } else {
                                    prev = None;
                                }
                            }
                        }
                    }
                }
            });
        };

        // Full-face fill (poly-style): tessellate UV grid → screen triangles.
        let draw_face_fill = |painter: &egui::Painter, solid: core_math::scene_graph::NodeId, fi: u32, fill: egui::Color32, grid: u32| {
            let xform = self.nurbs_face_world_xform(solid, fi);
            self.nurbs_with_face_surface(solid, fi, |surface| {
                let (u0, u1) = surface.domain_u();
                let (v0, v1) = surface.domain_v();
                let g = grid.max(2);
                let mut pts: Vec<Option<egui::Pos2>> = Vec::with_capacity(((g + 1) * (g + 1)) as usize);
                for iu in 0..=g {
                    let u = u0 + (u1 - u0) * iu as f32 / g as f32;
                    for iv in 0..=g {
                        let v = v0 + (v1 - v0) * iv as f32 / g as f32;
                        let p = xform.transform_point3(surface.evaluate(u, v));
                        pts.push(project(p));
                    }
                }
                let stride = g + 1;
                for iu in 0..g {
                    for iv in 0..g {
                        let i00 = (iu * stride + iv) as usize;
                        let i01 = i00 + 1;
                        let i10 = ((iu + 1) * stride + iv) as usize;
                        let i11 = i10 + 1;
                        if let (Some(a), Some(b), Some(c)) = (pts[i00], pts[i10], pts[i11]) {
                            painter.add(egui::Shape::convex_polygon(
                                vec![a, b, c],
                                fill,
                                egui::Stroke::NONE,
                            ));
                        }
                        if let (Some(a), Some(c), Some(d)) = (pts[i00], pts[i11], pts[i01]) {
                            painter.add(egui::Shape::convex_polygon(
                                vec![a, c, d],
                                fill,
                                egui::Stroke::NONE,
                            ));
                        }
                    }
                }
            });
        };
        let sel_fill = egui::Color32::from_rgba_unmultiplied(255, 220, 40, 70);
        let hover_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 160, 45);

        // Front-facing only: never draw back faces through the solid (Plasticity-clean).
        // Selection keys are NurbsFaceId — resolve to current index every frame (R12).
        match self.nurbs_work_mode {
            NurbsWorkMode::Object => {
                // Modeless: highlight hovered/selected face only (front-facing).
                for fi in 0..n_faces {
                    let fi = fi as u32;
                    if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                        continue;
                    }
                    let Some(fid) = self.nurbs_face_id_at(solid, fi) else {
                        continue;
                    };
                    if self.nurbs_sel.selected_faces.contains(&fid) {
                        draw_face_fill(painter, solid, fi, sel_fill, 8);
                        draw_boundary(painter, solid, fi, egui::Stroke::new(2.5, sel_col), 20);
                    } else if self.nurbs_sel.hovered_face == Some(fid)
                        && self.nurbs_sel.hovered_cv.is_none()
                        && self.nurbs_sel.hovered_edge.is_none()
                    {
                        draw_face_fill(painter, solid, fi, hover_fill, 6);
                        draw_boundary(painter, solid, fi, egui::Stroke::new(2.0, hover_col), 16);
                    }
                }
                // Hover edge / CV in object mode — geometry from CURRENT face list.
                if let Some((fid, ei)) = self.nurbs_sel.hovered_edge {
                    if let Some(fi) = self.nurbs_face_index(solid, fid) {
                        if self.nurbs_face_front_facing(solid, fi, cam_pos) {
                            let xform = self.nurbs_face_world_xform(solid, fi);
                            self.nurbs_with_face_surface(solid, fi, |surface| {
                                let (u0, u1) = surface.domain_u();
                                let (v0, v1) = surface.domain_v();
                                let edge_uv = [(u0, v0, u0, v1), (u0, v1, u1, v1), (u1, v1, u1, v0), (u1, v0, u0, v0)];
                                let (ua, va, ub, vb) = edge_uv[ei as usize % 4];
                                let stroke = egui::Stroke::new(2.5, hover_col);
                                let mut prev = None;
                                for i in 0..=16 {
                                    let t = i as f32 / 16.0;
                                    let p = xform.transform_point3(surface.evaluate(
                                        ua + (ub - ua) * t, va + (vb - va) * t));
                                    if let Some(sp) = project(p) {
                                        if let Some(pp) = prev { painter.line_segment([pp, sp], stroke); }
                                        prev = Some(sp);
                                    } else { prev = None; }
                                }
                            });
                        }
                    }
                }
                if let Some((row, col)) = self.nurbs_sel.hovered_cv {
                    if let Some(fid) = self.nurbs_sel.hovered_face {
                        if let Some(fi) = self.nurbs_face_index(solid, fid) {
                            if self.nurbs_face_front_facing(solid, fi, cam_pos) {
                                let xform = self.nurbs_face_world_xform(solid, fi);
                                self.nurbs_with_face_surface(solid, fi, |surface| {
                                    if let Some(cp) = surface.control_points.get(row as usize)
                                        .and_then(|r| r.get(col as usize))
                                    {
                                        if let Some(sp) = project(xform.transform_point3(cp.position)) {
                                            painter.circle_filled(sp, 4.5, hover_col);
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
            }
            NurbsWorkMode::Face => {
                for fi in 0..n_faces {
                    let fi = fi as u32;
                    if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                        continue;
                    }
                    let Some(fid) = self.nurbs_face_id_at(solid, fi) else {
                        continue;
                    };
                    if self.nurbs_sel.selected_faces.contains(&fid) {
                        draw_face_fill(painter, solid, fi, sel_fill, 8);
                        draw_boundary(painter, solid, fi, egui::Stroke::new(2.5, sel_col), 20);
                    } else if self.nurbs_sel.hovered_face == Some(fid) {
                        draw_face_fill(painter, solid, fi, hover_fill, 6);
                        draw_boundary(painter, solid, fi, egui::Stroke::new(2.0, hover_col), 16);
                    }
                }
            }
            NurbsWorkMode::Edge => {
                for fi in 0..n_faces {
                    let fi = fi as u32;
                    if !self.nurbs_face_front_facing(solid, fi, cam_pos) {
                        continue;
                    }
                    let Some(fid) = self.nurbs_face_id_at(solid, fi) else {
                        continue;
                    };
                    let xform = self.nurbs_face_world_xform(solid, fi);
                    // Trim-aware: never stroke UV samples outside free-end sector holes
                    // (that drew the ghost triangle legs on notched side faces).
                    let _ = self.nurbs_with_face(solid, fi, |face| {
                        let surface = face.surface.as_ref();
                        let (u0, u1) = surface.domain_u();
                        let (v0, v1) = surface.domain_v();
                        let edge_uv = [(u0, v0, u0, v1), (u0, v1, u1, v1), (u1, v1, u1, v0), (u1, v0, u0, v0)];
                        for (ei, (ua, va, ub, vb)) in edge_uv.iter().enumerate() {
                            let key = (fid, ei as u8);
                            let stroke = if self.nurbs_sel.selected_edges.contains(&key) {
                                Some(egui::Stroke::new(3.0, sel_col))
                            } else if self.nurbs_sel.hovered_edge == Some(key) {
                                Some(egui::Stroke::new(2.5, hover_col))
                            } else {
                                None
                            };
                            let Some(stroke) = stroke else { continue };
                            let mut prev = None;
                            for i in 0..=24 {
                                let t = i as f32 / 24.0;
                                let u = ua + (ub - ua) * t;
                                let v = va + (vb - va) * t;
                                if !face.uv_inside(u, v) {
                                    prev = None;
                                    continue;
                                }
                                let p = xform.transform_point3(surface.evaluate(u, v));
                                if let Some(sp) = project(p) {
                                    if let Some(pp) = prev {
                                        painter.line_segment([pp, sp], stroke);
                                    }
                                    prev = Some(sp);
                                } else {
                                    prev = None;
                                }
                            }
                        }
                    });
                }
            }
            NurbsWorkMode::Vertex => {
                // CVs only when vertex mode, and only on a front-facing face.
                let fi = match self.nurbs_active_face_index() {
                    Some(f) if self.nurbs_face_front_facing(solid, f, cam_pos) => f,
                    _ => {
                        // Fall back to first front-facing face for lattice.
                        let mut found = None;
                        for i in 0..n_faces {
                            let i = i as u32;
                            if self.nurbs_face_front_facing(solid, i, cam_pos) {
                                found = Some(i);
                                break;
                            }
                        }
                        match found {
                            Some(f) => f,
                            None => return,
                        }
                    }
                };
                let xform = self.nurbs_face_world_xform(solid, fi);
                let lattice = egui::Color32::from_rgba_unmultiplied(200, 140, 255, 90);
                self.nurbs_with_face_surface(solid, fi, |surface| {
                    let rows = surface.control_points.len();
                    let cols_n = if rows > 0 { surface.control_points[0].len() } else { 0 };
                    let mut screen: Vec<Vec<Option<egui::Pos2>>> = Vec::with_capacity(rows);
                    for row in &surface.control_points {
                        let mut sr = Vec::with_capacity(row.len());
                        for cp in row {
                            sr.push(project(xform.transform_point3(cp.position)));
                        }
                        screen.push(sr);
                    }
                    for r in 0..rows {
                        for c in 0..cols_n.saturating_sub(1) {
                            if let (Some(a), Some(b)) = (screen[r][c], screen[r][c + 1]) {
                                painter.line_segment([a, b], egui::Stroke::new(1.0, lattice));
                            }
                        }
                    }
                    for c in 0..cols_n {
                        for r in 0..rows.saturating_sub(1) {
                            if let (Some(a), Some(b)) = (screen[r][c], screen[r + 1][c]) {
                                painter.line_segment([a, b], egui::Stroke::new(1.0, lattice));
                            }
                        }
                    }
                    for (r, row) in screen.iter().enumerate() {
                        for (c, sp) in row.iter().enumerate() {
                            if let Some(sp) = *sp {
                                let key = (r as u32, c as u32);
                                if self.nurbs_sel.selected_cvs.contains(&key) {
                                    painter.circle_filled(sp, 5.0, cv_sel);
                                    painter.circle_stroke(
                                        sp,
                                        6.0,
                                        egui::Stroke::new(1.5, egui::Color32::WHITE),
                                    );
                                } else if self.nurbs_sel.hovered_cv == Some(key) {
                                    painter.circle_filled(sp, 4.5, hover_col);
                                } else {
                                    painter.circle_filled(sp, 3.0, cv_col);
                                }
                            }
                        }
                    }
                });
            }
        }
    }
}
