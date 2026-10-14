// core_engine/app/src/nurbs_tools/create.rs
// Solid helpers + mesh cache sync (NurbsSolid face access, tessellation refresh).

use std::sync::Arc;
use eframe::egui;
use glam::Vec3;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, NurbsWorkMode, NurbsTool,
    ActiveTool, NurbsCvDrag, NurbsKeyFlags, DirtyFlags,
    SceneItemId, world_to_screen,
};

impl AzraApp {
    // ============================================================
    // Solid helpers (NurbsSolid = one node, N faces — PLAN N1)
    // ============================================================

    /// Legacy group-of-NurbsBody packs (pre-NurbsSolid). Prefer `NurbsSolid`.
    pub(crate) fn nurbs_is_solid_group(&self, nid: NodeId) -> bool {
        let Some(node) = self.scene.scene_graph.get(nid) else { return false };
        match &node.payload {
            NodePayload::Group { name } if name.starts_with("NURBS.") => {
                let kids = self.scene.scene_graph.children(nid);
                !kids.is_empty()
                    && kids.iter().all(|&c| {
                        matches!(
                            self.scene.scene_graph.get(c).map(|n| &n.payload),
                            Some(NodePayload::NurbsBody { .. })
                        )
                    })
            }
            _ => false,
        }
    }

    pub(crate) fn nurbs_is_solid(&self, nid: NodeId) -> bool {
        matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::NurbsSolid { .. })
        ) || self.nurbs_is_solid_group(nid)
    }

    /// Resolve the solid / surface root from current selection.
    pub(crate) fn nurbs_solid_root(&self) -> Option<NodeId> {
        let nid = self.sel.usel.active_node()?;
        if self.nurbs_is_solid(nid) {
            return Some(nid);
        }
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if matches!(node.payload, NodePayload::NurbsBody { .. }) {
                if let Some(parent) = node.parent {
                    if self.nurbs_is_solid_group(parent) {
                        return Some(parent);
                    }
                }
                return Some(nid); // standalone surface
            }
        }
        None
    }

    /// Number of faces on a solid / single surface (1).
    pub(crate) fn nurbs_face_count(&self, solid: NodeId) -> usize {
        match self.scene.scene_graph.get(solid).map(|n| &n.payload) {
            Some(NodePayload::NurbsSolid { faces, .. }) => faces.len(),
            Some(NodePayload::NurbsBody { .. }) => 1,
            _ if self.nurbs_is_solid_group(solid) => self
                .scene
                .scene_graph
                .children(solid)
                .iter()
                .filter(|&&c| {
                    matches!(
                        self.scene.scene_graph.get(c).map(|n| &n.payload),
                        Some(NodePayload::NurbsBody { .. })
                    )
                })
                .count(),
            _ => 0,
        }
    }

    /// World transform for a face (solid xform; legacy group uses face node xform).
    pub(crate) fn nurbs_face_world_xform(&self, solid: NodeId, face_idx: u32) -> glam::Mat4 {
        if self.nurbs_is_solid_group(solid) {
            let kids: Vec<_> = self
                .scene
                .scene_graph
                .children(solid)
                .iter()
                .copied()
                .filter(|&c| {
                    matches!(
                        self.scene.scene_graph.get(c).map(|n| &n.payload),
                        Some(NodePayload::NurbsBody { .. })
                    )
                })
                .collect();
            if let Some(&fnid) = kids.get(face_idx as usize) {
                return self.scene.scene_graph.world_transform(fnid);
            }
        }
        self.scene.scene_graph.world_transform(solid)
    }

    /// Snapshot face surfaces as (face_idx, local CPs grid positions) for evaluate/draw.
    /// Returns owned surface clones only when needed for mutate paths — read path uses closures.
    pub(crate) fn nurbs_with_face<R>(
        &self,
        solid: NodeId,
        face_idx: u32,
        f: impl FnOnce(&core_math::nurbs::NurbsFace) -> R,
    ) -> Option<R> {
        let node = self.scene.scene_graph.get(solid)?;
        match &node.payload {
            NodePayload::NurbsSolid { faces, .. } => {
                let face = faces.get(face_idx as usize)?;
                Some(f(face))
            }
            _ => None,
        }
    }

    pub(crate) fn nurbs_with_face_surface<R>(
        &self,
        solid: NodeId,
        face_idx: u32,
        f: impl FnOnce(&core_math::nurbs::NurbsSurface) -> R,
    ) -> Option<R> {
        let node = self.scene.scene_graph.get(solid)?;
        match &node.payload {
            NodePayload::NurbsSolid { faces, .. } => {
                let face = faces.get(face_idx as usize)?;
                Some(f(face.surface.as_ref()))
            }
            NodePayload::NurbsBody { surface, .. } if face_idx == 0 => Some(f(surface.as_ref())),
            _ if self.nurbs_is_solid_group(solid) => {
                let kids: Vec<_> = self
                    .scene
                    .scene_graph
                    .children(solid)
                    .iter()
                    .copied()
                    .filter(|&c| {
                        matches!(
                            self.scene.scene_graph.get(c).map(|n| &n.payload),
                            Some(NodePayload::NurbsBody { .. })
                        )
                    })
                    .collect();
                let fnid = *kids.get(face_idx as usize)?;
                let n = self.scene.scene_graph.get(fnid)?;
                match &n.payload {
                    NodePayload::NurbsBody { surface, .. } => Some(f(surface.as_ref())),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Mutate a face surface (COW Arc).
    pub(crate) fn nurbs_with_face_surface_mut(
        &mut self,
        solid: NodeId,
        face_idx: u32,
        f: impl FnOnce(&mut core_math::nurbs::NurbsSurface),
    ) -> bool {
        let Some(node) = self.scene.scene_graph.get_mut(solid) else {
            return false;
        };
        match &mut node.payload {
            NodePayload::NurbsSolid { faces, .. } => {
                let Some(face) = faces.get_mut(face_idx as usize) else {
                    return false;
                };
                f(Arc::make_mut(&mut face.surface));
                true
            }
            NodePayload::NurbsBody { surface, .. } if face_idx == 0 => {
                f(Arc::make_mut(surface));
                true
            }
            _ => false,
        }
    }

    /// Legacy: face body nodes for group packs only. Empty for NurbsSolid.
    pub(crate) fn nurbs_face_nodes(&self, solid: NodeId) -> Vec<NodeId> {
        if self.nurbs_is_solid_group(solid) {
            return self
                .scene
                .scene_graph
                .children(solid)
                .iter()
                .copied()
                .filter(|&c| {
                    matches!(
                        self.scene.scene_graph.get(c).map(|n| &n.payload),
                        Some(NodePayload::NurbsBody { .. })
                    )
                })
                .collect();
        }
        if matches!(
            self.scene.scene_graph.get(solid).map(|n| &n.payload),
            Some(NodePayload::NurbsBody { .. })
        ) {
            return vec![solid];
        }
        Vec::new()
    }

    /// Resolve stable face id → current list index (None if dead after regenerate).
    pub(crate) fn nurbs_face_index(
        &self,
        solid: NodeId,
        id: core_math::nurbs::NurbsFaceId,
    ) -> Option<u32> {
        match self.scene.scene_graph.get(solid).map(|n| &n.payload) {
            Some(NodePayload::NurbsSolid { faces, .. }) => faces
                .iter()
                .position(|f| f.id == id)
                .map(|i| i as u32),
            Some(NodePayload::NurbsBody { .. }) if id.0 == 0 => Some(0),
            _ => None,
        }
    }

    /// Face id at list index (legacy NurbsBody → id 0).
    pub(crate) fn nurbs_face_id_at(
        &self,
        solid: NodeId,
        face_idx: u32,
    ) -> Option<core_math::nurbs::NurbsFaceId> {
        match self.scene.scene_graph.get(solid).map(|n| &n.payload) {
            Some(NodePayload::NurbsSolid { faces, .. }) => {
                faces.get(face_idx as usize).map(|f| f.id)
            }
            Some(NodePayload::NurbsBody { .. }) if face_idx == 0 => {
                Some(core_math::nurbs::NurbsFaceId(0))
            }
            _ => None,
        }
    }

    /// Live face-id set for prune after regenerate.
    pub(crate) fn nurbs_live_face_ids(
        &self,
        solid: NodeId,
    ) -> std::collections::HashSet<core_math::nurbs::NurbsFaceId> {
        use std::collections::HashSet;
        match self.scene.scene_graph.get(solid).map(|n| &n.payload) {
            Some(NodePayload::NurbsSolid { faces, .. }) => faces.iter().map(|f| f.id).collect(),
            Some(NodePayload::NurbsBody { .. }) => {
                let mut s = HashSet::new();
                s.insert(core_math::nurbs::NurbsFaceId(0));
                s
            }
            _ => HashSet::new(),
        }
    }

    /// Drop selection keys whose face ids vanished after a regenerate.
    pub(crate) fn nurbs_prune_selection(&mut self, solid: NodeId) {
        let live = self.nurbs_live_face_ids(solid);
        self.nurbs_sel.prune_dead_ids(&live);
    }

    /// Active face index for Vertex/Edge (first selected face, else 0).
    pub(crate) fn nurbs_active_face_index(&self) -> Option<u32> {
        let solid = self.nurbs_solid_root()?;
        let n = self.nurbs_face_count(solid);
        if n == 0 {
            return None;
        }
        if let Some(&id) = self.nurbs_sel.selected_faces.iter().next() {
            if let Some(fi) = self.nurbs_face_index(solid, id) {
                return Some(fi);
            }
        }
        Some(0)
    }

    /// Active face node — only for legacy NurbsBody / group packs.
    pub(crate) fn nurbs_active_face_node(&self) -> Option<NodeId> {
        let solid = self.nurbs_solid_root()?;
        if matches!(
            self.scene.scene_graph.get(solid).map(|n| &n.payload),
            Some(NodePayload::NurbsSolid { .. })
        ) {
            return Some(solid); // solid itself; use face index for surface
        }
        let faces = self.nurbs_face_nodes(solid);
        if faces.is_empty() {
            return None;
        }
        if let Some(&fid) = self.nurbs_sel.selected_faces.iter().next() {
            if let Some(fi) = self.nurbs_face_index(solid, fid) {
                if let Some(&nid) = faces.get(fi as usize) {
                    return Some(nid);
                }
            }
        }
        faces.first().copied()
    }

    /// True if the face centre looks toward the camera (front-facing, no backface X-ray).
    /// World normal from surface.normal at mid-domain; facing if n·(cam − p) > 0
    /// (outward normal points toward camera).
    pub(crate) fn nurbs_face_front_facing(&self, solid: NodeId, face_idx: u32, cam_pos: Vec3) -> bool {
        let xform = self.nurbs_face_world_xform(solid, face_idx);
        self.nurbs_with_face_surface(solid, face_idx, |surface| {
            let (u0, u1) = surface.domain_u();
            let (v0, v1) = surface.domain_v();
            let u = (u0 + u1) * 0.5;
            let v = (v0 + v1) * 0.5;
            let p_local = surface.evaluate(u, v);
            let n_local = surface.normal(u, v);
            // Direction (no translation); correct for uniform scale (cube default).
            let n_world = xform.transform_vector3(n_local).normalize_or_zero();
            let p_world = xform.transform_point3(p_local);
            let to_cam = (cam_pos - p_world).normalize_or_zero();
            n_world.dot(to_cam) > 0.05 // small epsilon so grazing edges still count
        })
        .unwrap_or(false)
    }

    // ============================================================
    // Mesh cache sync
    // ============================================================

    /// After any CV/knot change, retessellate and update the mesh cache.
    /// `NurbsSolid` merges all face tessellations into one display mesh.
    pub(crate) fn nurbs_sync_mesh_cache(&mut self, nid: NodeId) {
        let (cache_id, raster) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::NurbsBody { surface, mesh_cache_id, .. } => {
                    (*mesh_cache_id, surface.tessellate())
                }
                NodePayload::NurbsSolid {
                    faces,
                    mesh_cache_id,
                    ..
                } => {
                    let mut verts: Vec<[f32; 8]> = Vec::new();
                    let mut indices: Vec<u32> = Vec::new();
                    for face in faces {
                        // Respect parameter-space trims (PROMPT_TRIMMED_SURFACES T2)
                        let (fv, fi) = face.tessellate();
                        let base = verts.len() as u32;
                        verts.extend(fv);
                        indices.extend(fi.into_iter().map(|i| i + base));
                    }
                    (*mesh_cache_id, (verts, indices))
                }
                _ => return,
            }
        };

        let tri_count = raster.1.len() as u32 / 3;
        if let Some(cid) = cache_id {
            self.cache.mesh_cache.update_editable_mesh(cid, raster, tri_count);
        } else {
            let cid = self.cache.mesh_cache.alloc_id();
            self.cache.mesh_cache.update_editable_mesh(cid, raster, tri_count);
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                match &mut node.payload {
                    NodePayload::NurbsBody { mesh_cache_id, .. }
                    | NodePayload::NurbsSolid { mesh_cache_id, .. } => {
                        *mesh_cache_id = Some(cid);
                    }
                    _ => {}
                }
            }
        }
    }
}
