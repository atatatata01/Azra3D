// Gizmo System types, constants, and helper functions — extracted from types.rs
use eframe::egui;
use glam::{Quat, Vec3};
use core_math::primitives::{SdfPrimitive, SdfShape, FalloffAxis};

// ==================================================================
// Gizmo System (Blender-grade Pro)
// ==================================================================
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum GizmoMode { Translate, Rotate, Scale, Elongate }

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum TransformSpace { Global, Local }

/// Modo-style Action Center — how G/R/S and the gizmo resolve **center + axes**.
/// Session state on `ToolState`; chosen via the **T** pie. Orthogonal to body pivot (D pie).
#[derive(PartialEq, Clone, Copy, Debug, Default)]
pub enum ActionCenter {
    /// Like Automatic but no click-to-redefine; center from selection/geometry, axes from space.
    #[default]
    None,
    /// Center on selection (or last Shift+RMB / Shift+LMB place); axes World / Local / Work Plane.
    /// Shift+RMB redefines the center temporarily (Modo Automatic uses LMB; we use Shift because LMB selects).
    /// Snap applies when the magnet is on (or Ctrl held). Stored in `ToolState::action_automatic_center`.
    Automatic,
    /// Center + axes from average of the current selection (poly faces/edges → normals).
    Selection,
    /// Center on selection/geometry; axes locked to a major world axis (see `action_auto_axis`).
    SelectionCenterAutoAxis,
    /// Center at view target; axes = camera/screen frame.
    Screen,
    /// World origin (0,0,0); world axes.
    Origin,
    /// Item/body pivot position; axes from Local/Global space.
    Pivot,
    /// Live poly element (selected face/edge/vert) or last Shift+RMB pick — center + full frame
    /// aligned to that element (face: +Y normal, +X longest edge; edge: +X along edge).
    Element,
    /// Work plane origin + work-plane axes (location and orientation of the active workplane).
    Workplane,
}

impl ActionCenter {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Automatic => "Automatic",
            Self::Selection => "Selection",
            Self::SelectionCenterAutoAxis => "Sel Auto Axis",
            Self::Screen => "Screen",
            Self::Origin => "Origin",
            Self::Pivot => "Pivot",
            Self::Element => "Element",
            Self::Workplane => "Workplane",
        }
    }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum GizmoComponent { AxisX, AxisY, AxisZ, Center }

impl GizmoComponent {
    pub fn label(&self) -> &str { match self { Self::AxisX=>"X", Self::AxisY=>"Y", Self::AxisZ=>"Z", Self::Center=>"⊙" } }
    pub fn all_axes() -> [GizmoComponent;3] { [Self::AxisX, Self::AxisY, Self::AxisZ] }
}

/// How to compute the origin (pivot) for an SdfBody.
#[derive(PartialEq, Clone, Copy, Debug, Default)]
pub enum BodyPivotMode {
    /// Centroid of all primitives in the body. OPT-IN only — see `Custom` for why this is no longer
    /// the default.
    Centroid,
    /// World origin (0,0,0).
    WorldOrigin,
    /// Center of axis-aligned bounding box.
    BoundingBox,
    /// Fixed origin — do NOT auto-recalculate (DEFAULT). Auto-recentering the origin to the geometry
    /// centroid on every structural edit (add / duplicate / group) shifted ALL prims to compensate,
    /// which ballooned the bake grid, flipped the origin, and churned the resident volume — the
    /// "group → surface breaks / fragments" and duplicate-balloon bugs. A fixed origin keeps every
    /// primitive in ONE stable frame (the "all prims same state" invariant + standard DCC: Blender
    /// never auto-chases geometry). The gizmo pivot uses the origin as-is; re-centering is now an
    /// explicit action, never automatic.
    #[default]
    Custom,
}

#[derive(Clone)]
pub struct TransformSnapshot {
    pub prim_uid: u32,
    pub original_pos: Vec3,
    pub original_rot: Vec3,
    pub original_scale: Vec3,
    pub original_elongation: Vec3,
    /// Full world orientation at drag start (body_rotation * prim_rot). Used for Local axes
    /// and as the rotation composition base — NEVER reconstruct from euler (lossy).
    pub original_orient: Quat,
}

pub struct GizmoInteraction {
    pub component: GizmoComponent, pub mode: GizmoMode, pub space: TransformSpace,
    pub drag_start_mouse: egui::Pos2, pub median_start_pos: Vec3,
    pub targets: Vec<TransformSnapshot>,
    /// Scene graph node targets for gizmo transform (ImportedMesh etc.)
    pub node_targets: Vec<NodeGizmoTarget>,
    /// Light targets for gizmo transform (Translate only — Rotate/Scale not supported).
    pub light_targets: Vec<crate::LightTransformSnapshot>,
    /// Pre-drag conservative AABBs for dirty region bake: (prim_uid, aabb_min, aabb_max).
    pub pre_drag_aabbs: Vec<(u32, Vec3, Vec3)>,
    /// Shift+Drag duplicate: the gizmo operates on duplicated prims.
    pub shift_duplicate: bool,
    /// For Elongate mode: true = dragging negative face (-X/-Y/-Z).
    pub elongate_face_negative: bool,
    /// Frozen orientation for gizmo DRAW + hit-test during this drag (Blender freezes axes).
    /// Live `active_rotation_quat()` would spin the arrows mid-rotate while math stays fixed.
    pub orient: Quat,
}

/// Snapshot of a scene graph node's transform for gizmo dragging.
#[derive(Clone)]
pub struct NodeGizmoTarget {
    pub node_id: core_math::scene_graph::NodeId,
    pub original_pos: Vec3,
    pub original_rot: Vec3,
    pub original_scale: Vec3,
    /// Full world orientation at drag start (body_rotation or node local quat).
    pub original_orient: Quat,
}

pub const GIZMO_ARROW_PX: f32 = 85.0;
pub const GIZMO_HIT_RADIUS: f32 = 12.0;
pub const GIZMO_LINE_WIDTH: f32 = 2.5;
pub const GIZMO_HOVER_WIDTH: f32 = 4.5;
pub const GIZMO_CENTER_RADIUS: f32 = 10.0;

pub fn gizmo_color(comp: GizmoComponent, hovered: bool) -> egui::Color32 {
    let a = if hovered { 255 } else { 180 };
    match comp {
        GizmoComponent::AxisX => egui::Color32::from_rgba_unmultiplied(240,80,80,a),
        GizmoComponent::AxisY => egui::Color32::from_rgba_unmultiplied(130,220,80,a),
        GizmoComponent::AxisZ => egui::Color32::from_rgba_unmultiplied(80,140,240,a),
        GizmoComponent::Center => egui::Color32::from_rgba_unmultiplied(255,255,255,a),
    }
}

/// Determine which world axis is most perpendicular to the camera forward direction.
/// Returns the GizmoComponent for that axis.
pub fn perpendicular_axis(cam_fwd: Vec3) -> GizmoComponent {
    let ax = cam_fwd.x.abs();
    let ay = cam_fwd.y.abs();
    let az = cam_fwd.z.abs();
    // The axis most aligned with cam_fwd is the one perpendicular to the screen plane.
    // That's the one the user naturally wants to rotate around.
    if ax >= ay && ax >= az { GizmoComponent::AxisX }
    else if ay >= az { GizmoComponent::AxisY }
    else { GizmoComponent::AxisZ }
}

pub fn get_gizmo_axis(comp: GizmoComponent, space: TransformSpace, obj_rot: Quat, cam_fwd: Vec3) -> Vec3 {
    let base = match comp {
        GizmoComponent::AxisX => Vec3::X, GizmoComponent::AxisY => Vec3::Y,
        GizmoComponent::AxisZ => Vec3::Z, GizmoComponent::Center => return (-cam_fwd).normalize(),
    };
    if space == TransformSpace::Local { (obj_rot * base).normalize() } else { base }
}

/// Get the half-extents of a shape in local space (before scale).
pub fn shape_half_extent(prim: &SdfPrimitive) -> Vec3 {
    match &prim.shape {
        SdfShape::Sphere{radius}               => Vec3::splat(*radius),
        SdfShape::Box{half_extents,..}         => *half_extents,
        SdfShape::Cylinder{radius,half_height} => Vec3::new(*radius,*half_height,*radius),
        SdfShape::Torus{major_radius,minor_radius} => Vec3::new(major_radius+minor_radius,*minor_radius,major_radius+minor_radius),
        SdfShape::Cone{radius,half_height}     => Vec3::new(*radius,*half_height,*radius),
        SdfShape::Capsule{radius,half_height}  => Vec3::new(*radius,half_height+radius,*radius),
        SdfShape::Stair{half_extents,..}       => *half_extents,
        SdfShape::SpiralStair{radius,half_height,..} => Vec3::new(*radius,half_height+radius,*radius),
        SdfShape::Billboard{width,height,..} => Vec3::new(*width*0.5, *height*0.5, 0.005),
        SdfShape::CurveExtrude { profile, half_depth, wall_t, thickness, tube, open, .. } => {
            // AABB of profile verts in XZ (local U/V), plus wall / tube / open thickness padding.
            let mut max_u = 0.5f32;
            let mut max_v = 0.5f32;
            for v in &profile.verts {
                max_u = max_u.max(v.pos.x.abs());
                max_v = max_v.max(v.pos.y.abs());
                // Per-vertex bevel can push the surface outward.
                max_u = max_u.max(v.pos.x.abs() + v.bevel_radius);
                max_v = max_v.max(v.pos.y.abs() + v.bevel_radius);
            }
            let wall_pad = wall_t.abs().max(thickness.abs()).max(if *tube { thickness.abs().max(0.05) } else { 0.0 });
            let open_pad = if *open { wall_pad.max(0.02) } else { wall_pad };
            let h = if *half_depth > 0.001 { *half_depth } else { max_u.max(max_v) };
            Vec3::new(max_u + open_pad, h + open_pad * 0.25, max_v + open_pad)
        },
        SdfShape::MeshVolume{grid, ..} => grid.corner_abs_max(),
        // HalfSpace is an infinite plane; a nominal half-extent gives sensible gizmo/falloff sizing
        // (this heuristic is NOT used for culling — the cull bound lives in bounding_radius()).
        SdfShape::HalfSpace => Vec3::splat(0.5),
        SdfShape::SpinePipe { points, radius, .. } => {
            let max_pt = points.iter().map(|q| q.length()).fold(0.5f32, f32::max);
            Vec3::splat(max_pt + *radius)
        }
    }
}

/// Compute sensible falloff preset from shape bounding box.
/// Returns (direction, origin_offset, length).
/// "Top" (+Y) → direction=+Y, origin at bottom, ramps up full height.
pub fn compute_falloff_preset(prim: &SdfPrimitive, axis: FalloffAxis) -> (Vec3, f32, f32) {
    let half = shape_half_extent(prim);
    let dir = axis.to_vec3();
    if dir.length_squared() < 0.001 { return (Vec3::ZERO, 0.0, 1.0); }
    let (ai, sign): (usize, f32) = match axis {
        FalloffAxis::PosX=>(0, 1.0), FalloffAxis::NegX=>(0,-1.0),
        FalloffAxis::PosY=>(1, 1.0), FalloffAxis::NegY=>(1,-1.0),
        FalloffAxis::PosZ=>(2, 1.0), FalloffAxis::NegZ=>(2,-1.0),
        FalloffAxis::None => unreachable!(),
    };
    let s = [prim.scale.x, prim.scale.y, prim.scale.z][ai];
    let ext = [half.x, half.y, half.z][ai] * s;
    let origin_offset = -sign * ext;  // start at opposite end (where k=0)
    let length = ext * 2.0;           // ramp across full shape extent
    (dir, origin_offset, length.max(0.01))
}

pub fn world_to_screen(world_p: Vec3, cam_pos: Vec3, cam_orbit: Quat, rect: egui::Rect, aspect: f32, ortho_hw: f32, fov_deg: f32) -> Option<egui::Pos2> {
    world_to_screen_ex(world_p, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg)
}

/// CPU projection that MUST match the GPU camera (`render::CameraUniforms::from_orbit` →
/// `Mat4::perspective_rh(fov_deg, aspect)`). `fov_deg` is the VERTICAL field of view; the perspective
/// branch applies `f = 1/tan(fov/2)` to both axes exactly as glam's `perspective_rh` fills
/// `proj[1][1]=f` and `proj[0][0]=f/aspect`. At fov=90° `f==1`, reproducing the legacy behaviour.
/// Hardcoding 90° here (the old bug) shifted every pick/overlay/gizmo away from the rendered geometry
/// whenever the viewport FOV slider ≠ 90°. `fov_deg` is ignored in the orthographic branch.
pub fn world_to_screen_ex(world_p: Vec3, cam_pos: Vec3, cam_orbit: Quat, rect: egui::Rect, aspect: f32, ortho_hw: f32, fov_deg: f32) -> Option<egui::Pos2> {
    let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
    let right = (cam_orbit * Vec3::X).normalize();
    let up = (cam_orbit * Vec3::Y).normalize();
    let d = world_p - cam_pos;
    let d_fwd = d.dot(fwd);
    if ortho_hw > 0.0 {
        // Orthographic projection
        Some(egui::pos2(
            (d.dot(right) / (ortho_hw * aspect) + 1.0) * 0.5 * rect.width() + rect.min.x,
            (-d.dot(up) / ortho_hw + 1.0) * 0.5 * rect.height() + rect.min.y,
        ))
    } else {
        // Perspective projection — f = 1/tan(fov_y/2) matches Mat4::perspective_rh(fov_deg, aspect).
        if d_fwd <= 0.05 { return None; }
        let f = 1.0 / (fov_deg.to_radians() * 0.5).tan();
        Some(egui::pos2(
            (d.dot(right)/d_fwd * f/aspect + 1.0) * 0.5 * rect.width() + rect.min.x,
            (-d.dot(up)/d_fwd * f + 1.0) * 0.5 * rect.height() + rect.min.y,
        ))
    }
}

/// Project a world-space LINE SEGMENT to screen, clipping it against the camera near plane FIRST.
///
/// `world_to_screen` alone rejects a point only once it is fully behind the near plane (`d_fwd <=
/// 0.05`) — but a point JUST in front (say `d_fwd = 0.06`, off-axis) survives and the perspective
/// divide sends it thousands of pixels away, so a selection/overlay line drawn to it shoots clear
/// across the viewport (the view-angle-dependent "selection flare"). Clipping the SEGMENT to the near
/// plane before projecting replaces that runaway endpoint with the edge's real near-plane crossing, so
/// the drawn line stays geometrically correct (egui then clips it to the viewport). Returns `None` when
/// the whole segment is at/behind the near plane. Ortho has no perspective divide, so it just projects.
pub fn world_segment_to_screen(
    p0: Vec3, p1: Vec3, cam_pos: Vec3, cam_orbit: Quat,
    rect: egui::Rect, aspect: f32, ortho_hw: f32, fov_deg: f32,
) -> Option<(egui::Pos2, egui::Pos2)> {
    if ortho_hw > 0.0 {
        return Some((
            world_to_screen_ex(p0, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg)?,
            world_to_screen_ex(p1, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg)?,
        ));
    }
    // Clip to just IN FRONT of world_to_screen_ex's 0.05 reject, so the clipped endpoint projects.
    const NEAR: f32 = 0.06;
    let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
    let d0 = (p0 - cam_pos).dot(fwd);
    let d1 = (p1 - cam_pos).dot(fwd);
    if d0 < NEAR && d1 < NEAR { return None; } // whole segment behind/at the near plane
    let mut a = p0;
    let mut b = p1;
    if d0 < NEAR { let t = (NEAR - d0) / (d1 - d0); a = p0 + (p1 - p0) * t; }
    if d1 < NEAR { let t = (NEAR - d1) / (d0 - d1); b = p1 + (p0 - p1) * t; }
    Some((
        world_to_screen_ex(a, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg)?,
        world_to_screen_ex(b, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg)?,
    ))
}

pub fn dist_point_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a; let ap = p - a;
    let len2 = ab.x*ab.x + ab.y*ab.y;
    if len2 < 0.001 { return (p - a).length(); }
    let t = ((ap.x*ab.x + ap.y*ab.y)/len2).clamp(0.0,1.0);
    (p - egui::pos2(a.x+ab.x*t, a.y+ab.y*t)).length()
}

pub fn dist_point_to_ring(
    mouse: egui::Pos2, center_3d: Vec3, axis_3d: Vec3, radius_3d: f32,
    cam_pos: Vec3, cam_orbit: Quat, rect: egui::Rect, aspect: f32, ortho_hw: f32, fov_deg: f32,
) -> f32 {
    let p1 = if axis_3d.y.abs()>0.9{Vec3::X}else{Vec3::Y}.cross(axis_3d).normalize();
    let p2 = axis_3d.cross(p1).normalize();
    let mut prev: Option<egui::Pos2> = None;
    let mut best = f32::MAX;
    for i in 0..=32 {
        let a = (i as f32 / 32.0) * std::f32::consts::TAU;
        let pt = center_3d + (p1*a.cos() + p2*a.sin()) * radius_3d;
        if let Some(sp) = world_to_screen(pt, cam_pos, cam_orbit, rect, aspect, ortho_hw, fov_deg) {
            if let Some(pv) = prev { best = best.min(dist_point_to_segment(mouse, pv, sp)); }
            prev = Some(sp);
        } else { prev = None; }
    }
    best
}

/// Snap position to absolute grid points.
pub fn snap_to_grid(pos: Vec3, grid: f32) -> Vec3 {
    Vec3::new(
        (pos.x / grid).round() * grid,
        (pos.y / grid).round() * grid,
        (pos.z / grid).round() * grid,
    )
}

/// Ray-box intersection returning (t_hit, local_face_normal).
/// Uses slab method in box local space; returns the nearest front-face hit.
pub fn raycast_box_face(
    ray_origin: Vec3, ray_dir: Vec3,
    box_pos: Vec3, box_rot: Quat, half_extents: Vec3,
) -> Option<(f32, Vec3)> {
    let inv_rot = box_rot.inverse();
    let lo = inv_rot * (ray_origin - box_pos);
    let ld = inv_rot * ray_dir;
    let inv = |v: f32| if v.abs() > 1e-8 { 1.0 / v } else { f32::copysign(1e8, v) };
    let (ix, iy, iz) = (inv(ld.x), inv(ld.y), inv(ld.z));
    let (t1x, t2x) = ((-half_extents.x - lo.x) * ix, (half_extents.x - lo.x) * ix);
    let (t1y, t2y) = ((-half_extents.y - lo.y) * iy, (half_extents.y - lo.y) * iy);
    let (t1z, t2z) = ((-half_extents.z - lo.z) * iz, (half_extents.z - lo.z) * iz);
    let t_enter = t1x.min(t2x).max(t1y.min(t2y)).max(t1z.min(t2z));
    let t_exit  = t1x.max(t2x).min(t1y.max(t2y)).min(t1z.max(t2z));
    if t_enter > t_exit || t_exit < 0.001 { return None; }
    let t_hit = if t_enter > 0.001 { t_enter } else { t_exit };
    let lh = lo + ld * t_hit;
    // Determine which face is closest to hit point
    let faces: [(Vec3, f32); 6] = [
        (Vec3::X,     (lh.x - half_extents.x).abs()),
        (Vec3::NEG_X, (lh.x + half_extents.x).abs()),
        (Vec3::Y,     (lh.y - half_extents.y).abs()),
        (Vec3::NEG_Y, (lh.y + half_extents.y).abs()),
        (Vec3::Z,     (lh.z - half_extents.z).abs()),
        (Vec3::NEG_Z, (lh.z + half_extents.z).abs()),
    ];
    let (normal, _) = faces.iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    Some((t_hit, *normal))
}
