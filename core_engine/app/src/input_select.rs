//! Click-select and object click handlers — moved from input.rs.

use eframe::egui;
use crate::*;

impl AzraApp {
    /// Selection-undo wrapper: capture the pre-click selection, run the real click-select, then record
    /// a coalesced selection undo entry if it changed (Blender parity: Ctrl+Z reverts selection first).
    /// One choke point covers object / poly sub-element / NURBS select — all route through the inner fn.
    pub(crate) fn handle_click_select(&mut self, response: &egui::Response, rect: egui::Rect, shift: bool, ctrl: bool, alt: bool) {
        let pre = self.capture_selection_snapshot();
        self.handle_click_select_inner(response, rect, shift, ctrl, alt);
        // Record only if the selection actually moved (the recorder itself also coalesces vs the last
        // recorded state, so a no-op click and a re-select of the same element record nothing).
        if !self.capture_selection_snapshot().same_as(&pre) {
            self.record_selection_undo(pre);
            // Default orbit pivot follows selection (object / light / body / poly elements).
            self.sync_orbit_center_to_selection();
        }
    }

    fn handle_click_select_inner(&mut self, response: &egui::Response, rect: egui::Rect, shift: bool, ctrl: bool, alt: bool) {
        // Exit cycle mode on any viewport click
        if self.ui.sdf_cycle_mode {
            self.ui.sdf_cycle_mode = false;
        }
        // A click-consuming poly EDIT tool (Knife places a cut point, Extrude/Inset/Bevel/LoopCut arm/confirm)
        // OWNS the click — it already handled it. Do NOT run any selection/deselection here. Without this,
        // a knife click that MISSES the mesh (empty space) fell through to the Object-mode branch below and
        // deselected the body → pushed the user out of edit mode (undoable, so Ctrl+Z restored the knife).
        if self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && self.poly_tool != PolyTool::Idle
        {
            return;
        }
        // Automatic pivot place is Shift+RMB only (contextual — see input_viewport).
        // Do NOT steal Shift+LMB here: that remains multi-select in every work mode.
        // Depth pick confirm: LMB during depth pick accepts the current pick
        if self.tool.depth_pick.is_some() {
            self.tool.depth_pick = None;
        }
        else if self.tool.current.is_transform() { self.confirm_modal_transform(); }
        else if self.tool.current.is_poly_transform() { self.poly_confirm_transform(); }
        // Edit mode face selection
        else if self.sel.edit_target.is_some() && self.tool.work_mode == WorkMode::Face
                && self.tool.gizmo_hovered.is_none() && !self.tool.current.is_face_push_pull() {
            if let Some((_idx, face_n)) = self.tool.hovered_face {
                if let Some(pos) = self.sel.selected_faces.iter().position(|n| (*n - face_n).length() < 0.01) {
                    if shift { self.sel.selected_faces.remove(pos); }
                    else { self.sel.selected_faces.clear(); self.sel.selected_faces.push(face_n); }
                } else {
                    if shift { self.sel.selected_faces.push(face_n); }
                    else { self.sel.selected_faces.clear(); self.sel.selected_faces.push(face_n); }
                }
            } else {
                self.sel.selected_faces.clear();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // Polygon sub-element selection (Vertex/Edge/Face modes)
        else if self.modeling_context == ModelingContext::Polygonal
                && self.poly_work_mode != PolyWorkMode::Object
                && self.tool.gizmo_hovered.is_none() {
            // Single click = normal sub-element select. Edge Alt gestures are owned by the
            // Alt+LMB path in input_viewport (`poly_release_click_select` before this):
            //   Alt+click = loop, Alt+double-click = ring, Ctrl+Alt+click = ring.
            // Free-center is Alt+LMB *drag*; plain LMB is paint / click-select.
            self.poly_click_select(shift);
            // Element Action Center: click updates center/normal without requiring re-select.
            if self.tool.action_center == crate::ActionCenter::Element {
                let _ = self.action_center_try_element_pick();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // NURBS modeless / Face / Edge / Vertex selection (Plasticity-style)
        else if self.modeling_context == ModelingContext::Nurbs
                && self.tool.gizmo_hovered.is_none()
                && self.nurbs_solid_root().is_some() {
            self.nurbs_click_select(shift);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        // SDF prim-level selection (Edit/Vertex mode — individual prim picking)
        else if self.modeling_context == ModelingContext::Sdf
                && self.tool.work_mode == WorkMode::Vertex
                && self.tool.gizmo_hovered.is_none() && self.tool.hovered_bevel_edge.is_none() {
            self.handle_sdf_edit_click(response, rect, ctrl || shift, alt);
        }
        // Object mode selection (Body-level for SDF, Node-level for others).
        // Poly Object mode uses `poly_work_mode`, not only `tool.work_mode` — both must
        // route here so empty-space click can deselect a poly body (owner 2026-08-08).
        else if self.tool.gizmo_hovered.is_none()
            && self.tool.hovered_bevel_edge.is_none()
            && (self.tool.work_mode == WorkMode::Object
                || (self.modeling_context == ModelingContext::Polygonal
                    && self.poly_work_mode == PolyWorkMode::Object)
                || (self.modeling_context == ModelingContext::Nurbs
                    && self.nurbs_work_mode == NurbsWorkMode::Object))
        {
            self.handle_object_click(response, rect, ctrl || shift, alt);
        }
    }

    /// SDF edit mode: click selects individual prims within the active body.
    pub(crate) fn handle_sdf_edit_click(&mut self, response: &egui::Response, rect: egui::Rect, ctrl: bool, alt: bool) {
        if let Some(mp) = response.interact_pointer_pos() {
            let mut hit = self.raycast_select(mp, rect, alt);
            if let Some(h) = hit {
                if !ctrl { self.deselect_scene_node(); }
                let uid = self.scene.primitives[h].prim_uid;
                if ctrl {
                    self.toggle_select_unified(SceneItemId::Prim(uid));
                } else {
                    self.select_single_unified(SceneItemId::Prim(uid));
                }
                self.tool.gizmo_mode = app_types::GizmoMode::Translate;
                self.tool.suppress_gizmo_drag = true;
            } else if !ctrl {
                // Same empty-click tool-drop escalation as Object mode (see handle_object_click).
                if !self.drop_active_tool_only() {
                    self.deselect_scene_node();
                    self.deselect_all();
                }
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }

    pub(crate) fn handle_object_click(&mut self, response: &egui::Response, rect: egui::Rect, ctrl: bool, alt: bool) {
        if let Some(mp) = response.interact_pointer_pos() {
            // Prefer light handles (zone/soft/range) so a drag doesn't re-select the icon.
            if self.light_handle_hit_test(mp, rect).is_some() {
                // Handle press is started in input_viewport; don't deselect.
            } else if let Some(light_id) = self.light_gizmo_hit_test(mp, rect) {
                if ctrl {
                    self.toggle_select_unified(SceneItemId::Light(light_id));
                } else {
                    self.select_light(light_id);
                }
                self.tool.suppress_gizmo_drag = true;
            } else {
                let mut node_hit = false;
                if self.render.viewport_mode == ViewportRenderMode::Hybrid {
                    let px = (mp.x - rect.min.x) as u32;
                    let py = (mp.y - rect.min.y) as u32;
                    let w = rect.width() as u32;
                    let h = rect.height() as u32;
                    if let Some(nid) = self.pick_node_at(px, py, w, h, ctrl, false) {
                        if let Some(node) = self.scene.scene_graph.get(nid) {
                            match &node.payload {
                                core_math::scene_graph::NodePayload::ImportedMesh { .. }
                                | core_math::scene_graph::NodePayload::VoxelBody { .. }
                                | core_math::scene_graph::NodePayload::PolygonalBody { .. }
                                | core_math::scene_graph::NodePayload::NurbsBody { .. } => {
                                    if ctrl {
                                        self.toggle_select_unified(SceneItemId::Node(nid));
                                    } else {
                                        self.select_scene_node(nid);
                                    }
                                    self.tool.suppress_gizmo_drag = true;
                                    node_hit = true;
                                }
                                core_math::scene_graph::NodePayload::SdfBody { .. } => {
                                    // Body mode: select SDF body as a whole node
                                    self.scene.active_body_nid = nid;
                                    if ctrl {
                                        self.toggle_select_unified(SceneItemId::Node(nid));
                                    } else {
                                        self.select_scene_node(nid);
                                    }
                                    self.tool.suppress_gizmo_drag = true;
                                    node_hit = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // `pick_node_at` ALWAYS returns None on the click frame (async readback), so
                // `node_hit` is false even when the GPU will resolve a node next frame.
                let pick_pending = self.pending_pick.is_some();
                if !node_hit {
                    let mut hit = self.raycast_select(mp, rect, alt);
                    if let Some(h) = hit {
                        // Resolve which body this prim belongs to
                        let clicked_body = if h < self.scene.prim_body_map.len() {
                            self.scene.prim_body_map[h].0
                        } else {
                            self.scene.active_body_nid
                        };
                        // Body mode: select the whole body, not individual prims
                        self.scene.active_body_nid = clicked_body;
                        if ctrl {
                            self.toggle_select_unified(SceneItemId::Node(clicked_body));
                        } else {
                            self.select_scene_node(clicked_body);
                        }
                        self.tool.suppress_gizmo_drag = true;
                    } else {
                        // `raycast_select` only marches the analytical SDF prim field — it is
                        // BLIND to poly / imported / voxel / NURBS mesh bodies. Resolve those on
                        // the SAME frame with the CPU mesh raycast instead of falling through to
                        // a deselect that `handle_pending_pick` then undoes a frame later (the
                        // select→deselect→reselect flicker). CPU selection state only: no GPU
                        // resource is touched here (Frame Ordering Contract).
                        let (ro, rd) = self.cam_ray(mp, rect);
                        if let Some((_, _, nid)) = self.raycast_scene_meshes(ro, rd) {
                            if let Some(node) = self.scene.scene_graph.get(nid) {
                                if matches!(&node.payload,
                                    core_math::scene_graph::NodePayload::SdfBody { .. }) {
                                    self.scene.active_body_nid = nid;
                                }
                            }
                            if ctrl {
                                self.toggle_select_unified(SceneItemId::Node(nid));
                            } else {
                                self.select_scene_node(nid);
                            }
                            // Tell the in-flight GPU pick this click is already resolved,
                            // or its confirm next frame re-toggles the same node OFF.
                            if let Some(p) = self.pending_pick.as_mut() {
                                p.cpu_resolved = Some(nid);
                            }
                            self.tool.suppress_gizmo_drag = true;
                        } else if !ctrl {
                            // Empty-click tool-drop escalation (same as SelectionBox tiny-box release
                            // and poly_release_click_select): plain empty LMB drops the armed tool
                            // (G/R/S gizmo, SDF Bevel/Extrude mode, poly tool) and KEEPS selection;
                            // only a second empty click falls through to deselect.
                            //
                            // Always attempt the drop on THIS frame even when Hybrid left a
                            // `pending_pick` live — otherwise the GPU empty finish deselects first
                            // (or deselects on the same gesture as the drop). When pick is pending,
                            // do NOT deselect here; `handle_pending_pick` finishes empty with the
                            // same drop-then-deselect ladder.
                            if self.drop_active_tool_only() {
                                // Tool dropped; keep selection. Abort in-flight Hybrid pick so
                                // next frame's GPU miss does not deselect the prim we kept.
                                // MUST unmap `picking_staging` — bare `pending_pick = None`
                                // left the buffer mapped and the next submit panicked
                                // ("picking_staging is still mapped", 2026-08-10 crash).
                                self.cancel_pending_pick();
                            } else if !pick_pending {
                                self.deselect_scene_node();
                                self.deselect_all();
                            }
                        }
                    }
                }
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }
}
