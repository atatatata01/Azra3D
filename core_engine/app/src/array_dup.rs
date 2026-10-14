//! Shift+D array session — gizmo-driven duplicate with tool params.
//!
//! **SDF prims** and **poly bodies** share one session type.
//! - Scroll = array count (linear or circular)
//! - C = toggle circular vs linear
//! - I = toggle instance (share mesh Arc with source)
//! - M = toggle "as Array modifier" (Enter applies mod instead of real copies)
//! - Enter / ✓ = confirm · Space / Esc = cancel

use crate::*;
use core_math::primitives::ModifierType;
use core_math::scene_graph::{NodeId, NodePayload, PolyModifier, PolyModifierType};
use eframe::egui;
use glam::Vec3;

impl AzraApp {
    /// True when a Shift+D array session is live (prim or poly).
    pub(crate) fn array_dup_active(&self) -> bool {
        self.tool.array_dup.is_some()
    }

    /// Shift+D on SDF prims: duplicate, arm translate gizmo, open array session.
    pub(crate) fn start_shift_dup_array(&mut self) {
        let indices: Vec<usize> = self.selected_prim_indices_unified();
        if indices.is_empty() {
            self.toast("Select an SDF primitive to duplicate");
            return;
        }
        self.exit_tools_arm_grab();
        self.begin_episode("Duplicate + Array");

        let source_uid = self
            .active_prim_index_unified()
            .or_else(|| indices.first().copied())
            .and_then(|i| self.scene.primitives.get(i).map(|p| p.prim_uid));
        let Some(source_uid) = source_uid else {
            self.cancel_episode();
            return;
        };
        let (origin_local, origin_rot, origin_scale) = self
            .prim_index_by_uid(source_uid)
            .map(|i| {
                let p = &self.scene.primitives[i];
                (p.position, p.rotation_deg, p.scale)
            })
            .unwrap_or((Vec3::ZERO, Vec3::ZERO, Vec3::ONE));

        let mut primary_uids: Vec<u32> = Vec::new();
        for &i in &indices {
            if i < self.scene.primitives.len() && !self.scene.primitives[i].locked {
                if let Some((_new_idx, uid)) = self.duplicate_prim_for_transform(i) {
                    primary_uids.push(uid);
                }
            }
        }
        if primary_uids.is_empty() {
            self.cancel_episode();
            self.toast("Nothing to duplicate (locked?)");
            return;
        }

        self.clear_selection_unified();
        for &uid in &primary_uids {
            self.toggle_select_unified(SceneItemId::Prim(uid));
        }

        self.tool.array_dup = Some(ArrayDupSession {
            source_uid,
            origin_local,
            origin_rot,
            origin_scale,
            offset: Vec3::ZERO,
            rotation_deg: Vec3::ZERO,
            scale: Vec3::ONE,
            array_count: 1,
            array_prim_uids: Vec::new(),
            primary_uids,
            scroll_accum: 0.0,
            source_nid: None,
            array_nids: Vec::new(),
            primary_nids: Vec::new(),
            array_circle: false,
            as_modifier: false,
            as_instance: false,
        });
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.tool.current = ActiveTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast(
            "Duplicate: gizmo · scroll=count · C=circle · I=instance · M=as-modifier · Enter confirm",
        );
    }

    /// Shift+D on a poly body (Object mode): same array session as SDF, on scene nodes.
    pub(crate) fn start_poly_shift_dup_array(&mut self) {
        let mut sel: Vec<NodeId> = self
            .sel
            .usel
            .selected_nodes()
            .filter(|&n| {
                matches!(
                    self.scene.scene_graph.get(n).map(|x| &x.payload),
                    Some(NodePayload::PolygonalBody { .. })
                )
            })
            .collect();
        if sel.is_empty() {
            if let Some(n) = self.sel.usel.active_node().or_else(|| self.poly_active_body()) {
                if matches!(
                    self.scene.scene_graph.get(n).map(|x| &x.payload),
                    Some(NodePayload::PolygonalBody { .. })
                ) {
                    sel.push(n);
                }
            }
        }
        if sel.is_empty() {
            self.toast("Select a poly mesh to duplicate");
            return;
        }
        // Active first.
        if let Some(active) = self.sel.usel.active_node() {
            if let Some(pos) = sel.iter().position(|&n| n == active) {
                sel.swap(0, pos);
            }
        }
        let source_nid = sel[0];
        if self
            .scene
            .scene_graph
            .get(source_nid)
            .map(|n| n.locked)
            .unwrap_or(true)
        {
            self.toast("Object is locked");
            return;
        }

        self.exit_tools_arm_grab();
        self.begin_episode("Duplicate + Array");

        let (origin_local, origin_rot, origin_scale) = self
            .scene
            .scene_graph
            .get(source_nid)
            .map(|n| (n.local_position, n.local_rotation, n.local_scale))
            .unwrap_or((Vec3::ZERO, Vec3::ZERO, Vec3::ONE));

        let mut primary_nids: Vec<NodeId> = Vec::new();
        for &nid in &sel {
            if let Some(new_nid) = self.duplicate_node_no_undo(nid) {
                primary_nids.push(new_nid);
            }
        }
        if primary_nids.is_empty() {
            self.cancel_episode();
            self.toast("Nothing duplicated");
            return;
        }

        let new_ids: Vec<SceneItemId> = primary_nids
            .iter()
            .map(|&n| SceneItemId::Node(n))
            .collect();
        self.select_after_create(&new_ids);

        self.tool.array_dup = Some(ArrayDupSession {
            source_uid: 0,
            origin_local,
            origin_rot,
            origin_scale,
            offset: Vec3::ZERO,
            rotation_deg: Vec3::ZERO,
            scale: Vec3::ONE,
            array_count: 1,
            array_prim_uids: Vec::new(),
            primary_uids: Vec::new(),
            scroll_accum: 0.0,
            source_nid: Some(source_nid),
            array_nids: Vec::new(),
            primary_nids,
            array_circle: false,
            as_modifier: false,
            as_instance: false,
        });
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.tool.current = ActiveTool::Idle;
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
        self.toast(
            "Poly Duplicate: gizmo · scroll=count · C=circle · I=instance · M=as-modifier · Enter confirm",
        );
    }

    fn array_dup_is_poly(sess: &ArrayDupSession) -> bool {
        sess.source_nid.is_some()
    }

    /// Scroll during array session: one physical notch ≈ one count step.
    pub(crate) fn array_dup_scroll(&mut self, sc: f32) {
        let Some(sess) = self.tool.array_dup.as_mut() else {
            return;
        };
        const NOTCH: f32 = 40.0;
        sess.scroll_accum += sc;
        let steps = (sess.scroll_accum / NOTCH).trunc() as i32;
        if steps == 0 {
            return;
        }
        sess.scroll_accum -= steps as f32 * NOTCH;
        let steps = steps.clamp(-1, 1);
        let old = sess.array_count;
        if steps > 0 {
            sess.array_count = (sess.array_count as i32 + steps).min(50).max(1) as u32;
        } else {
            sess.array_count = (sess.array_count as i32 + steps).max(1) as u32;
        }
        if sess.array_count == old {
            return;
        }
        // Circle needs at least 2 copies on the ring.
        if sess.array_circle && sess.array_count < 2 {
            sess.array_count = 2;
        }
        self.array_dup_rebuild_intermediates();
        self.array_dup_apply_layout();
        let (count, circle) = self
            .tool
            .array_dup
            .as_ref()
            .map(|s| (s.array_count, s.array_circle))
            .unwrap_or((1, false));
        let mode = if circle { "Circle" } else { "Linear" };
        self.toast(&format!("{mode} array ×{count}"));
        self.mark_dirty(DirtyFlags::SCENE);
    }

    /// After gizmo drag: read primary transform → session params → redistribute copies.
    pub(crate) fn array_dup_update_positions(&mut self) {
        self.array_dup_sync_params_from_primary();
        self.array_dup_apply_layout();
    }

    fn array_dup_sync_params_from_primary(&mut self) {
        let Some(sess) = self.tool.array_dup.as_ref() else {
            return;
        };
        let origin_local = sess.origin_local;
        let origin_rot = sess.origin_rot;
        let origin_scale = sess.origin_scale;
        let is_poly = Self::array_dup_is_poly(sess);

        let (pos, rot, scale) = if is_poly {
            let Some(&nid) = sess.primary_nids.first() else {
                return;
            };
            let Some(n) = self.scene.scene_graph.get(nid) else {
                return;
            };
            (n.local_position, n.local_rotation, n.local_scale)
        } else {
            let Some(&primary_uid) = sess.primary_uids.first() else {
                return;
            };
            let Some(pidx) = self.prim_index_by_uid(primary_uid) else {
                return;
            };
            let p = &self.scene.primitives[pidx];
            (p.position, p.rotation_deg, p.scale)
        };

        let offset = pos - origin_local;
        let rotation_deg = rot - origin_rot;
        let safe = origin_scale.abs().max(Vec3::splat(1e-4)) * origin_scale.signum();
        let scale = scale / safe;
        if let Some(s) = self.tool.array_dup.as_mut() {
            s.offset = offset;
            s.rotation_deg = rotation_deg;
            s.scale = scale;
        }
    }

    /// Apply session params to primary + all intermediate copies.
    pub(crate) fn array_dup_apply_layout(&mut self) {
        let Some(sess) = self.tool.array_dup.clone() else {
            return;
        };
        if Self::array_dup_is_poly(&sess) {
            self.array_dup_apply_layout_poly(&sess);
        } else {
            self.array_dup_apply_layout_prim(&sess);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    fn array_dup_apply_layout_prim(&mut self, sess: &ArrayDupSession) {
        let count = sess.array_count.max(1) as f32;
        if sess.array_circle {
            self.array_dup_layout_circle_prim(sess);
            return;
        }
        if let Some(&primary_uid) = sess.primary_uids.first() {
            if let Some(idx) = self.prim_index_by_uid(primary_uid) {
                self.array_dup_write_copy_prim(idx, sess, 1.0);
            }
        }
        for (i, &uid) in sess.array_prim_uids.iter().enumerate() {
            let t = (i + 1) as f32 / count;
            if let Some(idx) = self.prim_index_by_uid(uid) {
                self.array_dup_write_copy_prim(idx, sess, t);
            }
        }
    }

    fn array_dup_layout_circle_prim(&mut self, sess: &ArrayDupSession) {
        let origin = sess.origin_local;
        let plane_n = self.viewport.wp_resolved.normal.normalize_or_zero();
        let u = self.viewport.wp_resolved.tangent_u.normalize_or_zero();
        let v = self.viewport.wp_resolved.tangent_v.normalize_or_zero();
        let flat = {
            let o = sess.offset;
            let f = o - plane_n * o.dot(plane_n);
            if f.length_squared() < 1e-8 {
                u * 1.5
            } else {
                f
            }
        };
        let radius = flat.length().max(0.5);
        let angle0 = {
            let x = flat.dot(u);
            let y = flat.dot(v);
            if x * x + y * y < 1e-8 {
                0.0
            } else {
                y.atan2(x)
            }
        };
        let n = sess.array_count.max(1) as f32;
        let place = |i: u32| {
            let a = angle0 + std::f32::consts::TAU * i as f32 / n;
            origin + u * (radius * a.cos()) + v * (radius * a.sin())
        };
        if let Some(&primary_uid) = sess.primary_uids.first() {
            if let Some(idx) = self.prim_index_by_uid(primary_uid) {
                self.scene.primitives[idx].position = place(0);
                self.mirror_prim_to_graph(idx);
            }
        }
        for (i, &uid) in sess.array_prim_uids.iter().enumerate() {
            if let Some(idx) = self.prim_index_by_uid(uid) {
                self.scene.primitives[idx].position = place((i + 1) as u32);
                self.mirror_prim_to_graph(idx);
            }
        }
    }

    fn array_dup_write_copy_prim(&mut self, idx: usize, sess: &ArrayDupSession, t: f32) {
        let pos = sess.origin_local + sess.offset * t;
        let rot = sess.origin_rot + sess.rotation_deg * t;
        let scale = sess.origin_scale * Vec3::ONE.lerp(sess.scale, t);
        self.scene.primitives[idx].position = pos;
        self.scene.primitives[idx].rotation_deg = rot;
        self.scene.primitives[idx].scale = scale;
        self.mirror_prim_to_graph(idx);
    }

    fn array_dup_apply_layout_poly(&mut self, sess: &ArrayDupSession) {
        let count = sess.array_count.max(1) as f32;
        if sess.array_circle {
            self.array_dup_layout_circle_poly(sess);
            return;
        }
        if let Some(&nid) = sess.primary_nids.first() {
            self.array_dup_write_copy_poly(nid, sess, 1.0);
        }
        for (i, &nid) in sess.array_nids.iter().enumerate() {
            let t = (i + 1) as f32 / count;
            self.array_dup_write_copy_poly(nid, sess, t);
        }
    }

    fn array_dup_layout_circle_poly(&mut self, sess: &ArrayDupSession) {
        let origin = sess.origin_local;
        // Prefer workplane; fall back to world Y-up ring in XZ.
        let (u, v) = {
            let plane_n = self.viewport.wp_resolved.normal.normalize_or_zero();
            let uu = self.viewport.wp_resolved.tangent_u.normalize_or_zero();
            let vv = self.viewport.wp_resolved.tangent_v.normalize_or_zero();
            if plane_n.length_squared() > 0.25 && uu.length_squared() > 0.25 {
                (uu, vv)
            } else {
                (Vec3::X, Vec3::Z)
            }
        };
        let flat = {
            let o = sess.offset;
            // Project offset into the ring plane (u,v).
            let f = u * o.dot(u) + v * o.dot(v);
            if f.length_squared() < 1e-8 {
                u * 1.5
            } else {
                f
            }
        };
        let radius = flat.length().max(0.5);
        let angle0 = {
            let x = flat.dot(u);
            let y = flat.dot(v);
            if x * x + y * y < 1e-8 {
                0.0
            } else {
                y.atan2(x)
            }
        };
        let n = sess.array_count.max(1) as f32;
        let place = |i: u32| {
            let a = angle0 + std::f32::consts::TAU * i as f32 / n;
            origin + u * (radius * a.cos()) + v * (radius * a.sin())
        };
        if let Some(&nid) = sess.primary_nids.first() {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.local_position = place(0);
            }
        }
        for (i, &nid) in sess.array_nids.iter().enumerate() {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                node.local_position = place((i + 1) as u32);
            }
        }
        self.mark_dirty(DirtyFlags::SCENE);
    }

    fn array_dup_write_copy_poly(&mut self, nid: NodeId, sess: &ArrayDupSession, t: f32) {
        let pos = sess.origin_local + sess.offset * t;
        let rot = sess.origin_rot + sess.rotation_deg * t;
        let scale = sess.origin_scale * Vec3::ONE.lerp(sess.scale, t);
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            node.local_position = pos;
            node.local_rotation = rot;
            node.local_scale = scale;
        }
        self.mark_dirty(DirtyFlags::SCENE);
    }

    fn array_dup_rebuild_intermediates(&mut self) {
        let is_poly = self
            .tool
            .array_dup
            .as_ref()
            .map(Self::array_dup_is_poly)
            .unwrap_or(false);
        if is_poly {
            self.array_dup_rebuild_intermediates_poly();
        } else {
            self.array_dup_rebuild_intermediates_prim();
        }
    }

    fn array_dup_rebuild_intermediates_prim(&mut self) {
        let Some(sess) = self.tool.array_dup.as_mut() else {
            return;
        };
        let array_count = sess.array_count;
        let mut array_uids = std::mem::take(&mut sess.array_prim_uids);
        let Some(&primary_uid) = sess.primary_uids.first() else {
            sess.array_prim_uids = array_uids;
            return;
        };
        let needed = (array_count as usize).saturating_sub(1);

        let source_idx = match self.prim_index_by_uid(primary_uid) {
            Some(i) => i,
            None => {
                if let Some(s) = self.tool.array_dup.as_mut() {
                    s.array_prim_uids = array_uids;
                }
                return;
            }
        };

        while array_uids.len() < needed {
            if let Some((_new_idx, uid)) = self.duplicate_prim_for_transform(source_idx) {
                array_uids.push(uid);
            } else {
                break;
            }
        }
        while array_uids.len() > needed {
            if let Some(uid) = array_uids.pop() {
                if let Some(idx) = self.prim_index_by_uid(uid) {
                    self.remove_prim_from_graph(idx);
                }
            }
        }

        let primary_uids = self
            .tool
            .array_dup
            .as_ref()
            .map(|s| s.primary_uids.clone())
            .unwrap_or_default();
        if let Some(s) = self.tool.array_dup.as_mut() {
            s.array_prim_uids = array_uids;
        }
        self.clear_selection_unified();
        for &uid in &primary_uids {
            self.toggle_select_unified(SceneItemId::Prim(uid));
        }
    }

    fn array_dup_rebuild_intermediates_poly(&mut self) {
        let (array_count, primary_nid, source_nid, as_instance) = {
            let Some(sess) = self.tool.array_dup.as_ref() else {
                return;
            };
            let Some(&primary) = sess.primary_nids.first() else {
                return;
            };
            (
                sess.array_count,
                primary,
                sess.source_nid,
                sess.as_instance,
            )
        };
        let needed = (array_count as usize).saturating_sub(1);
        let mut array_nids = self
            .tool
            .array_dup
            .as_mut()
            .map(|s| std::mem::take(&mut s.array_nids))
            .unwrap_or_default();

        // Template for new copies: primary (current mesh) unless instance → source mesh.
        let template = if as_instance {
            source_nid.unwrap_or(primary_nid)
        } else {
            primary_nid
        };

        while array_nids.len() < needed {
            if let Some(new_nid) = self.duplicate_node_no_undo(template) {
                if as_instance {
                    self.array_dup_share_poly_mesh(source_nid.unwrap_or(template), new_nid);
                }
                array_nids.push(new_nid);
            } else {
                break;
            }
        }
        while array_nids.len() > needed {
            if let Some(nid) = array_nids.pop() {
                self.delete_node_no_undo(nid);
            }
        }

        let primary_nids = self
            .tool
            .array_dup
            .as_ref()
            .map(|s| s.primary_nids.clone())
            .unwrap_or_default();
        if let Some(s) = self.tool.array_dup.as_mut() {
            s.array_nids = array_nids;
        }
        let new_ids: Vec<SceneItemId> = primary_nids
            .iter()
            .map(|&n| SceneItemId::Node(n))
            .collect();
        if !new_ids.is_empty() {
            self.select_after_create(&new_ids);
        }
        self.mark_outliner_dirty();
        self.mark_dirty(DirtyFlags::SCENE);
    }

    /// Share mesh Arc + mesh_cache_id from `src` onto `dst` (instance-like).
    fn array_dup_share_poly_mesh(&mut self, src: NodeId, dst: NodeId) {
        let (mesh, cache_id, modifiers) = match self.scene.scene_graph.get(src) {
            Some(n) => match &n.payload {
                NodePayload::PolygonalBody {
                    mesh,
                    mesh_cache_id,
                    modifiers,
                    ..
                } => (mesh.clone(), *mesh_cache_id, modifiers.clone()),
                _ => return,
            },
            None => return,
        };
        if let Some(n) = self.scene.scene_graph.get_mut(dst) {
            if let NodePayload::PolygonalBody {
                mesh: dm,
                mesh_cache_id: dc,
                modifiers: dmod,
                ..
            } = &mut n.payload
            {
                *dm = mesh;
                *dc = cache_id;
                *dmod = modifiers;
            }
        }
        self.poly_request_display_sync(dst);
    }

    /// C: toggle circular vs linear array.
    pub(crate) fn array_dup_toggle_circle(&mut self) {
        let Some(sess) = self.tool.array_dup.as_mut() else {
            return;
        };
        sess.array_circle = !sess.array_circle;
        let circle = sess.array_circle;
        if circle && sess.array_count < 2 {
            sess.array_count = 3;
        }
        self.array_dup_rebuild_intermediates();
        self.array_dup_apply_layout();
        self.toast(if circle {
            "Circle array · empty center · scroll=count · C=linear"
        } else {
            "Linear array · scroll=count · C=circle"
        });
    }

    /// I: toggle instance mode (poly: share mesh with source; SDF: toast only for now).
    pub(crate) fn array_dup_toggle_instance(&mut self) {
        let Some(sess) = self.tool.array_dup.as_mut() else {
            return;
        };
        sess.as_instance = !sess.as_instance;
        let on = sess.as_instance;
        let is_poly = Self::array_dup_is_poly(sess);
        let source_nid = sess.source_nid;
        let primary_nids = sess.primary_nids.clone();
        let array_nids = sess.array_nids.clone();
        if is_poly && on {
            if let Some(src) = source_nid {
                for &nid in primary_nids.iter().chain(array_nids.iter()) {
                    self.array_dup_share_poly_mesh(src, nid);
                }
            }
        }
        self.toast(if on {
            "Instance ON · copies share mesh with source (COW on edit)"
        } else {
            "Instance OFF · independent mesh copies"
        });
    }

    /// M: toggle "create as Array modifier" (applied on Enter).
    pub(crate) fn array_dup_toggle_as_modifier(&mut self) {
        let Some(sess) = self.tool.array_dup.as_mut() else {
            return;
        };
        sess.as_modifier = !sess.as_modifier;
        let on = sess.as_modifier;
        self.toast(if on {
            "As modifier ON · Enter applies Array on source (temps removed)"
        } else {
            "As modifier OFF · Enter keeps real copies"
        });
    }

    /// Enter: keep real copies, or apply modifier if `as_modifier`.
    pub(crate) fn array_dup_confirm(&mut self) {
        let Some(sess) = self.tool.array_dup.clone() else {
            return;
        };
        if sess.as_modifier {
            self.array_dup_make_modifier();
            return;
        }
        self.tool.array_dup = None;
        self.commit_episode();
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.toast("Duplicate confirmed");
        self.restore_pre_grab_state();
    }

    /// Esc / Space / Drop Tool: restore pre-Shift+D scene.
    pub(crate) fn array_dup_cancel(&mut self) {
        if self.tool.array_dup.is_none() {
            return;
        }
        self.tool.array_dup = None;
        self.tool.current = ActiveTool::Idle;
        self.set_gizmo_armed(false);
        if let Some(entry) = self.pending_episode.take() {
            if let app_types::UndoPayload::Full {
                scene_graph_snapshot: Some(sg),
                ..
            } = entry.payload
            {
                self.scene.scene_graph = sg;
                self.sync_flat_cache();
            }
            self.scene.active_body_nid = entry.active_body_nid;
            self.sel.usel = entry.usel;
            self.history.mark_prims_dirty();
            if self.cache.use_body_vol_cache {
                self.cache
                    .body_vol_cache
                    .invalidate_for_undo(&self.scene.scene_graph);
            }
            self.cache.body_vol_cache.suppress_bake_dispatch = false;
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH);
        } else {
            self.restore_undo_no_redo();
        }
        self.rebake_no_undo();
        self.toast("Duplicate cancelled");
        self.restore_pre_grab_state();
    }

    /// Collapse live copies into a non-destructive Array / ArrayCircular on the source.
    pub(crate) fn array_dup_make_modifier(&mut self) {
        let Some(sess) = self.tool.array_dup.clone() else {
            return;
        };
        if Self::array_dup_is_poly(&sess) {
            self.array_dup_make_modifier_poly(&sess);
        } else {
            self.array_dup_make_modifier_prim(&sess);
        }
    }

    fn array_dup_make_modifier_prim(&mut self, sess: &ArrayDupSession) {
        let Some(_src_idx) = self.prim_index_by_uid(sess.source_uid) else {
            self.toast("Source prim gone");
            return;
        };

        let total = sess.array_count.max(1) + 1;
        let step = sess.offset / sess.array_count.max(1) as f32;

        let mut to_remove: Vec<u32> = sess.array_prim_uids.clone();
        to_remove.extend_from_slice(&sess.primary_uids);
        let mut idxs: Vec<usize> = to_remove
            .iter()
            .filter_map(|u| self.prim_index_by_uid(*u))
            .collect();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        for idx in idxs {
            self.remove_prim_from_graph(idx);
        }

        let src_idx = match self.prim_index_by_uid(sess.source_uid) {
            Some(i) => i,
            None => {
                self.array_dup_cancel();
                return;
            }
        };
        let mut arr = crate::ui_panels::default_modifier(ModifierType::Array);
        arr.strength = total as f32;
        arr.mask.center = step;

        let mods = &mut self.scene.primitives[src_idx].modifiers;
        if let Some(existing) = mods.iter_mut().find(|m| m.mod_type == ModifierType::Array) {
            *existing = arr;
        } else {
            mods.push(arr);
        }
        self.scene.primitives[src_idx].sync_modifiers_to_gpu_fields();
        self.mirror_prim_to_graph(src_idx);

        self.tool.array_dup = None;
        self.commit_episode();
        self.select_single_unified(SceneItemId::Prim(sess.source_uid));
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.tool.current = ActiveTool::Idle;
        self.rebake_no_undo();
        self.toast(&format!(
            "Array modifier ×{total} · edit Count/Offset in Modifiers panel"
        ));
        self.restore_pre_grab_state();
    }

    fn array_dup_make_modifier_poly(&mut self, sess: &ArrayDupSession) {
        let Some(source_nid) = sess.source_nid else {
            return;
        };

        // Remove temporary body copies.
        let mut to_remove: Vec<NodeId> = sess.array_nids.clone();
        to_remove.extend_from_slice(&sess.primary_nids);
        for nid in to_remove {
            self.delete_node_no_undo(nid);
        }

        if self.scene.scene_graph.get(source_nid).is_none() {
            self.array_dup_cancel();
            return;
        }

        let count = (sess.array_count + 1).max(2); // original + end + intermediates
        let step = if sess.array_count > 0 {
            sess.offset / sess.array_count as f32
        } else {
            sess.offset
        };

        let mut m = if sess.array_circle {
            let mut m = PolyModifier::new(PolyModifierType::ArrayCircular);
            m.array_count = count.max(2);
            m.circ_center = Vec3::ZERO; // local body space center
            m.circ_angle_deg = 360.0;
            // Axis from workplane normal.
            let an = self.viewport.wp_resolved.normal.normalize_or_zero().abs();
            m.circ_axis = if an.y >= an.x && an.y >= an.z {
                1 // Y
            } else if an.x >= an.z {
                0 // X
            } else {
                2 // Z
            };
            m
        } else {
            let mut m = PolyModifier::new(PolyModifierType::Array);
            m.array_count = count;
            m.array_offset = if step.length_squared() > 1e-10 {
                step
            } else {
                Vec3::new(1.5, 0.0, 0.0)
            };
            m
        };

        if let Some(node) = self.scene.scene_graph.get_mut(source_nid) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut node.payload {
                // Replace existing Array / ArrayCircular of same kind, else push.
                let kind = m.mod_type;
                if let Some(existing) = modifiers.iter_mut().find(|x| x.mod_type == kind) {
                    *existing = m;
                } else {
                    modifiers.push(m);
                }
            }
        }

        self.tool.array_dup = None;
        self.commit_episode();
        self.select_single_unified(SceneItemId::Node(source_nid));
        self.tool.gizmo_mode = GizmoMode::Translate;
        self.tool.gizmo_armed = true;
        self.tool.current = ActiveTool::Idle;
        self.poly_request_display_sync(source_nid);
        self.poly_flush_mesh_dirty();
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        self.mark_outliner_dirty();
        let label = if sess.array_circle {
            "Circle Array"
        } else {
            "Linear Array"
        };
        self.toast(&format!(
            "{label} modifier ×{count} · edit in Modifiers panel"
        ));
        self.restore_pre_grab_state();
    }

    /// Floating tool-parameter bar at top of viewport during Shift+D.
    pub(crate) fn draw_array_dup_toolbar(&mut self, ctx: &egui::Context) {
        if self.tool.array_dup.is_none() {
            return;
        }

        let mut offset = self.tool.array_dup.as_ref().unwrap().offset;
        let mut rotation_deg = self.tool.array_dup.as_ref().unwrap().rotation_deg;
        let mut scale = self.tool.array_dup.as_ref().unwrap().scale;
        let mut count = self.tool.array_dup.as_ref().unwrap().array_count as i32;
        let circle = self.tool.array_dup.as_ref().unwrap().array_circle;
        let as_mod = self.tool.array_dup.as_ref().unwrap().as_modifier;
        let as_inst = self.tool.array_dup.as_ref().unwrap().as_instance;
        let is_poly = self
            .tool
            .array_dup
            .as_ref()
            .map(Self::array_dup_is_poly)
            .unwrap_or(false);
        let mut changed = false;
        let mut count_changed = false;
        let mut confirm = false;
        let mut cancel = false;
        let mut toggle_mod = false;
        let mut toggle_inst = false;
        let mut toggle_circle = false;

        egui::Area::new(egui::Id::new("array_dup_toolbar"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 48.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .rounding(6.0)
                    .fill(egui::Color32::from_rgba_unmultiplied(28, 30, 36, 230))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(80, 140, 220),
                    ))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(120, 190, 255),
                                egui::RichText::new(if is_poly {
                                    "Shift+D Poly Array"
                                } else {
                                    "Shift+D Array"
                                })
                                .strong(),
                            );
                            ui.separator();

                            ui.label("Copies");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut count)
                                        .speed(0.2)
                                        .range(1..=50)
                                        .prefix("×"),
                                )
                                .changed()
                            {
                                count_changed = true;
                                changed = true;
                            }

                            ui.separator();
                            ui.label("Offset");
                            for (axis, c) in [
                                ("X", &mut offset.x),
                                ("Y", &mut offset.y),
                                ("Z", &mut offset.z),
                            ] {
                                if ui
                                    .add(
                                        egui::DragValue::new(c)
                                            .speed(0.02)
                                            .prefix(axis)
                                            .fixed_decimals(3),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }

                            ui.separator();
                            if ui
                                .selectable_label(circle, "C circle")
                                .on_hover_text("Circular array (C)")
                                .clicked()
                            {
                                toggle_circle = true;
                            }
                            if ui
                                .selectable_label(as_inst, "I instance")
                                .on_hover_text("Share mesh with source (I)")
                                .clicked()
                            {
                                toggle_inst = true;
                            }
                            if ui
                                .selectable_label(as_mod, "M modifier")
                                .on_hover_text("Enter applies Array modifier on source (M)")
                                .clicked()
                            {
                                toggle_mod = true;
                            }

                            ui.separator();
                            if ui
                                .small_button("✓")
                                .on_hover_text("Confirm (Enter)")
                                .clicked()
                            {
                                confirm = true;
                            }
                            if ui
                                .small_button("✕")
                                .on_hover_text("Cancel (Space / Esc)")
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.weak(
                                "Gizmo · scroll=count · C=circle · I=instance · M=as-mod · Enter confirm",
                            );
                        });
                    });
            });

        if toggle_circle {
            self.array_dup_toggle_circle();
            return;
        }
        if toggle_inst {
            self.array_dup_toggle_instance();
            return;
        }
        if toggle_mod {
            self.array_dup_toggle_as_modifier();
            return;
        }
        if count_changed {
            if let Some(s) = self.tool.array_dup.as_mut() {
                s.array_count = count.clamp(1, 50) as u32;
            }
            self.array_dup_rebuild_intermediates();
        }
        if changed {
            if let Some(s) = self.tool.array_dup.as_mut() {
                s.offset = offset;
                s.rotation_deg = rotation_deg;
                s.scale = scale;
                if !count_changed {
                    s.array_count = count.clamp(1, 50) as u32;
                }
            }
            self.array_dup_apply_layout();
            // Keep selection on primaries so gizmo stays on the end copy.
            if let Some(sess) = self.tool.array_dup.as_ref() {
                if Self::array_dup_is_poly(sess) {
                    let ids: Vec<SceneItemId> = sess
                        .primary_nids
                        .iter()
                        .map(|&n| SceneItemId::Node(n))
                        .collect();
                    if !ids.is_empty() {
                        self.select_after_create(&ids);
                    }
                } else {
                    let uids = sess.primary_uids.clone();
                    self.clear_selection_unified();
                    for &uid in &uids {
                        self.toggle_select_unified(SceneItemId::Prim(uid));
                    }
                }
            }
        }
        if confirm {
            self.array_dup_confirm();
        } else if cancel {
            self.array_dup_cancel();
        }
    }
}
