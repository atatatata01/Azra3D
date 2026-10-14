//! Per-face material assignment for a poly boolean result (+ its post-hoc redo panel state).
//!
//! # Why this exists (searched first — what already existed)
//! `HalfEdgeMesh::from_polygons` (`core_math/src/half_edge/builders.rs:623`) hardcodes
//! `material_id: 0` on every face it allocates, and the boolean rebuilds its result through it.
//! Two consequences, both measured:
//!   * the LEGACY BSP kernel (`AZRA_BSP_BOOLEAN=1`) returns 24/24 faces at material 0 — total loss;
//!   * the DEFAULT Manifold kernel already repairs this with a nearest-input-face transfer
//!     (`mesh_boolean_manifold::transfer_materials_from_operands`), so the default path did
//!     carry materials — but with NO notion of target-vs-cutter and NO way to revisit it.
//! The app-layer app path had its own leak too: `poly::boolean::mesh_from_faces` (the edit-mode
//! island cutter extraction) rebuilt through `from_polygons` and dropped materials before the
//! boolean ever ran — fixed in place, the `merge_mesh_list` carry pattern.
//! This module supplies what no existing implementation could: the target/cutter SPLIT, the
//! policy over the cutter's faces, and post-hoc adjustability.
//!
//! # The rule
//! A result face is assigned the material of the INPUT face whose surface it lies on:
//!   * face from operand A (the target)  → the target face it sits on
//!   * face from operand B (the cutter)  → policy, see [`BoolCutterMaterial`]
//! Which operand a face belongs to is decided by `BoolProvenance` (already collected by
//! `mesh_boolean_with_provenance`); WHICH face inside that operand is a nearest-surface lookup
//! done here, in the app layer — `core_math/src/mesh_boolean/` is owned by another session and
//! is not touched.
//!
//! # Post-hoc adjustability (owner request)
//! The boolean is destructive, so "let me toggle it afterwards" cannot mean re-running the
//! kernel. Instead every result face keeps BOTH candidate ids ([`FaceMatPlan`]) plus whether it
//! came from the cutter, so flipping [`BoolCutterMaterial`] is a pure re-stamp of
//! `face.material_id` on the installed mesh — no geometry is recomputed, and the choice stays
//! adjustable for as long as the redo panel is open. Same lifecycle as `PolyToSphereAdjust`.

use core_math::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
use core_math::mesh_boolean::{point_triangle_dist_sq, BoolProvenance, FaceOrigin};
use core_math::scene_graph::NodeId;
use glam::Vec3;

/// What happens to the material of the faces the CUTTER contributed to the result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BoolCutterMaterial {
    /// Keep the cutter's own material on the carved surface (default — Blender parity: the
    /// cutter's material slots come across with its geometry).
    Cutter,
    /// Give the carved surface the target's material, so the result reads as one object.
    Target,
    /// Strip it: the carved surface falls back to the default material (`MaterialId::NONE`).
    None,
}

impl BoolCutterMaterial {
    pub(crate) fn label(self) -> &'static str {
        match self {
            BoolCutterMaterial::Cutter => "Cutter material",
            BoolCutterMaterial::Target => "Target material",
            BoolCutterMaterial::None => "No material",
        }
    }
}

impl Default for BoolCutterMaterial {
    fn default() -> Self {
        BoolCutterMaterial::Cutter
    }
}

/// Both candidate materials for ONE result face, so the policy can be re-applied later.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FaceMatPlan {
    /// Material of the input face this result face actually lies on (its own operand).
    pub(crate) own: u32,
    /// Material of the nearest TARGET (operand A) face — the "inherit" candidate.
    pub(crate) inherit: u32,
    /// True when this face's surface came from the cutter (operand B).
    pub(crate) from_cutter: bool,
}

impl FaceMatPlan {
    /// The material this face gets under `mode`.
    pub(crate) fn resolve(&self, mode: BoolCutterMaterial) -> u32 {
        if !self.from_cutter {
            return self.own;
        }
        match mode {
            BoolCutterMaterial::Cutter => self.own,
            BoolCutterMaterial::Target => self.inherit,
            BoolCutterMaterial::None => 0,
        }
    }
}

/// Triangles of a mesh, each tagged with its source face's `material_id`.
fn tris_with_material(m: &HalfEdgeMesh) -> Vec<([Vec3; 3], u32)> {
    let mut out = Vec::new();
    for i in 0..m.faces.len() as u32 {
        let f = FaceId(i);
        if m.face(f).half_edge == NO_EDGE {
            continue;
        }
        let vs = m.face_vertices(f);
        if vs.len() < 3 {
            continue;
        }
        let mid = m.face(f).material_id;
        let p0 = m.vert(vs[0]).position;
        for w in vs.windows(2).skip(1) {
            out.push(([p0, m.vert(w[0]).position, m.vert(w[1]).position], mid));
        }
    }
    out
}

/// Nearest triangle to `p`: `(squared distance, material)`. `None` on an empty list.
fn nearest_material(p: Vec3, tris: &[([Vec3; 3], u32)]) -> Option<(f32, u32)> {
    let mut best: Option<(f32, u32)> = None;
    for (t, mid) in tris {
        let d = point_triangle_dist_sq(p, t[0], t[1], t[2]);
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, *mid));
        }
    }
    best
}

/// Build the per-face material plan for a boolean result.
///
/// `res`, `a` (target) and `b` (cutter) must all be in the SAME space (the boolean's world
/// space) — the caller runs this before transforming the result back into local space, or with
/// all three already consistent. Indexed by `FaceId.0`; freed slots get a harmless default.
pub(crate) fn plan_result_materials(
    res: &HalfEdgeMesh,
    a: &HalfEdgeMesh,
    b: &HalfEdgeMesh,
    prov: &BoolProvenance,
) -> Vec<FaceMatPlan> {
    let ta = tris_with_material(a);
    let tb = tris_with_material(b);
    let mut plan = vec![
        FaceMatPlan { own: 0, inherit: 0, from_cutter: false };
        res.faces.len()
    ];
    for i in 0..res.faces.len() as u32 {
        let f = FaceId(i);
        if res.face(f).half_edge == NO_EDGE {
            continue;
        }
        let vs = res.face_vertices(f);
        if vs.is_empty() {
            continue;
        }
        let c: Vec3 = vs.iter().map(|&v| res.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        let na = nearest_material(c, &ta);
        let nb = nearest_material(c, &tb);
        // Which operand's surface is this? Provenance is authoritative when it has an answer;
        // `Cut` (the intersection rim, or an ambiguous coincident face) and a missing provenance
        // both fall back to "whichever operand's surface is actually closer", which is the same
        // question collect_provenance asks — just without its exclusion bias.
        let from_cutter = match prov.face(f) {
            Some(FaceOrigin::OperandA) => false,
            Some(FaceOrigin::OperandB) => true,
            _ => match (na, nb) {
                (Some((da, _)), Some((db, _))) => db < da,
                (None, Some(_)) => true,
                _ => false,
            },
        };
        let inherit = na.map(|(_, m)| m).unwrap_or(0);
        let own = if from_cutter {
            nb.map(|(_, m)| m).unwrap_or(inherit)
        } else {
            inherit
        };
        plan[i as usize] = FaceMatPlan { own, inherit, from_cutter };
    }
    plan
}

/// Stamp `plan` onto `mesh` under `mode`. Faces beyond `plan`'s length (e.g. the re-appended
/// cutter island on keep-cutter) are left exactly as they are.
pub(crate) fn apply_material_plan(
    mesh: &mut HalfEdgeMesh,
    plan: &[FaceMatPlan],
    mode: BoolCutterMaterial,
) {
    for (i, p) in plan.iter().enumerate() {
        if i >= mesh.faces.len() || mesh.faces[i].half_edge == NO_EDGE {
            continue;
        }
        mesh.faces[i].material_id = p.resolve(mode);
    }
}

/// Redo-panel state for the last boolean: lets the user change the cutter-material policy AFTER
/// the (destructive) op, by re-stamping the stored plan. Mirrors `PolyToSphereAdjust`.
#[derive(Clone)]
pub(crate) struct PolyBooleanAdjust {
    /// The op label, for the panel header + the undo entry name.
    pub(crate) op_label: String,
    /// One entry per body the boolean wrote, with its per-face plan (indices valid on the
    /// INSTALLED mesh).
    pub(crate) targets: Vec<(NodeId, Vec<FaceMatPlan>)>,
    /// Current policy.
    pub(crate) mode: BoolCutterMaterial,
    /// Live change since the last settle — only push undo when dirty.
    pub(crate) needs_settle: bool,
}

// ============================================================
// Redo panel ("Last · Boolean") — Tool Parameters dock + legacy float
// ============================================================

impl crate::AzraApp {
    /// True when the last-boolean panel is still meaningful: at least one target body still
    /// exists AND still has the face count the plan was built for. A later op that rebuilds the
    /// mesh (another boolean, a bevel, cleanup) invalidates the indices — fail CLOSED, drop the
    /// panel, never stamp materials onto strangers.
    pub(crate) fn poly_boolean_adjust_active(&self) -> bool {
        let Some(a) = self.ui.poly_boolean_adjust.as_ref() else {
            return false;
        };
        if !a.targets.iter().any(|(nid, _)| {
            self.poly_node_mesh(*nid).is_some() && self.sel.usel.active_node() == Some(*nid)
        }) {
            return false;
        }
        a.targets.iter().all(|(nid, plan)| match self.poly_node_mesh(*nid) {
            Some(m) => plan.len() <= m.faces.len(),
            None => true, // node deleted (the cutter) — nothing to stamp, not an invalidation
        })
    }

    /// Test hook: the panel's re-stamp, driven without egui (see `diag_bool_material`).
    #[cfg(test)]
    pub(crate) fn poly_boolean_apply_material_mode_for_test(&mut self, mode: BoolCutterMaterial) {
        self.poly_boolean_apply_material_mode(mode);
        if let Some(a) = self.ui.poly_boolean_adjust.as_mut() {
            a.mode = mode;
        }
    }

    /// Re-stamp every stored plan under `mode`. Pure material write — no geometry touched.
    fn poly_boolean_apply_material_mode(&mut self, mode: BoolCutterMaterial) {
        let targets = match self.ui.poly_boolean_adjust.as_ref() {
            Some(a) => a.targets.clone(),
            None => return,
        };
        for (nid, plan) in &targets {
            let Some(old) = self.poly_node_mesh(*nid) else { continue };
            let mut m = (*old).clone();
            apply_material_plan(&mut m, plan, mode);
            if let Some(node) = self.scene.scene_graph.get_mut(*nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                    &mut node.payload
                {
                    *mesh = std::sync::Arc::new(m);
                }
            }
            self.poly_request_display_sync(*nid);
        }
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Shared last-boolean UI + apply (Tool Parameters dock + legacy float).
    /// Returns false when the panel is not applicable (caller falls through).
    pub(crate) fn poly_boolean_adjust_panel(&mut self, ui: &mut eframe::egui::Ui) -> bool {
        use eframe::egui;
        if !self.poly_boolean_adjust_active() {
            self.ui.poly_boolean_adjust = None;
            return false;
        }
        let (header, mut mode) = match self.ui.poly_boolean_adjust.as_ref() {
            Some(a) => (a.op_label.clone(), a.mode),
            None => return false,
        };
        let before = mode;

        ui.label(
            egui::RichText::new(format!("Last · Boolean {header}"))
                .size(11.0)
                .color(egui::Color32::from_rgb(160, 170, 190)),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Cutter faces")
                .size(10.0)
                .color(egui::Color32::from_rgb(140, 148, 166)),
        );
        for m in [
            BoolCutterMaterial::Cutter,
            BoolCutterMaterial::Target,
            BoolCutterMaterial::None,
        ] {
            ui.radio_value(&mut mode, m, m.label());
        }
        ui.small(
            "The boolean itself is destructive — only the material assignment stays adjustable.",
        );
        ui.separator();
        let close = ui.button("Done").clicked();

        if mode != before {
            self.poly_boolean_apply_material_mode(mode);
            self.ui.bool_cutter_material = mode; // sticky for the next boolean
            if let Some(a) = self.ui.poly_boolean_adjust.as_mut() {
                a.mode = mode;
                a.needs_settle = true;
            }
            // A radio click is atomic (no drag) — settle immediately into ONE undo entry.
            self.transaction_snapshot_only("Adjust Boolean Material");
            if let Some(a) = self.ui.poly_boolean_adjust.as_mut() {
                a.needs_settle = false;
            }
        }
        if close {
            self.ui.poly_boolean_adjust = None;
        }
        true
    }

    /// Legacy floating window (old UI only) — mirrors `draw_poly_tosphere_adjust`.
    pub(crate) fn draw_poly_boolean_adjust(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui;
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            return;
        }
        if !self.poly_boolean_adjust_active() {
            return;
        }
        egui::Window::new("Adjust Boolean")
            .id(egui::Id::new("poly_boolean_adjust"))
            .anchor(egui::Align2::LEFT_BOTTOM, [12.0, -12.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let _ = self.poly_boolean_adjust_panel(ui);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_math::mesh_boolean::{mesh_boolean_with_provenance, BoolOp};

    /// Materials must survive a boolean, and the cutter policy must be honoured.
    ///
    /// Subtract a cylinder (material 7) through a cube (material 3). The kernel rebuilds the
    /// result through `from_polygons`, which zeroes every `material_id` — so without
    /// `plan_result_materials` EVERY face comes back 0. Asserts:
    ///   * the cube's untouched walls keep material 3 in all three modes,
    ///   * the hole's barrel is 7 / 3 / 0 under Cutter / Target / None.
    #[test]
    fn boolean_result_keeps_materials_under_every_cutter_policy() {
        let mut cube = core_math::half_edge::HalfEdgeMesh::new_box(glam::Vec3::splat(2.0));
        for f in cube.faces.iter_mut() {
            f.material_id = 3;
        }
        // Taller than the cube so it punches clean through (no cap coplanarity).
        let mut cyl = core_math::half_edge::HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
        for f in cyl.faces.iter_mut() {
            f.material_id = 7;
        }
        let (res, prov) = mesh_boolean_with_provenance(&cube, &cyl, BoolOp::Difference)
            .expect("cube minus cylinder must succeed");
        // NOTE: the DEFAULT (Manifold) kernel already restores materials by nearest input face
        // (`mesh_boolean_manifold::transfer_materials_from_operands`); the LEGACY BSP path
        // (`AZRA_BSP_BOOLEAN=1`) returns every face at material 0. This layer must produce the
        // right answer either way, so the test does NOT assert what the kernel handed back.

        let plan = plan_result_materials(&res, &cube, &cyl, &prov);
        assert_eq!(plan.len(), res.faces.len(), "one plan entry per result face");
        let n_cutter = plan.iter().filter(|p| p.from_cutter).count();
        assert!(n_cutter > 0, "the hole's barrel must be attributed to the cutter");
        assert!(
            plan.iter().any(|p| !p.from_cutter),
            "the cube's untouched walls must be attributed to the target"
        );

        for (mode, expect_cutter) in [
            (BoolCutterMaterial::Cutter, 7u32),
            (BoolCutterMaterial::Target, 3u32),
            (BoolCutterMaterial::None, 0u32),
        ] {
            let mut m = res.clone();
            apply_material_plan(&mut m, &plan, mode);
            for (i, p) in plan.iter().enumerate() {
                if m.faces[i].half_edge == NO_EDGE {
                    continue;
                }
                let got = m.faces[i].material_id;
                if p.from_cutter {
                    assert_eq!(got, expect_cutter, "cutter face {i} under {mode:?}");
                } else {
                    assert_eq!(got, 3, "target face {i} must keep its own material under {mode:?}");
                }
            }
        }
    }
}
