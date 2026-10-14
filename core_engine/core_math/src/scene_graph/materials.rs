//! Material graph accessors and material resolution.
//!
//! Pure relocation from former `scene_graph.rs`.

use crate::primitives::{MaterialId, PbrMaterial, SdfPrimitive};

use super::{NodeId, NodePayload, SceneGraph};

impl SceneGraph {
    /// Graph-material content generation (see field docs). Raster compares this to its
    /// last structural rebuild stamp to invalidate cached `RasterMaterial` rows.
    #[inline]
    pub fn material_generation(&self) -> u64 {
        self.material_generation
    }

    /// Bump material content generation. Prefer going through `material_by_id_mut` so every
    /// writer hits the choke point; call this only for rare paths that mutate material nodes
    /// without that API.
    #[inline]
    pub fn bump_material_generation(&mut self) {
        self.material_generation = self.material_generation.wrapping_add(1);
    }
    // ----------------------------------------------------------
    // Material graph accessors
    // ----------------------------------------------------------

    /// Iterate over all Material nodes.
    pub fn materials(&self) -> impl Iterator<Item = (NodeId, MaterialId, &str, &PbrMaterial)> {
        self.nodes.iter().filter_map(|(&nid, n)| {
            if let NodePayload::Material { id, name, data } = &n.payload {
                Some((nid, *id, name.as_str(), data))
            } else {
                None
            }
        })
    }

    /// Find a Material node by its stable MaterialId. Returns (NodeId, &PbrMaterial).
    pub fn material_by_id(&self, mid: MaterialId) -> Option<(NodeId, &PbrMaterial)> {
        if mid.is_none() { return None; }
        self.nodes.iter().find_map(|(&nid, n)| {
            if let NodePayload::Material { id, data, .. } = &n.payload {
                if *id == mid { return Some((nid, data)); }
            }
            None
        })
    }

    /// Find a Material node by its stable MaterialId (mutable).
    ///
    /// **Material-content choke point:** every successful call bumps
    /// [`material_generation`](Self::material_generation). Raster structural reuse keys off that
    /// counter so RENDER-only material edits (PBR factors, map installs, blend params) cannot
    /// leave a stale `RasterMaterial` in the cached draw list.
    pub fn material_by_id_mut(&mut self, mid: MaterialId) -> Option<&mut PbrMaterial> {
        if mid.is_none() {
            return None;
        }
        let nid = self.material_node_id(mid)?;
        // Bump before re-borrow: callers that take the mut ref may write any field.
        self.bump_material_generation();
        match self.nodes.get_mut(&nid).map(|n| &mut n.payload) {
            Some(NodePayload::Material { data, .. }) => Some(data),
            _ => None,
        }
    }

    /// Add a new Material node under the scene root. Returns its MaterialId.
    pub fn add_material(&mut self, name: &str, mat: PbrMaterial) -> MaterialId {
        // Find max existing material id to avoid collisions
        let max_id = self.nodes.values().filter_map(|n| {
            if let NodePayload::Material { id, .. } = &n.payload { Some(id.0) } else { None }
        }).max().unwrap_or(0);
        let new_id = MaterialId(max_id + 1);
        let root = self.root;
        self.add_child(root, NodePayload::Material {
            id: new_id,
            name: name.to_string(),
            data: mat,
        });
        self.bump_material_generation();
        new_id
    }

    /// Remove a Material node by its stable MaterialId.
    pub fn remove_material(&mut self, mid: MaterialId) {
        if mid.is_none() { return; }
        if let Some(nid) = self.material_node_id(mid) {
            self.bump_material_generation();
            self.remove(nid);
        }
    }

    /// NodeId of the Material node carrying `mid` (None for NONE / dangling ids).
    pub fn material_node_id(&self, mid: MaterialId) -> Option<NodeId> {
        if mid.is_none() { return None; }
        self.nodes.iter().find_map(|(&nid, n)| {
            if let NodePayload::Material { id, .. } = &n.payload {
                if *id == mid { return Some(nid); }
            }
            None
        })
    }

    /// Count every live reference to a material across the graph:
    /// `(sdf_prim_refs, poly_face_refs)`. SDF prims reference via `SdfPrimitive.material_id`;
    /// poly meshes via the serialized per-face `HFace.material_id` (the Ctrl+M assignment).
    /// Used by the outliner delete-in-use guard and the material editor's "used by" line.
    pub fn material_use_counts(&self, mid: MaterialId) -> (usize, usize) {
        if mid.is_none() { return (0, 0); }
        let mut prim_refs = 0usize;
        let mut face_refs = 0usize;
        for n in self.nodes.values() {
            match &n.payload {
                NodePayload::SdfBody { primitives, .. } => {
                    prim_refs += primitives.iter().filter(|p| p.material_id == mid).count();
                }
                NodePayload::PolygonalBody { mesh, .. } => {
                    face_refs += mesh.faces.iter().filter(|f| f.material_id == mid.0).count();
                }
                _ => {}
            }
        }
        (prim_refs, face_refs)
    }

    /// Clear every reference to `mid` — prims revert to `MaterialId::NONE` (embedded/default
    /// material), poly faces to `0`. Returns `(prims_cleared, faces_cleared)`. The companion of
    /// `remove_material`: deleting a material node without clearing its references leaves dangling
    /// ids (harmless for display — `resolve_prim_material` falls back — but a later `add_material`
    /// could REUSE the id and silently re-skin those prims, so deletion must clear refs).
    pub fn clear_material_references(&mut self, mid: MaterialId) -> (usize, usize) {
        if mid.is_none() { return (0, 0); }
        let mut prims_cleared = 0usize;
        let mut faces_cleared = 0usize;
        for n in self.nodes.values_mut() {
            match &mut n.payload {
                NodePayload::SdfBody { primitives, .. } => {
                    for p in primitives.iter_mut().filter(|p| p.material_id == mid) {
                        p.material_id = MaterialId::NONE;
                        prims_cleared += 1;
                    }
                }
                NodePayload::PolygonalBody { mesh, .. } => {
                    if mesh.faces.iter().any(|f| f.material_id == mid.0) {
                        let m = std::sync::Arc::make_mut(mesh);
                        for f in m.faces.iter_mut().filter(|f| f.material_id == mid.0) {
                            f.material_id = 0;
                            faces_cleared += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        (prims_cleared, faces_cleared)
    }

    /// Count of Material nodes.
    pub fn material_count(&self) -> usize {
        self.nodes.values().filter(|n| matches!(&n.payload, NodePayload::Material { .. })).count()
    }
    pub fn body_first_albedo_map(&self, body_id: NodeId) -> crate::pbr_material::TexMap {
        let Some(prims) = self.body_primitives(body_id) else { return Default::default(); };
        for p in prims {
            if !p.visible { continue; }
            return self.resolve_prim_material(p).albedo_map;
        }
        Default::default()
    }

    /// Resolve the effective PbrMaterial for a prim — THE canonical resolution used by GPU
    /// serialization, the raster G-buffer pass, and the albedo-map uniform. A referenced graph
    /// material (`material_id`) wins when its Material node is **visible**; `MaterialId::NONE`,
    /// a dangling id, or a **hidden** material (outliner eye off) falls back to the prim's
    /// embedded `material` (neutral warm gray — never black).
    ///
    /// Owner 2026-08-05: eye on Material rows was dead — now SceneNode.visible gates resolve.
    pub fn resolve_prim_material<'a>(&'a self, p: &'a SdfPrimitive) -> &'a PbrMaterial {
        if !p.material_id.is_none() {
            if let Some(mat) = self.material_data_if_visible(p.material_id) {
                return mat;
            }
        }
        &p.material
    }

    /// Record a prim write: bump `material_generation` when the write changed one of the two
    /// PRIM fields `resolve_prim_material` reads (`material_id`, embedded `material`) — keep
    /// the comparison in lockstep with the resolver above. Returns whether it bumped.
    ///
    /// Why: the raster structural draw list caches each drawable's RESOLVED `RasterMaterial`
    /// and re-gathers only on `material_generation` (or SCENE/MESH). A material REASSIGN
    /// (Ctrl+M / combo / transaction) touches neither — `hash_prim_slice` ignores materials, so
    /// it is bake-identical and no mesh event follows — and the raster kept the previous
    /// material while PT (prim buffer) showed the new one (RASTER-MATERIAL-REASSIGN-STALE).
    /// Called from the app's prim write funnels (`mirror_prim_to_graph`, `sync_flat_cache`).
    pub fn note_prim_material_write(&mut self, prev: &SdfPrimitive, new: &SdfPrimitive) -> bool {
        let changed = prev.material_id != new.material_id || prev.material != new.material;
        if changed {
            self.bump_material_generation();
        }
        changed
    }

    /// Graph material data only when the Material node exists and is visible.
    /// Hidden materials (outliner eye) resolve as if unassigned → embedded default.
    pub fn material_data_if_visible(&self, mid: MaterialId) -> Option<&PbrMaterial> {
        let nid = self.material_node_id(mid)?;
        let node = self.get(nid)?;
        if !node.visible {
            return None;
        }
        match &node.payload {
            NodePayload::Material { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Resolve the effective DISPLAY material for a mono-material poly body — the sibling of
    /// `resolve_prim_material` for `PolygonalBody`. Uses the first live face with a set
    /// `material_id` (object-level assign writes all faces); `0` / dangling / **hidden** → embedded.
    ///
    /// Multi-material bodies are drawn as multiple index ranges in `raster_gbuffer`
    /// (each range resolves its own graph material). This helper is the mono / fallback path.
    pub fn resolve_poly_body_material<'a>(
        &'a self,
        mesh: &'a crate::half_edge::HalfEdgeMesh,
        embedded: &'a PbrMaterial,
    ) -> &'a PbrMaterial {
        for f in &mesh.faces {
            if f.half_edge == crate::half_edge::NO_EDGE {
                continue;
            }
            if f.material_id > 0 {
                if let Some(mat) = self.material_data_if_visible(MaterialId(f.material_id)) {
                    return mat;
                }
            }
        }
        embedded
    }

    /// The body's PREVAILING shared material — what a NEWLY-CREATED prim in this body should
    /// inherit so it visually blends into the body (owner ask 2026-07-18: "a newly created
    /// primitive inside a body should receive the material from the body"; before this, a new
    /// cylinder always arrived with the default material and visibly broke the body's look).
    ///
    /// Pragmatic definition (chosen for predictability over a majority vote): the `material_id`
    /// of the LAST prim in the body — the most recently added. This is stable under the
    /// assign-to-body flow (which writes ALL prims uniformly) and chains predictably as prims
    /// are added (each new prim inherits from the previous one). Empty body, not an SdfBody, or
    /// last prim unassigned → `MaterialId::NONE` (the new prim keeps its embedded default —
    /// exactly the pre-existing behavior).
    pub fn body_prevailing_material(&self, body_id: NodeId) -> MaterialId {
        self.body_primitives(body_id)
            .and_then(|prims| prims.last())
            .map(|p| p.material_id)
            .unwrap_or(MaterialId::NONE)
    }


}
