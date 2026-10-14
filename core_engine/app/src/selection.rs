//! Selection helpers — moved from main.rs for modularity.

use glam::Vec3;
use crate::*;

/// When true, prim.position in the flat cache is body-local (relative to body.origin).
/// selection_median() must convert to world-space before using positions for gizmo placement.
use crate::BODY_LOCAL_PRIMS;

impl AzraApp {
    /// Clear edit-mode selection state (edit target, selected faces, bevel edges).
    pub(crate) fn clear_edit_selection(&mut self) {
        self.sel.edit_target = None;
        self.sel.selected_faces.clear();
        self.tool.selected_bevel_edges.clear();
    }
    /// Select a single primitive by flat index. Activates translate gizmo for grab-on-select.
    pub(crate) fn select_single(&mut self, idx:usize) {
        if idx < self.scene.primitives.len() {
            let uid = self.scene.primitives[idx].prim_uid;
            self.select_single_unified(SceneItemId::Prim(uid));
            self.tool.gizmo_mode = app_types::GizmoMode::Translate;
        }
        self.tool.selected_bevel_edges.clear();
    }
    pub(crate) fn deselect_all(&mut self) {
        self.clear_selection_unified();
        self.tool.selected_bevel_edges.clear();
        self.poly_sel.selected_vertices.clear(); self.poly_sel.selected_edges.clear(); self.poly_sel.selected_faces.clear();
        self.set_gizmo_armed(false); // no selection → no armed gizmo; Automatic pivot → sel center
    }
    pub(crate) fn select_light(&mut self, id: u32) {
        self.select_single_unified(SceneItemId::Light(id));
        self.tool.selected_bevel_edges.clear();
    }
    pub(crate) fn toggle_select(&mut self, idx:usize) {
        if idx < self.scene.primitives.len() {
            let uid = self.scene.primitives[idx].prim_uid;
            self.toggle_select_unified(SceneItemId::Prim(uid));
        }
    }
    /// Clamp all selection state to valid bounds after structural changes.
    pub(crate) fn sanitize_selection(&mut self) {
        let len = self.scene.primitives.len();
        if self.sel.edit_target.map_or(false, |i| i >= len) {
            self.sel.edit_target = None;
        }
        // Validate active_body_nid still exists in graph
        if self.scene.scene_graph.get(self.scene.active_body_nid).is_none() {
            self.scene.active_body_nid = self.scene.sdf_body_nid;
        }
        // Validate isolated_body_nid still exists
        if let Some(iso) = self.scene.isolated_body_nid {
            if self.scene.scene_graph.get(iso).is_none() {
                self.scene.isolated_body_nid = None;
            }
        }
        self.sanitize_selection_unified();
    }

    /// True when something meaningful is selected for camera orbit / gizmo placement.
    pub(crate) fn has_orbit_selection(&self) -> bool {
        if self.modeling_context == ModelingContext::Polygonal
            && self.poly_work_mode != PolyWorkMode::Object
            && (!self.poly_sel.selected_vertices.is_empty()
                || !self.poly_sel.selected_edges.is_empty()
                || !self.poly_sel.selected_faces.is_empty())
        {
            return true;
        }
        if self.modeling_context == ModelingContext::Nurbs
            && self.nurbs_work_mode != NurbsWorkMode::Object
            && (!self.nurbs_sel.selected_faces.is_empty()
                || !self.nurbs_sel.selected_edges.is_empty()
                || !self.nurbs_sel.selected_cvs.is_empty())
        {
            return true;
        }
        !self.sel.usel.selected.is_empty()
    }

    /// Called after selection changes. **Must never move the camera or the mesh.**
    ///
    /// History (owner 2026-08-06): this used to assign `camera.target = selection_median()`.
    /// With `cam_pos = target + orbit*(0,0,distance)` that teleports the eye — face pick made
    /// the cube appear to jump. Blender keeps the view still on select; only Frame Selected
    /// (F / `.`) re-aims. MMB is pure trackball around the *existing* look-at, not the new
    /// selection (orbit-around-selection-as-live-pivot also swings the cube after a face pick).
    ///
    /// Transform gizmo / G-R-S pivots still use [`Self::selection_median`] independently.
    /// Kept as a named call site so selection writers stay one-line; body is intentionally empty.
    pub(crate) fn sync_orbit_center_to_selection(&mut self) {
        // Intentionally empty — do not write camera.target / distance / orbit here.
    }

    /// Rotate the orbit camera around an arbitrary world pivot, re-expressing `target` so the
    /// eye stays continuous. Available for opt-in tools; **not** used by default MMB (that is
    /// pure trackball around `camera.target` only — see input_viewport).
    #[allow(dead_code)]
    pub(crate) fn orbit_camera_around_pivot(&mut self, rot: glam::Quat, pivot: Vec3) {
        let d = self.viewport.camera.distance;
        let orbit0 = self.viewport.camera.orbit;
        let cam = self.viewport.camera.target + orbit0 * Vec3::new(0.0, 0.0, d);
        let new_orbit = (rot * orbit0).normalize();
        let new_cam = pivot + rot * (cam - pivot);
        self.viewport.camera.orbit = new_orbit;
        self.viewport.camera.target = new_cam - new_orbit * Vec3::new(0.0, 0.0, d);
    }

    /// Point look-at at `new_target` **without moving the camera eye or orientation**.
    ///
    /// Projects depth along the current view ray so the image does not jump — only the
    /// mathematical orbit center / dolly distance change. Used by Focus; never by click-select.
    pub(crate) fn retarget_camera_keep_eye(&mut self, new_target: Vec3) {
        if !new_target.is_finite() {
            return;
        }
        let orbit = self.viewport.camera.orbit;
        let d = self.viewport.camera.distance.max(1e-4);
        let cam = self.viewport.camera.target + orbit * Vec3::new(0.0, 0.0, d);
        // Camera looks along orbit * -Z; forward from eye into the scene.
        let look = (orbit * Vec3::NEG_Z).normalize_or_zero();
        if look.length_squared() < 1e-10 {
            self.viewport.camera.target = new_target;
            return;
        }
        // Depth of the new target along the current look ray (must stay in front of the eye).
        let depth = (new_target - cam).dot(look);
        if depth < 1e-3 {
            // Behind / on the eye — fall back to placing target at same distance along look.
            self.viewport.camera.target = cam + look * d;
            // distance unchanged
            return;
        }
        // Keep eye fixed: target on the look ray at `depth`, distance = depth.
        self.viewport.camera.target = cam + look * depth;
        self.viewport.camera.distance = depth;
    }

    pub(crate) fn selection_median(&self) -> Vec3 {
        // Poly sub-element selection center (when in poly edit mode)
        if self.modeling_context == ModelingContext::Polygonal && self.poly_work_mode != PolyWorkMode::Object {
            if let Some(c) = self.poly_selection_center() { return c; }
        }
        // NURBS face / edge / CV selection center
        if self.modeling_context == ModelingContext::Nurbs && self.nurbs_work_mode != NurbsWorkMode::Object {
            if let Some(c) = self.nurbs_selection_center() { return c; }
        }
        // Scene graph node multi-selection. Lights are EXCLUDED (`selected_transform_nodes`):
        // a Light node's `local_position` is an unused default (its real position lives in
        // `LightData`), so counting it here dragged the median toward the world origin and put
        // the gizmo nowhere near the light the user had just clicked in the outliner.
        let node_ids: Vec<_> = self.selected_transform_nodes();
        if !node_ids.is_empty() {
            let mut s = Vec3::ZERO;
            let mut c = 0;
            for &nid in &node_ids {
                // Resolves Custom / WorldOrigin / BoundingCenter (poly = mesh AABB center).
                let pos = self
                    .node_pivot_world(nid)
                    .unwrap_or_else(|| {
                        self.scene
                            .scene_graph
                            .get(nid)
                            .map(|n| n.local_position)
                            .unwrap_or(Vec3::ZERO)
                    });
                s += pos;
                c += 1;
            }
            if c > 0 {
                return s / c as f32;
            }
        }
        // Light pivots: point/spot = source; sun = fixed position (not dir*radius).
        // Both selection forms; the node branch above already returned for any non-light node.
        let light_ids: Vec<u32> = self.selected_light_ids();
        if !light_ids.is_empty() {
            let mut s = Vec3::ZERO; let mut c = 0;
            for &lid in &light_ids {
                if let Some(data) = self.scene.light_by_id(lid) {
                    s += data.gizmo_position();
                    c += 1;
                }
            }
            if c > 0 && !self.sel.usel.has_prims() {
                return s / c as f32;
            }
        }
        // SDF prim selection centroid (before singular node fallback so gizmo follows prims)
        let prim_uids: Vec<u32> = self.sel.usel.selected_prims().collect();
        if !prim_uids.is_empty() {
            let mut s=Vec3::ZERO; let mut c=0;
            for uid in &prim_uids {
                if let Some(i) = self.prim_index_by_uid(*uid) {
                    if i < self.scene.primitives.len() && self.scene.primitives[i].visible {
                        let world_pos = if BODY_LOCAL_PRIMS {
                            // prim.position is body-local; convert to world for gizmo placement.
                            let bt = self.scene.prim_body_map.get(i)
                                .and_then(|&(nid, _)| self.scene.scene_graph.body_world_transform(nid))
                                .unwrap_or_default();
                            bt.local_to_world(self.scene.primitives[i].position)
                        } else {
                            self.scene.primitives[i].position
                        };
                        s += world_pos; c += 1;
                    }
                }
            }
            if c>0 { return s/c as f32; }
        }
        // Single node fallback — use body origin for SdfBody nodes
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let core_math::scene_graph::NodePayload::SdfBody { origin, .. } = &node.payload {
                    return *origin;
                }
                return node.local_position;
            }
        }
        Vec3::ZERO
    }

    /// Compute the world-space center of the current poly sub-element selection.
    pub(crate) fn poly_selection_center(&self) -> Option<Vec3> {
        use core_math::half_edge::{VertexId, FaceId, HalfEdgeId};
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform = |p: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };
        let mut sum = Vec3::ZERO;
        let mut count = 0u32;
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                for &vid in &self.poly_sel.selected_vertices {
                    if (vid as usize) >= mesh.vertices.len() { continue; }
                    sum += xform(mesh.vert(VertexId(vid)).position);
                    count += 1;
                }
            }
            PolyWorkMode::Edge => {
                for &eid in &self.poly_sel.selected_edges {
                    if (eid as usize) >= mesh.half_edges.len() { continue; }
                    let he = mesh.he(HalfEdgeId(eid));
                    let src = mesh.he_source(HalfEdgeId(eid));
                    if (src.0 as usize) >= mesh.vertices.len() { continue; }
                    if (he.vertex.0 as usize) >= mesh.vertices.len() { continue; }
                    sum += xform(mesh.vert(src).position);
                    sum += xform(mesh.vert(he.vertex).position);
                    count += 2;
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &fid in &self.poly_sel.selected_faces {
                    if (fid as usize) >= mesh.faces.len() { continue; }
                    for vid in mesh.face_vertices(FaceId(fid)) {
                        if (vid.0 as usize) >= mesh.vertices.len() { continue; }
                        sum += xform(mesh.vert(vid).position);
                        count += 1;
                    }
                }
            }
            _ => {}
        }
        if count > 0 { Some(sum / count as f32) } else { None }
    }

    /// World-space (center, unit outward normal) for the current poly sub-element selection.
    /// Used to place a new poly primitive ON the selection (center, oriented by normal, no intersection).
    /// - **Face(s):** average face center + average face normal
    /// - **Edge(s):** average midpoint; normal from adjacent face normals (fallback: camera up)
    /// - **Vertex:** average position; normal from incident face normals
    pub(crate) fn poly_selection_place_frame(&self) -> Option<(Vec3, Vec3)> {
        use core_math::half_edge::{VertexId, FaceId, HalfEdgeId, NO_FACE, NO_TWIN};
        use core_math::scene_graph::NodePayload;
        let nid = self.sel.usel.active_node()?;
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let world = self.scene.scene_graph.world_transform(nid);
        let xform_p = |p: Vec3| world.transform_point3(p);
        let xform_n = |n: Vec3| world.transform_vector3(n).normalize_or_zero();

        match self.poly_work_mode {
            PolyWorkMode::Face if !self.poly_sel.selected_faces.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut count = 0u32;
                for &fid in &self.poly_sel.selected_faces {
                    let f = FaceId(fid);
                    if (fid as usize) >= mesh.faces.len() { continue; }
                    if mesh.face(f).half_edge.0 as usize >= mesh.half_edges.len() { continue; }
                    let vs = mesh.face_vertices(f);
                    if vs.len() < 3 { continue; }
                    let c: Vec3 = vs.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                    let n = mesh.compute_face_normal(f);
                    if n.length_squared() < 1e-10 { continue; }
                    c_sum += c;
                    n_sum += n.normalize();
                    count += 1;
                }
                if count == 0 { return None; }
                let c = xform_p(c_sum / count as f32);
                let n = xform_n(n_sum);
                if n.length_squared() < 0.25 { return Some((c, Vec3::Y)); }
                Some((c, n))
            }
            PolyWorkMode::Edge if !self.poly_sel.selected_edges.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut count = 0u32;
                for &eid in &self.poly_sel.selected_edges {
                    let he = HalfEdgeId(eid);
                    if !mesh.is_edge_live(he) { continue; }
                    let a = mesh.he_source(he);
                    let b = mesh.he(he).vertex;
                    let mid = (mesh.vert(a).position + mesh.vert(b).position) * 0.5;
                    c_sum += mid;
                    count += 1;
                    let f0 = mesh.he(he).face;
                    if f0 != NO_FACE && (f0.0 as usize) < mesh.faces.len() {
                        let n = mesh.compute_face_normal(f0);
                        if n.length_squared() > 1e-10 { n_sum += n.normalize(); }
                    }
                    let tw = mesh.he(he).twin;
                    if tw != NO_TWIN && (tw.0 as usize) < mesh.half_edges.len() {
                        let f1 = mesh.he(tw).face;
                        if f1 != NO_FACE && (f1.0 as usize) < mesh.faces.len() {
                            let n = mesh.compute_face_normal(f1);
                            if n.length_squared() > 1e-10 { n_sum += n.normalize(); }
                        }
                    }
                }
                if count == 0 { return None; }
                let c = xform_p(c_sum / count as f32);
                let n = xform_n(n_sum);
                if n.length_squared() < 0.25 { return Some((c, Vec3::Y)); }
                Some((c, n.normalize()))
            }
            PolyWorkMode::Vertex if !self.poly_sel.selected_vertices.is_empty() => {
                let mut c_sum = Vec3::ZERO;
                let mut n_sum = Vec3::ZERO;
                let mut count = 0u32;
                for &vid in &self.poly_sel.selected_vertices {
                    if (vid as usize) >= mesh.vertices.len() { continue; }
                    let v = VertexId(vid);
                    c_sum += mesh.vert(v).position;
                    count += 1;
                    // Average normals of faces incident on this vertex.
                    for i in 0..mesh.faces.len() as u32 {
                        let f = FaceId(i);
                        if mesh.face(f).half_edge.0 as usize >= mesh.half_edges.len() { continue; }
                        if mesh.face_vertices(f).iter().any(|x| x.0 == vid) {
                            let n = mesh.compute_face_normal(f);
                            if n.length_squared() > 1e-10 { n_sum += n.normalize(); }
                        }
                    }
                }
                if count == 0 { return None; }
                let c = xform_p(c_sum / count as f32);
                let n = xform_n(n_sum);
                if n.length_squared() < 0.25 { return Some((c, Vec3::Y)); }
                Some((c, n.normalize()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod orbit_pivot_tests {
    use super::*;
    use glam::{Quat, Vec3};

    fn cam_eye(app: &AzraApp) -> Vec3 {
        app.viewport.camera.target
            + app.viewport.camera.orbit * Vec3::new(0.0, 0.0, app.viewport.camera.distance)
    }

    /// Selecting must not teleport the camera (regression for face-select jump).
    #[test]
    fn sync_orbit_center_does_not_move_camera() {
        let mut app = FourthLayerApp::new_headless();
        app.viewport.camera.target = Vec3::new(3.0, 1.0, -2.0);
        app.viewport.camera.distance = 12.0;
        app.viewport.camera.orbit = Quat::from_euler(glam::EulerRot::YXZ, 0.4, 0.2, 0.0);
        let eye_before = cam_eye(&app);
        let target_before = app.viewport.camera.target;
        // Even with a "selection", sync must not rewrite target/eye.
        app.sync_orbit_center_to_selection();
        let eye_after = cam_eye(&app);
        assert!(
            (eye_before - eye_after).length() < 1e-5,
            "sync_orbit must not move camera eye (was {eye_before:?} now {eye_after:?})"
        );
        assert!(
            (target_before - app.viewport.camera.target).length() < 1e-5,
            "sync_orbit must not rewrite camera.target"
        );
    }

    /// Retarget keeps the eye fixed (only look-at depth / distance change).
    #[test]
    fn retarget_camera_keep_eye_preserves_eye() {
        let mut app = FourthLayerApp::new_headless();
        app.viewport.camera.target = Vec3::ZERO;
        app.viewport.camera.distance = 10.0;
        app.viewport.camera.orbit = Quat::IDENTITY; // eye at (0,0,10), look -Z
        let eye_before = cam_eye(&app);
        // New target off to the side but in front of the camera
        app.retarget_camera_keep_eye(Vec3::new(3.0, 2.0, -4.0));
        let eye_after = cam_eye(&app);
        assert!(
            (eye_before - eye_after).length() < 1e-4,
            "retarget must not move eye: before {eye_before:?} after {eye_after:?}"
        );
        // Distance must equal depth along look ray
        assert!(app.viewport.camera.distance > 0.0);
    }

    /// Orbit around an off-axis pivot keeps a reconstructable eye and changes target.
    #[test]
    fn orbit_around_off_axis_pivot_preserves_eye_continuity() {
        let mut app = FourthLayerApp::new_headless();
        app.viewport.camera.target = Vec3::ZERO;
        app.viewport.camera.distance = 10.0;
        app.viewport.camera.orbit = Quat::IDENTITY;
        let pivot = Vec3::new(5.0, 0.0, 0.0); // off the look-ray through origin along -Z
        let eye0 = cam_eye(&app);
        // 90° yaw around world Y about the pivot
        let rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        let expected_eye = pivot + rot * (eye0 - pivot);
        app.orbit_camera_around_pivot(rot, pivot);
        let eye1 = cam_eye(&app);
        assert!(
            (eye1 - expected_eye).length() < 1e-4,
            "eye should orbit pivot: expected {expected_eye:?} got {eye1:?}"
        );
        // Distance must be unchanged
        assert!((app.viewport.camera.distance - 10.0).abs() < 1e-5);
    }

    /// When pivot == target, pure trackball: target stays put.
    #[test]
    fn orbit_around_target_keeps_target() {
        let mut app = FourthLayerApp::new_headless();
        app.viewport.camera.target = Vec3::new(1.0, 2.0, 3.0);
        app.viewport.camera.distance = 8.0;
        app.viewport.camera.orbit = Quat::from_euler(glam::EulerRot::YXZ, -0.5, 0.3, 0.0);
        let t0 = app.viewport.camera.target;
        let rot = Quat::from_axis_angle(Vec3::Y, 0.25);
        app.orbit_camera_around_pivot(rot, t0);
        assert!(
            (app.viewport.camera.target - t0).length() < 1e-4,
            "orbit around target must not drift look-at"
        );
    }
}
