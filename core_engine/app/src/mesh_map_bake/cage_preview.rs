//! Live viewport cage for Mesh Maps.
//!
//! Numeric Max Front / Max Rear is an envelope along the **display-mesh** vertex
//! normals (the same post-modifier mesh Self bakes). Mesh-cage bodies are drawn
//! as a second X-ray wire so the assigned envelope is obvious while hauling.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use core_math::half_edge::HalfEdgeMesh;
use core_math::mesh_curvature::{vertex_curvature_and_normals_rings, RingMesh};
use core_math::scene_graph::{NodeId, NodePayload};
use glam::{Mat4, Vec3};
use render::line_overlay::LineInstance;

use super::BakeSettings;

const PREVIEW_LINGER: Duration = Duration::from_millis(8_000);
const MAX_CAGE_SEGS: usize = 24_000;
const MAX_STALKS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CageSegKind {
    Front,
    Rear,
    Stalk,
}

#[derive(Clone, Debug)]
pub(crate) struct CageSeg {
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    pub kind: CageSegKind,
}

/// Snapshot of the cage the user is hauling. Runtime-only — never persisted.
#[derive(Clone, Debug)]
pub(crate) struct BakeCagePreview {
    pub body: NodeId,
    pub max_front: f32,
    pub max_rear: f32,
    pub cage_nids: Vec<NodeId>,
    pub until: Instant,
}

/// Display-mesh topology reused across drag frames.
#[derive(Clone, Debug)]
pub(crate) struct BakeCagePreviewCache {
    pub body: NodeId,
    pub stamp: u64,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub edges: Vec<(u32, u32)>,
}

/// Unique edges + smooth vertex normals of the **display** mesh.
pub(crate) fn cage_preview_topo(mesh: &HalfEdgeMesh) -> BakeCagePreviewCache {
    let rings = RingMesh::from_half_edge(mesh);
    let (_, normals) = vertex_curvature_and_normals_rings(&rings, 0);
    let mut edges = Vec::new();
    for he_id in mesh.canonical_edges() {
        let dst = mesh.he(he_id).vertex;
        let src = mesh.he_source(he_id);
        edges.push((src.0, dst.0));
    }
    BakeCagePreviewCache {
        body: NodeId(0),
        stamp: 0,
        positions: rings.positions,
        normals,
        edges,
    }
}

/// Offset the display mesh along ±normals. Front = +n × Max Front, rear = −n × Max Rear.
pub(crate) fn emit_numeric_cage(
    positions: &[Vec3],
    normals: &[Vec3],
    edges: &[(u32, u32)],
    max_front: f32,
    max_rear: f32,
    world: Mat4,
    max_segs: usize,
) -> Vec<CageSeg> {
    let n = positions.len().min(normals.len());
    if n == 0 || max_segs == 0 {
        return Vec::new();
    }
    let front = max_front.max(0.0);
    let rear = max_rear.max(0.0);
    let shells = (front > 0.0) as usize + (rear > 0.0) as usize;
    if shells == 0 && edges.is_empty() {
        return Vec::new();
    }
    let edge_budget = if shells == 0 {
        0
    } else {
        (max_segs.saturating_sub(MAX_STALKS.min(n))) / shells
    };
    let stride = if edge_budget == 0 {
        usize::MAX
    } else {
        ((edges.len() + edge_budget - 1) / edge_budget).max(1)
    };

    let offset = |i: usize, dist: f32| -> Vec3 {
        let p = positions[i];
        let nrm = normals[i].normalize_or_zero();
        world.transform_point3(p + nrm * dist)
    };

    let mut out = Vec::new();
    if stride != usize::MAX {
        let mut i = 0usize;
        while i < edges.len() && out.len() < max_segs {
            let (a, b) = edges[i];
            let ia = a as usize;
            let ib = b as usize;
            if ia < n && ib < n {
                if front > 0.0 {
                    let p0 = offset(ia, front);
                    let p1 = offset(ib, front);
                    out.push(CageSeg {
                        p0: p0.to_array(),
                        p1: p1.to_array(),
                        kind: CageSegKind::Front,
                    });
                }
                if rear > 0.0 && out.len() < max_segs {
                    let p0 = offset(ia, -rear);
                    let p1 = offset(ib, -rear);
                    out.push(CageSeg {
                        p0: p0.to_array(),
                        p1: p1.to_array(),
                        kind: CageSegKind::Rear,
                    });
                }
            }
            i += stride;
        }
    }

    // Stalks: surface → front / rear so the offset reads as a thickness, not a floating shell.
    let stalk_stride = (n / MAX_STALKS).max(1);
    let mut v = 0usize;
    while v < n && out.len() < max_segs {
        let base = world.transform_point3(positions[v]);
        if front > 0.0 {
            let tip = offset(v, front);
            out.push(CageSeg {
                p0: base.to_array(),
                p1: tip.to_array(),
                kind: CageSegKind::Stalk,
            });
        }
        if rear > 0.0 && out.len() < max_segs {
            let tip = offset(v, -rear);
            out.push(CageSeg {
                p0: base.to_array(),
                p1: tip.to_array(),
                kind: CageSegKind::Stalk,
            });
        }
        v += stalk_stride;
    }
    out
}

impl crate::AzraApp {
    pub(crate) fn touch_bake_cage_preview(&mut self, body: NodeId, s: &BakeSettings) {
        let next = BakeCagePreview {
            body,
            max_front: s.max_front.max(0.0),
            max_rear: s.max_rear.max(0.0),
            cage_nids: s.cage_nids.clone(),
            until: Instant::now() + PREVIEW_LINGER,
        };
        let changed = match &self.ui.bake_cage_preview {
            Some(p) => {
                p.body != next.body
                    || p.max_front != next.max_front
                    || p.max_rear != next.max_rear
                    || p.cage_nids != next.cage_nids
            }
            None => true,
        };
        self.ui.bake_cage_preview = Some(next);
        if changed {
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    pub(crate) fn clear_bake_cage_preview(&mut self) {
        if self.ui.bake_cage_preview.is_some() {
            self.ui.bake_cage_preview = None;
            self.overlay_lines_fp = u64::MAX;
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    pub(crate) fn expire_bake_cage_preview(&mut self) {
        if self.ui.bake_cage_preview_pinned {
            return;
        }
        let expired = self
            .ui
            .bake_cage_preview
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.until);
        if expired {
            self.ui.bake_cage_preview = None;
            self.overlay_lines_fp = u64::MAX;
        }
    }

    pub(crate) fn bake_cage_preview_live(&self) -> bool {
        if self.ui.bake_cage_preview_pinned {
            return true;
        }
        self.ui
            .bake_cage_preview
            .as_ref()
            .is_some_and(|p| Instant::now() < p.until)
    }

    pub(crate) fn hash_bake_cage_preview(&self, h: &mut impl Hasher) {
        let Some(p) = self.ui.bake_cage_preview.as_ref() else {
            self.ui.bake_cage_preview_pinned.hash(h);
            return;
        };
        if !self.bake_cage_preview_live() {
            0u8.hash(h);
            return;
        }
        1u8.hash(h);
        self.ui.bake_cage_preview_pinned.hash(h);
        p.body.0.hash(h);
        p.max_front.to_bits().hash(h);
        p.max_rear.to_bits().hash(h);
        for nid in &p.cage_nids {
            nid.0.hash(h);
        }
        self.hash_body_cage_identity(p.body, h);
        for &nid in &p.cage_nids {
            self.hash_body_cage_identity(nid, h);
        }
    }

    fn hash_body_cage_identity(&self, nid: NodeId, h: &mut impl Hasher) {
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return;
        };
        for v in self.scene.scene_graph.world_transform(nid).to_cols_array() {
            v.to_bits().hash(h);
        }
        if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
            (std::sync::Arc::as_ptr(mesh) as usize).hash(h);
        }
        if let Some(e) = self.poly_display_cache.get(&nid) {
            e.key.0.hash(h);
            e.key.1.hash(h);
        }
    }

    /// X-ray numeric cage (+ assigned mesh-cage wires). Called from the overlay line pass.
    pub(crate) fn append_bake_cage_preview(&mut self, out: &mut Vec<LineInstance>) {
        self.expire_bake_cage_preview();
        let Some(preview) = self.ui.bake_cage_preview.clone() else {
            return;
        };
        if !self.bake_cage_preview_live() {
            return;
        }

        let Some(mesh) = self.poly_cached_display_mesh(preview.body) else {
            return;
        };
        let stamp = self
            .poly_display_cache
            .get(&preview.body)
            .map(|e| e.key.0 ^ e.key.1.rotate_left(17))
            .unwrap_or(0);
        let cache_ok = self
            .ui
            .bake_cage_preview_cache
            .as_ref()
            .is_some_and(|c| c.body == preview.body && c.stamp == stamp);
        if !cache_ok {
            let mut topo = cage_preview_topo(mesh.as_ref());
            topo.body = preview.body;
            topo.stamp = stamp;
            self.ui.bake_cage_preview_cache = Some(topo);
        }
        let world = self.scene.scene_graph.world_transform(preview.body);
        if let Some(cache) = self.ui.bake_cage_preview_cache.as_ref() {
            let segs = emit_numeric_cage(
                &cache.positions,
                &cache.normals,
                &cache.edges,
                preview.max_front,
                preview.max_rear,
                world,
                MAX_CAGE_SEGS,
            );
            for s in segs {
                if out.len() >= MAX_CAGE_SEGS.saturating_mul(2) {
                    break;
                }
                let (color, width) = match s.kind {
                    CageSegKind::Front => (LineInstance::pack_color(0.20, 0.95, 1.00, 0.95), 1.8),
                    CageSegKind::Rear => (LineInstance::pack_color(1.00, 0.42, 0.12, 0.90), 1.6),
                    CageSegKind::Stalk => (LineInstance::pack_color(0.95, 0.88, 0.25, 0.55), 1.1),
                };
                out.push(LineInstance {
                    p0: s.p0,
                    p1: s.p1,
                    color,
                    width,
                });
            }
        }

        let mesh_col = LineInstance::pack_color(0.95, 0.30, 0.85, 0.90);
        for cid in preview.cage_nids {
            if cid == preview.body {
                continue;
            }
            let Some(cm) = self.poly_cached_display_mesh(cid) else {
                continue;
            };
            let cw = self.scene.scene_graph.world_transform(cid);
            let mut n = 0usize;
            for he_id in cm.canonical_edges() {
                if out.len() >= MAX_CAGE_SEGS.saturating_mul(2) {
                    break;
                }
                if n >= MAX_CAGE_SEGS / 2 {
                    break;
                }
                let dst = cm.he(he_id).vertex;
                let src = cm.he_source(he_id);
                let p0 = cw.transform_point3(cm.vert(src).position);
                let p1 = cw.transform_point3(cm.vert(dst).position);
                out.push(LineInstance {
                    p0: p0.to_array(),
                    p1: p1.to_array(),
                    color: mesh_col,
                    width: 1.7,
                });
                n += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_quad() -> ([Vec3; 4], [Vec3; 4], [(u32, u32); 4]) {
        (
            [Vec3::ZERO, Vec3::X, Vec3::X + Vec3::Y, Vec3::Y],
            [Vec3::Z; 4],
            [(0, 1), (1, 2), (2, 3), (3, 0)],
        )
    }

    #[test]
    fn numeric_cage_offsets_along_normals() {
        let (pos, nrm, edges) = unit_quad();
        let segs = emit_numeric_cage(&pos, &nrm, &edges, 0.2, 0.05, Mat4::IDENTITY, 1000);
        let fronts: Vec<_> = segs.iter().filter(|s| s.kind == CageSegKind::Front).collect();
        let rears: Vec<_> = segs.iter().filter(|s| s.kind == CageSegKind::Rear).collect();
        assert_eq!(fronts.len(), 4);
        assert_eq!(rears.len(), 4);
        for s in fronts {
            assert!((s.p0[2] - 0.2).abs() < 1e-5, "front z={}", s.p0[2]);
            assert!((s.p1[2] - 0.2).abs() < 1e-5);
        }
        for s in rears {
            assert!((s.p0[2] + 0.05).abs() < 1e-5, "rear z={}", s.p0[2]);
            assert!((s.p1[2] + 0.05).abs() < 1e-5);
        }
    }

    #[test]
    fn numeric_cage_follows_world_transform() {
        let (pos, nrm, edges) = unit_quad();
        let world = Mat4::from_translation(Vec3::new(0.0, 3.0, 0.0));
        let segs = emit_numeric_cage(&pos, &nrm, &edges, 0.1, 0.0, world, 1000);
        let fronts: Vec<_> = segs.iter().filter(|s| s.kind == CageSegKind::Front).collect();
        assert!(!fronts.is_empty());
        for s in fronts {
            // Quad verts are y=0 or y=1 locally → 3 or 4 after the +3 translation.
            assert!(
                (s.p0[1] - 3.0).abs() < 1e-5 || (s.p0[1] - 4.0).abs() < 1e-5,
                "y={}",
                s.p0[1]
            );
            assert!((s.p0[2] - 0.1).abs() < 1e-5);
        }
    }

    #[test]
    fn zero_distances_emit_no_shells() {
        let (pos, nrm, edges) = unit_quad();
        let segs = emit_numeric_cage(&pos, &nrm, &edges, 0.0, 0.0, Mat4::IDENTITY, 1000);
        assert!(segs.iter().all(|s| s.kind == CageSegKind::Stalk) || segs.is_empty());
        assert!(segs
            .iter()
            .filter(|s| s.kind == CageSegKind::Front || s.kind == CageSegKind::Rear)
            .count()
            == 0);
    }
}
