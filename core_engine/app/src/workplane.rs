// core_engine/app/src/workplane.rs
// Work Plane system — persistent construction plane that constrains all tools.
use eframe::egui;
use glam::Vec3;
use crate::{AzraApp, WorkPlaneMode, WorkPlaneOrientation, ResolvedWorkPlane, SavedWorkPlane};

impl AzraApp {
    /// Call once per frame (before tools) to update wp_resolved from mode + camera.
    pub(crate) fn resolve_workplane(&mut self) {
        match self.viewport.wp_mode {
            WorkPlaneMode::Auto => self.resolve_workplane_auto(),
            WorkPlaneMode::Locked => self.resolve_workplane_locked(),
        }
    }

    fn resolve_workplane_auto(&mut self) {
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize();
        let origin = self.viewport.wp_settings.origin;

        if self.viewport.wp_settings.orientation == WorkPlaneOrientation::CameraFacing {
            // True view-aligned plane: updates every frame with orbit (MMB) and angle.
            // Normal faces the camera; U/V = camera right / up so grid follows the view.
            let normal = if cam_fwd.length_squared() > 1e-8 {
                -cam_fwd.normalize()
            } else {
                Vec3::NEG_Z
            };
            let mut tu = (self.viewport.camera.orbit * Vec3::X).normalize_or_zero();
            let mut tv = (self.viewport.camera.orbit * Vec3::Y).normalize_or_zero();
            if tu.length_squared() < 1e-6 || tv.length_squared() < 1e-6 {
                let (a, b) = Self::tangent_frame_from_normal(normal);
                tu = a;
                tv = b;
            } else {
                // Re-orthonormalize against normal (right-handed: tv = tu × n is wrong;
                // we want n = tu × tv for standard WP, with normal facing camera).
                tu = (tu - normal * tu.dot(normal)).normalize_or_zero();
                tv = normal.cross(tu).normalize_or_zero();
                if tu.length_squared() < 1e-6 || tv.length_squared() < 1e-6 {
                    let (a, b) = Self::tangent_frame_from_normal(normal);
                    tu = a;
                    tv = b;
                }
            }
            self.viewport.wp_resolved = ResolvedWorkPlane {
                origin,
                normal,
                tangent_u: tu,
                tangent_v: tv,
            };
            return;
        }

        // AxisSnap mode: snap to nearest world axis plane based on camera angle.
        // All frames are right-handed: tv = tu.cross(normal).
        let looking_down = cam_fwd.y.abs() > 0.5;
        if looking_down {
            // Horizontal XZ plane
            let normal = Vec3::Y;
            let tu = Vec3::X;
            self.viewport.wp_resolved = ResolvedWorkPlane {
                origin, normal, tangent_u: tu, tangent_v: tu.cross(normal),
            };
        } else {
            if cam_fwd.z.abs() > cam_fwd.x.abs() {
                // Looking along Z → XY plane
                let normal = if cam_fwd.z > 0.0 { Vec3::NEG_Z } else { Vec3::Z };
                let tu = Vec3::X;
                self.viewport.wp_resolved = ResolvedWorkPlane {
                    origin, normal, tangent_u: tu, tangent_v: tu.cross(normal),
                };
            } else {
                // Looking along X → YZ plane
                let normal = if cam_fwd.x > 0.0 { Vec3::NEG_X } else { Vec3::X };
                let tu = Vec3::Z;
                self.viewport.wp_resolved = ResolvedWorkPlane {
                    origin, normal, tangent_u: tu, tangent_v: tu.cross(normal),
                };
            }
        }
    }

    fn resolve_workplane_locked(&mut self) {
        let normal = self.viewport.wp_locked_normal;
        let origin = self.viewport.wp_locked_origin;
        let (tu, tv) = Self::tangent_frame_from_normal(normal);
        self.viewport.wp_resolved = ResolvedWorkPlane { origin, normal, tangent_u: tu, tangent_v: tv };
    }

    /// Compute a stable tangent frame from a normal vector.
    pub(crate) fn tangent_frame_from_normal(normal: Vec3) -> (Vec3, Vec3) {
        // For horizontal surfaces use world-aligned axes
        if normal.y.abs() > 0.7 {
            let tu = Vec3::X;
            let tv = tu.cross(normal).normalize();
            return (tu, tv);
        }
        // For vertical/angled: pick up axis, project onto plane
        let up = Vec3::Y;
        let mut tu = up.cross(normal);
        if tu.length_squared() < 0.001 {
            tu = Vec3::X.cross(normal);
        }
        tu = tu.normalize();
        let tv = tu.cross(normal).normalize();
        (tu, tv)
    }

    /// Place workplane on the surface under the mouse (Home / Shift+RMB when not AC-place).
    /// Switches to Locked mode. Prefers **edge mid / face center / vertex** when the cursor
    /// is near them (same pick as Action Center Automatic), then smart-snap when magnet/Ctrl,
    /// then raw surface. Without that, "Snap · Edge Midpoints" never affected WP placement
    /// (owner: Shift+click set workplane with edge center snap "not working").
    pub(crate) fn place_workplane_on_surface(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        self.place_workplane_from_pick(mouse, rect, false);
    }

    /// Ctrl+Shift+RMB: same snap-aware place (edge mid / face center / surface).
    /// (Historical name said "body center"; behaviour is snap-aware surface point.)
    pub(crate) fn place_workplane_at_surface_center(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        self.place_workplane_from_pick(mouse, rect, true);
    }

    /// Shared snap-aware workplane lock.
    /// `prefer_smart_first` — when true (Ctrl held), try smart snap before poly element pick.
    fn place_workplane_from_pick(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        prefer_smart_first: bool,
    ) {
        // 1) Poly topology pick — always available (like AC Automatic). Edge midpoints work
        // even when the Snap magnet is off so "Edge Midpoints" targeting is not silent.
        let poly_el = self.pick_poly_element_near_mouse(mouse, rect);

        // 2) Smart snap when magnet / Enable Snap / Ctrl.
        let smart = if self.is_snap_active(prefer_smart_first) {
            self.find_smart_snap(mouse, rect, None)
        } else {
            None
        };

        let (hit_point, hit_normal, label): (Vec3, Vec3, String) = if prefer_smart_first {
            if let Some(hit) = smart {
                // Prefer smart world pos; normal from nearest poly element or surface.
                let n = poly_el
                    .as_ref()
                    .map(|(_, n, _)| *n)
                    .or_else(|| {
                        let (o, d) = self.cam_ray(mouse, rect);
                        self.raycast_scene_surface(o, d).map(|(_, n)| n)
                    })
                    .unwrap_or(Vec3::Y);
                (hit.world_pos, n, hit.label)
            } else if let Some((p, n, lab)) = poly_el {
                (p, n, lab.to_string())
            } else {
                match self.raycast_workplane_surface_fallback(mouse, rect) {
                    Some(t) => t,
                    None => {
                        self.reset_workplane();
                        return;
                    }
                }
            }
        } else if let Some((p, n, lab)) = poly_el {
            (p, n, lab.to_string())
        } else if let Some(hit) = smart {
            let n = {
                let (o, d) = self.cam_ray(mouse, rect);
                self.raycast_scene_surface(o, d)
                    .map(|(_, n)| n)
                    .unwrap_or(Vec3::Y)
            };
            (hit.world_pos, n, hit.label)
        } else {
            match self.raycast_workplane_surface_fallback(mouse, rect) {
                Some(t) => t,
                None => {
                    self.reset_workplane();
                    return;
                }
            }
        };

        let n = if hit_normal.length_squared() > 1e-8 {
            hit_normal.normalize()
        } else {
            Vec3::Y
        };
        self.viewport.wp_locked_origin = hit_point;
        self.viewport.wp_locked_normal = n;
        self.viewport.wp_mode = WorkPlaneMode::Locked;
        // Keep construction origin in sync for tools that read settings.origin.
        self.viewport.wp_settings.origin = hit_point;
        self.resolve_workplane();
        self.toast(&format!("Work plane locked → {label}"));
    }

    /// Raw surface hit (and optional nearby poly snap on the ray hit).
    fn raycast_workplane_surface_fallback(
        &self,
        mouse: egui::Pos2,
        rect: egui::Rect,
    ) -> Option<(Vec3, Vec3, String)> {
        let (origin, dir) = self.cam_ray(mouse, rect);
        let (hit, n) = self.raycast_scene_surface(origin, dir)?;
        // If the surface hit is near a face/edge center in screen space, pull to it.
        if let Some((sp, sn, lab)) = self.snap_world_point_to_nearby_poly_element(hit, mouse, rect)
        {
            return Some((sp, sn, lab.to_string()));
        }
        Some((hit, n, "surface".into()))
    }

    /// Reset to default auto mode.
    ///
    /// The ONE place `wp_mode` returns to `Auto`, so it is also where the transform center stops
    /// following the workplane. Without this, creating on a workplane and then disabling it would
    /// leave the gizmo pinned to a frame the user can no longer see (owner 2026-08-04).
    /// Only `Workplane` is reverted — a center the user chose explicitly is left alone.
    pub(crate) fn reset_workplane(&mut self) {
        self.viewport.wp_mode = WorkPlaneMode::Auto;
        self.viewport.wp_settings.origin = Vec3::ZERO;
        if self.tool.action_center == crate::types::ActionCenter::Workplane {
            self.tool.action_center = crate::types::ActionCenter::Automatic;
        }
        self.resolve_workplane();
        self.toast("Work plane reset");
    }

    /// Save current resolved workplane with a name.
    pub(crate) fn save_workplane(&mut self, name: String) {
        self.viewport.wp_saved.push(SavedWorkPlane {
            name,
            origin: self.viewport.wp_resolved.origin,
            normal: self.viewport.wp_resolved.normal,
        });
    }

    /// Load a saved workplane by index (sets Locked mode).
    pub(crate) fn load_workplane(&mut self, idx: usize) {
        if let Some(saved) = self.viewport.wp_saved.get(idx).cloned() {
            self.viewport.wp_locked_origin = saved.origin;
            self.viewport.wp_locked_normal = saved.normal;
            self.viewport.wp_mode = WorkPlaneMode::Locked;
            self.resolve_workplane();
            self.toast(&format!("Loaded work plane: {}", saved.name));
        }
    }

    /// Ray-workplane intersection helper.
    pub(crate) fn ray_workplane_hit(&self, mouse: egui::Pos2, rect: egui::Rect) -> Option<Vec3> {
        self.ray_plane_hit(mouse, rect, self.viewport.wp_resolved.origin, self.viewport.wp_resolved.normal)
    }
}
