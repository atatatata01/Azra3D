// core_engine/app/src/transform.rs
//
// =====================================================================
// WARNING: ALL transform math lives in compute_transform() below.
// Do NOT add grab/rotate/scale calculations elsewhere.
// To support a new target: add a TransformTarget variant and write the
// apply logic in apply_transform_result(). The math is SHARED.
// If you duplicate the math, future you will hate past you.
// =====================================================================

use eframe::egui;
use glam::{Quat, Vec3};
use crate::{AxisConstraint, GizmoComponent, TransformSpace, perpendicular_axis, world_to_screen};

/// Euler degrees (stored on prims/nodes) → quat. MUST match apply_to_scene_node / SceneNode (YXZ).
pub(crate) fn euler_yxz_deg_to_quat(e: Vec3) -> Quat {
    Quat::from_euler(glam::EulerRot::YXZ, e.y.to_radians(), e.x.to_radians(), e.z.to_radians())
}

/// Quat → euler degrees (YXZ). Inverse of [`euler_yxz_deg_to_quat`].
pub(crate) fn quat_to_euler_yxz_deg(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(glam::EulerRot::YXZ);
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}

/// World-space axis for an X/Y/Z constraint under Global or Local orientation.
/// `object_rot` is the orientation at drag-start (Identity is fine for pure Global).
pub(crate) fn constraint_axis_world(
    axis: crate::AxisConstraint,
    space: TransformSpace,
    object_rot: Quat,
) -> Option<Vec3> {
    use crate::AxisConstraint;
    let base = match axis {
        AxisConstraint::X | AxisConstraint::PlaneX => Vec3::X,
        AxisConstraint::Y | AxisConstraint::PlaneY => Vec3::Y,
        AxisConstraint::Z | AxisConstraint::PlaneZ => Vec3::Z,
        AxisConstraint::None => return None,
    };
    let w = if space == TransformSpace::Local {
        object_rot * base
    } else {
        base
    };
    let n = w.normalize_or_zero();
    if n.length_squared() < 1e-12 { None } else { Some(n) }
}

/// Pick Global/Local **X/Y/Z** from mouse motion around a pivot (screen-projected axes).
///
/// Used by free Grab Shift-lock and free **Scale** Shift-lock so Alt free-scale + Shift +
/// mouse direction locks scale to the axis the cursor is moving most along (owner 2026-08-07).
pub(crate) fn dominant_axis_constraint_from_mouse(
    pivot: Vec3,
    mouse_start: egui::Pos2,
    mouse_cur: egui::Pos2,
    cam: &TransformCamera,
    space: TransformSpace,
    object_rot: Quat,
) -> AxisConstraint {
    use crate::AxisConstraint;
    let rect = cam.viewport_rect;
    let aspect = rect.width() / rect.height().max(1.0);
    let ortho_hw = if cam.ortho { cam.cam_dist } else { 0.0 };
    let project = |p: Vec3| {
        world_to_screen(
            p,
            cam.cam_pos,
            cam.cam_orbit,
            rect,
            aspect,
            ortho_hw,
            cam.fov_deg,
        )
    };
    let Some(p0) = project(pivot) else {
        return AxisConstraint::X;
    };
    // Prefer drag delta; fall back to cursor vs pivot if the drag is still tiny.
    let motion = egui::vec2(mouse_cur.x - mouse_start.x, mouse_cur.y - mouse_start.y);
    let from_pivot = egui::vec2(mouse_cur.x - p0.x, mouse_cur.y - p0.y);
    let dir = if motion.length_sq() > 4.0 {
        motion
    } else {
        from_pivot
    };
    if dir.length_sq() < 1e-6 {
        return AxisConstraint::X;
    }
    let dir = dir.normalized();

    let tip_len = (cam.cam_dist.max(0.1) * 0.12).clamp(0.05, 50.0);
    let candidates = [
        AxisConstraint::X,
        AxisConstraint::Y,
        AxisConstraint::Z,
    ];
    let mut best = AxisConstraint::X;
    let mut best_score = -1.0f32;
    for ac in candidates {
        let Some(aw) = constraint_axis_world(ac, space, object_rot) else {
            continue;
        };
        let Some(ps) = project(pivot + aw * tip_len) else {
            continue;
        };
        let av = egui::vec2(ps.x - p0.x, ps.y - p0.y);
        let alen = av.length();
        if alen < 1e-4 {
            continue;
        }
        let score = (dir.dot(av / alen)).abs();
        if score > best_score {
            best_score = score;
            best = ac;
        }
    }
    best
}

/// Orbit an object's origin around `pivot` after a Rotate / Scale result.
/// Single-object gizmo R/S must do this so Action Center / Custom pivot is the
/// actual rotation center (owner: T=Pivot still spun around the object origin).
pub(crate) fn apply_orbit_around_pivot(
    kind: crate::TransformKind,
    origin: Vec3,
    pivot: Vec3,
    original_orient: Quat,
    original_scale: Vec3,
    result: &mut TransformResult,
) {
    let offset = origin - pivot;
    match kind {
        crate::TransformKind::Rotate => {
            let q1 = euler_yxz_deg_to_quat(result.rotation);
            let dq = (q1 * original_orient.inverse()).normalize();
            result.position = pivot + dq * offset;
        }
        crate::TransformKind::Scale => {
            const EPS: f32 = 1e-7;
            let safe_scale = original_scale.abs().max(Vec3::splat(EPS));
            let safe_result = result.scale.abs().max(Vec3::splat(EPS)) * result.scale.signum();
            let scale_ratio = safe_result / safe_scale;
            if scale_ratio.is_finite() {
                result.position = pivot + offset * scale_ratio;
            }
        }
        _ => {}
    }
}

/// Project a geometry snap target onto a gizmo / Shift axis lock through `origin`.
/// Used by `compute_transform` and multi-target gizmo snaps so Ctrl snap never leaves
/// the locked axis (owner: snap to vertex while gizmo handle still constrains).
pub(crate) fn project_snap_along_constraint(
    origin: Vec3,
    snap_target: Vec3,
    axis: crate::AxisConstraint,
    space: TransformSpace,
    object_rot: Quat,
) -> Vec3 {
    use crate::AxisConstraint;
    match axis {
        AxisConstraint::None => snap_target,
        AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z => {
            if let Some(dir) = constraint_axis_world(axis, space, object_rot) {
                origin + dir * (snap_target - origin).dot(dir)
            } else {
                snap_target
            }
        }
        AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
            if let Some(n) = constraint_axis_world(axis, space, object_rot) {
                let d = (snap_target - origin).dot(n);
                snap_target - n * d
            } else {
                snap_target
            }
        }
    }
}

/// Pure result of a transform computation — position, rotation, scale.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TransformResult {
    pub(crate) position: Vec3,
    pub(crate) rotation: Vec3,
    pub(crate) scale: Vec3,
    pub(crate) elongation: Vec3,
}

/// Unified transform settings — ONE source of truth for all paths.
#[derive(Clone, Copy)]
pub(crate) struct TransformSettings {
    pub(crate) grab_speed: f32,
    pub(crate) rotate_speed: f32,
    pub(crate) scale_speed: f32,
}

/// Camera state needed for transform math.
#[derive(Clone, Copy)]
pub(crate) struct TransformCamera {
    pub(crate) cam_pos: Vec3,
    pub(crate) cam_orbit: glam::Quat,
    pub(crate) cam_dist: f32,
    pub(crate) cam_fwd: Vec3,
    pub(crate) viewport_rect: egui::Rect,
    pub(crate) ortho: bool,
    /// Vertical FOV (deg) — MUST match the GPU camera so gizmo drag rays track the cursor at any FOV.
    pub(crate) fov_deg: f32,
    /// Active workplane normal (`viewport.wp_resolved.normal`) — free-move (Alt/Center grab) constrains to it.
    pub(crate) plane_normal: Vec3,
    /// Workplane U axis (in-plane reference for angle-lock 0°). Projected/orthonormalized at use.
    pub(crate) plane_u: Vec3,
}

/// Inputs that drive transform computation.
#[derive(Clone)]
pub(crate) struct TransformInput {
    pub(crate) kind: crate::TransformKind,
    pub(crate) axis: crate::AxisConstraint,
    pub(crate) original_pos: Vec3,
    pub(crate) original_rot: Vec3,
    pub(crate) original_scale: Vec3,
    pub(crate) original_elongation: Vec3,
    pub(crate) mouse_start: egui::Pos2,
    pub(crate) mouse_current: egui::Pos2,
    pub(crate) numeric_value: Option<f32>,
    pub(crate) snap_active: bool,
    pub(crate) snap_grid: f32,
    pub(crate) snap_rotation_deg: f32,
    pub(crate) snap_rotation_active: bool, // Shift-held rotation snapping (independent of snap_active)
    pub(crate) snap_pos: Option<Vec3>,
    /// When false, Grab ignores geometry/grid snap (Affects → Move off).
    pub(crate) snap_affect_move: bool,
    /// When false, Rotate ignores angle snap from magnet (Affects → Rotate off). Shift still snaps.
    pub(crate) snap_affect_rotate: bool,
    /// When false, Scale ignores grid-step quantize (Affects → Scale off).
    pub(crate) snap_affect_scale: bool,
    /// Free-drag grid quantize when no geometry hit (Grid target or Increment mode).
    pub(crate) snap_grid_quantize: bool,
    /// Snap OPTIONS → Angle lock: free-plane Grab direction snaps to `snap_rotation_deg`.
    /// Active when magnet/Ctrl + Affects Move. Shift axis-lock still wins over this.
    pub(crate) snap_angle_lock: bool,
    pub(crate) free_mode: bool,
    /// Elongate gizmo only: true when the grabbed face dot is on the negative side (-X/-Y/-Z).
    /// Flips the projected-drag sign so pulling EITHER face outward grows the elongation.
    pub(crate) elongate_face_negative: bool,
    /// Elongate + Alt: opposite face moves at the same world rate as the grab (symmetric).
    /// Default false = one-face extrude (opposite stays put).
    pub(crate) elongate_symmetric: bool,
    /// Global = world axes; Local = axes from `object_rot` (drag-start orientation).
    /// MUST match gizmo drawing (`get_gizmo_axis`) or arrows and motion diverge after rotation.
    pub(crate) space: TransformSpace,
    /// Object orientation at drag start (from original euler or body quat). Used only when
    /// `space == Local` (and for Local scale/rotate). Identity is fine for Global-only targets.
    pub(crate) object_rot: Quat,
    /// Mouse-delta multiplier for fine control while dragging.
    /// 1.0 = normal; Shift ≈ 0.25; Ctrl+Shift ≈ 0.08 (see [`drag_precision_scale`]).
    /// Callers may also pre-scale `mouse_current` via [`precision_scale_mouse`].
    pub(crate) precision_scale: f32,
}

/// Drag precision while a gizmo / transform is active.
/// - **Shift**: reduced sensitivity (~¼)
/// - **Ctrl+Shift**: fine precision (~1/12)
/// - neither: full speed
#[inline]
pub(crate) fn drag_precision_scale(shift: bool, ctrl: bool) -> f32 {
    if shift && ctrl {
        0.08
    } else if shift {
        0.25
    } else {
        1.0
    }
}

/// Scale `cur` toward `start` by `scale` (absolute mouse-from-start model used by transforms).
#[inline]
pub(crate) fn precision_scale_mouse(
    start: egui::Pos2,
    cur: egui::Pos2,
    scale: f32,
) -> egui::Pos2 {
    if (scale - 1.0).abs() < 1e-6 {
        return cur;
    }
    let s = scale.clamp(0.01, 1.0);
    egui::pos2(
        start.x + (cur.x - start.x) * s,
        start.y + (cur.y - start.y) * s,
    )
}

/// Compute the new position/rotation/scale from transform inputs.
/// This function is PURE — no mutation, no side effects.
/// Every transform path (modal G/R/S, gizmo drag, numeric input) calls this.
pub(crate) fn compute_transform(
    input: &TransformInput,
    cam: &TransformCamera,
    settings: &TransformSettings,
) -> TransformResult {
    use crate::{TransformKind, AxisConstraint};

    // Fine-control: shrink effective mouse travel from drag start (Shift / Ctrl+Shift).
    let prec = if input.precision_scale.is_finite() && input.precision_scale > 0.0 {
        input.precision_scale.clamp(0.01, 1.0)
    } else {
        1.0
    };
    let mouse_current = if (prec - 1.0).abs() < 1e-6 {
        input.mouse_current
    } else {
        precision_scale_mouse(input.mouse_start, input.mouse_current, prec)
    };

    let dx = mouse_current.x - input.mouse_start.x;
    let dy = mouse_current.y - input.mouse_start.y;
    let rect = cam.viewport_rect;
    let aspect = if rect.height() > 1.0 { rect.width() / rect.height() } else { 1.0 };
    let ortho_hw = if cam.ortho { cam.cam_dist } else { 0.0 };
    let spd = cam.cam_dist * settings.grab_speed;

    // ============ GRAB ============
    // Axis constraints respect TransformSpace: Global = world X/Y/Z, Local = object_rot * base.
    // (Previously always world — Local gizmo arrows and drag motion diverged after any rotation.)
    let new_pos = if input.kind == TransformKind::Grab {
        let explicit_plane_delta = |pn: Vec3| -> Vec3 {
            compute_plane_delta(
                input.mouse_start, mouse_current,
                input.original_pos, pn, cam,
            )
        };

        let mut delta = match input.axis {
            // Free move: active workplane (Global plane). Local free-move still uses workplane —
            // matching Blender's "no axis lock" grab, not a local plane.
            AxisConstraint::None => explicit_plane_delta(cam.plane_normal),
            AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z => {
                if let Some(aw) = constraint_axis_world(input.axis, input.space, input.object_rot) {
                    let d = input.numeric_value.unwrap_or_else(|| project_screen_to_axis(
                        input.original_pos, aw, dx, dy,
                        cam.cam_pos, cam.cam_orbit, rect, aspect, ortho_hw, cam.fov_deg, spd,
                    ));
                    aw * d
                } else {
                    Vec3::ZERO
                }
            }
            AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
                // Plane normal follows the same space as the axis gizmo (world or local).
                if let Some(n) = constraint_axis_world(input.axis, input.space, input.object_rot) {
                    explicit_plane_delta(n)
                } else {
                    Vec3::ZERO
                }
            }
        };

        // Shift while free-plane grab (AxisConstraint::None): lock to dominant world axis
        // (mouse-directed). Poly parity with 390fc40 — poly applies the same clamp after
        // compute_transform when free_mode+Shift; SDF free drag is gizmo Center / modal G
        // with axis None. snap_rotation_active is the Shift flag from handlers (also used
        // for rotation snap). Explicit axis constraints already single-axis-lock.
        if input.snap_rotation_active && matches!(input.axis, AxisConstraint::None) {
            let (ax, ay, az) = (delta.x.abs(), delta.y.abs(), delta.z.abs());
            delta = if ax >= ay && ax >= az {
                Vec3::new(delta.x, 0.0, 0.0)
            } else if ay >= az {
                Vec3::new(0.0, delta.y, 0.0)
            } else {
                Vec3::new(0.0, 0.0, delta.z)
            };
        } else if input.snap_angle_lock
            && input.snap_active
            && input.snap_affect_move
            && input.snap_rotation_deg > 0.5
            && input.numeric_value.is_none()
            && matches!(
                input.axis,
                AxisConstraint::None
                    | AxisConstraint::PlaneX
                    | AxisConstraint::PlaneY
                    | AxisConstraint::PlaneZ
            )
        {
            // Angle lock: quantize free / plane-constrained Move direction to Rotation step.
            // Reference 0° = workplane U (projected into the move plane). Used by poly edge
            // extrude → Grab, modal G, and free gizmo translate so extrude-at-N° works.
            let plane_n = match input.axis {
                AxisConstraint::None => cam.plane_normal,
                AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
                    constraint_axis_world(input.axis, input.space, input.object_rot)
                        .unwrap_or(cam.plane_normal)
                }
                _ => cam.plane_normal,
            };
            delta = quantize_delta_angle_in_plane(
                delta,
                plane_n,
                cam.plane_u,
                input.snap_rotation_deg,
            );
        }

        let raw = input.original_pos + delta;

        // Smart snap. Design rule (do NOT redesign Shift):
        //   • Shift + free grab already axis-locks `delta` above (mouse-dominant axis).
        //   • Geometry snap must NOT teleport off that lock or off a gizmo axis.
        //   • Free grab (no Shift, no gizmo axis) still teleports to the snap target.
        //   • Affects → Move gates all grab snapping.
        if input.snap_active && input.snap_affect_move {
            if let Some(sp) = input.snap_pos {
                // Effective lock:
                //   • Gizmo axis/plane → use that + transform space (Global/Local).
                //   • Shift free-lock → WORLD X/Y/Z only (matches the clamp above; do not
                //     re-interpret as Local — that was redesigning Shift).
                //   • Neither → free teleport to snap (original).
                let (lock, lock_space) = if !matches!(input.axis, AxisConstraint::None) {
                    (input.axis, input.space)
                } else if input.snap_rotation_active && delta.length_squared() > 1e-12 {
                    let (ax, ay, az) = (delta.x.abs(), delta.y.abs(), delta.z.abs());
                    let a = if ax >= ay && ax >= az {
                        AxisConstraint::X
                    } else if ay >= az {
                        AxisConstraint::Y
                    } else {
                        AxisConstraint::Z
                    };
                    (a, TransformSpace::Global)
                } else {
                    (AxisConstraint::None, input.space)
                };
                // Geometry snap stays on the gizmo / Shift lock — never free teleport off-axis.
                // Exact V/E/F hit: do NOT grid-quantize. Increment is the no-hit fallback
                // (owner: click X gizmo + vertex target must land on the vertex, not the grid).
                project_snap_along_constraint(
                    input.original_pos,
                    sp,
                    lock,
                    lock_space,
                    input.object_rot,
                )
            } else if input.snap_grid_quantize && input.snap_grid > 0.0 {
                // Grid quantize on the already-locked `raw` (Shift / gizmo already applied).
                Vec3::new(
                    (raw.x / input.snap_grid).round() * input.snap_grid,
                    (raw.y / input.snap_grid).round() * input.snap_grid,
                    (raw.z / input.snap_grid).round() * input.snap_grid,
                )
            } else {
                raw
            }
        } else {
            raw
        }
    } else {
        input.original_pos
    };

    // ============ ROTATE ============
    // Compose via quaternions (NOT euler addition). Euler += angle is wrong after any non-zero
    // rotation and is the main "R axis breaks after first rotate" failure.
    let new_rot = if input.kind == TransformKind::Rotate {
        let axis_w = match input.axis {
            AxisConstraint::None => {
                if input.free_mode {
                    cam.cam_fwd.normalize_or_zero()
                } else {
                    match perpendicular_axis(cam.cam_fwd) {
                        GizmoComponent::AxisX => {
                            constraint_axis_world(AxisConstraint::X, input.space, input.object_rot)
                                .unwrap_or(Vec3::X)
                        }
                        GizmoComponent::AxisY => {
                            constraint_axis_world(AxisConstraint::Y, input.space, input.object_rot)
                                .unwrap_or(Vec3::Y)
                        }
                        _ => {
                            constraint_axis_world(AxisConstraint::Z, input.space, input.object_rot)
                                .unwrap_or(Vec3::Z)
                        }
                    }
                }
            }
            other => constraint_axis_world(other, input.space, input.object_rot).unwrap_or(Vec3::Y),
        };
        let axis_w = if axis_w.length_squared() < 1e-12 { Vec3::Y } else { axis_w.normalize() };

        // Mouse → angle: TRUE angular delta around the ring axis (ray ∩ rotation plane).
        // A single screen-space tangent only matches ONE side of the circle — top half felt
        // reversed vs bottom. Angular measure works on the full ring.
        let raw_angle = input.numeric_value.unwrap_or_else(|| {
            rotation_delta_from_mouse(
                input.original_pos,
                axis_w,
                input.mouse_start,
                mouse_current,
                cam,
            )
        });
        // Angle snap: Shift always; magnet only when Affects → Rotate is on.
        let angle_snap = input.snap_rotation_active
            || (input.snap_active && input.snap_affect_rotate);
        let angle_deg = if angle_snap && input.snap_rotation_deg > 0.0 {
            (raw_angle / input.snap_rotation_deg).round() * input.snap_rotation_deg
        } else {
            raw_angle
        };
        let angle_rad = angle_deg.to_radians();

        let q_delta = Quat::from_axis_angle(axis_w, angle_rad);
        // MUST use object_rot (true quat at drag start), NOT euler_yxz_deg_to_quat(original_rot).
        let q0 = input.object_rot;
        let q1 = (q_delta * q0).normalize();
        quat_to_euler_yxz_deg(q1)
    } else {
        input.original_rot
    };

    // ============ SCALE ============
    // Scale components are stored in the object's local frame. Local space: scale the matching
    // component (X handle → scale.x). Global space after rotation: scale the local component whose
    // axis best aligns with the world handle (so the world-aligned handle still "does something
    // sensible" without inventing shear).
    //
    // Shift + free axis (None): lock to the axis the mouse is moving most along (Alt free-scale
    // or gizmo-center free scale — same idea as free Grab Shift-lock). Explicit X/Y/Z / Plane
    // constraints from keys or gizmo handles win over mouse-dir lock.
    let new_scale = if input.kind == TransformKind::Scale {
        let mut factor = input.numeric_value.unwrap_or_else(|| (1.0 + dx * settings.scale_speed).max(0.01));
        // Affects → Scale: quantize scale factor to grid-ish steps (0.05 when snap on).
        if input.snap_active && input.snap_affect_scale {
            let step = if input.snap_grid > 1e-6 {
                (input.snap_grid * 0.25).clamp(0.01, 0.25)
            } else {
                0.05
            };
            factor = (factor / step).round() * step;
            factor = factor.max(0.01);
        }
        let os = input.original_scale;

        let scale_axis = if matches!(input.axis, AxisConstraint::None)
            && input.snap_rotation_active
            && input.numeric_value.is_none()
        {
            // Shift held, no key/gizmo axis: mouse-direction single-axis lock.
            dominant_axis_constraint_from_mouse(
                input.original_pos,
                input.mouse_start,
                mouse_current,
                cam,
                input.space,
                input.object_rot,
            )
        } else {
            input.axis
        };

        match scale_axis {
            AxisConstraint::None => os * factor,
            AxisConstraint::X | AxisConstraint::Y | AxisConstraint::Z
            | AxisConstraint::PlaneX | AxisConstraint::PlaneY | AxisConstraint::PlaneZ => {
                if input.space == TransformSpace::Local {
                    match scale_axis {
                        AxisConstraint::X | AxisConstraint::PlaneX => Vec3::new(os.x * factor, os.y, os.z),
                        AxisConstraint::Y | AxisConstraint::PlaneY => Vec3::new(os.x, os.y * factor, os.z),
                        _ => Vec3::new(os.x, os.y, os.z * factor),
                    }
                } else {
                    // World handle / mouse lock → pick dominant local axis of the object at drag-start.
                    let aw = constraint_axis_world(scale_axis, TransformSpace::Global, Quat::IDENTITY)
                        .unwrap_or(Vec3::X);
                    let local = input.object_rot.inverse() * aw;
                    let ax = local.x.abs();
                    let ay = local.y.abs();
                    let az = local.z.abs();
                    if ax >= ay && ax >= az {
                        Vec3::new(os.x * factor, os.y, os.z)
                    } else if ay >= az {
                        Vec3::new(os.x, os.y * factor, os.z)
                    } else {
                        Vec3::new(os.x, os.y, os.z * factor)
                    }
                }
            }
        }
    } else {
        input.original_scale
    };

    // ============ ELONGATE (asymmetric face extrude, Alt = both faces) ============
    // Default: grabbed face follows cursor; opposite stays put (recenter).
    // Alt (`elongate_symmetric`): opposite moves at same world rate as grab (no recenter).
    let (new_elongation, elongate_pos_offset) = if input.kind == TransformKind::Elongate {
        let face_sign = if input.elongate_face_negative { -1.0 } else { 1.0 };
        let oe = input.original_elongation;
        // Elongation is a domain-warp in the prim's LOCAL frame, so its face dots — and the
        // grow direction — point along the prim's ROTATED axes, not world X/Y/Z. Project the
        // drag onto, and recenter along, the axis rotated by the prim's own orientation.
        // When the prim is unrotated this is identical to world-axis math (no regression).
        let r = input.original_rot * (std::f32::consts::PI / 180.0);
        let prim_rot = glam::Quat::from_euler(glam::EulerRot::YXZ, r.y, r.x, r.z);
        // One-face: de = disp/(2s) + recenter → grab moves disp, opposite fixed.
        // Both-face (Alt): de = disp/s, no recenter → each side moves disp.
        let solve = |axis_local: Vec3, oe_axis: f32, scale_axis: f32| -> (f32, Vec3) {
            let s = scale_axis.abs().max(1e-4);
            let axis_world = (prim_rot * axis_local).normalize_or_zero();
            let mut disp = input.numeric_value.unwrap_or_else(|| {
                let d = project_screen_to_axis(input.original_pos, axis_world, dx, dy, cam.cam_pos, cam.cam_orbit, rect, aspect, ortho_hw, cam.fov_deg, spd);
                if input.elongate_face_negative { -d } else { d }
            });
            // Snap: quantize displacement when magnet + Move affect + grid quantize.
            if input.snap_active
                && input.snap_affect_move
                && input.snap_grid_quantize
                && input.snap_grid > 1e-8
            {
                disp = (disp / input.snap_grid).round() * input.snap_grid;
            }
            if input.elongate_symmetric {
                let new_e = (oe_axis + disp / s).max(0.0);
                (new_e, Vec3::ZERO)
            } else {
                let new_e = (oe_axis + disp / (2.0 * s)).max(0.0);
                (new_e, axis_world * (face_sign * (new_e - oe_axis) * s))
            }
        };
        match input.axis {
            AxisConstraint::None => {
                // Center dot: symmetric all-axis elongation, no recentering.
                let d = input.numeric_value.unwrap_or(dx * settings.scale_speed * 2.0);
                ((oe + Vec3::splat(d)).max(Vec3::ZERO), Vec3::ZERO)
            }
            AxisConstraint::X | AxisConstraint::PlaneX => { let (ne, off) = solve(Vec3::X, oe.x, input.original_scale.x); (Vec3::new(ne, oe.y, oe.z), off) }
            AxisConstraint::Y | AxisConstraint::PlaneY => { let (ne, off) = solve(Vec3::Y, oe.y, input.original_scale.y); (Vec3::new(oe.x, ne, oe.z), off) }
            AxisConstraint::Z | AxisConstraint::PlaneZ => { let (ne, off) = solve(Vec3::Z, oe.z, input.original_scale.z); (Vec3::new(oe.x, oe.y, ne), off) }
        }
    } else {
        (input.original_elongation, Vec3::ZERO)
    };

    TransformResult { position: new_pos + elongate_pos_offset, rotation: new_rot, scale: new_scale, elongation: new_elongation }
}

/// Compute view-plane delta for free grab with constant speed.
/// Uses camera right/up vectors scaled by distance, so speed is independent
/// of where the mouse is on screen (no perspective foreshortening artifacts).
/// Retained as the camera-view-plane alternative; free-move now constrains to the active WORKPLANE
/// (`compute_plane_delta(cam.plane_normal)`) so there's no drift along the workplane normal.
#[allow(dead_code)]
fn compute_view_plane_delta(
    start: egui::Pos2,
    current: egui::Pos2,
    object_pos: Vec3,
    cam: &TransformCamera,
) -> Vec3 {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    let right = cam.cam_orbit * Vec3::X;
    let up = cam.cam_orbit * Vec3::Y;
    let h = cam.viewport_rect.height().max(64.0);
    // Scale factor: pixels → world units at the object's depth
    let depth = if cam.ortho {
        cam.cam_dist
    } else {
        let to_obj = object_pos - cam.cam_pos;
        let fwd = cam.cam_orbit * Vec3::NEG_Z;
        to_obj.dot(fwd).max(0.1)
    };
    // Assume ~45° half-FOV (tan ≈ 1.0); adjust if FOV is stored elsewhere
    let scale = if cam.ortho { cam.cam_dist * 2.0 / h } else { depth * 2.0 / h };
    right * (dx * scale) + up * (-dy * scale)
}

/// Lock a free-plane (or plane-constrained) translation delta to the nearest angle step.
///
/// - `plane_n` — plane normal (workplane or gizmo plane)
/// - `plane_u` — in-plane 0° reference (workplane U); re-orthonormalized against `plane_n`
/// - `step_deg` — degrees (from snap menu Rotation step); values < 0.5 leave `delta` unchanged
///
/// Length of the in-plane component is preserved. Residual along the normal is dropped.
pub(crate) fn quantize_delta_angle_in_plane(
    delta: Vec3,
    plane_n: Vec3,
    plane_u: Vec3,
    step_deg: f32,
) -> Vec3 {
    let n = plane_n.normalize_or_zero();
    if n.length_squared() < 0.25 || step_deg < 0.5 {
        return delta;
    }
    let d = delta - n * delta.dot(n);
    let len = d.length();
    if len < 1e-8 {
        return Vec3::ZERO;
    }
    let mut u = plane_u - n * plane_u.dot(n);
    if u.length_squared() < 1e-8 {
        let a = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        u = a - n * a.dot(n);
    }
    let u = u.normalize_or_zero();
    if u.length_squared() < 0.25 {
        return d;
    }
    let v = n.cross(u).normalize_or_zero();
    let x = d.dot(u);
    let y = d.dot(v);
    let step = step_deg.to_radians().max(1e-4);
    let ang = (y.atan2(x) / step).round() * step;
    (u * ang.cos() + v * ang.sin()) * len
}

/// Compute delta by intersecting mouse rays with a world-space plane through `object_pos`.
/// The returned delta has the component along `plane_normal` zeroed out.
fn compute_plane_delta(
    start: egui::Pos2,
    current: egui::Pos2,
    object_pos: Vec3,
    plane_normal: Vec3,
    cam: &TransformCamera,
) -> Vec3 {
    let (ro_start, rd_start) = cam_ray(start, cam);
    let (ro_current, rd_current) = cam_ray(current, cam);

    let denom_s = plane_normal.dot(rd_start);
    let denom_c = plane_normal.dot(rd_current);

    if denom_s.abs() < 1e-6 || denom_c.abs() < 1e-6 {
        return Vec3::ZERO;
    }

    let p_start = ro_start + rd_start * (plane_normal.dot(object_pos - ro_start) / denom_s);
    let p_current = ro_current + rd_current * (plane_normal.dot(object_pos - ro_current) / denom_c);

    let d = p_current - p_start;
    d - plane_normal * d.dot(plane_normal)
}

/// Compute a camera ray (origin, direction) from a screen position.
/// Perspective: origin = cam_pos, direction fans out per pixel.
/// Ortho: direction = forward for every pixel, and the ORIGIN shifts per pixel across the
/// view plane (same math as `AzraApp::cam_ray` in camera.rs). The old `cam_ray_dir` returned
/// only the direction, so in ortho both drag rays were identical → plane grab delta was
/// always ZERO → Alt/Center free-mode move was dead in orthographic view.
fn cam_ray(screen_pos: egui::Pos2, cam: &TransformCamera) -> (Vec3, Vec3) {
    let rect = cam.viewport_rect;
    let w = rect.width().max(64.0);
    let h = rect.height().max(64.0);
    let ndc_x = (screen_pos.x - rect.left()) / w * 2.0 - 1.0;
    let ndc_y = -((screen_pos.y - rect.top()) / h * 2.0 - 1.0);
    let right = cam.cam_orbit * Vec3::X;
    let up = cam.cam_orbit * Vec3::Y;
    let fwd = cam.cam_orbit * Vec3::NEG_Z;
    let aspect = (w / h).clamp(0.1, 10.0);

    if cam.ortho {
        // Ortho half-width matches world_to_screen / camera.rs cam_ray: ortho_hw = cam.distance.
        let ortho_hw = cam.cam_dist;
        let origin = cam.cam_pos + right * (ndc_x * ortho_hw * aspect) + up * (ndc_y * ortho_hw);
        (origin, fwd)
    } else {
        // tan(fov/2) inverts world_to_screen's f=1/tan(fov/2) — keeps drag rays on the cursor at any FOV.
        let tan_half = (cam.fov_deg.to_radians() * 0.5).tan();
        (cam.cam_pos, (fwd + right * ndc_x * aspect * tan_half + up * ndc_y * tan_half).normalize())
    }
}

/// Degrees of rotation from a mouse drag around `world_axis` through `origin`.
///
/// Intersects start/current camera rays with the rotation plane (normal = axis) and
/// measures the signed angle between the hit vectors. That is constant on the whole
/// ring — unlike a fixed screen tangent, which only matches one half of the circle.
fn rotation_delta_from_mouse(
    origin: Vec3,
    world_axis: Vec3,
    mouse_start: egui::Pos2,
    mouse_current: egui::Pos2,
    cam: &TransformCamera,
) -> f32 {
    let axis = world_axis.normalize_or_zero();
    if axis.length_squared() < 1e-12 {
        return 0.0;
    }

    let hit = |screen: egui::Pos2| -> Option<Vec3> {
        let (ro, rd) = cam_ray(screen, cam);
        let denom = axis.dot(rd);
        // Plane edge-on to the ray → no stable ring hit.
        if denom.abs() < 1e-5 {
            return None;
        }
        let t = axis.dot(origin - ro) / denom;
        // Behind the camera is unusable.
        if !cam.ortho && t < 0.0 {
            return None;
        }
        Some(ro + rd * t)
    };

    let (Some(p0), Some(p1)) = (hit(mouse_start), hit(mouse_current)) else {
        // Fallback: horizontal drag when the ring plane is edge-on to the view.
        let dx = mouse_current.x - mouse_start.x;
        return -dx * 0.5; // degrees-ish; matches previous rotate_speed default feel
    };

    // Vectors in the rotation plane (from pivot to ring hits).
    let mut v0 = p0 - origin;
    let mut v1 = p1 - origin;
    v0 -= axis * v0.dot(axis);
    v1 -= axis * v1.dot(axis);
    let l0 = v0.length();
    let l1 = v1.length();
    if l0 < 1e-6 || l1 < 1e-6 {
        return 0.0;
    }
    let v0n = v0 / l0;
    let v1n = v1 / l1;

    // Signed angle from v0 → v1 around `axis` (right-hand rule).
    let cos = v0n.dot(v1n).clamp(-1.0, 1.0);
    let sin = v0n.cross(v1n).dot(axis);
    sin.atan2(cos).to_degrees()
}

/// Project screen-space mouse delta onto a world axis.
/// Returns the signed distance along the axis.
fn project_screen_to_axis(
    origin: Vec3,
    world_axis: Vec3,
    dx: f32,
    dy: f32,
    cam_pos: Vec3,
    cam_orbit: glam::Quat,
    rect: egui::Rect,
    aspect: f32,
    ortho_hw: f32,
    fov_deg: f32,
    speed: f32,
) -> f32 {
    let p0 = world_to_screen(origin, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
    let p1 = world_to_screen(origin + world_axis, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg);
    if let (Some(p0), Some(p1)) = (p0, p1) {
        let screen_dir = egui::vec2(p1.x - p0.x, p1.y - p0.y);
        let len = screen_dir.length();
        if len < 1.0 { return 0.0; }
        let screen_norm = egui::vec2(screen_dir.x / len, screen_dir.y / len);
        (dx * screen_norm.x + dy * screen_norm.y) * speed
    } else {
        0.0
    }
}

/// Build a TransformCamera from AzraApp state.
/// Helper to avoid repeating camera extraction everywhere.
impl crate::AzraApp {
    pub(crate) fn transform_camera(&self) -> TransformCamera {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        TransformCamera {
            cam_pos,
            cam_orbit: self.viewport.camera.orbit,
            cam_dist: self.viewport.camera.distance,
            cam_fwd: (self.viewport.camera.orbit * Vec3::NEG_Z).normalize(),
            viewport_rect: self.viewport.viewport_rect,
            ortho: self.render.ortho,
            fov_deg: self.render.fov_deg,
            plane_normal: self.viewport.wp_resolved.normal,
            plane_u: self.viewport.wp_resolved.tangent_u,
        }
    }

    pub(crate) fn transform_settings(&self) -> TransformSettings {
        TransformSettings {
            grab_speed: self.tweak.grab_speed,
            rotate_speed: self.tweak.rotate_speed,
            scale_speed: self.tweak.scale_speed,
        }
    }
}

#[cfg(test)]
mod angle_lock_tests {
    use super::quantize_delta_angle_in_plane;
    use glam::Vec3;

    #[test]
    fn angle_lock_snaps_to_15_on_xz() {
        // Workplane Y-up, U = +X. ~22° from +X toward +Z → snaps to 15° at step 15°.
        // Basis: v = n×u = Y×X = −Z, so +Z direction is negative angle in atan2(y,x).
        let n = Vec3::Y;
        let u = Vec3::X;
        let ang = 22f32.to_radians();
        let raw = (Vec3::X * ang.cos() + Vec3::Z * ang.sin()) * 2.0;
        let out = quantize_delta_angle_in_plane(raw, n, u, 15.0);
        let want_ang = 15f32.to_radians();
        let want = (Vec3::X * want_ang.cos() + Vec3::Z * want_ang.sin()) * 2.0;
        assert!((out - want).length() < 1e-3, "got {out:?} want {want:?}");
        // In-plane angle from +X toward +Z should be ~15°.
        let got_ang = out.z.atan2(out.x).to_degrees().abs();
        assert!((got_ang - 15.0).abs() < 0.5, "got_ang={got_ang}");
    }

    #[test]
    fn angle_lock_preserves_length() {
        let raw = Vec3::new(1.2, 0.0, 0.7);
        let out = quantize_delta_angle_in_plane(raw, Vec3::Y, Vec3::X, 15.0);
        assert!((out.length() - raw.length()).abs() < 1e-4);
    }

    #[test]
    fn angle_lock_off_when_step_zero() {
        let raw = Vec3::new(1.0, 0.0, 0.5);
        let out = quantize_delta_angle_in_plane(raw, Vec3::Y, Vec3::X, 0.0);
        assert_eq!(out, raw);
    }
}

#[cfg(test)]
mod gizmo_vertex_snap_tests {
    use super::{project_snap_along_constraint, compute_transform, TransformInput, TransformCamera, TransformSettings};
    use crate::{AxisConstraint, TransformKind, TransformSpace};
    use glam::{Quat, Vec3};

    fn dummy_cam() -> TransformCamera {
        TransformCamera {
            cam_pos: Vec3::new(0.0, 0.0, 10.0),
            cam_orbit: Quat::IDENTITY,
            cam_dist: 10.0,
            cam_fwd: Vec3::NEG_Z,
            viewport_rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0)),
            ortho: true,
            fov_deg: 50.0,
            plane_normal: Vec3::Y,
            plane_u: Vec3::X,
        }
    }

    fn grab_input(origin: Vec3, axis: AxisConstraint, snap: Option<Vec3>, quantize: bool) -> TransformInput {
        TransformInput {
            kind: TransformKind::Grab,
            axis,
            original_pos: origin,
            original_rot: Vec3::ZERO,
            original_scale: Vec3::ONE,
            original_elongation: Vec3::ZERO,
            mouse_start: egui::pos2(400.0, 300.0),
            mouse_current: egui::pos2(480.0, 300.0),
            numeric_value: None,
            snap_active: true,
            snap_grid: 1.0,
            snap_rotation_deg: 15.0,
            snap_rotation_active: false,
            snap_pos: snap,
            snap_affect_move: true,
            snap_affect_rotate: true,
            snap_affect_scale: true,
            snap_grid_quantize: quantize,
            snap_angle_lock: false,
            free_mode: false,
            elongate_face_negative: false,
            elongate_symmetric: false,
            space: TransformSpace::Global,
            object_rot: Quat::IDENTITY,
            precision_scale: 1.0,
        }
    }

    #[test]
    fn project_snap_along_x_keeps_yz() {
        let origin = Vec3::new(3.0, 1.0, -2.0);
        let target = Vec3::new(0.4, 8.0, 5.0);
        let p = project_snap_along_constraint(
            origin,
            target,
            AxisConstraint::X,
            TransformSpace::Global,
            Quat::IDENTITY,
        );
        assert!((p.x - 0.4).abs() < 1e-5, "x must match target, got {p:?}");
        assert!((p.y - 1.0).abs() < 1e-5, "y stays on the lock, got {p:?}");
        assert!((p.z + 2.0).abs() < 1e-5, "z stays on the lock, got {p:?}");
    }

    #[test]
    fn gizmo_axis_vertex_snap_is_not_grid_quantized() {
        // Owner: X-gizmo + vertex target at x=2.13 must land on 2.13, not round to 2.0.
        let origin = Vec3::new(4.0, 0.0, 0.0);
        let target = Vec3::new(2.13, 1.5, -0.7);
        let input = grab_input(origin, AxisConstraint::X, Some(target), true);
        let out = compute_transform(&input, &dummy_cam(), &TransformSettings {
            grab_speed: 0.002,
            rotate_speed: 0.4,
            scale_speed: 0.01,
        });
        assert!(
            (out.position.x - 2.13).abs() < 1e-4,
            "vertex snap must not quantize to the increment grid, got {}",
            out.position.x
        );
        assert!((out.position.y).abs() < 1e-4);
        assert!((out.position.z).abs() < 1e-4);
    }

    #[test]
    fn rotate_orbits_object_origin_around_custom_pivot() {
        // Object origin at (0,0,0), pivot at (1,0,0). 180° about Y → origin goes to (2,0,0).
        let origin = Vec3::ZERO;
        let pivot = Vec3::new(1.0, 0.0, 0.0);
        let mut result = super::TransformResult {
            position: origin,
            rotation: Vec3::new(0.0, 180.0, 0.0),
            scale: Vec3::ONE,
            elongation: Vec3::ZERO,
        };
        super::apply_orbit_around_pivot(
            TransformKind::Rotate,
            origin,
            pivot,
            Quat::IDENTITY,
            Vec3::ONE,
            &mut result,
        );
        assert!(
            (result.position - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-3,
            "origin must orbit the pivot, got {:?}",
            result.position
        );
    }
}
