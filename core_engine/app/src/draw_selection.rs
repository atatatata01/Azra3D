// draw_selection.rs — Selection highlights, node outlines, SDF brush preview,
//   raycast select, finish selection drag
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;

/// Ray-AABB intersection test. Returns Some(t) at entry point, None if miss.
fn ray_aabb_intersect(origin: Vec3, dir: Vec3, aabb_min: Vec3, aabb_max: Vec3) -> Option<f32> {
    let inv_dir = Vec3::new(
        if dir.x.abs() > 1e-8 { 1.0 / dir.x } else { 1e20_f32.copysign(dir.x) },
        if dir.y.abs() > 1e-8 { 1.0 / dir.y } else { 1e20_f32.copysign(dir.y) },
        if dir.z.abs() > 1e-8 { 1.0 / dir.z } else { 1e20_f32.copysign(dir.z) },
    );
    let t0 = (aabb_min - origin) * inv_dir;
    let t1 = (aabb_max - origin) * inv_dir;
    let tmin = t0.min(t1);
    let tmax = t0.max(t1);
    let t_enter = tmin.x.max(tmin.y).max(tmin.z);
    let t_exit = tmax.x.min(tmax.y).min(tmax.z);
    if t_enter <= t_exit && t_exit >= 0.0 {
        Some(t_enter.max(0.0))
    } else {
        None
    }
}
use core_math::primitives::SdfShape;
use core_math::scene_graph::NodePayload;
use crate::*;

use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    // ---- SDF sculpt brush preview (Object mode, modifier-based) ----
    pub(crate) fn draw_sdf_brush_preview(&self, painter: &egui::Painter, rect: egui::Rect, mouse: egui::Pos2) {
        if (self.tool.work_mode != WorkMode::Object && self.tool.work_mode != WorkMode::Sculpt) || !self.sdf_sculpt_active || !rect.contains(mouse) { return; }
        // Project brush radius to screen space (approximate via cam distance)
        let sr = self.sdf_sculpt_radius * rect.height() / (self.viewport.camera.distance.max(0.1) * 1.5);
        let sr = sr.clamp(4.0, rect.width().min(rect.height()) * 0.4); // clamp to sane range
        // Amber = add, blue = subtract (Ctrl), yellow = flatten (Shift)
        let ring_color = match self.sdf_sculpt_op {
            core_math::primitives::SculptOp::Push => egui::Color32::from_rgba_unmultiplied(255, 158, 61, 180),
            core_math::primitives::SculptOp::Pull => egui::Color32::from_rgba_unmultiplied(100, 150, 255, 180),
            core_math::primitives::SculptOp::Flatten => egui::Color32::from_rgba_unmultiplied(220, 200, 80, 160),
            core_math::primitives::SculptOp::Inflate => egui::Color32::from_rgba_unmultiplied(200, 120, 220, 160),
            core_math::primitives::SculptOp::Pinch => egui::Color32::from_rgba_unmultiplied(220, 160, 60, 160),
            core_math::primitives::SculptOp::Crease => egui::Color32::from_rgba_unmultiplied(160, 80, 200, 160),
        };
        // Outer ring
        painter.circle_stroke(mouse, sr, egui::Stroke::new(2.0, ring_color));
        // Inner ring (strength indicator)
        let inner_r = sr * self.sdf_sculpt_strength.min(1.0);
        let inner_color = egui::Color32::from_rgba_unmultiplied(
            ring_color.r(), ring_color.g(), ring_color.b(), 80);
        painter.circle_stroke(mouse, inner_r, egui::Stroke::new(1.0, inner_color));
        // Center dot
        painter.circle_filled(mouse, 2.5, ring_color);
        // Crosshair when hitting surface
        if self.sdf_brush_preview_pos.is_some() {
            let cross = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100);
            painter.line_segment([egui::pos2(mouse.x - 5.0, mouse.y), egui::pos2(mouse.x + 5.0, mouse.y)], egui::Stroke::new(1.0, cross));
            painter.line_segment([egui::pos2(mouse.x, mouse.y - 5.0), egui::pos2(mouse.x, mouse.y + 5.0)], egui::Stroke::new(1.0, cross));
        }
    }

    // ---- Raycast select ----
    pub(crate) fn raycast_select(&self, mouse:egui::Pos2, rect:egui::Rect, include_cutters:bool) -> Option<usize> {
        // ONE pick ray (cam_ray) — has the orthographic branch this inline copy was missing, so SDF /
        // object-fallback picks no longer drift in ortho view (audit #3; also collapses a duplicate ray).
        let (cam_pos, rd) = self.cam_ray(mouse, rect);
        let mut best:Option<(usize,f32)>=None;
        for (i,p) in self.scene.primitives.iter().enumerate() {
            if !p.visible{continue;}
            if p.locked{continue;} // locked items are not selectable
            // Smart selection filter: skip cutters (non-additive ops) unless include_cutters.
            // The CHAIN BASE (first slot of the body's resolved child_order — its op is ignored
            // by the eval, it always contributes surface) stays selectable regardless. Was
            // `i > 0` (flat index), which diverged from the field after a reorder.
            if !include_cutters && !self.scene.is_base_prim_flat(i) && !p.operation.is_additive() { continue; }
            // Derive hit radius from the actual shape bounding sphere (matches BVH bound_r in lib.rs).
            // Minimum 0.08 so sub-voxel objects remain clickable.
            let hit_r: f32 = (match &p.shape {
                SdfShape::Sphere{radius}            => *radius * p.scale.x.max(p.scale.y).max(p.scale.z),
                SdfShape::Box{half_extents:h,..} => (p.scale * *h).length(),
                SdfShape::Cylinder{radius,half_height} => {
                    let r=p.scale.x.max(p.scale.z)**radius; let hh=p.scale.y**half_height;
                    (r*r+hh*hh).sqrt()
                },
                SdfShape::Torus{major_radius,minor_radius} => p.scale.x.max(p.scale.z)*(*major_radius+*minor_radius),
                SdfShape::Cone{radius,half_height} => {
                    let r=p.scale.x.max(p.scale.z)**radius; let hh=p.scale.y**half_height;
                    (r*r+hh*hh).sqrt()
                },
                SdfShape::Capsule{radius,half_height} => p.scale.y**half_height + p.scale.x.max(p.scale.z)**radius,
                SdfShape::Stair{half_extents,..} => (*half_extents * p.scale).length(),
                SdfShape::SpiralStair{radius,half_height,..} => p.scale.max_element() * (radius + half_height),
                SdfShape::Billboard{width,height,..} => {
                    let w = p.scale.x * *width * 0.5; let h = p.scale.y * *height * 0.5;
                    (w*w+h*h).sqrt()
                },
                SdfShape::CurveExtrude { half_depth, .. } => p.scale.max_element() * *half_depth,
                SdfShape::MeshVolume { grid, .. } => (p.scale * grid.corner_abs_max()).length(),
                SdfShape::HalfSpace => 0.5, // infinite plane: nominal viewport click radius
                SdfShape::SpinePipe { points, radius, point_scale, .. } => {
                    let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
                    let max_s = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
                    p.scale.max_element() * (max_pt + *radius * max_s)
                },
            }).max(0.08);
            let world_pos = if BODY_LOCAL_PRIMS {
                self.scene.prim_body_map.get(i)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default()
                    .local_to_world(p.position)
            } else { p.position };
            // Ray-AABB intersection: tight per-axis bounding box, not bloated sphere.
            // Uses the prim's axis-aligned half-extents in world space.
            let he = p.bounding_aabb_half_extents() * p.scale;
            let aabb_min = world_pos - he;
            let aabb_max = world_pos + he;
            let t_hit = ray_aabb_intersect(cam_pos, rd, aabb_min, aabb_max);
            if let Some(t) = t_hit {
                if t > 0.0 {
                    if best.is_none() || t < best.unwrap().1 { best = Some((i, t)); }
                }
            }
        }
        best.map(|(i,_)|i)
    }

    /// Collect ALL sphere-cast hits sorted by distance (nearest first).
    /// Used by depth scroll pick — no additive filter, returns everything visible.
    pub(crate) fn raycast_select_all(&self, mouse:egui::Pos2, rect:egui::Rect) -> Vec<(usize,f32)> {
        // Same single ortho-correct pick ray as raycast_select (audit #3).
        let (cam_pos, rd) = self.cam_ray(mouse, rect);
        let mut hits: Vec<(usize,f32)> = Vec::new();
        for (i,p) in self.scene.primitives.iter().enumerate() {
            if !p.visible{continue;}
            if p.locked{continue;} // locked items are not selectable
            let hit_r: f32 = (match &p.shape {
                SdfShape::Sphere{radius}            => *radius * p.scale.x.max(p.scale.y).max(p.scale.z),
                SdfShape::Box{half_extents:h,..} => (p.scale * *h).length(),
                SdfShape::Cylinder{radius,half_height} => {
                    let r=p.scale.x.max(p.scale.z)**radius; let hh=p.scale.y**half_height;
                    (r*r+hh*hh).sqrt()
                },
                SdfShape::Torus{major_radius,minor_radius} => p.scale.x.max(p.scale.z)*(*major_radius+*minor_radius),
                SdfShape::Cone{radius,half_height} => {
                    let r=p.scale.x.max(p.scale.z)**radius; let hh=p.scale.y**half_height;
                    (r*r+hh*hh).sqrt()
                },
                SdfShape::Capsule{radius,half_height} => p.scale.y**half_height + p.scale.x.max(p.scale.z)**radius,
                SdfShape::Stair{half_extents,..} => (*half_extents * p.scale).length(),
                SdfShape::SpiralStair{radius,half_height,..} => p.scale.max_element() * (radius + half_height),
                SdfShape::Billboard{width,height,..} => {
                    let w = p.scale.x * *width * 0.5; let h = p.scale.y * *height * 0.5;
                    (w*w+h*h).sqrt()
                },
                SdfShape::CurveExtrude { half_depth, .. } => p.scale.max_element() * *half_depth,
                SdfShape::MeshVolume { grid, .. } => (p.scale * grid.corner_abs_max()).length(),
                SdfShape::HalfSpace => 0.5, // infinite plane: nominal viewport click radius
                SdfShape::SpinePipe { points, radius, point_scale, .. } => {
                    let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
                    let max_s = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
                    p.scale.max_element() * (max_pt + *radius * max_s)
                },
            }).max(0.08);
            let world_pos = if BODY_LOCAL_PRIMS {
                self.scene.prim_body_map.get(i)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default()
                    .local_to_world(p.position)
            } else { p.position };
            // Ray-AABB intersection (same as raycast_select — tight per-axis bounds)
            let he = p.bounding_aabb_half_extents() * p.scale;
            let aabb_min = world_pos - he;
            let aabb_max = world_pos + he;
            if let Some(t) = ray_aabb_intersect(cam_pos, rd, aabb_min, aabb_max) {
                if t > 0.0 { hits.push((i, t)); }
            }
        }
        hits.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        // SDF REFINEMENT (fix 2026-07-28: "I click here in primitive mode and it won't select the
        // proper primitive").
        //
        // Everything above is a ray-AABB test, so the nearest BOUNDING BOX wins — not the nearest
        // actual surface. A tall thin prim standing next to a wide one has an AABB that hangs over
        // its neighbour, so clicking the neighbour's visible surface picks the tall prim. The
        // deeper the two AABBs overlap, the more often the visibly-wrong prim wins.
        //
        // Refine by sphere-tracing each candidate's REAL field and re-sorting on the true surface
        // hit. `world_distance` is the same evaluator the bake and CPU paths use, so the pick now
        // agrees with what is actually drawn (CLAUDE.md: tool math must match the real
        // representation — a bounding proxy where the true SDF exists is the forbidden case).
        //
        // FAIL-SOFT: a candidate whose march does not converge keeps its AABB `t` and its original
        // order. Refinement can only ever REORDER what the AABB pass already found — it never adds
        // or drops a candidate — so a march that behaves badly degrades to today's behaviour
        // rather than losing the click entirely.
        if hits.len() > 1 {
            for (i, t) in hits.iter_mut() {
                if let Some(refined) = self.sdf_surface_t(*i, cam_pos, rd, *t) {
                    *t = refined;
                }
            }
            hits.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        }
        hits
    }

    /// Sphere-trace prim `idx`'s actual SDF along the pick ray, starting at the AABB entry `t0`.
    /// Returns the parametric distance to the real surface, or `None` if the march does not land
    /// (caller keeps the AABB estimate — see the fail-soft note at the call site).
    ///
    /// Points are converted to the prim's PARENT frame before evaluation: `world_distance` is a
    /// documented misnomer — for a body's prim array it consumes BODY-LOCAL points
    /// (CLAUDE.md "Prim Position Space"). Skipping that conversion would evaluate the field at the
    /// wrong place and silently pick the wrong prim, which is the bug this function exists to fix.
    fn sdf_surface_t(&self, idx: usize, ro: glam::Vec3, rd: glam::Vec3, t0: f32) -> Option<f32> {
        let p = self.scene.primitives.get(idx)?;
        let xf = if BODY_LOCAL_PRIMS {
            self.scene.prim_body_map.get(idx)
                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
        } else {
            None
        };
        let to_local = |w: glam::Vec3| -> glam::Vec3 {
            match &xf {
                Some(t) => t.world_to_local(w),
                None => w,
            }
        };
        // BODY SCALE: `world_to_local` divides by the body scale, so `world_distance` returns a
        // distance in LOCAL units. Stepping the WORLD ray by that value would overshoot on a
        // shrunk body (local units are larger than world units) and could tunnel straight through
        // thin geometry. Convert back with the smallest axis scale — the conservative choice,
        // since a sphere trace must never step further than the true nearest surface.
        let scale_to_world = match &xf {
            Some(t) => t.scale.x.abs().min(t.scale.y.abs()).min(t.scale.z.abs()).max(1e-6),
            None => 1.0,
        };
        // Bounded march: 48 steps is ample for a click ray already inside the AABB, and the cap
        // guarantees this cannot stall the frame on a pathological field (Frame Budget Rule).
        const MAX_STEPS: u32 = 48;
        const HIT_EPS: f32 = 1e-3;
        let mut t = t0.max(0.0);
        let far = t0 + p.bounding_aabb_half_extents().max_element() * p.scale.max_element() * 4.0 + 1.0;
        for _ in 0..MAX_STEPS {
            let d = p.world_distance(to_local(ro + rd * t)) * scale_to_world;
            if d < HIT_EPS {
                return Some(t);
            }
            t += d.max(HIT_EPS);
            if t > far {
                return None;
            }
        }
        None
    }

    // ---- Finish selection drag ----
    /// `add` = keep existing + insert hits (LMB Shift/Ctrl marquee).
    /// `subtract` = remove hits only (Shift+RMB marquee deselect — owner 2026-08-07).
    /// Neither = replace (clear then insert). `subtract` wins over `add` if both set.
    pub(crate) fn finish_selection_drag(&mut self, cur:egui::Pos2, rect:egui::Rect, add:bool, include_cutters:bool, subtract:bool) {
        let drag=match self.tool.current.take_selection_box(){Some(d)=>d,None=>return};
        let cam_pos=self.viewport.camera.target+self.viewport.camera.orbit*Vec3::new(0.0,0.0,self.viewport.camera.distance);
        let aspect=rect.width()/rect.height();
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        // Object-mode node marquee: SDF bodies AND poly / imported / NURBS meshes.
        // (Was SDF-only — poly multi-marquee never selected; owner multi-select highlight fix.)
        let object_mode_nodes = self.tool.work_mode == WorkMode::Object
            || (self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode == PolyWorkMode::Object)
            || (self.modeling_context == ModelingContext::Nurbs
                && self.nurbs_work_mode == NurbsWorkMode::Object);
        if object_mode_nodes {
            if !add && !subtract {
                self.sel.usel.selected.retain(|id| !matches!(id, SceneItemId::Node(_)));
            }
            let mut hit_bodies = std::collections::HashSet::new();
            let mut first_body = None;
            let box_or_lasso = |sp: egui::Pos2| -> bool {
                match &drag {
                    SelectionDrag::Box { start } => {
                        egui::Rect::from_two_pos(*start, cur).contains(sp)
                    }
                    SelectionDrag::Lasso { center } => {
                        (sp - *center).length() <= (cur - *center).length()
                    }
                }
            };
            // SDF: body from prim screen positions
            if self.modeling_context == ModelingContext::Sdf
                || self.tool.work_mode == WorkMode::Object
            {
                for (i, p) in self.scene.primitives.iter().enumerate() {
                    if !p.visible {
                        continue;
                    }
                    let world_pos = if BODY_LOCAL_PRIMS {
                        self.scene
                            .prim_body_map
                            .get(i)
                            .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                            .unwrap_or_default()
                            .local_to_world(p.position)
                    } else {
                        p.position
                    };
                    let Some(sp) = world_to_screen(
                        world_pos,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    ) else {
                        continue;
                    };
                    if box_or_lasso(sp) && i < self.scene.prim_body_map.len() {
                        let body_nid = self.scene.prim_body_map[i].0;
                        if hit_bodies.insert(body_nid) {
                            if subtract {
                                self.sel.usel.selected.remove(&SceneItemId::Node(body_nid));
                            } else {
                                self.sel.usel.selected.insert(SceneItemId::Node(body_nid));
                                if first_body.is_none() {
                                    first_body = Some(body_nid);
                                }
                            }
                        }
                    }
                }
            }
            // Poly / imported / NURBS / empty: project node origin (AABB center when available)
            for (nid, node) in self.scene.scene_graph.all_nodes() {
                if !node.visible {
                    continue;
                }
                use core_math::scene_graph::NodePayload;
                let is_mesh = matches!(
                    &node.payload,
                    NodePayload::PolygonalBody { .. }
                        | NodePayload::ImportedMesh { .. }
                        | NodePayload::NurbsBody { .. }
                        | NodePayload::NurbsSolid { .. }
                        | NodePayload::VoxelBody { .. }
                        | NodePayload::SdfBody { .. }
                );
                if !is_mesh {
                    continue;
                }
                if hit_bodies.contains(&nid) {
                    continue; // already from SDF prim path
                }
                let world = self.scene.scene_graph.world_transform(nid);
                let world_pos = world.transform_point3(glam::Vec3::ZERO);
                let Some(sp) = world_to_screen(
                    world_pos,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ) else {
                    continue;
                };
                if box_or_lasso(sp) {
                    if hit_bodies.insert(nid) {
                        if subtract {
                            self.sel.usel.selected.remove(&SceneItemId::Node(nid));
                        } else {
                            self.sel.usel.selected.insert(SceneItemId::Node(nid));
                            if first_body.is_none() {
                                first_body = Some(nid);
                            }
                        }
                    }
                }
            }
            if subtract {
                if let Some(SceneItemId::Node(n)) = self.sel.usel.active {
                    if !self.sel.usel.selected.contains(&SceneItemId::Node(n)) {
                        self.sel.usel.active = self.sel.usel.selected.iter().find_map(|id| {
                            if let SceneItemId::Node(nid) = id {
                                Some(SceneItemId::Node(*nid))
                            } else {
                                None
                            }
                        });
                        if let Some(SceneItemId::Node(nid)) = self.sel.usel.active {
                            self.scene.active_body_nid = nid;
                        }
                    }
                }
            } else if let Some(nid) = first_body {
                self.sel.usel.active = Some(SceneItemId::Node(nid));
                self.scene.active_body_nid = nid;
            } else if !add {
                self.sel.usel.active = self.sel.usel.selected.last().copied();
            }
            self.mark_selection_dirty();
            return;
        }

        // Prim-level selection (edit mode or non-SDF)
        // INTENTIONAL: marquee selection with add/replace/subtract — doesn't fit canonical writers
        if !add && !subtract { self.sel.usel.selected.retain(|id| !matches!(id, SceneItemId::Prim(_))); }
        let mut first_uid=None;
        for (i,p) in self.scene.primitives.iter().enumerate() {
            if !p.visible{continue;}
            if p.locked{continue;} // locked items are not selectable
            if !include_cutters && i > 0 && !p.operation.is_additive() { continue; }
            let world_pos = if BODY_LOCAL_PRIMS {
                self.scene.prim_body_map.get(i)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default()
                    .local_to_world(p.position)
            } else { p.position };
            let Some(sp)=world_to_screen(world_pos,cam_pos,self.viewport.camera.orbit,rect,aspect,ortho_hw,self.render.fov_deg)else{continue};
            let inside=match&drag{SelectionDrag::Box{start}=>egui::Rect::from_two_pos(*start,cur).contains(sp),SelectionDrag::Lasso{center}=>(sp-*center).length()<=(cur-*center).length()};
            if inside {
                if subtract {
                    self.sel.usel.selected.remove(&SceneItemId::Prim(p.prim_uid));
                } else {
                    self.sel.usel.selected.insert(SceneItemId::Prim(p.prim_uid));
                    if first_uid.is_none(){first_uid=Some(p.prim_uid);}
                }
            }
        }
        if subtract {
            if let Some(SceneItemId::Prim(u)) = self.sel.usel.active {
                if !self.sel.usel.selected.contains(&SceneItemId::Prim(u)) {
                    self.sel.usel.active = self.sel.usel.selected.iter().find_map(|id| {
                        if let SceneItemId::Prim(uid) = id { Some(SceneItemId::Prim(*uid)) } else { None }
                    });
                }
            }
        } else if let Some(uid)=first_uid{
            self.sel.usel.active=Some(SceneItemId::Prim(uid));
        } else if !add {
            self.sel.usel.active=self.sel.usel.selected.last().copied();
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    // ---- Viewport selection circles + hover highlight ----
    pub(crate) fn draw_selection_highlights(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let sc = self.viewport.sel_color;
        let sa = (self.viewport.sel_opacity * 255.0) as u8;
        let hover_col = egui::Color32::from_rgba_unmultiplied((sc[0]*255.0) as u8, ((sc[1]*0.8+0.2)*255.0) as u8, ((sc[2]*0.5+0.2)*255.0) as u8, (sa as f32 * 0.5) as u8);

        // SELECTED-prim feedback is the GPU prim outline (exact eval_prim contour —
        // viewport_render's MaskOutline blend pass), not this overlay. The old CPU
        // shape wireframe was deleted (owner-directed 2026-07-09): it approximated the
        // surface (ignored modifiers/blends, lagged elongation) and doubled the GPU
        // outline. This overlay keeps ONLY the hover ring — hover has no GPU pass.
        let sel_prim_set: std::collections::HashSet<usize> =
            self.selected_prim_indices_for_gpu().into_iter().collect();

        for (i, p) in self.scene.primitives.iter().enumerate() {
            if !p.visible { continue; }
            let is_hover = self.tool.hovered_prim == Some(i) && !sel_prim_set.contains(&i);
            if !is_hover { continue; }
            let bt = if BODY_LOCAL_PRIMS {
                self.scene.prim_body_map.get(i)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                    .unwrap_or_default()
            } else { Default::default() };
            let world_pos = if BODY_LOCAL_PRIMS { bt.local_to_world(p.position) } else { p.position };
            let Some(sp) = world_to_screen(world_pos, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) else { continue };
            // Subtle hover ring
            let sr = self.viewport.sel_radius;
            painter.circle_stroke(sp, sr, egui::Stroke::new(self.viewport.sel_thickness.max(0.5), hover_col));
        }
    }

    /// Body that is the **active edit subject** — object outline is suppressed for this
    /// node only (component/sculpt feedback carries the state). Selection state is never
    /// changed; other selected bodies still outline.
    ///
    /// Contexts: poly Vertex/Edge/Face/Group, sculpt mode, NURBS Vertex/Edge/Face.
    /// Object mode → `None` (all selected bodies outline normally).
    pub(crate) fn outline_suppressed_edit_subject(
        &self,
    ) -> Option<core_math::scene_graph::NodeId> {
        // Sculpt: active edit target (brush/component feedback, not object rim).
        if self.tool.work_mode == WorkMode::Sculpt {
            if let Some(v) = self.sculpt.voxel.as_ref() {
                return Some(v.node);
            }
            // Poly sculpt (owner 2026-08-19 screenshot: green rim stuck on the sculpt
            // subject): the contract above always named "sculpt mode", but only the
            // voxel target was implemented — the PolySculpt subject kept its object rim.
            // Same rule as voxel/Blender: the brush cursor is the feedback while inside.
            if let Some((nid, _)) = self.sculpt.poly_edit {
                return Some(nid);
            }
        }
        // Poly sub-modes: user is inside the active poly body.
        // Prefer active_poly_nid; fall back to any active PolygonalBody node so a
        // missing modeling_context tick cannot leave the green rim on.
        if self.poly_work_mode != PolyWorkMode::Object {
            if let Some(n) = self.active_poly_nid() {
                return Some(n);
            }
            if self.modeling_context == ModelingContext::Polygonal {
                if let Some(n) = self.sel.usel.active_node() {
                    return Some(n);
                }
            }
        }
        // NURBS sub-select: user is inside the solid/surface root.
        if self.modeling_context == ModelingContext::Nurbs
            && self.nurbs_work_mode != NurbsWorkMode::Object
        {
            return self.nurbs_solid_root();
        }
        None
    }

    /// Draw silhouette outline for all selected scene graph nodes in object mode.
    /// Also draws bounding box for selected/active SDF bodies.
    /// Color/width follow Viewport Overlays → **Selection Outline** (`show_sel_rim` /
    /// `sel_rim_color` / `sel_outline_width`) when that overall toggle is on.
    ///
    /// **Poly object selection rim is GPU (`sel_bit` → deferred edge-detect)** — same
    /// path as SDF/NURBS body highlight (Blender-style object feedback). This egui path
    /// does **not** walk dense poly edges by default (owner-measured 75–90 ms on ~60k
    /// quads). Shaded+wire display mode is `wire_overlay_enabled` (GPU line pass).
    ///
    /// **Why "deselected" still used to draw wire:** we always forced `active_body_nid`
    /// into the outline set, so an active poly body paid the full CPU walk every idle
    /// frame even with an empty selection. That force is removed.
    ///
    /// **Active edit subject:** when the user is inside a body (poly V/E/F, sculpt, NURBS
    /// sub-select), that body is skipped here and stripped from `selected_outline` for
    /// the GPU rim; component highlights carry state. Other selected bodies still outline.
    ///
    /// Sabotage: `AZRA_CPU_POLY_OUTLINE=1` restores the old CPU silhouette walk so the
    /// perf gate fails on dense meshes.
    pub(crate) fn draw_node_selection_outlines(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let _outline_t0 = std::time::Instant::now();
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let cam_fwd = self.viewport.camera.orbit * Vec3::new(0.0, 0.0, -1.0);
        let (sc, sa, stroke_w) = if self.viewport.show_sel_rim {
            (
                self.viewport.sel_rim_color,
                255u8,
                self.viewport.sel_outline_width.max(0.5),
            )
        } else {
            (
                self.viewport.sel_color,
                (self.viewport.sel_opacity * 255.0) as u8,
                self.viewport.sel_thickness.max(0.5),
            )
        };
        let sel_col = egui::Color32::from_rgba_unmultiplied(
            (sc[0] * 255.0) as u8,
            (sc[1] * 255.0) as u8,
            (sc[2] * 255.0) as u8,
            sa,
        );

        // Explicit selection only — do NOT force active_body (that was the deselected 80ms leak).
        // When prims are selected, usel already includes the body / prims as appropriate.
        // Active edit subject is skipped (component highlights carry state); other selected
        // bodies still get outline even while the user is inside another body.
        let suppress = self.outline_suppressed_edit_subject();
        let sel_nodes: Vec<_> = self.sel.usel.selected_nodes().collect();
        if sel_nodes.is_empty() {
            self.poly_sel_outline_ms = 0.0;
            return;
        }
        let force_cpu_poly = crate::overlay_lines::cpu_poly_outline_sabotage();
        for &nid in &sel_nodes {
            if Some(nid) == suppress {
                continue;
            }
            let world_xform = self.scene.scene_graph.world_transform(nid);
            let xform_pos = |local: Vec3| -> Vec3 {
                let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
                Vec3::new(w.x, w.y, w.z)
            };

            // PolygonalBody: object rim is deferred `sel_bit` (raster_gbuffer). Skip the
            // egui walk unless sabotage re-enables CPU silhouette for perf gates.
            let is_poly = matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::PolygonalBody { .. })
            );
            if is_poly {
                if force_cpu_poly {
                    self.draw_poly_silhouette_cpu(
                        painter,
                        rect,
                        nid,
                        cam_pos,
                        cam_fwd,
                        aspect,
                        ortho_hw,
                        stroke_w,
                        sel_col,
                    );
                }
                continue;
            }

            // Bounding box for NURBS/Imported (SdfBody uses screen-space outline via post-process)
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => continue,
            };
            let (bb_min, bb_max) = match &node.payload {
                NodePayload::ImportedMesh { .. } => {
                    // The voxel sculpt object renders its own mesh; the fixed (-0.5..0.5) bbox
                    // cube around it is just noise during transform — suppress it for that node.
                    if self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid) { continue; }
                    (Vec3::splat(-0.5), Vec3::splat(0.5))
                }
                NodePayload::VoxelBody { .. } => {
                    // P0.5: voxel renders its own surface — suppress the placeholder bbox cube.
                    if self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid) { continue; }
                    (Vec3::splat(-0.5), Vec3::splat(0.5))
                }
                NodePayload::NurbsBody { surface, .. } => {
                    let mut mn = Vec3::splat(f32::MAX);
                    let mut mx = Vec3::splat(f32::MIN);
                    for row in &surface.control_points {
                        for cp in row {
                            mn = mn.min(cp.position);
                            mx = mx.max(cp.position);
                        }
                    }
                    if mn.x > mx.x { continue; }
                    (mn, mx)
                }
                NodePayload::NurbsSolid { faces, .. } => {
                    let mut mn = Vec3::splat(f32::MAX);
                    let mut mx = Vec3::splat(f32::MIN);
                    for face in faces {
                        for row in &face.surface.control_points {
                            for cp in row {
                                mn = mn.min(cp.position);
                                mx = mx.max(cp.position);
                            }
                        }
                    }
                    if mn.x > mx.x {
                        continue;
                    }
                    (mn, mx)
                }
                NodePayload::SdfBody { .. } => {
                    // SdfBody silhouette outline handled by post-process shader
                    continue;
                }
                _ => continue,
            };
            let signs: [(f32,f32,f32); 8] = [
                (0.0,0.0,0.0), (1.0,0.0,0.0), (1.0,0.0,1.0), (0.0,0.0,1.0),
                (0.0,1.0,0.0), (1.0,1.0,0.0), (1.0,1.0,1.0), (0.0,1.0,1.0),
            ];
            let corners: Vec<Option<egui::Pos2>> = signs.iter().map(|&(sx, sy, sz)| {
                let local = Vec3::new(
                    bb_min.x + (bb_max.x - bb_min.x) * sx,
                    bb_min.y + (bb_max.y - bb_min.y) * sy,
                    bb_min.z + (bb_max.z - bb_min.z) * sz,
                );
                let wp = xform_pos(local);
                world_to_screen(wp, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)
            }).collect();
            let edges: [(usize,usize); 12] = [
                (0,1),(1,2),(2,3),(3,0),(4,5),(5,6),(6,7),(7,4),(0,4),(1,5),(2,6),(3,7),
            ];
            for (a, b) in edges {
                if let (Some(sa), Some(sb)) = (corners[a], corners[b]) {
                    painter.line_segment([sa, sb], egui::Stroke::new(1.5, sel_col));
                }
            }
        }
        self.poly_sel_outline_ms = _outline_t0.elapsed().as_secs_f64() as f32 * 1000.0;
    }

    /// Sabotage-only: CPU silhouette walk for a poly body (was the default selection outline).
    /// O(edges) face-facing tests + screen projection every frame — dense meshes 75–90ms.
    fn draw_poly_silhouette_cpu(
        &mut self,
        painter: &egui::Painter,
        rect: egui::Rect,
        nid: core_math::scene_graph::NodeId,
        cam_pos: Vec3,
        cam_fwd: Vec3,
        aspect: f32,
        ortho_hw: f32,
        stroke_w: f32,
        sel_col: egui::Color32,
    ) {
        let Some(display_arc) = self.poly_cached_display_mesh(nid) else {
            return;
        };
        let display = display_arc.as_ref();
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform_pos = |local: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };
        use core_math::half_edge::{NO_FACE, NO_TWIN};
        for i in 0..display.half_edges.len() {
            if !display.is_edge_live(core_math::half_edge::HalfEdgeId(i as u32)) {
                continue;
            }
            let he = &display.half_edges[i];
            let twin = he.twin;
            if twin != NO_TWIN && twin.0 < i as u32 {
                continue;
            }
            let dst = he.vertex;
            let src_id = display.he_source(core_math::half_edge::HalfEdgeId(i as u32));
            let p0 = xform_pos(display.vert(src_id).position);
            let p1 = xform_pos(display.vert(dst).position);
            let mid = (p0 + p1) * 0.5;
            let view_dir = if ortho_hw > 0.0 {
                -cam_fwd
            } else {
                (mid - cam_pos).normalize_or_zero()
            };
            let face_a = he.face;
            let face_b = if twin != NO_TWIN {
                display.he(twin).face
            } else {
                NO_FACE
            };
            let face_facing = |fid: core_math::half_edge::FaceId| -> Option<f32> {
                if fid == NO_FACE {
                    return None;
                }
                let vids = display.face_vertices(fid);
                if vids.len() < 3 {
                    return None;
                }
                let a = xform_pos(display.vert(vids[0]).position);
                let b = xform_pos(display.vert(vids[1]).position);
                let c = xform_pos(display.vert(vids[2]).position);
                let fn_dir = (b - a).cross(c - a).normalize_or_zero();
                Some(fn_dir.dot(-view_dir))
            };
            let facing_a = face_facing(face_a);
            let facing_b = face_facing(face_b);
            const DEAD_BAND: f32 = 0.03;
            let is_silhouette = match (facing_a, facing_b) {
                (Some(da), Some(db)) => {
                    (da > 0.0) != (db > 0.0) && da.abs().max(db.abs()) > DEAD_BAND
                }
                (Some(_), None) | (None, Some(_)) => true,
                (None, None) => false,
            };
            if !is_silhouette {
                continue;
            }
            if let Some((a, b)) = world_segment_to_screen(
                p0,
                p1,
                cam_pos,
                self.viewport.camera.orbit,
                rect,
                aspect,
                ortho_hw,
                self.render.fov_deg,
            ) {
                painter.line_segment([a, b], egui::Stroke::new(stroke_w.max(1.5), sel_col));
            }
        }
    }
}
