//! Display-mesh snapshot that a bake worker can own.

use core_math::half_edge::{FaceId, NO_EDGE};
use core_math::mesh_curvature::RingMesh;
use core_math::scene_graph::NodeId;
use core_math::scene_graph::NodePayload;
use glam::Vec2;

/// How the map this grid feeds will be sampled. Mesh maps always bake in UV space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PolySampleSpace {
    pub projection: u32,
    pub tiling: f32,
    pub blend: f32,
    pub rot: [u8; 3],
}

impl Default for PolySampleSpace {
    fn default() -> Self {
        Self {
            projection: 1,
            tiling: 1.0,
            blend: 4.0,
            rot: [0, 0, 0],
        }
    }
}

/// Frame-thread snapshot of one polygonal body's **post-modifier display mesh**.
#[derive(Clone, Debug, Default)]
pub(crate) struct PolyCurvSource {
    pub rings: RingMesh,
    /// Per-CORNER UV, parallel to `rings.face_ring`.
    pub corner_uv: Vec<Vec2>,
    pub space: PolySampleSpace,
}

impl crate::AzraApp {
    /// The polygonal body mesh maps / curvature should measure — the selected one.
    ///
    /// Shared by the baker, the curvature panel gate, and bake snapshots. Do not use
    /// `scene.active_body_nid` first: that is the SDF edit-scope pointer, and
    /// `ensure_body()` heals it onto an SDF body whenever `body_primitives()` is
    /// `None` (which is exactly what a `PolygonalBody` returns).
    pub(crate) fn poly_curv_body(&self) -> Option<NodeId> {
        let is_poly = |nid: NodeId| -> bool {
            self.scene
                .scene_graph
                .get(nid)
                .is_some_and(|n| matches!(n.payload, NodePayload::PolygonalBody { .. }))
        };
        if let Some(nid) = self.poly_active_body() {
            return Some(nid);
        }
        if let Some(nid) = self.sel.usel.selected_nodes().find(|n| is_poly(*n)) {
            return Some(nid);
        }
        let scoped = self.scene.active_body_nid;
        if is_poly(scoped) {
            return Some(scoped);
        }
        None
    }

    /// Build the bake source for `body` — POST-modifier display mesh + per-corner UVs.
    /// Every failure reports; nothing returns `None` silently.
    pub(crate) fn mesh_map_source(&mut self, body: NodeId) -> Option<PolyCurvSource> {
        let Some(mesh) = self.poly_cached_display_mesh(body) else {
            self.toast("Bake Mesh Maps: the body has no display mesh (modifier stack not evaluated)");
            return None;
        };
        let rings = RingMesh::from_half_edge(&mesh);
        if rings.face_ring.is_empty() {
            self.toast("Bake Mesh Maps: the display mesh has no live faces");
            return None;
        }
        let Some(cd) = self.cache.poly_custom_data.get(&body) else {
            self.toast("Bake Mesh Maps: no UV data on this body — run Poly ▸ UV Unwrap");
            return None;
        };
        let mut corner_uv = Vec::with_capacity(rings.face_ring.len());
        let mut hes = Vec::with_capacity(8);
        let mut missing = 0usize;
        for fi in 0..rings.face_count() {
            let (lo, hi) = (
                rings.face_start[fi] as usize,
                rings.face_start[fi + 1] as usize,
            );
            if hi == lo {
                continue;
            }
            let f = FaceId(fi as u32);
            if mesh.face(f).half_edge == NO_EDGE {
                continue;
            }
            mesh.face_half_edges_into(f, &mut hes);
            for &he in &hes {
                match cd.uv.get(he) {
                    Some(uv) => corner_uv.push(*uv),
                    None => {
                        missing += 1;
                        corner_uv.push(Vec2::ZERO);
                    }
                }
            }
        }
        if corner_uv.len() != rings.face_ring.len() {
            self.toast(&format!(
                "Bake Mesh Maps: UV corner count {} != mesh corners {} — cannot bake to UV",
                corner_uv.len(),
                rings.face_ring.len()
            ));
            return None;
        }
        if missing > 0 {
            self.toast(&format!(
                "Bake Mesh Maps: {missing} corners have no UV (added by the modifier stack after \
                 unwrapping) — re-run UV Unwrap for a clean bake"
            ));
        }
        Some(PolyCurvSource {
            rings,
            corner_uv,
            space: PolySampleSpace::default(),
        })
    }

    /// Snapshot high-poly bodies into low-local space. `None` = self-bake.
    pub(crate) fn snapshot_high_target(
        &mut self,
        low: NodeId,
        settings: &super::BakeSettings,
    ) -> Option<super::project::HighTarget> {
        use super::project::{bake_name_stem, is_cage_name, triangulate_rings, HighTarget};
        use super::BakeSource;
        use core_math::mesh_curvature::{
            vertex_curvature_and_normals_rings, RingMesh,
        };
        use glam::Vec3;

        let BakeSource::HighPoly {
            nids,
            match_by_name,
        } = &settings.source
        else {
            return None;
        };

        let low_name = self
            .scene
            .scene_graph
            .get(low)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_default();
        let low_stem = bake_name_stem(&low_name);
        let low_world = self.scene.scene_graph.world_transform(low);

        let mut candidates: Vec<NodeId> = if nids.is_empty() {
            self.scene
                .scene_graph
                .poly_bodies()
                .map(|(nid, _)| nid)
                .filter(|&nid| nid != low)
                .collect()
        } else {
            nids.iter().copied().filter(|&nid| nid != low).collect()
        };
        candidates.retain(|&nid| {
            if settings.cage_nids.contains(&nid) {
                return false;
            }
            let name = self
                .scene
                .scene_graph
                .get(nid)
                .map(|n| n.payload.display_name().to_string())
                .unwrap_or_default();
            !is_cage_name(&name)
        });
        if *match_by_name {
            candidates.retain(|&nid| {
                let name = self
                    .scene
                    .scene_graph
                    .get(nid)
                    .map(|n| n.payload.display_name().to_string())
                    .unwrap_or_default();
                bake_name_stem(&name) == low_stem
            });
        }
        if candidates.is_empty() {
            self.toast(
                "Bake Mesh Maps: no high-poly bodies resolved — baking the low mesh (self)",
            );
            return None;
        }

        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let mut n0 = Vec::new();
        let mut n1 = Vec::new();
        let mut n2 = Vec::new();
        let mut tri_face = Vec::new();
        let mut face_group = Vec::new();
        let mut face_base = 0u32;

        for hid in candidates {
            let Some(mesh) = self.poly_cached_display_mesh(hid) else {
                continue;
            };
            let rings = RingMesh::from_half_edge(&mesh);
            if rings.face_ring.is_empty() {
                continue;
            }
            let (_, normals) = vertex_curvature_and_normals_rings(&rings, 0);
            let high_world = self.scene.scene_graph.world_transform(hid);
            let to_low = low_world.inverse() * high_world;
            let nrm_m = to_low.inverse().transpose();

            let groups: Vec<u16> = self
                .cache
                .poly_custom_data
                .get(&hid)
                .filter(|cd| !cd.groups.is_empty())
                .map(|cd| {
                    (0..rings.face_count())
                        .map(|fi| {
                            cd.groups
                                .get(core_math::half_edge::FaceId(fi as u32))
                                .copied()
                                .unwrap_or(0)
                        })
                        .collect()
                })
                .unwrap_or_else(|| vec![0; rings.face_count()]);

            let (hp, hi, ha, hb, hc, hf) = triangulate_rings(&rings, &normals);
            let vbase = positions.len() as u32;
            for p in hp {
                let q = to_low.transform_point3(Vec3::from_array(p));
                positions.push([q.x, q.y, q.z]);
            }
            for ix in hi {
                indices.push(ix + vbase);
            }
            for n in ha {
                n0.push(nrm_m.transform_vector3(n).normalize_or_zero());
            }
            for n in hb {
                n1.push(nrm_m.transform_vector3(n).normalize_or_zero());
            }
            for n in hc {
                n2.push(nrm_m.transform_vector3(n).normalize_or_zero());
            }
            for f in hf {
                tri_face.push(f + face_base);
            }
            face_group.extend(groups);
            face_base += rings.face_count() as u32;
        }

        let Some(target) =
            HighTarget::from_soup(positions, indices, n0, n1, n2, tri_face, face_group)
        else {
            self.toast("Bake Mesh Maps: high-poly had no triangles — baking the low mesh");
            return None;
        };
        Some(target)
    }

    /// Snapshot cage bodies into low-local space. `None` = numeric Max Front/Rear only.
    pub(crate) fn snapshot_cage_target(
        &mut self,
        low: NodeId,
        settings: &super::BakeSettings,
    ) -> Option<super::project::CageTarget> {
        use super::project::{bake_name_stem, cage_name_stem, is_cage_name, triangulate_rings, CageTarget};
        use super::BakeSource;
        use core_math::mesh_curvature::RingMesh;
        use glam::Vec3;

        let low_name = self
            .scene
            .scene_graph
            .get(low)
            .map(|n| n.payload.display_name().to_string())
            .unwrap_or_default();
        let low_stem = bake_name_stem(&low_name);
        let match_by_name = matches!(
            settings.source,
            BakeSource::HighPoly {
                match_by_name: true,
                ..
            }
        );

        let candidates: Vec<NodeId> = if settings.cage_nids.is_empty() {
            if !match_by_name {
                return None;
            }
            self.scene
                .scene_graph
                .poly_bodies()
                .map(|(nid, n)| (nid, n.payload.display_name().to_string()))
                .filter(|(nid, name)| {
                    *nid != low && is_cage_name(name) && cage_name_stem(name) == low_stem
                })
                .map(|(nid, _)| nid)
                .collect()
        } else {
            settings
                .cage_nids
                .iter()
                .copied()
                .filter(|&nid| nid != low)
                .collect()
        };
        if candidates.is_empty() {
            return None;
        }

        let low_world = self.scene.scene_graph.world_transform(low);
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for cid in candidates {
            let Some(mesh) = self.poly_cached_display_mesh(cid) else {
                continue;
            };
            let rings = RingMesh::from_half_edge(&mesh);
            if rings.face_ring.is_empty() {
                continue;
            }
            let cage_world = self.scene.scene_graph.world_transform(cid);
            let to_low = low_world.inverse() * cage_world;
            let dummy = vec![Vec3::Z; rings.positions.len()];
            let (cp, ci, _, _, _, _) = triangulate_rings(&rings, &dummy);
            let vbase = positions.len() as u32;
            for p in cp {
                let q = to_low.transform_point3(Vec3::from_array(p));
                positions.push([q.x, q.y, q.z]);
            }
            for ix in ci {
                indices.push(ix + vbase);
            }
        }

        match CageTarget::from_soup(positions, indices) {
            Some(t) => Some(t),
            None => {
                self.toast("Bake Mesh Maps: cage mesh had no triangles — using Max Front");
                None
            }
        }
    }
}
