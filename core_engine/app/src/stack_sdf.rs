//! Stack → SDF (merge-down) and shared field-freeze helpers for Extract Intersection.
//!
//! **Stack → SDF:** freeze selected prim + everything *before* it in the body stack into one
//! `MeshVolume` prim (visually tight solid; parametric history of that range is replaced).
//!
//! **Extract Intersection:** uses the same freeze for a single MeshVolume piece in a new body.

use crate::*;
use core_math::primitives::{BooleanOp, MaterialId, PbrMaterial, SdfPrimitive, SdfShape};
use core_math::sdf_shapes::MeshSdfData;
use glam::Vec3;
use std::sync::Arc;

/// Default grid resolution for freeze (cubic). Balance quality vs cost.
pub(crate) const STACK_FREEZE_RES: u32 = 64;
const STACK_FREEZE_RES_MIN: u32 = 32;
const STACK_FREEZE_RES_MAX: u32 = 128;
/// Pad AABB so the iso surface is not clipped at the grid boundary.
const STACK_FREEZE_PAD: f32 = 0.08;

impl AzraApp {
    // ── Shared freeze ─────────────────────────────────────────────────────────

    /// Bake a flat prim stack (common local frame) to `MeshSdfData`.
    /// Evaluated as a body (`BodyField::from_prims`, no body mirror).
    pub(crate) fn freeze_prims_to_mesh_sdf(
        prims: &[SdfPrimitive],
        res: u32,
    ) -> Option<MeshSdfData> {
        if prims.is_empty() {
            return None;
        }
        let res = res.clamp(STACK_FREEZE_RES_MIN, STACK_FREEZE_RES_MAX);
        let mut bmin = Vec3::splat(f32::MAX);
        let mut bmax = Vec3::splat(f32::MIN);
        for p in prims {
            if !p.visible {
                continue;
            }
            let he = p.bounding_aabb_half_extents();
            let ext = he * p.scale.abs() + Vec3::splat(0.02);
            let r = ext.length();
            let c = p.position;
            bmin = bmin.min(c - Vec3::splat(r));
            bmax = bmax.max(c + Vec3::splat(r));
        }
        if !bmin.is_finite() || !bmax.is_finite() || (bmax - bmin).min_element() < 1e-6 {
            return None;
        }
        let pad = (bmax - bmin).max_element() * STACK_FREEZE_PAD + 0.05;
        bmin -= Vec3::splat(pad);
        bmax += Vec3::splat(pad);

        let field = core_math::field_eval::BodyField::from_prims(prims, 0, 0, Vec3::ZERO);
        // Cubic grid (MeshSdfData is isotropic cells).
        let denom = (res.max(2) - 1) as f32;
        let extent = (bmax - bmin).max_element();
        let cell = extent / denom;
        let center = (bmin + bmax) * 0.5;
        let cubic_min = center - Vec3::splat(extent * 0.5);
        let cubic_max = center + Vec3::splat(extent * 0.5);
        let dist =
            crate::bake_utils::bake_body_grid_field(&field, res, cubic_min, cubic_max);
        Some(MeshSdfData {
            res,
            bounds_min: cubic_min,
            cell,
            dist: Arc::new(dist),
        })
    }

    /// Build a Union MeshVolume prim; grid is in body-local space (prim at origin).
    pub(crate) fn mesh_volume_prim_from_grid(
        &mut self,
        name: &str,
        grid: MeshSdfData,
        material: PbrMaterial,
        material_id: MaterialId,
    ) -> SdfPrimitive {
        let uid = self.scene.next_prim_id;
        self.scene.next_prim_id += 1;
        let mut prim = SdfPrimitive::new(
            name.to_string(),
            SdfShape::MeshVolume {
                grid: Arc::new(grid),
                source_poly: None,
            },
            Vec3::ZERO,
        );
        prim.prim_uid = uid;
        prim.operation = BooleanOp::Union { k: 0.0 };
        prim.material = material;
        prim.material_id = material_id;
        prim
    }

    // ── Stack → SDF (standalone merge-down) ───────────────────────────────────

    /// Freeze **selected prim + everything before it** in the same body stack into one MeshVolume.
    pub(crate) fn stack_to_sdf(&mut self) {
        let sel = self.selected_prim_indices_unified();
        if sel.is_empty() {
            self.toast("Stack → SDF: select a prim in the body stack");
            return;
        }
        let body = match crate::scene_actions::shared_prim_body(&self.scene.prim_body_map, &sel) {
            Ok(Some(b)) => b,
            Ok(None) => self.scene.active_body_nid,
            Err(()) => {
                self.toast("Stack → SDF: all selected prims must be in the same body");
                return;
            }
        };

        // Body-local prim indices in stack order (resolve_child_order → Ok(body_idx)).
        let ordered_body_idx: Vec<usize> = self
            .scene
            .scene_graph
            .resolve_child_order(body)
            .into_iter()
            .filter_map(|s| s.ok())
            .collect();
        let body_prims = match self.scene.scene_graph.body_primitives(body) {
            Some(p) if !p.is_empty() => p,
            _ => {
                self.toast("Stack → SDF: empty body");
                return;
            }
        };
        let ordered_body_idx = if ordered_body_idx.is_empty() {
            (0..body_prims.len()).collect()
        } else {
            ordered_body_idx
        };

        // Map selected flat indices → body-local indices; freeze through last selected in order.
        let mut end_ord = 0usize;
        for &fi in &sel {
            let Some(&(_, bidx)) = self.scene.prim_body_map.get(fi) else {
                continue;
            };
            if let Some(pos) = ordered_body_idx.iter().position(|&i| i == bidx) {
                end_ord = end_ord.max(pos);
            }
        }
        let range_body_idx: Vec<usize> = ordered_body_idx[..=end_ord].to_vec();
        let range_uids: Vec<u32> = range_body_idx
            .iter()
            .filter_map(|&bi| body_prims.get(bi).map(|p| p.prim_uid))
            .collect();
        if range_uids.is_empty() {
            self.toast("Stack → SDF: nothing to freeze");
            return;
        }

        let mut freeze_prims: Vec<SdfPrimitive> = Vec::with_capacity(range_uids.len());
        let mut mat = PbrMaterial::default();
        let mut mat_id = MaterialId::NONE;
        for uid in &range_uids {
            let Some(idx) = self.prim_index_by_uid(*uid) else {
                continue;
            };
            let p = self.scene.primitives[idx].clone();
            if p.operation.is_additive() {
                mat = p.material.clone();
                mat_id = p.material_id;
            }
            freeze_prims.push(p);
        }
        if freeze_prims.is_empty() {
            self.toast("Stack → SDF: could not resolve prims");
            return;
        }

        let t0 = std::time::Instant::now();
        let Some(grid) = Self::freeze_prims_to_mesh_sdf(&freeze_prims, STACK_FREEZE_RES) else {
            self.toast("Stack → SDF: freeze failed (empty bounds)");
            return;
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let n = range_uids.len();

        self.transaction("Stack → SDF", |s| {
            let mut flat: Vec<usize> = range_uids
                .iter()
                .filter_map(|uid| s.prim_index_by_uid(*uid))
                .collect();
            flat.sort_unstable();
            flat.dedup();
            s.remove_prims_from_graph(&flat);

            let name = format!("Solid.{:03}", {
                let id = s.scene.next_node_name_id;
                s.scene.next_node_name_id += 1;
                id
            });
            let prim = s.mesh_volume_prim_from_grid(&name, grid, mat, mat_id);
            let saved = s.scene.active_body_nid;
            s.scene.active_body_nid = body;
            let new_idx = s.add_prim_to_graph(prim);
            s.scene.active_body_nid = saved;
            s.sync_flat_cache();
            s.mark_node_body_vol_dirty(body);
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            if new_idx < s.scene.primitives.len() {
                s.select_single(new_idx);
            }
            s.toast(&format!(
                "Stack → SDF: {n} prim(s) → MeshVolume ({STACK_FREEZE_RES}³, {ms:.0}ms)"
            ));
            eprintln!(
                "[StackSdf] body={body:?} froze {n} prims → {STACK_FREEZE_RES}³ in {ms:.0}ms"
            );
        });
    }
}
