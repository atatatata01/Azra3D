// types/mod.rs — Re-exports all type modules. Import via `use crate::*`.
//
// Most types live in the `app_types` crate for faster incremental compilation.
// Only types_ai stays here (depends on agent/claude/ai_texture modules in app).

// Re-export everything from app_types
pub(crate) use app_types::*;

// These stay in app (depend on agent, claude, ai_texture, pie_menu, shader_nodes)
mod types_ai;                  pub(crate) use types_ai::*;
mod types_ui;                  pub(crate) use types_ui::*;

// ============================================================
// QA Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec3, Quat};
    use eframe::egui;
    use crate::keybindings::*;

    // ---- snap_to_grid ----

    #[test]
    fn snap_to_grid_basic() {
        let result = snap_to_grid(Vec3::new(0.37, 1.62, -0.18), 0.25);
        assert!((result.x - 0.25).abs() < 0.001, "x={}", result.x);
        assert!((result.y - 1.5).abs() < 0.001, "y={}", result.y);
        assert!((result.z - (-0.25)).abs() < 0.001, "z={}", result.z);
    }

    #[test]
    fn snap_to_grid_exact() {
        let result = snap_to_grid(Vec3::new(1.0, 2.0, 3.0), 0.5);
        assert!((result.x - 1.0).abs() < 0.001);
        assert!((result.y - 2.0).abs() < 0.001);
        assert!((result.z - 3.0).abs() < 0.001);
    }

    // ---- world_to_screen ----

    #[test]
    fn world_origin_projects_to_screen_center() {
        let cam_pos = Vec3::new(0.0, 0.0, 5.0);
        let cam_orbit = Quat::IDENTITY;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let aspect = 1.0;
        let ortho_hw = 0.0;
        let result = world_to_screen(Vec3::ZERO, cam_pos, cam_orbit, rect, aspect, ortho_hw, 90.0);
        assert!(result.is_some());
        let p = result.unwrap();
        assert!((p.x - 50.0).abs() < 1.0, "x={}", p.x);
        assert!((p.y - 50.0).abs() < 1.0, "y={}", p.y);
    }

    #[test]
    fn point_behind_camera_returns_none() {
        let cam_pos = Vec3::new(0.0, 0.0, 5.0);
        let cam_orbit = Quat::IDENTITY;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let result = world_to_screen(Vec3::new(0.0, 0.0, 10.0), cam_pos, cam_orbit, rect, 1.0, 0.0, 90.0);
        assert!(result.is_none(), "point behind camera should return None");
    }

    #[test]
    fn ortho_same_xy_different_depth() {
        let cam_pos = Vec3::new(0.0, 0.0, 5.0);
        let cam_orbit = Quat::IDENTITY;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let ortho_hw = 5.0;
        let p1 = world_to_screen(Vec3::new(1.0, 1.0, 0.0), cam_pos, cam_orbit, rect, 1.0, ortho_hw, 90.0).unwrap();
        let p2 = world_to_screen(Vec3::new(1.0, 1.0, -3.0), cam_pos, cam_orbit, rect, 1.0, ortho_hw, 90.0).unwrap();
        assert!((p1.x - p2.x).abs() < 0.01, "ortho should project same XY to same screen X");
        assert!((p1.y - p2.y).abs() < 0.01, "ortho should project same XY to same screen Y");
    }

    /// Regression guard for the FOV pick-offset bug: `world_to_screen` MUST reproduce the GPU camera
    /// (`Mat4::perspective_rh(fov, aspect)`) at a non-90° FOV. Before the fix it hardcoded 90° (tan=1),
    /// so a point off-center was projected toward the center — the mouse had to sit ~15px away from the
    /// rendered edge to select it. Here we cross-check against glam's perspective_rh directly.
    #[test]
    fn perspective_matches_gpu_projection_at_non_90_fov() {
        use glam::{Mat4, Vec4};
        let cam_pos = Vec3::new(0.0, 0.0, 5.0);
        let cam_orbit = Quat::IDENTITY;
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let aspect = rect.width() / rect.height();
        let fov_deg = 50.0f32;
        // A point clearly off-center vertically (where the old 90° bug diverged most).
        let world_p = Vec3::new(0.7, 1.3, 0.0);

        // Ground truth: same camera the raster/SDF pipeline builds.
        let forward = (cam_orbit * Vec3::NEG_Z).normalize();
        let up = (cam_orbit * Vec3::Y).normalize();
        let view = Mat4::look_at_rh(cam_pos, cam_pos + forward, up);
        let proj = Mat4::perspective_rh(fov_deg.to_radians(), aspect, 0.1, 100.0);
        let clip = proj * view * Vec4::new(world_p.x, world_p.y, world_p.z, 1.0);
        let ndc = clip.truncate() / clip.w;
        let gpu = egui::pos2(
            (ndc.x * 0.5 + 0.5) * rect.width() + rect.min.x,
            (-ndc.y * 0.5 + 0.5) * rect.height() + rect.min.y,
        );

        let cpu = world_to_screen(world_p, cam_pos, cam_orbit, rect, aspect, 0.0, fov_deg).unwrap();
        assert!((cpu.x - gpu.x).abs() < 0.5, "x: cpu={} gpu={}", cpu.x, gpu.x);
        assert!((cpu.y - gpu.y).abs() < 0.5, "y: cpu={} gpu={}", cpu.y, gpu.y);
    }

    // ---- raycast_box_face ----

    #[test]
    fn raycast_box_face_hit_front() {
        let hit = raycast_box_face(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::ONE,
        );
        assert!(hit.is_some());
        let (t, normal) = hit.unwrap();
        assert!((t - 4.0).abs() < 0.01, "t should be ~4.0, got {}", t);
        assert!((normal.z - 1.0).abs() < 0.01, "normal should be +Z, got {:?}", normal);
    }

    #[test]
    fn raycast_box_face_miss() {
        let hit = raycast_box_face(
            Vec3::new(5.0, 5.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::ONE,
        );
        assert!(hit.is_none(), "ray should miss the box");
    }

    // ---- Keybinding tests ----

    #[test]
    fn default_bindings_exist_for_all_actions() {
        let kb = KeyBindings::defaults();
        for def in ACTION_DEFS {
            let bind = kb.get(def.id);
            let _ = bind.label();
        }
    }

    #[test]
    fn keybind_label_format() {
        let kb = KeyBind { key: egui::Key::Z, ctrl: true, shift: true, alt: false };
        let label = kb.label();
        assert_eq!(label, "Ctrl+Shift+Z");
    }

    #[test]
    fn keybind_label_simple() {
        let kb = KeyBind::simple(egui::Key::G);
        assert_eq!(kb.label(), "G");
    }

    #[test]
    fn keybind_label_ctrl() {
        let kb = KeyBind::ctrl(egui::Key::S);
        assert_eq!(kb.label(), "Ctrl+S");
    }

    #[test]
    fn conflict_detection_same_key() {
        let mut kb = KeyBindings::defaults();
        kb.set(ACT_GRAB, KeyBind::simple(egui::Key::G));
        kb.set(ACT_ROTATE, KeyBind::simple(egui::Key::G));
        let conflicts = kb.find_conflicts(&KeyBind::simple(egui::Key::G), ACT_ROTATE);
        assert!(!conflicts.is_empty(), "should detect conflict with GRAB");
    }

    #[test]
    fn no_conflict_different_keys() {
        let kb = KeyBindings::defaults();
        let conflicts = kb.find_conflicts(&KeyBind::simple(egui::Key::R), ACT_GRAB);
        let grab_bind = kb.get(ACT_GRAB);
        if *grab_bind != KeyBind::simple(egui::Key::R) {
            let has_grab = conflicts.iter().any(|(name, _)| *name == "Grab (Move)");
            assert!(!has_grab, "GRAB should not conflict with R");
        }
    }

    #[test]
    fn keybind_json_roundtrip() {
        let original = KeyBind { key: egui::Key::Z, ctrl: true, shift: true, alt: false };
        let json = original.to_json();
        let restored = KeyBind::from_json(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn prop_falloff_weight_boundaries() {
        for f in [PropFalloff::Smooth, PropFalloff::Sphere, PropFalloff::Root,
                  PropFalloff::InverseSquare, PropFalloff::Sharp, PropFalloff::Linear,
                  PropFalloff::Constant, PropFalloff::Random] {
            let w0 = f.weight(0.0);
            assert!((w0 - 1.0).abs() < 0.01, "{:?} weight(0) = {} (expected ~1)", f, w0);
            let w1 = f.weight(1.0);
            assert!(w1.abs() < 0.01, "{:?} weight(1) = {} (expected ~0)", f, w1);
            let w5 = f.weight(0.5);
            assert!(w5 >= 0.0 && w5 <= 1.0, "{:?} weight(0.5) = {} out of range", f, w5);
        }
    }

    #[test]
    fn prop_falloff_cycle() {
        let mut f = PropFalloff::Smooth;
        let start = f;
        for _ in 0..8 {
            f = f.next();
        }
        assert_eq!(f, start, "cycling through all falloffs should return to start");
    }
}
