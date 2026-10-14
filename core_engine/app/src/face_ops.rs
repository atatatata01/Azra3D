//! Face push/pull operations — shared actions called from keyboard and viewport input.
//!
//! Elongate-gizmo face dots resize **true shape parameters** (grow AND shrink below the default
//! size). The legacy `elongation` domain-warp only grows (`max(0)`) and cannot shrink — Box,
//! Cylinder, CurveExtrude, and other parametric shapes use this path instead.

use glam::Vec2;
use core_math::primitives::BevelMode;
use crate::*;

/// Minimum half-extent / radius after shrink (world-ish local units).
const MIN_SIZE: f32 = 0.01;

impl AzraApp {
    // ---- Face mode (Push/Pull) ----

    /// Start face push/pull from G key in edit mode (uses first selected face).
    #[allow(dead_code)]
    pub(crate) fn start_face_push_from_selection(&mut self, mouse: egui::Pos2) {
        let target = match self.sel.edit_target { Some(t) => t, None => return };
        if target >= self.scene.primitives.len() { return; }
        let face_n = match self.sel.selected_faces.first() { Some(n) => *n, None => return };
        if self.scene.primitives[target].locked { self.toast("Object is locked"); return; }
        let (pos, he, sc, curve_uv) = {
            let p = &self.scene.primitives[target];
            match shape_face_size_and_curve_uv(&p.shape) {
                Some((he, uv)) => (p.position, he, p.scale, uv),
                None => return,
            }
        };
        self.begin_episode("Push/Pull Face");
        self.start_operation(ActiveTool::FacePushPull(FaceDrag {
            prim_idx: target, local_normal: face_n,
            start_mouse: mouse,
            original_pos: pos, original_extents: he,
            original_scale: sc,
            accumulated_delta: 0.0,
            original_curve_uv: curve_uv,
        }));
    }

    /// Elongate gizmo face dots: resize true shape params (grow AND shrink past default size).
    /// Handles all parametric SDF shapes. Returns false only for non-resizable shapes
    /// (HalfSpace, MeshVolume) so the caller can fall back to domain-warp elongation.
    pub(crate) fn try_start_box_face_extrude(&mut self, comp: GizmoComponent, mouse: egui::Pos2) -> bool {
        if self.tool.gizmo_mode != GizmoMode::Elongate { return false; }
        let sign = if self.tool.elongate_face_negative { -1.0 } else { 1.0 };
        let local_normal = match comp {
            GizmoComponent::AxisX => Vec3::new(sign, 0.0, 0.0),
            GizmoComponent::AxisY => Vec3::new(0.0, sign, 0.0),
            GizmoComponent::AxisZ => Vec3::new(0.0, 0.0, sign),
            GizmoComponent::Center => return false, // centre dot → symmetric elongate path
        };
        let idx = match self.active_prim_index_unified() {
            Some(i) if i < self.scene.primitives.len() => i,
            _ => return false,
        };
        if self.scene.primitives[idx].locked { self.toast("Object is locked"); return true; }
        if shape_face_size_and_curve_uv(&self.scene.primitives[idx].shape).is_none() {
            return false; // HalfSpace / MeshVolume → legacy elongation
        }

        self.begin_episode("Resize shape");
        // Fold any legacy `elongation` into real size so params are the single size-of-record.
        let elong = self.scene.primitives[idx].elongation;
        if elong != Vec3::ZERO {
            self.write_prim(idx, |p| {
                fold_elongation_into_shape(&mut p.shape, elong);
                p.elongation = Vec3::ZERO;
            });
        }
        let (ext, curve_uv) = match shape_face_size_and_curve_uv(&self.scene.primitives[idx].shape) {
            Some(v) => v,
            None => { self.cancel_episode(); return false; }
        };
        let pos = self.scene.primitives[idx].position;
        let scale = self.scene.primitives[idx].scale;
        self.start_operation(ActiveTool::FacePushPull(FaceDrag {
            prim_idx: idx, local_normal,
            start_mouse: mouse,
            original_pos: pos, original_extents: ext,
            original_scale: scale,
            accumulated_delta: 0.0,
            original_curve_uv: curve_uv,
        }));
        true
    }

    /// Apply push/pull delta: asymmetric size + position offset.
    /// `alt` = both faces move (symmetric; center fixed).
    /// `shift` / `ctrl+shift` = reduced / fine drag sensitivity (gizmo precision).
    /// Shrinks below default size are allowed (floor = MIN_SIZE).
    pub(crate) fn apply_face_drag(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) {
        let fd = match self.tool.current.as_face_push_pull() { Some(f) => f, None => return };
        let (prim_idx, local_normal, start_mouse, orig_pos, orig_ext, orig_scale, orig_curve_uv) =
            (fd.prim_idx, fd.local_normal, fd.start_mouse, fd.original_pos, fd.original_extents,
             fd.original_scale, fd.original_curve_uv.clone());
        if prim_idx >= self.scene.primitives.len() { return; }
        let prim = &self.scene.primitives[prim_idx];
        let rot = prim.rotation_quat();
        let world_normal = (rot * local_normal).normalize();
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let s0 = world_to_screen(orig_pos, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let s1 = world_to_screen(orig_pos + world_normal * 0.5, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
        let (s0, s1) = match (s0, s1) { (Some(a), Some(b)) => (a, b), _ => return };
        let screen_axis = (s1 - s0).normalized();
        let mouse = crate::transform::precision_scale_mouse(
            start_mouse,
            mouse,
            crate::transform::drag_precision_scale(shift, ctrl),
        );
        let screen_delta = egui::vec2(mouse.x - start_mouse.x, mouse.y - start_mouse.y);
        let projected = screen_delta.x * screen_axis.x + screen_delta.y * screen_axis.y;
        let world_delta = projected * self.viewport.camera.distance * self.tweak.face_drag_speed;
        let world_delta = if self.is_snap_active(ctrl) {
            let grid = self.viewport.grid_cell_size.max(0.01);
            (world_delta / grid).round() * grid
        } else { world_delta };
        let ext_delta = if alt { world_delta } else { world_delta * 0.5 };
        let pos_delta = if alt { 0.0 } else { world_delta * 0.5 };
        let axis = if local_normal.x.abs() > 0.5 { 0usize }
                   else if local_normal.y.abs() > 0.5 { 1 } else { 2 };
        let scale_comp = match axis { 0 => orig_scale.x, 1 => orig_scale.y, _ => orig_scale.z }.max(self.tweak.scale_min_clamp);
        let mut new_ext = orig_ext;
        match axis {
            0 => new_ext.x = (orig_ext.x + ext_delta / scale_comp).max(MIN_SIZE),
            1 => new_ext.y = (orig_ext.y + ext_delta / scale_comp).max(MIN_SIZE),
            _ => new_ext.z = (orig_ext.z + ext_delta / scale_comp).max(MIN_SIZE),
        }
        // Radial shapes: X and Z both map to the same radius — keep them equal when either is dragged.
        if shape_is_radial(&self.scene.primitives[prim_idx].shape) {
            if axis == 0 || axis == 2 {
                let r = if axis == 0 { new_ext.x } else { new_ext.z };
                new_ext.x = r;
                new_ext.z = r;
            }
        }
        // Sphere: all axes share one radius.
        if matches!(self.scene.primitives[prim_idx].shape, SdfShape::Sphere { .. }) {
            let r = match axis {
                0 => new_ext.x,
                1 => new_ext.y,
                _ => new_ext.z,
            };
            new_ext = Vec3::splat(r);
        }
        let new_pos = orig_pos + world_normal * pos_delta;
        self.write_prim(prim_idx, |p| {
            p.position = new_pos;
            apply_shape_face_size(&mut p.shape, new_ext, orig_ext, &orig_curve_uv);
            // Keep bevel radii legal after resize — Uniform round-box clamps r to min half-extent;
            // without this, shrink leaves r > half-extent and the cube "freezes" visually.
            clamp_box_bevel_to_extents(&mut p.shape);
        });
        if let Some(fd) = self.tool.current.as_face_push_pull_mut() { fd.accumulated_delta = world_delta; }
        self.rebake_no_undo();
    }
}

/// After half-extents change, clamp per-edge bevel radii to the smallest half-extent
/// (matches GPU/CPU `max_r` for Uniform and Per-Edge eval).
fn clamp_box_bevel_to_extents(shape: &mut SdfShape) {
    if let SdfShape::Box { half_extents, bevel } = shape {
        let max_r = half_extents.x.min(half_extents.y).min(half_extents.z).max(0.0);
        for i in 0..12 {
            if bevel.radii[i] > max_r {
                bevel.radii[i] = max_r;
            }
            if bevel.radii[i] < 1e-6 {
                bevel.modes[i] = BevelMode::None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shape size helpers — single mapping used by start + apply
// ---------------------------------------------------------------------------

/// True if X and Z both mean the same radial parameter.
fn shape_is_radial(shape: &SdfShape) -> bool {
    matches!(
        shape,
        SdfShape::Cylinder { .. }
            | SdfShape::Cone { .. }
            | SdfShape::Capsule { .. }
            | SdfShape::SpiralStair { .. }
            | SdfShape::Torus { .. }
            | SdfShape::Sphere { .. }
    )
}

/// Local size vector + optional CurveExtrude UV snapshot for non-compounding scale.
pub(crate) fn shape_face_size_and_curve_uv(shape: &SdfShape) -> Option<(Vec3, Vec<Vec2>)> {
    match shape {
        SdfShape::HalfSpace | SdfShape::MeshVolume { .. } => None,
        SdfShape::Sphere { radius } => Some((Vec3::splat(*radius), Vec::new())),
        SdfShape::Box { half_extents, .. } 
        | SdfShape::Stair { half_extents, .. } => Some((*half_extents, Vec::new())),
        SdfShape::Cylinder { radius, half_height }
        | SdfShape::Cone { radius, half_height } => {
            Some((Vec3::new(*radius, *half_height, *radius), Vec::new()))
        }
        SdfShape::Capsule { radius, half_height } => {
            Some((Vec3::new(*radius, *half_height, *radius), Vec::new()))
        }
        SdfShape::Torus { major_radius, minor_radius } => {
            let r = *major_radius + *minor_radius;
            Some((Vec3::new(r, *minor_radius, r), Vec::new()))
        }
        SdfShape::SpiralStair { radius, half_height, .. } => {
            Some((Vec3::new(*radius, *half_height, *radius), Vec::new()))
        }
        SdfShape::Billboard { width, height, .. } => {
            Some((Vec3::new(*width * 0.5, *height * 0.5, 0.005), Vec::new()))
        }
        SdfShape::CurveExtrude { profile, half_depth, .. } => {
            let mut max_x = MIN_SIZE;
            let mut max_z = MIN_SIZE;
            let mut uvs = Vec::with_capacity(profile.verts.len());
            for v in &profile.verts {
                max_x = max_x.max(v.pos.x.abs());
                max_z = max_z.max(v.pos.y.abs()); // profile Y = local Z
                uvs.push(v.pos);
            }
            let h = half_depth.max(MIN_SIZE);
            Some((Vec3::new(max_x, h, max_z), uvs))
        }
        SdfShape::SpinePipe { points, radius, .. } => {
            let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
            let he = (max_pt + *radius).max(MIN_SIZE);
            Some((Vec3::splat(he), Vec::new()))
        }
    }
}

/// Bake positive elongation into real shape params (inverse of domain-warp grow).
fn fold_elongation_into_shape(shape: &mut SdfShape, elong: Vec3) {
    if elong == Vec3::ZERO { return; }
    match shape {
        SdfShape::Box { half_extents, .. } 
        | SdfShape::Stair { half_extents, .. } => {
            *half_extents += elong;
        }
        SdfShape::Sphere { radius } => {
            *radius += elong.x.max(elong.y).max(elong.z);
        }
        SdfShape::Cylinder { radius, half_height }
        | SdfShape::Cone { radius, half_height }
        | SdfShape::Capsule { radius, half_height } => {
            *radius += elong.x.max(elong.z);
            *half_height += elong.y;
        }
        SdfShape::SpiralStair { radius, half_height, .. } => {
            *radius += elong.x.max(elong.z);
            *half_height += elong.y;
        }
        SdfShape::Torus { major_radius, minor_radius } => {
            *minor_radius += elong.y;
            *major_radius += elong.x.max(elong.z);
        }
        SdfShape::Billboard { width, height, .. } => {
            *width += elong.x * 2.0;
            *height += elong.y * 2.0;
        }
        SdfShape::CurveExtrude { profile, half_depth, .. } => {
            *half_depth += elong.y;
            let (ox, oz) = {
                let mut mx = MIN_SIZE;
                let mut mz = MIN_SIZE;
                for v in &profile.verts {
                    mx = mx.max(v.pos.x.abs());
                    mz = mz.max(v.pos.y.abs());
                }
                (mx, mz)
            };
            let sx = (ox + elong.x) / ox;
            let sz = (oz + elong.z) / oz;
            for v in &mut profile.verts {
                v.pos.x *= sx;
                v.pos.y *= sz;
            }
        }
        SdfShape::SpinePipe { points, radius, .. } => {
            *radius += elong.x.max(elong.y).max(elong.z);
            // Grow path extents slightly along axes
            for p in points.iter_mut() {
                *p += elong * p.signum(); // rough; mostly radius matters for feel
            }
        }
        _ => {}
    }
}

/// Write `new_ext` into shape params. `orig_ext` + `orig_curve_uv` used for CurveExtrude scale.
fn apply_shape_face_size(
    shape: &mut SdfShape,
    new_ext: Vec3,
    orig_ext: Vec3,
    orig_curve_uv: &[Vec2],
) {
    match shape {
        SdfShape::Box { half_extents, .. } 
        | SdfShape::Stair { half_extents, .. } => {
            *half_extents = new_ext;
        }
        SdfShape::Sphere { radius } => {
            *radius = new_ext.x.max(new_ext.y).max(new_ext.z).max(MIN_SIZE);
        }
        SdfShape::Cylinder { radius, half_height }
        | SdfShape::Cone { radius, half_height }
        | SdfShape::Capsule { radius, half_height } => {
            *radius = new_ext.x.max(new_ext.z).max(MIN_SIZE);
            *half_height = new_ext.y.max(MIN_SIZE);
        }
        SdfShape::SpiralStair { radius, half_height, .. } => {
            *radius = new_ext.x.max(new_ext.z).max(MIN_SIZE);
            *half_height = new_ext.y.max(MIN_SIZE);
        }
        SdfShape::Torus { major_radius, minor_radius } => {
            let minor = new_ext.y.max(MIN_SIZE);
            let total = new_ext.x.max(new_ext.z).max(minor + MIN_SIZE);
            *minor_radius = minor;
            *major_radius = (total - minor).max(MIN_SIZE);
        }
        SdfShape::Billboard { width, height, .. } => {
            *width = (new_ext.x * 2.0).max(MIN_SIZE * 2.0);
            *height = (new_ext.y * 2.0).max(MIN_SIZE * 2.0);
        }
        SdfShape::CurveExtrude { profile, half_depth, .. } => {
            *half_depth = new_ext.y.max(MIN_SIZE);
            if !orig_curve_uv.is_empty() && profile.verts.len() == orig_curve_uv.len() {
                let sx = new_ext.x / orig_ext.x.max(MIN_SIZE);
                let sz = new_ext.z / orig_ext.z.max(MIN_SIZE);
                for (i, v) in profile.verts.iter_mut().enumerate() {
                    v.pos = Vec2::new(orig_curve_uv[i].x * sx, orig_curve_uv[i].y * sz);
                }
            } else {
                // Fallback: scale current profile from current max extents (may compound if
                // called repeatedly without a snapshot — start path always supplies UV).
                let (cx, cz) = {
                    let mut mx = MIN_SIZE;
                    let mut mz = MIN_SIZE;
                    for v in &profile.verts {
                        mx = mx.max(v.pos.x.abs());
                        mz = mz.max(v.pos.y.abs());
                    }
                    (mx, mz)
                };
                let sx = new_ext.x / cx;
                let sz = new_ext.z / cz;
                for v in &mut profile.verts {
                    v.pos.x *= sx;
                    v.pos.y *= sz;
                }
            }
        }
        SdfShape::SpinePipe { points, radius, .. } => {
            // Scale tube radius from the size vector; scale points from origin by extent ratio.
            let old_he = orig_ext.x.max(MIN_SIZE);
            let new_he = new_ext.x.max(new_ext.y).max(new_ext.z).max(MIN_SIZE);
            let s = new_he / old_he;
            // Prefer keeping radius proportional when only size changes.
            *radius = (*radius * s).max(MIN_SIZE * 0.5);
            for p in points.iter_mut() {
                *p *= s;
            }
        }
        _ => {}
    }
}
