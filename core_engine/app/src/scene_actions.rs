//! Scene item creation, deletion, and duplication — shared actions
//! called from outliner, pie menu, keyboard, menus, and agent bar.

use crate::*;

/// Outcome of [`FourthLayerApp::reparent_body_node`] / [`FourthLayerApp::reparent_body_check`]
/// so callers can toast product-policy rejects without duplicating the guard list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReparentBodyResult {
    /// Graph reparent + checklist applied (or would apply for the pure check).
    Applied,
    /// No-op / cycle / self — silent (hover already rejected most of these).
    Skipped,
    /// Kitbash Group under / under-body Group is illegal for bake serialize.
    ForbiddenGroupUnderBody,
}

impl FourthLayerApp {
    /// The RENDER/VOLUME unit for the current edit scope: the TOP-LEVEL ancestor of
    /// `active_body_nid`. A nested sub-body never owns a grid, resident volume, or mesh —
    /// it is subsumed into its top-level ancestor's subtree bake (`trigger_bake` skips
    /// nested bodies; all dirty marks climb) — so every consumer that keys a grid lookup,
    /// resident-volume upload, or render decision on "the active body" MUST resolve through
    /// this. Keying on the RAW nested nid rendered a frozen stale volume while the ancestor
    /// re-baked correctly (the "boolean tree inside a sub-body not working" symptom — the
    /// tree was right, the displayed volume was stale). CLAUDE.md: "Active analytical unit
    /// is always the whole top-level SdfBody subtree."
    pub(crate) fn active_top_body(&self) -> core_math::scene_graph::NodeId {
        self.scene.scene_graph.top_level_body(self.scene.active_body_nid)
    }

    // ---- X boolean pie ---------------------------------------------------------

    /// Apply an X boolean-pie choice (slot in `pie_menu::sdf_boolean_pie_def` —
    /// 8-sector layout: 0=Union, 1=Extrude, 2=Subtract, 4=Intersect, 6=Seam,
    /// 3/5/7=spacers) to the current selection: every selected NON-base prim,
    /// or — when no prims are selected — every selected SdfBody sub-body.
    /// Preserves each target's blend k / seam / extrude params (same semantics
    /// as `ui_panels::bool_op_menu_items`, the context menu set). Prim op writes
    /// route through `set_prims_boolean_op` — the same mutation path as
    /// `AppCommand::SetBooleanOp` (no second code path). Extrude (idx 1) is the
    /// `BooleanOp::Extrude` FOOTPRINT boolean (owner 2026-07-14 — supersedes the
    /// old Emboss/Deboss relief-modifier push, which only inflated the prim's own
    /// field and read as a scale; the ModifierType::Emboss/Deboss properties UI
    /// remains as a separate legacy feature the pie no longer touches).
    pub(crate) fn apply_sdf_bool_pie(&mut self, slot: usize) {
        use crate::ui_panels::{bool_op_preserved_k, seam_op_defaults, extrude_op_defaults};
        let map_op = |cur: &BooleanOp| -> BooleanOp {
            match slot {
                0 => BooleanOp::Union { k: bool_op_preserved_k(cur) },
                2 => BooleanOp::Subtract { k: bool_op_preserved_k(cur) },
                4 => BooleanOp::Intersect,
                // ONE Extrude slot: raise default on a fresh pick; an existing
                // Extrude keeps its params incl. polarity (raise: None) — the
                // viewport handle owns the raise↔recess flip (vertical drag).
                1 => extrude_op_defaults(cur, None),
                // ONE Seam slot: groove default on a fresh pick; an existing Seam
                // keeps ALL its params incl. polarity (raised: None) — the viewport
                // handle owns the groove↔bead flip (signed depth drag).
                _ => seam_op_defaults(cur, None),
            }
        };
        let label = match slot { 0 => "Union", 1 => "Extrude", 2 => "Subtract", 4 => "Intersect", _ => "Seam" };

        // Prim targets: all selected non-base prims (the chain base keeps the
        // "no boolean op" rule — the pie never opens for a base-only selection).
        let prim_changes: Vec<(u32, BooleanOp)> = self.selected_prim_indices_unified().iter()
            .filter(|&&i| i < self.scene.primitives.len() && !self.scene.is_base_prim_flat(i))
            .map(|&i| (self.scene.primitives[i].prim_uid, map_op(&self.scene.primitives[i].operation)))
            .collect();
        if !prim_changes.is_empty() {
            // set_prims_boolean_op filters no-ops; only toast when something actually changed.
            if self.set_prims_boolean_op(&prim_changes) {
                let n = prim_changes.len();
                self.toast(&if n > 1 { format!("Boolean: {} ({} prims)", label, n) }
                           else { format!("Boolean: {}", label) });
            }
            return;
        }

        // Sub-body targets: all selected SdfBody nodes. Same mutation steps as the
        // old X body-op cycle and the sub-body Boolean combo: set_body_op +
        // flat-cache sync + SCENE + rebake, in ONE transaction.
        // Drop bodies whose op is already the mapped result (re-pick same pie slot).
        let body_changes: Vec<(core_math::scene_graph::NodeId, BooleanOp)> =
            self.sel.usel.selected_nodes()
                .filter(|&nid| self.scene.scene_graph.get(nid)
                    .map(|n| matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                    .unwrap_or(false))
                .filter_map(|nid| {
                    let cur = self.scene.scene_graph.body_op(nid)?;
                    let next = map_op(cur);
                    if next == *cur { return None; }
                    Some((nid, next))
                })
                .collect();
        if body_changes.is_empty() { return; }
        let n = body_changes.len();
        self.transaction("Change body boolean", |s| {
            for (nid, op) in body_changes {
                s.scene.scene_graph.set_body_op(nid, op);
            }
            s.sync_flat_cache();
            s.mark_dirty(DirtyFlags::SCENE);
            s.rebake_no_undo();
        });
        self.toast(&if n > 1 { format!("Body: {} ({} bodies)", label, n) }
                   else { format!("Body: {}", label) });
    }

    // (apply_extrude_pie DELETED 2026-07-14: the pie Extrude slot now sets the
    // BooleanOp::Extrude footprint boolean via the shared map_op/set_prims_boolean_op
    // path above — the old ModifierType::Emboss/Deboss self-stamp push only inflated
    // the prim's own field and read as a scale.)

    // ---- Lights ----------------------------------------------------------------

    /// Spawn world position: screen-center ray, a short distance in front of the camera.
    /// Always visible right after create (unlike fixed (0,2,0) which is often off-screen).
    /// World-space spawn for a new light.
    ///
    /// Owner 2026-08-07: when the **workplane is active (Locked)**, place at the workplane
    /// **origin** (same placement target as create-on-WP). Otherwise fall back to the
    /// camera-relative spawn used for free placement.
    pub(crate) fn light_spawn_position(&self) -> Vec3 {
        if self.viewport.wp_mode == crate::WorkPlaneMode::Locked {
            return self.viewport.wp_resolved.origin;
        }
        let cam = &self.viewport.camera;
        let cam_pos = cam.target + cam.orbit * Vec3::new(0.0, 0.0, cam.distance);
        let fwd = (cam.orbit * Vec3::NEG_Z).normalize_or_zero();
        // ~¼ of orbit distance, clamped — close enough to pick/move, far enough from near clip.
        let spawn_dist = (cam.distance * 0.28).clamp(1.5, 8.0);
        if self.render.ortho {
            // Ortho: sit at the orbit target (screen center), slightly toward camera so it
            // is not buried inside geometry at the far plane.
            cam.target - fwd * (cam.distance * 0.05).clamp(0.25, 2.0)
        } else {
            cam_pos + fwd * spawn_dist
        }
    }

    pub(crate) fn add_light(&mut self, lt: LightType) {
        let label = match lt { LightType::Directional => "Directional", LightType::Point => "Point", LightType::Spot => "Spot" };
        let spawn = self.light_spawn_position();
        let on_workplane = self.viewport.wp_mode == crate::WorkPlaneMode::Locked;
        let wp_normal = self.viewport.wp_resolved.normal.normalize_or_zero();
        let _mpos = self.viewport.viewport_mouse;
        // After create: arm Translate gizmo (same GRS as prims) — not a free-grab modal that
        // felt like "hold G to move". Point/Spot always arm; directional too for placement.
        let want_gizmo = matches!(lt, LightType::Point | LightType::Spot | LightType::Directional);
        let mut created_id = 0u32;
        self.transaction("Add Light", |s| {
            let id = s.scene.next_light_id;
            s.scene.next_light_id += 1;
            created_id = id;
            let existing_count = s.scene.light_count();
            let light = match lt {
                LightType::Directional => {
                    let mut l = LightData::default();
                    l.id = id;
                    l.name = format!("Sun.{:03}", id);
                    l.azimuth = 45.0 + existing_count as f32 * 30.0;
                    // Fixed sun marker position (Blender: location independent of aim).
                    // Lighting uses azimuth/elevation only; G re-places the marker.
                    l.position = spawn;
                    l
                }
                LightType::Point => LightData::new_point(id, format!("Point.{:03}", id), spawn),
                LightType::Spot => {
                    let mut l = LightData::new_spot(id, format!("Spot.{:03}", id), spawn);
                    if on_workplane && wp_normal.length_squared() > 0.25 {
                        // Aim along −WP normal (into the plane / typical floor light).
                        let n = -wp_normal;
                        l.elevation = n.y.asin().to_degrees().clamp(-89.0, 89.0);
                        l.azimuth = n.x.atan2(-n.z).to_degrees();
                    } else {
                        // Aim roughly toward scene center / camera target from spawn.
                        let to = s.viewport.camera.target - spawn;
                        if to.length_squared() > 1e-4 {
                            let n = to.normalize();
                            l.elevation = n.y.asin().to_degrees().clamp(-89.0, 89.0);
                            l.azimuth = n.x.atan2(-n.z).to_degrees();
                        }
                    }
                    l
                }
            };
            // Graph-native: add Light node to scene graph
            let root = s.scene.scene_graph.root();
            s.scene.scene_graph.add_child(root, core_math::scene_graph::NodePayload::Light(light));
            s.select_light(id);
            // Ensure Object mode so light icons + transform gizmo draw.
            if s.tool.work_mode != WorkMode::Object {
                s.tool.work_mode = WorkMode::Object;
            }
            s.viewport.show_light_gizmos = true;
            s.viewport.show_overlays = true;
            s.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            if on_workplane {
                s.toast(&format!(
                    "Added {} light at workplane origin — G/R gizmo armed",
                    label
                ));
            } else {
                s.toast(&format!(
                    "Added {} light — drag axis or Alt+LMB free move (same GRS as objects)",
                    label
                ));
            }
        });
        // Arm Translate gizmo so placement uses the same GRS path as prims/bodies.
        if want_gizmo && created_id != 0 {
            let _ = created_id;
            self.tool.gizmo_mode = GizmoMode::Translate;
            self.tool.gizmo_armed = true; // arm without clearing Automatic pivot (was never set)
        }
    }

    pub(crate) fn delete_light(&mut self, id: u32) {
        // Find the graph node for this light
        let nid = self.scene.light_nid(id);
        let Some(nid) = nid else { return; };
        let name = self.scene.scene_graph.get(nid)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_default();
        self.transaction("Delete Light", |s| {
            s.scene.scene_graph.remove(nid);
            s.deselect_items(&[SceneItemId::Light(id)]);
            s.mark_dirty(DirtyFlags::LIGHT);
            s.mark_outliner_dirty();
            s.toast(&format!("Deleted {}", name));
        });
    }

    /// Per-light duplication primitive (NO transaction, NO selection, NO toast) — THE light-copy
    /// core (two-code-paths rule): wrapped by `duplicate_light` (outliner context menu) and looped
    /// by `duplicate_selected()`'s multi-light arm inside ONE transaction. Returns the new light id.
    pub(crate) fn duplicate_light_no_undo(&mut self, id: u32) -> Option<u32> {
        let src = self.scene.light_by_id(id).cloned()?;
        let mut dup_data = src;
        dup_data.id = self.scene.next_light_id;
        self.scene.next_light_id += 1;
        dup_data.name = match dup_data.light_type {
            LightType::Directional => format!("Sun.{:03}", dup_data.id),
            LightType::Point => format!("Point.{:03}", dup_data.id),
            LightType::Spot => format!("Spot.{:03}", dup_data.id),
        };
        match dup_data.light_type {
            LightType::Directional => { dup_data.azimuth += 30.0; }
            _ => { dup_data.position += Vec3::new(1.0, 0.0, 0.0); }
        }
        let new_id = dup_data.id;
        let root = self.scene.scene_graph.root();
        self.scene.scene_graph.add_child(root, core_math::scene_graph::NodePayload::Light(dup_data));
        Some(new_id)
    }

    pub(crate) fn duplicate_light(&mut self, id: u32) {
        // Missing light → no empty undo.
        if self.scene.light_by_id(id).is_none() { return; }
        self.transaction("Duplicate Light", |s| {
            if let Some(new_id) = s.duplicate_light_no_undo(id) {
                s.select_light(new_id);
                s.mark_dirty(DirtyFlags::LIGHT);
                s.mark_outliner_dirty();
                s.toast("Light duplicated");
            }
        });
    }

    /// Find the scene graph NodeId for a light by its stable id.
    pub(crate) fn light_graph_nid(&self, id: u32) -> Option<core_math::scene_graph::NodeId> {
        self.scene.light_nid(id)
    }

    /// Pack GPU lights for this frame (P3.1 shade list + link remap + Phase C/D).
    /// `cull` true for PBR/Lit top-K + clusters; false for PathTrace full visible list.
    /// `screen` = viewport pixel size for Phase D cluster assignment.
    pub(crate) fn build_frame_lights_gpu(
        &mut self,
        cull: bool,
        screen: (u32, u32),
    ) -> crate::light_pack::FrameLightGpu {
        // TWO visibility flags exist and both must be honoured (owner 2026-07-29: "when I click on
        // the eye on the sun the lighting does not disappear").
        //
        //   * `LightData.visible`  — the light's own "Visible" checkbox in its properties panel
        //   * `SceneNode.visible`  — the OUTLINER EYE, written by `outliner_actions.rs:393`
        //
        // This filtered on `LightData.visible` only, so the outliner eye toggled a flag the light
        // packer never read: the light stayed packed and kept lighting the scene. Same class as
        // several bugs found today — a control that changes state no consumer consults.
        //
        // Filtering on BOTH is the honest fix: hiding via either route removes the light's
        // contribution, which is what "the eye should remove all the effect from the object" means.
        let visible: Vec<&LightData> = self.scene.scene_graph.lights()
            .filter(|(nid, _)| {
                self.scene
                    .scene_graph
                    .get(*nid)
                    .map_or(true, |n| n.visible)
            })
            .map(|(_, d)| d)
            .filter(|l| l.visible)
            .collect();
        let cam = &self.viewport.camera;
        let cam_pos = cam.target + cam.orbit * glam::Vec3::new(0.0, 0.0, cam.distance);
        let cam_target = cam.target;
        let cam_forward = (cam_target - cam_pos).normalize_or_zero();
        let world_up = (cam.orbit * glam::Vec3::Y).normalize_or_zero();
        let cam_right = cam_forward.cross(world_up).normalize_or_zero();
        let cam_up = cam_right.cross(cam_forward).normalize_or_zero();
        let tan_half = (self.render.fov_deg.to_radians() * 0.5).tan().max(0.01);
        // Force-include selected lights + any non-Default link light (linking correctness).
        // Both selection forms — an outliner-selected light must not be culled out of the
        // shade list while the user is editing it.
        let mut force: Vec<u32> = self.selected_light_ids();
        for l in &visible {
            if !matches!(l.link_mode, LightLinkMode::Default) && !force.contains(&l.id) {
                force.push(l.id);
            }
        }
        let mut uid_to_flat = std::collections::HashMap::new();
        for (i, p) in self.scene.primitives.iter().enumerate() {
            uid_to_flat.insert(p.prim_uid, i);
        }
        let n_prims = self.scene.primitives.len();
        let cluster = if cull && screen.0 > 0 && screen.1 > 0 {
            Some(crate::light_cluster::ClusterParams {
                screen_w: screen.0,
                screen_h: screen.1,
                cam_pos,
                cam_forward,
                cam_right,
                cam_up,
                tan_half_fov: tan_half,
                near: 0.1,
                far: (cam.distance * 20.0).max(50.0).min(2000.0),
                enabled: true,
            })
        } else {
            None
        };
        let hdr_amb = self.env_hdr_ambient_tint();
        crate::light_pack::build_frame_lights(
            &visible,
            cam_pos,
            cam_target,
            cull,
            &force,
            &uid_to_flat,
            n_prims,
            Some(&mut self.cache.shade_hysteresis),
            cluster.as_ref(),
            hdr_amb,
        )
    }

    /// SH sky average × env brightness for sun Ambient "From HDR".
    /// Uses the last 27 floats of the env mip buffer (SH9 after the mip chain).
    pub(crate) fn env_hdr_ambient_tint(&self) -> Option<[f32; 3]> {
        if !self.render.env_enabled || self.env_hdr_width == 0 {
            return None;
        }
        let data = &self.env_hdr_mip_data;
        if data.len() < 27 {
            return None;
        }
        let start = data.len() - 27;
        let mut sh = [0.0f32; 27];
        sh.copy_from_slice(&data[start..]);
        // Up-hemisphere sample of SH irradiance (sky fill color).
        let rgb = crate::env_ibl::eval_sh9(&sh, [0.0, 1.0, 0.0]);
        let b = self.render.env_brightness.max(0.0);
        Some([rgb[0] * b, rgb[1] * b, rgb[2] * b])
    }

    /// Legacy helper — full visible list, no cull (tests / offline).
    pub(crate) fn serialize_lights_gpu(&mut self) -> Vec<f32> {
        self.build_frame_lights_gpu(false, (1, 1)).light_data
    }

    // ---- SDF Body --------------------------------------------------------------

    /// Create a new independent SdfBody at root level and switch to it.
    pub(crate) fn create_new_sdf_body(&mut self) {
        self.transaction("New SDF Body", |s| {
            let name = format!("Body.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;

            use core_math::primitives::BooleanOp;
            let new_body_nid = s.scene.scene_graph.add_child(
                s.scene.scene_graph.root(),
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
                    material: core_math::primitives::PbrMaterial::default(),
                    child_order: Vec::new(),
                mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
            );

            s.scene.active_body_nid = new_body_nid;
            s.sync_flat_cache();
            s.select_single_unified(SceneItemId::Node(new_body_nid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {} \u{2014} Shift+A to add primitives", name));
        });
    }

    /// Resolve the host body for Ctrl+Shift+A: selected SdfBody, else selected prim's body,
    /// else `active_body_nid`.
    pub(crate) fn resolve_selection_host_body(&self) -> core_math::scene_graph::NodeId {
        if let Some(nid) = self.sel.usel.active_node() {
            if self
                .scene
                .scene_graph
                .get(nid)
                .map(|n| matches!(&n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }))
                .unwrap_or(false)
            {
                return nid;
            }
        }
        // Selected prim → its owning body
        for id in self.sel.usel.selected.iter() {
            if let SceneItemId::Prim(uid) = id {
                if let Some(fi) = self.prim_index_by_uid(*uid) {
                    if let Some(&(bnid, _)) = self.scene.prim_body_map.get(fi) {
                        return bnid;
                    }
                }
            }
        }
        self.scene.active_body_nid
    }

    /// Ctrl+Shift+A path — before adding the new shape:
    ///
    /// 1. Resolve the selected prim (active, else first selected).
    /// 2. Create a **new child sub-body** under that prim's body.
    /// 3. Sub-body `body_op` = the selected prim's boolean (Union/Subtract/Intersect…).
    /// 4. Move the selected prim into the sub-body as **first** prim; reset its op to
    ///    Union so the boolean is not applied twice (same rule as outliner drop-group).
    /// 5. Set `active_body_nid` to the sub-body so the new menu prim is added second.
    ///
    /// No prim selected → empty child under host body (Union), new shape is first.
    pub(crate) fn prepare_add_into_selected_sub_body(&mut self) {
        use core_math::primitives::BooleanOp;
        use core_math::scene_graph::{BodyChildRef, NodePayload};

        // Prefer active prim, else first selected prim.
        let anchor_uid: Option<u32> = self
            .sel
            .usel
            .active
            .as_ref()
            .and_then(|id| match id {
                SceneItemId::Prim(u) => Some(*u),
                _ => None,
            })
            .or_else(|| {
                self.sel.usel.selected.iter().find_map(|id| match id {
                    SceneItemId::Prim(u) => Some(*u),
                    _ => None,
                })
            });

        let host = self.resolve_selection_host_body();
        if !self
            .scene
            .scene_graph
            .get(host)
            .map(|n| matches!(&n.payload, NodePayload::SdfBody { .. }))
            .unwrap_or(false)
        {
            return;
        }

        // Capture anchor prim data before the transaction mutates the graph.
        let (anchor_fi, body_idx, group_op) = if let Some(uid) = anchor_uid {
            let fi = match self.prim_index_by_uid(uid) {
                Some(fi) => fi,
                None => {
                    // Fall through to empty sub-body under host.
                    self.create_empty_subbody_under(host, BooleanOp::Union { k: 0.0 });
                    return;
                }
            };
            let (bnid, bidx) = match self.scene.prim_body_map.get(fi) {
                Some(&e) => e,
                None => {
                    self.create_empty_subbody_under(host, BooleanOp::Union { k: 0.0 });
                    return;
                }
            };
            if bnid != host {
                // Selection host and prim body disagree — use prim's body as host.
            }
            let op = self
                .scene
                .primitives
                .get(fi)
                .map(|p| p.operation.clone())
                .unwrap_or(BooleanOp::Union { k: 0.0 });
            (Some(fi), Some((bnid, bidx)), op)
        } else {
            (None, None, BooleanOp::Union { k: 0.0 })
        };

        let parent = body_idx.map(|(b, _)| b).unwrap_or(host);

        self.transaction("Add SDF in sub-body", |s| {
            // Extract the anchor prim from its body (if any).
            let mut first_prim: Option<core_math::primitives::SdfPrimitive> = None;
            let mut insert_slot = s.scene.scene_graph.body_child_order(parent).len();
            if let (Some(_fi), Some((bnid, bidx))) = (anchor_fi, body_idx) {
                // Slot BEFORE remove — where the group replaces the selected prim in the stack.
                if let Some(uid) = anchor_uid {
                    let order = s.scene.scene_graph.body_child_order(bnid);
                    insert_slot = order
                        .iter()
                        .position(|&r| r == BodyChildRef::Prim(uid))
                        .unwrap_or(order.len());
                }
                if let Some(prims) = s.scene.scene_graph.body_primitives_mut(bnid) {
                    if bidx < prims.len() {
                        let mut p = prims.remove(bidx);
                        // Sub-body carries this prim's boolean; inside the group it is the base.
                        p.operation = BooleanOp::Union { k: 0.0 };
                        first_prim = Some(p);
                    }
                }
            }

            let name = format!("Group.{:03}", {
                let id = s.scene.next_node_name_id;
                s.scene.next_node_name_id += 1;
                id
            });
            let parent_bt = s
                .scene
                .scene_graph
                .body_world_transform(parent)
                .unwrap_or_default();

            let mut primitives = Vec::new();
            if let Some(p) = first_prim {
                primitives.push(p); // FIRST in the sub-body
            }

            let sub = s.scene.scene_graph.add_child(
                parent,
                NodePayload::SdfBody {
                    name: name.clone(),
                    primitives,
                    mesh_cache_id: None,
                    body_op: group_op, // same boolean as selected prim
                    origin: parent_bt.origin,
                    body_rotation: parent_bt.rotation,
                    body_scale: parent_bt.scale,
                    mirror_flags: 0,
                    mirror_sign: 0,
                    mirror_center: Vec3::ZERO,
                    bevel_radius: 0.0,
                    blend_profile: core_math::primitives::BlendProfile::default(),
                    material: core_math::primitives::PbrMaterial::default(),
                    child_order: Vec::new(),
                    mesh_modifiers: Vec::new(),
                    field_modifiers: Vec::new(),
                },
            );

            s.scene.scene_graph.ensure_body_child_order(parent);
            s.scene.scene_graph.ensure_body_child_order(sub);
            // Parent: drop dangling Prim ref (if moved) and insert Body(sub) at the slot.
            s.scene.scene_graph.insert_child_order(
                parent,
                BodyChildRef::Body(sub),
                insert_slot.min(s.scene.scene_graph.body_child_order(parent).len()),
            );

            s.recalculate_body_origin(sub);
            s.recalculate_body_origin(parent);
            s.scene.active_body_nid = sub; // next add_primitive lands here (second+)
            s.sync_flat_cache();
            s.mark_node_body_vol_dirty(sub);
            s.mark_node_body_vol_dirty(parent);
            s.invalidate_pt_sdf_snapshot(sub);
            s.invalidate_pt_sdf_snapshot(parent);
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
            s.toast(&format!(
                "{name} — body boolean from selection; new prim adds after"
            ));
        });
    }

    /// Empty child sub-body under `parent` with the given body boolean (no anchor prim).
    fn create_empty_subbody_under(
        &mut self,
        parent: core_math::scene_graph::NodeId,
        body_op: core_math::primitives::BooleanOp,
    ) {
        use core_math::scene_graph::{BodyChildRef, NodePayload};
        self.transaction("New sub-body", |s| {
            let name = format!("Group.{:03}", {
                let id = s.scene.next_node_name_id;
                s.scene.next_node_name_id += 1;
                id
            });
            let parent_bt = s
                .scene
                .scene_graph
                .body_world_transform(parent)
                .unwrap_or_default();
            let sub = s.scene.scene_graph.add_child(
                parent,
                NodePayload::SdfBody {
                    name,
                    primitives: Vec::new(),
                    mesh_cache_id: None,
                    body_op,
                    origin: parent_bt.origin,
                    body_rotation: parent_bt.rotation,
                    body_scale: parent_bt.scale,
                    mirror_flags: 0,
                    mirror_sign: 0,
                    mirror_center: Vec3::ZERO,
                    bevel_radius: 0.0,
                    blend_profile: core_math::primitives::BlendProfile::default(),
                    material: core_math::primitives::PbrMaterial::default(),
                    child_order: Vec::new(),
                    mesh_modifiers: Vec::new(),
                    field_modifiers: Vec::new(),
                },
            );
            s.scene.scene_graph.ensure_body_child_order(parent);
            s.scene.scene_graph.ensure_body_child_order(sub);
            let tail = s.scene.scene_graph.body_child_order(parent).len();
            s.scene.scene_graph.insert_child_order(
                parent,
                BodyChildRef::Body(sub),
                tail,
            );
            s.scene.active_body_nid = sub;
            s.sync_flat_cache();
            s.mark_node_body_vol_dirty(parent);
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
        });
    }

    /// Create an empty SDF body (no prims) and select it.
    pub(crate) fn add_empty_sdf_body(&mut self) {
        self.create_new_sdf_body();
    }

    // ---- Polygonal Mesh --------------------------------------------------------

    /// Create an empty polygonal body (default box) and select it.
    pub(crate) fn add_empty_poly_body(&mut self) {
        use core_math::scene_graph::NodePayload;
        self.transaction("Add Polygon Mesh", |s| {
            let name = format!("Poly.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let mesh = core_math::half_edge::HalfEdgeMesh::new_box(glam::Vec3::splat(0.5));
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::PolygonalBody {
                name: name.clone(),
                mesh: std::sync::Arc::new(mesh),
                source: Default::default(),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
            });
            s.select_single_unified(SceneItemId::Node(nid));
            s.render.viewport_mode = ViewportRenderMode::Hybrid;
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {}", name));
        });
    }

    // ---- NURBS Surface ---------------------------------------------------------

    /// Create an empty NURBS surface (default plane) and select it.
    pub(crate) fn add_empty_nurbs_body(&mut self) {
        use core_math::scene_graph::NodePayload;
        use core_math::nurbs::NurbsSurface;
        self.transaction("Add NURBS Surface", |s| {
            let name = format!("NURBS.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let surface = NurbsSurface::new_plane(2.0, 2.0);
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::NurbsBody {
                name: name.clone(),
                surface: std::sync::Arc::new(surface),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
            });
            s.select_single_unified(SceneItemId::Node(nid));
            s.render.viewport_mode = ViewportRenderMode::Hybrid;
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {}", name));
        });
    }

    // ---- Group / Empty / Camera ------------------------------------------------

    /// Create an empty group node and select it.
    pub(crate) fn add_empty_group(&mut self) {
        use core_math::scene_graph::NodePayload;
        self.transaction("Add Group", |s| {
            let name = format!("Group.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::Group { name: name.clone() });
            s.select_single_unified(SceneItemId::Node(nid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {}", name));
        });
    }

    /// Multi-select bodies (and/or groups) → nest them under a new kitbash Group folder.
    /// Single body is intentional (RMB "Create Group" / Ctrl+G). Double-click the group to rename.
    ///
    /// If only prims are selected (no nestable nodes), falls back to `group_selected_into_body`.
    pub(crate) fn group_selected_bodies_or_prims(&mut self) {
        use core_math::scene_graph::NodePayload;

        // Collect selected Group / SdfBody / SdfObject nodes (topmost only).
        let mut selected: Vec<NodeId> = self
            .sel
            .usel
            .selected
            .iter()
            .filter_map(|id| match id {
                SceneItemId::Node(n) => {
                    let n = *n;
                    match self.scene.scene_graph.get(n).map(|node| &node.payload) {
                        Some(NodePayload::SdfBody { .. })
                        | Some(NodePayload::Group { .. })
                        | Some(NodePayload::SdfObject { .. }) => Some(n),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect();

        // Drop nodes whose ancestor is also selected (keep outermost).
        let sel_set: std::collections::HashSet<NodeId> = selected.iter().copied().collect();
        selected.retain(|&nid| {
            let mut cur = self.scene.scene_graph.get(nid).and_then(|n| n.parent);
            while let Some(p) = cur {
                if sel_set.contains(&p) {
                    return false;
                }
                cur = self.scene.scene_graph.get(p).and_then(|n| n.parent);
            }
            true
        });

        // 1+ body/group/object → new kitbash Group folder with them inside.
        if !selected.is_empty() {
            let n = selected.len();
            let also_prims = !self.selected_prim_indices_unified().is_empty();
            // Prefer shared parent when every selected node shares one non-SdfBody parent
            // (Root / Group / SdfObject). Group-under-SdfBody is forbidden (bake does not
            // walk Groups). Leaving a boolean SdfBody parent → extract to root + clear toast.
            let parents: Vec<Option<NodeId>> = selected
                .iter()
                .map(|&nid| self.scene.scene_graph.get(nid).and_then(|n| n.parent))
                .collect();
            let common_parent = parents[0].filter(|&p0| parents.iter().all(|p| *p == Some(p0)));
            let parent_is_sdf_body = common_parent.map_or(false, |p| {
                matches!(
                    self.scene.scene_graph.get(p).map(|n| &n.payload),
                    Some(NodePayload::SdfBody { .. })
                )
            });
            let any_leaving_body = selected
                .iter()
                .any(|&nid| self.scene.scene_graph.parent_body(nid).is_some());
            let group_parent = match common_parent {
                Some(p) if !parent_is_sdf_body => p,
                _ => self.scene.scene_graph.root(),
            };
            // Toast when we actually leave an SdfBody boolean parent (kitbash extract).
            let extracted_from_body = any_leaving_body;

            self.transaction("Create kitbash Group", |s| {
                let name = format!("Group.{:03}", {
                    let id = s.scene.next_node_name_id;
                    s.scene.next_node_name_id += 1;
                    id
                });
                let gid = s
                    .scene
                    .scene_graph
                    .add_child(group_parent, NodePayload::Group { name: name.clone() });
                for nid in selected {
                    let _ = s.reparent_body_node(nid, gid);
                }
                s.clear_selection_unified();
                s.select_single_unified(SceneItemId::Node(gid));
                s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
                s.mark_outliner_dirty();
                let count = if n == 1 {
                    "1 item".to_string()
                } else {
                    format!("{n} items")
                };
                let mut msg = format!(
                    "{name} ({count}) — double-click name to rename · Save to Library for kitbash"
                );
                if extracted_from_body {
                    let extracted = if n == 1 {
                        "1 body".to_string()
                    } else {
                        format!("{n} bodies")
                    };
                    msg = format!(
                        "Extracted {extracted} from body stack into {name} — undo (Ctrl+Z) to restore boolean nest"
                    );
                }
                if also_prims {
                    msg.push_str(" · selected prims were not included");
                }
                s.toast(&msg);
            });
            return;
        }

        // No body/group selection → prim multi-select boolean sub-body group
        self.group_selected_into_body(None);
    }

    /// Create an empty locator node and select it.
    pub(crate) fn add_empty_locator(&mut self) {
        use core_math::scene_graph::NodePayload;
        self.transaction("Add Empty", |s| {
            let name = format!("Locator.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::Empty { name: name.clone() });
            s.select_single_unified(SceneItemId::Node(nid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {}", name));
        });
    }

    /// Apply a mutation to a light in the scene graph.
    /// Use instead of writing to lights directly.
    ///
    /// ```ignore
    /// self.write_light(id, |l| l.visible = false);
    /// self.write_light(id, |l| l.intensity = 2.0);
    /// ```
    pub(crate) fn write_light<F: FnOnce(&mut LightData)>(&mut self, id: u32, f: F) {
        if let Some(data) = self.scene.light_by_id_mut(id) {
            f(data);
        }
    }

    /// Change a light's type (Directional / Point / Spot). Self-transacting.
    /// Preserves name, color, intensity, position, angles — only `light_type` flips.
    /// Light ids in the current selection — both `SceneItemId::Light` AND a
    /// `SceneItemId::Node` whose payload is a Light.
    ///
    /// A light can reach the selection either way depending on the route (viewport pick vs
    /// outliner row), and the `SceneItemId::Light`-only checks silently miss the node form:
    /// the light stops counting as selected, so its range sphere / handle chrome and the
    /// light-gizmo branch both drop out. Every "is a light selected" test should go through
    /// here rather than reading `usel` directly.
    pub(crate) fn selected_light_ids(&self) -> Vec<u32> {
        use core_math::scene_graph::NodePayload;
        let mut ids: Vec<u32> = self.sel.usel.selected_lights().collect();
        for nid in self.sel.usel.selected_nodes() {
            if let Some(n) = self.scene.scene_graph.get(nid) {
                if let NodePayload::Light(d) = &n.payload {
                    if !ids.contains(&d.id) {
                        ids.push(d.id);
                    }
                }
            }
        }
        ids
    }

    /// The active light, resolved from either selection form (see [`Self::selected_light_ids`]).
    pub(crate) fn active_light_id_any(&self) -> Option<u32> {
        use core_math::scene_graph::NodePayload;
        self.sel.usel.active_light().or_else(|| {
            self.sel.usel.active_node().and_then(|nid| {
                match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::Light(d)) => Some(d.id),
                    _ => None,
                }
            })
        })
    }

    /// True when `nid` is a Light node — an item whose TRANSFORM does not live on the node.
    ///
    /// A `NodePayload::Light` carries its own `position` / `azimuth` / `elevation` / `range` inside
    /// `LightData`; the node's `local_position` / `local_rotation` / `local_scale` are unused and
    /// stay at their defaults forever. Anything that transforms "a selected node" must therefore
    /// route lights to the light path instead — writing the node transform edits a field no
    /// renderer, packer or gizmo ever reads.
    pub(crate) fn is_light_node(&self, nid: core_math::scene_graph::NodeId) -> bool {
        matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(core_math::scene_graph::NodePayload::Light(_))
        )
    }

    /// Selected graph nodes whose transform really IS the node's own local transform — i.e.
    /// `selected_nodes()` minus lights (see [`Self::is_light_node`]).
    ///
    /// The counterpart of [`Self::selected_light_ids`]: together they partition the selection, so
    /// a light is transformed by exactly one path no matter which route selected it. Every
    /// node-transform site (gizmo targets, modal G/R/S, gizmo placement, framing) must use this
    /// rather than `usel.selected_nodes()`, and every "are only lights selected?" gate must test
    /// this for emptiness rather than `usel.has_nodes()`.
    pub(crate) fn selected_transform_nodes(&self) -> Vec<core_math::scene_graph::NodeId> {
        self.sel.usel.selected_nodes()
            .filter(|&nid| !self.is_light_node(nid))
            .collect()
    }

    pub(crate) fn set_light_type(&mut self, id: u32, new_type: core_math::scene_graph::LightType) {
        let cur = self.scene.light_by_id(id).map(|l| l.light_type);
        if cur == Some(new_type) {
            return;
        }
        self.transaction("Change light type", |s| {
            s.write_light(id, |l| l.light_type = new_type);
            s.sync_light_to_gpu();
            s.mark_dirty(DirtyFlags::LIGHT | DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
        });
    }

    /// Sync first **visible** light into the legacy GPU `light_config` struct.
    /// If every light is eye-hidden (or the graph has no lights), intensity and
    /// ambient are forced to 0 so deferred flat ambient cannot keep lighting
    /// the scene from a stale sun Ambient value.
    pub(crate) fn sync_light_to_gpu(&mut self) {
        let first_visible: Option<LightData> = self
            .scene
            .scene_graph
            .lights()
            .map(|(_, d)| d)
            .find(|l| l.visible)
            .cloned();
        if let Some(light) = first_visible {
            self.scene.light_config = LightConfig {
                azimuth: light.azimuth,
                elevation: light.elevation,
                intensity: light.intensity,
                color: light.color,
                ambient: light.ambient,
            };
        } else if let Some((_, light)) = self.scene.scene_graph.lights().next() {
            // Keep last-known direction/color for Solid form shading, but kill
            // intensity + ambient so PBR/Lit go dark when the sun is eye-hidden.
            self.scene.light_config = LightConfig {
                intensity: 0.0,
                ambient: 0.0,
                azimuth: light.azimuth,
                elevation: light.elevation,
                color: light.color,
            };
        } else {
            self.scene.light_config.intensity = 0.0;
            self.scene.light_config.ambient = 0.0;
        }
    }

    /// Create a camera node at the current view position and select it.
    pub(crate) fn add_camera(&mut self) {
        use core_math::scene_graph::NodePayload;
        // Capture camera state before entering transaction (avoids borrowing s.viewport inside closure while s is mutably borrowed)
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_target = self.viewport.camera.target;
        let cam_fov = self.render.fov_deg;
        self.transaction("Add Camera", |s| {
            let name = format!("Camera.{:03}", s.scene.next_node_name_id);
            s.scene.next_node_name_id += 1;
            let root = s.scene.scene_graph.root();
            let nid = s.scene.scene_graph.add_child(root, NodePayload::Camera {
                name: name.clone(),
                position: cam_pos,
                target: cam_target,
                fov: cam_fov,
            });
            s.select_single_unified(SceneItemId::Node(nid));
            s.mark_dirty(DirtyFlags::RENDER);
            s.mark_outliner_dirty();
            s.toast(&format!("Created {}", name));
        });
    }

    // ---- Subtree Duplication ---------------------------------------------------

    /// Generate a unique name by appending `.001`, `.002`, etc. based on existing
    /// names in the scene graph.
    pub(crate) fn next_unique_name(&self, base: &str) -> String {
        use core_math::scene_graph::NodePayload;
        // Strip trailing ".copy" chains and " (copy)" patterns first
        let mut s = base.to_string();
        while s.ends_with(".copy") {
            s.truncate(s.len() - 5);
        }
        while s.ends_with(" (copy)") {
            s.truncate(s.len() - 7);
        }
        // Strip any existing numeric suffix like ".001"
        let stem: &str = if let Some(dot_pos) = s.rfind('.') {
            if s[dot_pos + 1..].chars().all(|c| c.is_ascii_digit()) && dot_pos + 1 < s.len() {
                &s[..dot_pos]
            } else {
                &s
            }
        } else {
            &s
        };

        // Collect all display names in the scene graph
        let existing: std::collections::HashSet<String> = self.scene.scene_graph.all_nodes()
            .filter_map(|(_, n)| match &n.payload {
                NodePayload::Root => None,
                other => Some(other.display_name().to_string()),
            })
            .collect();

        for i in 1u32.. {
            let candidate = format!("{}.{:03}", stem, i);
            if !existing.contains(&candidate) {
                return candidate;
            }
        }
        format!("{}.999", stem) // fallback
    }

    /// Recursively duplicate a scene graph subtree under `parent`.
    /// Returns the NodeId of the duplicated root node.
    /// Cache ids are cleared; names are made unique; hierarchy is preserved.
    pub(crate) fn duplicate_subtree(
        &mut self,
        source_nid: core_math::scene_graph::NodeId,
        parent: core_math::scene_graph::NodeId,
    ) -> Option<core_math::scene_graph::NodeId> {
        // M72: depth-limited entry point — prevents infinite recursion on cyclic graphs
        self.duplicate_subtree_inner(source_nid, parent, 0)
    }

    fn duplicate_subtree_inner(
        &mut self,
        source_nid: core_math::scene_graph::NodeId,
        parent: core_math::scene_graph::NodeId,
        depth: usize,
    ) -> Option<core_math::scene_graph::NodeId> {
        const MAX_DEPTH: usize = 64;
        if depth >= MAX_DEPTH {
            eprintln!("[Azra] duplicate_subtree: recursion depth limit ({MAX_DEPTH}) reached — aborting");
            return None;
        }
        use core_math::scene_graph::NodePayload;

        // Read source node data (clone everything we need to avoid borrow issues)
        let (mut payload, pos, rot, scl, children) = {
            let node = self.scene.scene_graph.get(source_nid)?;
            (
                node.payload.clone(),
                node.local_position,
                node.local_rotation,
                node.local_scale,
                node.children.clone(),
            )
        };

        // Rename with unique suffix
        let new_name = self.next_unique_name(payload.display_name());
        match &mut payload {
            NodePayload::PolygonalBody { name, .. } => *name = new_name,
            NodePayload::NurbsBody { name, .. } => *name = new_name,
            NodePayload::NurbsSolid { name, .. } => *name = new_name,
            NodePayload::ImportedMesh { name, .. } => *name = new_name,
            NodePayload::VoxelBody { name, .. } => *name = new_name,
            NodePayload::SdfBody { name, .. } => *name = new_name,
            NodePayload::SdfObject { name, .. } => *name = new_name,
            NodePayload::Empty { name } => *name = new_name,
            NodePayload::Group { name } => *name = new_name,
            NodePayload::Camera { name, .. } => *name = new_name,
            NodePayload::Light(data) => data.name = new_name,
            NodePayload::Material { name, .. } => *name = new_name,
            NodePayload::Root => {}
        }

        // AZRA_MASTER_ROADMAP B.4: KEEP mesh_cache_id for shared GPU mesh (Poly/NURBS/SDF/Import).
        // ImportedMesh already kept its id. VoxelBody still clears (independent grid).
        // COW on first edit: `cow_mesh_cache_id_before_write` in poly_sync / bake paths.
        match &mut payload {
            NodePayload::PolygonalBody { .. }
            | NodePayload::NurbsBody { .. }
            | NodePayload::NurbsSolid { .. } => {
                // keep mesh_cache_id
            }
            NodePayload::SdfBody { primitives, child_order, .. } => {
                // keep mesh_cache_id — same baked surface until one body is edited
                // C3: Reassign prim_uid for every cloned prim to avoid UID collision
                for p in primitives.iter_mut() {
                    p.prim_uid = self.scene.next_prim_id;
                    self.scene.next_prim_id += 1;
                    p.instance_master_id = None;
                }
                // child_order: the cloned order references the SOURCE body's old prim uids and
                // child NodeIds — ALL stale after reassignment/re-id. Clear it; we rebuild a
                // canonical order below once the (re-id'd) prims + duplicated children exist.
                child_order.clear();
            }
            NodePayload::VoxelBody { mesh_cache_id, .. } => {
                *mesh_cache_id = None; // independent density grid
            }
            // C4: Reassign light ID to avoid ID collision
            NodePayload::Light(data) => {
                data.id = self.scene.next_light_id;
                self.scene.next_light_id += 1;
            }
            // C4b: Reassign MaterialId — two Material nodes must NEVER share an id
            // (material_by_id returns the first match → prims would resolve ambiguously).
            // Materials became user-duplicable with the outliner Materials section (Shift+D /
            // generic Duplicate route here); same max+1 scheme add_material uses.
            NodePayload::Material { id, .. } => {
                let max_id = self.scene.scene_graph.materials().map(|(_, m, _, _)| m.0).max().unwrap_or(0);
                *id = core_math::primitives::MaterialId(max_id + 1);
            }
            _ => {}
        }

        // Create the new node
        let new_nid = self.scene.scene_graph.add_child_with_transform(parent, payload, pos, rot, scl);

        // Recursively duplicate children (order preserved)
        for child_nid in children {
            self.duplicate_subtree_inner(child_nid, new_nid, depth + 1);
        }

        // child_order: now that the new body's re-id'd prims AND duplicated child bodies exist,
        // synthesise a canonical order (prims-first, then bodies — legacy eval order).
        self.scene.scene_graph.ensure_body_child_order(new_nid);

        Some(new_nid)
    }

    /// Slice-Duplicate: split the active body into two independently-editable bodies along a
    /// `HalfSpace` slice prim. The ORIGINAL keeps the slice as authored (Subtract → keeps side A);
    /// the DUPLICATE gets the SAME slice with its normal flipped 180° (keeps the opposite side B).
    /// Both bodies keep the live slice prim, so the split stays editable. `flat_idx` is the flat
    /// index of the selected slice prim.
    ///
    /// Reuses the canonical `duplicate_subtree()` (scene_actions.rs) — does NOT write a new
    /// duplicator. Runs the Structural Mutation Checklist for both bodies (membership is unchanged
    /// — duplicate copies prims 1:1 — so no origin recalc is needed; flat cache + volume dirty do).
    pub(crate) fn slice_duplicate(&mut self, flat_idx: usize) {
        use core_math::scene_graph::NodePayload;
        // Resolve body + the slice's body-local index from the flat prim index (read before mutating).
        let Some((body_nid, body_local_idx)) = self.scene.prim_body_map.get(flat_idx).copied() else {
            self.toast("Slice Duplicate: prim is not inside a body");
            return;
        };
        // Only a HalfSpace slice can be slice-duplicated.
        if !matches!(self.scene.primitives.get(flat_idx).map(|p| &p.shape), Some(SdfShape::HalfSpace)) {
            self.toast("Slice Duplicate: select a Slice (HalfSpace) prim first");
            return;
        }
        let parent = self.scene.scene_graph.get(body_nid).and_then(|n| n.parent)
            .unwrap_or(self.scene.scene_graph.root());
        let offset = Vec3::new(self.tweak.duplicate_offset, 0.0, 0.0);
        let mut new_body: Option<core_math::scene_graph::NodeId> = None;
        self.transaction("Slice Duplicate", |s| {
            let Some(new_nid) = s.duplicate_subtree(body_nid, parent) else { return; };
            new_body = Some(new_nid);
            if let Some(node) = s.scene.scene_graph.get_mut(new_nid) {
                if let NodePayload::SdfBody { origin, primitives, .. } = &mut node.payload {
                    // Offset the duplicate so the two halves are visibly separated (user moves further).
                    *origin += offset;
                    if !BODY_LOCAL_PRIMS {
                        for p in primitives.iter_mut() { p.position += offset; }
                    }
                    // Flip the duplicate's copy of the slice: rotate 180° about the prim's LOCAL X
                    // axis so its +Z plane normal points the opposite way → it keeps the OTHER half.
                    // Post-multiplying the current rotation by a 180° X turn and round-tripping
                    // through Euler composes correctly for ANY current orientation (not a naive
                    // Euler-component add, which misbehaves when the other angles are non-zero).
                    if let Some(slice) = primitives.get_mut(body_local_idx) {
                        let q = slice.rotation_quat() * Quat::from_rotation_x(std::f32::consts::PI);
                        let (ey, ex, ez) = q.to_euler(EulerRot::YXZ);
                        slice.rotation_deg = Vec3::new(ex.to_degrees(), ey.to_degrees(), ez.to_degrees());
                    }
                }
            }
            // Structural checklist: sync flat cache, allocate the duplicate's mesh-cache id, and mark
            // BOTH bodies' volumes dirty so each re-bakes with its own half of the slice.
            s.sync_flat_cache();
            if s.cache.use_body_vol_cache {
                let cid = s.cache.mesh_cache.alloc_id();
                if let Some(node) = s.scene.scene_graph.get_mut(new_nid) {
                    if let NodePayload::SdfBody { mesh_cache_id, .. } = &mut node.payload {
                        *mesh_cache_id = Some(cid);
                    }
                }
                s.cache.body_vol_cache.mark_body_dirty(new_nid);
            }
            s.mark_node_body_vol_dirty(new_nid);
            s.mark_node_body_vol_dirty(body_nid);
            s.scene.active_body_nid = new_nid;
            s.select_single_unified(SceneItemId::Node(new_nid));
            s.mark_dirty(DirtyFlags::SCENE);
            s.mark_outliner_dirty();
        });
        if new_body.is_some() {
            self.toast("Sliced into 2 bodies — move one half apart");
        }
    }

    /// Walk a duplicated subtree and sync mesh caches for any Poly/NURBS nodes.
    pub(crate) fn sync_duplicated_mesh_caches(&mut self, root_nid: core_math::scene_graph::NodeId) {
        // Collect all node ids in the subtree first to avoid borrow conflicts
        let nids: Vec<core_math::scene_graph::NodeId> = self.scene.scene_graph
            .walk_depth_first(root_nid)
            .map(|(_, nid, _)| nid)
            .collect();
        for nid in nids {
            let label = self.scene.scene_graph.get(nid)
                .map(|n| Self::node_type_label(&n.payload))
                .unwrap_or("");
            match label {
                "POLY" => self.poly_request_display_sync(nid),
                "NURBS" => self.nurbs_sync_mesh_cache(nid),
                _ => {}
            }
        }
    }

    // ---- Transform Reset -------------------------------------------------------

    /// Reset position to origin for all selected prims.
    ///
    /// With BODY_LOCAL_PRIMS=true, Vec3::ZERO is the body-local origin (i.e. the
    /// body's world origin). This is identical in both modes since the intent is
    /// "move the prim to sit at the body's centre point".
    pub(crate) fn reset_position(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        // Already at body-local origin → no empty undo.
        let needs = indices.iter().any(|&idx| {
            idx < self.scene.primitives.len()
                && self.scene.primitives[idx].position.length_squared() > 1e-12
        });
        if !needs { return; }
        self.transaction("Reset position", |s| {
            for idx in indices {
                if idx < s.scene.primitives.len() {
                    let rot = s.scene.primitives[idx].rotation_deg;
                    let scale = s.scene.primitives[idx].scale;
                    s.write_prim_transform(idx, Vec3::ZERO, rot, scale);
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
        });
    }

    /// Reset rotation to zero for all selected prims.
    pub(crate) fn reset_rotation(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        let needs = indices.iter().any(|&idx| {
            idx < self.scene.primitives.len()
                && self.scene.primitives[idx].rotation_deg.length_squared() > 1e-10
        });
        if !needs { return; }
        self.transaction("Reset rotation", |s| {
            for idx in indices {
                if idx < s.scene.primitives.len() {
                    let pos = s.scene.primitives[idx].position;
                    let scale = s.scene.primitives[idx].scale;
                    s.write_prim_transform(idx, pos, Vec3::ZERO, scale);
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
        });
    }

    /// Reset scale to (1,1,1) for all selected prims.
    pub(crate) fn reset_scale(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        let needs = indices.iter().any(|&idx| {
            idx < self.scene.primitives.len()
                && (self.scene.primitives[idx].scale - Vec3::ONE).length_squared() > 1e-12
        });
        if !needs { return; }
        self.transaction("Reset scale", |s| {
            for idx in indices {
                if idx < s.scene.primitives.len() {
                    let pos = s.scene.primitives[idx].position;
                    let rot = s.scene.primitives[idx].rotation_deg;
                    s.write_prim_transform(idx, pos, rot, Vec3::ONE);
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
        });
    }

    pub(crate) fn reset_elongation(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        let needs = indices.iter().any(|&idx| {
            idx < self.scene.primitives.len()
                && self.scene.primitives[idx].elongation.length_squared() > 1e-12
        });
        if !needs { return; }
        self.transaction("Reset elongation", |s| {
            for idx in indices {
                if idx < s.scene.primitives.len() {
                    s.scene.primitives[idx].elongation = Vec3::ZERO;
                    s.mirror_prim_to_graph(idx);
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
        });
    }

    /// Reset all transform components (position, rotation, scale) for selected prims.
    pub(crate) fn reset_transform(&mut self) {
        let indices = self.selected_prim_indices_unified();
        if indices.is_empty() { return; }
        let needs = indices.iter().any(|&idx| {
            if idx >= self.scene.primitives.len() { return false; }
            let p = &self.scene.primitives[idx];
            p.position.length_squared() > 1e-12
                || p.rotation_deg.length_squared() > 1e-10
                || (p.scale - Vec3::ONE).length_squared() > 1e-12
        });
        if !needs { return; }
        self.transaction("Reset transform", |s| {
            for idx in indices {
                if idx < s.scene.primitives.len() {
                    s.write_prim_transform(idx, Vec3::ZERO, Vec3::ZERO, Vec3::ONE);
                }
            }
            s.mark_dirty(DirtyFlags::SCENE);
        });
    }

    // ---- Body nest / un-nest ----------------------------------------------------

    /// Pure guards for nest/reparent — no mutation. Shared by `reparent_body_node` and
    /// outliner release (toast without empty undo).
    pub(crate) fn reparent_body_check(
        &self,
        child_nid: core_math::scene_graph::NodeId,
        new_parent_nid: core_math::scene_graph::NodeId,
    ) -> ReparentBodyResult {
        use core_math::scene_graph::NodePayload;
        let old_parent = self.scene.scene_graph.get(child_nid).and_then(|n| n.parent);
        if child_nid == new_parent_nid
            || old_parent == Some(new_parent_nid)
            || self.scene.scene_graph.is_descendant_of(new_parent_nid, child_nid)
        {
            return ReparentBodyResult::Skipped;
        }
        let child_is_group = matches!(
            self.scene.scene_graph.get(child_nid).map(|n| &n.payload),
            Some(NodePayload::Group { .. })
        );
        let new_is_group = matches!(
            self.scene.scene_graph.get(new_parent_nid).map(|n| &n.payload),
            Some(NodePayload::Group { .. })
        );
        let new_is_sdf_body = matches!(
            self.scene.scene_graph.get(new_parent_nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        );
        if child_is_group && new_is_sdf_body {
            return ReparentBodyResult::ForbiddenGroupUnderBody;
        }
        if new_is_group && self.scene.scene_graph.parent_body(new_parent_nid).is_some() {
            return ReparentBodyResult::ForbiddenGroupUnderBody;
        }
        ReparentBodyResult::Applied
    }

    /// Reparent an SdfBody under a new parent (nest as a sub-body when the parent is a body;
    /// un-nest to the graph root / an SdfObject otherwise). THE single nest/un-nest action —
    /// used by outliner body drag-and-drop (nest center-zone, cross-level edge drops, empty-area
    /// un-nest) so every path shares one code path (CLAUDE "two code paths" rule).
    ///
    /// The moved body's `body_op` is PRESERVED (owner-directed 2026-07-13): a nested body's
    /// body_op is its operand op in the parent's boolean chain — it already has one; never
    /// reset it on nest/un-nest.
    ///
    /// Runs the full Structural Mutation Checklist for BOTH the old and new parent:
    /// graph mutate → child_order reconcile → recalculate_body_origin → sync_flat_cache →
    /// mark_node_body_vol_dirty (which climbs to the top-level bake owners).
    /// Caller is responsible for the surrounding `transaction()` (undo).
    pub(crate) fn reparent_body_node(
        &mut self,
        child_nid: core_math::scene_graph::NodeId,
        new_parent_nid: core_math::scene_graph::NodeId,
    ) -> ReparentBodyResult {
        use core_math::scene_graph::NodePayload;
        let check = self.reparent_body_check(child_nid, new_parent_nid);
        if check != ReparentBodyResult::Applied {
            return check;
        }
        let old_parent = self.scene.scene_graph.get(child_nid).and_then(|n| n.parent);
        // Mesh objects: keep world placement when parenting (Blender parity). SDF boolean
        // nests intentionally leave local TRS alone (body origins recalculated below).
        let preserve_world = matches!(
            self.scene.scene_graph.get(child_nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. } | NodePayload::ImportedMesh { .. })
        );
        let world_mat = if preserve_world {
            Some(self.scene.scene_graph.world_transform(child_nid))
        } else {
            None
        };
        self.scene.scene_graph.reparent(child_nid, new_parent_nid);
        if let Some(world) = world_mat {
            let parent_world = self.scene.scene_graph.world_transform(new_parent_nid);
            let local = parent_world.inverse() * world;
            let (s, r, t) = local.to_scale_rotation_translation();
            if s.is_finite() && r.is_finite() && t.is_finite() {
                if let Some(node) = self.scene.scene_graph.get_mut(child_nid) {
                    node.local_position = t;
                    node.local_rotation = crate::transform::quat_to_euler_yxz_deg(r);
                    node.local_scale = s;
                }
            }
        }
        // child_order bookkeeping: drop the now-dangling Body ref from the old parent's order,
        // append a Body ref into the new parent's (end of its stack). ensure_* is a no-op for
        // non-SdfBody parents (Root / SdfObject / poly — those use plain node edges).
        if let Some(op) = old_parent {
            self.scene.scene_graph.ensure_body_child_order(op);
        }
        self.scene.scene_graph.ensure_body_child_order(new_parent_nid);
        // Structural Mutation Checklist steps 2–4 for ALL affected bodies (recalculate_body_origin
        // is a no-op for non-body nodes; mark_node_body_vol_dirty climbs to top-level owners).
        if let Some(op) = old_parent {
            self.recalculate_body_origin(op);
        }
        self.recalculate_body_origin(new_parent_nid);
        self.recalculate_body_origin(child_nid);
        self.sync_flat_cache();
        if let Some(op) = old_parent {
            self.mark_node_body_vol_dirty(op);
        }
        self.mark_node_body_vol_dirty(new_parent_nid);
        self.mark_node_body_vol_dirty(child_nid);
        self.mark_dirty(DirtyFlags::SCENE);
        self.mark_outliner_dirty();
        ReparentBodyResult::Applied
    }

    /// User-facing message when nest/reparent hits a kitbash/boolean topology rule.
    pub(crate) const REPARENT_GROUP_UNDER_BODY_MSG: &'static str =
        "Can't put a Group inside an SDF body (or nest into a Group under a body) — use a top-level kitbash Group";

    // ---- Node deletion (shared, no-undo core) ------------------------------------

    /// Delete ONE scene-graph node (any kind except Root) with full cache/state cleanup.
    /// THE single per-node deletion core (CLAUDE "two code paths" rule) — looped by
    /// `delete_selected` (Del), reused by the outliner context-menu delete. Merges the
    /// cleanup the three former inline copies each did partially: SDF subtree flat-prim
    /// removal, subtree mesh caches, volume cache, active-voxel working set, sdf_body_nid /
    /// active_body / isolation reassignment, shared-source purge, deselect.
    ///
    /// Caller is responsible for the surrounding `transaction()` and for the ONE-SHOT
    /// tail after the loop: `sync_flat_cache` + `sanitize_selection(_unified)` +
    /// `poly_sync_all_meshes` + `rebake_no_undo` + dirty/outliner marks.
    pub(crate) fn delete_node_no_undo(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        let Some(node) = self.scene.scene_graph.get(nid) else { return };
        if matches!(node.payload, NodePayload::Root) { return; }

        // Capture light id before removal so we can clear SceneItemId::Light selection too.
        let light_id = match &node.payload {
            NodePayload::Light(d) => Some(d.id),
            _ => None,
        };

        // Remove flat-cache prims belonging to this node's subtree (SdfBody descendants).
        let flat_indices: Vec<usize> = self.scene.prim_body_map.iter().enumerate()
            .filter(|(_, (body_nid, _))| {
                let mut cur = *body_nid;
                loop {
                    if cur == nid { return true; }
                    match self.scene.scene_graph.get(cur).and_then(|n| n.parent) {
                        Some(p) if p != cur => cur = p,
                        _ => return false,
                    }
                }
            })
            .map(|(fi, _)| fi)
            .collect();
        if !flat_indices.is_empty() {
            self.remove_prims_from_graph(&flat_indices);
        }
        // Clear edit_target if it pointed into the deleted subtree / went stale.
        if self.sel.edit_target.map_or(false, |i| i >= self.scene.primitives.len() || flat_indices.contains(&i)) {
            self.clear_edit_selection();
            if self.tool.work_mode != WorkMode::Object { self.tool.work_mode = WorkMode::Object; }
        }

        // Mesh caches for the ENTIRE subtree (poly / imported / SDF descendants).
        let mut cache_ids = Vec::new();
        Self::collect_subtree_caches(&self.scene.scene_graph, nid, &mut cache_ids);
        for cid in cache_ids { self.cache.mesh_cache.remove(cid); }
        self.cache.body_vol_cache.remove_body(nid);

        // Active voxel working set: node deletion must not leave sculpt.voxel.node dangling.
        if self.sculpt.voxel.as_ref().map_or(false, |v| v.node == nid) {
            if let Some(v) = self.sculpt.voxel.take() {
                for cid in v.chunks { self.cache.mesh_cache.remove(cid); }
            }
            self.tool.work_mode = WorkMode::Object;
        }

        // Material node: clear every reference BEFORE removal (prims → NONE, poly faces → 0).
        // add_material allocates max+1, so a dangling id could be REUSED by the next created
        // material and silently re-skin the old users — clearing here keeps the ONE deletion
        // path (Del key, outliner menu, delete_selected) safe without per-caller knowledge.
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::Material { id, .. } = &node.payload {
                let mid = *id;
                self.scene.scene_graph.clear_material_references(mid);
                self.cache.prim_buf_stale = true;
            }
        }

        self.scene.scene_graph.remove(nid);
        // Purge stale shared_source references (deleted body may be a clone source).
        self.cache.body_vol_cache.validate_shared_sources(&self.scene.scene_graph);

        // If we deleted the "primary" body, reassign to another top-level body — or
        // leave empty (NodeId(0)): do NOT auto-spawn an empty Body.001 (owner 2026-08-06).
        if nid == self.scene.sdf_body_nid {
            let root = self.scene.scene_graph.root();
            let top_bodies = self.scene.scene_graph.child_bodies(root);
            self.scene.sdf_body_nid = top_bodies
                .first()
                .copied()
                .unwrap_or(core_math::scene_graph::NodeId(0));
        }
        if self.scene.active_body_nid == nid {
            self.scene.active_body_nid = self.scene.sdf_body_nid;
        }
        if self.scene.isolated_body_nid == Some(nid) { self.scene.isolated_body_nid = None; }
        self.deselect_items(&[SceneItemId::Node(nid)]);
        if let Some(lid) = light_id {
            self.deselect_items(&[SceneItemId::Light(lid)]);
            self.mark_dirty(DirtyFlags::LIGHT);
        }
    }

    // ---- Light helpers (no longer needed: rebuild_light_id_to_index, sync_light_cache) ----

    /// Recalculate `next_light_id` to be greater than the max light id in the scene.
    /// Call after deserialization to prevent stale ID collisions.
    pub(crate) fn recalculate_next_light_id(&mut self) {
        let max_id = self.scene.scene_graph.lights().map(|(_, d)| d.id).max().unwrap_or(0);
        if self.scene.next_light_id <= max_id {
            self.scene.next_light_id = max_id + 1;
        }
    }

    /// Load-time self-heal: reassign fresh ids to lights whose `light.id` collides with an
    /// earlier light's. Selection/deletion resolve lights by id via `light_nid` (FIRST match),
    /// so a collision makes the second light unreachable and delete-by-id hit the wrong node.
    /// Old saves (and the former id-0 default) can carry such collisions — heal them once at
    /// load, then bump `next_light_id` past the new max.
    pub(crate) fn heal_duplicate_light_ids(&mut self) {
        let healed = heal_duplicate_light_ids(&mut self.scene.scene_graph);
        if healed > 0 {
            eprintln!("[SelfHeal] reassigned {} duplicate light id(s) on load", healed);
        }
        self.recalculate_next_light_id();
    }

}

// ---- scene-node select/deselect + glTF import (moved from raster_render.rs, Slice 21) ----
impl FourthLayerApp {
    /// Select a scene graph node (single-select), clearing primitive selection.
    /// Activates the translate gizmo so the user can immediately grab-move.
    pub(crate) fn select_scene_node(&mut self, nid: core_math::scene_graph::NodeId) {
        self.select_single_unified(SceneItemId::Node(nid));
        self.tool.gizmo_mode = app_types::GizmoMode::Translate;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Deselect the scene graph node.
    pub(crate) fn deselect_scene_node(&mut self) {
        // Remove all Node items from unified selection
        self.remove_from_selection(|id| matches!(id, SceneItemId::Node(_)));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Show file dialog and import a glTF file as ImportedMesh scene nodes.
    pub(crate) fn import_gltf_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("glTF", &["gltf", "glb"])
            .set_title("Import glTF")
            .pick_file()
        else { return };

        let path_str = path.to_string_lossy().to_string();
        match io_bridge::gltf_import::import_gltf(&path_str) {
            Ok(result) => {
                let root = self.scene.scene_graph.root();
                let mut total_tris = 0u32;
                let mesh_count = result.meshes.len();

                // Create a group node if multiple meshes
                let parent = if mesh_count > 1 {
                    self.scene.scene_graph.add_child(root, NodePayload::Group {
                        name: result.root_name.clone(),
                    })
                } else {
                    root
                };

                for mesh_data in result.meshes {
                    let tri_count = mesh_data.triangle_count();
                    total_tris += tri_count;

                    // Build PbrMaterial from glTF material (or default)
                    let pbr_mat = if let Some(idx) = mesh_data.material_idx {
                        if let Some(gmat) = result.materials.get(idx) {
                            let mut m = core_math::primitives::PbrMaterial::default();
                            m.albedo = gmat.albedo;
                            m.opacity = gmat.opacity;
                            m.roughness = gmat.roughness;
                            m.metallic = gmat.metallic;
                            m.emission_color = gmat.emission_color;
                            m.emission_intensity = gmat.emission_intensity;
                            m.name = gmat.name.clone();
                            m
                        } else {
                            core_math::primitives::PbrMaterial::default()
                        }
                    } else {
                        core_math::primitives::PbrMaterial::default()
                    };

                    // Insert into mesh cache
                    let cid = self.cache.mesh_cache.alloc_id();
                    let raster_data = mesh_data.to_raster_data();
                    self.cache.mesh_cache.insert_imported(cid, raster_data, tri_count);

                    // Add scene graph node
                    let new_nid = self.scene.scene_graph.add_child(parent, NodePayload::ImportedMesh {
                        name: mesh_data.name.clone(),
                        mesh_cache_id: core_math::scene_graph::MeshCacheId(cid.0),
                        materials: vec![pbr_mat],
                    });
                    // Auto-select the last imported node
                    self.select_single_unified(SceneItemId::Node(new_nid));
                }

                // Upload GPU buffers
                if let Some(gpu) = &self.gpu {
                    self.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue);
                }

                // Switch to hybrid mode to see the imported meshes
                self.render.viewport_mode = ViewportRenderMode::Hybrid;
                self.mark_dirty(DirtyFlags::RENDER);
                self.toast(&format!(
                    "\u{2705} {} \u{2014} {} mesh(es), {} triangles",
                    result.root_name, mesh_count, total_tris
                ));
            }
            Err(e) => {
                self.toast(&format!("\u{274C} glTF: {}", e));
            }
        }
    }
}

// =====================================================================
// Pure selection-structure helpers (headless-testable)
// =====================================================================

/// TOP-MOST ancestor filter (the 3aafa67 rule, extracted for reuse): drop any node whose
/// ANCESTOR is also in the set — subtree operations (duplicate, delete) on the ancestor
/// already cover it, so acting on the descendant too would double-apply. Preserves the
/// input order. Used by multi-node duplicate AND multi-node delete.
pub(crate) fn top_most_nodes(
    sg: &core_math::scene_graph::SceneGraph,
    nodes: &[core_math::scene_graph::NodeId],
) -> Vec<core_math::scene_graph::NodeId> {
    let set: std::collections::HashSet<core_math::scene_graph::NodeId> =
        nodes.iter().copied().collect();
    nodes.iter().copied()
        .filter(|&n| {
            let mut cur = sg.get(n).and_then(|x| x.parent);
            while let Some(p) = cur {
                if set.contains(&p) { return false; }
                cur = sg.get(p).and_then(|x| x.parent);
            }
            true
        })
        .collect()
}

/// Reassign fresh ids to lights whose id duplicates an earlier light's (deterministic
/// NodeId order — the first holder keeps the id). Returns how many lights were healed.
/// Pure graph logic so the load-time self-heal is headless-testable; wrapped by
/// `FourthLayerApp::heal_duplicate_light_ids` (which also bumps `next_light_id`).
pub(crate) fn heal_duplicate_light_ids(sg: &mut core_math::scene_graph::SceneGraph) -> usize {
    use core_math::scene_graph::NodePayload;
    let mut lights: Vec<(core_math::scene_graph::NodeId, u32)> =
        sg.lights().map(|(nid, d)| (nid, d.id)).collect();
    lights.sort_by_key(|(nid, _)| nid.0); // HashMap iteration is random — sort (CLAUDE rule)
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut next_free = lights.iter().map(|(_, id)| *id).max().unwrap_or(0) + 1;
    let mut healed = 0usize;
    for (nid, id) in lights {
        if seen.insert(id) { continue; }
        // Collision: give this light the next fresh id.
        if let Some(node) = sg.get_mut(nid) {
            if let NodePayload::Light(d) = &mut node.payload {
                eprintln!(
                    "[SelfHeal] light '{}' had duplicate id {} → reassigned {}",
                    d.name, id, next_free
                );
                d.id = next_free;
                seen.insert(next_free);
                next_free += 1;
                healed += 1;
            }
        }
    }
    healed
}

/// The ONE body shared by every selected prim (group gate). `Ok(Some(body))` = all selected
/// prims live in that single body; `Ok(None)` = no resolvable body (empty/stale selection —
/// caller falls back to active_body); `Err(())` = the selection SPANS bodies (reject).
pub(crate) fn shared_prim_body(
    prim_body_map: &[(core_math::scene_graph::NodeId, usize)],
    selected_flat: &[usize],
) -> Result<Option<core_math::scene_graph::NodeId>, ()> {
    let mut shared: Option<core_math::scene_graph::NodeId> = None;
    for &fi in selected_flat {
        let Some(&(nid, _)) = prim_body_map.get(fi) else { continue };
        match shared {
            None => shared = Some(nid),
            Some(s) if s != nid => return Err(()),
            _ => {}
        }
    }
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::{heal_duplicate_light_ids, shared_prim_body, top_most_nodes};
    use core_math::scene_graph::{LightData, NodePayload, SceneGraph};

    fn nid(g: &mut SceneGraph, parent: core_math::scene_graph::NodeId) -> core_math::scene_graph::NodeId {
        g.add_child(parent, NodePayload::Empty { name: "n".into() })
    }

    #[test]
    fn top_most_filter_drops_descendants_keeps_order() {
        let mut g = SceneGraph::new();
        let root = g.root();
        let a = nid(&mut g, root);
        let b = nid(&mut g, a);      // child of a
        let c = nid(&mut g, b);      // grandchild of a
        let d = nid(&mut g, root);   // independent
        assert_eq!(top_most_nodes(&g, &[d, a, b, c]), vec![d, a]);
        // No ancestors selected → passthrough.
        assert_eq!(top_most_nodes(&g, &[b, d]), vec![b, d]);
    }

    fn add_light(g: &mut SceneGraph, id: u32, name: &str) -> core_math::scene_graph::NodeId {
        let mut l = LightData::default();
        l.id = id;
        l.name = name.into();
        let root = g.root();
        g.add_child(root, NodePayload::Light(l))
    }

    fn light_ids_sorted_by_node(g: &SceneGraph) -> Vec<(u64, u32)> {
        let mut v: Vec<(u64, u32)> = g.lights().map(|(nid, d)| (nid.0, d.id)).collect();
        v.sort();
        v
    }

    #[test]
    fn default_light_id_is_allocated_range() {
        // The default-scene light must carry a real allocator id (1), matching "Sun.001"
        // and the scenes' next_light_id = 2 init — never the out-of-range 0.
        assert_eq!(LightData::default().id, 1);
        assert_eq!(LightData::default().name, "Sun.001");
    }

    #[test]
    fn heal_reassigns_duplicate_light_ids_first_holder_keeps() {
        let mut g = SceneGraph::new();
        let a = add_light(&mut g, 1, "Sun.001");
        let b = add_light(&mut g, 1, "Sun.001 (dup)"); // the collision (old-save class)
        let c = add_light(&mut g, 3, "Point.003");
        assert_eq!(heal_duplicate_light_ids(&mut g), 1);
        let ids = light_ids_sorted_by_node(&g);
        // First holder (lowest NodeId) keeps id 1; duplicate got a fresh id > max(3).
        assert_eq!(ids.iter().find(|(n, _)| *n == a.0).unwrap().1, 1);
        assert_eq!(ids.iter().find(|(n, _)| *n == b.0).unwrap().1, 4);
        assert_eq!(ids.iter().find(|(n, _)| *n == c.0).unwrap().1, 3);
        // All unique now; light_by_id resolves each light unambiguously.
        let uniq: std::collections::HashSet<u32> = ids.iter().map(|(_, id)| *id).collect();
        assert_eq!(uniq.len(), 3);
        assert_eq!(g.light_by_id(4).unwrap().0, b);
        // Idempotent: a healed graph heals zero.
        assert_eq!(heal_duplicate_light_ids(&mut g), 0);
    }

    #[test]
    fn heal_noop_on_unique_ids() {
        let mut g = SceneGraph::new();
        add_light(&mut g, 1, "Sun.001");
        add_light(&mut g, 2, "Point.002");
        assert_eq!(heal_duplicate_light_ids(&mut g), 0);
        assert_eq!(
            light_ids_sorted_by_node(&g).iter().map(|(_, id)| *id).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn shared_prim_body_gate() {
        let mut g = SceneGraph::new();
        let root = g.root();
        let b1 = nid(&mut g, root);
        let b2 = nid(&mut g, root);
        let map = vec![(b1, 0), (b1, 1), (b2, 0)];
        // All in ONE body (even if it's not the "active" one) → that body wins.
        assert_eq!(shared_prim_body(&map, &[0, 1]), Ok(Some(b1)));
        assert_eq!(shared_prim_body(&map, &[2]), Ok(Some(b2)));
        // Spanning bodies → reject.
        assert_eq!(shared_prim_body(&map, &[0, 2]), Err(()));
        // Empty / stale-only selection → ambiguity (caller falls back to active_body).
        assert_eq!(shared_prim_body(&map, &[]), Ok(None));
        assert_eq!(shared_prim_body(&map, &[99]), Ok(None));
    }
}
