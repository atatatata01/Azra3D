//! Extract Intersection — freeze a Subtract/Intersect solid into a **new MeshVolume** body.
//!
//! Flow: select a Subtract/Intersect cutter → auto-find overlapping base →
//! bake targets∩cutter as one `MeshVolume` on a new root body (visually tight).
//! Default: original cutter becomes **Subtract** (piece workflow). Optional remove cutter.
//!
//! Parametric stack copy is gone for prim cutters (user request 2026-07-21). Sub-body cutters
//! still nest a clone (freeze of nested scopes is a later pass).

use crate::*;
use core_math::primitives::{BooleanOp, PbrMaterial};
use glam::Vec3;

/// Ops that define a solid region we can “extract” into a new body.
fn is_extractable_cutter_op(op: &BooleanOp) -> bool {
    matches!(
        op,
        BooleanOp::Subtract { .. }
            | BooleanOp::ChamferSubtract { .. }
            | BooleanOp::Intersect
            | BooleanOp::SmoothIntersect { .. }
    )
}

fn cutter_blend_k(op: &BooleanOp) -> f32 {
    match op {
        BooleanOp::Subtract { k }
        | BooleanOp::ChamferSubtract { k }
        | BooleanOp::SmoothIntersect { k }
        | BooleanOp::Union { k }
        | BooleanOp::ChamferUnion { k } => *k,
        _ => 0.0,
    }
}

impl AzraApp {
    /// Extract the solid defined by a Subtract or Intersect cutter into a new root body.
    pub(crate) fn extract_intersection(&mut self) {
        let settings = self.tool.extract_intersection.clone();

        enum Cutter {
            Prim {
                flat_idx: usize,
                body: core_math::scene_graph::NodeId,
            },
            Body {
                nid: core_math::scene_graph::NodeId,
                parent: core_math::scene_graph::NodeId,
            },
        }

        let cutter = if let Some(idx) = self
            .selected_prim_indices_unified()
            .into_iter()
            .find(|&i| {
                i < self.scene.primitives.len()
                    && is_extractable_cutter_op(&self.scene.primitives[i].operation)
            })
        {
            let body = self
                .scene
                .prim_body_map
                .get(idx)
                .map(|(b, _)| *b)
                .unwrap_or(self.scene.active_body_nid);
            Cutter::Prim {
                flat_idx: idx,
                body,
            }
        } else {
            let body_nid = self.sel.usel.selected_nodes().find(|&nid| {
                self.scene.scene_graph.get(nid).map_or(false, |n| {
                    matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. })
                }) && self
                    .scene
                    .scene_graph
                    .body_op(nid)
                    .map_or(false, is_extractable_cutter_op)
            });
            match body_nid {
                Some(nid) => {
                    let parent = self
                        .scene
                        .scene_graph
                        .get(nid)
                        .and_then(|n| n.parent)
                        .unwrap_or(self.scene.scene_graph.root());
                    Cutter::Body { nid, parent }
                }
                None => {
                    self.toast(
                        "Extract Intersection: select a Subtract or Intersect prim / sub-body",
                    );
                    return;
                }
            }
        };

        let (cutter_aabb, scope_body, cutter_prim_uid, cutter_body_nid) = match &cutter {
            Cutter::Prim { flat_idx, body } => {
                let aabb = self.prim_world_aabb(*flat_idx);
                let uid = self.scene.primitives[*flat_idx].prim_uid;
                (aabb, *body, Some(uid), None)
            }
            Cutter::Body { nid, parent } => {
                let aabb = self.body_world_aabb(*nid);
                (aabb, *parent, None, Some(*nid))
            }
        };

        let mut target_idxs: Vec<usize> = Vec::new();
        for i in 0..self.scene.primitives.len() {
            if Some(self.scene.primitives[i].prim_uid) == cutter_prim_uid {
                continue;
            }
            if let Some(cbid) = cutter_body_nid {
                if self.scene.prim_body_map.get(i).map(|(b, _)| *b) == Some(cbid) {
                    continue;
                }
            }
            if !settings.geometric_all {
                let bi = self.scene.prim_body_map.get(i).map(|(b, _)| *b);
                if bi != Some(scope_body) {
                    continue;
                }
            }
            if !self.prim_world_aabb_overlaps(i, cutter_aabb) {
                continue;
            }
            target_idxs.push(i);
        }

        let additive: Vec<usize> = target_idxs
            .iter()
            .copied()
            .filter(|&i| self.scene.primitives[i].operation.is_additive())
            .collect();
        if !additive.is_empty() {
            target_idxs = additive;
        }
        if target_idxs.is_empty() && !settings.geometric_all {
            for i in 0..self.scene.primitives.len() {
                if Some(self.scene.primitives[i].prim_uid) == cutter_prim_uid {
                    continue;
                }
                if self.scene.prim_body_map.get(i).map(|(b, _)| *b) != Some(scope_body) {
                    continue;
                }
                if let Some(cbid) = cutter_body_nid {
                    if self.scene.prim_body_map.get(i).map(|(b, _)| *b) == Some(cbid) {
                        continue;
                    }
                }
                target_idxs.push(i);
            }
            if !target_idxs.is_empty() {
                self.toast("Extract: no overlap found — using full body stack");
            }
        }
        if target_idxs.is_empty() {
            self.toast("Extract Intersection: nothing overlaps this cutter");
            return;
        }

        let cutter_prim_clone = match &cutter {
            Cutter::Prim { flat_idx, .. } => Some(self.scene.primitives[*flat_idx].clone()),
            Cutter::Body { .. } => None,
        };
        let cutter_body_src = match &cutter {
            Cutter::Body { nid, .. } => Some(*nid),
            _ => None,
        };
        let remove_cutter = settings.remove_cutter;
        let keep_material = settings.keep_material;
        let target_uids: Vec<u32> = target_idxs
            .iter()
            .filter_map(|&i| self.scene.primitives.get(i).map(|p| p.prim_uid))
            .collect();

        // ── Prim cutter: freeze to MeshVolume (tight solid) ──
        if let Some(mut cut) = cutter_prim_clone {
            // Build evaluation stack in world space (same recipe as old parametric extract).
            let mut freeze_list: Vec<core_math::primitives::SdfPrimitive> = Vec::new();
            let mut mat = PbrMaterial::default();
            let mut mat_id = core_math::primitives::MaterialId::NONE;
            for uid in &target_uids {
                let Some(idx) = self.prim_index_by_uid(*uid) else {
                    continue;
                };
                let mut p = self.scene.primitives[idx].clone();
                if p.operation.is_additive() {
                    mat = p.material.clone();
                    mat_id = p.material_id;
                }
                let k = match p.operation {
                    BooleanOp::Union { k }
                    | BooleanOp::Subtract { k }
                    | BooleanOp::ChamferUnion { k }
                    | BooleanOp::ChamferSubtract { k }
                    | BooleanOp::SmoothIntersect { k } => k,
                    _ => 0.0,
                };
                p.operation = BooleanOp::Union { k };
                if crate::BODY_LOCAL_PRIMS {
                    if let Some(&(src_body, _)) = self.scene.prim_body_map.get(idx) {
                        if let Some(bt) = self.scene.scene_graph.body_world_transform(src_body) {
                            p.position = bt.local_to_world(p.position);
                            let world_q = (bt.rotation * p.rotation_quat()).normalize();
                            let (y, x, z) = world_q.to_euler(glam::EulerRot::YXZ);
                            p.rotation_deg =
                                Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                            p.scale = bt.scale * p.scale;
                        }
                    }
                }
                freeze_list.push(p);
            }
            let k = cutter_blend_k(&cut.operation);
            cut.operation = if k > 1e-6 {
                BooleanOp::SmoothIntersect { k }
            } else {
                BooleanOp::Intersect
            };
            if crate::BODY_LOCAL_PRIMS {
                if let Some(bt) = self.scene.scene_graph.body_world_transform(scope_body) {
                    cut.position = bt.local_to_world(cut.position);
                    let world_q = (bt.rotation * cut.rotation_quat()).normalize();
                    let (y, x, z) = world_q.to_euler(glam::EulerRot::YXZ);
                    cut.rotation_deg =
                        Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                    cut.scale = bt.scale * cut.scale;
                }
            }
            freeze_list.push(cut);

            if !keep_material {
                mat = PbrMaterial::default();
                mat_id = core_math::primitives::MaterialId::NONE;
            }

            let t0 = std::time::Instant::now();
            let Some(grid) =
                Self::freeze_prims_to_mesh_sdf(&freeze_list, crate::stack_sdf::STACK_FREEZE_RES)
            else {
                self.toast("Extract: freeze failed (empty bounds)");
                return;
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;

            self.transaction("Extract Intersection", |s| {
                let name = format!("Extract.{:03}", s.scene.next_node_name_id);
                s.scene.next_node_name_id += 1;
                let root = s.scene.scene_graph.root();
                let new_body = s.scene.scene_graph.add_child(
                    root,
                    core_math::scene_graph::NodePayload::SdfBody {
                        name: name.clone(),
                        primitives: Vec::new(),
                        mesh_cache_id: None,
                        body_op: BooleanOp::Union { k: 0.0 },
                        origin: Vec3::ZERO,
                        body_rotation: glam::Quat::IDENTITY,
                        body_scale: Vec3::ONE,
                        mirror_flags: 0,
                        mirror_sign: 0,
                        mirror_center: Vec3::ZERO,
                        bevel_radius: 0.0,
                        blend_profile: core_math::primitives::BlendProfile::default(),
                        material: PbrMaterial::default(),
                        child_order: Vec::new(),
                        mesh_modifiers: Vec::new(),
                        field_modifiers: Vec::new(),
                    },
                );

                let solid = s.mesh_volume_prim_from_grid(
                    &format!("{name}.solid"),
                    grid,
                    mat,
                    mat_id,
                );
                let saved = s.scene.active_body_nid;
                s.scene.active_body_nid = new_body;
                s.add_prim_to_graph(solid);
                s.scene.active_body_nid = saved;

                // Original cutter: Subtract (hole) or remove.
                if remove_cutter {
                    if let Some(uid) = cutter_prim_uid {
                        if let Some(idx) = s.prim_index_by_uid(uid) {
                            s.remove_prim_from_graph(idx);
                        }
                    }
                } else if let Some(uid) = cutter_prim_uid {
                    if let Some(idx) = s.prim_index_by_uid(uid) {
                        let k = cutter_blend_k(&s.scene.primitives[idx].operation);
                        s.scene.primitives[idx].operation = BooleanOp::Subtract { k };
                        s.mirror_prim_to_graph(idx);
                    }
                }

                s.scene.active_body_nid = new_body;
                s.sync_flat_cache();
                s.select_single_unified(SceneItemId::Node(new_body));
                s.mark_node_body_vol_dirty(new_body);
                s.mark_node_body_vol_dirty(scope_body);
                s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
                s.mark_outliner_dirty();
                s.rebake_no_undo();
                let piece_note = if remove_cutter {
                    "cutter removed"
                } else {
                    "original → Subtract (piece cut)"
                };
                let fr = crate::stack_sdf::STACK_FREEZE_RES;
                s.toast(&format!(
                    "Extracted MeshVolume → {name} ({} target(s), {fr}³, {ms:.0}ms, {piece_note})",
                    target_uids.len(),
                ));
                eprintln!(
                    "[Extract] MeshVolume freeze {fr}³ in {ms:.0}ms targets={}",
                    target_uids.len(),
                );
            });
            return;
        }

        // ── Sub-body cutter: keep parametric nest (freeze of nested scopes later) ──
        self.transaction("Extract Intersection", |s| {
            let name = format!("Extract.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let root = s.scene.scene_graph.root();
            let new_body = s.scene.scene_graph.add_child(
                root,
                core_math::scene_graph::NodePayload::SdfBody {
                    name: name.clone(),
                    primitives: Vec::new(),
                    mesh_cache_id: None,
                    body_op: BooleanOp::Union { k: 0.0 },
                    origin: Vec3::ZERO,
                    body_rotation: glam::Quat::IDENTITY,
                    body_scale: Vec3::ONE,
                    mirror_flags: 0,
                    mirror_sign: 0,
                    mirror_center: Vec3::ZERO,
                    bevel_radius: 0.0,
                    blend_profile: core_math::primitives::BlendProfile::default(),
                    material: PbrMaterial::default(),
                    child_order: Vec::new(),
                    mesh_modifiers: Vec::new(),
                    field_modifiers: Vec::new(),
                },
            );

            s.scene.active_body_nid = new_body;
            for uid in &target_uids {
                let Some(idx) = s.prim_index_by_uid(*uid) else {
                    continue;
                };
                let mut p = s.scene.primitives[idx].clone();
                p.prim_uid = s.scene.next_prim_id;
                s.scene.next_prim_id += 1;
                p.instance_master_id = None;
                let k = match p.operation {
                    BooleanOp::Union { k }
                    | BooleanOp::Subtract { k }
                    | BooleanOp::ChamferUnion { k }
                    | BooleanOp::ChamferSubtract { k }
                    | BooleanOp::SmoothIntersect { k } => k,
                    _ => 0.0,
                };
                p.operation = BooleanOp::Union { k };
                if !keep_material {
                    p.material = PbrMaterial::default();
                    p.material_id = core_math::primitives::MaterialId::NONE;
                }
                if crate::BODY_LOCAL_PRIMS {
                    if let Some(&(src_body, _)) = s.scene.prim_body_map.get(idx) {
                        if let Some(bt) = s.scene.scene_graph.body_world_transform(src_body) {
                            p.position = bt.local_to_world(p.position);
                            let world_q = (bt.rotation * p.rotation_quat()).normalize();
                            let (y, x, z) = world_q.to_euler(glam::EulerRot::YXZ);
                            p.rotation_deg =
                                Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
                            p.scale = bt.scale * p.scale;
                        }
                    }
                }
                s.add_prim_to_graph(p);
            }

            if let Some(src_body) = cutter_body_src {
                if let Some(clone_nid) = s.duplicate_subtree(src_body, new_body) {
                    s.scene
                        .scene_graph
                        .set_body_op(clone_nid, BooleanOp::Intersect);
                    if let Some(n) = s.scene.scene_graph.get_mut(clone_nid) {
                        n.visible = true;
                    }
                }
            }

            if remove_cutter {
                if let Some(nid) = cutter_body_src {
                    s.scene.scene_graph.remove(nid);
                }
            } else if let Some(nid) = cutter_body_src {
                let k = s
                    .scene
                    .scene_graph
                    .body_op(nid)
                    .map(|op| cutter_blend_k(&op))
                    .unwrap_or(0.0);
                s.scene
                    .scene_graph
                    .set_body_op(nid, BooleanOp::Subtract { k });
            }

            s.scene.active_body_nid = new_body;
            s.sync_flat_cache();
            s.select_single_unified(SceneItemId::Node(new_body));
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            s.rebake_no_undo();
            s.toast(&format!(
                "Extracted sub-body piece → {name} ({} target(s); nest Intersect)",
                target_uids.len()
            ));
        });
    }

    // --- AABB helpers (world space) -------------------------------------------

    fn prim_world_aabb(&self, flat_idx: usize) -> (Vec3, Vec3) {
        let p = &self.scene.primitives[flat_idx];
        let he = p.bounding_aabb_half_extents();
        let (c, he_w) = if crate::BODY_LOCAL_PRIMS {
            if let Some(&(body, _)) = self.scene.prim_body_map.get(flat_idx) {
                if let Some(bt) = self.scene.scene_graph.body_world_transform(body) {
                    let c = bt.local_to_world(p.position);
                    let r = (he * p.scale.abs() * bt.scale.abs()).length();
                    return (c - Vec3::splat(r), c + Vec3::splat(r));
                }
            }
            (p.position, he * p.scale.abs())
        } else {
            (p.position, he * p.scale.abs())
        };
        (c - he_w, c + he_w)
    }

    fn body_world_aabb(&self, nid: core_math::scene_graph::NodeId) -> (Vec3, Vec3) {
        let mut bmin = Vec3::splat(f32::MAX);
        let mut bmax = Vec3::splat(f32::MIN);
        let mut any = false;
        for (i, (b, _)) in self.scene.prim_body_map.iter().enumerate() {
            if *b != nid {
                // Also include nested under this body.
                let mut cur = *b;
                let mut under = false;
                while let Some(p) = self.scene.scene_graph.get(cur).and_then(|n| n.parent) {
                    if p == nid {
                        under = true;
                        break;
                    }
                    cur = p;
                }
                if !under && *b != nid {
                    continue;
                }
            }
            let (mn, mx) = self.prim_world_aabb(i);
            bmin = bmin.min(mn);
            bmax = bmax.max(mx);
            any = true;
        }
        if !any {
            if let Some(bt) = self.scene.scene_graph.body_world_transform(nid) {
                return (
                    bt.origin - Vec3::splat(0.5),
                    bt.origin + Vec3::splat(0.5),
                );
            }
            return (Vec3::splat(-0.5), Vec3::splat(0.5));
        }
        (bmin, bmax)
    }

    fn prim_world_aabb_overlaps(&self, flat_idx: usize, other: (Vec3, Vec3)) -> bool {
        let (a0, a1) = self.prim_world_aabb(flat_idx);
        let (b0, b1) = other;
        a0.x <= b1.x
            && a1.x >= b0.x
            && a0.y <= b1.y
            && a1.y >= b0.y
            && a0.z <= b1.z
            && a1.z >= b0.z
    }
}
