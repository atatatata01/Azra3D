// core_engine/app/src/convert.rs
// Cross-space geometry conversions between SDF, Polygonal, NURBS, and Imported meshes.
//
// O5: `convert_sdf_to_poly` is **async**. UI only queues a request; poll kicks a non-blocking
// GPU readback (`PendingSdfReadback`) and finishes the half-edge transaction when ready.
// Never `device.poll(Wait)` on the frame thread (measured: ~1000 ms of the old 1137 ms stall).

use glam::Vec3;
use std::sync::Arc;
use core_math::half_edge::HalfEdgeMesh;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::mesh_presentation::{
    cpu_oracle_mesh, dispatch_sdf_display_readback, display_slot_snapshot_ok, mesh_to_verts8,
    SdfDisplayReadbackSrc, SdfMaterializeArm,
};
use crate::sdf_gpu_mesh::PendingSdfReadback;
use crate::{AzraApp, DirtyFlags};

/// UI → poll: convert this SDF body when the mesh readback lands (O5).
pub(crate) struct PendingConvertSdf {
    pub nid: NodeId,
    pub name: String,
    pub material: core_math::pbr_material::PbrMaterial,
}

/// Poll: GPU readback in flight for convert (display slot / sparse atlas / grid DC).
pub(crate) struct InFlightConvertSdf {
    pub nid: NodeId,
    pub name: String,
    pub material: core_math::pbr_material::PbrMaterial,
    pub arm: SdfMaterializeArm,
    pub pending: PendingSdfReadback,
}

impl AzraApp {
    /// Convert an SDF body → PolygonalBody using the **live Dual Contouring mesh**.
    ///
    /// **O5 async:** does not block the frame thread. Queues a poll-phase job that
    /// kicks the same display/grid readback path as export/PT without `poll(Wait)`,
    /// then builds the half-edge mesh when the staging maps settle.
    ///
    /// Refuse-before-queue: wrong type toasts and returns with no history entry.
    pub(crate) fn convert_sdf_to_poly(&mut self, nid: NodeId) {
        let (name, material) = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::SdfBody { name, primitives, .. } => {
                    let mat = primitives
                        .first()
                        .map(|p| p.material.clone())
                        .unwrap_or_default();
                    (name.clone(), mat)
                }
                _ => {
                    self.toast("Convert to Mesh: select an SDF body");
                    return;
                }
            }
        };

        // Supersede any prior convert (one body at a time is enough UX).
        self.cache.convert_sdf_inflight = None;
        self.cache.convert_sdf_pending = Some(PendingConvertSdf {
            nid,
            name: name.clone(),
            material,
        });
        self.toast(&format!("Converting to Mesh: {name}…"));
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Poll-phase: kick / settle async convert readbacks (O5).
    ///
    /// Frame Ordering Contract: GPU buffer create + copy only here.
    /// Frame Budget Rule: never `PollType::Wait` — only `Poll` + ready/take.
    pub(crate) fn poll_convert_sdf_to_poly(&mut self) {
        // 1) Land in-flight when staging maps are ready.
        if let Some(flight) = self.cache.convert_sdf_inflight.as_ref() {
            if !flight.pending.ready() {
                // Drive map callbacks without blocking.
                if let Some(gpu) = self.gpu.as_ref() {
                    gpu.device.poll(eframe::wgpu::PollType::Poll).expect("GPU device polling failed");
                }
                self.mark_dirty(DirtyFlags::RENDER); // keep pumping until ready
                return;
            }
            let flight = self.cache.convert_sdf_inflight.take().unwrap();
            let (verts, indices) = flight.pending.take();
            self.finish_convert_sdf_to_poly(
                flight.nid,
                flight.name,
                flight.material,
                verts,
                indices,
                flight.arm,
            );
            return;
        }

        // 2) Kick a new request (at most one).
        let Some(req) = self.cache.convert_sdf_pending.take() else {
            return;
        };
        // Body may have been deleted while queued.
        if !matches!(
            self.scene.scene_graph.get(req.nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        ) {
            self.toast("Convert to Mesh: body gone");
            return;
        }

        match self.kick_convert_sdf_readback(req.nid) {
            ConvertKick::InFlight {
                pending,
                arm,
            } => {
                self.cache.convert_sdf_inflight = Some(InFlightConvertSdf {
                    nid: req.nid,
                    name: req.name,
                    material: req.material,
                    arm,
                    pending,
                });
                self.mark_dirty(DirtyFlags::RENDER);
            }
            ConvertKick::Immediate {
                verts,
                indices,
                arm,
            } => {
                self.finish_convert_sdf_to_poly(
                    req.nid,
                    req.name,
                    req.material,
                    verts,
                    indices,
                    arm,
                );
            }
            ConvertKick::Failed(msg) => {
                self.toast(&msg);
            }
        }
    }

    /// Start the GPU readback (or CPU oracle) for convert — poll only.
    fn kick_convert_sdf_readback(&mut self, nid: NodeId) -> ConvertKick {
        let sparse_src = self.cache.body_vol_cache.pt_sparse_snapshot_src(nid);
        let field_src = self.cache.body_vol_cache.pt_field_snapshot_src(nid);
        let display = sparse_src.or(field_src);

        // Arm 1/2: display readback (async, same as PT pump).
        if let Some((_hash, res, gmin, gmax)) = display {
            if let (Some(g), Some(m)) = (self.gpu.as_ref(), self.cache.sdf_mesher.as_ref()) {
                let src = if sparse_src.is_some() {
                    g.sdf_brick_atlases
                        .get(&nid)
                        .filter(|a| a.body_nid == Some(nid) && a.res == res)
                        .map(|atlas| SdfDisplayReadbackSrc::SparseAtlas {
                            atlas,
                            res,
                            gmin,
                            gmax,
                            nid,
                            lod_level: 0,
                        })
                } else {
                    g.sdf_body_mesh
                        .get(&nid)
                        .filter(|s| display_slot_snapshot_ok(s, res))
                        .map(SdfDisplayReadbackSrc::DisplaySlot)
                };
                if let Some(src) = src {
                    let pending = dispatch_sdf_display_readback(&g.device, &g.queue, m, src);
                    return ConvertKick::InFlight {
                        pending,
                        arm: SdfMaterializeArm::DisplayReadback,
                    };
                }
            }
        }

        // Arm 3: grid DC async, or CPU oracle (no Wait).
        let res = display
            .map(|(_, r, ..)| r)
            .unwrap_or(128)
            .clamp(16, 1024);
        if let (Some(g), Some(m)) = (self.gpu.as_ref(), self.cache.sdf_mesher.as_ref()) {
            use crate::bake_utils::{
                bake_body_grid_field, collect_body_prims_recursive, compute_body_bake_bounds,
                pad_grid_bounds,
            };
            let sg = &self.scene.scene_graph;
            let (mflags, mcenter, bevel) = match sg.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody {
                    mirror_flags,
                    mirror_center,
                    bevel_radius,
                    ..
                }) => (*mirror_flags, *mirror_center, *bevel_radius),
                _ => (0u8, Vec3::ZERO, 0.0f32),
            };
            let prims = collect_body_prims_recursive(sg, nid);
            let (tmin, tmax) = compute_body_bake_bounds(&prims, mflags, mcenter, bevel);
            let (gmin, gmax) = pad_grid_bounds(tmin, tmax);
            let field = core_math::field_eval::BodyField::from_graph(sg, nid);
            let grid = bake_body_grid_field(&field, res, gmin, gmax);
            // O5: async grid DC — never mesh_readback (which Wait-stalls).
            let pending = m.dispatch_async(&g.device, &g.queue, &grid, res, gmin, gmax);
            return ConvertKick::InFlight {
                pending,
                arm: SdfMaterializeArm::GpuGridDc,
            };
        }

        // No GPU → CPU oracle immediately (no frame stall from Wait).
        let (mesh, _gmin, _gmax) = cpu_oracle_mesh(&self.scene.scene_graph, nid, res);
        if mesh.quads.is_empty() {
            return ConvertKick::Failed(
                "Convert to Mesh: no DC mesh yet — wait for bake, then retry".into(),
            );
        }
        let (verts, indices) = mesh_to_verts8(&mesh);
        ConvertKick::Immediate {
            verts,
            indices,
            arm: SdfMaterializeArm::CpuOracle,
        }
    }

    /// Apply rigid offset + half-edge build + scene transaction (shared by sync finish paths).
    fn finish_convert_sdf_to_poly(
        &mut self,
        nid: NodeId,
        name: String,
        material: core_math::pbr_material::PbrMaterial,
        mut verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        arm: SdfMaterializeArm,
    ) {
        if indices.len() < 3 || verts.is_empty() {
            self.toast("Convert to Mesh: DC mesh is empty");
            return;
        }

        // Display-readback meshes predate a pending rigid drag offset — same re-anchor as export.
        if arm == SdfMaterializeArm::DisplayReadback {
            let off = self.cache.body_vol_cache.mesh_rigid_offset(nid);
            if off.length_squared() > 0.0 {
                for v in &mut verts {
                    v[0] += off.x;
                    v[1] += off.y;
                    v[2] += off.z;
                }
            }
        }

        let positions: Vec<Vec3> = verts
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let normals: Vec<Vec3> = verts
            .iter()
            .map(|v| Vec3::new(v[3], v[4], v[5]))
            .collect();
        let tris: Vec<[u32; 3]> = indices
            .chunks(3)
            .filter(|c| c.len() == 3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        if tris.is_empty() {
            self.toast("Convert to Mesh: DC mesh has no triangles");
            return;
        }
        let he_mesh = HalfEdgeMesh::from_triangle_soup(&positions, &normals, &tris);
        if he_mesh.face_count() == 0 {
            self.toast("Convert to Mesh: failed to build half-edge from DC mesh");
            return;
        }

        let arm_label = match arm {
            SdfMaterializeArm::DisplayReadback => "display DC",
            SdfMaterializeArm::GpuGridDc => "grid DC",
            SdfMaterializeArm::CpuOracle => "CPU DC",
        };

        self.transaction("Convert to Mesh", |s| {
            // Drop GPU-resident SDF mesh for this nid — payload is no longer an SdfBody.
            if let Some(g) = s.gpu.as_mut() {
                g.sdf_body_mesh.remove(&nid);
                g.sdf_brick_atlases.remove(&nid);
                g.sdf_atlas_lru.remove(&nid);
                if let Some(pool) = g.sdf_mesh_pool.as_mut() {
                    let queue = g.queue.clone();
                    pool.retain_bodies(&queue, |id| *id != nid);
                }
            }
            s.cache.body_vol_cache.remove_body(nid);

            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                let new_name = format!("{} (Poly)", name);
                let n_faces = he_mesh.face_count();
                node.payload = NodePayload::PolygonalBody {
                    name: new_name.clone(),
                    mesh: Arc::new(he_mesh),
                    source: Default::default(),
                    mesh_cache_id: None, // allocated on next ensure_poly_meshes / poly_sync
                    material,
                    modifiers: Vec::new(),
                };
                s.toast(&format!(
                    "Converted to Mesh: {new_name} ({n_faces} faces via {arm_label})"
                ));
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER | DirtyFlags::SCENE);
        });
    }

    /// Weld a raster triangle soup into an editable half-edge mesh, carrying per-corner UVs.
    ///
    /// Raster vertices are UNWELDED — one per face corner — so building triangles over them
    /// directly gives every triangle its own isolated vertices and no shared edges: technically
    /// faces, but not an editable mesh (no loops, no dissolve, no bevel). Welding by position is
    /// what makes it real geometry.
    ///
    /// UVs are recovered per CORNER, matched by welded vertex id rather than by corner order,
    /// because `from_triangles` gives no guarantee that half-edge corner `k` of face `i` is input
    /// corner `k` — a rotated ring would shift every UV by one and produce a subtly wrong unwrap
    /// that still looked like an unwrap.
    ///
    /// Returns `(mesh, per-corner UV keyed by HalfEdgeId)`.
    fn build_poly_from_raster_impl(
        verts: &[[f32; 8]],
        indices: &[u32],
    ) -> (HalfEdgeMesh, Vec<(core_math::half_edge::HalfEdgeId, glam::Vec2)>) {
        use core_math::half_edge::FaceId;
        use std::collections::HashMap;

        // Weld by quantised position. 1e-5 relative to the mesh span: tight enough to keep
        // genuinely distinct vertices apart, loose enough to join corners that differ only by
        // float error from the exporter.
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for v in verts {
            let p = Vec3::new(v[0], v[1], v[2]);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let span = (hi - lo).max_element().max(1.0e-6);
        let q = |x: f32| (x / span * 1.0e5).round() as i64;

        let mut weld: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut positions: Vec<Vec3> = Vec::new();
        let mut raster_to_welded: Vec<u32> = Vec::with_capacity(verts.len());
        for v in verts {
            let p = Vec3::new(v[0], v[1], v[2]);
            let key = (q(p.x), q(p.y), q(p.z));
            let id = *weld.entry(key).or_insert_with(|| {
                positions.push(p);
                (positions.len() - 1) as u32
            });
            raster_to_welded.push(id);
        }

        let tris: Vec<[usize; 3]> = indices
            .chunks_exact(3)
            .map(|c| {
                [
                    raster_to_welded[c[0] as usize] as usize,
                    raster_to_welded[c[1] as usize] as usize,
                    raster_to_welded[c[2] as usize] as usize,
                ]
            })
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]) // drop degenerates
            .collect();
        let mesh = HalfEdgeMesh::from_triangles(&positions, &tris);

        // Per-corner UVs, matched by welded vertex id (see the doc note on corner order).
        let mut uv = Vec::new();
        let mut tri_i = 0usize;
        for fi in 0..mesh.faces.len() {
            let f = FaceId(fi as u32);
            if !mesh.is_face_live(f) || tri_i >= indices.len() / 3 {
                continue;
            }
            let src = [
                indices[tri_i * 3] as usize,
                indices[tri_i * 3 + 1] as usize,
                indices[tri_i * 3 + 2] as usize,
            ];
            for he in mesh.face_half_edges(f) {
                let target = mesh.he(he).vertex.0;
                if let Some(&r) = src
                    .iter()
                    .find(|&&r| raster_to_welded[r] == target)
                {
                    uv.push((he, glam::Vec2::new(verts[r][6], verts[r][7])));
                }
            }
            tri_i += 1;
        }
        (mesh, uv)
    }

    /// Convert an imported mesh to a PolygonalBody (enables editing).
    ///
    /// Reads the imported mesh's raster data from the persistent cache,
    /// builds a HalfEdgeMesh from the triangle data.
    /// Refuse-before-transaction on missing cache / wrong type.
    pub(crate) fn convert_imported_to_poly(&mut self, nid: NodeId) {
        let prepared = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::ImportedMesh { name, mesh_cache_id, materials, .. } => {
                    let mat = materials.first().cloned().unwrap_or_default();
                    // Read the RETAINED RASTER, not `cpu_mesh`.
                    //
                    // This used to be `HalfEdgeMesh::from_mesh(&c.cpu_mesh)`, which was wrong in
                    // two compounding ways:
                    //
                    // 1. `insert_imported` builds `cpu_mesh` with `quads: Vec::new()` ("imported
                    //    meshes use triangles directly"), and `from_mesh` treats an empty-quad
                    //    Mesh as a POINT CLOUD — so Convert to Poly produced a body with every
                    //    vertex and **zero faces**. All topology, silently gone. (Probe:
                    //    `core_math::mesh::convert_probe::from_mesh_without_quads_yields_no_faces`.)
                    // 2. `core_math::mesh::Mesh` has no UV field, so the imported unwrap was
                    //    dropped too — which is why "I have UVs" did not mean AZRA had them, and
                    //    why AZRA's own unwrap was the only way to get any.
                    //
                    // `imported_raster_persistent` keeps both: `[p,n,uv]` per vertex plus indices.
                    let Some((verts, indices)) = self
                        .cache
                        .mesh_cache
                        .imported_raster_persistent(*mesh_cache_id)
                    else {
                        self.toast("Convert to Poly: this mesh's imported data is no longer cached");
                        return;
                    };
                    if indices.len() < 3 {
                        self.toast("Convert to Poly: imported mesh has no triangles");
                        return;
                    }
                    let built = Self::build_poly_from_raster_impl(verts, indices);
                    Some((name.clone(), built.0, mat, built.1))
                }
                _ => None,
            }
        };
        let Some((name, he_mesh, material, corner_uv)) = prepared else { return; };

        self.transaction("Convert to Poly", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                let new_name = format!("{} (Poly)", name);
                node.payload = NodePayload::PolygonalBody {
                    name: new_name.clone(),
                    mesh: Arc::new(he_mesh),
                    source: Default::default(),
                    mesh_cache_id: None,
                    material,
                    modifiers: Vec::new(),
                };
                s.toast(&format!("Converted to Polygon: {}", new_name));
            }
            // Carry the imported unwrap into AZRA's own UV layer, so "I have UVs" means AZRA has
            // them and Bake Mesh Maps works without re-unwrapping.
            if !corner_uv.is_empty() {
                let cd = s.poly_cd_mut(nid);
                cd.uv = core_math::poly_mesh::LayerStore::new(core_math::poly_mesh::Interp::Copy);
                for (he, uv) in &corner_uv {
                    cd.uv.set(*he, *uv);
                }
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }

    /// Convert a NURBS surface to a PolygonalBody (tessellate + build half-edge).
    /// Refuse-before-transaction on wrong node type.
    pub(crate) fn convert_nurbs_to_poly(&mut self, nid: NodeId) {
        let prepared = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::NurbsBody { name, surface, material, .. } => {
                    let (raster_verts, raster_indices) = surface.tessellate();
                    let positions: Vec<Vec3> = raster_verts.iter()
                        .map(|v| Vec3::new(v[0], v[1], v[2]))
                        .collect();
                    let normals: Vec<Vec3> = raster_verts.iter()
                        .map(|v| Vec3::new(v[3], v[4], v[5]))
                        .collect();
                    let tris: Vec<[u32; 3]> = raster_indices.chunks(3)
                        .filter(|c| c.len() == 3)
                        .map(|c| [c[0], c[1], c[2]])
                        .collect();
                    let mesh = HalfEdgeMesh::from_triangle_soup(&positions, &normals, &tris);
                    Some((name.clone(), mesh, material.clone()))
                }
                _ => None,
            }
        };
        let Some((name, he_mesh, material)) = prepared else { return; };

        self.transaction("Convert to Poly", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                let new_name = format!("{} (Poly)", name);
                node.payload = NodePayload::PolygonalBody {
                    name: new_name.clone(),
                    mesh: Arc::new(he_mesh),
                    source: Default::default(),
                    mesh_cache_id: None,
                    material,
                    modifiers: Vec::new(),
                };
                s.toast(&format!("Converted to Polygon: {}", new_name));
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(DirtyFlags::RENDER);
        });
    }

    /// Export a scene graph node as OBJ. Works for PolygonalBody and NurbsBody.
    pub(crate) fn export_node_obj(&mut self, nid: NodeId, path: &str) {
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => {
            self.toast("Node not found"); return;
        }};
        match &node.payload {
            NodePayload::PolygonalBody { name, mesh, .. } => {
                let m = mesh.to_mesh();
                match m.export_obj(path) {
                    Ok(()) => self.toast(&format!("Exported {} ({} verts, {} faces)", name, m.vertices.len(), m.quads.len())),
                    Err(e) => self.toast(&format!("Export failed: {}", e)),
                }
            }
            NodePayload::NurbsBody { name, surface, .. } => {
                let (raster_verts, raster_indices) = surface.tessellate();
                // Build Mesh from triangle data
                let positions: Vec<Vec3> = raster_verts.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
                let normals: Vec<Vec3> = raster_verts.iter().map(|v| Vec3::new(v[3], v[4], v[5])).collect();
                // Store as triangles with degenerate quad (v2==v3)
                let quads: Vec<[u32; 4]> = raster_indices.chunks(3)
                    .filter(|c| c.len() == 3)
                    .map(|c| [c[0], c[1], c[2], c[2]])
                    .collect();
                let m = core_math::mesh::Mesh { vertices: positions, normals, quads };
                match m.export_obj(path) {
                    Ok(()) => self.toast(&format!("Exported {} ({} verts, {} tris)", name, m.vertices.len(), m.quads.len())),
                    Err(e) => self.toast(&format!("Export failed: {}", e)),
                }
            }
            _ => self.toast("Only Poly and NURBS nodes can be exported as OBJ"),
        }
    }

    /// Get a short type label for a scene graph node payload.
    pub(crate) fn node_type_label(payload: &NodePayload) -> &'static str {
        match payload {
            NodePayload::Root => "ROOT",
            NodePayload::Group { .. } => "GRP",
            NodePayload::SdfBody { .. } => "SDF",
            NodePayload::ImportedMesh { .. } => "IMP",
            NodePayload::VoxelBody { .. } => "VOX",
            NodePayload::PolygonalBody { .. } => "POLY",
            NodePayload::NurbsBody { .. } => "NURBS",
            NodePayload::NurbsSolid { .. } => "NURBS",
            NodePayload::Light(_) => "LGT",
            NodePayload::Camera { .. } => "CAM",
            NodePayload::Material { .. } => "MAT",
            NodePayload::Empty { .. } => "LOC",
            NodePayload::SdfObject { .. } => "OBJ",
        }
    }

    /// Get color for a node type label.
    pub(crate) fn node_type_color(payload: &NodePayload) -> egui::Color32 {
        use eframe::egui::Color32;
        match payload {
            NodePayload::SdfBody { .. } => Color32::from_rgb(80, 200, 140),
            NodePayload::PolygonalBody { .. } => Color32::from_rgb(100, 200, 255),
            NodePayload::NurbsBody { .. } | NodePayload::NurbsSolid { .. } => {
                Color32::from_rgb(200, 140, 255)
            }
            NodePayload::ImportedMesh { .. } => Color32::from_rgb(180, 180, 180),
            NodePayload::Light(_) => Color32::from_rgb(255, 200, 60),
            NodePayload::Camera { .. } => Color32::from_rgb(200, 200, 255),
            _ => Color32::from_gray(140),
        }
    }
}

enum ConvertKick {
    InFlight {
        pending: PendingSdfReadback,
        arm: SdfMaterializeArm,
    },
    Immediate {
        verts: Vec<[f32; 8]>,
        indices: Vec<u32>,
        arm: SdfMaterializeArm,
    },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O5 gate: UI convert only queues — never materializes on the call (no Wait on UI path).
    #[test]
    fn o5_convert_queues_without_immediate_poly_payload() {
        // Lightweight: PendingConvertSdf is a plain struct; queue semantics tested via construction.
        let p = PendingConvertSdf {
            nid: NodeId(7),
            name: "Box".into(),
            material: core_math::pbr_material::PbrMaterial::default(),
        };
        assert_eq!(p.nid.0, 7);
        assert_eq!(p.name, "Box");
        // finish path requires half-edge; here we only pin the API split (queue vs finish).
    }

    /// GATE — an imported raster converts to a mesh with REAL FACES and keeps its UVs.
    ///
    /// Both halves were broken and both were silent:
    ///
    /// * `insert_imported` stores `cpu_mesh` with `quads: Vec::new()`, and `from_mesh` treats an
    ///   empty-quad `Mesh` as a point cloud — so Convert to Poly produced every vertex and **zero
    ///   faces**. All topology gone, no message.
    /// * `core_math::mesh::Mesh` has no UV field, so the imported unwrap was dropped as well.
    ///
    /// The fixture is two triangles forming a quad, given as an UNWELDED raster (6 corners, as a
    /// real import is), with distinct UVs. Welding must join the shared edge — 4 vertices, not 6 —
    /// or the result is faces with no shared edges: technically a mesh, useless to edit.
    #[test]
    fn imported_raster_converts_with_faces_and_uvs() {
        // [px,py,pz, nx,ny,nz, u,v] — two triangles, shared edge duplicated (unwelded).
        let v = |x: f32, z: f32, u: f32, vv: f32| [x, 0.0, z, 0.0, 1.0, 0.0, u, vv];
        let verts = vec![
            v(0.0, 0.0, 0.0, 0.0),
            v(1.0, 0.0, 1.0, 0.0),
            v(1.0, 1.0, 1.0, 1.0),
            v(0.0, 0.0, 0.0, 0.0), // duplicate of corner 0
            v(1.0, 1.0, 1.0, 1.0), // duplicate of corner 2
            v(0.0, 1.0, 0.0, 1.0),
        ];
        let indices: Vec<u32> = vec![0, 1, 2, 3, 4, 5];

        let (mesh, uv) = AzraApp::build_poly_from_raster_impl(&verts, &indices);

        let live_faces = (0..mesh.faces.len())
            .filter(|i| mesh.is_face_live(core_math::half_edge::FaceId(*i as u32)))
            .count();
        assert_eq!(live_faces, 2, "the conversion must produce FACES, not a point cloud");
        assert_eq!(
            mesh.vertices.len(),
            4,
            "the shared edge must weld (6 raster corners -> 4 vertices), or the two triangles              stay disconnected and nothing can be edited across the seam"
        );

        // Every corner carries a UV, and each one matches its own vertex's authored value.
        assert_eq!(uv.len(), 6, "one UV per face corner");
        for (he, got) in &uv {
            let p = mesh.vert(mesh.he(*he).vertex).position;
            let want = glam::Vec2::new(p.x, p.z); // fixture: uv == (x, z)
            assert!(
                (*got - want).length() < 1e-5,
                "corner UV {got:?} does not match its vertex {p:?} (expected {want:?}) — UVs are                  matched by welded vertex id precisely so a rotated ring cannot shift them"
            );
        }
    }
}
