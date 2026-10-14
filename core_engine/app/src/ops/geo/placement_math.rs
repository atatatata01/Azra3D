//! Pure placement / extrude math for create sessions and future nodes.
//!
//! Extracted from `create_tool.rs` (engine still owns session state; math is shared).

use glam::Vec3;

/// Camera-facing fence plane depth: virtual plane hinged around the extrude axis.
/// Returns depth along `extrude_axis` from `draw_center`.
pub fn compute_extrude_depth(
    ray_o: Vec3,
    ray_d: Vec3,
    draw_center: Vec3,
    extrude_axis: Vec3,
    cam_pos: Vec3,
    cam_up: Vec3,
) -> Option<f32> {
    let axis = extrude_axis.normalize();
    let to_cam = cam_pos - draw_center;

    let mut plane_n = to_cam - axis * to_cam.dot(axis);
    let len_sq = plane_n.length_squared();

    if len_sq < 1e-6 {
        plane_n = (cam_up - axis * cam_up.dot(axis)).normalize_or_zero();
        if plane_n.length_squared() < 1e-6 {
            return None;
        }
    } else {
        plane_n /= len_sq.sqrt();
    }

    let denom = ray_d.dot(plane_n);
    if denom.abs() < 1e-6 {
        return None;
    }

    let t = (draw_center - ray_o).dot(plane_n) / denom;
    if t < 0.0 {
        return None;
    }

    let hit_pt = ray_o + ray_d * t;
    Some((hit_pt - draw_center).dot(axis))
}

/// Creation plane priority: locked workplane → scene hit → floor → workplane fallback.
/// Returns `(hit_point, hit_normal, on_surface)`.
pub fn compute_creation_plane(
    origin: Vec3,
    dir: Vec3,
    wp_locked: bool,
    wp_origin: Vec3,
    wp_normal: Vec3,
    scene_hit: Option<(Vec3, Vec3)>,
    floor_y: f32,
) -> Option<(Vec3, Vec3, bool)> {
    if wp_locked {
        let denom = dir.dot(wp_normal);
        if denom.abs() < 1e-8 {
            return None;
        }
        let t = (wp_origin - origin).dot(wp_normal) / denom;
        if t < 0.001 || t > 500.0 {
            return None;
        }
        return Some((origin + dir * t, wp_normal, false));
    }
    if let Some((p, n)) = scene_hit {
        return Some((p, n, true));
    }
    let floor_t = if dir.y.abs() > 1e-6 {
        (floor_y - origin.y) / dir.y
    } else {
        -1.0
    };
    if floor_t > 0.001 && floor_t < 500.0 {
        return Some((origin + dir * floor_t, Vec3::Y, false));
    }
    let denom = dir.dot(wp_normal);
    if denom.abs() < 1e-8 {
        return None;
    }
    let t = (wp_origin - origin).dot(wp_normal) / denom;
    if t < 0.001 || t > 500.0 {
        return None;
    }
    Some((origin + dir * t, wp_normal, false))
}
