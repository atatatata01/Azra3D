// Boundary capping — close an open mesh's rims so a SOLID kernel can operate on it, while
// keeping enough information to RE-OPEN those rims afterwards.
//
// Why this exists: the exact boolean refuses any operand carrying a boundary half-edge
// (`mesh_boolean_manifold.rs`), so an intrude / boolean against a mesh that merely has a
// hole in it is impossible today — which contradicts the recorded owner directive "all
// tools must work on open and non-manifold shapes". Capping turns "solid with a hole" into
// a solid the kernel accepts; the caller drops the cap-derived faces afterwards to restore
// the hole. A genuine open SHEET caps to ~zero volume and is routed to a surface cut
// instead — capping is not a universal answer, only the solid-with-holes half of it.

use super::*;

/// An open mesh closed with temporary planar caps, plus the polygons those caps occupy.
pub struct CappedMesh {
    /// The closed mesh — safe to hand to a solid boolean.
    pub mesh: HalfEdgeMesh,
    /// One polygon per cap, as mesh-space positions in the cap face's own winding.
    ///
    /// GEOMETRY, not ids, and deliberately so: a boolean re-tessellates its operands, so no
    /// FaceId survives the call. Testing a result face against these polygons is the only
    /// durable way to answer "did this come from a cap?" — and it is the same technique
    /// `mesh_boolean::collect_provenance` already uses to answer "which operand?".
    pub caps: Vec<Vec<Vec3>>,
}

impl HalfEdgeMesh {
    /// Every boundary loop of the WHOLE mesh, ordered, one ring per hole.
    ///
    /// Thin reuse of [`Self::region_boundary_loops`] over all live faces: with every face in
    /// the region, an edge can only be "outside" when it has no twin — i.e. a real mesh rim.
    /// Inherits that function's refusal policy verbatim (pinch vertex / dead-end walk →
    /// empty), so a caller that cannot handle ambiguous boundaries gets one consistent
    /// answer from both entry points.
    ///
    /// Rings hold DESTINATION vertices in the adjacent faces' winding direction (`he.vertex`
    /// is the destination). A cap filling the hole must therefore be wound in REVERSE — its
    /// half-edges would be the twins of these.
    pub fn mesh_boundary_loops(&self) -> Vec<Vec<VertexId>> {
        let all: std::collections::HashSet<FaceId> = (0..self.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| self.face(f).half_edge != NO_EDGE)
            .collect();
        self.region_boundary_loops(&all)
    }

    /// Close every boundary loop with one planar n-gon cap.
    ///
    /// `None` when the mesh is already closed (nothing to do — the caller uses the plain
    /// solid path unchanged) or when the boundary is not a set of simple closed loops
    /// (`mesh_boundary_loops` refuses; capping garbage is worse than declining).
    ///
    /// REBUILD, not half-edge surgery, and the id loss is deliberate: the sole consumer
    /// feeds this straight into a boolean, which rebuilds BOTH operands anyway, so no stable
    /// id could have survived the call regardless. CLAUDE.md's "prefer ID-preserving ops"
    /// rule is about ops whose output the user keeps editing — this output is a throwaway
    /// kernel operand that never reaches the scene graph.
    pub fn cap_boundary_loops(&self) -> Option<CappedMesh> {
        let loops = self.mesh_boundary_loops();
        if loops.is_empty() {
            return None; // already closed, or an ambiguous boundary we must not guess at
        }

        // Compact live vertices, remembering where each landed.
        let mut positions: Vec<Vec3> = Vec::new();
        let mut remap: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        for i in 0..self.vertices.len() as u32 {
            let v = VertexId(i);
            if self.vert(v).half_edge == NO_EDGE {
                continue;
            }
            remap.insert(i, positions.len());
            positions.push(self.vert(v).position);
        }

        // Every live face ring, unchanged.
        let mut rings: Vec<Vec<usize>> = Vec::new();
        for i in 0..self.faces.len() as u32 {
            let f = FaceId(i);
            if self.face(f).half_edge == NO_EDGE {
                continue;
            }
            let ring: Vec<usize> = self
                .face_vertices(f)
                .iter()
                .filter_map(|v| remap.get(&v.0).copied())
                .collect();
            if ring.len() >= 3 {
                rings.push(ring);
            }
        }

        // One cap per rim, wound OPPOSITE the rim (see `mesh_boundary_loops`).
        let mut caps: Vec<Vec<Vec3>> = Vec::new();
        for lp in &loops {
            let mut ring: Vec<usize> = lp
                .iter()
                .filter_map(|v| remap.get(&v.0).copied())
                .collect();
            if ring.len() != lp.len() || ring.len() < 3 {
                return None; // a rim referencing a dead vertex — refuse rather than cap wrong
            }
            ring.reverse();
            caps.push(ring.iter().map(|&i| positions[i]).collect());
            rings.push(ring);
        }

        let mesh = HalfEdgeMesh::from_polygons(&positions, &rings);
        // Fail closed: if capping did not actually close the mesh, the caller must not
        // hand it to a solid kernel that will only refuse it again (or worse, accept a
        // self-intersecting shell). One check, at the source.
        if !mesh.mesh_boundary_loops().is_empty() {
            return None;
        }
        Some(CappedMesh { mesh, caps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_boolean::{boundary_half_edge_count, mesh_signed_volume};

    /// Unit cube corners, shared by the fixtures below.
    fn cube_pts() -> Vec<Vec3> {
        vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ]
    }

    /// The four side faces, each OUTWARD-wound (verified by Newell: -Z, +X, +Z, -X).
    fn side_faces() -> Vec<Vec<usize>> {
        vec![
            vec![0, 4, 5, 1], // front  z=-1 → -Z
            vec![1, 5, 6, 2], // right  x=+1 → +X
            vec![2, 6, 7, 3], // back   z=+1 → +Z
            vec![3, 7, 4, 0], // left   x=-1 → -X
        ]
    }

    /// All six faces, outward-wound → positive volume.
    fn closed_box() -> HalfEdgeMesh {
        let mut f = vec![
            vec![0, 1, 2, 3], // bottom y=-1 → -Y
            vec![4, 7, 6, 5], // top    y=+1 → +Y
        ];
        f.extend(side_faces());
        HalfEdgeMesh::from_polygons(&cube_pts(), &f)
    }

    /// The same box with the +Y face removed — one 4-edge rim.
    fn open_box() -> HalfEdgeMesh {
        let mut f = vec![vec![0, 1, 2, 3]]; // bottom only
        f.extend(side_faces());
        HalfEdgeMesh::from_polygons(&cube_pts(), &f)
    }

    /// Four sides only — two disjoint rims (+Y and -Y).
    fn open_tube() -> HalfEdgeMesh {
        HalfEdgeMesh::from_polygons(&cube_pts(), &side_faces())
    }

    /// The fixture itself must be outward-wound, or every volume assertion below is
    /// comparing two inside-out meshes and proves nothing (it did, on first run: cap and
    /// box agreed at -8 and the sign assert was the only thing that caught it).
    #[test]
    fn closed_box_fixture_is_outward_wound() {
        assert!(
            mesh_signed_volume(&closed_box()) > 0.0,
            "fixture must be a positively-wound solid"
        );
    }

    #[test]
    fn closed_mesh_has_no_boundary_loops_and_declines_capping() {
        let m = closed_box();
        assert!(m.mesh_boundary_loops().is_empty(), "closed box: no rims");
        assert!(
            m.cap_boundary_loops().is_none(),
            "already closed → None, caller keeps the plain solid path"
        );
    }

    #[test]
    fn open_box_reports_one_rim_of_four() {
        let loops = open_box().mesh_boundary_loops();
        assert_eq!(loops.len(), 1, "one hole");
        assert_eq!(loops[0].len(), 4, "square rim");
    }

    #[test]
    fn open_tube_reports_two_rims() {
        let mut sizes: Vec<usize> = open_tube()
            .mesh_boundary_loops()
            .iter()
            .map(|l| l.len())
            .collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![4, 4], "two square rims");
    }

    /// THE gate: capping an open box must reproduce the closed box a solid kernel accepts —
    /// watertight, manifold, and the SAME positive volume (8.0 for the ±1 cube). A cap wound
    /// the wrong way still closes the mesh but inverts the solid, so the volume sign is the
    /// assertion that actually pins the winding.
    #[test]
    fn capping_an_open_box_yields_the_closed_solid() {
        let capped = open_box().cap_boundary_loops().expect("open box must cap");
        assert_eq!(
            boundary_half_edge_count(&capped.mesh),
            0,
            "capped mesh must be watertight"
        );
        assert!(
            capped.mesh.is_manifold().is_ok(),
            "capped mesh must be manifold: {:?}",
            capped.mesh.is_manifold()
        );
        let want = mesh_signed_volume(&closed_box());
        let got = mesh_signed_volume(&capped.mesh);
        assert!(
            (got - want).abs() < 1e-4,
            "capped volume {got} must match the closed box {want} (sign pins cap winding)"
        );
        assert!(got > 0.0, "capped solid must be positively wound, got {got}");
        assert_eq!(capped.caps.len(), 1, "one cap polygon reported");
        assert_eq!(capped.caps[0].len(), 4, "cap is the 4-vert rim");
        for p in &capped.caps[0] {
            assert!((p.y - 1.0).abs() < 1e-6, "cap sits on the +Y plane: {p:?}");
        }
    }

    #[test]
    fn capping_a_tube_closes_both_rims() {
        let capped = open_tube().cap_boundary_loops().expect("tube must cap");
        assert_eq!(boundary_half_edge_count(&capped.mesh), 0, "watertight");
        assert!(capped.mesh.is_manifold().is_ok(), "manifold");
        assert_eq!(capped.caps.len(), 2, "both rims capped");
        let got = mesh_signed_volume(&capped.mesh);
        assert!(
            (got - mesh_signed_volume(&closed_box())).abs() < 1e-4,
            "capped tube is the cube, got {got}"
        );
    }

    /// A genuine open SHEET caps to ~zero volume. That is the ladder's routing signal: the
    /// caller must send this to a surface cut, never to a volumetric difference.
    #[test]
    fn flat_sheet_caps_to_degenerate_volume() {
        let p = vec![
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let sheet = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        let capped = sheet.cap_boundary_loops().expect("sheet caps (front+back)");
        assert!(
            mesh_signed_volume(&capped.mesh).abs() < 1e-6,
            "a sheet has no volume to subtract from — routing signal for the surface cut"
        );
    }
}
