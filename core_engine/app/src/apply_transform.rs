// apply_transform.rs — Freeze / Reset Location · Rotation · Scale · All
//
// Freeze = bake channels so world look stays, transform channels clean.
// Reset  = zero channels (object may move) — separate destructive path.
//
// Poly freeze: bake into mesh verts.
// SDF body freeze: bake body TRS into prims.
// SDF prim freeze location: re-home body origin so prim local pos → 0, world stays.
// SDF prim freeze rotation: absorb prim rot into body rot, keep world.
// SDF prim freeze scale: bake into shape params (stays parametric).

use eframe::egui;
use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use core_math::primitives::SdfShape;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::ui_theme as t;
use crate::{DirtyFlags, FourthLayerApp};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyXform {
    Location,
    Rotation,
    Scale,
    All,
}

/// Menu pick from Properties → Display → Apply Transform dropdown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyMenuAction {
    Freeze(ApplyXform),
    Reset(ApplyXform),
}

impl ApplyXform {
    fn freeze_label(self) -> &'static str {
        match self {
            Self::Location => "Freeze Location",
            Self::Rotation => "Freeze Rotation",
            Self::Scale => "Freeze Scale",
            Self::All => "Freeze All",
        }
    }
    fn reset_label(self) -> &'static str {
        match self {
            Self::Location => "Reset Location",
            Self::Rotation => "Reset Rotation",
            Self::Scale => "Reset Scale",
            Self::All => "Reset All",
        }
    }
    pub(crate) fn tip_freeze_sdf_prim(self) -> &'static str {
        match self {
            Self::Location => {
                "Re-home body origin to this prim (local pos → 0). World look unchanged."
            }
            Self::Rotation => {
                "Absorb prim rotation into body rotation (prim rot → 0). World look unchanged."
            }
            Self::Scale => {
                "Bake scale into shape size (half-extents / radius…). Scale → 1. Keeps SDF parametric."
            }
            Self::All => "Freeze Scale, then Location, then Rotation. World look unchanged.",
        }
    }
    pub(crate) fn tip_reset_sdf_prim(self) -> &'static str {
        match self {
            Self::Location => "Zero position — moves prim to body/world origin",
            Self::Rotation => "Zero rotation — reorients prim axes",
            Self::Scale => "Set scale to 1 (does not bake — use Freeze Scale to keep size)",
            Self::All => "Zero position & rotation, scale → 1 (may move / shrink)",
        }
    }
    pub(crate) fn tip_freeze_poly(self) -> &'static str {
        match self {
            Self::Location => "Bake location into mesh verts · Position → 0 · look unchanged",
            Self::Rotation => "Bake rotation into mesh verts · Rotation → 0 · look unchanged",
            Self::Scale => "Bake scale into mesh verts · Scale → 1 · look unchanged",
            Self::All => "Bake full TRS into mesh · transform → identity · look unchanged",
        }
    }
    pub(crate) fn tip_reset_poly(self) -> &'static str {
        match self {
            Self::Location => "Zero node position (mesh moves to parent origin)",
            Self::Rotation => "Zero node rotation",
            Self::Scale => "Set node scale to 1 (mesh shrinks/grows if non-1)",
            Self::All => "Identity transform (mesh may jump)",
        }
    }
    pub(crate) fn tip_freeze_sdf_body(self) -> &'static str {
        match self {
            Self::Location => "Add body origin into every prim · body origin → 0 · look unchanged",
            Self::Rotation => "Rotate prims by body rot · body rot → I · look unchanged",
            Self::Scale => "Bake body scale into each prim · body scale → 1 · look unchanged",
            Self::All => "Bake body TRS into prims · body transform → identity",
        }
    }
    pub(crate) fn tip_reset_sdf_body(self) -> &'static str {
        match self {
            Self::Location => "Zero body origin (body moves)",
            Self::Rotation => "Zero body rotation",
            Self::Scale => "Body scale → 1",
            Self::All => "Identity body transform",
        }
    }
}

/// Kind of object the menu is for (tips + dispatch).
#[derive(Clone, Copy)]
pub(crate) enum ApplyTargetKind {
    SdfPrim,
    Poly,
    SdfBody,
}

/// Dropdown: Freeze + Reset groups.
pub(crate) fn draw_apply_menu(
    ui: &mut egui::Ui,
    target: ApplyTargetKind,
) -> Option<ApplyMenuAction> {
    let mut hit = None;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("APPLY TRANSFORM")
                .size(9.5)
                .color(t::TEXT_SECTION)
                .strong(),
        );
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let btn = ui.add_sized(
            egui::vec2((ui.available_width() - 8.0).clamp(120.0, 220.0), 24.0),
            egui::Button::new(
                egui::RichText::new("Freeze / Reset  ▾")
                    .size(11.5)
                    .color(t::TEXT),
            ),
        );
        let popup_id = ui.make_persistent_id("props_apply_xform_menu");
        if btn.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup_below_widget(
            ui,
            popup_id,
            &btn,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(200.0);
                ui.label(
                    egui::RichText::new("FREEZE (keep look)")
                        .size(9.5)
                        .color(t::TEXT_SECTION)
                        .strong(),
                );
                for k in [
                    ApplyXform::Location,
                    ApplyXform::Rotation,
                    ApplyXform::Scale,
                    ApplyXform::All,
                ] {
                    let tip = match target {
                        ApplyTargetKind::SdfPrim => k.tip_freeze_sdf_prim(),
                        ApplyTargetKind::Poly => k.tip_freeze_poly(),
                        ApplyTargetKind::SdfBody => k.tip_freeze_sdf_body(),
                    };
                    if ui
                        .add(egui::Button::new(k.freeze_label()).wrap())
                        .on_hover_text(tip)
                        .clicked()
                    {
                        hit = Some(ApplyMenuAction::Freeze(k));
                        ui.close_menu();
                    }
                }
                ui.separator();
                ui.label(
                    egui::RichText::new("RESET (may move)")
                        .size(9.5)
                        .color(t::TEXT_SECTION)
                        .strong(),
                );
                for k in [
                    ApplyXform::Location,
                    ApplyXform::Rotation,
                    ApplyXform::Scale,
                    ApplyXform::All,
                ] {
                    let tip = match target {
                        ApplyTargetKind::SdfPrim => k.tip_reset_sdf_prim(),
                        ApplyTargetKind::Poly => k.tip_reset_poly(),
                        ApplyTargetKind::SdfBody => k.tip_reset_sdf_body(),
                    };
                    if ui
                        .add(egui::Button::new(k.reset_label()).wrap())
                        .on_hover_text(tip)
                        .clicked()
                    {
                        hit = Some(ApplyMenuAction::Reset(k));
                        ui.close_menu();
                    }
                }
            },
        );
    });
    ui.add_space(2.0);
    hit
}

/// Fold scale into analytical shape parameters. Returns false if shape cannot absorb scale.
pub(crate) fn bake_scale_into_shape(shape: &mut SdfShape, scale: Vec3) -> bool {
    let s = scale.abs();
    let sx = s.x.max(1e-6);
    let sy = s.y.max(1e-6);
    let sz = s.z.max(1e-6);
    let smax = sx.max(sy).max(sz);
    let sxz = ((sx + sz) * 0.5).max(1e-6);
    match shape {
        SdfShape::Sphere { radius } => {
            *radius = (*radius * smax).max(1e-6);
            true
        }
        SdfShape::Box { half_extents, .. } => {
            *half_extents = (*half_extents * s).max(Vec3::splat(1e-6));
            true
        }
        SdfShape::Cylinder { radius, half_height } => {
            *radius = (*radius * sxz).max(1e-6);
            *half_height = (*half_height * sy).max(1e-6);
            true
        }
        SdfShape::Torus {
            major_radius,
            minor_radius,
        } => {
            *major_radius = (*major_radius * sxz).max(1e-6);
            *minor_radius = (*minor_radius * smax).max(1e-6);
            true
        }
        SdfShape::Cone { radius, half_height } => {
            *radius = (*radius * sxz).max(1e-6);
            *half_height = (*half_height * sy).max(1e-6);
            true
        }
        SdfShape::Capsule { radius, half_height } => {
            *radius = (*radius * sxz).max(1e-6);
            *half_height = (*half_height * sy).max(1e-6);
            true
        }
        SdfShape::Stair { half_extents, .. } => {
            *half_extents = (*half_extents * s).max(Vec3::splat(1e-6));
            true
        }
        SdfShape::SpiralStair {
            radius,
            half_height,
            step_width,
            ..
        } => {
            *radius = (*radius * sxz).max(1e-6);
            *half_height = (*half_height * sy).max(1e-6);
            *step_width = (*step_width * sxz).max(1e-6);
            true
        }
        SdfShape::Billboard { width, height, .. } => {
            *width = (*width * sx).max(1e-6);
            *height = (*height * sy).max(1e-6);
            true
        }
        SdfShape::CurveExtrude {
            profile,
            half_depth,
            thickness,
            ..
        } => {
            let s2 = Vec2::new(sx, sz);
            for v in &mut profile.verts {
                v.pos *= s2;
            }
            *half_depth = (*half_depth * sy).max(0.0);
            *thickness = (*thickness * sxz).max(0.0);
            true
        }
        SdfShape::SpinePipe { points, radius, .. } => {
            for q in points.iter_mut() {
                *q *= s;
            }
            // Tube radius is isotropic — fold the dominant axis (Sphere precedent).
            *radius = (*radius * smax).max(1e-6);
            true
        }
        SdfShape::MeshVolume { .. } | SdfShape::HalfSpace => false,
    }
}

fn euler_to_quat(e: Vec3) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        e.y.to_radians(),
        e.x.to_radians(),
        e.z.to_radians(),
    )
}
fn quat_to_euler(q: Quat) -> Vec3 {
    let (yaw, pitch, roll) = q.to_euler(EulerRot::YXZ);
    Vec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees())
}

impl FourthLayerApp {
    /// Dispatch Properties Apply Transform menu for an SDF prim.
    pub(crate) fn apply_menu_sdf_prim(&mut self, idx: usize, action: ApplyMenuAction) {
        match action {
            ApplyMenuAction::Freeze(k) => self.freeze_sdf_prim(idx, k),
            ApplyMenuAction::Reset(k) => self.reset_sdf_prim(idx, k),
        }
    }
    pub(crate) fn apply_menu_poly(&mut self, nid: NodeId, action: ApplyMenuAction) {
        match action {
            ApplyMenuAction::Freeze(k) => self.apply_transform_poly(nid, k),
            ApplyMenuAction::Reset(k) => self.reset_poly_node(nid, k),
        }
    }
    pub(crate) fn apply_menu_sdf_body(&mut self, nid: NodeId, action: ApplyMenuAction) {
        match action {
            ApplyMenuAction::Freeze(k) => self.apply_transform_sdf_body(nid, k),
            ApplyMenuAction::Reset(k) => self.reset_sdf_body(nid, k),
        }
    }

    /// True freeze for SDF prim — world look preserved.
    pub(crate) fn freeze_sdf_prim(&mut self, idx: usize, kind: ApplyXform) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        if self.scene.primitives[idx].locked {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Freeze Location",
            ApplyXform::Rotation => "Freeze Rotation",
            ApplyXform::Scale => "Freeze Scale",
            ApplyXform::All => "Freeze All Transforms",
        };
        let mut ok_msg = String::new();
        self.transaction(name, |s| {
            match kind {
                ApplyXform::Scale => {
                    let (pos, rot, scl, baked) = {
                        let p = &mut s.scene.primitives[idx];
                        let sc = p.scale;
                        if (sc - Vec3::ONE).abs().max_element() < 1e-6 {
                            ok_msg = "Scale already frozen (1)".into();
                            return;
                        }
                        let baked = bake_scale_into_shape(&mut p.shape, sc);
                        p.elongation *= sc.abs();
                        if baked {
                            p.scale = Vec3::ONE;
                        }
                        (p.position, p.rotation_deg, p.scale, baked)
                    };
                    ok_msg = if baked {
                        "Scale frozen into shape params".into()
                    } else {
                        "Shape keeps scale on transform (MeshVolume / HalfSpace)".into()
                    };
                    s.mirror_prim_fields(idx);
                    s.write_prim_transform_ex(idx, pos, rot, scl, true);
                }
                ApplyXform::Location => {
                    // Re-home body origin so this prim's local position → 0; all prims stay in world.
                    let Some(&(nid, _)) = s.scene.prim_body_map.get(idx) else {
                        // No body: just zero (orphaned prim) — still freeze-ish for flat list
                        let p = &mut s.scene.primitives[idx];
                        // Without a body we cannot re-home; keep world by no-op if already 0
                        if p.position.length_squared() < 1e-12 {
                            ok_msg = "Location already at origin".into();
                            return;
                        }
                        // Orphan: true freeze impossible without pivot_offset — keep look by no-op toast
                        ok_msg = "No body to re-home — select a body prim".into();
                        return;
                    };
                    let delta = s.scene.primitives[idx].position;
                    if delta.length_squared() < 1e-12 {
                        ok_msg = "Location already frozen (local 0)".into();
                        return;
                    }
                    // body.origin (payload) += body_rotation * (body_scale * delta)
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody {
                            primitives,
                            origin,
                            body_rotation,
                            body_scale,
                            ..
                        } = &mut node.payload
                        {
                            let world_delta = *body_rotation * (*body_scale * delta);
                            *origin += world_delta;
                            node.local_position = *origin;
                            for p in primitives.iter_mut() {
                                p.position -= delta;
                            }
                        }
                    }
                    s.sync_flat_cache();
                    ok_msg = "Location frozen (body origin re-homed)".into();
                }
                ApplyXform::Rotation => {
                    // Absorb prim rotation into body rotation; adjust sibling positions/rots.
                    let Some(&(nid, body_idx)) = s.scene.prim_body_map.get(idx) else {
                        ok_msg = "No body to absorb rotation".into();
                        return;
                    };
                    let prim_q = euler_to_quat(s.scene.primitives[idx].rotation_deg);
                    if (prim_q.xyz().length_squared() + (prim_q.w - 1.0).powi(2)) < 1e-10 {
                        ok_msg = "Rotation already frozen".into();
                        return;
                    }
                    let inv = prim_q.inverse();
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody {
                            primitives,
                            body_rotation,
                            ..
                        } = &mut node.payload
                        {
                            // New body rot absorbs prim rot
                            *body_rotation = *body_rotation * prim_q;
                            let (yaw, pitch, roll) = body_rotation.to_euler(EulerRot::YXZ);
                            node.local_rotation =
                                Vec3::new(pitch.to_degrees(), yaw.to_degrees(), roll.to_degrees());
                            // Transform all prims into new body frame: p' = inv * p
                            for (i, p) in primitives.iter_mut().enumerate() {
                                p.position = inv * p.position;
                                let q = inv * euler_to_quat(p.rotation_deg);
                                p.rotation_deg = quat_to_euler(q);
                                if i == body_idx {
                                    p.rotation_deg = Vec3::ZERO;
                                }
                            }
                        }
                    }
                    s.sync_flat_cache();
                    ok_msg = "Rotation frozen into body".into();
                }
                ApplyXform::All => {
                    // Scale first, then location, then rotation (order matters).
                    s.freeze_sdf_prim_inner_scale(idx, &mut ok_msg);
                    // Location + rotation need nested logic — call sequences via flags
                }
            }
        });
        // All: do three passes outside single match for clarity
        if kind == ApplyXform::All {
            self.transaction("Freeze All Transforms", |s| {
                let mut msg = String::new();
                s.freeze_sdf_prim_inner_scale(idx, &mut msg);
                // Location re-home
                if let Some(&(nid, _)) = s.scene.prim_body_map.get(idx) {
                    let delta = s.scene.primitives[idx].position;
                    if delta.length_squared() > 1e-12 {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            if let NodePayload::SdfBody {
                                primitives,
                                origin,
                                body_rotation,
                                body_scale,
                                ..
                            } = &mut node.payload
                            {
                                let world_delta = *body_rotation * (*body_scale * delta);
                                *origin += world_delta;
                                node.local_position = *origin;
                                for p in primitives.iter_mut() {
                                    p.position -= delta;
                                }
                            }
                        }
                        s.sync_flat_cache();
                    }
                }
                // Rotation absorb
                if let Some(&(nid, body_idx)) = s.scene.prim_body_map.get(idx) {
                    let prim_q = euler_to_quat(s.scene.primitives[idx].rotation_deg);
                    if prim_q.xyz().length_squared() + (prim_q.w - 1.0).powi(2) > 1e-10 {
                        let inv = prim_q.inverse();
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            if let NodePayload::SdfBody {
                                primitives,
                                body_rotation,
                                ..
                            } = &mut node.payload
                            {
                                *body_rotation = *body_rotation * prim_q;
                                let (yaw, pitch, roll) = body_rotation.to_euler(EulerRot::YXZ);
                                node.local_rotation = Vec3::new(
                                    pitch.to_degrees(),
                                    yaw.to_degrees(),
                                    roll.to_degrees(),
                                );
                                for (i, p) in primitives.iter_mut().enumerate() {
                                    p.position = inv * p.position;
                                    let q = inv * euler_to_quat(p.rotation_deg);
                                    p.rotation_deg = quat_to_euler(q);
                                    if i == body_idx {
                                        p.rotation_deg = Vec3::ZERO;
                                    }
                                }
                            }
                        }
                        s.sync_flat_cache();
                    }
                }
                ok_msg = "All transforms frozen".into();
            });
        }
        self.toast(&ok_msg);
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
    }

    /// Helper used inside transactions — scale freeze only.
    fn freeze_sdf_prim_inner_scale(&mut self, idx: usize, ok_msg: &mut String) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        let (pos, rot, scl, baked) = {
            let p = &mut self.scene.primitives[idx];
            let sc = p.scale;
            if (sc - Vec3::ONE).abs().max_element() < 1e-6 {
                return;
            }
            let baked = bake_scale_into_shape(&mut p.shape, sc);
            p.elongation *= sc.abs();
            if baked {
                p.scale = Vec3::ONE;
            }
            (p.position, p.rotation_deg, p.scale, baked)
        };
        self.mirror_prim_fields(idx);
        self.write_prim_transform_ex(idx, pos, rot, scl, true);
        if baked {
            *ok_msg = "Scale frozen".into();
        }
    }

    fn mirror_prim_fields(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        let p = &self.scene.primitives[idx];
        let pos = p.position;
        let rot = p.rotation_deg;
        let scl = p.scale;
        let shape = p.shape.clone();
        let elong = p.elongation;
        if let Some(&(nid, body_idx)) = self.scene.prim_body_map.get(idx) {
            if let Some(prims) = self.scene.scene_graph.body_primitives_mut(nid) {
                if body_idx < prims.len() {
                    prims[body_idx].position = pos;
                    prims[body_idx].rotation_deg = rot;
                    prims[body_idx].scale = scl;
                    prims[body_idx].shape = shape;
                    prims[body_idx].elongation = elong;
                }
            }
        }
    }

    /// Destructive reset for SDF prim (may move).
    pub(crate) fn reset_sdf_prim(&mut self, idx: usize, kind: ApplyXform) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        if self.scene.primitives[idx].locked {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Reset Location",
            ApplyXform::Rotation => "Reset Rotation",
            ApplyXform::Scale => "Reset Scale",
            ApplyXform::All => "Reset Transform",
        };
        self.transaction(name, |s| {
            let p = &mut s.scene.primitives[idx];
            match kind {
                ApplyXform::Location => p.position = Vec3::ZERO,
                ApplyXform::Rotation => p.rotation_deg = Vec3::ZERO,
                ApplyXform::Scale => p.scale = Vec3::ONE,
                ApplyXform::All => {
                    p.position = Vec3::ZERO;
                    p.rotation_deg = Vec3::ZERO;
                    p.scale = Vec3::ONE;
                }
            }
            let pos = p.position;
            let rot = p.rotation_deg;
            let scl = p.scale;
            s.mirror_prim_fields(idx);
            s.write_prim_transform_ex(idx, pos, rot, scl, true);
        });
        self.toast(name);
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
    }

    pub(crate) fn reset_poly_node(&mut self, nid: NodeId, kind: ApplyXform) {
        if self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.locked)
            .unwrap_or(true)
        {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Reset Location",
            ApplyXform::Rotation => "Reset Rotation",
            ApplyXform::Scale => "Reset Scale",
            ApplyXform::All => "Reset Transform",
        };
        self.transaction(name, |s| {
            if let Some(n) = s.scene.scene_graph.get_mut(nid) {
                match kind {
                    ApplyXform::Location => n.local_position = Vec3::ZERO,
                    ApplyXform::Rotation => n.local_rotation = Vec3::ZERO,
                    ApplyXform::Scale => n.local_scale = Vec3::ONE,
                    ApplyXform::All => {
                        n.local_position = Vec3::ZERO;
                        n.local_rotation = Vec3::ZERO;
                        n.local_scale = Vec3::ONE;
                    }
                }
            }
        });
        self.toast(name);
        self.mark_transform_dirty();
    }

    pub(crate) fn reset_sdf_body(&mut self, nid: NodeId, kind: ApplyXform) {
        if self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.locked)
            .unwrap_or(true)
        {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Reset Body Location",
            ApplyXform::Rotation => "Reset Body Rotation",
            ApplyXform::Scale => "Reset Body Scale",
            ApplyXform::All => "Reset Body Transform",
        };
        self.transaction(name, |s| {
            if let Some(n) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::SdfBody {
                    origin,
                    body_rotation,
                    body_scale,
                    ..
                } = &mut n.payload
                {
                    match kind {
                        ApplyXform::Location => {
                            *origin = Vec3::ZERO;
                            n.local_position = Vec3::ZERO;
                        }
                        ApplyXform::Rotation => {
                            *body_rotation = Quat::IDENTITY;
                            n.local_rotation = Vec3::ZERO;
                        }
                        ApplyXform::Scale => {
                            *body_scale = Vec3::ONE;
                            n.local_scale = Vec3::ONE;
                        }
                        ApplyXform::All => {
                            *origin = Vec3::ZERO;
                            *body_rotation = Quat::IDENTITY;
                            *body_scale = Vec3::ONE;
                            n.local_position = Vec3::ZERO;
                            n.local_rotation = Vec3::ZERO;
                            n.local_scale = Vec3::ONE;
                        }
                    }
                }
            }
        });
        self.toast(name);
        self.mark_transform_dirty();
    }

    // ── Poly freeze (mesh bake) — was apply_transform_poly ──

    pub(crate) fn apply_transform_poly(&mut self, nid: NodeId, kind: ApplyXform) {
        let locked = self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.locked)
            .unwrap_or(true);
        if locked {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Freeze Location",
            ApplyXform::Rotation => "Freeze Rotation",
            ApplyXform::Scale => "Freeze Scale",
            ApplyXform::All => "Freeze All Transforms",
        };
        let mut ok = false;
        self.transaction(name, |s| {
            let Some(node) = s.scene.scene_graph.get_mut(nid) else {
                return;
            };
            let pos = node.local_position;
            let rot_e = node.local_rotation;
            let scl = node.local_scale;
            let rot = euler_to_quat(rot_e);
            let mat = match kind {
                ApplyXform::Location => Mat4::from_translation(pos),
                ApplyXform::Rotation => Mat4::from_quat(rot),
                ApplyXform::Scale => Mat4::from_scale(scl),
                ApplyXform::All => Mat4::from_scale_rotation_translation(scl, rot, pos),
            };
            let NodePayload::PolygonalBody {
                mesh,
                mesh_cache_id,
                ..
            } = &mut node.payload
            else {
                return;
            };
            {
                let m = std::sync::Arc::make_mut(mesh);
                for v in &mut m.vertices {
                    v.position = mat.transform_point3(v.position);
                }
                m.recompute_normals();
            }
            *mesh_cache_id = None;
            match kind {
                ApplyXform::Location => node.local_position = Vec3::ZERO,
                ApplyXform::Rotation => node.local_rotation = Vec3::ZERO,
                ApplyXform::Scale => node.local_scale = Vec3::ONE,
                ApplyXform::All => {
                    node.local_position = Vec3::ZERO;
                    node.local_rotation = Vec3::ZERO;
                    node.local_scale = Vec3::ONE;
                }
            }
            ok = true;
        });
        if ok {
            self.toast(name);
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        }
    }

    // ── SDF body freeze ──

    pub(crate) fn apply_transform_sdf_body(&mut self, nid: NodeId, kind: ApplyXform) {
        let locked = self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| n.locked)
            .unwrap_or(true);
        if locked {
            self.toast("Object is locked");
            return;
        }
        let name = match kind {
            ApplyXform::Location => "Freeze Body Location",
            ApplyXform::Rotation => "Freeze Body Rotation",
            ApplyXform::Scale => "Freeze Body Scale",
            ApplyXform::All => "Freeze Body Transforms",
        };
        let mut count = 0usize;
        self.transaction(name, |s| {
            let Some(node) = s.scene.scene_graph.get_mut(nid) else {
                return;
            };
            let NodePayload::SdfBody {
                primitives,
                origin,
                body_rotation,
                body_scale,
                ..
            } = &mut node.payload
            else {
                return;
            };
            let o = *origin;
            let br = *body_rotation;
            let bs = *body_scale;
            count = primitives.len();
            match kind {
                ApplyXform::Location => {
                    for p in primitives.iter_mut() {
                        p.position += o;
                    }
                    *origin = Vec3::ZERO;
                    node.local_position = Vec3::ZERO;
                }
                ApplyXform::Rotation => {
                    for p in primitives.iter_mut() {
                        p.position = br * p.position;
                        p.rotation_deg = quat_to_euler(br * euler_to_quat(p.rotation_deg));
                    }
                    *body_rotation = Quat::IDENTITY;
                    node.local_rotation = Vec3::ZERO;
                }
                ApplyXform::Scale => {
                    for p in primitives.iter_mut() {
                        p.position *= bs;
                        let sc = p.scale * bs;
                        let _ = bake_scale_into_shape(&mut p.shape, sc);
                        p.elongation *= sc.abs();
                        p.scale = Vec3::ONE;
                    }
                    *body_scale = Vec3::ONE;
                    node.local_scale = Vec3::ONE;
                }
                ApplyXform::All => {
                    for p in primitives.iter_mut() {
                        let scaled = p.position * bs;
                        p.position = br * scaled + o;
                        p.rotation_deg = quat_to_euler(br * euler_to_quat(p.rotation_deg));
                        let sc = p.scale * bs;
                        let _ = bake_scale_into_shape(&mut p.shape, sc);
                        p.elongation *= sc.abs();
                        p.scale = Vec3::ONE;
                    }
                    *origin = Vec3::ZERO;
                    *body_rotation = Quat::IDENTITY;
                    *body_scale = Vec3::ONE;
                    node.local_position = Vec3::ZERO;
                    node.local_rotation = Vec3::ZERO;
                    node.local_scale = Vec3::ONE;
                }
            }
            s.sync_flat_cache();
        });
        if count > 0 {
            self.toast(name);
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        }
    }

    // ── Pivot center actions (yellow dot menu) ──

    pub(crate) fn pivot_center_to_mass(&mut self) {
        let Some(nid) = self.resolve_pivot_target_nid() else {
            self.toast("Select an object first");
            return;
        };
        // AABB center ≈ mass for uniform solids
        let center = self.compute_node_world_center(nid);
        self.transaction("Pivot: Center to Mass", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                node.pivot.position = center;
                node.pivot.mode = core_math::scene_graph::PivotMode::BoundingCenter;
            }
        });
        self.ui.expose_selection_pivot = true;
        self.toast("Pivot → mass / bounds center");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn pivot_center_to_origin(&mut self) {
        let Some(nid) = self.resolve_pivot_target_nid() else {
            self.toast("Select an object first");
            return;
        };
        self.transaction("Pivot: Center to Origin", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                node.pivot.position = Vec3::ZERO;
                node.pivot.mode = core_math::scene_graph::PivotMode::WorldOrigin;
            }
        });
        self.ui.expose_selection_pivot = true;
        self.toast("Pivot → world origin");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn pivot_center_to_workplane(&mut self) {
        let Some(nid) = self.resolve_pivot_target_nid() else {
            self.toast("Select an object first");
            return;
        };
        let wp = self.viewport.wp_resolved.origin;
        self.transaction("Pivot: Center to Workplane", |s| {
            s.set_node_custom_pivot_world(nid, wp);
        });
        self.ui.expose_selection_pivot = true;
        self.toast("Pivot → workplane origin");
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn pivot_reset_to_bounds(&mut self) {
        self.pivot_center_to_mass();
    }

    fn compute_node_world_center(&self, nid: NodeId) -> Vec3 {
        // Poly mesh AABB, SDF body bounds, etc. — never silently use transform origin alone.
        self.node_geometry_center_world(nid).unwrap_or_else(|| {
            let prims = self.scene.scene_graph.body_primitives(nid).unwrap_or(&[]);
            if prims.is_empty() {
                return self
                    .scene
                    .scene_graph
                    .body_origin(nid)
                    .unwrap_or(Vec3::ZERO);
            }
            let mut c = Vec3::ZERO;
            for p in prims {
                c += p.position;
            }
            c / prims.len() as f32
        })
    }
}
