//! Instance creation, sync, and management — shared actions.

use crate::*;
use glam::Vec3;

impl AzraApp {
    /// Create a linked instance of the active (or first selected) SDF prim.
    /// Instance keeps its own transform; shape/material follow the master via
    /// `instance_master_id` + `sync_instances_of`.
    ///
    /// Works from Object or SDF Vertex mode (create auto-enters Vertex). Resolves
    /// body-only selection to the first prim in that body so outliner body clicks work.
    pub(crate) fn create_instance(&mut self) {
        // Prefer active prim → selected prims → prims under selected body nodes.
        let master_idx = self
            .active_prim_index_unified()
            .or_else(|| self.selected_prim_indices_unified().into_iter().next())
            .or_else(|| {
                // Body selected in outliner: expand to first prim (same as GPU highlight set).
                self.selected_prim_indices_including_bodies().into_iter().next()
            })
            .filter(|&i| i < self.scene.primitives.len());
        let Some(master_idx) = master_idx else {
            self.toast("Select an SDF primitive to instance");
            return;
        };
        if self.scene.primitives[master_idx].locked {
            self.toast("Object is locked");
            return;
        }
        // Place into the SOURCE prim's body (same as Shift+D), not a stale active_body.
        if master_idx < self.scene.prim_body_map.len() {
            let source_body = self.scene.prim_body_map[master_idx].0;
            if source_body != self.scene.active_body_nid {
                self.scene.active_body_nid = source_body;
            }
        }
        let master_uid = self.scene.primitives[master_idx].prim_uid;
        let master_name = self.scene.primitives[master_idx].name.clone();
        // Same placement as master — user moves the instance.
        let master_pos = self.scene.primitives[master_idx].position;
        let offset = self.tweak.instance_offset;
        // Allocate prim_uid INSIDE the transaction so a failed/cancelled path never
        // burns ids (and so undo restores id counters with the rest of the scene).
        self.transaction("Create Instance", |s| {
            let uid = s.scene.next_prim_id;
            s.scene.next_prim_id += 1;
            let mut inst = s.scene.primitives[master_idx].clone();
            inst.prim_uid = uid;
            inst.instance_master_id = Some(master_uid);
            inst.name = format!("{master_name}.inst");
            inst.position = master_pos + Vec3::new(offset, 0.0, 0.0);
            let new_idx = s.add_prim_to_graph(inst);
            let new_uid = s.scene.primitives[new_idx].prim_uid;
            s.select_single_unified(SceneItemId::Prim(new_uid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.rebake_no_undo();
            s.toast(&format!("Instance created · {master_name}.inst (same place — move it)"));
        });
    }

    /// Push ALL content fields from master onto every linked instance.
    /// Preserves per-instance: rotation, scale, prim_uid, name, instance_master_id, locked.
    ///
    /// `content_pos_delta`: body/world-space position shift that accompanied a **content**
    /// edit on the master (one-sided face extrude / asymmetric elongate recenter). Applied
    /// in each instance's local frame so "grow one face, opposite fixed" matches — without
    /// this, instances only get new half_extents/elongation and grow **both** sides.
    pub(crate) fn sync_instances_of(&mut self, master_uid: u32) {
        self.sync_instances_of_ex(master_uid, Vec3::ZERO);
    }

    pub(crate) fn sync_instances_of_ex(&mut self, master_uid: u32, content_pos_delta: Vec3) {
        let master_idx = match self.prim_index_by_uid(master_uid) {
            Some(i) => i,
            None => return,
        };
        let master = self.scene.primitives[master_idx].clone();
        let indices: Vec<usize> = (0..self.scene.primitives.len())
            .filter(|&i| self.scene.primitives[i].instance_master_id == Some(master_uid))
            .collect();
        if indices.is_empty() {
            return;
        }

        // Convert master's body-space recenter into unit local shape space, then re-apply
        // with each instance's rotation/scale so one-sided grow stays one-sided.
        let master_rot = master.rotation_quat();
        let ms = master.scale.abs().max(Vec3::splat(1e-4));
        let shape_delta = if content_pos_delta.length_squared() > 1e-14 {
            (master_rot.inverse() * content_pos_delta) / ms
        } else {
            Vec3::ZERO
        };

        for i in indices {
            let rot = self.scene.primitives[i].rotation_deg;
            let scale = self.scene.primitives[i].scale;
            let uid = self.scene.primitives[i].prim_uid;
            let name = self.scene.primitives[i].name.clone();
            let locked = self.scene.primitives[i].locked;
            let visible = self.scene.primitives[i].visible;
            let layer_id = self.scene.primitives[i].layer_id;
            let object_id = self.scene.primitives[i].object_id;
            let inst_rot = self.scene.primitives[i].rotation_quat();
            let is = scale.abs().max(Vec3::splat(1e-4));
            let pos = self.scene.primitives[i].position + inst_rot * (shape_delta * is);

            // Full content clone — shape (incl. box bevel), elongation, modifiers, materials…
            let mut p = master.clone();
            p.position = pos;
            p.rotation_deg = rot;
            p.scale = scale;
            p.prim_uid = uid;
            p.name = name;
            p.instance_master_id = Some(master_uid);
            p.locked = locked;
            p.visible = visible;
            p.layer_id = layer_id;
            p.object_id = object_id;
            self.scene.primitives[i] = p;
            self.mirror_prim_to_graph(i);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Break instance link (SDF “instance → independent”). Same prim data, no longer follows master.
    pub(crate) fn make_instance_unique(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() { return; }
        // Already unique → no empty undo.
        if self.scene.primitives[idx].instance_master_id.is_none() { return; }
        self.transaction("Break instance link", |s| {
            s.write_prim(idx, |p| p.instance_master_id = None);
            s.mark_dirty(DirtyFlags::SCENE);
            s.toast("Instance → independent (link broken)");
        });
    }

    /// Break link for every selected instance prim (and active if instance).
    pub(crate) fn make_selected_instances_unique(&mut self) {
        let mut idxs: Vec<usize> = self
            .selected_prim_indices_unified()
            .into_iter()
            .filter(|&i| {
                i < self.scene.primitives.len()
                    && self.scene.primitives[i].instance_master_id.is_some()
            })
            .collect();
        if idxs.is_empty() {
            if let Some(i) = self.active_prim_index_unified() {
                if i < self.scene.primitives.len()
                    && self.scene.primitives[i].instance_master_id.is_some()
                {
                    idxs.push(i);
                }
            }
        }
        if idxs.is_empty() {
            self.toast("Select linked instance prims to break");
            return;
        }
        self.transaction("Break instance links", |s| {
            let mut n = 0usize;
            for idx in idxs {
                if idx < s.scene.primitives.len()
                    && s.scene.primitives[idx].instance_master_id.is_some()
                {
                    s.write_prim(idx, |p| p.instance_master_id = None);
                    n += 1;
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
            s.toast(&format!("Broke link on {n} instance(s) → independent"));
        });
    }

    /// Instance the selected prim into **every other top-level SDF body**, keeping boolean type.
    /// Clones are linked (`instance_master_id`) so shape/material/boolean follow the master.
    /// Per-instance transform is free (position converted into each body-local frame).
    pub(crate) fn instance_into_all_bodies(&mut self) {
        let master_idx = self
            .active_prim_index_unified()
            .or_else(|| self.selected_prim_indices_unified().into_iter().next())
            .or_else(|| self.selected_prim_indices_including_bodies().into_iter().next())
            .filter(|&i| i < self.scene.primitives.len());
        let Some(master_idx) = master_idx else {
            self.toast("Select an SDF primitive to instance into all bodies");
            return;
        };
        if self.scene.primitives[master_idx].locked {
            self.toast("Object is locked");
            return;
        }

        // If the selection is itself an instance, link all new copies to its master
        // (one shared source of truth).
        let content_idx = self.resolve_content_edit_idx(master_idx);
        let master_uid = self.scene.primitives[content_idx].prim_uid;
        let master_name = self.scene.primitives[content_idx].name.clone();
        let master_op = self.scene.primitives[content_idx].operation.clone();
        let master_pos_local = self.scene.primitives[content_idx].position;
        let master_rot = self.scene.primitives[content_idx].rotation_deg;
        let master_scale = self.scene.primitives[content_idx].scale;

        let source_body = self
            .scene
            .prim_body_map
            .get(content_idx)
            .map(|(b, _)| *b)
            .unwrap_or(self.scene.active_body_nid);

        // World position of master so placement is consistent across differently placed bodies.
        let master_world = if crate::BODY_LOCAL_PRIMS {
            self.scene
                .scene_graph
                .effective_body_transform(source_body)
                .map(|m| m.transform_point3(master_pos_local))
                .unwrap_or(master_pos_local)
        } else {
            master_pos_local
        };

        // All top-level bodies except those that already own the master or an instance of it.
        let mut target_bodies: Vec<core_math::scene_graph::NodeId> = Vec::new();
        for (nid, _) in self.scene.scene_graph.sdf_bodies() {
            if self.scene.scene_graph.is_sub_body(nid) {
                continue;
            }
            if nid == source_body {
                continue;
            }
            // Skip bodies that already have this master or a linked instance.
            let already = self.scene.prim_body_map.iter().enumerate().any(|(i, (b, _))| {
                *b == nid
                    && i < self.scene.primitives.len()
                    && (self.scene.primitives[i].prim_uid == master_uid
                        || self.scene.primitives[i].instance_master_id == Some(master_uid))
            });
            if already {
                continue;
            }
            target_bodies.push(nid);
        }

        if target_bodies.is_empty() {
            self.toast("No other top-level bodies need an instance (or all already have one)");
            return;
        }

        let n_targets = target_bodies.len();
        self.transaction("Instance into all bodies", |s| {
            let mut created = 0usize;
            for body in target_bodies {
                // Place into this body
                s.scene.active_body_nid = body;
                let uid = s.scene.next_prim_id;
                s.scene.next_prim_id += 1;
                // Fresh clone from master content (shape + boolean + material…)
                let master_i = match s.prim_index_by_uid(master_uid) {
                    Some(i) => i,
                    None => continue,
                };
                let mut inst = s.scene.primitives[master_i].clone();
                inst.prim_uid = uid;
                inst.instance_master_id = Some(master_uid);
                inst.name = format!("{master_name}.inst");
                // Body-local position matching master's world placement.
                inst.position = if crate::BODY_LOCAL_PRIMS {
                    s.scene
                        .scene_graph
                        .effective_body_transform(body)
                        .map(|m| m.inverse().transform_point3(master_world))
                        .unwrap_or(master_pos_local)
                } else {
                    master_world
                };
                // Keep master's orientation/scale as starting pose (user can edit per instance).
                inst.rotation_deg = master_rot;
                inst.scale = master_scale;
                // Explicit boolean retain (clone already has it; reaffirm for clarity).
                inst.operation = master_op.clone();
                s.add_prim_to_graph(inst);
                created += 1;
            }
            // Restore active body to source
            s.scene.active_body_nid = source_body;
            s.sync_flat_cache();
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            s.rebake_no_undo();
            s.toast(&format!(
                "Instanced “{master_name}” into {created}/{n_targets} bodies (linked · same boolean)"
            ));
        });
    }

    pub(crate) fn is_master(&self, idx: usize) -> bool {
        if idx >= self.scene.primitives.len() {
            return false;
        }
        let uid = self.scene.primitives[idx].prim_uid;
        self.scene
            .primitives
            .iter()
            .any(|p| p.instance_master_id == Some(uid))
    }

    /// Content edits (bevel edges, materials, …) should target the **master**.
    /// Selecting an instance and dragging a bevel still mutates the master so all
    /// linked copies stay identical.
    pub(crate) fn resolve_content_edit_idx(&self, idx: usize) -> usize {
        if idx >= self.scene.primitives.len() {
            return idx;
        }
        if let Some(mid) = self.scene.primitives[idx].instance_master_id {
            if let Some(mi) = self.prim_index_by_uid(mid) {
                return mi;
            }
        }
        idx
    }

    /// After a content mutation that bypassed `write_prim` (bevel drag, etc.).
    pub(crate) fn sync_instances_if_master(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        if self.is_master(idx) {
            let uid = self.scene.primitives[idx].prim_uid;
            self.sync_instances_of(uid);
        }
    }
}
