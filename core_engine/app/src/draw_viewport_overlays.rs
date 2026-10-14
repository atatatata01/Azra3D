// draw_viewport_overlays.rs — Snap indicator, selection overlay, brush preview,
//   toasts, grid overlay, horizon line, depth pick, billboard camera icon
// Extracted from drawing.rs
use eframe::egui;
use glam::{Quat, Vec3};
use core_math::primitives::SdfShape;
use core_math::sdf_volume::BrushType;
use crate::*;

impl AzraApp {
    /// Draw snap indicator: cyan diamond at snapped position with label.
    pub(crate) fn draw_snap_indicator(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let (wp, label, fade) = match &self.viewport.snap_indicator {
            Some(si) if si.fade > 0 => (si.world_pos, si.label, si.fade),
            _ => { self.viewport.snap_indicator = None; return; }
        };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        if let Some(sp) = world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let alpha = ((fade as f32 / 12.0) * 255.0) as u8;
            let col = egui::Color32::from_rgba_unmultiplied(0, 220, 200, alpha);
            let s = self.tweak.snap_indicator_px;
            painter.add(egui::Shape::closed_line(
                vec![sp + egui::vec2(0.0, -s), sp + egui::vec2(s, 0.0),
                     sp + egui::vec2(0.0, s),  sp + egui::vec2(-s, 0.0)],
                egui::Stroke::new(2.0, col),
            ));
            painter.circle_filled(sp, 2.5, col);
            painter.text(sp + egui::vec2(0.0, -14.0), egui::Align2::CENTER_BOTTOM,
                label, egui::FontId::monospace(10.0), col);
        }
        // Fade out only when not actively dragging
        if !self.tool.current.is_gizmo() && !self.tool.current.is_transform() {
            if let Some(ref mut si) = self.viewport.snap_indicator { si.fade = si.fade.saturating_sub(1); }
        }
    }

    // ---- Selection overlay ----
    pub(crate) fn draw_selection_overlay(&self, painter:&egui::Painter, cur:egui::Pos2) {
        // Blue = select; red = Shift+RMB deselect marquee
        let (o, os) = if self.tool.selection_box_subtract {
            (
                egui::Color32::from_rgba_unmultiplied(255, 90, 90, 40),
                egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 90, 90, 200)),
            )
        } else {
            (
                egui::Color32::from_rgba_unmultiplied(80, 160, 255, 25),
                egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(80, 160, 255, 180)),
            )
        };
        match self.tool.current.as_selection_box() {
            Some(SelectionDrag::Box{start})=>{let r=egui::Rect::from_two_pos(*start,cur);painter.rect_filled(r,0.0,o);painter.rect_stroke(r,0.0,os, egui::StrokeKind::Middle);}
            Some(SelectionDrag::Lasso{center})=>{let r=(cur-*center).length();painter.circle_filled(*center,r,o);painter.circle_stroke(*center,r,os);}
            None=>{}
        }
    }

    /// Project a circle given in `world`-matrix local space (`local_c`, plane normal `n`,
    /// radius `local_r`) to a screen-space polyline plus its projected centre.
    ///
    /// SHARED projector — extracted from the sculpt brush ring so the proportional-edit
    /// falloff ring reuses the same maths instead of copying it. Returns `None` when the
    /// normal is degenerate or when more than a quarter of the ring falls outside the
    /// projection (behind the camera / off-frustum), which is the caller's "don't draw"
    /// signal. The 1% outward nudge along `n` keeps the ring off the surface it hugs.
    pub(crate) fn project_world_ring(
        &self, rect: egui::Rect, world: glam::Mat4, local_c: Vec3, n: Vec3, local_r: f32,
    ) -> Option<(Vec<egui::Pos2>, Option<egui::Pos2>)> {
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        let n = n.normalize_or_zero();
        if n.length_squared() < 1e-8 { return None; }
        let up = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let mut tu = up.cross(n);
        if tu.length_squared() < 1e-8 { return None; }
        tu = tu.normalize();
        let tv = n.cross(tu).normalize();
        const SEGS: usize = 48;
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(SEGS);
        let nudge = n * (local_r * 0.01);
        for i in 0..SEGS {
            let a = (i as f32 / SEGS as f32) * std::f32::consts::TAU;
            let local_p = local_c + (tu * a.cos() + tv * a.sin()) * local_r + nudge;
            let wp = world.transform_point3(local_p);
            if let Some(sp) = world_to_screen(wp, cam_pos, orbit, rect, aspect, ortho_hw, fov) {
                pts.push(sp);
            }
        }
        if pts.len() < SEGS * 3 / 4 { return None; }
        let center_sp = world_to_screen(
            world.transform_point3(local_c + nudge), cam_pos, orbit, rect, aspect, ortho_hw, fov,
        );
        Some((pts, center_sp))
    }

    // ---- Brush preview ----
    pub(crate) fn draw_brush_preview(&self, painter:&egui::Painter, rect:egui::Rect, mouse:egui::Pos2, shift_held:bool, ctrl_held:bool) {
        // RMB-drag brush adjust: pin the ring to the gesture ANCHOR (ZBrush) — the cursor
        // is off measuring dx/dy, and a ring chasing it reads as noise, not as a size preview.
        let mouse = match self.sculpt.rmb_adjust {
            Some((a, _, _)) => egui::pos2(a.x, a.y),
            None => mouse,
        };
        if self.tool.work_mode!=WorkMode::Sculpt||!rect.contains(mouse){return;}
        // The sculpt brush ring must ONLY draw when there is an actual sculpt SUBJECT. `work_mode` and
        // `modeling_context` are orthogonal fields: entering the poly Knife (or any poly/nurbs/curve edit)
        // leaves a stale `work_mode == Sculpt`, and the legacy `else` branch below then drew a green
        // brush ring + center dot over the cursor during poly editing (the reported "green circle" that
        // rides on top of the knife's own overlay). Gate on a live voxel sculpt object; anything else
        // (no voxel + non-SDF context) has nothing to sculpt, so no ring. SDF sculpt has its OWN preview
        // (`draw_sdf_brush_preview`, gated on `sdf_sculpt_active`), so this one stays voxel-only.
        if self.sculpt.voxel.is_none()
            && self.sculpt.poly_edit.is_none()
            && self.modeling_context != ModelingContext::Sdf
            // Pre-first-stroke poly hover: a sculptable target in OBJECT poly mode gets the
            // ring too (poly_edit only arms on the first drag). The Object-mode gate keeps
            // the knife/edit-mode ghost-ring class out (those force non-Object modes).
            && !(self.poly_work_mode == crate::PolyWorkMode::Object
                && self.poly_sculpt_target().is_some())
        {
            return;
        }
        // Voxel room: drive the cursor from the LIVE voxel brush radius, world-scaled by the
        // object's node, so resizing (Z/X, slider, RMB-drag) actually changes the circle.
        // Falls back to the legacy BrushSettings for any non-voxel sculpt path.
        // `inner_frac` = the falloff's 50%-strength distance as a fraction of the radius —
        // the inner indicator ring (ZBrush focal shift / Blender inner circle). Solved from
        // the SAME `falloff_weight_preset` the stamp uses, so it can never lie about feel.
        // None (Constant preset / legacy path) = no inner ring.
        let poly_ring_nid = self.sculpt.poly_edit.map(|(n, _)| n).or_else(|| {
            (self.poly_work_mode == crate::PolyWorkMode::Object)
                .then(|| self.poly_sculpt_target())
                .flatten()
        });
        let (sr, c, inner_frac) = if let Some(nid) = poly_ring_nid {
            use core_math::voxel_sculpt::VoxelBrush;
            let s = self.scene.scene_graph.world_transform(nid).to_scale_rotation_translation().0;
            let ws = (s.x.abs() + s.y.abs() + s.z.abs()) / 3.0;
            let sr = self.poly_sculpt_radius() * ws * rect.height()
                / (self.viewport.camera.distance.max(0.1) * 1.5);
            let eff = if shift_held {
                VoxelBrush::Smooth
            } else {
                self.sculpt.poly_brush.unwrap_or(VoxelBrush::Draw)
            };
            let col = if ctrl_held && !shift_held {
                egui::Color32::from_rgba_unmultiplied(200, 80, 80, 120)
            } else {
                match eff {
                    VoxelBrush::Draw => egui::Color32::from_rgba_unmultiplied(80, 200, 80, 120),
                    VoxelBrush::Erase => egui::Color32::from_rgba_unmultiplied(200, 80, 80, 120),
                    VoxelBrush::Smooth => egui::Color32::from_rgba_unmultiplied(80, 140, 220, 120),
                    VoxelBrush::Flatten => egui::Color32::from_rgba_unmultiplied(220, 200, 80, 120),
                    VoxelBrush::Clay => egui::Color32::from_rgba_unmultiplied(180, 140, 80, 120),
                    VoxelBrush::Scrape => egui::Color32::from_rgba_unmultiplied(200, 60, 60, 120),
                    VoxelBrush::Crease => egui::Color32::from_rgba_unmultiplied(160, 80, 200, 120),
                    VoxelBrush::Fill => egui::Color32::from_rgba_unmultiplied(120, 180, 120, 120),
                    VoxelBrush::ClayBuildup => egui::Color32::from_rgba_unmultiplied(200, 160, 100, 120),
                    VoxelBrush::Inflate => egui::Color32::from_rgba_unmultiplied(200, 120, 220, 120),
                    VoxelBrush::Pinch => egui::Color32::from_rgba_unmultiplied(220, 160, 60, 120),
                    VoxelBrush::Move => egui::Color32::from_rgba_unmultiplied(120, 200, 220, 120),
                    VoxelBrush::Smudge => egui::Color32::from_rgba_unmultiplied(150, 210, 210, 120),
                    VoxelBrush::SnakeHook => egui::Color32::from_rgba_unmultiplied(150, 150, 230, 120),
                }
            };
            let inner = core_math::voxel_sculpt::falloff_half_radius_t(
                self.poly_sculpt_hardness(),
                self.sculpt.poly_tool.falloff_preset,
            );
            (sr, col, inner)
        } else if let Some(v) = self.sculpt.voxel.as_ref() {
            use core_math::voxel_sculpt::VoxelBrush;
            let s = self.scene.scene_graph.world_transform(v.node).to_scale_rotation_translation().0;
            let ws = (s.x.abs()+s.y.abs()+s.z.abs())/3.0;
            let sr = v.radius*ws*rect.height()/(self.viewport.camera.distance.max(0.1)*1.5);
            let eff = if shift_held { VoxelBrush::Smooth } else { v.brush };
            let col = if ctrl_held && !shift_held { egui::Color32::from_rgba_unmultiplied(200,80,80,120) }
                else { match eff {
                    VoxelBrush::Draw=>egui::Color32::from_rgba_unmultiplied(80,200,80,120),
                    VoxelBrush::Erase=>egui::Color32::from_rgba_unmultiplied(200,80,80,120),
                    VoxelBrush::Smooth=>egui::Color32::from_rgba_unmultiplied(80,140,220,120),
                    VoxelBrush::Flatten=>egui::Color32::from_rgba_unmultiplied(220,200,80,120),
                    VoxelBrush::Clay=>egui::Color32::from_rgba_unmultiplied(180,140,80,120),
                    VoxelBrush::Scrape=>egui::Color32::from_rgba_unmultiplied(200,60,60,120),
                    VoxelBrush::Crease=>egui::Color32::from_rgba_unmultiplied(160,80,200,120),
                    VoxelBrush::Fill=>egui::Color32::from_rgba_unmultiplied(120,180,120,120),
                    VoxelBrush::ClayBuildup=>egui::Color32::from_rgba_unmultiplied(200,160,100,120),
                    VoxelBrush::Inflate=>egui::Color32::from_rgba_unmultiplied(200,120,220,120),
                    VoxelBrush::Pinch=>egui::Color32::from_rgba_unmultiplied(220,160,60,120),
                    VoxelBrush::Move=>egui::Color32::from_rgba_unmultiplied(120,200,220,120),
                    // Smudge = Move's soft-falloff sibling — a paler teal so the two read as related.
                    VoxelBrush::Smudge=>egui::Color32::from_rgba_unmultiplied(150,210,210,120),
                    // Snake Hook = tube pull (Move family) — a distinct violet-teal.
                    VoxelBrush::SnakeHook=>egui::Color32::from_rgba_unmultiplied(150,150,230,120),
                }};
            let inner = core_math::voxel_sculpt::falloff_half_radius_t(v.falloff, v.falloff_preset);
            (sr, col, inner)
        } else {
            let sr=self.brush.size*rect.height()/(self.viewport.camera.distance.max(0.1)*1.5);
            let effective = if shift_held { BrushType::Smooth } else if ctrl_held { self.brush.brush_type.inverted() } else { self.brush.brush_type };
            let c=match effective{BrushType::Add=>egui::Color32::from_rgba_unmultiplied(80,200,80,120),BrushType::Subtract=>egui::Color32::from_rgba_unmultiplied(200,80,80,120),BrushType::Smooth=>egui::Color32::from_rgba_unmultiplied(80,140,220,120),BrushType::Flatten=>egui::Color32::from_rgba_unmultiplied(220,200,80,120),BrushType::Inflate=>egui::Color32::from_rgba_unmultiplied(200,120,220,120),BrushType::Pinch=>egui::Color32::from_rgba_unmultiplied(220,160,60,120),BrushType::Clay=>egui::Color32::from_rgba_unmultiplied(180,140,80,120),BrushType::Crease=>egui::Color32::from_rgba_unmultiplied(160,80,200,120),BrushType::Trim=>egui::Color32::from_rgba_unmultiplied(200,60,60,120)};
            (sr, c, None)
        };
        // Inner falloff ring style: same hue, quieter + thinner than the outer ring.
        let inner_col = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 70);

        // ---- Oriented brush ring (Direction-aware) ----
        // CENTER tracks the mouse. ORIENTATION: Lock = first-click normal; Surf = live.
        // Move/Smudge grab: BOTH start (anchor) and end (cursor) rings use the FIRST-CLICK surface
        // normal — never the viewport/view-plane normal when the cursor leaves the mesh. Parallel
        // discs + generators read as a cone/frustum of the grab falloff (3D-Coat style).
        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let orbit = self.viewport.camera.orbit;
        let fov = self.render.fov_deg;

        // Project a local-space circle (center, normal, radius) → screen polyline + center.
        // Thin adapter over the shared `project_world_ring` so the sculpt brush ring and the
        // proportional-edit falloff ring cannot drift apart (one projector, two callers).
        let project_ring = |world: glam::Mat4, local_c: Vec3, n: Vec3, local_r: f32|
            -> Option<(Vec<egui::Pos2>, Option<egui::Pos2>)>
        {
            self.project_world_ring(rect, world, local_c, n, local_r)
        };

        // Local points on a ring for generator lines (start ↔ end cone).
        let ring_samples = |local_c: Vec3, n: Vec3, local_r: f32, count: usize| -> Option<Vec<Vec3>> {
            let n = n.normalize_or_zero();
            if n.length_squared() < 1e-8 { return None; }
            let up = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
            let mut tu = up.cross(n);
            if tu.length_squared() < 1e-8 { return None; }
            tu = tu.normalize();
            let tv = n.cross(tu).normalize();
            let nudge = n * (local_r * 0.01);
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let a = (i as f32 / count as f32) * std::f32::consts::TAU;
                out.push(local_c + (tu * a.cos() + tv * a.sin()) * local_r + nudge);
            }
            Some(out)
        };

        let mut drew_oriented = false;
        if let Some(v) = self.sculpt.voxel.as_ref() {
            // Move = mesh_grab (Blender); Smudge = elastic_grab (density).
            let grab_on = v.mesh_grab.is_active() || v.elastic_grab.is_active();
            let world = self.scene.scene_graph.world_transform(v.node);
            let inv = world.inverse();
            // Ring hit must match the stamp tip (no density-vs-gbuf fight mid-stroke).
            //   • Mid-stroke: prefer `stroke_last_surface` (same stick as the brush).
            //   • Atlas/CPU-stale: prefer live gbuf (density is pre-stroke → jumps/misses).
            //   • Fresh hover: density raycast + density normal (pre-brush skin).
            let (ro_w, rd_w) = self.cam_ray(mouse, rect);
            let ro_l = inv.transform_point3(ro_w);
            let rd_l = inv.transform_vector3(rd_w).normalize_or_zero();
            let atlas_live = v.atlas_stroke || v.cpu_grid_stale;
            let density_hit = v.grid.raycast(ro_l, rd_l).and_then(|h| {
                let n = v.grid.normal_at(h);
                if n.length_squared() > 1e-12 {
                    Some((h, n.normalize_or_zero()))
                } else {
                    None
                }
            });
            let gbuf_hit = v.last_surface_hit.filter(|(_, n)| n.length_squared() > 1e-8)
                .map(|(h, n)| (h, n.normalize_or_zero()));
            let surface = if self.sculpt.stroke_active {
                // Same plane the stamps just used — ring never brick-jumps away from the tip.
                v.stroke_last_surface
                    .or(if atlas_live { gbuf_hit.or(density_hit) } else { density_hit.or(gbuf_hit) })
            } else if atlas_live {
                gbuf_hit.or(density_hit)
            } else {
                match (density_hit, gbuf_hit) {
                    (Some((h, n)), _) => Some((h, n)),
                    (None, Some((h, n))) => {
                        let n2 = v.grid.normal_at(h);
                        let n = if n2.length_squared() > 1e-12 { n2.normalize_or_zero() } else { n };
                        Some((h, n))
                    }
                    _ => None,
                }
            };

            if grab_on {
                // ---- Move/Smudge: first-point normal on BOTH ends (cone falloff) ----
                // Viewport gbuf often has NO valid surface under the cursor mid-pull (empty /
                // floor / silhouette). Never re-pick orientation from that — lock to the
                // first-click surface normal latched at grab begin.
                let (first_n, plane_n, start_c, local_r) = if v.mesh_grab.is_active() {
                    let g = &v.mesh_grab;
                    let n = if g.normal.length_squared() > 1e-8 {
                        g.normal
                    } else {
                        v.first_point_normal.unwrap_or(Vec3::ZERO)
                    };
                    (n, g.plane_n, g.anchor, g.outer_r)
                } else {
                    let n = if v.elastic_grab.surface_n.length_squared() > 1e-8 {
                        v.elastic_grab.surface_n
                    } else {
                        v.first_point_normal.unwrap_or(Vec3::ZERO)
                    };
                    (n, v.elastic_grab.plane_n, v.elastic_grab.anchor, v.elastic_grab.radius)
                };
                if first_n.length_squared() > 1e-8 {
                    let (ro_w, rd_w) = self.cam_ray(mouse, rect);
                    let ro = inv.transform_point3(ro_w);
                    let rd = inv.transform_vector3(rd_w).normalize_or_zero();
                    let pn = plane_n;
                    let denom = rd.dot(pn);
                    let end_c = if denom.abs() > 1e-5 {
                        let t = (start_c - ro).dot(pn) / denom;
                        if t > 0.0 { ro + rd * t } else { start_c }
                    } else {
                        start_c
                    };
                    let n = first_n.normalize_or_zero();

                    // Start ring (grab footprint) — slightly quieter so end reads as the active tip.
                    let start_col = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90);
                    if let Some((pts, center_sp)) = project_ring(world, start_c, n, local_r) {
                        painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.5, start_col)));
                        if let Some(sp) = center_sp {
                            painter.circle_filled(sp, 2.5, start_col);
                        }
                    }
                    // End ring under cursor — SAME first-point normal (parallel disc → cone with start).
                    if let Some((pts, center_sp)) = project_ring(world, end_c, n, local_r) {
                        let label_y = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                        let label_x = center_sp.map(|p| p.x).unwrap_or(mouse.x);
                        painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.0, c)));
                        if let Some(sp) = center_sp {
                            painter.circle_filled(sp, 3.0, c);
                        } else {
                            painter.circle_filled(mouse, 3.0, c);
                        }
                        if self.brush.symmetry_x {
                            painter.text(
                                egui::pos2(label_x, label_y - 10.0),
                                egui::Align2::CENTER_BOTTOM,
                                "🦋",
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_rgba_unmultiplied(255, 200, 100, 160),
                            );
                        }
                        drew_oriented = true;
                    }
                    // Generators: start ring → end ring (frustum / cone silhouette of the pull).
                    if let (Some(s_pts), Some(e_pts)) = (
                        ring_samples(start_c, n, local_r, 8),
                        ring_samples(end_c, n, local_r, 8),
                    ) {
                        let gen_col = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 70);
                        let gen_stroke = egui::Stroke::new(1.0, gen_col);
                        for i in 0..s_pts.len().min(e_pts.len()) {
                            let a = world_to_screen(
                                world.transform_point3(s_pts[i]), cam_pos, orbit, rect, aspect, ortho_hw, fov,
                            );
                            let b = world_to_screen(
                                world.transform_point3(e_pts[i]), cam_pos, orbit, rect, aspect, ortho_hw, fov,
                            );
                            if let (Some(sa), Some(sb)) = (a, b) {
                                painter.line_segment([sa, sb], gen_stroke);
                            }
                        }
                        // Axis: grab anchor → cursor tip.
                        if let (Some(sa), Some(sb)) = (
                            world_to_screen(
                                world.transform_point3(start_c + n * (local_r * 0.01)),
                                cam_pos, orbit, rect, aspect, ortho_hw, fov,
                            ),
                            world_to_screen(
                                world.transform_point3(end_c + n * (local_r * 0.01)),
                                cam_pos, orbit, rect, aspect, ortho_hw, fov,
                            ),
                        ) {
                            painter.line_segment(
                                [sa, sb],
                                egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 110)),
                            );
                        }
                    }
                }
            } else if let Some((h, density_n)) = surface {
                // Hover / non-grab: Direction resolves orientation.
                // `density_n` is ALWAYS the pre-brush density gradient under the tip (see above).
                // Surface / Normal / Average sit the disk on that skin — never a late gbuf normal.
                let view_local = {
                    let cam_l = inv.transform_point3(cam_pos);
                    (cam_l - h).normalize_or_zero()
                };
                // Spatial Area normal (Avg region × radius) so the ring tilts when knobs change.
                let area_n = v.grid.sample_area_normal_fast(h, v.radius, v.dir_average_region.clamp(0.25, 4.0));
                // Mid-stroke Original: keep latched first-click density normal.
                // Surface extrude forces live surface plane (same as stamp producer).
                let effective_dir = if v.surface_extrude {
                    core_math::voxel_sculpt::BrushDirection::Surface
                } else {
                    v.brush_dir
                };
                // Path direction for Brush/Tangent when a stroke is active.
                let stroke_dir = if self.sculpt.stroke_active {
                    v.stroke_prev_pos.map(|prev| h - prev)
                } else {
                    None
                };
                let mut n = effective_dir
                    .resolve(core_math::voxel_sculpt::BrushDirContext {
                        surface: density_n,
                        view: view_local,
                        first_point: v.first_point_normal,
                        average: if area_n.length_squared() > 1e-12 {
                            Some(area_n)
                        } else if v.average_normal_count > 0 {
                            Some(v.average_normal_sum / v.average_normal_count as f32)
                        } else {
                            None
                        },
                        stroke_dir,
                    })
                    .normalize_or_zero();
                // Smooth Normal: blend toward area normal (same as stroke producer).
                let sm = v.dir_smooth_normal.clamp(0.0, 1.0);
                if sm > 1e-4 && area_n.length_squared() > 1e-12 {
                    n = (n * (1.0 - sm) + area_n * sm).normalize_or_zero();
                    if n.length_squared() < 1e-12 { n = area_n; }
                }
                // Degenerate resolve → fall back to density skin under tip.
                if n.length_squared() < 1e-12 {
                    n = density_n;
                }
                if let Some((pts, center_sp)) = project_ring(world, h, n, v.radius) {
                    let label_y = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                    let label_x = center_sp.map(|p| p.x).unwrap_or(mouse.x);
                    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.0, c)));
                    // Inner 50%-strength ring — same center/normal, radius scaled by the
                    // falloff's half-strength point.
                    if let Some(f) = inner_frac {
                        if let Some((ipts, _)) = project_ring(world, h, n, v.radius * f) {
                            painter.add(egui::Shape::closed_line(ipts, egui::Stroke::new(1.0, inner_col)));
                        }
                    }
                    if let Some(sp) = center_sp {
                        painter.circle_filled(sp, 3.0, c);
                    } else {
                        painter.circle_filled(mouse, 3.0, c);
                    }
                    if self.brush.symmetry_x {
                        painter.text(
                            egui::pos2(label_x, label_y - 10.0),
                            egui::Align2::CENTER_BOTTOM,
                            "🦋",
                            egui::FontId::proportional(12.0),
                            egui::Color32::from_rgba_unmultiplied(255, 200, 100, 160),
                        );
                    }
                    drew_oriented = true;
                }
            }
        }

        // ---- Poly sculpt: ring ON the surface (Blender paint_cursor) ----
        // `poly_cursor_hit` is the per-frame budgeted raycast (hover + mid-stroke). Miss
        // (None) → the flat 2D fallback below, exactly Blender's no-hit cursor.
        if !drew_oriented && self.sculpt.voxel.is_none() {
            if let (Some(nid), Some((hp, hn))) = (poly_ring_nid, self.sculpt.poly_cursor_hit) {
                let n = hn.normalize_or_zero();
                if n.length_squared() > 1e-10 {
                    let world = self.scene.scene_graph.world_transform(nid);
                    let local_r = self.poly_sculpt_radius();
                    if let Some((pts, center_sp)) = project_ring(world, hp, n, local_r) {
                        painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.0, c)));
                        if let Some(f) = inner_frac {
                            if let Some((ipts, _)) = project_ring(world, hp, n, local_r * f) {
                                painter.add(egui::Shape::closed_line(
                                    ipts,
                                    egui::Stroke::new(1.0, inner_col),
                                ));
                            }
                        }
                        if let Some(sp) = center_sp {
                            painter.circle_filled(sp, 3.0, c);
                        }
                        drew_oriented = true;
                    }
                }
            }
        }
        if !drew_oriented {
            painter.circle_stroke(mouse, sr, egui::Stroke::new(2.0, c));
            if let Some(f) = inner_frac {
                painter.circle_stroke(mouse, sr * f, egui::Stroke::new(1.0, inner_col));
            }
            painter.circle_filled(mouse, 3.0, c);
            if self.brush.symmetry_x {
                painter.text(
                    egui::pos2(mouse.x, mouse.y - sr - 10.0),
                    egui::Align2::CENTER_BOTTOM,
                    "🦋",
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgba_unmultiplied(255, 200, 100, 160),
                );
            }
        }
        // Live readout while the RMB adjust gesture is held.
        if self.sculpt.rmb_adjust.is_some() {
            let (r, s) = if let Some(v) = self.sculpt.voxel.as_ref() {
                (v.radius, v.strength)
            } else {
                (self.sculpt.poly_radius, self.sculpt.poly_strength)
            };
            painter.text(
                egui::pos2(mouse.x, mouse.y + sr + 8.0),
                egui::Align2::CENTER_TOP,
                format!("Size {r:.2}  ·  Strength {s:.2}"),
                egui::FontId::monospace(11.0),
                egui::Color32::from_rgba_unmultiplied(230, 230, 235, 220),
            );
        }
    }

    // ---- Toasts ----
    pub(crate) fn draw_toasts(&mut self,ctx:&egui::Context) {
        let dt=ctx.input(|i|i.predicted_dt); self.ui.toasts.retain_mut(|t|{t.remaining-=dt;t.remaining>0.0});
        if self.ui.toasts.is_empty(){return;}
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::LEFT_BOTTOM,[10.0,-10.0]).order(egui::Order::Foreground).show(ctx,|ui|{
            for t in &self.ui.toasts { let a=(t.remaining.min(0.5)*2.0*255.0)as u8; ui.colored_label(egui::Color32::from_rgba_unmultiplied(255,220,100,a),&t.message); }
        });
    }

    // ---- Floor grid (egui stub) ----
    //
    // Owner 2026-08-07: "GRID NEED TO RENDER UNDER GEO". Egui has no depth buffer, so the
    // old wire grid always painted on top of solid meshes. Live path is now
    // `overlay_lines::append_floor_grid` → GPU line pass, depth-tested against the G-buffer.
    // Shadow Catcher (grid_mode==1) still uses the deferred/raymarch GPU floor.
    // Call sites keep calling this; it is intentionally a no-op.
    pub(crate) fn draw_grid_overlay(&self, _painter: &egui::Painter, _rect: egui::Rect) {}

    // ---- Horizon line — vanishing line of the floor plane ----
    #[allow(dead_code)]
    pub(crate) fn draw_horizon_line(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        if cam_fwd.y.abs() > 0.95 { return; } // skip when looking straight down/up
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        // Project a point very far away on the floor plane along the camera's XZ
        // forward direction — this converges to the vanishing line of the floor.
        let cam_fwd_xz = Vec3::new(cam_fwd.x, 0.0, cam_fwd.z);
        if cam_fwd_xz.length_squared() < 0.001 { return; }
        let fwd_xz = cam_fwd_xz.normalize();
        let far = 100000.0;
        let far_pt = Vec3::new(
            cam_pos.x + fwd_xz.x * far,
            self.viewport.grid_floor_y,
            cam_pos.z + fwd_xz.z * far,
        );
        if let Some(sp) = world_to_screen(far_pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            let horizon_y = sp.y.clamp(rect.min.y, rect.max.y);
            let color = egui::Color32::from_rgba_unmultiplied(180, 180, 190, 22);
            painter.line_segment(
                [egui::pos2(rect.min.x, horizon_y), egui::pos2(rect.max.x, horizon_y)],
                egui::Stroke::new(0.5, color),
            );
        }
    }

    // ---- Depth pick overlay ----
    pub(crate) fn draw_depth_pick_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        let dp = match &self.tool.depth_pick { Some(d) => d, None => return };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let magenta = egui::Color32::from_rgb(220, 60, 220);
        let magenta_dim = egui::Color32::from_rgba_unmultiplied(180, 60, 180, 100);
        let total = dp.candidates.len();

        for (ci, &uid) in dp.candidates.iter().enumerate() {
            let pidx = match self.scene.prim_uid_to_index.get(&uid) { Some(&i) => i, None => continue };
            if pidx >= self.scene.primitives.len() { continue; }
            let p = &self.scene.primitives[pidx];
            let Some(sp) = world_to_screen(p.position, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
            if ci == dp.index {
                // Active pick: magenta double-ring
                painter.circle_stroke(sp, 20.0, egui::Stroke::new(2.0, magenta));
                painter.circle_stroke(sp, 24.0, egui::Stroke::new(1.0, magenta));
                // Label: "ShapeName [+/-] (2/5)"
                let op_tag = if p.operation.is_additive() { "+" } else { "-" };
                let name = if p.name.is_empty() { p.shape.label().to_string() } else { p.name.clone() };
                let label = format!("{} [{}] ({}/{})", name, op_tag, ci + 1, total);
                let label_pos = egui::pos2(sp.x, sp.y - 32.0);
                let font = egui::FontId::proportional(13.0);
                let galley = painter.layout_no_wrap(label.clone(), font.clone(), magenta);
                let text_rect = egui::Rect::from_min_size(
                    egui::pos2(label_pos.x - galley.size().x * 0.5 - 4.0, label_pos.y - galley.size().y * 0.5 - 2.0),
                    egui::vec2(galley.size().x + 8.0, galley.size().y + 4.0),
                );
                painter.rect_filled(text_rect, 3.0, egui::Color32::from_rgba_unmultiplied(20, 10, 20, 200));
                painter.text(label_pos, egui::Align2::CENTER_CENTER, label, font, magenta);
            } else {
                // Other candidates: dim dot
                painter.circle_filled(sp, 4.0, magenta_dim);
            }
        }
    }

    // ---- Billboard camera lock icon overlay ----
    pub(crate) fn draw_billboard_camera_icon(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: egui::Rect) {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let (clicked, mouse_pos) = ctx.input(|i| (
            i.pointer.any_click(),
            i.pointer.interact_pos().unwrap_or(egui::pos2(-100.0, -100.0)),
        ));
        for i in self.selected_prim_indices_unified() {
            if i >= self.scene.primitives.len() { continue; }
            if let SdfShape::Billboard { face_camera, .. } = &self.scene.primitives[i].shape {
                let fc = *face_camera;
                let half = crate::shape_half_extent(&self.scene.primitives[i]);
                let corner_local = Vec3::new(half.x, half.y, 0.0);
                let q = self.scene.primitives[i].rotation_quat();
                let corner_world = self.scene.primitives[i].position + q * (corner_local * self.scene.primitives[i].scale);
                if let Some(sp) = world_to_screen(corner_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    let icon_pos = egui::pos2(sp.x + 8.0, sp.y - 8.0);
                    let label = if fc { "Lock" } else { "Free" };
                    let bg_col = if fc {
                        egui::Color32::from_rgba_unmultiplied(0, 120, 255, 200)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(80, 80, 80, 180)
                    };
                    let icon_rect = egui::Rect::from_center_size(icon_pos, egui::vec2(36.0, 18.0));
                    painter.rect_filled(icon_rect, 4.0, bg_col);
                    painter.text(icon_pos, egui::Align2::CENTER_CENTER,
                        label, egui::FontId::proportional(10.0),
                        egui::Color32::WHITE);
                    if clicked && icon_rect.contains(mouse_pos) {
                        self.transaction("Toggle billboard face-camera", |s| {
                            s.write_prim(i, |p| {
                                if let SdfShape::Billboard { ref mut face_camera, .. } = p.shape {
                                    *face_camera = !*face_camera;
                                }
                            });
                            s.mark_dirty(DirtyFlags::RENDER);
                        });
                    }
                }
            }
        }
    }

    /// Draw blend-k handle: small diamond near selected prim or body.
    /// Unified: detects whether selection is a prim or body and adapts.
    /// Same handle, same drag, same click-to-cycle for both targets.
    /// The handle's MEANING branches on the target state (`prim_handle_mode`):
    /// blend k, signed seam depth (groove/bead), or signed relief strength
    /// (emboss/deboss) — one handle, one drag tool, three parameters.
    pub(crate) fn draw_blend_k_handle(&mut self, painter: &egui::Painter, rect: egui::Rect, mouse: egui::Pos2) {
        use core_math::primitives::BooleanOp;
        use core_math::scene_graph::NodePayload;

        // Bevel mode owns the off-silhouette handle slot: Uniform bevel (yellow square) uses the
        // same `overlay_handle_off_prim` anchor as this diamond. While B is armed (or mid edge
        // drag), hide the blend-k/seam/extrude diamond so the two never stack / steal clicks.
        // Matches set_mode(Bevel): "B owns the viewport".
        if self.tool.mode.is_bevel() || self.tool.current.is_bevel() {
            self.tool.blend_k_hovered = false;
            return;
        }

        // Resolve target: prim selection takes priority, then body selection.
        // Multi-select: one diamond on the anchor prim, drag applies to all matching prims.
        // `label` is the full hover text (name + value), preformatted per mode.
        let (handle_world, label, color_rgb) = 'resolve: {
            // Try prims first (single or multi)
            let indices = self.selected_prim_indices_unified();
            let valid: Vec<usize> = indices.into_iter()
                .filter(|&i| crate::edge_detect::prim_accepts_blend_handle(&self.scene, i))
                .collect();
            if !valid.is_empty() {
                // Anchor = active if selected+valid, else first valid.
                let idx = self.active_prim_index_unified()
                    .filter(|a| valid.contains(a))
                    .unwrap_or(valid[0]);
                let prim = &self.scene.primitives[idx];
                let half = crate::shape_half_extent(prim);
                let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
                // Own body transform (not active_body — nested / multi-body selection).
                let bt = self
                    .scene
                    .prim_body_map
                    .get(idx)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default();
                // Same off-silhouette rule as Uniform bevel / other overlay handles
                // (center + cam-right past OBB). Fixes Polyline/CurveExtrude diamond on-mesh.
                let handle = Self::overlay_handle_off_prim(
                    prim.position,
                    prim.rotation_quat(),
                    prim.scale,
                    half,
                    &bt,
                    cam_right,
                );
                use crate::edge_detect::BlendHandleMode;
                let multi_n = valid.len();
                let multi_sfx = if multi_n > 1 { format!(" ×{}", multi_n) } else { String::new() };
                let (label, rgb) = match crate::edge_detect::prim_handle_mode(prim) {
                    BlendHandleMode::SeamDepth => {
                        let s = crate::edge_detect::seam_signed_depth(&prim.operation);
                        let w = prim.operation.blend_k();
                        let prof = if let BooleanOp::Seam { profile, .. } = &prim.operation {
                            crate::edge_detect::seam_profile_label(*profile)
                        } else { "" };
                        if s < 0.0 { (format!("Seam {} Bead d:{:.3} w:{:.3}{}", prof, -s, w, multi_sfx), (230u8, 120u8, 180u8)) }
                        else       { (format!("Seam {} Groove d:{:.3} w:{:.3}{}", prof, s, w, multi_sfx), (150, 100, 240)) }
                    }
                    BlendHandleMode::ExtrudeStrength => {
                        let s = crate::edge_detect::extrude_signed_strength(&prim.operation);
                        let w = prim.operation.blend_k();
                        if s < 0.0 { (format!("Extrude In s:{:.3} w:{:.3}{}", -s, w, multi_sfx), (230u8, 150u8, 60u8)) }
                        else       { (format!("Extrude Out s:{:.3} w:{:.3}{}", s, w, multi_sfx), (110, 220, 130)) }
                    }
                    BlendHandleMode::BlendK => {
                        let k = crate::edge_detect::op_get_k(&prim.operation);
                        use core_math::primitives::BlendProfile;
                        match prim.blend_profile {
                            BlendProfile::Chamfer => {
                                (format!("Chamfer k:{:.3}{}", k, multi_sfx), (230, 160, 40))
                            }
                            // Smooth + legacy Circular/Curve
                            _ => (format!("Smooth k:{:.3}{}", k, multi_sfx), (60u8, 150u8, 230u8)),
                        }
                    }
                };
                break 'resolve (handle, label, rgb);
            }

            // Try body (Object mode only, non-first body)
            if self.tool.work_mode == WorkMode::Object {
                let sel_nid = self.sel.usel.selected.iter().find_map(|item| {
                    if let SceneItemId::Node(nid) = item { Some(*nid) } else { None }
                });
                if let Some(body_nid) = sel_nid {
                    if let Some(node) = self.scene.scene_graph.get(body_nid) {
                        if let NodePayload::SdfBody { body_op, origin, .. } = &node.payload {
                            if !matches!(body_op, BooleanOp::Intersect) {
                                let root_bodies: Vec<_> = self.scene.scene_graph.sdf_bodies().map(|(nid, _)| nid).collect();
                                if root_bodies.first() != Some(&body_nid) {
                                    let bt = self
                                        .scene
                                        .scene_graph
                                        .body_world_transform(body_nid)
                                        .unwrap_or_default();
                                    // Body AABB half-extent from child prims (fallback 0.5).
                                    let half = self
                                        .scene
                                        .prim_body_map
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, &(nid, _))| nid == body_nid)
                                        .map(|(i, _)| {
                                            let p = &self.scene.primitives[i];
                                            crate::shape_half_extent(p) * p.scale.abs()
                                        })
                                        .fold(Vec3::splat(0.5), |a, b| a.max(b));
                                    let cam_right =
                                        (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
                                    let handle = Self::overlay_handle_off_prim(
                                        *origin,
                                        Quat::IDENTITY,
                                        Vec3::ONE,
                                        half,
                                        &bt,
                                        cam_right,
                                    );
                                    // Seam bodies: the handle drags signed DEPTH vertically +
                                    // width horizontally (like prims); Extrude bodies drag
                                    // signed STRENGTH + width; everything else drags blend k.
                                    let (label, rgb) = match body_op {
                                        BooleanOp::Seam { raised: false, profile, .. } =>
                                            (format!("Seam {} Groove d:{:.3} w:{:.3}", crate::edge_detect::seam_profile_label(*profile), crate::edge_detect::seam_signed_depth(body_op), body_op.blend_k()), (150u8, 100u8, 240u8)),
                                        BooleanOp::Seam { raised: true, profile, .. } =>
                                            (format!("Seam {} Bead d:{:.3} w:{:.3}", crate::edge_detect::seam_profile_label(*profile), -crate::edge_detect::seam_signed_depth(body_op), body_op.blend_k()), (230, 120, 180)),
                                        BooleanOp::Extrude { strength, width } => {
                                            if *strength < 0.0 { (format!("Extrude In s:{:.3} w:{:.3}", -strength, width), (230, 150, 60)) }
                                            else               { (format!("Extrude Out s:{:.3} w:{:.3}", strength, width), (110, 220, 130)) }
                                        }
                                        op => {
                                            let name = match op {
                                                BooleanOp::Union { .. } => "Union",
                                                BooleanOp::Subtract { .. } => "Subtract",
                                                BooleanOp::ChamferUnion { .. } => "Chamfer Union",
                                                BooleanOp::ChamferSubtract { .. } => "Chamfer Sub",
                                                BooleanOp::Intersect => "Intersect",
                                                BooleanOp::SmoothIntersect { .. } => "Smooth Intersect",
                                                BooleanOp::Seam { .. } | BooleanOp::Extrude { .. } => unreachable!(),
                                            };
                                            (format!("{} k:{:.3}", name, op.blend_k()), (60, 150, 230))
                                        }
                                    };
                                    break 'resolve (handle, label, rgb);
                                }
                            }
                        }
                    }
                }
            }

            // No valid target
            self.tool.blend_k_hovered = false;
            return;
        };

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        let sp = match world_to_screen(handle_world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
            Some(p) => p,
            None => { self.tool.blend_k_hovered = false; return; }
        };

        // Hover detection
        let dist = (mouse - sp).length();
        let hovered = dist < 16.0;
        let dragging = self.tool.current.is_blend_k_drag();
        self.tool.blend_k_hovered = hovered;

        // Diamond drawing + label — shared primitive (also used by the curve-bevel handle).
        self.draw_diamond_handle(painter, sp, color_rgb, &label, hovered, dragging);
    }

    /// Shared diamond-handle primitive: draws a hover/drag-styled diamond at `sp` plus a floating
    /// monospace label above it when hovered/dragging. Reused by BOTH the blend-k / seam / extrude
    /// handle (`draw_blend_k_handle`) and the curve-bevel size handle — the diamond visuals live in
    /// ONE place so the two never fork. Screen-radius hover convention (<16px) is the caller's.
    pub(crate) fn draw_diamond_handle(&self, painter: &egui::Painter, sp: egui::Pos2,
                                      color_rgb: (u8, u8, u8), label: &str, hovered: bool, dragging: bool) {
        let (pr, pg, pb) = color_rgb;
        let base_col = if dragging {
            egui::Color32::from_rgb(pr.min(200) + 40, pg.min(200) + 40, pb.min(200) + 40)
        } else if hovered {
            egui::Color32::from_rgb(pr.min(180) + 60, pg.min(180) + 60, pb.min(180) + 60)
        } else {
            egui::Color32::from_rgba_unmultiplied(pr, pg, pb, 200)
        };
        let s = if hovered || dragging { 10.0 } else { 8.0 };
        let diamond = vec![
            sp + egui::vec2(0.0, -s), sp + egui::vec2(s, 0.0),
            sp + egui::vec2(0.0, s),  sp + egui::vec2(-s, 0.0),
        ];
        painter.add(egui::Shape::convex_polygon(diamond.clone(), base_col, egui::Stroke::NONE));
        painter.add(egui::Shape::closed_line(diamond, egui::Stroke::new(1.5, egui::Color32::from_gray(20))));
        if hovered || dragging {
            let label_pos = egui::pos2(sp.x, sp.y - 18.0);
            let font = egui::FontId::monospace(11.0);
            let galley = painter.layout_no_wrap(label.to_string(), font.clone(), base_col);
            let bg_rect = egui::Rect::from_min_size(
                egui::pos2(label_pos.x - galley.size().x * 0.5 - 3.0, label_pos.y - galley.size().y * 0.5 - 1.0),
                egui::vec2(galley.size().x + 6.0, galley.size().y + 2.0),
            );
            painter.rect_filled(bg_rect, 3.0, egui::Color32::from_rgba_unmultiplied(10, 20, 40, 210));
            painter.text(label_pos, egui::Align2::CENTER_CENTER, label.to_string(), font, base_col);
        }
    }

    /// Draw the blend-k popup (opened by right-clicking the diamond).
    /// Works for both prim blend profiles and body boolean types.
    pub(crate) fn draw_blend_k_popup(&mut self, ctx: &egui::Context) {
        use core_math::primitives::BooleanOp;

        let target = match self.tool.blend_k_popup.clone() {
            Some(t) => t,
            None => return,
        };

        let mut open = true;
        let mut changed = false;

        match target {
            BlendKTarget::Body(body_nid) => {
                egui::Window::new("Body Boolean")
                    .collapsible(false).resizable(false).auto_sized().open(&mut open)
                    .show(ctx, |ui| {
                        let current_op = self.scene.scene_graph.body_op(body_nid).cloned();
                        let k = current_op.as_ref().map_or(0.0, |op| op.blend_k());
                        let ops: &[(&str, BooleanOp)] = &[
                            ("Union", BooleanOp::Union { k }),
                            ("Subtract", BooleanOp::Subtract { k }),
                            ("Intersect", BooleanOp::Intersect),
                        ];
                        for (label, op) in ops {
                            let is_current = current_op.as_ref().map_or(false, |c|
                                std::mem::discriminant(c) == std::mem::discriminant(op));
                            if ui.selectable_label(is_current, *label).clicked() {
                                // Re-clicking the active op must not push an empty undo.
                                if !is_current {
                                    self.transaction("Set body boolean", |s| {
                                        s.scene.scene_graph.set_body_op(body_nid, op.clone());
                                        s.mark_dirty(DirtyFlags::SCENE);
                                    });
                                    if let Some(ref mut gpu) = self.gpu {
                                        if let Some(ref mut atlas) = gpu.volume_atlas {
                                            atlas.rebuild_params(&self.scene.scene_graph);
                                        }
                                    }
                                    self.toast(&format!("Body: {}", label));
                                }
                                changed = true;
                            }
                        }
                    });
            }
            BlendKTarget::Prims(indices) => {
                egui::Window::new(if indices.len() > 1 { "Blend Profile (multi)" } else { "Blend Profile" })
                    .collapsible(false).resizable(false).auto_sized().open(&mut open)
                    .show(ctx, |ui| {
                        let anchor = indices.first().copied().unwrap_or(0);
                        if anchor < self.scene.primitives.len() {
                            let current = self.scene.primitives[anchor].blend_profile;
                            for (prof, label) in crate::properties_sdf::BLEND_PROFILE_CHOICES {
                                if ui.selectable_label(current == prof, label).clicked() {
                                    // Re-clicking the active profile must not push an empty undo.
                                    if current != prof {
                                        let p = prof;
                                        let idxs = indices.clone();
                                        self.transaction("Set blend profile", |s| {
                                            for &idx in &idxs {
                                                if idx < s.scene.primitives.len() {
                                                    s.write_prim(idx, |pr| pr.blend_profile = p);
                                                }
                                            }
                                            s.rebake_no_undo();
                                        });
                                        let n = indices.len();
                                        if n > 1 {
                                            self.toast(&format!("Blend: {} ({} prims)", label, n));
                                        } else {
                                            self.toast(&format!("Blend: {}", label));
                                        }
                                    }
                                    changed = true;
                                }
                            }
                        }
                    });
            }
        }
        if !open || changed { self.tool.blend_k_popup = None; }
    }
}
