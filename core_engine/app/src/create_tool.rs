// core_engine/app/src/create_tool.rs
// Modo-style modal creation tool + box bevel system
//
// ENGINE LAYER: owns footprint/extrude session state (CreateState phases).
// Pure math lives in ops::geo::placement_math; shape data in ops::geo::shape_presets.
// External/UI/node entry: ops::create::session / ops::create::{instant_origin,click_place}.
//
// !! DO NOT CHANGE the placement logic in start_create() or update_create_extrude() !!
// The creation plane priority (surface → floor → workplane), right-handed tangent frames,
// and delta-based extrude depth were carefully debugged. See workplane.rs for frame convention:
// all tangent frames must be right-handed (tv = tu.cross(normal)) for frame_to_rotation_deg.
use eframe::egui;
use glam::{Vec3, Quat};
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BoxBevel, BevelMode};
use core_math::scene_graph::BodyTransform;
use crate::{WorkMode, ModelingContext};
use crate::{AzraApp, ActiveTool, ToolMode, CreateState, CreatePhase, CreateOp, WorkPlaneMode, world_to_screen};
use crate::BevelDrag;

/// Snapshot of bevel radii / angles / profiles at drag start (for undo no-op detection).
pub(crate) struct BevelDragStartSnap {
    pub ca_bot: f32,
    pub ca_top: f32,
    pub ca_vert: f32,
    pub bp_bot: f32,
    pub bp_top: f32,
    pub bp_vert: f32,
    pub box_radii: [f32; 12],
    pub cyl_top: f32,
    pub cyl_bot: f32,
}

// Pure geo kernels — re-exported so create_tool call sites / tests keep a short path.
pub(crate) use crate::ops::geo::placement_math::{compute_extrude_depth, compute_creation_plane};

impl AzraApp {
    /// Ctrl+Click on shape button: instant-create at world origin [0,0,0] with default size.
    pub(crate) fn instant_create_at_origin(&mut self, shape_idx: usize) {
        let shape = Self::shape_preset(shape_idx);
        let uid = self.scene.next_prim_id;
        let label = shape.label().to_string();
        let name = self.next_name(&label);
        let desc = format!("Create {}", name);
        let inherit = self.inherited_material_for_create(); // new prim blends into the target body
        self.transaction(&desc, |s| {
            let idx = s.add_prim_to_graph(SdfPrimitive::with_uid(name, shape, Vec3::ZERO, uid));
            let op = s.tool.create_op.to_boolean_op(s.tool.create_blend_k);
            s.write_prim(idx, |p| { p.operation = op; p.material_id = inherit; });
            s.select_single(idx);
            if s.modeling_context == ModelingContext::Sdf && s.tool.work_mode != WorkMode::Object {
                s.switch_work_mode(WorkMode::Object);
            }
            s.tool.gizmo_armed = true;
            s.rebake_no_undo();
        });
        self.toast(&format!("{} created at origin", label));
    }

    /// Simple click-create: raycast the click position and place a default-sized
    /// shape there, properly oriented to the hit surface/floor. No footprint or
    /// extrude — just click and get a cube (or whatever the active tool shape is).
    pub(crate) fn click_create_at_point(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let (origin, dir) = self.cam_ray(mouse, rect);

        let wp = self.viewport.wp_resolved;
        let scene_hit = self.raycast_scene(origin, dir);
        let (hit_point, hit_normal, on_surface) = match compute_creation_plane(
            origin, dir,
            self.viewport.wp_mode == WorkPlaneMode::Locked,
            wp.origin, wp.normal,
            scene_hit,
            self.viewport.grid_floor_y,
        ) {
            Some(r) => r,
            None => return,
        };

        let shape_idx = self.tool.mode.armed_shape_idx().unwrap_or(1); // default = Box
        let shape = Self::shape_preset(shape_idx);
        let label = shape.label().to_string();

        let is_spine = matches!(&shape, SdfShape::SpinePipe { .. });

        // Position: offset half the shape height above the surface (pure geo kernel).
        // Spine: sits ON the hit/workplane (helper returns 0) so seed + new points share one plane.
        let shape_half_y = crate::ops::geo::half_extent_along_up(&shape);
        let pos = hit_point + hit_normal * shape_half_y;

        // Orient: spine uses **resolved workplane** frame (same as clip-curve / polyline).
        // Other shapes follow surface/floor like before.
        let (tu, normal, tv) = if is_spine {
            let wp = self.viewport.wp_resolved;
            (wp.tangent_u, wp.normal, wp.tangent_v)
        } else if on_surface {
            let (tu, tv) = Self::surface_tangent_frame(hit_normal, self.viewport.camera.orbit);
            (tu, hit_normal, tv)
        } else if hit_normal.y.abs() > self.tweak.floor_detect_threshold {
            (Vec3::X, hit_normal, Vec3::Z)
        } else {
            (
                self.viewport.wp_resolved.tangent_u,
                hit_normal,
                self.viewport.wp_resolved.tangent_v,
            )
        };
        let rot_deg = Self::frame_to_rotation_deg(tu, normal, tv);

        let uid = self.scene.next_prim_id;
        let name = self.next_name(&label);
        let desc = format!("Create {}", name);
        let inherit = self.inherited_material_for_create(); // new prim blends into the target body
        self.transaction(&desc, |s| {
            let mut prim = SdfPrimitive::with_uid(name, shape, pos, uid);
            prim.operation = s.tool.create_op.to_boolean_op(s.tool.create_blend_k);
            prim.rotation_deg = rot_deg;
            prim.material_id = inherit;
            let idx = s.add_prim_to_graph(prim);
            s.select_single(idx);
            if is_spine {
                s.set_gizmo_armed(false);
            } else if s.modeling_context == ModelingContext::Sdf {
                // Exit edit mode → Object with new prim selected + gizmo ready.
                if s.tool.work_mode != WorkMode::Object {
                    s.switch_work_mode(WorkMode::Object);
                }
                s.tool.gizmo_armed = true;
            }
            s.rebake_no_undo();
        });
        if is_spine {
            // Shared authoring setup (same path as the Add-menu op — spine_point_edit.rs).
            if let Some(i) = self.prim_index_by_uid(uid) {
                self.spine_enter_edit(i);
            }
        } else {
            self.toast(&format!("{} placed", label));
        }
    }

    /// Deactivate the active creation tool.
    pub(crate) fn deactivate_tool(&mut self) {
        if self.tool.mode.is_create_armed() {
            self.set_mode(ToolMode::Select);
            self.toast("Tool dropped");
        }
    }

    /// Cycle create operation: Cut → Slice → Intersect → Cut.
    pub(crate) fn cycle_create_op(&mut self) {
        let next = match self.tool.create_op {
            CreateOp::Subtract  => CreateOp::Slice,
            CreateOp::Slice     => CreateOp::Intersect,
            CreateOp::Intersect => CreateOp::Subtract,
            CreateOp::Union     => CreateOp::Subtract,
        };
        self.set_create_op(next);
    }

    /// Start a click+drag creation: raycast → create thin box aligned to surface → enter Footprint phase.
    pub(crate) fn start_create(&mut self, mouse: egui::Pos2, rect: egui::Rect) -> Option<CreateState> {
        let (origin, dir) = self.cam_ray(mouse, rect);

        // Creation plane priority: locked WP → surface → floor → WP fallback
        let wp = self.viewport.wp_resolved;
        let scene_hit = self.raycast_scene(origin, dir);
        let (hit_point, hit_normal, on_surface) = compute_creation_plane(
            origin, dir,
            self.viewport.wp_mode == WorkPlaneMode::Locked,
            wp.origin, wp.normal,
            scene_hit,
            self.viewport.grid_floor_y,
        )?;

        // Tangent frame: surface = camera-relative, floor = world-aligned, workplane = pre-computed.
        let (tu, tv) = if on_surface {
            Self::surface_tangent_frame(hit_normal, self.viewport.camera.orbit)
        } else if hit_normal.y.abs() > self.tweak.floor_detect_threshold {
            (Vec3::X, Vec3::Z)
        } else {
            (self.viewport.wp_resolved.tangent_u, self.viewport.wp_resolved.tangent_v)
        };
        let rot_deg = Self::frame_to_rotation_deg(tu, hit_normal, tv);

        let shape_idx = self.tool.mode.armed_shape_idx().unwrap_or(1);
        let uid = self.scene.next_prim_id;
        let shape = Self::shape_preset(shape_idx);
        let name = self.next_name(shape.label());
        // Start with a tiny version of the chosen shape
        let tiny_shape = Self::shape_preset_tiny(shape_idx);
        let mut prim = SdfPrimitive::with_uid(name, tiny_shape, hit_point, uid);
        prim.operation = self.tool.create_op.to_boolean_op(self.tool.create_blend_k);
        prim.rotation_deg = rot_deg;
        // Inherit the target body's prevailing material (owner ask — new prim blends in).
        prim.material_id = self.inherited_material_for_create();

        let mut prim_idx = 0usize;
        self.transaction("Create", |s| {
            prim_idx = s.add_prim_to_graph(prim);
            s.select_single(prim_idx);
        });
        // Do NOT set is_dirty here — the tiny placeholder prim should NOT be
        // uploaded to the GPU during footprint. The overlay draws the rectangle
        // directly from CreateState. Primitive is first shown on finalize_footprint.

        Some(CreateState {
            phase: CreatePhase::Footprint,
            op: self.tool.create_op,
            shape_idx,
            hit_point, hit_normal,
            tangent_u: tu, tangent_v: tv,
            anchor: hit_point,
            local_u: 0.0, local_v: 0.0,
            depth: 0.0, depth_start_t: 0.0,
            corner_radius: 0.0, corner_radius_tb: 0.0, corner_chamfer: false,
            blend_k: self.tool.create_blend_k,
            prim_idx,
            num_buf: String::new(),
            center_mode: false,
            rotation_angle: 0.0,
            base_rotation_deg: rot_deg,
            depth_orbit_frozen: false,
        })
    }

    /// Update footprint during drag — only updates local_u / local_v in CreateState.
    /// The primitive is NOT touched; the overlay draws from CreateState directly.
    /// `snap_world` — if Some, use this world point instead of raycasting mouse.
    pub(crate) fn update_create_footprint(&mut self, mouse: egui::Pos2, rect: egui::Rect, shift: bool, alt: bool, snap_world: Option<Vec3>) {
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let (hit_point, hit_normal, tu, tv, anchor) =
            (cs.hit_point, cs.hit_normal, cs.tangent_u, cs.tangent_v, cs.anchor);

        // Alt rotation: compute angle from horizontal mouse delta relative to anchor screen position
        let rotation_angle = if alt {
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
            if let Some(anchor_screen) = world_to_screen(anchor, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let dx = mouse.x - anchor_screen.x;
                dx * self.tweak.create_alt_rot_speed // degrees per pixel
            } else { 0.0 }
        } else {
            self.tool.current.as_create().map_or(0.0, |cs| cs.rotation_angle)
        };

        // Apply rotation to tangent frame
        let rot = Quat::from_axis_angle(hit_normal, rotation_angle.to_radians());
        let eff_tu = rot * tu;
        let eff_tv = rot * tv;

        // If smart snap provided a world position, project it onto the creation surface plane;
        // otherwise raycast from mouse as before.
        let surface_pt = if let Some(wp) = snap_world {
            let d = wp - hit_point;
            wp - hit_normal * d.dot(hit_normal)
        } else {
            match self.ray_plane_hit(mouse, rect, hit_point, hit_normal) {
                Some(p) => p, None => return,
            }
        };

        // When Alt is held, we only rotate — don't resize. Use the rotated frame for lu/lv.
        let (use_tu, use_tv) = if alt { (eff_tu, eff_tv) } else { (tu, tv) };
        let mut lu = (surface_pt - anchor).dot(use_tu);
        let mut lv = (surface_pt - anchor).dot(use_tv);

        if shift && !alt {
            let m = lu.abs().max(lv.abs());
            lu = m * lu.signum();
            lv = m * lv.signum();
        }

        if let Some(cs) = self.tool.current.as_create_mut() {
            if !alt {
                cs.local_u = lu;
                cs.local_v = lv;
            }
            if alt {
                cs.rotation_angle = rotation_angle;
            }
        }
    }

    /// Finalize the footprint and prepare the primitive for the extrude phase.
    /// Called once when the mouse is released after drawing the footprint.
    /// `center_mode` = Ctrl+Shift held (symmetric growth from center).
    pub(crate) fn finalize_footprint(&mut self, center_mode: bool) {
        if let Some(cs) = self.tool.current.as_create_mut() {
            cs.center_mode = center_mode;
        }
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let (anchor, tu, tv, hit_normal, prim_idx, _shape_idx) =
            (cs.anchor, cs.tangent_u, cs.tangent_v, cs.hit_normal, cs.prim_idx, cs.shape_idx);
        let (lu, lv) = (cs.local_u, cs.local_v);
        let rotation_angle = cs.rotation_angle;

        let init_he_y = 0.001; // start nearly flat, extrude grows from here

        // Apply rotation to tangent frame for position computation
        let rot = Quat::from_axis_angle(hit_normal, rotation_angle.to_radians());
        let eff_tu = rot * tu;
        let eff_tv = rot * tv;

        let pos = if center_mode {
            let footprint_center = anchor + eff_tu * (lu * 0.5) + eff_tv * (lv * 0.5);
            footprint_center + hit_normal * init_he_y
        } else {
            let he_x = lu * 0.5;
            let he_z = lv * 0.5;
            anchor + eff_tu * he_x + eff_tv * he_z + hit_normal * init_he_y
        };

        // Footprint dimensions → shape-specific parameters
        let he_x = (lu.abs() * 0.5).max(0.001);
        let he_z = (lv.abs() * 0.5).max(0.001);
        let r_max = he_x.max(he_z);   // radius for circular shapes
        let r_min = he_x.min(he_z);   // minor dim for capsule/torus

        if prim_idx < self.scene.primitives.len() {
            let rotated_rot_deg = if rotation_angle.abs() > 0.001 {
                Some(Self::frame_to_rotation_deg(eff_tu, hit_normal, eff_tv))
            } else {
                None
            };
            self.write_prim(prim_idx, |p| {
                p.position = pos;
                match &mut p.shape {
                    SdfShape::Box { ref mut half_extents, .. } => {
                        *half_extents = Vec3::new(he_x, init_he_y, he_z);
                    }
                    SdfShape::Sphere { ref mut radius } => {
                        *radius = r_max;
                    }
                    SdfShape::Cylinder { ref mut radius, ref mut half_height } => {
                        *radius = r_max;
                        *half_height = init_he_y;
                    }
                    SdfShape::Cone { ref mut radius, ref mut half_height } => {
                        *radius = r_max;
                        *half_height = init_he_y;
                    }
                    SdfShape::Capsule { ref mut radius, ref mut half_height } => {
                        *radius = r_min.max(0.01);
                        *half_height = init_he_y;
                    }
                    SdfShape::Torus { ref mut major_radius, ref mut minor_radius } => {
                        *major_radius = r_max;
                        *minor_radius = (r_min * 0.5).max(0.01);
                    }
                    SdfShape::Stair { ref mut half_extents, .. } => {
                        *half_extents = Vec3::new(he_x, init_he_y, he_z);
                    }
                    SdfShape::SpiralStair { ref mut radius, ref mut half_height, .. } => {
                        *radius = r_max;
                        *half_height = init_he_y;
                    }
                    SdfShape::Billboard { ref mut width, ref mut height, .. } => {
                        *width = he_x * 2.0;
                        *height = init_he_y * 2.0;
                    }
                    SdfShape::CurveExtrude { .. } => {}
                    SdfShape::MeshVolume { .. } => {}
                    SdfShape::HalfSpace => {} // infinite plane, no drag-to-size
                    SdfShape::SpinePipe { .. } => {} // spine points authored; not footprint-drag scaled
                }
                // Apply rotation to the primitive's orientation
                if let Some(rot_deg) = rotated_rot_deg {
                    p.rotation_deg = rot_deg;
                }
            });
            self.rebake_no_undo();
        }
    }

    /// Update the extrusion depth (Extrude phase): box top face tracks mouse cursor exactly.
    /// `shift` = symmetric extrude (box center at surface, grows up/down equally).
    /// `alt` = hinge-tilt: rotate shape around the short footprint edge at the anchor.
    pub(crate) fn update_create_extrude(&mut self, mouse: egui::Pos2, rect: egui::Rect, shift: bool, alt: bool) {
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let (hit_normal, prim_idx) = (cs.hit_normal, cs.prim_idx);
        let (lu, lv) = (cs.local_u, cs.local_v);
        let blend_k = cs.blend_k;

        if prim_idx >= self.scene.primitives.len() { return; }

        // Compute tilt angle when Alt is held (hinge rotation around short edge)
        let cs = self.tool.current.as_create().unwrap();
        let tilt_angle = if alt {
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
            let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
            if let Some(anchor_screen) = world_to_screen(cs.anchor, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                let dx = mouse.x - anchor_screen.x;
                dx * self.tweak.create_alt_rot_speed // degrees
            } else { cs.rotation_angle }
        } else {
            cs.rotation_angle
        };

        // Footprint tangent frame (no surface-plane rotation during extrude)
        let base_tu = cs.tangent_u;
        let base_tv = cs.tangent_v;
        let draw_center = cs.anchor + base_tu * (lu * 0.5) + base_tv * (lv * 0.5);

        // ---------- Fence-plane depth ----------
        // A virtual plane hinged on the extrude axis, always facing the camera.
        // Intersect mouse ray → project onto axis. Robust at all camera angles.
        let depth = if alt {
            cs.depth
        } else {
            let (ray_o, ray_d) = self.cam_ray(mouse, rect);
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize();
            let start_t = cs.depth_start_t;
            match compute_extrude_depth(ray_o, ray_d, draw_center, hit_normal, cam_pos, cam_up) {
                Some(t) => t - start_t,
                None => cs.depth,
            }
        };

        let he_x = (lu.abs() * 0.5).max(0.001);
        let he_z = (lv.abs() * 0.5).max(0.001);
        let he_y = depth.abs().max(0.001) * 0.5;

        let op = cs.op;
        let sink = if matches!(op, CreateOp::Subtract | CreateOp::Intersect | CreateOp::Slice) {
            self.tweak.create_sink_depth
        } else { 0.0 };

        // Determine final frame + position (with optional hinge tilt)
        let (final_tu, final_normal, final_tv, final_pos);
        if tilt_angle.abs() > 0.001 {
            // Hinge tilt: rotate around the SHORT edge at the anchor.
            // Short edge = along tangent_v if |lu| >= |lv|, else along tangent_u.
            let (hinge_axis, swing_dir, swing_len) = if lu.abs() >= lv.abs() {
                // Short edge along tv at anchor; box swings along tu
                (base_tv, base_tu, lu)
            } else {
                // Short edge along tu at anchor; box swings along tv
                (base_tu, base_tv, lv)
            };
            let tilt_q = Quat::from_axis_angle(hinge_axis.normalize(), tilt_angle.to_radians());
            let tilted_swing = tilt_q * swing_dir;
            let tilted_normal = tilt_q * hit_normal;
            // Hinge midpoint: midpoint of short edge at anchor
            let hinge_mid = if lu.abs() >= lv.abs() {
                cs.anchor + base_tv * (lv * 0.5)
            } else {
                cs.anchor + base_tu * (lu * 0.5)
            };
            // Box center pivots around hinge
            let center_offset = tilted_swing * (swing_len * 0.5) + tilted_normal * (depth * 0.5 + sink);
            final_pos = hinge_mid + center_offset;
            // Reconstruct frame: tu maps to local X, normal maps to local Y, tv maps to local Z
            if lu.abs() >= lv.abs() {
                final_tu = tilted_swing;
                final_normal = tilted_normal;
                final_tv = hinge_axis;
            } else {
                final_tu = hinge_axis;
                final_normal = tilted_normal;
                final_tv = tilted_swing;
            }
        } else {
            final_tu = base_tu;
            final_normal = hit_normal;
            final_tv = base_tv;
            final_pos = if shift {
                draw_center + hit_normal * sink
            } else {
                draw_center + hit_normal * (depth * 0.5 + sink)
            };
        }

        // Shape-specific extrude: update dimensions based on depth
        let r_max = he_x.max(he_z);
        let bool_op = op.to_boolean_op(blend_k);
        let rot_deg = Self::frame_to_rotation_deg(final_tu, final_normal, final_tv);
        let depth_abs = depth.abs();
        self.write_prim(prim_idx, |p| {
            p.position = final_pos;
            match &mut p.shape {
                SdfShape::Box { ref mut half_extents, .. } => {
                    *half_extents = Vec3::new(he_x, he_y, he_z);
                }
                SdfShape::Sphere { ref mut radius } => {
                    *radius = r_max.max(depth_abs);
                }
                SdfShape::Cylinder { ref mut half_height, .. } => {
                    *half_height = he_y;
                }
                SdfShape::Cone { ref mut half_height, .. } => {
                    *half_height = he_y;
                }
                SdfShape::Capsule { ref mut half_height, .. } => {
                    *half_height = he_y;
                }
                SdfShape::Torus { ref mut minor_radius, .. } => {
                    *minor_radius = (depth_abs * 0.5).max(0.01);
                }
                SdfShape::Stair { ref mut half_extents, .. } => {
                    *half_extents = Vec3::new(he_x, he_y, he_z);
                }
                SdfShape::SpiralStair { ref mut half_height, .. } => {
                    *half_height = he_y;
                }
                SdfShape::Billboard { ref mut height, .. } => {
                    *height = he_y * 2.0;
                }
                SdfShape::CurveExtrude { .. } => {}
                SdfShape::MeshVolume { .. } => {}
                SdfShape::HalfSpace => {} // infinite plane, no drag-to-size
                SdfShape::SpinePipe { .. } => {}
            }
            p.operation = bool_op;
            p.rotation_deg = rot_deg;
        });

        if let Some(cs) = self.tool.current.as_create_mut() {
            cs.depth = depth;
            if alt { cs.rotation_angle = tilt_angle; }
        }
        self.rebake_no_undo();
    }

    /// Commit the creation. If `infinite`, set huge depth for through-cut.
    pub(crate) fn commit_create(&mut self, infinite: bool) {
        if let Some(cs) = self.tool.current.as_create() {
            let prim_idx = cs.prim_idx;
            let (lu, lv, anchor, _hit_point) = (cs.local_u, cs.local_v, cs.anchor, cs.hit_point);
            let blend_k = cs.blend_k;
            let tu = cs.tangent_u;
            let tv = cs.tangent_v;
            let op = cs.op;

            if prim_idx < self.scene.primitives.len() {
                // Pre-compute values needed inside the closure
                let draw_center = anchor + tu * (lu * 0.5) + tv * (lv * 0.5);
                let bool_op = op.to_boolean_op(blend_k);
                let cut_d = self.scene_bound_radius().max(self.tweak.through_cut_depth);
                let sdf_bias = self.tweak.sdf_placement_bias;
                let he_x = (lu.abs() * 0.5).max(0.01);
                let he_z = (lv.abs() * 0.5).max(0.01);

                self.write_prim(prim_idx, |p| {
                    if infinite {
                        p.position = draw_center;
                        p.operation = bool_op;
                        // Dynamic through-cut depth: scene bounding radius (floor: tweak value)
                        match &mut p.shape {
                            SdfShape::Box { ref mut half_extents, .. } => {
                                *half_extents = Vec3::new(he_x, cut_d, he_z);
                            }
                            SdfShape::Cylinder { ref mut half_height, .. }
                            | SdfShape::Cone { ref mut half_height, .. }
                            | SdfShape::Capsule { ref mut half_height, .. } => {
                                *half_height = cut_d;
                            }
                            SdfShape::Sphere { ref mut radius } => {
                                *radius = cut_d;
                            }
                            SdfShape::Stair { ref mut half_extents, .. } => {
                                half_extents.y = cut_d;
                            }
                            SdfShape::SpiralStair { ref mut half_height, .. } => {
                                *half_height = cut_d;
                            }
                            _ => {}
                        }
                    }

                    // Compensate for SDF raymarcher hit-threshold inset:
                    // The raymarcher stops at hit_threshold (0.002) inside the true
                    // surface. Inflate by this amount so the committed SDF matches
                    // the geometric boundary.
                    if !infinite {
                        match &mut p.shape {
                            SdfShape::Box { half_extents, .. }
                             => {
                                *half_extents += Vec3::splat(sdf_bias);
                            }
                            SdfShape::Sphere { radius } => *radius += sdf_bias,
                            SdfShape::Cylinder { radius, half_height }
                            | SdfShape::Cone { radius, half_height }
                            | SdfShape::Capsule { radius, half_height } => {
                                *radius += sdf_bias; *half_height += sdf_bias;
                            }
                            SdfShape::Torus { major_radius, minor_radius } => {
                                *major_radius += sdf_bias; *minor_radius += sdf_bias;
                            }
                            SdfShape::Stair { half_extents, .. } => *half_extents += Vec3::splat(sdf_bias),
                            SdfShape::SpiralStair { radius, half_height, .. } => {
                                *radius += sdf_bias; *half_height += sdf_bias;
                            }
                            SdfShape::Billboard { .. } => {}
                            SdfShape::CurveExtrude { .. } => {}
                            SdfShape::MeshVolume { .. } => {}
                            SdfShape::HalfSpace => {} // infinite plane, no surface-inset bias
                            SdfShape::SpinePipe { .. } => {}
                        }
                    }

                    // Handle Slice: make it a thin infinite-plane subtract
                    if op == CreateOp::Slice {
                        // Convert any shape to a large thin box for slicing
                        p.shape = SdfShape::Box {
                            half_extents: Vec3::new(10.0, 0.001, 10.0),
                            bevel: BoxBevel::default(),
                        };
                        p.operation = BooleanOp::Subtract { k: 0.0 };
                    }
                });

                // Select the new prim and leave Object mode (exit Vertex edit if we were in it).
                self.select_single(prim_idx);
                if self.modeling_context == ModelingContext::Sdf
                    && self.tool.work_mode != WorkMode::Object
                {
                    self.switch_work_mode(WorkMode::Object);
                }
                self.tool.gizmo_armed = true;
                self.rebake_no_undo();
            }
        }
        self.tool.current = ActiveTool::Idle;
    }

    /// Cancel the active creation, removing the live primitive.
    pub(crate) fn cancel_create(&mut self) {
        if let Some(cs) = self.tool.current.as_create() {
            let idx = cs.prim_idx;
            if idx < self.scene.primitives.len() {
                self.remove_prim_from_graph(idx);
                self.pop_last_undo();
                self.deselect_all();
                self.rebake_no_undo();
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.toast("Create cancelled");
    }

    /// Recalibrate depth_start_t after camera orbit so depth stays continuous.
    pub(crate) fn unfreeze_depth_orbit(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let (ray_o, ray_d) = self.cam_ray(mouse, rect);
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_up = (self.viewport.camera.orbit * Vec3::Y).normalize();
        if let Some(cs) = self.tool.current.as_create_mut() {
            let draw_center = cs.anchor + cs.tangent_u * (cs.local_u * 0.5)
                                        + cs.tangent_v * (cs.local_v * 0.5);
            if let Some(t_now) = compute_extrude_depth(ray_o, ray_d, draw_center, cs.hit_normal, cam_pos, cam_up) {
                cs.depth_start_t = t_now - cs.depth;
            }
            cs.depth_orbit_frozen = false;
        }
    }

    /// Update bevel on the creation-tool primitive.
    /// Vertical edges (8-11) use `corner_radius`, top+bottom edges (0-7) use `corner_radius_tb`.
    pub(crate) fn update_create_bevel(&mut self) {
        let cs = match self.tool.current.as_create() { Some(cs) => cs, None => return };
        let (prim_idx, r_vert, r_tb, chamfer) = (cs.prim_idx, cs.corner_radius, cs.corner_radius_tb, cs.corner_chamfer);
        if prim_idx >= self.scene.primitives.len() { return; }
        self.write_prim(prim_idx, |p| {
            if let SdfShape::Box { ref mut bevel, .. } = p.shape {
                let mode_fn = |r: f32| -> BevelMode {
                    if r < 0.001 { BevelMode::None } else if chamfer { BevelMode::Chamfer } else { BevelMode::Round }
                };
                // Bottom edges (0-3) + Top edges (4-7)
                for i in 0..8  { bevel.radii[i] = r_tb;   bevel.modes[i] = mode_fn(r_tb); }
                // Vertical edges (8-11)
                for i in 8..12 { bevel.radii[i] = r_vert;  bevel.modes[i] = mode_fn(r_vert); }
            }
        });
        self.rebake_no_undo();
    }

    /// Set the create operation on the live primitive and update it immediately.
    pub(crate) fn set_create_op(&mut self, op: CreateOp) {
        self.tool.create_op = op;
        if let Some(cs) = self.tool.current.as_create_mut() {
            cs.op = op;
            let blend_k = cs.blend_k;
            let prim_idx = cs.prim_idx;
            let bool_op = op.to_boolean_op(blend_k);
            if prim_idx < self.scene.primitives.len() {
                self.write_prim(prim_idx, |p| p.operation = bool_op);
                self.rebake_no_undo();
            }
        }
        self.toast(op.label());
    }

    /// Returns a short status string for the header bar when a create tool is active.
    pub(crate) fn create_tool_status_text(&self) -> Option<String> {
        let cs = self.tool.current.as_create()?;
        let shape = Self::shape_preset(cs.shape_idx);
        let shape_label = shape.label();
        let phase = match cs.phase {
            CreatePhase::Footprint => "Footprint",
            CreatePhase::Extrude => "Extrude",
        };
        let op = cs.op.label();
        Some(format!("{} {} — {} [{:.2} × {:.2} × {:.2}]",
            op, shape_label, phase,
            cs.local_u.abs(), cs.local_v.abs(), cs.depth.abs()))
    }

    // ---------------------------------------------------------------
    // Box edge bevel helpers
    // ---------------------------------------------------------------

    /// World-space position for an overlay handle (Uniform bevel square, blend-k diamond, …).
    /// Anchored at the **prim center**, then offset along camera-right so it always sits
    /// **outside** the OBB silhouette (extent along that axis + padding).
    pub(crate) fn overlay_handle_off_prim(
        prim_pos: Vec3,
        prim_rot: Quat,
        prim_scale: Vec3,
        half_extents: Vec3,
        bt: &BodyTransform,
        cam_right: Vec3,
    ) -> Vec3 {
        let right = cam_right.normalize_or_zero();
        if right.length_squared() < 1e-8 {
            return bt.local_to_world(prim_pos);
        }
        // Prim center in world.
        let center = bt.local_to_world(prim_pos);
        // OBB axes in world (body scale/rot · prim scale/rot · half extents).
        let he = half_extents * prim_scale.abs();
        let ax = bt.rotation * (prim_rot * Vec3::X) * (he.x * bt.scale.x.abs());
        let ay = bt.rotation * (prim_rot * Vec3::Y) * (he.y * bt.scale.y.abs());
        let az = bt.rotation * (prim_rot * Vec3::Z) * (he.z * bt.scale.z.abs());
        // Support extent of the OBB along camera-right = sum |axis · right|.
        let extent = ax.dot(right).abs() + ay.dot(right).abs() + az.dot(right).abs();
        // Clear the silhouette + a small world pad so the control never sits on the mesh.
        let pad = 0.22_f32.max(extent * 0.08);
        center + right * (extent + pad)
    }

    /// Alias used by the uniform-bevel yellow square (same placement rule).
    pub(crate) fn uniform_bevel_handle_world(
        prim_pos: Vec3,
        prim_rot: Quat,
        prim_scale: Vec3,
        half_extents: Vec3,
        bt: &BodyTransform,
        cam_right: Vec3,
    ) -> Vec3 {
        Self::overlay_handle_off_prim(
            prim_pos, prim_rot, prim_scale, half_extents, bt, cam_right,
        )
    }

    pub(crate) fn bevel_drag_start_snapshot(p: &core_math::primitives::SdfPrimitive) -> BevelDragStartSnap {
        let mut box_radii = [0.0f32; 12];
        let (cyl_top, cyl_bot) = match &p.shape {
            SdfShape::Box { bevel, .. } => {
                box_radii = bevel.radii;
                (0.0, 0.0)
            }
            SdfShape::Cylinder { .. } => (p.cyl_bevel.top, p.cyl_bevel.bot),
            _ => (0.0, 0.0),
        };
        BevelDragStartSnap {
            ca_bot: p.chamfer_angle_bot,
            ca_top: p.chamfer_angle_top,
            ca_vert: p.chamfer_angle_vert,
            bp_bot: p.bevel_profile_bot,
            bp_top: p.bevel_profile_top,
            bp_vert: p.bevel_profile_vert,
            box_radii,
            cyl_top,
            cyl_bot,
        }
    }

    /// True if the prim's bevel state still matches drag-start (empty click / zero drag).
    pub(crate) fn bevel_drag_is_noop(
        bd: &BevelDrag,
        prims: &[core_math::primitives::SdfPrimitive],
    ) -> bool {
        if bd.prim_idx >= prims.len() {
            return true;
        }
        let p = &prims[bd.prim_idx];
        // Angles / profiles
        if (p.chamfer_angle_bot - bd.start_chamfer_angle_bot).abs() >= 1e-5
            || (p.chamfer_angle_top - bd.start_chamfer_angle_top).abs() >= 1e-5
            || (p.chamfer_angle_vert - bd.start_chamfer_angle_vert).abs() >= 1e-5
            || (p.bevel_profile_bot - bd.start_bevel_profile_bot).abs() >= 1e-5
            || (p.bevel_profile_top - bd.start_bevel_profile_top).abs() >= 1e-5
            || (p.bevel_profile_vert - bd.start_bevel_profile_vert).abs() >= 1e-5
        {
            return false;
        }
        // Radii (the common drag path — was previously ignored → no undo)
        match &p.shape {
            SdfShape::Box { bevel, .. } => {
                for &ei in &bd.target_edges {
                    let i = ei as usize;
                    if i < 12 && (bevel.radii[i] - bd.start_box_radii[i]).abs() >= 1e-5 {
                        return false;
                    }
                }
                // Uniform also rewrites algo/modes on all edges
                if bd.uniform_all {
                    for i in 0..12 {
                        if (bevel.radii[i] - bd.start_box_radii[i]).abs() >= 1e-5 {
                            return false;
                        }
                    }
                }
            }
            SdfShape::Cylinder { .. } => {
                if (p.cyl_bevel.top - bd.start_cyl_top).abs() >= 1e-5
                    || (p.cyl_bevel.bot - bd.start_cyl_bot).abs() >= 1e-5
                {
                    return false;
                }
            }
            _ => {}
        }
        true
    }

    /// Returns the 12 world-space edge segments of a Box, respecting `up_axis` permutation.
    /// Edge groups: indices 0-3=bottom, 4-7=top, 8-11=vertical.
    /// `up_axis`: 0=Y (default), 1=X, 2=Z — determines which axis is "up" for edge classification.
    pub(crate) fn box_bevel_edges(pos: Vec3, rot: Quat, scale: Vec3, he: Vec3, up_axis: u8, bt: &BodyTransform) -> [(Vec3, Vec3); 12] {
        let c = |x: f32, y: f32, z: f32| -> Vec3 {
            bt.local_to_world(rot * (Vec3::new(x, y, z) * scale) + pos)
        };
        let v = [
            c(-he.x,-he.y,-he.z), c( he.x,-he.y,-he.z),
            c(-he.x,-he.y, he.z), c( he.x,-he.y, he.z),
            c(-he.x, he.y,-he.z), c( he.x, he.y,-he.z),
            c(-he.x, he.y, he.z), c( he.x, he.y, he.z),
        ];
        let idx: [(usize,usize); 12] = match up_axis {
            1 => [ // X up
                (0,4),(2,6),(0,2),(4,6),
                (1,5),(3,7),(1,3),(5,7),
                (0,1),(4,5),(2,3),(6,7),
            ],
            2 => [ // Z up
                (0,1),(4,5),(0,4),(1,5),
                (2,3),(6,7),(2,6),(3,7),
                (0,2),(1,3),(4,6),(5,7),
            ],
            _ => [ // Y up (default)
                (0,1),(2,3),(0,2),(1,3),
                (4,5),(6,7),(4,6),(5,7),
                (0,4),(1,5),(2,6),(3,7),
            ],
        };
        let mut edges = [(Vec3::ZERO, Vec3::ZERO); 12];
        for i in 0..12 { edges[i] = (v[idx[i].0], v[idx[i].1]); }
        edges
    }

    /// Screen-space distance from point `m` to 2D segment (a→b).
    pub(crate) fn pt_to_seg_dist(m: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
        let ab = b - a; let am = m - a;
        let len2 = ab.x*ab.x + ab.y*ab.y;
        if len2 < 0.0001 { return am.length(); }
        let t = ((am.x*ab.x + am.y*ab.y) / len2).clamp(0.0, 1.0);
        let closest = a + egui::vec2(ab.x*t, ab.y*t);
        (m - closest).length()
    }

    /// Returns the hovered edge index (0-11) if mouse is within 8px of any edge segment.
    pub(crate) fn detect_bevel_hover(
        edges: &[(Vec3, Vec3); 12],
        mouse: egui::Pos2, rect: egui::Rect,
        cam_pos: Vec3, cam_orbit: Quat, ortho_hw: f32,
        hover_px: f32, fov_deg: f32,
    ) -> Option<u8> {
        let aspect = rect.width() / rect.height().max(1.0);
        let mut best_dist = hover_px;
        let mut best_edge: Option<u8> = None;
        for (i, (p0, p1)) in edges.iter().enumerate() {
            let s0 = world_to_screen(*p0, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
            let s1 = world_to_screen(*p1, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
            if let (Some(a), Some(b)) = (s0, s1) {
                let d = Self::pt_to_seg_dist(mouse, a, b);
                if d < best_dist { best_dist = d; best_edge = Some(i as u8); }
            }
        }
        best_edge
    }

    /// Generate cylinder edge ring segments (8 line segments per ring, 2 rings = indices 0=top, 1=bot).
    /// Returns 16 segments: [0..8) = top ring, [8..16) = bottom ring.
    pub(crate) fn cylinder_bevel_segments(pos: Vec3, rot: Quat, scale: Vec3, radius: f32, half_h: f32, bt: &BodyTransform) -> Vec<(Vec3, Vec3)> {
        let n = 8usize;
        let mut segs = Vec::with_capacity(n * 2);
        for ring_y in [half_h, -half_h] {
            for i in 0..n {
                let a0 = (i as f32) / (n as f32) * std::f32::consts::TAU;
                let a1 = ((i + 1) as f32) / (n as f32) * std::f32::consts::TAU;
                let p0 = bt.local_to_world(rot * (Vec3::new(a0.cos() * radius, ring_y, a0.sin() * radius) * scale) + pos);
                let p1 = bt.local_to_world(rot * (Vec3::new(a1.cos() * radius, ring_y, a1.sin() * radius) * scale) + pos);
                segs.push((p0, p1));
            }
        }
        segs
    }

    /// Detect cylinder bevel edge hover. Returns Some(0) for top ring, Some(1) for bottom ring.
    pub(crate) fn detect_cyl_bevel_hover(
        segs: &[(Vec3, Vec3)],
        mouse: egui::Pos2, rect: egui::Rect,
        cam_pos: Vec3, cam_orbit: Quat, ortho_hw: f32,
        hover_px: f32, fov_deg: f32,
    ) -> Option<u8> {
        let aspect = rect.width() / rect.height().max(1.0);
        let mut best_dist = hover_px;
        let mut best_ring: Option<u8> = None;
        for (i, (p0, p1)) in segs.iter().enumerate() {
            let s0 = world_to_screen(*p0, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
            let s1 = world_to_screen(*p1, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
            if let (Some(a), Some(b)) = (s0, s1) {
                let d = Self::pt_to_seg_dist(mouse, a, b);
                if d < best_dist {
                    best_dist = d;
                    best_ring = Some(if i < 8 { 0 } else { 1 }); // 0=top, 1=bot
                }
            }
        }
        best_ring
    }

    /// Draws box bevel handles in the viewport painter for the selected Box primitive
    /// (edge highlights + yellow uniform-all square). Algorithm is properties-only.
    pub(crate) fn draw_bevel_handles(&mut self, painter: &egui::Painter, rect: egui::Rect, ctx: &egui::Context) {
        self.tool.uniform_bevel_hovered = false;
        let idx = match self.selected_prim_indices_unified().first() { Some(&i) => i, None => return };
        let p = match self.scene.primitives.get(idx) { Some(p) => p, None => return };
        // Body transform: prim positions are body-local, convert to world for overlays.
        let bt = self.scene.prim_body_map.get(idx)
            .and_then(|(nid, _)| self.scene.scene_graph.body_world_transform(*nid))
            .unwrap_or_default();
        // Cylinder bevel handles
        if let SdfShape::Cylinder{radius, half_height} = &p.shape {
            let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
            let aspect = rect.width() / rect.height().max(1.0);
            let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
            let segs = Self::cylinder_bevel_segments(p.position, p.rotation_quat(), p.scale, *radius, *half_height, &bt);
            let ring_colors: [(u8,u8,u8); 2] = [(80, 220, 100), (80, 160, 255)]; // top=green, bot=blue
            let ring_labels = ["Top", "Bottom"];
            for (si, (s0, s1)) in segs.iter().enumerate() {
                let ring = if si < 8 { 0usize } else { 1usize };
                let ei = ring as u8;
                let is_hovered = self.tool.hovered_bevel_edge == Some(ei);
                let is_selected = self.tool.selected_bevel_edges.contains(&ei);
                let (gr, gg, gb) = ring_colors[ring];
                let (color, width) = if is_hovered {
                    (egui::Color32::from_rgb(255, 255, 200), 3.0f32)
                } else if is_selected {
                    (egui::Color32::from_rgb(gr, gg, gb), 2.5f32)
                } else {
                    (egui::Color32::from_rgba_unmultiplied(gr, gg, gb, 90), 1.2f32)
                };
                if let (Some(a), Some(b)) = (
                    world_to_screen(*s0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                    world_to_screen(*s1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                ) {
                    if is_hovered || is_selected {
                        let glow = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 50);
                        painter.line_segment([a, b], egui::Stroke::new(width + 4.0, glow));
                    }
                    painter.line_segment([a, b], egui::Stroke::new(width, color));
                }
            }
            if let Some(ei) = self.tool.hovered_bevel_edge {
                if (ei as usize) < 2 {
                    // Show midpoint handle at first segment of hovered ring
                    let seg_idx = (ei as usize) * 8;
                    let mid = (segs[seg_idx].0 + segs[seg_idx].1) * 0.5;
                    if let Some(sp) = world_to_screen(mid, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        painter.rect(
                            egui::Rect::from_center_size(sp, egui::vec2(8.0, 8.0)), 1.0,
                            egui::Color32::from_rgb(255, 170, 40),
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 210, 90)), egui::StrokeKind::Middle,
                        );
                        let radius_val = if ei == 0 { p.cyl_bevel.top } else { p.cyl_bevel.bot };
                        if radius_val > 0.001 {
                            painter.text(sp + egui::vec2(0.0, -12.0), egui::Align2::CENTER_BOTTOM,
                                format!("r={:.3}", radius_val), egui::FontId::proportional(10.0),
                                egui::Color32::from_rgb(255, 220, 100));
                        }
                        if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                            let tip_color = egui::Color32::from_rgb(ring_colors[ei as usize].0, ring_colors[ei as usize].1, ring_colors[ei as usize].2);
                            painter.text(mp + egui::vec2(12.0, -20.0), egui::Align2::LEFT_CENTER,
                                format!(
                                    "{} edge — ← Fillet · Chamfer →  ·  Shift+click add  ·  Alt+click both rims",
                                    ring_labels[ei as usize]
                                ),
                                egui::FontId::proportional(11.0), tip_color);
                        }
                    }
                }
            }
            return;
        }
        let (he, up_ax) = match &p.shape { SdfShape::Box{half_extents, bevel} => (*half_extents, bevel.up_axis), _ => return };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let edges = Self::box_bevel_edges(p.position, p.rotation_quat(), p.scale, he, up_ax, &bt);
        let group_colors: [(u8,u8,u8); 3] = [(80, 160, 255), (80, 220, 100), (255, 170, 60)];
        for (i, (p0, p1)) in edges.iter().enumerate() {
            let ei = i as u8;
            let is_hovered  = self.tool.hovered_bevel_edge == Some(ei);
            let is_selected = self.tool.selected_bevel_edges.contains(&ei);
            let gi = (i / 4).min(2);
            let (gr, gg, gb) = group_colors[gi];
            let (color, width) = if is_hovered {
                (egui::Color32::from_rgb(255, 255, 200), 3.0f32)
            } else if is_selected {
                (egui::Color32::from_rgb(gr, gg, gb), 2.5f32)
            } else {
                (egui::Color32::from_rgba_unmultiplied(gr, gg, gb, 90), 1.2f32)
            };
            if let (Some(a), Some(b)) = (
                world_to_screen(*p0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
                world_to_screen(*p1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg),
            ) {
                // Glow: wider translucent stroke behind the main line
                if is_hovered || is_selected {
                    let glow = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 50);
                    painter.line_segment([a, b], egui::Stroke::new(width + 4.0, glow));
                }
                painter.line_segment([a, b], egui::Stroke::new(width, color));
            }
        }
        if let Some(ei) = self.tool.hovered_bevel_edge {
            let mid = (edges[ei as usize].0 + edges[ei as usize].1) * 0.5;
            if let Some(sp) = world_to_screen(mid, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                painter.rect(
                    egui::Rect::from_center_size(sp, egui::vec2(8.0, 8.0)),
                    1.0,
                    egui::Color32::from_rgb(255, 170, 40),
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 210, 90)), egui::StrokeKind::Middle,
                );
                // Show radius value at edge midpoint
                let radius_val = if let SdfShape::Box { bevel, .. } = &p.shape {
                    bevel.radii[ei as usize]
                } else { 0.0 };
                if radius_val > 0.001 {
                    painter.text(
                        sp + egui::vec2(0.0, -12.0),
                        egui::Align2::CENTER_BOTTOM,
                        format!("r={:.3}", radius_val),
                        egui::FontId::proportional(10.0),
                        egui::Color32::from_rgb(255, 220, 100),
                    );
                }
                if let Some(mp) = ctx.input(|i| i.pointer.hover_pos()) {
                    let (group_lbl, tip_color) = if ei < 4 {
                        ("Bottom", egui::Color32::from_rgb(130, 190, 255))
                    } else if ei < 8 {
                        ("Top", egui::Color32::from_rgb(130, 240, 150))
                    } else {
                        ("Vertical", egui::Color32::from_rgb(255, 200, 120))
                    };
                    painter.text(
                        mp + egui::vec2(12.0, -20.0),
                        egui::Align2::LEFT_CENTER,
                        format!(
                            "{} edge — ← Fillet · Chamfer →  ·  Shift+click add  ·  Alt+click all {}",
                            group_lbl, group_lbl
                        ),
                        egui::FontId::proportional(11.0),
                        tip_color,
                    );
                }
            }
        }

        // ---- Yellow square: drag = Uniform all-edge bevel, click = reset all ----
        // Always off the prim: center + camera-right past OBB silhouette.
        let (he_full, prim_pos, prim_rot, prim_scale, uniform_r) = {
            let pr = &self.scene.primitives[idx];
            match &pr.shape {
                SdfShape::Box { half_extents, bevel } => {
                    let ur = bevel.radii.iter().copied().fold(0.0f32, f32::max);
                    (
                        *half_extents,
                        pr.position,
                        pr.rotation_quat(),
                        pr.scale,
                        ur,
                    )
                }
                _ => return,
            }
        };
        let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
        let world_sq = Self::uniform_bevel_handle_world(
            prim_pos, prim_rot, prim_scale, he_full, &bt, cam_right,
        );
        let mp = ctx.input(|i| i.pointer.hover_pos());
        self.tool.uniform_bevel_hovered = false;
        if let Some(sq_sp) = world_to_screen(
            world_sq,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        ) {
            let sq = 22.0f32;
            let sq_rect = egui::Rect::from_center_size(sq_sp, egui::vec2(sq, sq));
            let hovered = mp.map_or(false, |m| sq_rect.contains(m));
            let dragging = self.tool.current.as_bevel().map_or(false, |b| b.uniform_all);
            self.tool.uniform_bevel_hovered = hovered || dragging;
            let fill = if dragging {
                egui::Color32::from_rgb(255, 240, 80)
            } else if hovered {
                egui::Color32::from_rgb(255, 230, 60)
            } else {
                egui::Color32::from_rgb(255, 210, 40)
            };
            let stroke_col = if hovered || dragging {
                egui::Color32::from_rgb(255, 255, 200)
            } else {
                egui::Color32::from_rgb(180, 140, 20)
            };
            painter.rect(
                sq_rect,
                3.0,
                fill,
                egui::Stroke::new(if hovered || dragging { 2.0 } else { 1.5 }, stroke_col), egui::StrokeKind::Middle,
            );
            if uniform_r > 0.001 {
                painter.text(
                    sq_sp + egui::vec2(0.0, -sq * 0.5 - 4.0),
                    egui::Align2::CENTER_BOTTOM,
                    format!("r={:.3}", uniform_r),
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_rgb(255, 240, 120),
                );
            }
            if hovered || dragging {
                if let Some(m) = mp {
                    painter.text(
                        m + egui::vec2(14.0, -10.0),
                        egui::Align2::LEFT_CENTER,
                        "Uniform bevel — drag: set all · click: reset",
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgb(255, 240, 140),
                    );
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn approx_eq(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn test_locked_workplane_hit() {
        // Ray straight down onto XZ workplane at origin
        let r = compute_creation_plane(
            Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0),
            true, Vec3::ZERO, Vec3::Y,
            None, 0.0,
        );
        let (hp, hn, on_surf) = r.unwrap();
        assert!(approx_eq(hp, Vec3::ZERO, 0.01));
        assert!(approx_eq(hn, Vec3::Y, 0.01));
        assert!(!on_surf);
    }

    #[test]
    fn test_locked_workplane_parallel_ray_returns_none() {
        // Ray parallel to the workplane — should return None
        let r = compute_creation_plane(
            Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 0.0, 0.0),
            true, Vec3::ZERO, Vec3::Y,
            None, 0.0,
        );
        assert!(r.is_none());
    }

    #[test]
    fn test_scene_hit_takes_priority() {
        // Scene hit should win over floor
        let scene_hit = Some((Vec3::new(1.0, 2.0, 3.0), Vec3::new(0.0, 0.0, 1.0)));
        let r = compute_creation_plane(
            Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0),
            false, Vec3::ZERO, Vec3::Y,
            scene_hit, 0.0,
        );
        let (hp, hn, on_surf) = r.unwrap();
        assert!(approx_eq(hp, Vec3::new(1.0, 2.0, 3.0), 0.01));
        assert!(approx_eq(hn, Vec3::new(0.0, 0.0, 1.0), 0.01));
        assert!(on_surf);
    }

    #[test]
    fn test_floor_hit_when_no_scene() {
        // No scene hit, ray hits floor at y=0
        let r = compute_creation_plane(
            Vec3::new(3.0, 10.0, 0.0), Vec3::new(0.0, -1.0, 0.0),
            false, Vec3::ZERO, Vec3::Y,
            None, 0.0,
        );
        let (hp, hn, on_surf) = r.unwrap();
        assert!(approx_eq(hp, Vec3::new(3.0, 0.0, 0.0), 0.01));
        assert!(approx_eq(hn, Vec3::Y, 0.01));
        assert!(!on_surf);
    }

    #[test]
    fn test_workplane_fallback_when_no_floor() {
        // Ray pointing up — misses floor, falls back to workplane at z=5
        let r = compute_creation_plane(
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0),
            false, Vec3::new(0.0, 0.0, 5.0), Vec3::new(0.0, 0.0, 1.0),
            None, 0.0,
        );
        let (hp, hn, on_surf) = r.unwrap();
        assert!(approx_eq(hp, Vec3::new(0.0, 0.0, 5.0), 0.01));
        assert!(approx_eq(hn, Vec3::new(0.0, 0.0, 1.0), 0.01));
        assert!(!on_surf);
    }

    #[test]
    fn test_no_hit_returns_none() {
        // Ray points away from everything — behind camera
        let r = compute_creation_plane(
            Vec3::new(0.0, -5.0, 0.0), Vec3::new(0.0, -1.0, 0.0),
            false, Vec3::new(0.0, 100.0, 0.0), Vec3::Y,
            None, 0.0,
        );
        // Floor at y=0 is behind the ray (origin at -5, going further down)
        // Workplane at y=100 is also behind
        assert!(r.is_none());
    }
}
