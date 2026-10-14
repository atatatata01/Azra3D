// core_engine/app/src/bool_cut.rs
// BoolCut tool + box bevel system — extracted from main.rs
use eframe::egui;
use glam::Vec3;
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BoxBevel, BevelMode};
use crate::{AzraApp, BoolCutState, BoolCutPhase, BoolCutOp, WorkPlaneMode};

#[allow(dead_code)]
impl AzraApp {
    /// Start a BoolCut: raycast scene, create a thin box aligned to hit surface.
    /// Same placement priority as create tool: surface → floor → workplane.
    pub(crate) fn start_bool_cut(&mut self, mouse: egui::Pos2, rect: egui::Rect) -> Option<BoolCutState> {
        let (origin, dir) = self.cam_ray(mouse, rect);

        // Placement priority (mirrors create_tool.rs):
        // 1. Locked mode → workplane always
        // 2. Auto mode + surface hit → surface
        // 3. Auto mode + ray hits floor → floor
        // 4. Auto mode + above horizon → workplane
        let (hit_point, hit_normal, on_surface) = if self.viewport.wp_mode == WorkPlaneMode::Locked {
            let wp = self.viewport.wp_resolved;
            let denom = dir.dot(wp.normal);
            if denom.abs() < 1e-8 { return None; }
            let t = (wp.origin - origin).dot(wp.normal) / denom;
            if t < 0.001 || t > 500.0 { return None; }
            (origin + dir * t, wp.normal, false)
        } else {
            match self.raycast_scene(origin, dir) {
                Some((p, n)) => (p, n, true),
                None => {
                    let floor_y = self.viewport.grid_floor_y;
                    let floor_t = if dir.y.abs() > 1e-6 {
                        (floor_y - origin.y) / dir.y
                    } else { -1.0 };

                    if floor_t > 0.001 && floor_t < 500.0 {
                        (origin + dir * floor_t, Vec3::Y, false)
                    } else {
                        let wp = self.viewport.wp_resolved;
                        let denom = dir.dot(wp.normal);
                        if denom.abs() < 1e-8 { return None; }
                        let t = (wp.origin - origin).dot(wp.normal) / denom;
                        if t < 0.001 || t > 500.0 { return None; }
                        (origin + dir * t, wp.normal, false)
                    }
                }
            }
        };

        // Tangent frame: surface = camera-relative, floor = world-aligned, workplane = pre-computed.
        let (tu, tv) = if on_surface {
            Self::surface_tangent_frame(hit_normal, self.viewport.camera.orbit)
        } else if hit_normal.y.abs() > self.tweak.floor_detect_threshold {
            (Vec3::X, Vec3::Z)
        } else {
            (self.viewport.wp_resolved.tangent_u, self.viewport.wp_resolved.tangent_v)
        };
        let rot_deg = Self::frame_to_rotation_deg(tu, hit_normal, tv);

        // Create a thin box primitive at hit point
        let uid = self.scene.next_prim_id;
        let name = self.next_name("BoolCut");
        let mut prim = SdfPrimitive::with_uid(name, SdfShape::Box {
            half_extents: Vec3::new(0.001, 0.001, 0.001),
            bevel: BoxBevel::default(),
        }, hit_point, uid);
        prim.operation = BooleanOp::Subtract { k: 0.0 };
        prim.rotation_deg = rot_deg;
        // Inherit the body's prevailing material: the CUT SURFACE renders with the cutter's
        // material, so a default-material cutter would paint gray gouges into a textured body.
        prim.material_id = self.inherited_material_for_create();

        let mut prim_idx = 0;
        self.transaction("BoolCut", |s| {
            prim_idx = s.add_prim_to_graph(prim);
            s.select_single(prim_idx);
            s.rebake_no_undo();
        });

        Some(BoolCutState {
            phase: BoolCutPhase::Drawing,
            op: BoolCutOp::Cut,
            hit_point, hit_normal,
            tangent_u: tu, tangent_v: tv,
            anchor: hit_point,
            local_u: 0.0, local_v: 0.0,
            depth: 0.0, depth_start_mouse_y: 0.0,
            corner_radius: 0.0, corner_chamfer: false,
            blend_k: 0.0,
            prim_idx,
            num_buf: String::new(),
        })
    }

    /// Update the BoolCut rectangle during draw phase.
    pub(crate) fn update_bool_cut_draw(&mut self, mouse: egui::Pos2, rect: egui::Rect, shift: bool, alt: bool) {
        let bc = match &self.tool.bool_cut { Some(bc) => bc, None => return };
        let (hit_point, hit_normal, tu, tv, anchor, prim_idx) =
            (bc.hit_point, bc.hit_normal, bc.tangent_u, bc.tangent_v, bc.anchor, bc.prim_idx);

        // Ray-plane intersection to get current surface point
        let surface_pt = match self.ray_plane_hit(mouse, rect, hit_point, hit_normal) {
            Some(p) => p, None => return,
        };

        let mut lu = (surface_pt - anchor).dot(tu);
        let mut lv = (surface_pt - anchor).dot(tv);

        // Shift = square constraint
        if shift {
            let m = lu.abs().max(lv.abs());
            lu = m * lu.signum();
            lv = m * lv.signum();
        }

        // Compute center and half_extents
        let (center, he_x, he_z) = if alt {
            // Alt = draw from center: anchor is center, extents = |lu|, |lv|
            (anchor, lu.abs().max(0.001), lv.abs().max(0.001))
        } else {
            // Corner-to-corner: center = midpoint
            let cx = anchor + tu * (lu * 0.5) + tv * (lv * 0.5);
            (cx, (lu.abs() * 0.5).max(0.001), (lv.abs() * 0.5).max(0.001))
        };

        // Offset the center slightly above surface so the thin slab is visible
        let pos = center + hit_normal * 0.001;

        if prim_idx < self.scene.primitives.len() {
            let he = Vec3::new(he_x, 0.01, he_z);
            self.write_prim(prim_idx, |p| {
                p.position = pos;
                if let SdfShape::Box { ref mut half_extents, .. } = p.shape { *half_extents = he; }
            });
            // Store back
            if let Some(ref mut bc) = self.tool.bool_cut {
                bc.local_u = lu;
                bc.local_v = lv;
            }
            self.rebake_no_undo();
        }
    }

    /// Update the BoolCut depth during depth mode (mouse Y movement).
    pub(crate) fn update_bool_cut_depth(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        let bc = match &self.tool.bool_cut { Some(bc) => bc, None => return };
        let (hit_normal, prim_idx, start_y) =
            (bc.hit_normal, bc.prim_idx, bc.depth_start_mouse_y);
        let (lu, lv) = (bc.local_u, bc.local_v);
        let _corner_radius = bc.corner_radius;
        let _is_chamfer = bc.corner_chamfer;
        let blend_k = bc.blend_k;

        // Convert mouse Y delta to depth (screen px → world units)
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        if prim_idx >= self.scene.primitives.len() { return; }
        let prim_center = self.scene.primitives[prim_idx].position;
        let view_depth = (prim_center - cam_pos).dot(cam_fwd).max(0.1);
        let px_to_world = view_depth / (rect.height() * 0.5);
        let mouse_dy = start_y - mouse.y; // up = positive depth (into surface)
        let depth = mouse_dy * px_to_world;

        // Determine half_extents
        let bc_ref = self.tool.bool_cut.as_ref().unwrap();
        let (he_x, he_z) = if bc_ref.anchor == bc_ref.hit_point {
            // center draw mode: extents = |lu|, |lv|
            (lu.abs().max(0.001), lv.abs().max(0.001))
        } else {
            ((lu.abs() * 0.5).max(0.001), (lv.abs() * 0.5).max(0.001))
        };
        let he_y = depth.abs().max(0.001) * 0.5;

        // Position: offset center by half-depth along normal
        let (draw_center, _draw_alt) = if bc_ref.anchor == bc_ref.hit_point {
            (bc_ref.anchor, true)
        } else {
            (bc_ref.anchor + bc_ref.tangent_u * (lu * 0.5) + bc_ref.tangent_v * (lv * 0.5), false)
        };
        let pos = draw_center + hit_normal * (depth * 0.5);

        // Auto-switch operation based on depth direction
        let op = if depth < 0.0 { BoolCutOp::Add } else { BoolCutOp::Cut };
        let new_op = match op {
            BoolCutOp::Cut => BooleanOp::Subtract { k: blend_k },
            BoolCutOp::Add => BooleanOp::Union { k: blend_k },
            BoolCutOp::Intersect => BooleanOp::Intersect,
        };
        let he = Vec3::new(he_x, he_y, he_z);
        self.write_prim(prim_idx, |p| {
            p.position = pos;
            if let SdfShape::Box { ref mut half_extents, .. } = p.shape { *half_extents = he; }
            p.operation = new_op;
        });

        if let Some(ref mut bc) = self.tool.bool_cut {
            bc.depth = depth;
            bc.op = op;
        }
        self.rebake_no_undo();
    }

    /// Commit the BoolCut. If `infinite`, set huge depth for through-cut.
    pub(crate) fn commit_bool_cut(&mut self, infinite: bool) {
        if let Some(ref bc) = self.tool.bool_cut {
            let prim_idx = bc.prim_idx;
            let _hit_normal = bc.hit_normal;
            let (lu, lv, anchor, hit_point) = (bc.local_u, bc.local_v, bc.anchor, bc.hit_point);
            let blend_k = bc.blend_k;
            let _op = bc.op;
            let tu = bc.tangent_u;
            let tv = bc.tangent_v;

            if prim_idx < self.scene.primitives.len() {
                if infinite {
                    // Set huge depth for infinite through-cut
                    let (he_x, he_z) = if anchor == hit_point {
                        (lu.abs().max(0.01), lv.abs().max(0.01))
                    } else {
                        ((lu.abs() * 0.5).max(0.01), (lv.abs() * 0.5).max(0.01))
                    };
                    let cut_d = self.scene_bound_radius().max(self.tweak.through_cut_depth);
                    let he = Vec3::new(he_x, cut_d, he_z);
                    // Center the box at surface, it extends 10 units both ways
                    let draw_center = if anchor == hit_point {
                        anchor
                    } else {
                        anchor + tu * (lu * 0.5) + tv * (lv * 0.5)
                    };
                    self.write_prim(prim_idx, |p| {
                        if let SdfShape::Box { ref mut half_extents, .. } = p.shape { *half_extents = he; }
                        p.position = draw_center;
                        p.operation = BooleanOp::Subtract { k: blend_k };
                    });
                }
                self.rebake_no_undo();
            }
        }
        let op_label = match self.tool.bool_cut.as_ref().map(|bc| bc.op) {
            Some(BoolCutOp::Cut) => "cut",
            Some(BoolCutOp::Add) => "add",
            Some(BoolCutOp::Intersect) => "intersect",
            None => "cut",
        };
        self.toast(&format!("Boolean {} confirmed", op_label));
        self.tool.bool_cut = None;
        // Keep bool_cut_armed = true so user can immediately draw another
    }

    /// Cancel the active BoolCut, removing the cutter primitive.
    pub(crate) fn cancel_bool_cut(&mut self) {
        if let Some(ref bc) = self.tool.bool_cut {
            let idx = bc.prim_idx;
            if idx < self.scene.primitives.len() {
                self.remove_prim_from_graph(idx);
                self.pop_last_undo();
                self.deselect_all();
                self.rebake_no_undo();
            }
        }
        self.tool.bool_cut = None;
        self.toast("BoolCut cancelled");
    }

    /// Update bevel on the BoolCut primitive (all 12 edges uniform).
    pub(crate) fn update_bool_cut_bevel(&mut self) {
        let bc = match &self.tool.bool_cut { Some(bc) => bc, None => return };
        let (prim_idx, r, chamfer) = (bc.prim_idx, bc.corner_radius, bc.corner_chamfer);
        if prim_idx >= self.scene.primitives.len() { return; }
        self.write_prim(prim_idx, |p| {
            if let SdfShape::Box { ref mut bevel, .. } = p.shape {
                let mode = if r < 0.001 { BevelMode::None } else if chamfer { BevelMode::Chamfer } else { BevelMode::Round };
                for i in 0..12 { bevel.radii[i] = r; bevel.modes[i] = mode; }
            }
        });
        self.rebake_no_undo();
    }
}
