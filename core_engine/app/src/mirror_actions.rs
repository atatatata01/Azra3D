//! Smart mirror (SDF) + Poly Smart Symmetry (Alt+X on mesh) operations.

use core_math::scene_graph::BodyTransform;
use crate::*;

/// When true, prim.position in the flat cache is body-local (relative to body.origin).
/// Mirror plane (world-space) must be converted to body-local before comparing with prim positions.
use crate::BODY_LOCAL_PRIMS;

/// One pickable source for a **Volume Mirror**'s center plane, already converted into the
/// body-local frame `mirror_center` lives in (Prim Position Space rule — the mirror center is
/// body-local, every world source must go through `BodyTransform::world_to_local`).
pub(crate) struct MirrorCenterSource {
    pub label: String,
    /// Plane point in the target body's local frame.
    pub center: glam::Vec3,
    /// Which mirror axis the source's plane normal corresponds to (bit 0=X, 1=Y, 2=Z), when the
    /// normal IS a body-local axis. `None` = the source carries no usable normal.
    pub axis_bit: Option<u8>,
    /// The source HAD a normal but it is not axis-aligned. The volume mirror reflects per axis,
    /// so a tilted plane cannot be its mirror plane: the center is still usable, the orientation
    /// is not. Reported to the user instead of being silently rounded to the nearest axis.
    pub tilted: bool,
}

/// A plane normal counts as axis-aligned within ~5°.
const MIRROR_AXIS_SNAP_COS: f32 = 0.9962;

impl AzraApp {
    /// Center-plane sources offered by the Volume Mirror card: the live work plane, every saved
    /// work plane (they already round-trip through the project file — `ProjectFile.editor.wp_saved`),
    /// the current selection (`selection_pivot_world`, so a custom pivot is honored), and the two
    /// origins. Read-only snapshot: the properties panel builds it BEFORE the egui closure that
    /// mutates the modifier stack, so nothing borrows `self` twice.
    pub(crate) fn mirror_center_sources(
        &self,
        nid: core_math::scene_graph::NodeId,
    ) -> Vec<MirrorCenterSource> {
        use glam::Vec3;
        let bt = self.scene.scene_graph.body_world_transform(nid);
        let to_local = |w: Vec3| match bt {
            Some(b) => b.world_to_local(w),
            None => w,
        };
        // Direction through the same transform (it may rotate/scale): difference of two mapped
        // points, so no separate normal-matrix path can drift from `world_to_local`.
        let local_zero = to_local(Vec3::ZERO);
        let dir_to_local = |n: Vec3| (to_local(n) - local_zero).normalize_or_zero();

        let axis_of = |world_normal: Vec3| -> (Option<u8>, bool) {
            let n = dir_to_local(world_normal);
            if n.length_squared() < 0.25 {
                return (None, false);
            }
            let a = n.abs();
            let (axis, m) = if a.x >= a.y && a.x >= a.z {
                (0u8, a.x)
            } else if a.y >= a.z {
                (1, a.y)
            } else {
                (2, a.z)
            };
            if m >= MIRROR_AXIS_SNAP_COS {
                (Some(1 << axis), false)
            } else {
                (None, true)
            }
        };

        let mut out = Vec::new();

        let (wp_axis, wp_tilted) = axis_of(self.viewport.wp_resolved.normal);
        out.push(MirrorCenterSource {
            label: "Work Plane (current)".into(),
            center: to_local(self.viewport.wp_resolved.origin),
            axis_bit: wp_axis,
            tilted: wp_tilted,
        });

        for saved in &self.viewport.wp_saved {
            let (axis_bit, tilted) = axis_of(saved.normal);
            out.push(MirrorCenterSource {
                label: format!("Plane: {}", saved.name),
                center: to_local(saved.origin),
                axis_bit,
                tilted,
            });
        }

        if let Some(w) = self.selection_pivot_world() {
            out.push(MirrorCenterSource {
                label: "Selection (object / prim)".into(),
                center: to_local(w),
                axis_bit: None,
                tilted: false,
            });
        }

        out.push(MirrorCenterSource {
            label: "Body Origin".into(),
            center: Vec3::ZERO,
            axis_bit: None,
            tilted: false,
        });
        out.push(MirrorCenterSource {
            label: "World Origin".into(),
            center: local_zero,
            axis_bit: None,
            tilted: false,
        });
        out
    }

    // ---- Poly Smart Symmetry (Alt+X) — NOT Smart Mirror ----
    // Hold-tool: place/adjust plane via the amber SymmetryPlane visual, then symmetrize the mesh.

    /// Start poly Smart Symmetry hold-tool for the active PolygonalBody.
    pub(crate) fn start_poly_symmetry_tool(&mut self, mouse: egui::Pos2) {
        let Some(nid) = self.active_poly_nid() else {
            self.toast("Select a polygon mesh first");
            return;
        };
        let rect = self.viewport.viewport_rect;
        let world = self.scene.scene_graph.world_transform(nid);

        let from_sel = self.poly_sym_plane_from_selection(nid);
        let (p_origin, p_normal, surf_n, phase) = if let Some((o_loc, n_loc)) = from_sel {
            let ow = world.transform_point3(o_loc);
            let nw = world.transform_vector3(n_loc).normalize_or_zero();
            if nw.length_squared() > 0.25 {
                (ow, nw, nw, PolySymmetryPhase::Adjusting)
            } else {
                let (o, n, s, _, ph) = self.poly_symmetry_seed_from_mouse(mouse, rect);
                (o, n, s, ph)
            }
        } else {
            let (o, n, s, _, ph) = self.poly_symmetry_seed_from_mouse(mouse, rect);
            (o, n, s, ph)
        };

        let (tu, tv) = crate::mirror_tangent_frame(p_normal, surf_n);
        let abs_n = p_normal.abs();
        let snapped = if abs_n.x >= abs_n.y && abs_n.x >= abs_n.z {
            0u8
        } else if abs_n.y >= abs_n.z {
            1
        } else {
            2
        };

        self.start_operation(ActiveTool::PolySymmetry(PolySymmetryState {
            phase,
            nid,
            plane_origin: p_origin,
            plane_normal: p_normal,
            base_normal: p_normal,
            surface_normal: surf_n,
            tangent_u: tu,
            tangent_v: tv,
            snapped_axis: snapped,
            rotation_angle: 0.0,
            drag_start_mouse: None,
            drag_start_origin: p_origin,
            action: PolySymmetryAction::Symmetrize,
        }));

        self.viewport.symmetry.origin = p_origin;
        self.viewport.symmetry.normal = p_normal;
        self.viewport.symmetry.enabled = true;
        self.viewport.symmetry.visible = true;

        let seed = if matches!(phase, PolySymmetryPhase::Adjusting) {
            "plane from selection"
        } else {
            "LMB=Place"
        };
        self.toast(&format!(
            "Symmetry — {seed}  |  Drag=Move  Scroll=Rotate  X/Y/Z=Axis\n\
             LMB=Confirm (symmetrize)  Shift=Instance  Shift+Alt=Cut  Esc=Cancel"
        ));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn poly_symmetry_seed_from_mouse(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> (glam::Vec3, glam::Vec3, glam::Vec3, bool, PolySymmetryPhase) {
        use glam::Vec3;
        let (origin, dir) = self.cam_ray(mouse, rect);
        if self.viewport.wp_mode == WorkPlaneMode::Locked {
            (
                self.viewport.wp_resolved.origin,
                self.viewport.wp_resolved.normal,
                self.viewport.wp_resolved.normal,
                true,
                PolySymmetryPhase::Placing,
            )
        } else if let Some((hit_pos, hit_normal)) = self.raycast_scene(origin, dir) {
            let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
            let proj = cam_fwd - hit_normal * cam_fwd.dot(hit_normal);
            let pn = if proj.length_squared() > 0.001 {
                proj.normalize()
            } else {
                let cam_right = (self.viewport.camera.orbit * Vec3::X).normalize();
                (cam_right - hit_normal * cam_right.dot(hit_normal)).normalize()
            };
            (hit_pos, pn, hit_normal, false, PolySymmetryPhase::Placing)
        } else {
            (
                self.viewport.wp_resolved.origin,
                self.viewport.wp_resolved.normal,
                Vec3::Y,
                false,
                PolySymmetryPhase::Placing,
            )
        }
    }

    pub(crate) fn confirm_poly_symmetry(&mut self) {
        let state = match self.tool.current.take_poly_symmetry() {
            Some(s) => s,
            None => return,
        };
        let world = self.scene.scene_graph.world_transform(state.nid);
        let inv = world.inverse();
        let origin_local = inv.transform_point3(state.plane_origin);
        let normal_local = world
            .transpose()
            .transform_vector3(state.plane_normal)
            .normalize_or_zero();
        if normal_local.length_squared() < 0.25 {
            self.toast("Symmetry: invalid plane");
            return;
        }
        match state.action {
            PolySymmetryAction::Symmetrize => {
                self.poly_symmetrize_op(state.nid, origin_local, normal_local, false);
            }
            PolySymmetryAction::Instance => {
                self.poly_mirror_instance(state.nid, origin_local, normal_local);
            }
            PolySymmetryAction::Cut => {
                self.poly_symmetrize_op(state.nid, origin_local, normal_local, true);
            }
        }
        self.viewport.symmetry.origin = state.plane_origin;
        self.viewport.symmetry.normal = state.plane_normal.normalize_or_zero();
        self.viewport.symmetry.enabled = true;
        self.viewport.symmetry.visible = true;
    }

    pub(crate) fn cancel_poly_symmetry(&mut self) {
        let _ = self.tool.current.take_poly_symmetry();
        self.toast("Symmetry cancelled");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    // ---- Smart Mirror Tool (SDF prims only — unfinished / separate from poly) ----

    pub(crate) fn confirm_smart_mirror(&mut self) {
        let state = match self.tool.current.take_smart_mirror() {
            Some(s) => s,
            None => return,
        };
        match state.action {
            SmartMirrorAction::Mirror => self.apply_smart_mirror(&state),
            SmartMirrorAction::Instance => self.create_mirror_instances(&state),
            SmartMirrorAction::DeleteHalf => self.delete_mirror_half(&state),
        }
    }

    /// Get target primitive indices for smart mirror: selected prims.
    pub(crate) fn smart_mirror_targets(&self) -> Vec<usize> {
        if self.sel.usel.has_prims() {
            return self.selected_prim_indices_unified();
        }
        Vec::new()
    }

    fn apply_smart_mirror(&mut self, state: &SmartMirrorState) {
        let axis = state.snapped_axis;
        let plane_origin = state.plane_origin;
        let axis_bit = 1u8 << axis;
        let axis_name = match axis {
            0 => "X",
            1 => "Y",
            _ => "Z",
        };

        let sel = self.smart_mirror_targets();
        let mut planned: Vec<(usize, u8, glam::Vec3)> = Vec::new();
        for idx in sel {
            if idx >= self.scene.primitives.len() || self.scene.primitives[idx].locked {
                continue;
            }
            let plane_origin_for_prim = if BODY_LOCAL_PRIMS {
                let bt: Option<BodyTransform> = self
                    .scene
                    .prim_body_map
                    .get(idx)
                    .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid));
                if let Some(bt) = bt {
                    bt.world_to_local(plane_origin)
                } else {
                    plane_origin
                }
            } else {
                plane_origin
            };
            let inv_rot = self.scene.primitives[idx].rotation_quat().inverse();
            let local_plane =
                inv_rot * (plane_origin_for_prim - self.scene.primitives[idx].position);
            let clamp_nonzero = |v: f32| {
                if v.abs() < 1e-7 {
                    if v < 0.0 {
                        -1e-7
                    } else {
                        1e-7
                    }
                } else {
                    v
                }
            };
            let sc = self.scene.primitives[idx].scale;
            let safe_scale =
                glam::Vec3::new(clamp_nonzero(sc.x), clamp_nonzero(sc.y), clamp_nonzero(sc.z));
            let scaled_plane = local_plane / safe_scale;
            let mut mc = self.scene.primitives[idx].mirror_center;
            match axis {
                0 => mc.x = scaled_plane.x,
                1 => mc.y = scaled_plane.y,
                _ => mc.z = scaled_plane.z,
            }
            let flags = self.scene.primitives[idx].mirror_flags | axis_bit;
            let same = flags == self.scene.primitives[idx].mirror_flags
                && (mc - self.scene.primitives[idx].mirror_center).length_squared() < 1e-12;
            if !same {
                planned.push((idx, flags, mc));
            }
        }
        if planned.is_empty() {
            self.toast("Smart Mirror: nothing to apply");
            return;
        }

        let count = planned.len();
        self.transaction("Smart Mirror", |s| {
            for (idx, flags, mc) in planned {
                s.write_prim(idx, |p| {
                    p.mirror_flags = flags;
                    p.mirror_center = mc;
                });
            }
            s.rebake_no_undo();
            s.toast(&format!("Mirror {} applied to {} object(s)", axis_name, count));
        });
    }

    fn create_mirror_instances(&mut self, state: &SmartMirrorState) {
        let plane_origin = state.plane_origin;
        let plane_normal = state.plane_normal;
        let sel: Vec<usize> = self
            .smart_mirror_targets()
            .into_iter()
            .filter(|&idx| idx < self.scene.primitives.len() && !self.scene.primitives[idx].locked)
            .collect();
        if sel.is_empty() {
            self.toast("Mirror Instances: select unlocked object(s) first");
            return;
        }
        self.transaction("Mirror Instances", |s| {
            let mut new_indices = Vec::new();
            for idx in sel {
                let master_uid = s.scene.primitives[idx].prim_uid;
                let uid = s.scene.next_prim_id;
                s.scene.next_prim_id += 1;
                let mut inst = s.scene.primitives[idx].clone();
                inst.prim_uid = uid;
                inst.instance_master_id = Some(master_uid);
                inst.name = format!("{}.mirror", s.scene.primitives[idx].name);

                let bt: Option<BodyTransform> = if BODY_LOCAL_PRIMS {
                    s.scene
                        .prim_body_map
                        .get(idx)
                        .and_then(|&(nid, _)| s.scene.scene_graph.body_world_transform(nid))
                } else {
                    None
                };
                let pos_world = if BODY_LOCAL_PRIMS {
                    if let Some(bt) = bt {
                        bt.local_to_world(inst.position)
                    } else {
                        inst.position
                    }
                } else {
                    inst.position
                };
                let to_prim = pos_world - plane_origin;
                let dist = to_prim.dot(plane_normal);
                let reflected_world = pos_world - plane_normal * (2.0 * dist);
                inst.position = if BODY_LOCAL_PRIMS {
                    if let Some(bt) = bt {
                        bt.world_to_local(reflected_world)
                    } else {
                        reflected_world
                    }
                } else {
                    reflected_world
                };
                // Reflect rotation: reverse the component along plane normal
                let q = inst.rotation_quat();
                let n = plane_normal;
                // Simple axis flip of scale sign for mirror (keeps instancing simple)
                match state.snapped_axis {
                    0 => inst.scale.x = -inst.scale.x,
                    1 => inst.scale.y = -inst.scale.y,
                    _ => inst.scale.z = -inst.scale.z,
                }
                let _ = q;
                let _ = n;
                let new_idx = s.scene.primitives.len();
                s.scene.primitives.push(inst);
                new_indices.push(new_idx);
            }
            s.rebake_no_undo();
            s.toast(&format!("Created {} mirror instance(s)", new_indices.len()));
        });
    }

    fn delete_mirror_half(&mut self, state: &SmartMirrorState) {
        let indices = state.half_delete_indices.clone();
        if indices.is_empty() {
            self.toast("Delete Half: nothing on this side");
            return;
        }
        self.transaction("Delete Mirror Half", |s| {
            let mut sorted = indices;
            sorted.sort_unstable();
            sorted.dedup();
            for idx in sorted.into_iter().rev() {
                if idx < s.scene.primitives.len() {
                    s.scene.primitives.remove(idx);
                }
            }
            s.rebake_no_undo();
            s.toast("Deleted half");
        });
    }
}
