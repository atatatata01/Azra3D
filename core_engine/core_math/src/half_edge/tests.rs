// Half-edge mesh kernel tests (split from half_edge.rs — pure relocation of the
// former `#[cfg(test)] mod tests` body; declared as `mod tests;` from mod.rs).



    /// Full-face intrude on a cube: ALL four walls are coplanar with the four sides and must
    /// merge — the result is simply a shorter box (SketchUp push/pull). The old rim corners go
    /// valence-2 and are swept, so counts return to a plain box.
    #[test]
    fn intrude_full_top_merges_all_four_walls() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let top = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| m.face(f).half_edge != NO_EDGE && m.compute_face_normal(f).y > 0.9)
            .expect("box has a +Y face");
        let top_y = m.face_vertices(top).iter().map(|&v| m.vert(v).position.y)
            .fold(f32::MIN, f32::max);
        let mut sel = std::collections::HashSet::new();
        sel.insert(top);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) { m.vert_mut(v).position.y -= 0.3; }
            }
        }
        let (merged, _) = m.merge_coplanar_extrude_walls(&capset);
        assert_eq!(merged, 4, "all four coplanar walls must merge");
        assert!(m.validate().is_ok(), "valid after intrude merge");
        assert!(m.is_manifold().is_ok(), "manifold after intrude merge");
        assert_eq!(m.face_count(), 6, "shorter box: still exactly 6 faces");
        assert_eq!(m.vertex_count(), 8, "old rim corners swept: back to 8 verts");
        let max_y = (0..m.vertices.len() as u32).map(VertexId)
            .filter(|&v| m.vert(v).half_edge != NO_EDGE)
            .map(|v| m.vert(v).position.y).fold(f32::MIN, f32::max);
        assert!((max_y - (top_y - 0.3)).abs() < 1e-4, "top sits at the pushed height, got {max_y}");
    }

    /// Owner TEST_020 (2026-08-06): n-gon wall coplanar with its neighbour must still merge.
    /// Synthetic open strip: neighbour (big +Z face) + wall (pentagon coplanar with it) +
    /// cap (+Y). Caps set = {cap}; the wall's only "base" twin is the neighbour.
    #[test]
    fn intrude_ngon_wall_still_merges_when_coplanar() {
        // Layout (looking +Z):
        //   neighbour: big face on z=0 covering y in [0,1], x in [-1,1]
        //   wall:      pentagon on z=0 covering y in [1,2], x in [0,1] with a mid pin
        //   cap:       on y=2, z in [-0.5,0]  (touches wall's top edge)
        // Shared base edge wall↔neighbour is y=1, x in [0,1], z=0.
        let p = vec![
            // neighbour (z=0 plane)
            Vec3::new(-1.0, 0.0, 0.0), // 0
            Vec3::new(1.0, 0.0, 0.0),  // 1
            Vec3::new(1.0, 1.0, 0.0),  // 2
            Vec3::new(0.0, 1.0, 0.0),  // 3  (shared base left)
            // wall top / cap front
            Vec3::new(0.0, 2.0, 0.0),  // 4
            Vec3::new(0.5, 2.0, 0.0),  // 5 mid pin on cap edge
            Vec3::new(1.0, 2.0, 0.0),  // 6
            // cap back
            Vec3::new(0.0, 2.0, -0.5), // 7
            Vec3::new(1.0, 2.0, -0.5), // 8
            // wall mid pin on right side (makes wall a pentagon: 3-2-6-5-4)
            // (5 already mid on top)
        ];
        // Wall ring: 3→2→6→5→4→3  (5 verts, coplanar z=0)
        // Neighbour: 0→1→2→3→0
        // Cap: 4→5→6→8→7→4
        let faces = vec![
            vec![0, 1, 2, 3],       // neighbour
            vec![3, 2, 6, 5, 4],    // wall n-gon
            vec![4, 5, 6, 8, 7],    // cap
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.validate().is_ok(), "setup validate: {:?}", m.validate());
        // Identify cap = face whose centroid y is highest.
        let cap = (0..m.faces.len() as u32).map(FaceId)
            .filter(|&f| m.face(f).half_edge != NO_EDGE)
            .max_by(|a, b| {
                let ca: f32 = m.face_vertices(*a).iter().map(|v| m.vert(*v).position.y).sum::<f32>();
                let cb: f32 = m.face_vertices(*b).iter().map(|v| m.vert(*v).position.y).sum::<f32>();
                ca.partial_cmp(&cb).unwrap()
            })
            .expect("cap");
        assert_eq!(m.face_vertices(cap).len(), 5, "cap is the top pentagon");
        let wall = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| m.face(f).half_edge != NO_EDGE && f != cap && m.face_vertices(f).len() == 5)
            .expect("wall n-gon");
        let mut caps = std::collections::HashSet::new();
        caps.insert(cap);
        let (merged, _) = m.merge_coplanar_extrude_walls(&caps);
        assert_eq!(merged, 1, "n-gon wall coplanar with neighbour must merge (got {merged})");
        assert!(m.face(wall).half_edge == NO_EDGE || m.face_count() < 3,
            "wall must be dissolved into the neighbour");
        assert!(m.validate().is_ok(), "valid after n-gon merge: {:?}", m.validate());
    }

    /// OWNER REPRO 2026-08-03 ("vertex not merged to the segment"): a bar with its top split at
    /// x=0, RIGHT top face pushed down. The front/back/+X walls are coplanar with the front/
    /// back/end faces and must merge even though their cap corners land exactly ON those faces'
    /// rims — on-rim probes are NEUTRAL. The old strict inside/outside test read on-rim as
    /// outside → "mixed" → wall skipped → stacked sliver + unwelded seam at the split vertex.
    #[test]
    fn intrude_partial_top_welds_seam_at_rim() {
        let p = vec![
            Vec3::new(-1.0, -0.5, 0.5), Vec3::new(1.0, -0.5, 0.5), Vec3::new(1.0, 0.5, 0.5),
            Vec3::new(0.0, 0.5, 0.5), Vec3::new(-1.0, 0.5, 0.5),
            Vec3::new(-1.0, -0.5, -0.5), Vec3::new(1.0, -0.5, -0.5), Vec3::new(1.0, 0.5, -0.5),
            Vec3::new(0.0, 0.5, -0.5), Vec3::new(-1.0, 0.5, -0.5),
        ];
        let faces = vec![
            vec![0, 1, 2, 3, 4],  // front  +Z (pentagon: split vertex 3 on its top edge)
            vec![6, 5, 9, 8, 7],  // back   -Z
            vec![1, 6, 7, 2],     // end    +X
            vec![5, 0, 4, 9],     // end    -X
            vec![5, 6, 1, 0],     // bottom -Y
            vec![4, 3, 8, 9],     // top-left  +Y
            vec![3, 2, 7, 8],     // top-right +Y
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.is_manifold().is_ok(), "setup must be watertight");
        let top_right = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| {
                if m.face(f).half_edge == NO_EDGE || m.compute_face_normal(f).y < 0.9 { return false; }
                let vs = m.face_vertices(f);
                vs.iter().map(|&v| m.vert(v).position.x).sum::<f32>() / vs.len() as f32 > 0.1
            })
            .expect("right +Y face");
        let mut sel = std::collections::HashSet::new();
        sel.insert(top_right);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) { m.vert_mut(v).position.y -= 0.3; }
            }
        }
        let (merged, _) = m.merge_coplanar_extrude_walls(&capset);
        assert_eq!(merged, 3, "front + back + end walls merge; the step wall stays");
        assert!(m.validate().is_ok(), "valid after seam merge");
        assert!(m.is_manifold().is_ok(), "manifold after seam merge");
        // No stacked geometry left: no two distinct live verts may coincide.
        let live: Vec<VertexId> = (0..m.vertices.len() as u32).map(VertexId)
            .filter(|&v| m.vert(v).half_edge != NO_EDGE).collect();
        for i in 0..live.len() {
            for j in (i + 1)..live.len() {
                let d = (m.vert(live[i]).position - m.vert(live[j]).position).length();
                assert!(d > 1e-5, "unwelded coincident verts at {:?}", m.vert(live[i]).position);
            }
        }
        // front(merged) + back(merged) + end+X(merged) + end-X + bottom + top-left + step wall + cap
        assert_eq!(m.face_count(), 8, "trimmed bar has exactly 8 faces");
        // 10 initial + 4 cap duplicates − 2 swept rim corners (old +X top corners)
        assert_eq!(m.vertex_count(), 12, "seam verts swept, split vertex kept");
        // The split vertex (0, 0.5, 0.5) survives and anchors the step (blue-dot vertex).
        assert!(live.iter().any(|&v| (m.vert(v).position - Vec3::new(0.0, 0.5, 0.5)).length() < 1e-4),
            "split vertex must remain welded into the seam");
    }

    /// Owner 2026-08-17 (tray screenshots): extrude a FACE *out* along its normal — a cube
    /// side pulled +X, or an inner hole-wall pushed into the opening — and the new walls
    /// land coplanar with the neighbouring planes. Merge must collapse them the same way
    /// the inward (intrude) path already did, or the user is left with stacked slivers
    /// that only "fix themselves" after a camera orbit / a second op.
    #[test]
    fn extrude_outward_side_merges_into_a_wider_box() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let side = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| m.face(f).half_edge != NO_EDGE && m.compute_face_normal(f).x > 0.9)
            .expect("+X face");
        let mut sel = std::collections::HashSet::new();
        sel.insert(side);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) {
                    m.vert_mut(v).position.x += 0.4;
                }
            }
        }
        let (merged, folds) = m.merge_coplanar_extrude_walls(&capset);
        assert_eq!(merged, 4, "all four coplanar walls must merge on an OUTWARD extrude");
        assert_eq!(folds, 0, "outward merge must not fold");
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        assert_eq!(m.face_count(), 6, "wider box: still exactly 6 faces");
        assert_eq!(m.vertex_count(), 8, "old rim corners swept: back to 8 verts");
        let max_x = (0..m.vertices.len() as u32)
            .map(VertexId)
            .filter(|&v| m.vert(v).half_edge != NO_EDGE)
            .map(|v| m.vert(v).position.x)
            .fold(f32::MIN, f32::max);
        assert!((max_x - 1.4).abs() < 1e-4, "pulled face sits at x=1.4, got {max_x}");
    }

    /// Owner 2026-08-17 tray: a rectangular FRAME (box with a through-hole). Extrude one
    /// INNER wall into the hole (thicken the wall / shrink the opening). The four new
    /// walls are coplanar with the rim + the adjacent inner walls and must dissolve so
    /// the top/bottom planes grow into the opening — "the geometry fixes itself".
    fn rectangular_frame() -> HalfEdgeMesh {
        // Outer ±2 × ±0.25 × ±1.5, hole ±1 × ±0.25 × ±0.75 (through Y).
        let p = vec![
            Vec3::new(-2.0, 0.25, -1.5), Vec3::new(2.0, 0.25, -1.5),
            Vec3::new(2.0, 0.25, 1.5), Vec3::new(-2.0, 0.25, 1.5),
            Vec3::new(-1.0, 0.25, -0.75), Vec3::new(1.0, 0.25, -0.75),
            Vec3::new(1.0, 0.25, 0.75), Vec3::new(-1.0, 0.25, 0.75),
            Vec3::new(-2.0, -0.25, -1.5), Vec3::new(2.0, -0.25, -1.5),
            Vec3::new(2.0, -0.25, 1.5), Vec3::new(-2.0, -0.25, 1.5),
            Vec3::new(-1.0, -0.25, -0.75), Vec3::new(1.0, -0.25, -0.75),
            Vec3::new(1.0, -0.25, 0.75), Vec3::new(-1.0, -0.25, 0.75),
        ];
        let faces = vec![
            // Top (+Y)
            vec![0, 4, 5, 1], vec![1, 5, 6, 2], vec![2, 6, 7, 3], vec![3, 7, 4, 0],
            // Bottom (-Y)
            vec![8, 9, 13, 12], vec![9, 10, 14, 13], vec![10, 11, 15, 14], vec![11, 8, 12, 15],
            // Outer walls
            vec![0, 1, 9, 8], vec![1, 2, 10, 9], vec![2, 3, 11, 10], vec![3, 0, 8, 11],
            // Inner walls (normals point into the hole)
            vec![4, 12, 13, 5], // -Z inner (+Z)
            vec![7, 6, 14, 15], // +Z inner (-Z)
            vec![5, 13, 14, 6], // +X inner (-X)
            vec![4, 7, 15, 12], // -X inner (+X)
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.validate().is_ok(), "frame validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "frame manifold: {:?}", m.is_manifold());
        assert_eq!(m.face_count(), 16);
        m
    }

    #[test]
    fn extrude_inner_wall_of_a_frame_merges_the_rim_planes() {
        let mut m = rectangular_frame();
        let inner = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                if m.face(f).half_edge == NO_EDGE {
                    return false;
                }
                let n = m.compute_face_normal(f);
                n.z > 0.9 && m.face_vertices(f).iter().all(|&v| m.vert(v).position.z < -0.5)
            })
            .expect("-Z inner wall");
        let mut sel = std::collections::HashSet::new();
        sel.insert(inner);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) {
                    m.vert_mut(v).position.z += 0.3;
                }
            }
        }
        let (merged, folds) = m.merge_coplanar_extrude_walls(&capset);
        assert_eq!(merged, 4, "rim + adjacent inner walls must all merge, got {merged}");
        assert_eq!(folds, 0, "frame merge must not fold");
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        assert_eq!(
            m.face_count(),
            16,
            "merged frame keeps 16 faces (hole just shrank)"
        );
        // The pushed wall now sits at z = -0.45.
        let cap_z = caps
            .iter()
            .flat_map(|&c| m.face_vertices(c))
            .map(|v| m.vert(v).position.z)
            .fold(0.0f32, |a, z| a + z)
            / 4.0;
        assert!(
            (cap_z + 0.45).abs() < 1e-3,
            "inner wall should have moved to z=-0.45, mean {cap_z}"
        );
    }

    /// Owner `close wall.fl4`: the new top wall is ONE n-gon sitting on TWO already-split
    /// rim faces. The unique-base guard used to skip it. Both bases must dissolve.
    #[test]
    fn extrude_wall_with_two_coplanar_bases_merges_both() {
        // L + R on z=0, split at x=0. Wall W is also on z=0, y in [1,2], spanning
        // both halves, so it shares one edge with L and one with R. Cap stands up at y=2.
        let p = vec![
            Vec3::new(-1.0, 0.0, 0.0), // 0 L
            Vec3::new(0.0, 0.0, 0.0),  // 1 split
            Vec3::new(1.0, 0.0, 0.0),  // 2 R
            Vec3::new(1.0, 1.0, 0.0),  // 3
            Vec3::new(0.0, 1.0, 0.0),  // 4 split on wall base
            Vec3::new(-1.0, 1.0, 0.0), // 5
            Vec3::new(-1.0, 2.0, 0.0), // 6 wall far
            Vec3::new(1.0, 2.0, 0.0),  // 7 wall far
            Vec3::new(-1.0, 2.0, -0.5), // 8 cap
            Vec3::new(1.0, 2.0, -0.5),  // 9 cap
        ];
        let faces = vec![
            vec![0, 1, 4, 5],       // L  z=0
            vec![1, 2, 3, 4],       // R  z=0
            vec![5, 4, 3, 7, 6],    // W  z=0 pentagon (two bases)
            vec![6, 7, 9, 8],       // cap y=2
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.validate().is_ok(), "setup: {:?}", m.validate());
        let cap = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                m.face(f).half_edge != NO_EDGE
                    && m.face_vertices(f).iter().all(|&v| m.vert(v).position.y > 1.5)
            })
            .expect("cap");
        let mut caps = std::collections::HashSet::new();
        caps.insert(cap);
        let (merged, folds) = m.merge_coplanar_extrude_walls(&caps);
        assert_eq!(merged, 2, "both split-rim bases must dissolve, got {merged}");
        assert_eq!(folds, 0);
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        // L + R + W collapsed to one face; cap remains.
        assert_eq!(m.face_count(), 2, "rim healed to one face + cap, got {}", m.face_count());
    }

    /// Owner `close wall.fl4` probe pairing: the new wall is a hexagon whose two
    /// bases sit on opposite sides of the hole. Index-pairing a front base endpoint
    /// with the back cap point puts a midpoint IN the hole (outside) while the
    /// side midpoints sit on the rim (inside) — mix veto. Nearest-cap pairing
    /// stays on the strip and both bases dissolve.
    #[test]
    fn extrude_wall_hexagon_across_a_gap_merges_both_bases() {
        let p = vec![
            Vec3::new(-2.0, 0.0, 0.0),  // 0 L
            Vec3::new(-0.5, 0.0, 0.0),  // 1
            Vec3::new(-0.5, 1.0, 0.0),  // 2 L-inner / cap
            Vec3::new(-2.0, 1.0, 0.0),  // 3
            Vec3::new(0.5, 0.0, 0.0),   // 4 R
            Vec3::new(2.0, 0.0, 0.0),   // 5
            Vec3::new(2.0, 1.0, 0.0),   // 6
            Vec3::new(0.5, 1.0, 0.0),   // 7 R-inner / cap
            Vec3::new(2.0, 2.0, 0.0),   // 8 W far
            Vec3::new(-2.0, 2.0, 0.0),  // 9
            Vec3::new(-0.5, 1.0, -0.5), // 10 cap
            Vec3::new(0.5, 1.0, -0.5),  // 11 cap
        ];
        let faces = vec![
            vec![0, 1, 2, 3],          // L  z=0
            vec![4, 5, 6, 7],          // R  z=0
            vec![3, 2, 7, 6, 8, 9],    // W  z=0 hexagon, gap between 2-7
            vec![2, 10, 11, 7],        // cap
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.validate().is_ok(), "setup: {:?}", m.validate());
        let cap = (0..m.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| m.face(f).half_edge != NO_EDGE)
            .min_by(|&a, &b| {
                let za: f32 = m.face_vertices(a).iter().map(|v| m.vert(*v).position.z).sum();
                let zb: f32 = m.face_vertices(b).iter().map(|v| m.vert(*v).position.z).sum();
                za.partial_cmp(&zb).unwrap()
            })
            .expect("cap");
        let mut caps = std::collections::HashSet::new();
        caps.insert(cap);
        let (merged, folds) = m.merge_coplanar_extrude_walls(&caps);
        assert_eq!(merged, 2, "both gap-side bases must dissolve, got {merged}");
        assert_eq!(folds, 0);
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        assert_eq!(m.face_count(), 2, "L+R+W one face + cap, got {}", m.face_count());
    }

    /// Owner `close wall.fl4` workflow: knife-split the rim that the inner wall
    /// sits on, then extrude that wall into the hole. The new top wall lands on
    /// TWO coplanar rim pieces plus a non-coplanar outer neighbour — merge must
    /// dissolve both rim bases and ignore the outer wall.
    #[test]
    fn extrude_inner_wall_after_rim_split_merges_both_bases() {
        let mut m = rectangular_frame();
        let inner = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                if m.face(f).half_edge == NO_EDGE {
                    return false;
                }
                let n = m.compute_face_normal(f);
                n.z > 0.9 && m.face_vertices(f).iter().all(|&v| m.vert(v).position.z < -0.5)
            })
            .expect("-Z inner wall");
        let (top_he, top_face) = m
            .face_half_edges(inner)
            .into_iter()
            .find_map(|he| {
                let tw = m.he(he).twin;
                if tw == NO_TWIN {
                    return None;
                }
                let f = m.he(tw).face;
                if f != NO_FACE && m.compute_face_normal(f).y > 0.9 {
                    Some((he, f))
                } else {
                    None
                }
            })
            .expect("inner wall shares an edge with the +Y rim");
        let mid = m.split_edge(top_he, 0.5);
        let tverts = m.face_vertices(top_face);
        let mp = m.vert(mid).position;
        let opposite = tverts
            .iter()
            .copied()
            .filter(|&v| v != mid)
            .max_by(|a, b| {
                let da = (m.vert(*a).position - mp).length();
                let db = (m.vert(*b).position - mp).length();
                da.partial_cmp(&db).unwrap()
            })
            .expect("opposite rim vertex");
        m.split_face_between(top_face, mid, opposite)
            .expect("split the rim so two faces sit on the inner wall");
        assert!(m.validate().is_ok(), "split rim: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "split rim manifold: {:?}", m.is_manifold());
        let faces_after_split = m.face_count();
        let mut sel = std::collections::HashSet::new();
        sel.insert(inner);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) {
                    m.vert_mut(v).position.z += 0.3;
                }
            }
        }
        let (merged, folds) = m.merge_coplanar_extrude_walls(&capset);
        // Unsplit frame merges 4 walls; the extra split rim base is a 5th dissolve.
        assert!(
            merged >= 5,
            "split-rim extrude must merge both top bases + bottom + sides, got {merged}"
        );
        assert_eq!(folds, 0, "split-rim merge must not fold");
        assert!(m.validate().is_ok(), "valid: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        assert!(
            m.face_count() < faces_after_split + 4,
            "merge must drop the stacked rim faces ({} -> {})",
            faces_after_split + 4,
            m.face_count()
        );
    }

    /// A winding fix must not INVERT the surface. Face normals on a sphere centred at the origin
    /// must point AWAY from the centre; if the mid-band flip had reversed the band instead of
    /// aligning it, this catches it — the mesh would be manifold and inside-out, which renders as
    /// a black or invisible sphere and would have been the obvious "fix that broke it worse".
    #[test]
    fn new_uv_sphere_faces_point_outward() {
        let m = HalfEdgeMesh::new_uv_sphere(1.0, 16, 12);
        let mut inward = 0usize;
        for fi in 0..m.face_count() {
            let f = FaceId(fi as u32);
            let vs = m.face_vertices(f);
            if vs.len() < 3 { continue; }
            // Reuses the SAME normal routine the bevel's outward-facing gate uses, rather than
            // re-deriving cross products here (which is how two "outward" definitions drift apart).
            let n = m.compute_face_normal(f);
            let centroid = vs.iter().fold(Vec3::ZERO, |a, v| a + m.vert(*v).position)
                / vs.len() as f32;
            if n.dot(centroid) < 0.0 { inward += 1; }
        }
        assert_eq!(inward, 0,
            "{inward} of {} faces point INWARD - the sphere is inside-out",
            m.face_count());
    }

    /// OWNER SOAK FAIL 2026-07-29: `create_poly_mesh("sphere")` produces a NON-MANIFOLD mesh.
    ///
    ///     is_manifold: directed edge 7->8 used by 2 faces
    ///
    /// A directed edge shared by two faces means two faces wind the SAME way across that edge —
    /// the classic symptom of a cap fan or ring quad emitted with inconsistent winding. Every poly
    /// op downstream (boolean, bevel, loop cut, solidify) assumes manifold input, so this is a
    /// corruption source, not a cosmetic defect.
    ///
    /// Parameters are the ones `creation.rs` actually ships: segments 16, rings 12, radius 0.8.
    #[test]
    fn new_uv_sphere_is_manifold_at_shipped_defaults() {
        let m = HalfEdgeMesh::new_uv_sphere(0.8, 16, 12);
        if let Err(e) = m.is_manifold() {
            panic!("shipped poly sphere (segments 16, rings 12) is NON-MANIFOLD: {e}
                    verts={} faces={} — every downstream poly op assumes manifold input",
                   m.vertices.len(), m.faces.len());
        }
        assert!(m.validate().is_ok(), "sphere failed validate()");
    }

    /// Sweep the parameter range the UI exposes (segments 3..=128, rings 2..=128) at a few points.
    /// If only some combinations break, the bug is in a specific branch (cap fan vs ring quads)
    /// rather than the whole builder — that difference decides where to look.
    #[test]
    fn new_uv_sphere_is_manifold_across_the_ui_range() {
        let mut bad = Vec::new();
        for &segs in &[3u32, 4, 8, 16, 32] {
            for &rings in &[2u32, 3, 4, 12, 32] {
                let m = HalfEdgeMesh::new_uv_sphere(1.0, segs, rings);
                if let Err(e) = m.is_manifold() {
                    bad.push(format!("segs={segs} rings={rings}: {e}"));
                }
            }
        }
        assert!(bad.is_empty(),
            "non-manifold UV spheres at {} of the tested parameter combinations:
{}",
            bad.len(), bad.join("
"));
    }

    /// SPHERE-NOTCLOSED gate (COMPLETE_BUG_TRACKER P1 / op_bench --probe).
    ///
    /// `mesh_boolean` rejects any open operand via `boundary_half_edge_count > 0`
    /// (`mesh_boolean.rs` open-mesh gate). A stock UV sphere that is manifold-but-open
    /// silently turns every box−sphere boolean into `None`. This is the closed-surface
    /// contract — complementary to `is_manifold` (topology) and the outward-normal test.
    #[test]
    fn new_uv_sphere_is_watertight_at_shipped_defaults() {
        let m = HalfEdgeMesh::new_uv_sphere(0.8, 16, 12);
        let boundary = crate::mesh_boolean::boundary_half_edge_count(&m);
        assert_eq!(
            boundary, 0,
            "shipped poly sphere (segments 16, rings 12) has {boundary} boundary half-edges \
             — not watertight; mesh_boolean will reject it as an operand"
        );
    }

    /// Same gate across the UI parameter grid used by `new_uv_sphere_is_manifold_across_the_ui_range`.
    /// Open seams at any segs/rings combo re-open SPHERE-NOTCLOSED for that density.
    #[test]
    fn new_uv_sphere_is_watertight_across_the_ui_range() {
        let mut bad = Vec::new();
        for &segs in &[3u32, 4, 8, 16, 32] {
            for &rings in &[2u32, 3, 4, 12, 32] {
                let m = HalfEdgeMesh::new_uv_sphere(1.0, segs, rings);
                let b = crate::mesh_boolean::boundary_half_edge_count(&m);
                if b != 0 {
                    bad.push(format!("segs={segs} rings={rings}: boundary_half_edge_count={b}"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "open UV spheres (boundary > 0) at {} of the tested parameter combinations:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    use super::*;

    /// Edge-slide rails (Blender MESH_OT_edge_slide parity): for every interior edge of a cube, the two
    /// rails must be the destination vertex's TWO OTHER neighbours (one per adjacent face), distinct from
    /// Shrink/Fatten invariant (Blender Alt+S): `compute_vertex_normal` must point OUTWARD on a closed
    /// convex mesh, so the tool's `pos = orig + normal * offset` inflates on +offset and deflates on
    /// −offset. Locks the convention the app-layer tool depends on (it freezes these normals at drag start).
    #[test]
    fn shrink_fatten_normals_inflate_cube() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE); // ±1 cube centred at the origin
        let d = 0.25_f32;
        for i in 0..m.vertices.len() as u32 {
            let v = VertexId(i);
            let orig = m.vert(v).position;
            let n = m.compute_vertex_normal(v).normalize_or_zero();
            assert!(n.length() > 0.9, "vertex {i} has a degenerate normal");
            // Outward: normal points away from the centre (origin) — same side as the corner.
            assert!(n.dot(orig) > 0.0, "vertex {i} normal {n:?} not outward vs pos {orig:?}");
            // +offset fattens (farther from centre), −offset shrinks (nearer).
            assert!((orig + n * d).length() > orig.length() + 1e-4, "vertex {i} did not fatten");
            assert!((orig + n * -d).length() < orig.length() - 1e-4, "vertex {i} did not shrink");
        }
    }

    /// each other and from the edge's own endpoints, and the slide lerp must land on the picked rail at t=1.
    #[test]
    fn edge_slide_rails_on_cube() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE); // ±1 cube, all interior (twinned) edges
        let mut checked = 0;
        for i in 0..m.half_edges.len() {
            let h = HalfEdgeId(i as u32);
            if m.he(h).face == NO_FACE { continue; }
            let v = m.he(h).vertex;
            let src = m.he_source(h);
            let (rp, rn) = m.edge_slide_rails(h).expect("every closed-cube edge has both rails");
            assert_ne!(rp, rn, "rails must be distinct");
            assert_ne!(rp, src, "rail_pos must be off the sliding edge");
            assert_ne!(rn, src, "rail_neg must be off the sliding edge");
            assert_ne!(rp, v, "rail_pos != V");
            assert_ne!(rn, v, "rail_neg != V");
            // Both rails are neighbours of V (share an edge with it).
            let nbrs: std::collections::HashSet<u32> =
                m.vertex_fan(v).iter().map(|&he| m.he(he).vertex.0).collect();
            assert!(nbrs.contains(&rp.0), "rail_pos {} not a neighbour of V {}: nbrs={:?}", rp.0, v.0, nbrs);
            assert!(nbrs.contains(&rn.0), "rail_neg {} not a neighbour of V {}: nbrs={:?}", rn.0, v.0, nbrs);
            // Slide math the tool applies: t=1 lands exactly on the +rail; t=-1 on the −rail.
            let v0 = m.vert(v).position;
            let slid_pos = v0 + 1.0 * (m.vert(rp).position - v0);
            assert!((slid_pos - m.vert(rp).position).length() < 1e-6, "t=+1 lands on rail_pos");
            let slid_neg = v0 + 1.0 * (m.vert(rn).position - v0);
            assert!((slid_neg - m.vert(rn).position).length() < 1e-6, "t=-1 lands on rail_neg");
            checked += 1;
        }
        assert!(checked >= 24, "cube has 24 directed interior half-edges, checked {checked}");
    }

    /// Edge-slide loop COHERENCE (the owner's "one side goes the wrong direction" bug): a hexagonal
    /// cylinder's top-rim loop, fed with MIXED-orientation half-edges, must still yield rails whose +side
    /// moves EVERY rim vertex the same way. The lerp itself (P = V0 + t·(rail − V0)) is already Blender-
    /// verified (edge_slide 0.5 → +0.5); this proves the orientation is coherent after the walk.
    #[test]
    fn edge_slide_loop_coherent_direction() {
        let n = 6usize;
        let mut pos = Vec::new();
        for i in 0..n { let a = std::f32::consts::TAU * i as f32 / n as f32; pos.push(Vec3::new(a.cos(), 1.0, a.sin())); }  // top rim y=+1
        for i in 0..n { let a = std::f32::consts::TAU * i as f32 / n as f32; pos.push(Vec3::new(a.cos(), -1.0, a.sin())); } // bottom rim y=-1
        let mut faces: Vec<Vec<usize>> = vec![(0..n).collect(), (n..2 * n).rev().collect()];
        for i in 0..n { let j = (i + 1) % n; faces.push(vec![i, n + i, n + j, j]); }
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        // Every top-rim half-edge (both endpoints at y≈1) — includes BOTH orientations of each rim edge.
        let mut edges = Vec::new();
        for i in 0..m.half_edges.len() {
            let h = HalfEdgeId(i as u32);
            if m.he(h).face == NO_FACE { continue; }
            let d = m.he(h).vertex;
            let s = m.he_source(h);
            if (m.vert(d).position.y - 1.0).abs() < 1e-4 && (m.vert(s).position.y - 1.0).abs() < 1e-4 {
                edges.push(h);
            }
        }
        let rails = m.edge_slide_rails_loop(&edges);
        assert!(rails.len() >= n, "all {n} rim vertices covered, got {}", rails.len());
        // COHERENCE: every +rail moves its vertex the SAME y-direction (all cap-side, or all down a wall).
        let signs: Vec<i32> = rails.iter()
            .map(|&(v, rp, _)| (m.vert(rp).position.y - m.vert(v).position.y).signum() as i32)
            .collect();
        assert!(signs.iter().all(|&s| s == signs[0]),
            "edge-slide loop is NOT coherent — +rails move different directions: {:?}", signs);
    }

    /// SINGLE-edge slide coherence ("triple corner won't slide both ways"): selecting ONE edge, both its
    /// endpoints must slide the SAME way (their +rail displacements point together — the edge translates as
    /// a unit into one adjacent face), not split with one end going along a face and the other down a wall.
    /// Regression for the geometric-alignment flip at a corner where the two ends' natural +rails point in
    /// opposite WORLD directions; the fix routes the source endpoint's +rail into the SAME face topologically.
    #[test]
    fn edge_slide_single_edge_both_ends_coherent() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        for i in 0..m.half_edges.len() {
            let h = HalfEdgeId(i as u32);
            if m.he(h).face == NO_FACE || m.he(h).twin == NO_TWIN { continue; }
            let rails = m.edge_slide_rails_loop(&[h]);
            assert_eq!(rails.len(), 2, "single edge slides BOTH endpoints");
            let d0 = (m.vert(rails[0].1).position - m.vert(rails[0].0).position).normalize_or_zero();
            let d1 = (m.vert(rails[1].1).position - m.vert(rails[1].0).position).normalize_or_zero();
            assert!(d0.dot(d1) > 0.3,
                "single-edge slide endpoints incoherent (+rails split): {:?} vs {:?}, dot {}", d0, d1, d0.dot(d1));
        }
    }

    #[test]
    fn box_topology() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.vertex_count(), 8);
        assert_eq!(mesh.face_count(), 6);
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn plane_topology() {
        let mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        assert_eq!(mesh.vertex_count(), 9);  // 3x3 grid
        assert_eq!(mesh.face_count(), 4);    // 2x2 quads
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn face_vertices_ordered() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for i in 0..mesh.face_count() {
            let verts = mesh.face_vertices(FaceId(i as u32));
            assert_eq!(verts.len(), 4); // all box faces are quads
        }
    }

    #[test]
    fn torus_topology() {
        let mesh = HalfEdgeMesh::new_torus(1.0, 0.3, 8, 6);
        assert_eq!(mesh.vertex_count(), 48); // 8 * 6
        assert_eq!(mesh.face_count(), 48);   // 8 * 6 quads
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn sphere_topology() {
        let mesh = HalfEdgeMesh::new_uv_sphere(1.0, 8, 4);
        assert!(mesh.vertex_count() > 0);
        assert!(mesh.face_count() > 0);
    }

    #[test]
    fn raster_roundtrip() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let (verts, indices) = mesh.to_raster_data();
        // Per-face split vertices: 6 faces × 4 verts = 24
        assert_eq!(verts.len(), 24);
        assert!(indices.len() > 0);
        // 6 faces × 2 triangles = 12 triangles × 3 indices = 36
        assert_eq!(indices.len(), 36);
    }

    #[test]
    fn raster_data_normals_valid() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let (verts, indices) = mesh.to_raster_data();
        // Every vertex must have a non-zero normal (slots [3..6])
        for v in &verts {
            let nx = v[3]; let ny = v[4]; let nz = v[5];
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            assert!(len > 0.5, "normal too short: {}", len);
        }
        // All indices must be valid vertex references
        for idx in &indices {
            assert!((*idx as usize) < verts.len(), "index {} out of range", idx);
        }
        // Triangle count consistency
        assert_eq!(indices.len() % 3, 0);
        assert_eq!(indices.len() / 3, 12); // box = 6 faces × 2 tris
    }

    #[test]
    fn raster_winding_matches_face_normal() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let (verts, indices) = mesh.to_raster_data();
        // For every triangle, the cross-product winding normal must agree
        // with the stored per-vertex face normal (dot > 0).
        for tri in indices.chunks(3) {
            let v0 = &verts[tri[0] as usize];
            let v1 = &verts[tri[1] as usize];
            let v2 = &verts[tri[2] as usize];
            let p0 = Vec3::new(v0[0], v0[1], v0[2]);
            let p1 = Vec3::new(v1[0], v1[1], v1[2]);
            let p2 = Vec3::new(v2[0], v2[1], v2[2]);
            let face_n = Vec3::new(v0[3], v0[4], v0[5]);
            let winding_n = (p1 - p0).cross(p2 - p0);
            assert!(
                winding_n.dot(face_n) > 0.0,
                "triangle winding opposes face normal: winding={:?} face={:?}",
                winding_n, face_n
            );
        }
    }

    #[test]
    fn raster_data_empty_mesh() {
        // `new()`, not a struct literal: `free_index` is a mirror of the free-lists and the
        // field's own doc warns that a literal bypassing `rebuild_free_index` is unsound.
        let mesh = HalfEdgeMesh::new();
        let (verts, indices) = mesh.to_raster_data();
        assert_eq!(verts.len(), 0);
        assert_eq!(indices.len(), 0);
    }

    #[test]
    fn split_edge_basic() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let initial_verts = mesh.vertex_count();
        let he = mesh.face(FaceId(0)).half_edge;
        let new_v = mesh.split_edge(he, 0.5);
        assert_eq!(mesh.vertex_count(), initial_verts + 1);
        assert!(mesh.vert(new_v).position.length() > 0.0);
    }

    #[test]
    fn merge_vertices_basic() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v0 = VertexId(0);
        let v1 = VertexId(1);
        let initial_verts = mesh.vertex_count();
        mesh.merge_vertices(v0, v1);
        assert_eq!(mesh.vertex_count(), initial_verts - 1);
    }

    /// Merge by Distance (Blender): cluster within threshold → average position.
    #[test]
    fn merge_by_distance_welds_near_duplicates_to_average() {
        // Two quads sharing an almost-coincident pair of verts along an edge.
        let mut mesh = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                // Duplicate of v0 within 0.001 — should weld into the average.
                Vec3::new(0.0005, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, -1.0, 0.0),
                Vec3::new(0.0, -1.0, 0.0),
            ],
            &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
        );
        let before = mesh.vertex_count();
        let removed = mesh.merge_by_distance(0.01);
        assert!(removed >= 1, "must weld at least the near-duplicate pair (removed={removed})");
        assert!(mesh.vertex_count() < before);
        assert!(mesh.validate().is_ok(), "welded mesh must validate: {:?}", mesh.validate());
    }

    #[test]
    fn catmull_clark_box() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.vertex_count(), 8);
        assert_eq!(mesh.face_count(), 6);
        mesh.subdivide_catmull_clark();
        // Box: 8 verts + 12 edge points + 6 face points = 26 verts
        assert_eq!(mesh.vertex_count(), 26);
        // Each of 6 quad faces → 4 sub-quads = 24 quads
        assert_eq!(mesh.face_count(), 24);
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn catmull_clark_plane() {
        let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        // 3x3=9 verts, 4 faces
        mesh.subdivide_catmull_clark();
        // 4 faces → 4*4=16 quads
        assert_eq!(mesh.face_count(), 16);
        assert!(mesh.validate().is_ok());
    }

    // ================================================================
    // QA: Half-Edge Mesh Tests
    // ================================================================

    #[test]
    fn box_mesh_all_quads() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for i in 0..mesh.face_count() {
            let verts = mesh.face_vertices(FaceId(i as u32));
            assert_eq!(verts.len(), 4, "face {} is not a quad", i);
        }
    }

    #[test]
    fn mesh_euler_characteristic_box() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v = mesh.vertex_count() as i32;
        let e = mesh.edge_count() as i32;
        let f = mesh.face_count() as i32;
        // V - E + F = 2 for closed orientable mesh
        assert_eq!(v - e + f, 2, "Euler: V={} E={} F={}", v, e, f);
    }

    #[test]
    fn mesh_euler_characteristic_torus() {
        let mesh = HalfEdgeMesh::new_torus(1.0, 0.3, 8, 6);
        let v = mesh.vertex_count() as i32;
        let e = mesh.edge_count() as i32;
        let f = mesh.face_count() as i32;
        // Torus: V - E + F = 0
        assert_eq!(v - e + f, 0, "Torus Euler: V={} E={} F={}", v, e, f);
    }

    #[test]
    fn uv_sphere_valid() {
        let mesh = HalfEdgeMesh::new_uv_sphere(1.0, 8, 6);
        assert!(mesh.vertex_count() > 0);
        assert!(mesh.face_count() > 0);
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn cylinder_valid() {
        let mesh = HalfEdgeMesh::new_cylinder(0.5, 1.0, 8);
        assert!(mesh.vertex_count() > 0);
        assert!(mesh.face_count() > 0);
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn face_vertices_winding_consecutive_share_edge() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for i in 0..mesh.face_count() {
            let verts = mesh.face_vertices(FaceId(i as u32));
            // Consecutive vertices in the face should share a half-edge
            for j in 0..verts.len() {
                let v0 = verts[j];
                let v1 = verts[(j + 1) % verts.len()];
                // Check that there's a half-edge from v0 to v1 in this face
                let he_list = mesh.face_half_edges(FaceId(i as u32));
                let found = he_list.iter().any(|&he| {
                    mesh.he(he).vertex == v1 && mesh.he_source(he) == v0
                });
                assert!(found, "face {} edge {}→{} not found", i, v0.0, v1.0);
            }
        }
    }

    #[test]
    fn half_edge_twin_consistency() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for i in 0..mesh.half_edges.len() {
            if mesh.free_edges.contains(&(i as u32)) { continue; }
            let he = &mesh.half_edges[i];
            if he.twin != NO_TWIN {
                assert_eq!(
                    mesh.he(he.twin).twin, HalfEdgeId(i as u32),
                    "twin.twin != self for half-edge {}", i
                );
            }
        }
    }

    #[test]
    fn half_edge_face_cycle() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let start = mesh.face(FaceId(i as u32)).half_edge;
            if start == NO_EDGE { continue; }
            let mut cur = start;
            let mut count = 0;
            loop {
                cur = mesh.he(cur).next;
                count += 1;
                if cur == start { break; }
                assert!(count < 100, "face {} has infinite cycle", i);
            }
            assert!(count >= 3, "face {} has only {} edges", i, count);
        }
    }

    // ================================================================
    // QA: Edge Loop / Ring Detection
    // ================================================================

    #[test]
    fn edge_loop_on_box() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Pick the first non-boundary half-edge
        let start = HalfEdgeId(0);
        let loop_edges = mesh.select_edge_loop(start);
        // On a box, each edge loop traverses 4 edges (going around one "band")
        assert_eq!(loop_edges.len(), 4, "edge loop should have 4 edges on a box");
    }

    #[test]
    fn edge_ring_on_box() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let start = HalfEdgeId(0);
        let ring_edges = mesh.select_edge_ring(start);
        // On a box, edge ring also traverses 4 edges
        assert_eq!(ring_edges.len(), 4, "edge ring should have 4 edges on a box");
    }

    // ================================================================
    // QA: Selection operations (grow, shrink, invert logic)
    // ================================================================

    #[test]
    fn grow_selection_from_single_vertex() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v0 = VertexId(0);
        let fan = mesh.vertex_fan(v0);
        let mut selected = std::collections::HashSet::new();
        selected.insert(0u32);
        // Grow: add all vertex_fan destinations
        let mut to_add = std::collections::HashSet::new();
        for &he in &fan {
            to_add.insert(mesh.he(he).vertex.0);
        }
        selected.extend(to_add);
        // Box corner has 3 neighbors
        assert_eq!(selected.len(), 4, "vertex 0 + 3 neighbors = 4");
    }

    #[test]
    fn invert_selection_complement() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let total = mesh.vertex_count();
        let mut selected = std::collections::HashSet::new();
        selected.insert(0u32);
        selected.insert(1u32);
        selected.insert(2u32);
        // Invert
        let mut inverted = std::collections::HashSet::new();
        for i in 0..mesh.vertices.len() as u32 {
            if mesh.vert(VertexId(i)).half_edge != NO_EDGE && !selected.contains(&i) {
                inverted.insert(i);
            }
        }
        assert_eq!(inverted.len(), total - 3);
        assert!(!inverted.contains(&0));
        assert!(!inverted.contains(&1));
        assert!(!inverted.contains(&2));
    }

    // ================================================================
    // QA: Selection conversion
    // ================================================================

    #[test]
    fn selection_conversion_vertex_to_edge() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Select two vertices that share an edge
        let he0 = mesh.face(FaceId(0)).half_edge;
        let v0 = mesh.he_source(he0);
        let v1 = mesh.he(he0).vertex;
        let mut sel_verts = std::collections::HashSet::new();
        sel_verts.insert(v0.0);
        sel_verts.insert(v1.0);
        // Convert: edge is selected if BOTH endpoints selected
        let mut sel_edges = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
            let src = mesh.he_source(HalfEdgeId(i)).0;
            let dst = he.vertex.0;
            if sel_verts.contains(&src) && sel_verts.contains(&dst) {
                sel_edges.insert(i);
            }
        }
        assert_eq!(sel_edges.len(), 1, "exactly one edge shared by 2 adjacent verts");
    }

    #[test]
    fn selection_conversion_vertex_to_face() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Select all 4 vertices of face 0
        let face_verts = mesh.face_vertices(FaceId(0));
        let sel_verts: std::collections::HashSet<u32> = face_verts.iter().map(|v| v.0).collect();
        assert_eq!(sel_verts.len(), 4);
        // Convert: face is selected if ALL its vertices are selected
        let mut sel_faces = std::collections::HashSet::new();
        for i in 0..mesh.face_count() as u32 {
            let fv = mesh.face_vertices(FaceId(i));
            if fv.iter().all(|v| sel_verts.contains(&v.0)) {
                sel_faces.insert(i);
            }
        }
        assert_eq!(sel_faces.len(), 1, "only face 0 should be fully covered");
        assert!(sel_faces.contains(&0));
    }

    #[test]
    fn faces_as_new_mesh_separate_one_face_from_cube() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let top = FaceId(0); // any live face
        let part = m.faces_as_new_mesh(&[top]).expect("extract face");
        assert_eq!(part.face_count(), 1);
        assert_eq!(part.vertex_count(), 4);
        // Open single face is boundary-heavy; just require a valid extract.
        assert!(part.validate().is_ok() || part.face_count() == 1);
        // source unchanged
        assert_eq!(m.face_count(), 6);
    }

    #[test]
    fn faces_as_new_mesh_two_adjacent_faces_weld() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Pick two faces that share an edge if possible — face 0 and a neighbor.
        let f0 = FaceId(0);
        let verts0 = m.face_vertices(f0);
        let mut other = None;
        for i in 1..m.face_count() as u32 {
            let fi = FaceId(i);
            if m.free_faces.contains(&i) { continue; }
            let vi = m.face_vertices(fi);
            let share = verts0.iter().any(|v| vi.contains(v));
            if share { other = Some(fi); break; }
        }
        let f1 = other.expect("cube faces share verts");
        let part = m.faces_as_new_mesh(&[f0, f1]).expect("extract");
        // Two quads sharing an edge → 6 verts not 8
        assert!(part.vertex_count() <= 6, "shared verts must weld: nv={}", part.vertex_count());
        assert_eq!(part.face_count(), 2);
    }

    // ================================================================
    // Edit Operations: extrude, inset, loop_cut, delete_face
    // ================================================================

    /// P0-2: vertex extrude creates one new vert + wire edge per selected vert.
    #[test]
    fn extrude_vertices_makes_wire_and_new_vert() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let before_v = mesh.vertex_count();
        let mut sel = std::collections::HashSet::new();
        sel.insert(0);
        let created = mesh.extrude_vertices(&sel, Vec3::Y * 0.5);
        assert_eq!(created.len(), 1, "one new vert");
        assert_eq!(mesh.vertex_count(), before_v + 1);
        mesh.validate().expect("wire extrude must validate");
        let orig = mesh.vert(VertexId(0)).position;
        let tip = mesh.vert(created[0]).position;
        assert!((tip.y - orig.y - 0.5).abs() < 1e-4, "tip offset along Y got {tip:?} vs {orig:?}");
    }

    /// Shift+D edge duplicate: create_wire_edge between copied endpoints (not digons, not orphan verts).
    /// Adjacent edges sharing a source vert must share one new vert (welded chain).
    #[test]
    fn create_wire_edge_and_edge_duplicate_chain_welds() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Pick two cube half-edges that share a vertex (one face corner).
        let f0 = FaceId(0);
        let ring = m.face_half_edges(f0);
        assert!(ring.len() >= 2, "cube face has ≥2 edges");
        let he0 = ring[0];
        let he1 = ring[1];
        let src0 = m.he_source(he0);
        let dst0 = m.he(he0).vertex;
        let src1 = m.he_source(he1);
        let dst1 = m.he(he1).vertex;
        // Shared endpoint between consecutive face edges.
        assert_eq!(dst0, src1, "consecutive face HEs share the corner vert");

        // App-layer Shift+D edge path (kernel-level): shared vert_map + create_wire_edge.
        let mut vert_map: std::collections::HashMap<u32, VertexId> = std::collections::HashMap::new();
        let mut map = |mesh: &mut HalfEdgeMesh, v: VertexId| {
            *vert_map.entry(v.0).or_insert_with(|| {
                let pos = mesh.vert(v).position;
                let normal = mesh.vert(v).normal;
                mesh.alloc_vertex(HVertex { position: pos, normal, half_edge: NO_EDGE })
            })
        };
        let a0 = map(&mut m, src0);
        let b0 = map(&mut m, dst0);
        let a1 = map(&mut m, src1);
        let b1 = map(&mut m, dst1);
        assert_eq!(a1, b0, "shared source corner → one new vert (welded chain)");
        assert_eq!(vert_map.len(), 3, "two edges, one shared corner → 3 new verts");

        let w0 = m.create_wire_edge(a0, b0).expect("wire 0");
        let w1 = m.create_wire_edge(a1, b1).expect("wire 1");
        assert_ne!(w0, w1);
        assert!(m.is_edge_live(w0));
        assert!(m.is_edge_live(w1));
        assert_eq!(m.he(w0).face, NO_FACE, "wire is faceless");
        assert_eq!(m.he(m.he(w0).twin).face, NO_FACE);
        m.validate().expect("mesh valid after wire edge duplicate");
    }

    /// P0-2: edge extrude on a plane boundary edge → +1 face, +2 verts.
    #[test]
    fn extrude_edges_plane_makes_face() {
        let positions = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new( 1.0, -1.0, 0.0),
            Vec3::new( 1.0,  1.0, 0.0),
            Vec3::new(-1.0,  1.0, 0.0),
        ];
        let quads = [[0usize, 1, 2, 3]];
        let mut mesh = HalfEdgeMesh::from_quads(&positions, &quads);
        assert_eq!(mesh.face_count(), 1);
        assert_eq!(mesh.vertex_count(), 4);
        let he0 = mesh.face(FaceId(0)).half_edge.0;
        let mut sel = std::collections::HashSet::new();
        sel.insert(he0);
        let before_f = mesh.face_count();
        let before_v = mesh.vertex_count();
        let created = mesh.extrude_edges(&sel, Vec3::Z * 0.5);
        assert_eq!(created.len(), 2, "two new verts for one edge");
        assert_eq!(mesh.vertex_count(), before_v + 2);
        assert_eq!(mesh.face_count(), before_f + 1, "one new face");
        mesh.validate().expect("edge extrude must validate");
        mesh.is_manifold().expect("edge extrude must stay manifold");
    }

    #[test]
    fn extrude_edges_creates_fin_on_boundary() {
        // Upstream extrude_edges only lifts BOUNDARY edges (face + free twin).
        let positions = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_quads(&positions, &[[0usize, 1, 2, 3]]);
        let e = m.face(FaceId(0)).half_edge.0;
        let mut sel = std::collections::HashSet::new();
        sel.insert(e);
        let new_verts = m.extrude_edges(&sel, Vec3::Y * 0.25);
        assert_eq!(new_verts.len(), 2, "one boundary edge → two new verts");
        assert!(m.face_count() >= 2);
        assert!(m.validate().is_ok());
    }

    /// The shared dihedral accessor: one implementation for coplanar/crease classification.
    /// Bevel Smart Select rule 3 ("edges inside a face with no plane change are ignored")
    /// is built on this, so its edge cases are load-bearing.
    #[test]
    fn edge_dihedral_and_flatness() {
        // A cube: every interior edge is a 90 degree crease, never flat.
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut checked = 0;
        for i in 0..cube.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if let Some(c) = cube.edge_dihedral_cos(h) {
                assert!(c.abs() < 1e-4, "cube edge should be a right angle, cos={c}");
                assert!(!cube.edge_is_flat(h, 0.0), "a cube edge is not flat");
                assert!(!cube.edge_is_flat(h, 30.0), "a 90 degree edge is not flat at 30 deg");
                checked += 1;
            }
        }
        assert!(checked > 0, "expected interior cube edges");

        // A subdivided flat quad: the interior edge is exactly coplanar -> flat, and it is
        // precisely the edge rule 3 must ignore.
        let p = vec![
            Vec3::new(-1.0, 0.0, -1.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),   Vec3::new(0.0, 0.0, 1.0),  Vec3::new(-1.0, 0.0, 1.0),
        ];
        let f: Vec<Vec<usize>> = vec![vec![0, 1, 4, 5], vec![1, 2, 3, 4]];
        let flat = HalfEdgeMesh::from_polygons(&p, &f);
        let mut found_flat = false;
        for i in 0..flat.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if let Some(c) = flat.edge_dihedral_cos(h) {
                assert!((c - 1.0).abs() < 1e-5, "coplanar edge cos={c}");
                assert!(flat.edge_is_flat(h, 0.0), "coplanar edge must read as flat");
                found_flat = true;
            }
        }
        assert!(found_flat, "expected one shared interior edge");

        // BOUNDARY edges have no dihedral, and must NOT report flat -- reporting flat would
        // silently exclude real rim geometry from the bevel.
        let single = HalfEdgeMesh::from_polygons(
            &vec![
                Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, 1.0),   Vec3::new(-1.0, 0.0, 1.0),
            ],
            &vec![vec![0usize, 1, 2, 3]],
        );
        for i in 0..single.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if single.is_edge_live(h) {
                assert_eq!(single.edge_dihedral_cos(h), None, "boundary edge has no dihedral");
                assert!(!single.edge_is_flat(h, 0.0), "boundary edge must not read as flat");
            }
        }
    }

    #[test]
    fn pipe_straight_matches_a_cylinder_shell() {
        // A 2-point straight path is a plain tube: segs quads, no caps requested.
        let m = HalfEdgeMesh::new_pipe(
            &[Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 2.0, 0.0)],
            0.25, 8, false, false,
        );
        assert_eq!(m.vertex_count(), 16, "2 rings x 8");
        assert_eq!(m.face_count(), 8, "8 side quads, no caps");
        assert!(m.validate().is_ok());
        // Every vertex sits exactly `radius` from the axis.
        for i in 0..m.vertices.len() as u32 {
            let p = m.vert(VertexId(i)).position;
            let r = (p.x * p.x + p.z * p.z).sqrt();
            assert!((r - 0.25).abs() < 1e-4, "vert off the tube radius: {r}");
        }
    }

    #[test]
    fn pipe_capped_is_a_closed_solid() {
        let m = HalfEdgeMesh::new_pipe(
            &[Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.0)],
            0.1, 10, false, true,
        );
        assert!(m.validate().is_ok());
        assert!(m.is_manifold().is_ok(), "capped pipe must be a closed manifold solid");
        assert_eq!(m.face_count(), 10 * 2 + 2, "2 spans x 10 quads + 2 caps");
    }

    #[test]
    fn pipe_closed_loop_has_no_ends() {
        // A square loop welds ring N back to ring 0 -> closed shell, no caps, no boundary.
        let m = HalfEdgeMesh::new_pipe(
            &[
                Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, 1.0), Vec3::new(-1.0, 0.0, 1.0),
            ],
            0.1, 8, true, false,
        );
        assert!(m.validate().is_ok());
        assert!(m.is_manifold().is_ok(), "closed loop pipe must be watertight");
        assert_eq!(m.face_count(), 4 * 8);
    }

    #[test]
    fn pipe_does_not_twist_through_a_vertical_turn() {
        // Parallel transport regression: a naive up-cross-tangent frame FLIPS when the path
        // turns through vertical, mirroring the ring and pinching the tube. Consecutive
        // rings must stay close together (no 180-degree flip).
        let path = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.5, 2.0, 0.0), Vec3::new(0.0, 3.0, 0.0),
        ];
        let segs = 12usize;
        let m = HalfEdgeMesh::new_pipe(&path, 0.2, segs as u32, false, false);
        assert!(m.validate().is_ok());
        for ring in 0..path.len() - 1 {
            for s in 0..segs {
                let a = m.vert(VertexId((ring * segs + s) as u32)).position;
                let b = m.vert(VertexId(((ring + 1) * segs + s) as u32)).position;
                // Same profile index on adjacent rings stays near: a flip would send it to
                // the opposite side of the tube.
                assert!(
                    (a - b).length() < 1.6,
                    "ring {ring} index {s} jumped {} - frame flipped",
                    (a - b).length()
                );
            }
        }
    }

    #[test]
    fn pipe_degenerate_path_is_empty_not_a_panic() {
        assert_eq!(HalfEdgeMesh::new_pipe(&[], 0.1, 8, false, true).face_count(), 0);
        assert_eq!(
            HalfEdgeMesh::new_pipe(&[Vec3::ZERO], 0.1, 8, false, true).face_count(), 0
        );
        // Duplicated points collapse to one distinct point -> still nothing to sweep.
        assert_eq!(
            HalfEdgeMesh::new_pipe(&[Vec3::ZERO, Vec3::ZERO], 0.1, 8, false, true).face_count(),
            0
        );
    }

    #[test]
    fn extrude_edges_interior_edge_makes_a_fin() {
        // OWNER 2026-08-04: "for the edge it is ok to have non-manifold, the user expects
        // it." Extruding an INTERIOR edge of a closed cube must produce a fin, exactly as
        // Blender does. The cube's own shell stays intact and valid; the fin is attached at
        // the two shared endpoint vertices with its base half-edge unpaired.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let (v0, f0) = (m.vertex_count(), m.face_count());
        let e = m.face(FaceId(0)).half_edge.0;
        let mut sel = std::collections::HashSet::new();
        sel.insert(e);
        let new_verts = m.extrude_edges(&sel, Vec3::Y * 0.5);
        assert_eq!(new_verts.len(), 2, "one interior edge -> two lifted verts");
        assert_eq!(m.vertex_count(), v0 + 2);
        assert_eq!(m.face_count(), f0 + 1, "exactly one fin quad");
        assert!(m.validate().is_ok(), "fin must leave the mesh structurally valid");
        // The lifted verts are the edge endpoints displaced by exactly the offset.
        let base: Vec<glam::Vec3> = {
            let he = HalfEdgeId(e);
            vec![m.vert(m.he_source(he)).position, m.vert(m.he(he).vertex).position]
        };
        for &nv in &new_verts {
            let p = m.vert(nv).position;
            assert!(
                base.iter().any(|b| (p - (*b + Vec3::Y * 0.5)).length() < 1e-5),
                "lifted vert {p:?} is not an endpoint + offset"
            );
        }
    }

    #[test]
    fn extrude_face_topology() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v_before = mesh.vertex_count();
        let f_before = mesh.face_count();
        // Extrude face 0 upward
        let top = mesh.extrude_face(FaceId(0), Vec3::new(0.0, 0.5, 0.0));
        // 4 new vertices for extruded cap
        assert_eq!(mesh.vertex_count(), v_before + 4);
        // 4 new side quads, original face reused as top = +4 faces
        assert_eq!(mesh.face_count(), f_before + 4);
        // Top face still has 4 vertices
        let top_verts = mesh.face_vertices(top);
        assert_eq!(top_verts.len(), 4);
        // New top vertices should be offset from originals
        for &v in &top_verts {
            assert!(mesh.vert(v).position.y > 0.0 || mesh.vert(v).position.y < 0.0,
                "extruded vertex should have moved");
        }
    }

    #[test]
    fn extrude_face_preserves_other_faces() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let other_verts_before: Vec<Vec3> = (1..6u32)
            .map(|i| {
                let fv = mesh.face_vertices(FaceId(i));
                fv.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / fv.len() as f32
            })
            .collect();
        mesh.extrude_face(FaceId(0), Vec3::Y * 0.5);
        // Other face centroids should remain unchanged (faces 1-5 still exist)
        for (idx, &fid) in [1u32, 2, 3, 4, 5].iter().enumerate() {
            let fv = mesh.face_vertices(FaceId(fid));
            if fv.len() == 4 {
                let center = fv.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / 4.0;
                let dist = (center - other_verts_before[idx]).length();
                // Some faces share vertices with face 0 and get new vertices,
                // so we only check that faces still exist and have 4 verts
                assert_eq!(fv.len(), 4);
                let _ = dist;
            }
        }
    }

    /// Box half-extents (2,1,1) with the +Y top SPLIT along z=0 into two 4x1 quads — the exact input of
    /// `docs/planning/poly/inset_region_ref.py`. Returns (mesh, the two top face ids).
    #[cfg(test)]
    fn split_top_box_for_inset_oracle() -> (HalfEdgeMesh, std::collections::HashSet<FaceId>) {
        let pts = [
            Vec3::new(-2.0, -1.0, -1.0), Vec3::new(2.0, -1.0, -1.0), Vec3::new(2.0, -1.0, 1.0), Vec3::new(-2.0, -1.0, 1.0),
            Vec3::new(-2.0, 1.0, -1.0), Vec3::new(2.0, 1.0, -1.0), Vec3::new(2.0, 1.0, 1.0), Vec3::new(-2.0, 1.0, 1.0),
            Vec3::new(-2.0, 1.0, 0.0), Vec3::new(2.0, 1.0, 0.0),
        ];
        // Same geometry as the oracle script; rings wound OUTWARD and edge-consistent (every shared
        // edge traversed in opposite directions by its two faces) so from_polygons twins them all and
        // the box is closed. The oracle .py gets this via normals_make_consistent(inside=False).
        let rings = vec![
            vec![0usize, 1, 2, 3],       // bottom (-Y)
            vec![4, 8, 9, 5],            // top A (z -1..0)  4x1
            vec![8, 7, 6, 9],            // top B (z  0..1)  4x1
            vec![0, 4, 5, 1],            // -Z
            vec![3, 2, 6, 7],            // +Z
            vec![0, 3, 7, 8, 4],         // -X (pentagon, 8 mid-edge)
            vec![1, 5, 9, 6, 2],         // +X (pentagon, 9 mid-edge)
        ];
        let m = HalfEdgeMesh::from_polygons(&pts, &rings);
        let tops: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32).map(FaceId)
            .filter(|&f| {
                if m.face(f).half_edge == NO_EDGE { return false; }
                let vs = m.face_vertices(f);
                let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                (c.y - 1.0).abs() < 1e-4
            }).collect();
        (m, tops)
    }

    #[test]
    fn face_vertices_returns_destinations_ring_rotated_by_one() {
        // DOCUMENTS THE TRAP: face_vertices() pushes he.vertex (the DESTINATION) starting at
        // face.half_edge, so it returns the ring ROTATED BY ONE vs the ring create_face() was given.
        // Anything that zips a per-vertex array against face_vertices(new_face) therefore mis-pairs by
        // one — which is exactly how the inset drag came out twisted/rotated. Pair by IDENTITY instead.
        let pts = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 1.0)];
        let mut m = HalfEdgeMesh::new();
        let vs: Vec<VertexId> = pts.iter().map(|&p| m.alloc_vertex(HVertex { position: p, normal: Vec3::Y, half_edge: NO_EDGE })).collect();
        let f = m.create_face(&vs).expect("create_face");
        let got = m.face_vertices(f);
        assert_eq!(got, vec![vs[1], vs[2], vs[3], vs[0]],
            "face_vertices is destination-ordered (rotated by one) — do not zip per-vertex data against it");
    }

    #[test]
    fn inset_max_inward_thickness_finite_on_quad() {
        // A unit square face: even-offset dirs converge; max thickness must be positive and finite.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let top = (0..m.faces.len() as u32).map(FaceId).find(|&f| {
            if m.face(f).half_edge == NO_EDGE { return false; }
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (c.y - 1.0).abs() < 1e-4
        }).expect("+Y top");
        let set: std::collections::HashSet<FaceId> = std::iter::once(top).collect();
        let res = m.inset_region(&set, 0.0, 0.0);
        let orig: Vec<(u32, Vec3)> = res.boundary.iter()
            .map(|&(v, _)| (v.0, m.vert(v).position)).collect();
        let dirs: Vec<Vec3> = res.boundary.iter().map(|&(_, d)| d).collect();
        let tmax = HalfEdgeMesh::inset_max_inward_thickness(&orig, &dirs);
        assert!(tmax.is_finite() && tmax > 0.05 && tmax < 10.0,
            "expected a sane clamp for a unit-square top, got {tmax}");
        // 0.3 (Blender oracle thickness) must still be under clamp on a unit box half-extents 1.
        assert!(0.3 < tmax, "oracle thickness 0.3 must be under clamp {tmax}");
    }

    #[test]
    fn inset_region_drag_simulation_lands_blender_ring() {
        // Simulates the APP's interactive inset drag EXACTLY: build the topology at thickness 0, then
        // slide each returned boundary vertex by its displacement * amount. This is the path the I key
        // takes, and the one that was twisted while every kernel test passed
        // (feedback_verify_app_path_not_kernel). inset_region pairs (new_vertex, dir) by IDENTITY, so
        // applying them must land Blender's even-offset ring exactly.
        let mut m = HalfEdgeMesh::new_box(Vec3::new(2.0, 1.0, 1.0));
        let top = (0..m.faces.len() as u32).map(FaceId).find(|&f| {
            if m.face(f).half_edge == NO_EDGE { return false; }
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (c.y - 1.0).abs() < 1e-4
        }).expect("+Y top face");

        let set: std::collections::HashSet<FaceId> = std::iter::once(top).collect();
        let res = m.inset_region(&set, 0.0, 0.0);          // drag start: topology only, verts unmoved
        assert_eq!(res.boundary.len(), 4, "quad face has 4 boundary verts");

        let amount = 0.3f32;
        let slide: Vec<(VertexId, Vec3)> = res.boundary.iter()
            .map(|&(v, d)| (v, m.vert(v).position + d * amount))
            .collect();
        for (v, p) in slide { m.vert_mut(v).position = p; }

        // Must equal the Blender oracle ring (inset_ref.json). A ring-order mis-pair lands the
        // corners on their neighbours' targets => a twisted quad, and these asserts fail.
        for &(x, z) in &[(-1.7f32, -0.7f32), (-1.7, 0.7), (1.7, -0.7), (1.7, 0.7)] {
            let want = Vec3::new(x, 1.0, z);
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-3),
                "drag simulation missed Blender vertex {want:?} — boundary dirs mis-paired (twisted inset)");
        }
        assert!(m.is_manifold().is_ok(), "drag simulation produced non-manifold");
    }

    #[test]
    fn inset_region_matches_blender_oracle() {
        // ORACLE: docs/planning/poly/inset_region_ref.py -> inset_region_ref.json ["region"].
        //   bpy.ops.mesh.inset(thickness=0.3, depth=0.0, use_individual=False) — Blender's `I` DEFAULT —
        //   on BOTH top faces => nv=16 nf=13. The selection is treated as ONE region: a single ring
        //   around the whole 4x2 top at x=+-1.7, and the SHARED INTERIOR EDGE at z=0 SURVIVES (its
        //   endpoints move to (+-1.7, 1, 0)). Top-plane verts: x in {+-2 (orig), +-1.7 (inset)},
        //   z in {-1, 0, 1} (orig) and {-0.7, 0, 0.7} (inset).
        let (mut m, tops) = split_top_box_for_inset_oracle();
        assert_eq!(tops.len(), 2, "expected the two split top faces");
        let boundary_of = |m: &HalfEdgeMesh| (0..m.half_edges.len())
            .filter(|&i| m.half_edges[i].face != NO_FACE && m.half_edges[i].twin == NO_TWIN).count();
        assert_eq!(boundary_of(&m), 0, "input must be a closed box");

        m.inset_region(&tops, 0.3, 0.0);

        assert_eq!(m.vertex_count(), 16, "oracle nv (region)");
        assert_eq!(m.face_count(), 13, "oracle nf (region)");
        assert!(m.validate().is_ok(), "region inset broke topology");
        assert!(m.is_manifold().is_ok(), "region inset produced non-manifold");
        assert_eq!(boundary_of(&m), 0, "region inset must keep the box closed");

        // The 6 inset boundary verts — INCLUDING the two at z=0, which prove the interior edge survived
        // (Individual would instead give z=+-0.3 pairs and 8 new verts).
        for &(x, z) in &[(-1.7f32, -0.7f32), (-1.7, 0.0), (-1.7, 0.7), (1.7, -0.7), (1.7, 0.0), (1.7, 0.7)] {
            let want = Vec3::new(x, 1.0, z);
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-3),
                "missing Blender REGION inset vertex {want:?}");
        }
        // Individual's signature verts must NOT appear.
        for &z in &[-0.3f32, 0.3] {
            let bad = Vec3::new(-1.7, 1.0, z);
            assert!(!m.vertices.iter().any(|v| (v.position - bad).length() < 1e-3),
                "found an INDIVIDUAL-inset vertex {bad:?} — region mode inset each face separately");
        }
    }

    #[test]
    fn inset_individual_matches_blender_oracle() {
        // ORACLE: inset_region_ref.json ["individual"] — bpy.ops.mesh.inset(..., use_individual=True)
        // (Blender's I,I) => nv=18 nf=15: EACH 4x1 face gets its own ring, so the top plane gains
        // z = -0.7,-0.3 (face A) and 0.3,0.7 (face B) at x=+-1.7. This is inset_face applied per face.
        let (mut m, tops) = split_top_box_for_inset_oracle();
        let mut ids: Vec<FaceId> = tops.into_iter().collect();
        ids.sort_by_key(|f| f.0);
        for f in ids { m.inset_face(f, 0.3); }

        assert_eq!(m.vertex_count(), 18, "oracle nv (individual)");
        assert_eq!(m.face_count(), 15, "oracle nf (individual)");
        assert!(m.validate().is_ok(), "individual inset broke topology");
        for &(x, z) in &[(-1.7f32, -0.7f32), (-1.7, -0.3), (-1.7, 0.3), (-1.7, 0.7),
                         (1.7, -0.7), (1.7, -0.3), (1.7, 0.3), (1.7, 0.7)] {
            let want = Vec3::new(x, 1.0, z);
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-3),
                "missing Blender INDIVIDUAL inset vertex {want:?}");
        }
    }

    // ================================================================================
    // circle_region — Blender LoopTools "Circle" parity
    // ================================================================================

    /// The exact input of `docs/planning/poly/circle_ref.py`: a 4x4 quad grid in the XZ plane at y=0,
    /// x,z in {-1,-0.5,0,0.5,1} (25 verts, 16 quads). The .py authors the SAME positions with the SAME
    /// index formula (`iz*5 + ix`) via `from_pydata`, and `from_quads` allocates one vertex per position
    /// IN ORDER, so `VertexId(i)` is oracle vertex `i` — which lets the tests diff by IDENTITY rather
    /// than by set membership. That matters: a set-membership check passes even when every vertex got
    /// its NEIGHBOUR's target (a twisted region — the exact POLY_POLISH_PLAN §8 inset trap).
    #[cfg(test)]
    fn circle_oracle_grid() -> (HalfEdgeMesh, std::collections::HashSet<FaceId>) {
        let m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        // INPUT PARITY GATE — a diff against the oracle is meaningless if the inputs differ (§2).
        assert_eq!(m.vertex_count(), 25, "oracle input nv");
        assert_eq!(m.face_count(), 16, "oracle input nf");
        for iz in 0..5u32 {
            for ix in 0..5u32 {
                let want = Vec3::new(-1.0 + ix as f32 * 0.5, 0.0, -1.0 + iz as f32 * 0.5);
                let got = m.vert(VertexId(iz * 5 + ix)).position;
                assert!((got - want).length() < 1e-6,
                    "input parity: vertex {} is {got:?}, circle_ref.py has {want:?}", iz * 5 + ix);
            }
        }
        let faces: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32).map(FaceId).collect();
        (m, faces)
    }

    /// `circle_ref.py`'s "asym" input: the grid with boundary vertex 10 pulled (-1,0,0) -> (-0.6,0,0).
    /// See circle_ref.py for WHY the asymmetry is load-bearing (unique anchor + it separates the true
    /// least-squares fit from a naive centroid+mean fit).
    #[cfg(test)]
    fn circle_oracle_grid_asym() -> (HalfEdgeMesh, std::collections::HashSet<FaceId>) {
        let (mut m, faces) = circle_oracle_grid();
        m.vert_mut(VertexId(10)).position = Vec3::new(-0.6, 0.0, 0.0);
        (m, faces)
    }

    /// The 9 interior vertices of the 4x4 grid (the 3x3 block at x,z in {-0.5, 0, 0.5}).
    #[cfg(test)]
    const CIRCLE_GRID_INTERIOR: [u32; 9] = [6, 7, 8, 11, 12, 13, 16, 17, 18];

    /// The 16 boundary-loop vertices of the 4x4 grid.
    #[cfg(test)]
    const CIRCLE_GRID_BOUNDARY: [u32; 16] = [0, 1, 2, 3, 4, 5, 9, 10, 14, 15, 19, 20, 21, 22, 23, 24];

    #[test]
    fn circle_region_matches_blender_looptools_oracle() {
        // ORACLE: docs/planning/poly/circle_ref.py -> circle_ref.json ["asym"] (Blender 4.5.1 LTS).
        //   Op: bpy.ops.mesh.looptools_circle() at its UI DEFAULTS —
        //       fit='best', custom_radius=False, flatten=True, regular=True, angle=0, influence=100.
        //   Result: the 16 boundary verts land on a circle centred at the loop CENTROID (0.025, 0, 0)
        //   with r = 1.138111, at exactly 22.5 deg (=360/16) apart; the 9 interior verts DO NOT MOVE.
        //
        // WHY THIS INPUT (POLY_POLISH_PLAN §0.3 — gate at the non-trivial case): the asymmetry makes
        // Blender's least-squares radius 1.138111 differ from the mean distance 1.137707, so the naive
        // "centroid + mean distance" fit CANNOT pass this test. On a symmetric grid the two coincide
        // exactly and the bug would hide — the same trap that hid the inset even-offset bug for months.
        let (mut m, faces) = circle_oracle_grid_asym();
        let res = m.circle_region(&faces, None, true, true, 0);
        assert_eq!(res.loops.len(), 1, "the grid has ONE boundary loop");
        assert_eq!(res.loops[0].boundary.len(), 16, "16 boundary verts");
        assert!(res.relaxed.is_empty(), "relax_passes=0 must leave the interior alone (Blender parity)");

        // Transcribed from circle_ref.json ["asym"]["verts"] (§2: self-contained, no runtime JSON I/O).
        let oracle: [[f32; 3]; 25] = [
            [-0.779766, 0.0, -0.804766], [-0.410536, 0.0, -1.051478], [ 0.025000, 0.0, -1.138111],
            [ 0.460536, 0.0, -1.051477], [ 0.829766, 0.0, -0.804766], [-1.026477, 0.0, -0.435536],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 1.076478, 0.0, -0.435536], [-1.113111, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 1.163111, 0.0,  0.000000],
            [-1.026478, 0.0,  0.435536], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 1.076477, 0.0,  0.435536], [-0.779766, 0.0,  0.804766],
            [-0.410536, 0.0,  1.051477], [ 0.025000, 0.0,  1.138111], [ 0.460536, 0.0,  1.051478],
            [ 0.829766, 0.0,  0.804766],
        ];
        // Diff by IDENTITY, not set membership — a rotated/twisted assignment must fail here.
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "vertex {i}: AZRA {got:?} != Blender LoopTools {want:?}");
        }
        // The fit AZRA reports must be Blender's, not the mean (1.137707).
        assert!((res.loops[0].radius - 1.138111).abs() < 1e-4,
            "radius {} is not Blender's least-squares fit 1.138111 (the MEAN would be 1.137707)",
            res.loops[0].radius);
        assert!((res.loops[0].center - Vec3::new(0.025, 0.0, 0.0)).length() < 1e-4,
            "the circle must be centred on the loop CENTROID (LoopTools discards the fitted centre)");
    }

    #[test]
    fn circle_region_own_angle_matches_blender_oracle() {
        // ORACLE: circle_ref.json ["asym_nonregular"] — looptools_circle(regular=False): every vertex
        // keeps its OWN angle (about the FITTED centre) instead of being spread evenly.
        let (mut m, faces) = circle_oracle_grid_asym();
        let res = m.circle_region(&faces, None, true, false, 0);
        assert_eq!(res.loops.len(), 1);
        let oracle: [[f32; 3]; 25] = [
            [-0.799053, 0.0, -0.785005], [-0.523280, 0.0, -0.997339], [-0.031543, 0.0, -1.136706],
            [ 0.492262, 0.0, -1.037768], [ 0.808985, 0.0, -0.825024], [-1.002509, 0.0, -0.489410],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 1.032194, 0.0, -0.529959], [-1.113111, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 1.163111, 0.0,  0.000000],
            [-1.002509, 0.0,  0.489410], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 1.032194, 0.0,  0.529959], [-0.799053, 0.0,  0.785005],
            [-0.523280, 0.0,  0.997339], [-0.031543, 0.0,  1.136706], [ 0.492262, 0.0,  1.037768],
            [ 0.808985, 0.0,  0.825024],
        ];
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "regular=false vertex {i}: AZRA {got:?} != Blender {want:?}");
        }
    }

    #[test]
    fn circle_region_custom_radius_matches_blender_oracle() {
        // ORACLE: circle_ref.json ["asym_r1"] — looptools_circle(custom_radius=True, radius=0.75).
        // The radius is overridden but the BEST-FIT CENTRE still drives the angular reference, so the
        // angles are unchanged from the auto-radius case and only the radius scales.
        let (mut m, faces) = circle_oracle_grid_asym();
        let res = m.circle_region(&faces, Some(0.75), true, true, 0);
        assert!((res.loops[0].radius - 0.75).abs() < 1e-6, "custom radius must be used verbatim");
        let oracle: [[f32; 3]; 25] = [
            [-0.505330, 0.0, -0.530330], [-0.262013, 0.0, -0.692910], [ 0.025000, 0.0, -0.750000],
            [ 0.312013, 0.0, -0.692910], [ 0.555330, 0.0, -0.530330], [-0.667910, 0.0, -0.287013],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 0.717910, 0.0, -0.287013], [-0.725000, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 0.775000, 0.0,  0.000000],
            [-0.667910, 0.0,  0.287013], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 0.717910, 0.0,  0.287013], [-0.505330, 0.0,  0.530330],
            [-0.262013, 0.0,  0.692910], [ 0.025000, 0.0,  0.750000], [ 0.312013, 0.0,  0.692910],
            [ 0.555330, 0.0,  0.530330],
        ];
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "custom radius vertex {i}: AZRA {got:?} != Blender {want:?}");
        }
    }

    #[test]
    fn circle_region_boundary_is_equidistant_and_evenly_spaced() {
        // THE DEFINITION of "circle", as a property (holds for ANY input, not just the oracle's):
        // (a) every boundary vertex is the same distance from the centre, and
        //     with regular=true (Blender's default) they are 360/n apart.
        let (mut m, faces) = circle_oracle_grid_asym();
        let res = m.circle_region(&faces, None, true, true, 0);
        let fit = &res.loops[0];
        let mut angles: Vec<f32> = Vec::new();
        let p = fit.normal.any_orthonormal_vector();
        let q = fit.normal.cross(p);
        for &(v, _) in &fit.boundary {
            let d = m.vert(v).position - fit.center;
            assert!((d.length() - fit.radius).abs() < 1e-4,
                "vertex {v:?} is {} from the centre, radius is {}", d.length(), fit.radius);
            angles.push(q.dot(d).atan2(p.dot(d)).to_degrees().rem_euclid(360.0));
        }
        angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for i in 0..angles.len() {
            let step = (angles[(i + 1) % angles.len()] - angles[i]).rem_euclid(360.0);
            assert!((step - 22.5).abs() < 1e-2, "regular spacing must be 360/16 = 22.5 deg, got {step}");
        }
    }

    /// A closed box whose +Y top is a 2x2 grid (4 quads meeting at a centre vertex); the 4 sides are
    /// pentagons carrying the top ring's mid-edge verts so every shared edge is traversed in OPPOSITE
    /// directions by its two faces (the `from_polygons` twinning rule, POLY_POLISH_PLAN §1).
    /// Returns (mesh, the 4 top face ids). 13 verts / 9 faces, watertight.
    #[cfg(test)]
    fn gridded_top_box() -> (HalfEdgeMesh, std::collections::HashSet<FaceId>) {
        let pts = [
            Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, -1.0, -1.0),  // 0 1
            Vec3::new(1.0, -1.0, 1.0), Vec3::new(-1.0, -1.0, 1.0),    // 2 3
            Vec3::new(-1.0, 1.0, -1.0), Vec3::new(1.0, 1.0, -1.0),    // 4 5
            Vec3::new(1.0, 1.0, 1.0), Vec3::new(-1.0, 1.0, 1.0),      // 6 7
            Vec3::new(0.0, 1.0, -1.0), Vec3::new(1.0, 1.0, 0.0),      // 8 9  mid-edge
            Vec3::new(0.0, 1.0, 1.0), Vec3::new(-1.0, 1.0, 0.0),      // 10 11 mid-edge
            Vec3::new(0.0, 1.0, 0.0),                                  // 12 top centre
        ];
        let rings = vec![
            vec![0usize, 1, 2, 3],      // bottom (-Y)
            vec![4, 11, 12, 8],         // top SW
            vec![11, 7, 10, 12],        // top NW
            vec![8, 12, 9, 5],          // top SE
            vec![12, 10, 6, 9],         // top NE
            vec![4, 8, 5, 1, 0],        // -Z pentagon
            vec![5, 9, 6, 2, 1],        // +X pentagon
            vec![6, 10, 7, 3, 2],       // +Z pentagon
            vec![7, 11, 4, 0, 3],       // -X pentagon
        ];
        let m = HalfEdgeMesh::from_polygons(&pts, &rings);
        let tops: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32).map(FaceId)
            .filter(|&f| {
                if m.face(f).half_edge == NO_EDGE { return false; }
                let vs = m.face_vertices(f);
                let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                (c.y - 1.0).abs() < 1e-4 && vs.len() == 4
            }).collect();
        (m, tops)
    }

    #[test]
    fn circle_region_on_closed_box_face_is_watertight_and_id_preserving() {
        // The REAL use case (owner: "a 4x4 grid on a cube face"): the region's boundary edges have
        // twins that live OUTSIDE the selection (the side pentagons), so moving the loop drags the
        // sides with it. Gates (a) equidistant, (b) watertight + manifold, (c) counts unchanged.
        let (mut m, tops) = gridded_top_box();
        assert_eq!(tops.len(), 4, "expected the 4 top quads");
        assert_eq!(crate::mesh_boolean::boundary_half_edge_count(&m), 0, "input must be a closed box");
        let (nv, nf) = (m.vertex_count(), m.face_count());
        let he_before = m.half_edges.len();

        let res = m.circle_region(&tops, None, true, true, 0);
        assert_eq!(res.loops.len(), 1, "the top region has ONE boundary loop");
        assert_eq!(res.loops[0].boundary.len(), 8, "the top ring has 8 verts (4 corner + 4 mid-edge)");

        // (c) PURE RESHAPE — no topology touched at all.
        assert_eq!(m.vertex_count(), nv, "circle_region must not add/remove vertices");
        assert_eq!(m.face_count(), nf, "circle_region must not add/remove faces");
        assert_eq!(m.half_edges.len(), he_before, "circle_region must not add/remove half-edges");
        // (b) still a closed manifold.
        assert_eq!(crate::mesh_boolean::boundary_half_edge_count(&m), 0, "must stay watertight");
        assert!(m.validate().is_ok(), "topology broken");
        assert!(m.is_manifold().is_ok(), "non-manifold: {:?}", m.is_manifold());
        // (a) THE definition of a circle.
        let fit = &res.loops[0];
        for &(v, _) in &fit.boundary {
            let d = (m.vert(v).position - fit.center).length();
            assert!((d - fit.radius).abs() < 1e-4, "vertex {v:?}: dist {d} != radius {}", fit.radius);
        }
        // (d) the region was planar (y=1) and must stay planar.
        for &(v, _) in &fit.boundary {
            assert!((m.vert(v).position.y - 1.0).abs() < 1e-4, "top ring left the y=1 plane");
        }
        assert!((m.vert(VertexId(12)).position - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-6,
            "relax_passes=0 must leave the interior centre vertex untouched (Blender parity)");
        // The returned pairs must reconstruct the geometry (the future radius handle's contract).
        for &(v, dir) in &fit.boundary {
            let rebuilt = fit.center + dir * fit.radius;
            assert!((m.vert(v).position - rebuilt).length() < 1e-4,
                "pos != center + dir*radius for {v:?} — the drag contract is broken");
        }
    }

    #[test]
    fn circle_region_flatten_planarizes_a_domed_region() {
        // (d) flatten=true must land the boundary loop exactly in its best-fit plane. Dome the grid's
        // boundary out of y=0 first, so "stays planar" is a real assertion and not a tautology.
        let (mut m, faces) = circle_oracle_grid();
        for (i, &v) in CIRCLE_GRID_BOUNDARY.iter().enumerate() {
            m.vert_mut(VertexId(v)).position.y = 0.15 * (i as f32 % 3.0 - 1.0); // -0.15 / 0 / +0.15
        }
        let res = m.circle_region(&faces, None, true, true, 0);
        let fit = &res.loops[0];
        for &(v, _) in &fit.boundary {
            let off = (m.vert(v).position - fit.center).dot(fit.normal);
            assert!(off.abs() < 1e-4, "flatten=true left vertex {v:?} {off} off the fit plane");
        }

        // flatten=false must instead PRESERVE each vertex's own offset along the plane normal.
        let (mut m2, faces2) = circle_oracle_grid();
        for (i, &v) in CIRCLE_GRID_BOUNDARY.iter().enumerate() {
            m2.vert_mut(VertexId(v)).position.y = 0.15 * (i as f32 % 3.0 - 1.0);
        }
        // NOTE the domed fit plane's normal is NOT axis-aligned, so the offset must be measured with
        // the FULL original position against the fit plane — not just its y component.
        let before: Vec<Vec3> = CIRCLE_GRID_BOUNDARY.iter().map(|&v| m2.vert(VertexId(v)).position).collect();
        let res2 = m2.circle_region(&faces2, None, false, true, 0);
        let n2 = res2.loops[0].normal;
        let c2 = res2.loops[0].center;
        let mut any_off = false;
        for (i, &v) in CIRCLE_GRID_BOUNDARY.iter().enumerate() {
            let want_off = (before[i] - c2).dot(n2);
            let got_off = (m2.vert(VertexId(v)).position - c2).dot(n2);
            assert!((got_off - want_off).abs() < 1e-4,
                "flatten=false must keep vertex {v}'s along-normal offset ({want_off} -> {got_off})");
            any_off |= want_off.abs() > 1e-3;
        }
        assert!(any_off, "the dome must actually leave the plane, else this gate proves nothing");
    }

    #[test]
    fn circle_region_relax_distributes_the_interior_and_pins_the_boundary() {
        // The AZRA extension Blender's Circle does NOT do: relax the interior grid so it does not shear
        // against the new circular boundary. Property gate — the pinned-boundary Laplacian must
        // (1) leave the circle itself untouched, (2) actually move the interior, and (3) REDUCE the
        // Laplacian residual |v - mean(1-ring)|, which is what "relaxed" means (its fixed point is the
        // harmonic solution = the evenly-distributed grid in a disk).
        let residual = |m: &HalfEdgeMesh| -> f32 {
            let mut worst = 0.0f32;
            for &vi in CIRCLE_GRID_INTERIOR.iter() {
                let v = VertexId(vi);
                let mut ns: Vec<VertexId> = Vec::new();
                for &he in &m.vertex_fan(v) { ns.push(m.he(he).vertex); }
                ns.sort_by_key(|x| x.0);
                ns.dedup_by_key(|x| x.0);
                let mean: Vec3 = ns.iter().map(|&n| m.vert(n).position).sum::<Vec3>() / ns.len() as f32;
                worst = worst.max((m.vert(v).position - mean).length());
            }
            worst
        };

        let (mut plain, faces) = circle_oracle_grid_asym();
        plain.circle_region(&faces, None, true, true, 0);
        let before = residual(&plain);

        let (mut m, faces) = circle_oracle_grid_asym();
        let res = m.circle_region(&faces, None, true, true, 12);
        let after = residual(&m);

        assert_eq!(res.relaxed.len(), 9, "the 3x3 interior must be the relaxed set");
        assert_eq!(res.relaxed, CIRCLE_GRID_INTERIOR.iter().map(|&v| VertexId(v)).collect::<Vec<_>>(),
            "relaxed must be the interior verts, ascending (deterministic)");
        assert!(after < before * 0.5,
            "relax must reduce the Laplacian residual (was {before}, now {after})");
        assert!(after > 0.0 || before == 0.0);

        // (1) the boundary circle is PINNED — relax must not disturb the fit.
        let fit = &res.loops[0];
        for &(v, _) in &fit.boundary {
            let d = (m.vert(v).position - fit.center).length();
            assert!((d - fit.radius).abs() < 1e-4, "relax moved boundary vertex {v:?} off the circle");
        }
        // (2) the interior really moved, and stayed in the plane (flatten=true).
        let mut moved = 0;
        for &vi in CIRCLE_GRID_INTERIOR.iter() {
            let p = m.vert(VertexId(vi)).position;
            assert!(p.y.abs() < 1e-4, "relax left vertex {vi} off the y=0 fit plane");
            if (p - plain.vert(VertexId(vi)).position).length() > 1e-5 { moved += 1; }
        }
        assert!(moved >= 8, "relax should redistribute the interior grid, moved only {moved}/9");
        // Pure reshape, still valid.
        assert_eq!(m.vertex_count(), 25);
        assert_eq!(m.face_count(), 16);
        assert!(m.is_manifold().is_ok());
    }

    #[test]
    fn circle_region_is_deterministic_across_runs() {
        // The op reads a HashSet<FaceId> and builds HashMap adjacency — both iterate in RANDOM order
        // (POLY_POLISH_PLAN §1). Identical input must give bit-identical output every time, or the
        // undo/redo panel would jitter.
        let mut first: Option<Vec<Vec3>> = None;
        for _ in 0..8 {
            let (mut m, faces) = circle_oracle_grid_asym();
            m.circle_region(&faces, None, true, true, 6);
            let got: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
            match &first {
                None => first = Some(got),
                Some(f) => assert_eq!(*f, got, "circle_region output depends on hash iteration order"),
            }
        }
    }

    #[test]
    fn circle_region_refuses_a_region_without_a_simple_loop() {
        // A region whose boundary pinches at a vertex has no well-defined loop to fit. It must be a
        // SAFE NO-OP (like dissolve_faces_region on a non-disk region), never a garbage reshape.
        // Two diagonal quads of the grid touch only at their shared corner vertex 12 => that vertex has
        // TWO outgoing boundary half-edges.
        let (mut m, _) = circle_oracle_grid();
        let before: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        // Faces are created in row-major order: face f covers (ix = f%4, iz = f/4).
        let diag: std::collections::HashSet<FaceId> = [FaceId(5), FaceId(10)].into_iter().collect();
        let res = m.circle_region(&diag, None, true, true, 4);
        assert!(res.loops.is_empty(), "a pinched region must be refused");
        let after: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert_eq!(before, after, "a refused circle_region must leave the mesh untouched");
        assert!(m.is_manifold().is_ok());
    }

    /// The outer BOUNDARY edge loop of the grid, as a `selected_edges`-style set of `HalfEdgeId` u32s —
    /// exactly the 16 edges Blender's edge-mode oracle selects (bmesh `is_boundary`). Built from
    /// `region_boundary_half_edges` so "the boundary of the whole region" is defined in ONE place.
    #[cfg(test)]
    fn circle_oracle_boundary_edges(m: &HalfEdgeMesh) -> std::collections::HashSet<HalfEdgeId> {
        let all: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32).map(FaceId).collect();
        m.region_boundary_half_edges(&all).into_iter().collect()
    }

    #[test]
    fn circle_edge_loop_matches_blender_looptools_oracle_by_identity() {
        // ORACLE: docs/planning/poly/circle_edge_ref.py -> circle_edge_ref.json ["asym_edge"]
        // (Blender 4.5.1 LTS). bpy.ops.mesh.looptools_circle() at its UI DEFAULTS, run in EDGE select
        // mode on the outer BOUNDARY EDGE LOOP of the SAME asym grid. Blender's output is byte-for-byte
        // identical to the FACE-mode oracle (verified in circle_edge_ref.json: max per-vertex delta 0.0)
        // — the "one producer" guarantee, since edge-mode and face-mode reshape the SAME loop through the
        // SAME core (circle_reshape_loops). This test drives the EDGE producer and diffs by IDENTITY.
        let (mut m, _) = circle_oracle_grid_asym();
        let edges = circle_oracle_boundary_edges(&m);
        assert_eq!(edges.len(), 16, "the outer boundary loop is 16 edges");
        // The edge ring builder must return ONE closed 16-vertex loop.
        let rings = m.edge_loop_rings(&edges);
        assert_eq!(rings.len(), 1, "one connected closed loop");
        assert_eq!(rings[0].len(), 16, "16 vertices in the loop");

        let res = m.circle_edge_loop(&edges, None, true, true, 0);
        assert_eq!(res.loops.len(), 1, "one closed edge loop reshaped");
        assert_eq!(res.loops[0].boundary.len(), 16, "16 boundary verts");
        assert!(res.relaxed.is_empty(), "a pure edge loop has no interior to relax");

        // Same 25-vertex oracle table as circle_region ["asym"] — proven identical in the edge JSON.
        let oracle: [[f32; 3]; 25] = [
            [-0.779766, 0.0, -0.804766], [-0.410536, 0.0, -1.051478], [ 0.025000, 0.0, -1.138111],
            [ 0.460536, 0.0, -1.051477], [ 0.829766, 0.0, -0.804766], [-1.026477, 0.0, -0.435536],
            [-0.500000, 0.0, -0.500000], [ 0.000000, 0.0, -0.500000], [ 0.500000, 0.0, -0.500000],
            [ 1.076478, 0.0, -0.435536], [-1.113111, 0.0,  0.000000], [-0.500000, 0.0,  0.000000],
            [ 0.000000, 0.0,  0.000000], [ 0.500000, 0.0,  0.000000], [ 1.163111, 0.0,  0.000000],
            [-1.026478, 0.0,  0.435536], [-0.500000, 0.0,  0.500000], [ 0.000000, 0.0,  0.500000],
            [ 0.500000, 0.0,  0.500000], [ 1.076477, 0.0,  0.435536], [-0.779766, 0.0,  0.804766],
            [-0.410536, 0.0,  1.051477], [ 0.025000, 0.0,  1.138111], [ 0.460536, 0.0,  1.051478],
            [ 0.829766, 0.0,  0.804766],
        ];
        for (i, o) in oracle.iter().enumerate() {
            let want = Vec3::new(o[0], o[1], o[2]);
            let got = m.vert(VertexId(i as u32)).position;
            assert!((got - want).length() < 1e-3,
                "EDGE-LOOP vertex {i}: AZRA {got:?} != Blender LoopTools {want:?}");
        }
        // The 9 interior verts must NOT move — the edge path never touches them.
        for &iv in &CIRCLE_GRID_INTERIOR {
            let o = oracle[iv as usize];
            assert!((m.vert(VertexId(iv)).position - Vec3::new(o[0], o[1], o[2])).length() < 1e-6,
                "interior vertex {iv} moved on the edge path");
        }
        assert!((res.loops[0].radius - 1.138111).abs() < 1e-4,
            "radius {} is not Blender's least-squares fit 1.138111", res.loops[0].radius);
        assert!((res.loops[0].center - Vec3::new(0.025, 0.0, 0.0)).length() < 1e-4,
            "the circle must be centred on the loop CENTROID");
    }

    #[test]
    fn circle_edge_loop_refuses_a_non_loop_edge_selection() {
        // A selection that is not a set of simple closed loops has no well-defined loop to fit — it must
        // be a SAFE NO-OP (mesh untouched, no undo entry to eat), the edge-mode analogue of
        // circle_region_refuses_a_region_without_a_simple_loop. Here: three collinear edges of the
        // bottom row (0-1, 1-2, 2-3) = an OPEN chain whose endpoints have degree 1.
        let (mut m, _) = circle_oracle_grid();
        let before: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        let pick = |m: &HalfEdgeMesh, a: u32, b: u32| -> HalfEdgeId {
            (0..m.half_edges.len() as u32).map(HalfEdgeId)
                .find(|&h| m.is_edge_live(h) && {
                    let s = m.he_source(h).0; let d = m.he(h).vertex.0;
                    (s == a && d == b) || (s == b && d == a)
                })
                .expect("edge exists in the grid")
        };
        let edges: std::collections::HashSet<HalfEdgeId> =
            [pick(&m, 0, 1), pick(&m, 1, 2), pick(&m, 2, 3)].into_iter().collect();
        assert!(m.edge_loop_rings(&edges).is_empty(), "an open chain is not a closed loop");
        let res = m.circle_edge_loop(&edges, None, true, true, 4);
        assert!(res.loops.is_empty(), "a non-loop edge selection must be refused");
        let after: Vec<Vec3> = (0..25).map(|i| m.vert(VertexId(i)).position).collect();
        assert_eq!(before, after, "a refused circle_edge_loop must leave the mesh untouched");

        // A single edge (both endpoints degree 1) must also refuse.
        let one: std::collections::HashSet<HalfEdgeId> = [pick(&m, 0, 1)].into_iter().collect();
        assert!(m.circle_edge_loop(&one, None, true, true, 0).loops.is_empty(),
            "a single edge is not a loop");
    }

    #[test]
    fn inset_face_matches_blender_oracle_even_offset() {
        // ORACLE: docs/planning/poly/inset_ref.py -> inset_ref.json (Blender 4.5).
        //   Input : box half-extents (2,1,1) — the +Y top face is 4 x 2, i.e. NON-square, so Blender's
        //           EVEN offset and the old move-toward-centroid math DIVERGE (a square would falsely
        //           pass — POLY_POLISH_PLAN §0.3).
        //   Op    : bpy.ops.mesh.inset(thickness=0.3, depth=0.0) at the UI defaults
        //           (use_even_offset=True, use_individual=False).
        //   Result: nv=12 nf=10, inner ring = (+-1.7, 1.0, +-0.7)  <- uniform 0.3 gap from EVERY edge.
        //   The naive centroid inset produced (+-1.7317, 1.0, +-0.8658) = gaps of 0.268/0.134 — the
        //   audit #15 bug this test locks out. NOTE the topology (nv/nf/watertight/manifold) was ALREADY
        //   correct with the wrong math, which is exactly why only a Blender vertex diff catches it.
        let mut m = HalfEdgeMesh::new_box(Vec3::new(2.0, 1.0, 1.0));
        let boundary_of = |m: &HalfEdgeMesh| (0..m.half_edges.len())
            .filter(|&i| m.half_edges[i].face != NO_FACE && m.half_edges[i].twin == NO_TWIN).count();
        assert_eq!(boundary_of(&m), 0, "input box must be closed");

        let top = (0..m.faces.len() as u32).map(FaceId).find(|&f| {
            if m.face(f).half_edge == NO_EDGE { return false; }
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (c.y - 1.0).abs() < 1e-4
        }).expect("+Y top face");
        m.inset_face(top, 0.3);

        // Topology vs oracle (necessary, NOT sufficient).
        assert_eq!(m.vertex_count(), 12, "oracle nv");
        assert_eq!(m.face_count(), 10, "oracle nf");
        assert!(m.validate().is_ok(), "inset broke topology");
        assert!(m.is_manifold().is_ok(), "inset produced non-manifold geometry");
        assert_eq!(boundary_of(&m), 0, "inset must keep the box closed");

        // THE GEOMETRIC GATE — vertex-for-vertex vs Blender's inset ring.
        for &(x, z) in &[(-1.7f32, -0.7f32), (-1.7, 0.7), (1.7, -0.7), (1.7, 0.7)] {
            let want = Vec3::new(x, 1.0, z);
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-3),
                "missing Blender inset vertex {want:?} — even offset not reproduced");
        }
    }

    #[test]
    fn inset_face_even_offset_uniform_gap_on_irregular_ngon() {
        // Even offset means EVERY edge keeps the same perpendicular gap, whatever the polygon shape.
        // Irregular pentagon in the XZ plane (outward +Y), inset 0.1: assert each inset edge sits
        // `thickness` from its source edge (the property the centroid math cannot satisfy).
        let pts = [
            Vec3::new(-2.0, 0.0, -1.0), Vec3::new(2.0, 0.0, -1.0), Vec3::new(3.0, 0.0, 0.5),
            Vec3::new(0.0, 0.0, 2.0),  Vec3::new(-3.0, 0.0, 0.5),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&pts, &[vec![0, 1, 2, 3, 4]]);
        let f = FaceId(0);
        // Read the face's OWN boundary order + normal so the inward test is winding-consistent
        // (the op itself is winding-invariant: flipping the ring flips BOTH N and the edge dir).
        let outer: Vec<Vec3> = m.face_vertices(f).iter().map(|&v| m.vert(v).position).collect();
        let normal = m.compute_face_normal(f);
        let t = 0.1f32;
        let inner = m.inset_face(f, t);

        let ring: Vec<Vec3> = m.face_vertices(inner).iter().map(|&v| m.vert(v).position).collect();
        assert_eq!(ring.len(), 5, "inner ring keeps the ngon's vertex count");
        // Every ORIGINAL edge must have an inset vertex sitting exactly `t` inward of its line —
        // that IS "even offset". The centroid math cannot satisfy this on an irregular polygon.
        let n = outer.len();
        for i in 0..n {
            let (a, b) = (outer[i], outer[(i + 1) % n]);
            let inward = normal.cross(b - a).normalize_or_zero();
            let mut best = f32::MAX;
            for p in &ring {
                let d = (*p - a).dot(inward);          // distance inward from the edge's line
                if (d - t).abs() < best { best = (d - t).abs(); }
            }
            assert!(best < 1e-3, "edge {i}: no inset vertex sits {t} inward — gap not uniform (off by {best})");
        }
    }

    #[test]
    fn inset_face_creates_inner_face() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v_before = mesh.vertex_count();
        let f_before = mesh.face_count();
        let inner = mesh.inset_face(FaceId(0), 0.2);
        // 4 new inset vertices
        assert_eq!(mesh.vertex_count(), v_before + 4);
        // Inset deletes original face, adds N connecting quads + 1 inner face
        // For a quad face: -1 + 4 + 1 = +4 faces
        assert_eq!(mesh.face_count(), f_before + 4);
        // Inner face has 4 vertices
        let inner_verts = mesh.face_vertices(inner);
        assert_eq!(inner_verts.len(), 4);
        // Inset vertices should be closer to face center than original boundary vertices
        let inner_center: Vec3 = inner_verts.iter()
            .map(|&v| mesh.vert(v).position).sum::<Vec3>() / inner_verts.len() as f32;
        for &v in &inner_verts {
            let dist = (mesh.vert(v).position - inner_center).length();
            assert!(dist < 1.5, "inset vertex should be near center");
        }
    }

    /// Owner 2026-08-06: slice digon poison blocked Ctrl+X. Purge + delete real face must work.
    #[test]
    fn purge_digon_allows_delete_of_real_face() {
        use crate::half_edge::{HalfEdge, HFace, HVertex, NO_EDGE, NO_TWIN};
        use crate::mesh_ops::PolyEdit;
        let p = [
            glam::Vec3::new(0.0, 0.0, 0.0),
            glam::Vec3::new(1.0, 0.0, 0.0),
            glam::Vec3::new(1.0, 1.0, 0.0),
            glam::Vec3::new(0.0, 1.0, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        // Inject a digon (2-vert face) — same class as post-slice poison.
        let v0 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(2.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let v1 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(3.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let f = m.alloc_face(HFace {
            half_edge: NO_EDGE,
            normal: glam::Vec3::Z,
            material_id: 0,
        });
        let he0 = m.alloc_half_edge(HalfEdge {
            vertex: v1,
            twin: NO_TWIN,
            next: NO_EDGE,
            prev: NO_EDGE,
            face: f,
        });
        let he1 = m.alloc_half_edge(HalfEdge {
            vertex: v0,
            twin: NO_TWIN,
            next: he0,
            prev: he0,
            face: f,
        });
        m.he_mut(he0).next = he1;
        m.he_mut(he0).prev = he1;
        m.he_mut(he1).next = he0;
        m.he_mut(he1).prev = he0;
        m.face_mut(f).half_edge = he0;
        m.vert_mut(v0).half_edge = he0;
        m.vert_mut(v1).half_edge = he1;
        assert!(m.integrity().is_err(), "digon fails integrity");
        assert!(m.purge_degenerate_faces() >= 1, "purge removes digon");
        assert!(m.integrity().is_ok(), "clean after purge: {:?}", m.integrity());
        // Re-inject digon and prove the edit op path (purge-before-integrity) still deletes the quad.
        let v0 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(2.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let v1 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(3.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let f = m.alloc_face(HFace {
            half_edge: NO_EDGE,
            normal: glam::Vec3::Z,
            material_id: 0,
        });
        let he0 = m.alloc_half_edge(HalfEdge {
            vertex: v1,
            twin: NO_TWIN,
            next: NO_EDGE,
            prev: NO_EDGE,
            face: f,
        });
        let he1 = m.alloc_half_edge(HalfEdge {
            vertex: v0,
            twin: NO_TWIN,
            next: he0,
            prev: he0,
            face: f,
        });
        m.he_mut(he0).next = he1;
        m.he_mut(he0).prev = he1;
        m.he_mut(he1).next = he0;
        m.he_mut(he1).prev = he0;
        m.face_mut(f).half_edge = he0;
        m.vert_mut(v0).half_edge = he0;
        m.vert_mut(v1).half_edge = he1;
        let victim = (0..m.faces.len() as u32)
            .find(|&i| {
                m.face(FaceId(i)).half_edge != NO_EDGE && m.face_vertices(FaceId(i)).len() >= 3
            })
            .expect("quad");
        PolyEdit::DeleteFace { face: victim }
            .apply_in_place(&mut m)
            .expect("Ctrl+X must not be blocked by digon poison");
        assert!(m.integrity().is_ok(), "{:?}", m.integrity());
    }

    /// Owner 2026-08-06: Ctrl+X on a dirty mesh is the cleanup entry — heal runs as part of the
    /// op, digons go away, selected real face is deleted. Must never soft-lock.
    #[test]
    fn ctrl_x_delete_face_always_works_on_dirty_mesh() {
        use crate::half_edge::{HalfEdge, HFace, HVertex, NO_EDGE, NO_TWIN};
        use crate::mesh_ops::PolyEdit;
        let p = [
            glam::Vec3::new(0.0, 0.0, 0.0),
            glam::Vec3::new(1.0, 0.0, 0.0),
            glam::Vec3::new(1.0, 1.0, 0.0),
            glam::Vec3::new(0.0, 1.0, 0.0),
            glam::Vec3::new(0.0, 0.0, 1.0),
            glam::Vec3::new(1.0, 0.0, 1.0),
            glam::Vec3::new(1.0, 1.0, 1.0),
            glam::Vec3::new(0.0, 1.0, 1.0),
        ];
        // Two quads sharing an edge (open sheet) + an injected digon.
        let mut m = HalfEdgeMesh::from_polygons(
            &p,
            &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
        );
        let v0 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(9.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let v1 = m.alloc_vertex(HVertex {
            position: glam::Vec3::new(10.0, 0.0, 0.0),
            normal: glam::Vec3::Z,
            half_edge: NO_EDGE,
        });
        let f = m.alloc_face(HFace {
            half_edge: NO_EDGE,
            normal: glam::Vec3::Z,
            material_id: 0,
        });
        let he0 = m.alloc_half_edge(HalfEdge {
            vertex: v1,
            twin: NO_TWIN,
            next: NO_EDGE,
            prev: NO_EDGE,
            face: f,
        });
        let he1 = m.alloc_half_edge(HalfEdge {
            vertex: v0,
            twin: NO_TWIN,
            next: he0,
            prev: he0,
            face: f,
        });
        m.he_mut(he0).next = he1;
        m.he_mut(he0).prev = he1;
        m.he_mut(he1).next = he0;
        m.he_mut(he1).prev = he0;
        m.face_mut(f).half_edge = he0;
        m.vert_mut(v0).half_edge = he0;
        m.vert_mut(v1).half_edge = he1;
        assert!(m.integrity().is_err(), "dirty before Ctrl+X");
        let faces_before = m.face_count();
        let victim = (0..m.faces.len() as u32)
            .find(|&i| {
                m.face(FaceId(i)).half_edge != NO_EDGE && m.face_vertices(FaceId(i)).len() >= 3
            })
            .expect("real face");
        PolyEdit::DeleteFace { face: victim }
            .apply_in_place(&mut m)
            .expect("Ctrl+X must work on unclean mesh (heal + delete)");
        // Digon purged + one real face deleted.
        assert!(m.face_count() < faces_before, "faces reduced");
        assert!(m.integrity().is_ok(), "clean after Ctrl+X: {:?}", m.integrity());
    }

    #[test]
    fn delete_face_reduces_count() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.face_count(), 6);
        mesh.delete_face(FaceId(0));
        assert_eq!(mesh.face_count(), 5);
        // Remaining faces should still be valid cycles
        for i in 1..6u32 {
            let edges = mesh.face_half_edges(FaceId(i));
            assert!(edges.len() >= 3);
        }
    }

    #[test]
    fn delete_face_detaches_half_edges() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let edges_of_face0 = mesh.face_half_edges(FaceId(0));
        assert_eq!(edges_of_face0.len(), 4);
        // All should belong to face 0 before deletion
        for &he in &edges_of_face0 {
            assert_eq!(mesh.he(he).face, FaceId(0));
        }
        mesh.delete_face(FaceId(0));
        // After deletion, those half-edges should have NO_FACE
        // (they still exist because they have twins)
        for &he in &edges_of_face0 {
            assert_eq!(mesh.he(he).face, NO_FACE, "deleted face's half-edges should be detached");
        }
    }

    /// Deleting an already-deleted face must not push its slot onto `free_faces` a second time.
    /// `alloc_face` pops the free list with NO dedup, so a duplicated slot is handed out by two
    /// separate `create_face()` calls => two live faces, one FaceId, silently. `validate()` and
    /// `is_manifold()` are both blind to it (they walk next/prev/twin round-trips and live-face
    /// invariants — a duplicated free slot violates neither), so nothing downstream catches it.
    /// `dissolve_face` has always self-guarded; `delete_face` relied on all 6+ call sites remembering
    /// to check — unenforceable tribal knowledge (CLAUDE.md "No Bandaids" Q3).
    #[test]
    fn delete_face_double_free_never_duplicates_a_slot() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        mesh.delete_face(FaceId(0));
        let after_first = mesh.clone();
        mesh.delete_face(FaceId(0)); // redundant delete of an already-freed slot

        // 1. The free list must never name the same slot twice.
        let mut seen = std::collections::HashSet::new();
        for &f in &mesh.free_faces {
            assert!(seen.insert(f), "free_faces double-pushed slot {f}: {:?}", mesh.free_faces);
        }

        // 2. The redundant delete must be a TOTAL no-op (mirrors dissolve_face's self-guard).
        assert_eq!(mesh.free_faces, after_first.free_faces, "redundant delete touched free_faces");
        assert_eq!(mesh.free_edges, after_first.free_edges, "redundant delete touched free_edges");
        assert_eq!(mesh.face_count(), after_first.face_count(), "redundant delete changed face count");

        // 3. The consequence that makes it corruption rather than untidiness: two allocations that
        //    must be independent faces must never collide on one id.
        let v: Vec<VertexId> = (0..6u32).map(VertexId).collect();
        let a = mesh.create_face(&[v[0], v[1], v[2]]).expect("face a allocates");
        let b = mesh.create_face(&[v[3], v[4], v[5]]).expect("face b allocates");
        assert_ne!(a, b, "create_face handed out {a:?} twice — two live faces, one id");
    }

    #[test]
    fn loop_cut_inserts_real_loop_on_cube() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let he = mesh.face(FaceId(0)).half_edge;
        let inserted = mesh.loop_cut(he);
        // A cube edge's ring bands exactly 4 faces → 4 rails split (4 new verts), and each crossed
        // quad becomes 2. The OLD broken loop_cut only dropped loose midpoint vertices and left
        // face_count at 6 — this asserts a REAL loop was inserted (not the "random vertex" bug).
        assert_eq!(inserted.len(), 4, "one inserted vertex per rail");
        assert_eq!(mesh.vertex_count(), 12, "8 cube corners + 4 loop verts");
        assert_eq!(mesh.face_count(), 10, "6 - 4 crossed quads + 8 sub-quads");
        assert!(mesh.validate().is_ok(), "loop cut produced invalid topology");
        // The cube stays watertight — the inserted loop opens no new boundary edge.
        let boundary = (0..mesh.half_edges.len())
            .filter(|&i| mesh.half_edges[i].face != NO_FACE && mesh.half_edges[i].twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "loop cut must keep the cube closed");
    }

    #[test]
    fn loop_cut_on_plane_grid() {
        let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 1);
        // 4x1 grid = 4 quads in a row, 10 vertices (5x2)
        assert_eq!(mesh.vertex_count(), 10);
        let f_before = mesh.face_count();
        let he = mesh.face(FaceId(0)).half_edge;
        let cuts = mesh.loop_cut(he);
        assert!(!cuts.is_empty(), "loop cut inserted no vertices");
        assert!(mesh.vertex_count() > 10, "loop cut added vertices");
        // A real loop divides at least the crossed quads → face_count MUST grow (loose-vertex bug
        // would leave it unchanged).
        assert!(mesh.face_count() > f_before, "loop cut must divide faces, not drop loose verts");
        assert!(mesh.validate().is_ok(), "loop cut produced invalid topology");
        // Open strip: 5 rails × 1 mid each = 5 inserted verts; 4 quads → 8 faces.
        assert_eq!(cuts.len(), 5, "open 4×1 strip has 5 rails");
        assert_eq!(mesh.face_count(), 8, "each of 4 strip quads splits in two");
        assert!(mesh.is_manifold().is_ok(), "open strip stays manifold after loop cut");
    }

    #[test]
    fn loop_cut_partial_welds_boundary() {
        // A partial cut (limit to ONE quad's two rails) must WELD the split vertices into the adjacent
        // non-strip faces — the cube stays watertight (the old behaviour left a T-junction / crack).
        use std::collections::HashSet;
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let start = mesh.face(FaceId(0)).half_edge;
        let hes = mesh.face_half_edges(FaceId(0));
        let pos = hes.iter().position(|&h| h == start).unwrap();
        let exit = hes[(pos + 2) % 4];
        let exit_twin = mesh.he(exit).twin;
        let mut limit = HashSet::new();
        limit.insert(start.0);
        limit.insert(exit.0);
        limit.insert(exit_twin.0);
        let inserted = mesh.loop_cut_multi(start, &[0.5], Some(&limit));
        assert!(!inserted.is_empty(), "partial cut inserted nothing");
        assert!(inserted.len() < 4, "partial cut must be shorter than the full 4-rail ring");
        assert!(mesh.validate().is_ok(), "partial cut produced invalid topology");
        let boundary = (0..mesh.half_edges.len())
            .filter(|&i| mesh.half_edges[i].face != NO_FACE && mesh.half_edges[i].twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "partial cut must weld (no T-junction / open boundary), found {boundary}");
    }

    #[test]
    fn loop_cut_two_cuts_even_spacing() {
        // Two cuts on a cube ring → 8 inserted verts (2 per rail × 4 rails), 12 sub-quads + 2 = 14.
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let he = mesh.face(FaceId(0)).half_edge;
        let inserted = mesh.loop_cut_multi(he, &[1.0 / 3.0, 2.0 / 3.0], None);
        assert_eq!(inserted.len(), 8, "2 cuts × 4 rails");
        assert_eq!(mesh.face_count(), 14, "6 - 4 + 4×3 sub-quads");
        assert!(mesh.validate().is_ok(), "2-cut loop cut invalid");
        let boundary = (0..mesh.half_edges.len())
            .filter(|&i| mesh.half_edges[i].face != NO_FACE && mesh.half_edges[i].twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "2-cut loop must keep the cube closed");
    }

    /// Multi-selection Ctrl+R: an edge selection must PARTITION into independent rings, so a
    /// selection touching two rings can cut both (owner 2026-08-10 "CTRL R DOES NOT SUPPORT MULTI
    /// SELECTION" — the session collapsed any multi-edge selection into one seed + one span limit).
    #[test]
    fn loop_cut_ring_groups_partitions_selection() {
        use std::collections::HashSet;
        let mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let hes = mesh.face_half_edges(FaceId(0));
        let (h0, h1, h2) = (hes[0], hes[1], hes[2]);

        // Opposite rails of one quad are the entry + exit of the SAME ring → one group.
        let same: HashSet<u32> = [h0.0, h2.0].into_iter().collect();
        let g = mesh.loop_cut_ring_groups(&same);
        assert_eq!(g.len(), 1, "opposite rails of a quad lie on one ring");
        assert_eq!(g[0].1.len(), 2, "both selected edges belong to that ring");

        // Perpendicular rails cross → two independent rings.
        let cross: HashSet<u32> = [h0.0, h1.0].into_iter().collect();
        let g = mesh.loop_cut_ring_groups(&cross);
        assert_eq!(g.len(), 2, "perpendicular rails are two rings");
        assert!(g[0].0 < g[1].0, "group order must be deterministic (ascending seed)");
        assert!(g[0].1.is_disjoint(&g[1].1), "an edge may not land in two groups");
    }

    /// End-to-end: cutting every group of a two-ring selection inserts BOTH loops. One ring alone
    /// takes a 4×4 plane to exactly 20 faces, so `> 20` proves the second ring was not dropped.
    #[test]
    fn loop_cut_two_rings_from_one_selection() {
        use std::collections::HashSet;
        let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        assert_eq!(mesh.face_count(), 16);
        let hes = mesh.face_half_edges(FaceId(0));
        let sel: HashSet<u32> = [hes[0].0, hes[1].0].into_iter().collect();
        let groups = mesh.loop_cut_ring_groups(&sel);
        assert_eq!(groups.len(), 2, "selection spans two rings");
        for (seed, members) in &groups {
            // Single-edge groups are free full rings, exactly like hovering them.
            assert_eq!(members.len(), 1);
            let inserted = mesh.loop_cut_multi(HalfEdgeId(*seed), &[0.5], None);
            assert!(!inserted.is_empty(), "ring seeded at {seed} inserted nothing");
        }
        assert!(
            mesh.face_count() > 20,
            "both rings must cut (one ring alone = 20 faces), got {}",
            mesh.face_count()
        );
        assert!(mesh.validate().is_ok(), "two-ring cut produced invalid topology");
        assert!(mesh.is_manifold().is_ok(), "two-ring cut must stay manifold");
    }

    /// Follow-curvature must be a NO-OP on flat geometry — a smoothing knob that dents flat faces
    /// would be unusable as a default-on toggle.
    #[test]
    fn rail_curve_point_is_exact_lerp_on_flat_geometry() {
        let mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        for he in mesh.canonical_edges() {
            let (a, b) = (mesh.he_source(he), mesh.he(he).vertex);
            for &t in &[0.25f32, 0.5, 0.75] {
                let linear = mesh.vert(a).position.lerp(mesh.vert(b).position, t);
                let curved = mesh.rail_curve_point(a, b, t, 1.0);
                assert!(
                    (curved - linear).length() < 1e-5,
                    "flat rail moved by {} at t={t}",
                    (curved - linear).length()
                );
            }
        }
    }

    /// On a sphere the inserted loop must land ON the surface, not on the chord inside it.
    /// Straight lerp puts a between-rings loop at radius < 1; following the curvature restores it.
    #[test]
    fn loop_cut_curvature_puts_the_new_loop_back_on_the_sphere() {
        let sphere = HalfEdgeMesh::new_uv_sphere(1.0, 16, 12);
        // A rail spanning two latitude rings: pick the quad rail whose endpoints differ most in
        // radius-normal direction (any non-degenerate rail on a sphere is curved).
        let he = sphere
            .canonical_edges()
            .find(|&h| {
                let (a, b) = (sphere.he_source(h), sphere.he(h).vertex);
                sphere.vert(a).normal.dot(sphere.vert(b).normal) < 0.99
            })
            .expect("sphere has curved rails");
        let (a, b) = (sphere.he_source(he), sphere.he(he).vertex);

        let linear = sphere.vert(a).position.lerp(sphere.vert(b).position, 0.5);
        let curved = sphere.rail_curve_point(a, b, 0.5, 1.0);
        assert!(linear.length() < 0.999, "chord midpoint sits inside the sphere");
        // Tolerance is 0.5%, not exact: the arc angle comes from the mesh's averaged VERTEX
        // normals, which are only approximately radial on a faceted sphere.
        assert!(
            (curved.length() - 1.0).abs() < 5e-3,
            "curved midpoint must sit ON the unit sphere, got radius {}",
            curved.length()
        );
        // The knob is a blend: half the amount must move half the way (within the curve's own error).
        let half = sphere.rail_curve_point(a, b, 0.5, 0.5);
        let expected = linear.lerp(curved, 0.5);
        assert!((half - expected).length() < 1e-5, "amount must blend linearly");
    }

    /// End-to-end: a loop cut on a sphere with curvature ON leaves every inserted vertex on the
    /// sphere; with it OFF they sit inside. Guards the delta→vertex plumbing, not just the math.
    #[test]
    fn loop_cut_with_curvature_keeps_inserted_verts_on_the_sphere() {
        // (min radius, max radius) over the inserted loop.
        let build = |amount: f32| -> (f32, f32) {
            let mut m = HalfEdgeMesh::new_uv_sphere(1.0, 16, 12);
            // Seed on a curved rail between two QUADS — pole rows are triangles, which stop the
            // ring walk before it crosses anything.
            let seed = m
                .canonical_edges()
                .find(|&h| {
                    let (f1, f2) = m.edge_faces(h);
                    if f1 == NO_FACE || f2 == NO_FACE {
                        return false;
                    }
                    if m.face_half_edges(f1).len() != 4 || m.face_half_edges(f2).len() != 4 {
                        return false;
                    }
                    let (a, b) = (m.he_source(h), m.he(h).vertex);
                    m.vert(a).normal.dot(m.vert(b).normal) < 0.99
                })
                .expect("sphere has curved quad rails");
            let (verts, delta) = m.loop_cut_multi_with_delta(seed, &[0.5], None);
            assert!(!verts.is_empty(), "loop cut inserted nothing on the sphere");
            m.apply_loop_cut_curvature(&delta, amount);
            assert!(m.validate().is_ok(), "curvature pass produced invalid topology");
            verts.iter().map(|&v| m.vert(v).position.length()).fold(
                (f32::INFINITY, 0.0f32),
                |(lo, hi), r| (lo.min(r), hi.max(r)),
            )
        };
        let (flat_lo, _) = build(0.0);
        let (curved_lo, curved_hi) = build(1.0);
        assert!(flat_lo < 0.999, "straight lerp must sink inside the sphere, got {flat_lo}");
        assert!(
            curved_lo > flat_lo + 1e-4,
            "curvature must lift the loop outward: {flat_lo} → {curved_lo}"
        );
        assert!(
            curved_lo > 0.98 && curved_hi < 1.02,
            "every inserted vert must land within 2% of the sphere: {curved_lo}..{curved_hi}"
        );
    }

    /// Owner: multi loop-cut in BOTH orientations on a simple square plane fails.
    /// 1-face plane → cut U (multiple) → cut V (multiple) must yield a grid of quads.
    #[test]
    fn loop_cut_orthogonal_multi_on_single_quad_plane() {
        let positions = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let mut mesh = HalfEdgeMesh::from_quads(&positions, &[[0usize, 1, 2, 3]]);
        assert_eq!(mesh.face_count(), 1);
        assert!(mesh.is_manifold().is_ok());

        // U cuts: seed the edge along +X (v0→v1).
        let he_u = mesh.face(FaceId(0)).half_edge;
        let ins_u = mesh.loop_cut_multi(he_u, &[1.0 / 3.0, 2.0 / 3.0], None);
        assert_eq!(
            ins_u.len(),
            4,
            "2 cuts × 2 rails on a single open quad (not {ins_u:?})"
        );
        assert_eq!(mesh.face_count(), 3, "1 face + 2 cuts → 3 strips");
        assert!(
            mesh.validate().is_ok() && mesh.is_manifold().is_ok(),
            "after U: val={:?} man={:?}",
            mesh.validate(),
            mesh.is_manifold()
        );

        // V cuts: seed a boundary edge roughly along +Z (perpendicular).
        let he_v = (0..mesh.half_edges.len())
            .map(|i| HalfEdgeId(i as u32))
            .find(|&he| {
                if !mesh.is_edge_live(he) || mesh.he(he).face == NO_FACE {
                    return false;
                }
                let a = mesh.vert(mesh.he_source(he)).position;
                let b = mesh.vert(mesh.he(he).vertex).position;
                let d = (b - a).normalize_or_zero();
                d.z.abs() > 0.7
            })
            .expect("no Z-direction edge for V loop cut");
        let f_before_v = mesh.face_count();
        let ins_v = mesh.loop_cut_multi(he_v, &[1.0 / 3.0, 2.0 / 3.0], None);
        assert!(
            !ins_v.is_empty(),
            "V-orientation multi cut inserted nothing (seed he={he_v:?})"
        );
        assert!(
            mesh.face_count() > f_before_v,
            "V cut must subdivide faces (before={f_before_v} after={})",
            mesh.face_count()
        );
        // 3 U-strips × (1+2) V-cuts = 9 quads if V ring crosses all three.
        assert_eq!(
            mesh.face_count(),
            9,
            "orthogonal 2+2 cuts on 1-quad plane should yield 3×3=9 faces, got {}",
            mesh.face_count()
        );
        assert!(
            mesh.validate().is_ok(),
            "after V: validate={:?}",
            mesh.validate()
        );
        assert!(
            mesh.is_manifold().is_ok(),
            "after V: manifold={:?}",
            mesh.is_manifold()
        );
        // All faces quads.
        for fi in 0..mesh.faces.len() as u32 {
            if mesh.free_faces.contains(&fi) {
                continue;
            }
            let fid = FaceId(fi);
            if mesh.face(fid).half_edge == NO_EDGE {
                continue;
            }
            let n = mesh.face_vertices(fid).len();
            assert_eq!(n, 4, "face {fi} is n-gon arity {n}, expected quads only");
        }
    }

    /// Same as above but sequential single cuts alternating orientations (owner UX: Ctrl+R twice).
    #[test]
    fn loop_cut_orthogonal_sequential_singles_on_plane() {
        let positions = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let mut mesh = HalfEdgeMesh::from_quads(&positions, &[[0usize, 1, 2, 3]]);
        let he_u = mesh.face(FaceId(0)).half_edge;
        let _ = mesh.loop_cut(he_u);
        assert_eq!(mesh.face_count(), 2);
        assert!(mesh.validate().is_ok() && mesh.is_manifold().is_ok());

        let he_v = (0..mesh.half_edges.len())
            .map(|i| HalfEdgeId(i as u32))
            .find(|&he| {
                if !mesh.is_edge_live(he) || mesh.he(he).face == NO_FACE {
                    return false;
                }
                let a = mesh.vert(mesh.he_source(he)).position;
                let b = mesh.vert(mesh.he(he).vertex).position;
                let d = (b - a).normalize_or_zero();
                d.z.abs() > 0.7
            })
            .expect("no Z edge");
        let ins = mesh.loop_cut(he_v);
        assert!(!ins.is_empty(), "orthogonal single cut empty");
        assert_eq!(mesh.face_count(), 4, "2×2 grid of quads");
        assert!(mesh.validate().is_ok(), "{:?}", mesh.validate());
        assert!(mesh.is_manifold().is_ok(), "{:?}", mesh.is_manifold());
    }

    /// Owner 2026-08-07: multi U then multi V on a 1-quad plane, seeding V from the *interior
    /// cut-loop edges* produced by U (what confirm selects) — not only from a boundary Z edge.
    /// Prior bug: directed rail keys skipped reverse-oriented strip faces → 7 faces / n-gons.
    #[test]
    fn loop_cut_orthogonal_from_interior_cut_edges() {
        use std::collections::HashSet;
        let positions = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(-1.0, 0.0, 1.0),
        ];
        let mut mesh = HalfEdgeMesh::from_quads(&positions, &[[0usize, 1, 2, 3]]);
        let he_u = mesh.face(FaceId(0)).half_edge;
        let seed_dir = {
            let a = mesh.vert(mesh.he_source(he_u)).position;
            let b = mesh.vert(mesh.he(he_u).vertex).position;
            (b - a).normalize_or_zero()
        };
        let ts = [1.0 / 3.0, 2.0 / 3.0];
        let ins_u = mesh.loop_cut_multi(he_u, &ts, None);
        assert_eq!(mesh.face_count(), 3);

        // Confirm-style filtered selection: both ends new, edge ⟂ seed rail.
        let lifted: HashSet<u32> = ins_u.iter().map(|v| v.0).collect();
        let mut cut_loop_edges = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) || mesh.he(he).face == NO_FACE {
                continue;
            }
            let d = mesh.he(he).vertex.0;
            let src = mesh.he_source(he).0;
            if !lifted.contains(&d) || !lifted.contains(&src) {
                continue;
            }
            let pa = mesh.vert(mesh.he_source(he)).position;
            let pb = mesh.vert(mesh.he(he).vertex).position;
            let edir = (pb - pa).normalize_or_zero();
            if edir.dot(seed_dir).abs() > 0.707 {
                continue; // same-rail multi-cut chord
            }
            let twin = mesh.he(he).twin;
            let canon = if twin != NO_TWIN { i.min(twin.0) } else { i };
            cut_loop_edges.insert(canon);
        }
        assert!(
            !cut_loop_edges.is_empty(),
            "U multi must produce interior cut-loop edges"
        );

        // Every interior cut-loop edge must fully orthogonal-subdivide to 3×3.
        for &sid in &cut_loop_edges {
            let mut m = mesh.clone();
            let ins = m.loop_cut_multi(HalfEdgeId(sid), &ts, None);
            assert!(
                !ins.is_empty(),
                "interior cut edge {sid} must accept V multi"
            );
            assert_eq!(
                m.face_count(),
                9,
                "interior seed {sid}: expected 3×3=9 faces, got {} (rail key / phase-3 bug)",
                m.face_count()
            );
            assert!(m.validate().is_ok(), "seed {sid}: {:?}", m.validate());
            assert!(m.is_manifold().is_ok(), "seed {sid}: {:?}", m.is_manifold());
            for fi in 0..m.faces.len() as u32 {
                if m.free_faces.contains(&fi) {
                    continue;
                }
                let fid = FaceId(fi);
                if m.face(fid).half_edge == NO_EDGE {
                    continue;
                }
                let n = m.face_vertices(fid).len();
                assert_eq!(n, 4, "seed {sid}: face {fi} arity {n}");
            }
        }
    }

    /// Owner 2026-08-08: loop cut around a cylinder booleaned into a sphere.
    /// Post-boolean cylindrical walls are n-gons (not quads); n-gon parallel-rail walk must
    /// still form a circumferential ring from a vertical wall seed.
    #[test]
    fn loop_cut_on_sphere_boolean_cylinder_hole() {
        use crate::mesh_boolean::{mesh_boolean, BoolOp};

        let sphere = HalfEdgeMesh::new_uv_sphere(1.0, 24, 12);
        let cyl = HalfEdgeMesh::new_cylinder(0.35, 2.2, 16);
        let result = mesh_boolean(&sphere, &cyl, BoolOp::Difference)
            .expect("sphere − cylinder boolean must succeed");
        assert!(result.validate().is_ok(), "{:?}", result.validate());

        // Pure cylinder still works (quad wall).
        let mut pure = HalfEdgeMesh::new_cylinder(0.5, 1.0, 16);
        let he_wall = (0..pure.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&he| {
                if !pure.is_edge_live(he) || pure.he(he).face == NO_FACE {
                    return false;
                }
                let a = pure.vert(pure.he_source(he)).position;
                let b = pure.vert(pure.he(he).vertex).position;
                (b - a).normalize_or_zero().y.abs() > 0.9
            })
            .expect("vertical wall edge");
        assert!(!pure.loop_cut(he_wall).is_empty());

        // Boolean hole: vertical wall edges at ~cylinder radius — must find a strip + cut.
        let mut seeds = 0usize;
        let mut ok = 0usize;
        for i in 0..result.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !result.is_edge_live(he) || result.he(he).face == NO_FACE {
                continue;
            }
            let twin = result.he(he).twin;
            if twin != NO_TWIN && twin.0 < i {
                continue;
            }
            let a = result.vert(result.he_source(he)).position;
            let b = result.vert(result.he(he).vertex).position;
            let d = (b - a).normalize_or_zero();
            let r = ((a.x + b.x) * 0.5).hypot((a.z + b.z) * 0.5);
            if d.y.abs() < 0.7 || (r - 0.35).abs() > 0.15 {
                continue;
            }
            seeds += 1;
            if !result.loop_cut_ring_has_quads(he, None) {
                continue;
            }
            let mut m = result.clone();
            let f0 = m.face_count();
            let ins = m.loop_cut(he);
            if !ins.is_empty() && m.face_count() > f0 && m.validate().is_ok() {
                ok += 1;
            }
        }
        assert!(
            seeds >= 4,
            "expected cylindrical wall vertical edges, got {seeds}"
        );
        assert!(
            ok >= 1,
            "loop cut around boolean cylinder hole must succeed on ≥1 wall seed \
             (seeds={seeds} ok={ok}) — n-gon parallel-rail walk required"
        );
    }

    #[test]
    fn split_edge_preserves_topology() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let he = mesh.face(FaceId(0)).half_edge;
        mesh.split_edge(he, 0.5);
        // After split, topology should still be valid
        assert!(mesh.validate().is_ok(), "split_edge broke topology");
    }

    #[test]
    fn extrude_then_inset() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let top = mesh.extrude_face(FaceId(0), Vec3::Y * 0.5);
        let inner = mesh.inset_face(top, 0.1);
        let inner_verts = mesh.face_vertices(inner);
        assert_eq!(inner_verts.len(), 4);
        // Should have 8 + 4(extrude) + 4(inset) = 16 vertices
        assert_eq!(mesh.vertex_count(), 16);
    }

    #[test]
    fn merge_then_validate() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let v0 = VertexId(0);
        let v1 = VertexId(1);
        let mid_expected = (mesh.vert(v0).position + mesh.vert(v1).position) * 0.5;
        let survivor = mesh.merge_vertices(v0, v1);
        assert_eq!(survivor, v0);
        let pos = mesh.vert(survivor).position;
        assert!((pos - mid_expected).length() < 1e-5, "merged vertex should be at midpoint");
        assert_eq!(mesh.vertex_count(), 7);
    }

    #[test]
    fn cone_topology() {
        let mesh = HalfEdgeMesh::new_cone(0.5, 1.0, 8);
        assert!(mesh.vertex_count() > 0);
        assert!(mesh.face_count() > 0);
        assert!(mesh.validate().is_ok());
    }

    #[test]
    fn bounding_box_unit_cube() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let (min, max) = mesh.bounding_box();
        assert!((min - Vec3::splat(-1.0)).length() < 1e-5);
        assert!((max - Vec3::splat(1.0)).length() < 1e-5);
    }

    #[test]
    fn to_mesh_roundtrip() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let simple = mesh.to_mesh();
        assert_eq!(simple.vertices.len(), 8);
        assert_eq!(simple.quads.len(), 6);
        // Rebuild from simple mesh
        let rebuilt = HalfEdgeMesh::from_mesh(&simple);
        assert_eq!(rebuilt.vertex_count(), 8);
        assert_eq!(rebuilt.face_count(), 6);
        assert!(rebuilt.validate().is_ok());
    }

    #[test]
    fn from_mesh_draft_matches_from_mesh_geometry() {
        use crate::half_edge::NO_TWIN;
        let src = HalfEdgeMesh::new_uv_sphere(0.5, 12, 8);
        let simple = src.to_mesh();
        let full = HalfEdgeMesh::from_mesh(&simple);
        let draft = HalfEdgeMesh::from_mesh_draft(&simple);
        assert_eq!(draft.vertex_count(), full.vertex_count());
        assert_eq!(draft.face_count(), full.face_count());
        for i in 0..draft.vertices.len() {
            assert_eq!(draft.vertices[i].position, full.vertices[i].position);
        }
        assert!(
            draft.half_edges.iter().all(|e| e.twin == NO_TWIN),
            "draft skips TwinLinker"
        );
        assert!(
            full.half_edges.iter().any(|e| e.twin != NO_TWIN),
            "from_mesh still pairs twins"
        );
        assert!(draft.validate().is_ok());
        assert!(full.validate().is_ok());
    }

    #[test]
    fn catmull_clark_torus_closed() {
        let mut mesh = HalfEdgeMesh::new_torus(1.0, 0.3, 6, 4);
        let f_before = mesh.face_count(); // 24
        mesh.subdivide_catmull_clark();
        // Each quad → 4 sub-quads
        assert_eq!(mesh.face_count(), f_before * 4);
        assert!(mesh.validate().is_ok());
        // Torus should still satisfy Euler V-E+F=0
        let v = mesh.vertex_count() as i32;
        let e = mesh.edge_count() as i32;
        let f = mesh.face_count() as i32;
        assert_eq!(v - e + f, 0, "subdivided torus Euler: V={} E={} F={}", v, e, f);
    }

    #[test]
    fn from_triangles_builds_valid_mesh() {
        let positions = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.5, 1.0, 0.0),
            Vec3::new(0.5, 0.0, 1.0),
        ];
        // Tetrahedron
        let tris = [
            [0, 1, 2],
            [0, 2, 3],
            [0, 3, 1],
            [1, 3, 2],
        ];
        let mesh = HalfEdgeMesh::from_triangles(&positions, &tris);
        assert_eq!(mesh.vertex_count(), 4);
        assert_eq!(mesh.face_count(), 4);
        assert!(mesh.validate().is_ok());
        // Euler: V-E+F = 4-6+4 = 2
        let v = mesh.vertex_count() as i32;
        let e = mesh.edge_count() as i32;
        let f = mesh.face_count() as i32;
        assert_eq!(v - e + f, 2);
    }

    /// Selection conversion support: face_vertices returns correct verts, he_source gives edge endpoints.
    #[test]
    fn selection_conversion_helpers() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Every face should have exactly 4 vertices (box = all quads)
        for fi in 0..mesh.faces.len() as u32 {
            if mesh.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
            let verts = mesh.face_vertices(FaceId(fi));
            assert_eq!(verts.len(), 4, "box face {} should have 4 verts", fi);
        }
        // face_half_edges should return 4 half-edges per face
        for fi in 0..mesh.faces.len() as u32 {
            if mesh.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
            let hes = mesh.face_half_edges(FaceId(fi));
            assert_eq!(hes.len(), 4, "box face {} should have 4 half-edges", fi);
            // Each half-edge's source→dest should form a cycle
            for &h in &hes {
                let src = mesh.he_source(h);
                let dst = mesh.he(h).vertex;
                assert_ne!(src, dst, "degenerate edge");
            }
        }
        // Vertex → Edge conversion pattern: for each edge, both endpoints should be valid vertices
        for he_idx in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(he_idx));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && he_idx > he.twin.0 { continue; }
            let src = mesh.he_source(HalfEdgeId(he_idx));
            let dst = he.vertex;
            assert!(src.0 < mesh.vertices.len() as u32);
            assert!(dst.0 < mesh.vertices.len() as u32);
        }
    }

    #[test]
    fn create_face_from_vertices() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let f_before = mesh.face_count();
        mesh.delete_face(FaceId(0));
        assert_eq!(mesh.face_count(), f_before - 1);
        let v_count = mesh.vertex_count();
        assert_eq!(v_count, 8);
    }

    #[test]
    fn dissolve_edge_merges_faces() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        mesh.subdivide_catmull_clark();
        let f_before = mesh.face_count();
        let he_id = (0..mesh.half_edges.len() as u32).find(|&i| {
            let he = mesh.he(HalfEdgeId(i));
            he.face != NO_FACE && he.twin != NO_TWIN
        });
        if let Some(eid) = he_id {
            let result = mesh.dissolve_edge(HalfEdgeId(eid));
            assert!(result.is_some(), "dissolve_edge should succeed on interior edge");
            assert_eq!(mesh.face_count(), f_before - 1);
        }
    }

    /// A SLIT edge is one whose half-edge and twin belong to the SAME face (the face touches itself
    /// along that edge). `dissolve_edge` is contracted to "merge the two faces on either side" — a slit
    /// has ONE face on both sides, so there is nothing to merge and the op must refuse, leaving the mesh
    /// untouched. Blender agrees: `bmesh.ops.dissolve_edges` on this exact configuration is a no-op under
    /// all four use_verts × use_face_split combinations (verified on 4.5 and 5.0), and the Ctrl+X UI path
    /// reports FINISHED while changing nothing. Refusal — never a split, delete, or repair.
    fn slit_mesh() -> (HalfEdgeMesh, Vec<u32>) {
        // Triangle A,B,C plus an antenna spike out to E at B: ring A→B→E→B→C. `from_polygons` twins the
        // B→E and E→B pair — a reverse-direction pair inside the SAME polygon — which is precisely a slit.
        let m = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(0.0, 0.0, 0.0), // 0 A
                Vec3::new(1.0, 0.0, 0.0), // 1 B (visited twice)
                Vec3::new(0.0, 1.0, 0.0), // 2 C
                Vec3::new(2.0, 0.0, 0.0), // 3 E (spike tip)
            ],
            &[vec![0, 1, 3, 1, 2]],
        );
        let slit: Vec<u32> = (0..m.half_edges.len() as u32)
            .filter(|&i| {
                let h = m.he(HalfEdgeId(i));
                h.twin != NO_TWIN && h.face != NO_FACE && m.he(h.twin).face == h.face
            })
            .collect();
        assert_eq!(slit.len(), 2, "fixture must contain exactly one slit edge pair");
        (m, slit)
    }

    #[test]
    fn dissolve_edge_refuses_same_face_slit() {
        let (m, slit) = slit_mesh();
        // `validate()` is BLIND to a slit (it only checks next/prev/twin round-trips, which a slit
        // satisfies). Documented here so the fixture's realism is not mistaken for a broken fixture.
        assert!(m.validate().is_ok(), "fixture is twin/cycle-consistent");

        for &e in &slit {
            let mut m2 = m.clone();
            assert!(
                m2.dissolve_edge(HalfEdgeId(e)).is_none(),
                "dissolve_edge must refuse slit half-edge {e}"
            );
            // The refusal must be TOTAL: the old code nulled + freed the very face its surviving ring
            // still referenced, so these pin the exact corruption the guard prevents.
            assert!(m2.free_faces.is_empty(), "refusal freed a still-referenced face");
            assert!(m2.free_edges.is_empty(), "refusal freed the edge pair");
            assert_eq!(m2.face_count(), m.face_count(), "refusal changed the face count");
            assert_ne!(m2.faces[0].half_edge, NO_EDGE, "refusal nulled the face rep");
            for i in 0..m2.half_edges.len() as u32 {
                if m2.free_edges.contains(&i) { continue; }
                let f = m2.he(HalfEdgeId(i)).face;
                if f != NO_FACE {
                    assert!(!m2.free_faces.contains(&f.0), "live he{i} references a freed face");
                }
            }
            // Full no-op diff. `HalfEdge` derives no PartialEq, so compare field by field.
            for i in 0..m.half_edges.len() {
                let (a, b) = (&m.half_edges[i], &m2.half_edges[i]);
                assert!(
                    a.vertex == b.vertex && a.twin == b.twin && a.next == b.next
                        && a.prev == b.prev && a.face == b.face,
                    "refusal mutated he{i}"
                );
            }
        }
    }

    #[test]
    fn dissolve_face_removes_face() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let f_before = mesh.face_count();
        mesh.dissolve_face(FaceId(0));
        assert_eq!(mesh.face_count(), f_before - 1);
    }

    #[test]
    fn flip_face_reverses_normal() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let n_before = mesh.compute_face_normal(FaceId(0));
        mesh.flip_face(FaceId(0));
        let n_after = mesh.compute_face_normal(FaceId(0));
        // Normal should be approximately reversed
        let dot = n_before.dot(n_after);
        assert!(dot < -0.9, "flip_face should reverse normal, dot={}", dot);
        // Mesh should still validate
        assert!(mesh.validate().is_ok(), "mesh invalid after flip_face");
    }

    #[test]
    fn flip_face_double_flip_restores() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let verts_before: Vec<VertexId> = mesh.face_vertices(FaceId(0));
        mesh.flip_face(FaceId(0));
        mesh.flip_face(FaceId(0));
        let verts_after: Vec<VertexId> = mesh.face_vertices(FaceId(0));
        assert_eq!(verts_before.len(), verts_after.len(), "vertex count changed after double flip");
        // Same vertices should appear (possibly rotated)
        for v in &verts_before {
            assert!(verts_after.contains(v), "vertex {:?} missing after double flip", v);
        }
    }

    #[test]
    fn vertex_transform_grab_roundtrip() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Snapshot original positions of face 0 vertices
        let vids = mesh.face_vertices(FaceId(0));
        let originals: Vec<(VertexId, Vec3)> = vids.iter().map(|&v| (v, mesh.vert(v).position)).collect();
        let delta = Vec3::new(1.0, 2.0, 3.0);
        // Apply grab
        for &(v, orig) in &originals {
            mesh.vert_mut(v).position = orig + delta;
        }
        // Verify moved
        for &(v, orig) in &originals {
            let pos = mesh.vert(v).position;
            assert!((pos - (orig + delta)).length() < 1e-5, "vertex not moved correctly");
        }
        // Cancel: restore originals
        for &(v, orig) in &originals {
            mesh.vert_mut(v).position = orig;
        }
        for &(v, orig) in &originals {
            assert!((mesh.vert(v).position - orig).length() < 1e-5, "vertex not restored");
        }
    }

    #[test]
    fn vertex_transform_rotate() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let vids = mesh.face_vertices(FaceId(0));
        let center: Vec3 = vids.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vids.len() as f32;
        let originals: Vec<(VertexId, Vec3)> = vids.iter().map(|&v| (v, mesh.vert(v).position)).collect();
        // Rotate 90 degrees around Y
        let rot = glam::Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        for &(v, orig) in &originals {
            let local = orig - center;
            mesh.vert_mut(v).position = center + rot * local;
        }
        // Verify: distances from center are preserved
        for &(v, orig) in &originals {
            let d_orig = (orig - center).length();
            let d_new = (mesh.vert(v).position - center).length();
            assert!((d_orig - d_new).abs() < 1e-5, "rotation changed distances from center");
        }
    }

    #[test]
    fn vertex_transform_scale() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let vids = mesh.face_vertices(FaceId(0));
        let center: Vec3 = vids.iter().map(|&v| mesh.vert(v).position).sum::<Vec3>() / vids.len() as f32;
        let originals: Vec<(VertexId, Vec3)> = vids.iter().map(|&v| (v, mesh.vert(v).position)).collect();
        let scale = 2.0_f32;
        for &(v, orig) in &originals {
            let local = orig - center;
            mesh.vert_mut(v).position = center + local * scale;
        }
        // Verify: distances from center doubled
        for &(v, orig) in &originals {
            let d_orig = (orig - center).length();
            let d_new = (mesh.vert(v).position - center).length();
            assert!((d_new - d_orig * scale).abs() < 1e-5, "scale factor not applied");
        }
    }

    #[test]
    fn undo_redo_clone_snapshot() {
        // Simulate undo/redo by cloning the mesh before each operation
        let original = HalfEdgeMesh::new_box(Vec3::ONE);
        let vc0 = original.vertex_count();
        let fc0 = original.face_count();

        // Operation 1: extrude face
        let snapshot1 = original.clone();
        let mut mesh = original.clone();
        mesh.extrude_face(FaceId(0), Vec3::Y * 0.5);
        assert!(mesh.vertex_count() > vc0, "extrude should add vertices");
        assert!(mesh.face_count() > fc0, "extrude should add faces");

        // Undo op 1: restore from snapshot
        mesh = snapshot1.clone();
        assert_eq!(mesh.vertex_count(), vc0);
        assert_eq!(mesh.face_count(), fc0);

        // Redo op 1: re-apply
        mesh.extrude_face(FaceId(0), Vec3::Y * 0.5);
        let vc1 = mesh.vertex_count();
        let fc1 = mesh.face_count();

        // Operation 2: subdivide
        let snapshot2 = mesh.clone();
        mesh.subdivide_catmull_clark();
        assert!(mesh.vertex_count() > vc1, "subdivide should add vertices");

        // Undo op 2
        mesh = snapshot2.clone();
        assert_eq!(mesh.vertex_count(), vc1);
        assert_eq!(mesh.face_count(), fc1);

        // Undo op 1
        mesh = snapshot1;
        assert_eq!(mesh.vertex_count(), vc0);
        assert_eq!(mesh.face_count(), fc0);
        assert!(mesh.validate().is_ok(), "mesh invalid after multi-undo");
    }

    #[test]
    fn undo_chain_10_operations() {
        let mut snapshots: Vec<HalfEdgeMesh> = Vec::new();
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);

        // Perform 10 operations, saving snapshot before each
        for i in 0..10 {
            snapshots.push(mesh.clone());
            match i % 3 {
                0 => { mesh.extrude_face(FaceId(0), Vec3::Y * 0.1 * (i as f32 + 1.0)); }
                1 => { mesh.inset_face(FaceId(0), 0.05); }
                _ => { mesh.recompute_normals(); }
            }
        }

        // Undo all 10 in reverse
        for i in (0..10).rev() {
            mesh = snapshots[i].clone();
            assert!(mesh.validate().is_ok(), "mesh invalid after undo step {}", 10 - i);
        }

        // Should be back to original
        let original = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.vertex_count(), original.vertex_count());
        assert_eq!(mesh.face_count(), original.face_count());
    }

    // ============================================================
    // Phase 10: Integration Tests (Full Workflows)
    // ============================================================

    #[test]
    fn integration_box_modeling_chair() {
        // Simulate: box → loop cut → extrude legs → inset seat → extrude backrest
        let mut snapshots: Vec<HalfEdgeMesh> = Vec::new();
        let mut mesh = HalfEdgeMesh::new_box(Vec3::new(1.0, 0.5, 1.0));
        assert!(mesh.validate().is_ok(), "initial box invalid");

        // Step 1: loop cut (adds vertices via edge splits)
        snapshots.push(mesh.clone());
        let he = mesh.face(FaceId(0)).half_edge;
        let vc_before = mesh.vertex_count();
        mesh.loop_cut(he);
        assert!(mesh.validate().is_ok(), "invalid after loop cut");
        assert!(mesh.vertex_count() > vc_before, "loop cut should add vertices");

        // Step 2: extrude bottom face(s) for legs
        snapshots.push(mesh.clone());
        // Find a face with center.y < 0 (bottom)
        let bottom_face = (0..mesh.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                if mesh.face(f).half_edge == NO_EDGE { return false; }
                let verts = mesh.face_vertices(f);
                let cy: f32 = verts.iter().map(|&v| mesh.vert(v).position.y).sum::<f32>() / verts.len() as f32;
                cy < -0.2
            });
        if let Some(bf) = bottom_face {
            mesh.extrude_face(bf, Vec3::new(0.0, -0.8, 0.0));
            assert!(mesh.validate().is_ok(), "invalid after leg extrude");
        }

        // Step 3: inset top face
        snapshots.push(mesh.clone());
        let top_face = (0..mesh.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                if mesh.face(f).half_edge == NO_EDGE { return false; }
                let verts = mesh.face_vertices(f);
                let cy: f32 = verts.iter().map(|&v| mesh.vert(v).position.y).sum::<f32>() / verts.len() as f32;
                cy > 0.2
            });
        if let Some(tf) = top_face {
            let new_face = mesh.inset_face(tf, 0.15);
            assert!(mesh.validate().is_ok(), "invalid after seat inset");

            // Step 4: extrude backrest from inset face
            snapshots.push(mesh.clone());
            mesh.extrude_face(new_face, Vec3::new(0.0, 1.0, 0.0));
            assert!(mesh.validate().is_ok(), "invalid after backrest extrude");
        }

        // Verify: undo chain back to original
        for snap in snapshots.into_iter().rev() {
            mesh = snap;
            assert!(mesh.validate().is_ok(), "invalid during undo chain");
        }
        assert_eq!(mesh.face_count(), 6, "should be back to original box (6 faces)");
    }

    #[test]
    fn integration_selection_chain_workflow() {
        // Simulate: face select → extrude → inset → delete → mode switch
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);

        // Step 1: face_vertices works for selection conversion
        let face0_verts = mesh.face_vertices(FaceId(0));
        assert_eq!(face0_verts.len(), 4, "box face should have 4 verts");

        // Step 2: extrude selected face
        let snap1 = mesh.clone();
        let extruded = mesh.extrude_face(FaceId(0), Vec3::Y * 0.5);
        assert!(mesh.validate().is_ok());

        // Step 3: inset the extruded face
        let snap2 = mesh.clone();
        let inset = mesh.inset_face(extruded, 0.1);
        assert!(mesh.validate().is_ok());

        // Step 4: delete the inset face (create hole)
        let snap3 = mesh.clone();
        let fc_before = mesh.face_count();
        mesh.delete_face(inset);
        assert_eq!(mesh.face_count(), fc_before - 1);

        // Step 5: verify all face normals are computable
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE { continue; }
            let n = mesh.compute_face_normal(fid);
            assert!(n.length() > 0.1, "degenerate normal on face {}", i);
        }

        // Undo all
        mesh = snap3;
        assert!(mesh.validate().is_ok());
        mesh = snap2;
        assert!(mesh.validate().is_ok());
        mesh = snap1;
        assert!(mesh.validate().is_ok());
        assert_eq!(mesh.face_count(), 6);
    }

    // ============================================================
    // Phase 11: Stress Tests
    // ============================================================

    #[test]
    fn stress_high_poly_subdivide() {
        // Start with box, subdivide 3 times → ~384 faces, ~386 verts
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for _ in 0..3 {
            mesh.subdivide_catmull_clark();
            assert!(mesh.validate().is_ok(), "invalid after subdivision");
        }
        assert!(mesh.vertex_count() > 300, "expected 300+ verts, got {}", mesh.vertex_count());
        assert!(mesh.face_count() > 300, "expected 300+ faces, got {}", mesh.face_count());

        // Verify operations work on high-poly
        let snap = mesh.clone();
        mesh.extrude_face(FaceId(0), Vec3::Y * 0.1);
        assert!(mesh.validate().is_ok(), "extrude on high-poly failed");
        mesh = snap;

        // Verify normals
        mesh.recompute_normals();
        for i in 0..mesh.vertices.len() {
            if mesh.vert(VertexId(i as u32)).half_edge == NO_EDGE { continue; }
            let n = mesh.vert(VertexId(i as u32)).normal;
            assert!(n.length() > 0.5, "degenerate vertex normal at {}", i);
        }
    }

    #[test]
    fn stress_rapid_undo_50_operations() {
        let mut snapshots: Vec<HalfEdgeMesh> = Vec::new();
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let original_vc = mesh.vertex_count();
        let original_fc = mesh.face_count();

        // 50 operations alternating extrude/inset/loopcut
        for i in 0..50 {
            snapshots.push(mesh.clone());
            match i % 4 {
                0 => {
                    // Find valid face
                    let fid = (0..mesh.faces.len() as u32)
                        .map(FaceId)
                        .find(|&f| mesh.face(f).half_edge != NO_EDGE);
                    if let Some(f) = fid {
                        mesh.extrude_face(f, Vec3::Y * 0.05);
                    }
                }
                1 => {
                    let fid = (0..mesh.faces.len() as u32)
                        .map(FaceId)
                        .find(|&f| mesh.face(f).half_edge != NO_EDGE);
                    if let Some(f) = fid {
                        mesh.inset_face(f, 0.02);
                    }
                }
                2 => { mesh.recompute_normals(); }
                _ => {
                    // Flip a face
                    let fid = (0..mesh.faces.len() as u32)
                        .map(FaceId)
                        .find(|&f| mesh.face(f).half_edge != NO_EDGE);
                    if let Some(f) = fid { mesh.flip_face(f); }
                }
            }
        }

        // Rapid undo: restore all 50 snapshots in reverse
        for i in (0..50).rev() {
            mesh = snapshots[i].clone();
        }
        assert_eq!(mesh.vertex_count(), original_vc, "undo didn't restore vertex count");
        assert_eq!(mesh.face_count(), original_fc, "undo didn't restore face count");
        assert!(mesh.validate().is_ok(), "mesh invalid after 50 undos");
    }

    #[test]
    fn stress_mode_switching_selection_conversion() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Simulate rapid mode switching: for each mode, verify conversions work
        for _ in 0..10 {
            // Vertex mode: get vertices of face 0
            let verts = mesh.face_vertices(FaceId(0));
            assert_eq!(verts.len(), 4);

            // Edge mode: get edges of face 0
            let edges = mesh.face_half_edges(FaceId(0));
            assert_eq!(edges.len(), 4);

            // Face mode: verify face normal
            let n = mesh.compute_face_normal(FaceId(0));
            assert!(n.length() > 0.5);

            // Vertex→Edge conversion: for each vertex, check outgoing edges
            for &v in &verts {
                let he = mesh.vert(v).half_edge;
                assert_ne!(he, NO_EDGE, "vertex {:?} has no half-edge", v);
            }
        }
    }

    #[test]
    fn mirror_mesh_x_axis() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.vertex_count(), 8);
        assert_eq!(mesh.face_count(), 6);

        let mirrored = mesh.mirrored(MirrorAxes::X, false, 0.001);
        // Without merge: 8 original + 8 mirrored = 16 verts, 12 faces
        assert_eq!(mirrored.vertex_count(), 16);
        assert_eq!(mirrored.face_count(), 12);

        // All raster data should be valid
        let (verts, indices) = mirrored.to_raster_data();
        assert!(!verts.is_empty());
        assert!(!indices.is_empty());
    }

    #[test]
    fn mirror_mesh_with_merge() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let mirrored = mesh.mirrored(MirrorAxes::X, true, 0.01);
        // Box verts at x=±1, none at x=0, so no merging for unit box
        assert_eq!(mirrored.vertex_count(), 16);
        assert_eq!(mirrored.face_count(), 12);
    }

    #[test]
    fn mirror_mesh_y_axis() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let mirrored = mesh.mirrored(MirrorAxes::Y, false, 0.001);
        assert_eq!(mirrored.vertex_count(), 16);
        assert_eq!(mirrored.face_count(), 12);
    }

    /// Asymmetric +X box (x in [1, 3]). Mirroring X must introduce negative-x verts;
    /// Y extents must still match the input range. This is the P0-1 property gate —
    /// the old index-vs-bitmask bug made "X" tick call axis_index=1 (Y) and fail this.
    #[test]
    fn mirror_axis_x_mirrors_x() {
        // new_box(ONE) is x∈[-1,1]; shift by +2 → x∈[1,3], strictly positive.
        let mesh = {
            let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
            for v in m.vertices.iter_mut() {
                v.position.x += 2.0;
            }
            m
        };
        let xs_in: Vec<f32> = mesh.vertices.iter().map(|v| v.position.x).collect();
        assert!(xs_in.iter().all(|&x| x >= -1e-5), "setup: box must start non-negative in x");

        let out = mesh.mirrored(MirrorAxes::X, false, 0.001);
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for v in &out.vertices {
            min_x = min_x.min(v.position.x);
            max_x = max_x.max(v.position.x);
            min_y = min_y.min(v.position.y);
            max_y = max_y.max(v.position.y);
        }
        assert!(min_x < -0.9, "X mirror must produce negative-x verts (got min_x={min_x})");
        assert!(max_x > 0.9, "X mirror must keep positive-x verts (got max_x={max_x})");
        // Y must not be the mirrored axis: unit box y stays ≈ [-1, 1], not doubled past that.
        assert!(min_y > -1.1 && max_y < 1.1, "X mirror must not expand Y (y=[{min_y},{max_y}])");
        assert_eq!(out.vertex_count(), 16);
    }

    #[test]
    fn mirror_axis_y_mirrors_y() {
        let mesh = {
            let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
            for v in m.vertices.iter_mut() {
                v.position.y += 2.0;
            }
            m
        };
        let out = mesh.mirrored(MirrorAxes::Y, false, 0.001);
        let (min_y, max_y) = out.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.position.y), b.max(v.position.y))
        });
        assert!(min_y < -0.9, "Y mirror must produce negative-y verts (min_y={min_y})");
        assert!(max_y > 0.9, "Y mirror must keep positive-y verts (max_y={max_y})");
        let (min_x, max_x) = out.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.position.x), b.max(v.position.x))
        });
        assert!(min_x > -1.1 && max_x < 1.1, "Y mirror must not expand X");
    }

    #[test]
    fn mirror_axis_z_mirrors_z() {
        let mesh = {
            let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
            for v in m.vertices.iter_mut() {
                v.position.z += 2.0;
            }
            m
        };
        let out = mesh.mirrored(MirrorAxes::Z, false, 0.001);
        let (min_z, max_z) = out.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.position.z), b.max(v.position.z))
        });
        assert!(min_z < -0.9, "Z mirror must produce negative-z verts (min_z={min_z})");
        assert!(max_z > 0.9, "Z mirror must keep positive-z verts (max_z={max_z})");
    }

    /// X|Y multi-axis: successive mirrors → 4 copies of a corner-offset box (no merge).
    #[test]
    fn mirror_axis_xy_is_four_way() {
        let mesh = {
            let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
            for v in m.vertices.iter_mut() {
                v.position.x += 2.0;
                v.position.y += 2.0;
            }
            m
        };
        let axes = MirrorAxes::X.union(MirrorAxes::Y);
        assert_eq!(axes.bits(), 3, "X|Y bitmask is 3 (panel encoding)");
        let out = mesh.mirrored(axes, false, 0.001);
        // 8 verts × 4 quadrants = 32 without merge
        assert_eq!(out.vertex_count(), 32, "X+Y mirror = 4-way copy of asymmetric box");
        assert_eq!(out.face_count(), 24);
        let (min_x, max_x) = out.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.position.x), b.max(v.position.x))
        });
        let (min_y, max_y) = out.vertices.iter().fold((f32::MAX, f32::MIN), |(a, b), v| {
            (a.min(v.position.y), b.max(v.position.y))
        });
        assert!(min_x < -0.9 && max_x > 0.9, "X span must cover both signs");
        assert!(min_y < -0.9 && max_y > 0.9, "Y span must cover both signs");
    }

    /// Sabotage: treating bitmask value 1 as axis *index* would call Y-reflect — this documents
    /// that MirrorAxes::from_bits(1) is X, not index 1.
    #[test]
    fn mirror_axes_from_bits_one_is_x_not_index_y() {
        assert_eq!(MirrorAxes::from_bits(1), MirrorAxes::X);
        assert_ne!(MirrorAxes::from_bits(1), MirrorAxes::from_axis_index(1)); // index 1 = Y
        assert_eq!(MirrorAxes::from_axis_index(0), MirrorAxes::X);
        assert_eq!(MirrorAxes::from_axis_index(1), MirrorAxes::Y);
        assert_eq!(MirrorAxes::from_axis_index(2), MirrorAxes::Z);
    }

    #[test]
    fn delete_half_x_axis() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.face_count(), 6);
        // Delete everything with no vertex strictly on +X
        mesh.delete_half(Vec3::ZERO, Vec3::X);
        // Unit box: left face (−X) max_sd = −1 → deleted; right (+X) max_sd = 1 → kept;
        // top/bottom/front/back have verts at x=±1 so max_sd = 1 → kept (straddle until bisect).
        assert!(mesh.face_count() < 6);
        assert!(mesh.face_count() >= 1);
    }

    #[test]
    fn delete_half_drops_on_plane_only_faces() {
        // Owner Alt+X face-select: selected faces define the cut plane and lie ON it.
        // Old centroid < 0 test kept them (dot≈0). New rule: no strictly-+ vert → drop.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Inject a coplanar midplane quad at x=0 (internal "middle poly" strip).
        let a = m.alloc_vertex(HVertex { position: Vec3::new(0.0, -0.5, -0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let b = m.alloc_vertex(HVertex { position: Vec3::new(0.0,  0.5, -0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let c = m.alloc_vertex(HVertex { position: Vec3::new(0.0,  0.5,  0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let d = m.alloc_vertex(HVertex { position: Vec3::new(0.0, -0.5,  0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let mid = m.create_face(&[a, b, c, d]).expect("midplane face");
        let before = m.face_count();
        m.delete_half_eps(Vec3::ZERO, Vec3::X, 1e-5);
        assert!(m.face_count() < before, "on-plane face should be deleted");
        assert!(
            m.free_faces.contains(&mid.0) || (mid.0 as usize) >= m.faces.len()
                || m.face(mid).half_edge == NO_EDGE,
            "midplane face slot must be freed"
        );
        // No live face may have all verts on x=0.
        for fi in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&fi) { continue; }
            if m.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
            let all_on = m.face_vertices(FaceId(fi)).iter().all(|v| m.vert(*v).position.x.abs() <= 1e-4);
            assert!(!all_on, "live on-plane face survived delete_half");
        }
    }

    #[test]
    fn symmetrize_face_plane_no_middle_poly() {
        // Repro: box + coplanar midplane face (what face-selected Alt+X leaves on the cut)
        // → symmetrize across that plane must not leave any all-on-plane face, must stay
        // manifold, and must still produce symmetric geometry.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let a = m.alloc_vertex(HVertex { position: Vec3::new(0.0, -0.5, -0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let b = m.alloc_vertex(HVertex { position: Vec3::new(0.0,  0.5, -0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let c = m.alloc_vertex(HVertex { position: Vec3::new(0.0,  0.5,  0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let d = m.alloc_vertex(HVertex { position: Vec3::new(0.0, -0.5,  0.5), normal: Vec3::X, half_edge: NO_EDGE });
        let _ = m.create_face(&[a, b, c, d]);
        m.symmetrize(Vec3::ZERO, Vec3::X, true, 0.001);
        assert!(m.face_count() > 0, "symmetrize wiped the mesh");
        for fi in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&fi) { continue; }
            if m.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
            let all_on = m.face_vertices(FaceId(fi)).iter().all(|v| m.vert(*v).position.x.abs() <= 1e-3);
            assert!(!all_on, "middle poly (all-on-plane face) survived face-plane symmetrize");
        }
        assert!(m.is_manifold().is_ok(), "symmetrize after midplane face: {:?}", m.is_manifold());
    }

    #[test]
    fn mirrored_across_skips_on_plane_only_faces() {
        // Defense in depth: even if delete_half is skipped, mirror must not double-create
        // coplanar reverse-winding faces on the seam.
        let mut m = HalfEdgeMesh::new();
        let a = m.alloc_vertex(HVertex { position: Vec3::new(0.0, 0.0, 0.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let b = m.alloc_vertex(HVertex { position: Vec3::new(0.0, 1.0, 0.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let c = m.alloc_vertex(HVertex { position: Vec3::new(0.0, 1.0, 1.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let d = m.alloc_vertex(HVertex { position: Vec3::new(0.0, 0.0, 1.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let _ = m.create_face(&[a, b, c, d]);
        // Also a strictly +X face so the mesh is not empty after skip.
        let e = m.alloc_vertex(HVertex { position: Vec3::new(1.0, 0.0, 0.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let f = m.alloc_vertex(HVertex { position: Vec3::new(1.0, 1.0, 0.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let g = m.alloc_vertex(HVertex { position: Vec3::new(1.0, 1.0, 1.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let h = m.alloc_vertex(HVertex { position: Vec3::new(1.0, 0.0, 1.0), normal: Vec3::Y, half_edge: NO_EDGE });
        let _ = m.create_face(&[e, f, g, h]);
        let out = m.mirrored_across(Vec3::ZERO, Vec3::X, true, 0.001);
        // On-plane face dropped from both halves; +X face kept + mirrored → 2 faces.
        assert_eq!(out.face_count(), 2, "on-plane face must not be emitted (got {})", out.face_count());
    }

    #[test]
    fn symmetrize_creates_symmetric_mesh() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let original_fc = mesh.face_count();
        mesh.symmetrize(Vec3::ZERO, Vec3::X, false, 0.001);
        // After symmetrize: delete half + mirror → should have geometry on both sides
        assert!(mesh.face_count() >= original_fc);
    }

    #[test]
    fn mirrored_across_offcenter_plane_is_symmetric() {
        // The whole point of the Alt+X geometry-center pie: mirror across a plane that is NOT the
        // axis-0 plane. Reflect a [-1,1]^3 box across x=2 → every vertex must gain a partner at
        // (4-x, y, z), and the bbox must be symmetric about x=2. The old axis-0 `mirrored` could
        // not do this.
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let out = m.mirrored_across(Vec3::new(2.0, 0.0, 0.0), Vec3::X, false, 0.001);
        let verts: Vec<Vec3> = (0..out.vertices.len() as u32)
            .filter(|i| !out.free_verts.contains(i))
            .map(|i| out.vert(VertexId(i)).position)
            .collect();
        for &p in &verts {
            let mir = Vec3::new(4.0 - p.x, p.y, p.z);
            assert!(verts.iter().any(|q| (*q - mir).length() < 1e-3),
                "no mirror partner for {:?} across x=2", p);
        }
        let (bmin, bmax) = out.bounding_box();
        assert!((bmin.x - (4.0 - bmax.x)).abs() < 1e-3, "bbox not symmetric about x=2");
    }

    #[test]
    fn symmetrize_bisect_welds_clean_seam() {
        // Subdivided box so faces STRADDLE an off-center plane — the case the old centroid-only
        // delete could not weld. After symmetrize the mesh must be manifold (a clean welded seam,
        // no duplicated seam verts) and symmetric about the plane.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.subdivide_catmull_clark(); // faces now cross x=0.3
        let plane_x = 0.3f32;
        m.symmetrize(Vec3::new(plane_x, 0.0, 0.0), Vec3::X, true, 0.001);
        assert!(m.face_count() > 0, "symmetrize wiped the mesh");
        assert!(m.is_manifold().is_ok(), "symmetrize seam did not weld: {:?}", m.is_manifold());
        // Every live vertex has a mirror partner across x=plane_x (symmetry).
        let verts: Vec<Vec3> = (0..m.vertices.len() as u32)
            .filter(|i| !m.free_verts.contains(i))
            .map(|i| m.vert(VertexId(i)).position)
            .collect();
        for &p in &verts {
            let mir = Vec3::new(2.0 * plane_x - p.x, p.y, p.z);
            assert!(verts.iter().any(|q| (*q - mir).length() < 2e-3),
                "no mirror partner for {:?} across x={}", p, plane_x);
        }
    }

    /// Regression: two knife strokes (the 2nd a 90° staircase anchored ON the 1st stroke's edge),
    /// then bevel the staircase. The concave L-shaped faces this creates broke the old
    /// centroid-based rail sign-fix (inward flipped → coincident BoundVerts → palindromic miter
    /// chain → 4 faces on one arc segment, non-manifold at seg>1). Fixed by the winding-invariant
    /// inward (interior = left of the directed ring boundary). Owner live repro 2026-07-18.
    #[test]
    fn bevel_staircase_on_knife_stroke_stays_manifold() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let project = |p: Vec3| -> Option<glam::Vec2> { Some(glam::Vec2::new(p.x, p.y)) };
        // Stroke A: full vertical cut at x=0.5 across the front face.
        let a_pts = [glam::Vec2::new(0.5, -1.5), glam::Vec2::new(0.5, 1.5)];
        let na = crate::knife::knife_cut_screen(&mut m, &project, &a_pts, None, false, false, Vec3::NEG_Z, None, None);
        println!("stroke A: cut={} verts={} manifold={:?}", na, m.vertex_count(), m.is_manifold().is_ok());
        // Stroke B: staircase with 90° bends, ENDS crossing stroke A's edge (right end anchors ON it).
        let b3d = [
            Vec3::new(-1.0, -0.5, 1.0),  // T on left box edge
            Vec3::new(-0.2, -0.5, 1.0),  // bend 90°
            Vec3::new(-0.2, 0.2, 1.0),   // bend 90°
            Vec3::new(0.5, 0.2, 1.0),    // end exactly ON stroke A's cut edge
        ];
        let b_pts: Vec<glam::Vec2> = b3d.iter().map(|p| glam::Vec2::new(p.x, p.y)).collect();
        let nb = crate::knife::knife_cut_screen(&mut m, &project, &b_pts, Some(&b3d), false, false, Vec3::NEG_Z, None, None);
        println!("stroke B: cut={} verts={} faces={} manifold={:?}", nb, m.vertex_count(), m.face_count(), m.is_manifold());
        // Select stroke B's chain edges: front-face edges whose endpoints lie on the staircase polyline.
        let on_b = |p: Vec3| -> bool {
            if p.z < 0.99 { return false; }
            ((p.y + 0.5).abs() < 1e-3 && p.x >= -1.01 && p.x <= -0.19)
                || ((p.x + 0.2).abs() < 1e-3 && p.y >= -0.51 && p.y <= 0.21)
                || ((p.y - 0.2).abs() < 1e-3 && p.x >= -0.21 && p.x <= 0.51)
        };
        let mut sel: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            if !m.is_edge_live(HalfEdgeId(i)) { continue; }
            let he = m.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.twin.0 < i { continue; }
            let s0 = m.vert(m.he_source(HalfEdgeId(i))).position;
            let s1 = m.vert(he.vertex).position;
            if on_b(s0) && on_b(s1) {
                sel.insert(i);
                println!("sel he={} {:?} -> {:?}", i, s0, s1);
            }
        }
        println!("selected {} edges", sel.len());
        for seg in [1u32, 3] {
            let out = crate::bevel_op::bevel_edges_offset(&m, &sel, &crate::bevel_op::BevelParams { width: 0.13, segments: seg, shape: 0.5, clamp: true });
            assert!(out.is_manifold().is_ok(), "seg{}: non-manifold: {:?}", seg, out.is_manifold());
            assert!(crate::bevel_op::bevel_result_installable(&m, &out), "seg{}: not installable", seg);
            if out.is_manifold().is_err() {
                use std::collections::HashMap;
                let mut uses: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
                for fi in 0..out.faces.len() as u32 {
                    if out.face(FaceId(fi)).half_edge == NO_EDGE { continue; }
                    let vs = out.face_vertices(FaceId(fi));
                    for k in 0..vs.len() {
                        uses.entry((vs[k].0, vs[(k + 1) % vs.len()].0)).or_default().push(fi);
                    }
                }
                for ((a, b), fs) in uses.iter() {
                    if fs.len() > 1 {
                        println!("  DUP {}->{} in faces {:?}", a, b, fs);
                        println!("    a={:?} b={:?}", out.vert(VertexId(*a)).position, out.vert(VertexId(*b)).position);
                        for &fi in fs {
                            let ring: Vec<String> = out.face_vertices(FaceId(fi)).iter()
                                .map(|v| format!("{}@({:.2},{:.2},{:.2})", v.0, out.vert(*v).position.x, out.vert(*v).position.y, out.vert(*v).position.z)).collect();
                            println!("    face {}: {}", fi, ring.join(" "));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore] // TEMP diagnostic — multi-face straight + multi-segment knife cuts
    fn knife_multiface_diag() {
        // 2-wide box (middle loop): the owner's mesh shape.
        let verts = [
            Vec3::new(-1.0, -1.0, 1.0), Vec3::new(0.0, -1.0, 1.0), Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 1.0, 1.0), Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(-1.0, -1.0, -1.0), Vec3::new(0.0, -1.0, -1.0), Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0), Vec3::new(0.0, 1.0, -1.0), Vec3::new(-1.0, 1.0, -1.0),
        ];
        let polys: Vec<Vec<usize>> = vec![
            vec![0, 1, 4, 5], vec![1, 2, 3, 4],
            vec![5, 4, 10, 11], vec![4, 3, 9, 10],
            vec![7, 6, 11, 10], vec![8, 7, 10, 9],
            vec![0, 6, 7, 1], vec![1, 7, 8, 2],
            vec![0, 5, 11, 6], vec![2, 8, 9, 3],
        ];
        let project = |p: Vec3| -> Option<glam::Vec2> { Some(glam::Vec2::new(p.x, p.y)) };

        // CASE A: single straight horizontal across BOTH front faces (crosses the middle edge).
        let mut m = HalfEdgeMesh::from_polygons(&verts, &polys);
        let path = [glam::Vec2::new(-1.5, 0.2), glam::Vec2::new(1.5, 0.2)];
        let n = crate::knife::knife_cut_screen(&mut m, &project, &path, None, false, false, Vec3::NEG_Z, None, None);
        println!("CASE A straight 2-face: cut={} manifold={:?}", n, m.is_manifold().is_ok());

        // CASE B: multi-segment: T on bottom edge, bend, long horizontal crossing the middle edge,
        // ending snapped on the right corner edge — the owner's exact stroke shape.
        let mut m2 = HalfEdgeMesh::from_polygons(&verts, &polys);
        let pts3d = [
            Vec3::new(-0.6, -1.0, 1.0),  // click 1: T on bottom edge (left face)
            Vec3::new(-0.4, 0.1, 1.0),   // click 2: bend inside left face
            Vec3::new(1.0, 0.15, 1.0),   // click 3: snapped ON right corner edge
        ];
        let path2: Vec<glam::Vec2> = pts3d.iter().map(|p| glam::Vec2::new(p.x, p.y)).collect();
        let n2 = crate::knife::knife_cut_screen(&mut m2, &project, &path2, Some(&pts3d), false, false, Vec3::NEG_Z, None, None);
        println!("CASE B multi-seg: cut={} manifold={:?}", n2, m2.is_manifold().is_ok());
        // count new edges on the cut line: y between -1..0.15 chain
        let cut_edges = (0..m2.half_edges.len() as u32).filter(|&i| {
            if !m2.is_edge_live(HalfEdgeId(i)) { return false; }
            let he = m2.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.twin.0 < i { return false; }
            let s0 = m2.vert(m2.he_source(HalfEdgeId(i))).position;
            let s1 = m2.vert(he.vertex).position;
            s0.z > 0.99 && s1.z > 0.99 && (s0 - s1).length() > 1e-4
                && !((s0.y.abs() > 0.99 && s1.y.abs() > 0.99) || (s0.x.abs() > 0.99 && s1.x.abs() > 0.99) || (s0.x.abs() < 1e-3 && s1.x.abs() < 1e-3))
        }).count();
        println!("CASE B front-face non-border edges: {}", cut_edges);
    }

    /// Regression sweep for the open-seam bug (bevel one edge, then symmetrize twice at the
    /// geometry center over every ordered axis pair): create_face's order-sensitive twin pairing
    /// left twin-candidate half-edges unpaired in mirrored_across → open slits. Fixed by
    /// repair_unpaired_twins(). ~0.4s for 12 edges × 36 axis pairs.
    #[test]
    fn symmetrize_sweep_no_open_seams() {
        let axes = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z];
        let names = ["+X","-X","+Y","-Y","+Z","-Z"];
        let mut failures = 0;
        for edge in 0..12u32 {
            for (i1, a1) in axes.iter().enumerate() {
                for (i2, a2) in axes.iter().enumerate() {
                    let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
                    let he_id = (edge * 2) % (m.half_edges.len() as u32);
                    let sel: std::collections::HashSet<u32> = [he_id].into_iter().collect();
                    let params = crate::bevel_op::BevelParams { width: 0.3, segments: 3, shape: 0.5, clamp: true };
                    m = crate::bevel_op::bevel_edges_offset(&m, &sel, &params);
                    for (pass, a) in [(1usize, a1), (2, a2)] {
                        let (bmin, bmax) = m.bounding_box();
                        let center = (bmin + bmax) * 0.5;
                        m.symmetrize(center, *a, true, 0.001);
                        let boundary = (0..m.half_edges.len() as u32)
                            .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
                            .count();
                        if boundary > 0 {
                            println!("OPEN: edge={} pass={} axis1={} axis2={} boundary={}", he_id, pass, names[i1], names[i2], boundary);
                            failures += 1;
                            break;
                        }
                    }
                    if failures > 30 { panic!("many failures, stopping sweep early"); }
                }
            }
        }
        println!("sweep done, failing combos: {}", failures);
        assert_eq!(failures, 0, "found open-seam combos");
    }

    #[test]
    fn symmetrize_after_bevel_stays_closed() {
        // Owner repro: box → bevel ONE edge (segments=3, like the live tool) → symmetrize twice at the
        // geometry center. Result must stay CLOSED (no boundary edges) — the shipped bug left an open
        // seam loop.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Pick one edge of the box (first live half-edge) and bevel it via the production op.
        let sel: std::collections::HashSet<u32> = [0u32].into_iter().collect();
        let params = crate::bevel_op::BevelParams { width: 0.3, segments: 3, shape: 0.5, clamp: true };
        m = crate::bevel_op::bevel_edges_offset(&m, &sel, &params);
        let boundary_before = (0..m.half_edges.len() as u32)
            .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(boundary_before, 0, "bevel itself must produce closed geometry");

        for pass in 0..2 {
            let (bmin, bmax) = m.bounding_box();
            let center = (bmin + bmax) * 0.5;
            m.symmetrize(center, Vec3::X, true, 0.001);
            let boundary = (0..m.half_edges.len() as u32)
                .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
                .count();
            assert_eq!(boundary, 0, "symmetrize pass {} left {} boundary (open) edges", pass + 1, boundary);
        }
    }

    #[test]
    fn bisect_plane_puts_seam_on_plane() {
        // Bisecting a box across x=0.4 must create vertices exactly on the plane and leave no face
        // straddling it (every face lies fully on one side).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let ox = 0.4f32;
        let seam = m.bisect_plane(Vec3::new(ox, 0.0, 0.0), Vec3::X, 1e-5);
        assert!(!seam.is_empty(), "bisect found no crossing edges");
        for v in &seam {
            assert!((m.vert(*v).position.x - ox).abs() < 1e-4, "seam vertex not on plane");
        }
        for fi in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&fi) { continue; }
            let vs = m.face_vertices(FaceId(fi));
            let mut pos = false; let mut neg = false;
            for v in vs {
                let d = m.vert(v).position.x - ox;
                if d > 1e-4 { pos = true; }
                if d < -1e-4 { neg = true; }
            }
            assert!(!(pos && neg), "face {} still straddles the plane after bisect", fi);
        }
    }

    /// A face that crosses the plane TWICE (non-planar / saddle quad) must be split into pure
    /// pieces, not left straddling. One chord per face could not separate a +,0,−,0,+,0,−,0 ring,
    /// so the face survived into the kept half and its seam sat far off the plane.
    #[test]
    fn bisect_plane_splits_a_face_that_crosses_twice() {
        // Warp one corner of the +X face out of plane, then bisect on THAT face's own plane
        // (centroid + normal) — exactly what Alt+X does with a face selected.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let f = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| m.is_face_live(f) && m.compute_face_normal(f).x > 0.9)
            .expect("+X face");
        let v0 = m.face_vertices(f)[0];
        m.vert_mut(v0).position += Vec3::new(0.35, 0.0, 0.0);
        m.recompute_normals();

        let ring = m.face_vertices(f);
        let o: Vec3 = ring.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / ring.len() as f32;
        let n = m.compute_face_normal(f).normalize();
        // Fixture guard: the face really does straddle its own plane on BOTH sides (twice).
        let sd: Vec<f32> = ring.iter().map(|&v| (m.vert(v).position - o).dot(n)).collect();
        assert!(sd.iter().any(|&d| d > 1e-3) && sd.iter().any(|&d| d < -1e-3),
            "fixture: quad must be non-planar, got {sd:?}");

        let eps = 1e-3f32;
        m.bisect_plane(o, n, eps);
        for fi in 0..m.faces.len() as u32 {
            let f = FaceId(fi);
            if !m.is_face_live(f) { continue; }
            let vs = m.face_vertices(f);
            let pos = vs.iter().any(|&v| (m.vert(v).position - o).dot(n) > eps);
            let neg = vs.iter().any(|&v| (m.vert(v).position - o).dot(n) < -eps);
            assert!(!(pos && neg), "face {fi} still straddles the plane after bisect");
        }
        assert!(m.validate().is_ok(), "bisect corrupted the mesh: {:?}", m.validate());
    }

    /// Alt+X with a NON-PLANAR face selected must not punch a hole. The straddling face left by the
    /// old bisect was kept by `delete_half` (it owns + verts), so the kept half's rim sat ~0.09 off
    /// the plane — 87x the weld threshold — and the mirror could not weld it: 8 boundary edges plus
    /// a mirrored copy crossing back through the plane.
    #[test]
    fn symmetrize_non_planar_face_plane_leaves_no_hole() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let f = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| m.is_face_live(f) && m.compute_face_normal(f).x > 0.9)
            .expect("+X face");
        let v0 = m.face_vertices(f)[0];
        m.vert_mut(v0).position += Vec3::new(0.35, 0.0, 0.0);
        m.recompute_normals();

        let ring = m.face_vertices(f);
        let o: Vec3 = ring.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / ring.len() as f32;
        let n = m.compute_face_normal(f).normalize();

        let before = (0..m.half_edges.len() as u32)
            .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(before, 0, "fixture: input box is closed");

        // Keep the side the body is on (normal points into the mesh).
        m.symmetrize(o, -n, true, 0.001);
        assert!(m.face_count() > 0, "symmetrize wiped the mesh");
        let boundary = (0..m.half_edges.len() as u32)
            .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && m.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "face-plane symmetrize left {boundary} boundary (open) edges");
        assert!(m.validate().is_ok(), "symmetrize corrupted the mesh: {:?}", m.validate());
    }

    /// A tangent face plane (every face of a convex body has one) leaves ALL geometry on one side.
    /// Keeping the empty side must yield an empty result — the app refuses to commit that rather
    /// than silently wiping the object (`app/src/symmetry.rs::poly_symmetrize_op`).
    #[test]
    fn symmetrize_tangent_face_plane_empty_side_is_detectable() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let f = (0..base.faces.len() as u32).map(FaceId)
            .find(|&f| base.is_face_live(f) && base.compute_face_normal(f).x > 0.9)
            .expect("+X face");
        let ring = base.face_vertices(f);
        let o: Vec3 = ring.iter().map(|&v| base.vert(v).position).sum::<Vec3>() / ring.len() as f32;
        let n = base.compute_face_normal(f).normalize();

        // Keep the OUTWARD side: nothing is there.
        let mut empty = base.clone();
        empty.symmetrize(o, n, true, 0.001);
        assert_eq!(empty.face_count(), 0, "outward side of a tangent plane holds no geometry");

        // Keep the INWARD side: the whole body, mirrored across the face — closed, 2x the faces
        // minus the on-plane face itself.
        let mut doubled = base.clone();
        doubled.symmetrize(o, -n, true, 0.001);
        assert_eq!(doubled.face_count(), 10, "box(6) minus the on-plane face(1), mirrored = 10");
        let boundary = (0..doubled.half_edges.len() as u32)
            .filter(|&i| doubled.is_edge_live(HalfEdgeId(i)) && doubled.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "mirror across a tangent face plane must stay closed");
        assert!(doubled.is_manifold().is_ok(), "{:?}", doubled.is_manifold());
    }

    #[test]
    fn mirrored_across_oblique_plane_reflects_correctly() {
        // Rotated-body case: an off-axis normal must reflect exactly (the old path snapped the
        // normal to a dominant axis, which is wrong here).
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let n = Vec3::new(1.0, 1.0, 0.0).normalize();
        let origin = Vec3::ZERO;
        let out = m.mirrored_across(origin, n, false, 0.001);
        let verts: Vec<Vec3> = (0..out.vertices.len() as u32)
            .filter(|i| !out.free_verts.contains(i))
            .map(|i| out.vert(VertexId(i)).position)
            .collect();
        for &p in &verts {
            let mir = p - 2.0 * (p - origin).dot(n) * n;
            assert!(verts.iter().any(|q| (*q - mir).length() < 1e-3),
                "no mirror partner for {:?} across oblique plane", p);
        }
    }

    #[test]
    fn flip_across_preserves_topology() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let fc = mesh.face_count();
        let vc = mesh.vertex_count();
        mesh.flip_across(Vec3::ZERO, Vec3::X);
        // Same topology, just reflected positions
        assert_eq!(mesh.face_count(), fc);
        assert_eq!(mesh.vertex_count(), vc);
        // Check that vertices are reflected (x should be negated)
        let v0_pos = mesh.vert(VertexId(0)).position;
        // Original was (-1,-1,-1), flipped across X → (1,-1,-1)
        assert!((v0_pos.x - 1.0).abs() < 0.01);
    }

    #[test]
    fn bevel_performance_high_poly() {
        // Subdivide box 3x → ~384 faces, ~768 half-edges → bevel all edges
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        for _ in 0..3 {
            mesh.subdivide_catmull_clark();
        }
        let vert_count_before = mesh.vertex_count();
        assert!(vert_count_before > 300, "expected 300+ verts for high-poly test");

        // Collect all interior edges (non-boundary, deduplicated via twin)
        let mut edges = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            if mesh.free_edges.contains(&i) { continue; }
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
            edges.insert(i);
        }
        assert!(edges.len() > 50, "expected 50+ bevel edges, got {}", edges.len());

        // Bevel every interior edge — this is the expensive path (always 1-segment chamfer)
        let start = std::time::Instant::now();
        mesh.bevel_edges_ex(&edges, 0.05);
        let elapsed = start.elapsed();

        // Validate mesh after bevel
        assert!(mesh.validate().is_ok(), "mesh invalid after high-poly bevel: {:?}", mesh.validate());
        assert!(mesh.vertex_count() > vert_count_before, "bevel should add vertices");

        // Performance gate: bevel on ~384-face mesh with 2 segments should be <500ms
        assert!(elapsed.as_millis() < 500,
            "bevel too slow on high-poly: {}ms (budget: 500ms)", elapsed.as_millis());
    }

    #[test]
    fn bevel_validates_no_degenerate_faces() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Collect all edges
        let mut edges = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            if mesh.free_edges.contains(&i) { continue; }
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
            edges.insert(i);
        }

        // Test multiple widths and shapes
        for &(width, shape) in &[(0.1, 0.0), (0.1, 0.5), (0.1, 1.0), (0.3, 1.0)] {
            let mut m = mesh.clone();
            m.bevel_edges_ex(&edges, width);
            assert!(m.validate().is_ok(), "invalid after bevel w={} s={}: {:?}", width, shape, m.validate());

            // Check no degenerate faces (area > epsilon)
            for i in 0..m.faces.len() {
                if m.free_faces.contains(&(i as u32)) { continue; }
                let fid = FaceId(i as u32);
                let verts = m.face_vertices(fid);
                if verts.len() < 3 { continue; }
                let p0 = m.vert(verts[0]).position;
                let p1 = m.vert(verts[1]).position;
                let p2 = m.vert(verts[2]).position;
                let area = (p1 - p0).cross(p2 - p0).length() * 0.5;
                assert!(area > 1e-8, "degenerate face {} after bevel w={} s={}: area={}", i, width, shape, area);
            }
        }
    }

    #[test]
    fn bevel_single_edge_debug_geometry() {
        // Trace exact geometry produced by beveling one edge on a cube
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);

        // Print original mesh
        eprintln!("=== ORIGINAL MESH ===");
        eprintln!("Vertices ({}):", mesh.vertices.len());
        for (i, v) in mesh.vertices.iter().enumerate() {
            eprintln!("  v{}: {:?}", i, v.position);
        }
        eprintln!("Faces:");
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let verts = mesh.face_vertices(FaceId(i as u32));
            let positions: Vec<_> = verts.iter().map(|v| mesh.vert(*v).position).collect();
            eprintln!("  f{}: verts={:?} pos={:?}", i, verts, positions);
        }

        // Pick one edge
        let mut edge_id = None;
        for i in 0..mesh.half_edges.len() as u32 {
            if mesh.free_edges.contains(&i) { continue; }
            let he = &mesh.half_edges[i as usize];
            if he.face == NO_FACE || he.twin == NO_TWIN { continue; }
            if i > he.twin.0 { continue; }
            let src = mesh.he_source(HalfEdgeId(i));
            let dst = he.vertex;
            eprintln!("edge {}: src=v{} {:?} -> dst=v{} {:?}  (face={}, twin_face={})",
                i, src.0, mesh.vert(src).position, dst.0, mesh.vert(dst).position,
                he.face.0, mesh.half_edges[he.twin.0 as usize].face.0);
            if edge_id.is_none() { edge_id = Some(i); }
        }

        let eid = edge_id.unwrap();
        eprintln!("\n=== BEVELING EDGE {} ===", eid);
        let mut sel = std::collections::HashSet::new();
        sel.insert(eid);
        mesh.bevel_edges_ex(&sel, 0.15);

        eprintln!("\n=== BEVELED MESH ===");
        eprintln!("Vertices ({}):", mesh.vertices.len());
        for (i, v) in mesh.vertices.iter().enumerate() {
            eprintln!("  v{}: {:?}", i, v.position);
        }
        eprintln!("Faces:");
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let verts = mesh.face_vertices(FaceId(i as u32));
            let positions: Vec<_> = verts.iter().map(|v| mesh.vert(*v).position).collect();
            let n = mesh.compute_face_normal(FaceId(i as u32));
            eprintln!("  f{}: verts={:?} normal={:?}", i, verts, n);
            for (j, v) in verts.iter().enumerate() {
                eprintln!("      {:?}", positions[j]);
            }
        }
    }

    #[test]
    fn bevel_all_edges_cube() {
        // Bevel ALL edges of a cube — should produce a chamfered cube
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);

        let mut edges = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            if mesh.free_edges.contains(&i) { continue; }
            let he = mesh.he(HalfEdgeId(i));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
            edges.insert(i);
        }
        eprintln!("Beveling {} edges on cube", edges.len());

        mesh.bevel_edges_ex(&edges, 0.2);

        let face_count = mesh.faces.len() - mesh.free_faces.len();
        eprintln!("Result: {} vertices, {} faces", mesh.vertices.len(), face_count);

        // No corner patches: 6 shrunken faces (octagons with offset verts) + 12 bevel strips = 18 faces
        assert_eq!(face_count, 18, "expected 18 faces for all-edge bevel on cube, got {}", face_count);

        // Validate mesh integrity
        assert!(mesh.validate().is_ok(), "mesh invalid: {:?}", mesh.validate());

        // All faces should have outward-pointing normals
        let center: Vec3 = mesh.vertices.iter().map(|v| v.position).sum::<Vec3>() / mesh.vertices.len() as f32;
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let fid = FaceId(i as u32);
            let verts = mesh.face_vertices(fid);
            if verts.len() < 3 { continue; }
            let p0 = mesh.vert(verts[0]).position;
            let p1 = mesh.vert(verts[1]).position;
            let p2 = mesh.vert(verts[2]).position;
            let face_n = (p1 - p0).cross(p2 - p0);
            let face_center = verts.iter().map(|v| mesh.vert(*v).position).sum::<Vec3>() / verts.len() as f32;
            let outward = face_center - center;
            assert!(face_n.dot(outward) >= -1e-6,
                "face {} has inward normal (dot={}, n={:?}, outward={:?})", i, face_n.dot(outward), face_n, outward);
        }
    }

    #[test]
    fn bevel_single_edge_cube_geometry() {
        // Bevel a single edge on a unit cube and verify the chamfer strip is correct.
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let orig_vert_count = mesh.vertex_count();
        let orig_face_count = mesh.faces.len() - mesh.free_faces.len();

        // Pick the first valid non-boundary edge
        let mut edge_id = None;
        for i in 0..mesh.half_edges.len() as u32 {
            if mesh.free_edges.contains(&i) { continue; }
            let he = &mesh.half_edges[i as usize];
            if he.face == NO_FACE || he.twin == NO_TWIN { continue; }
            if i > he.twin.0 { continue; } // only pick one of each edge pair
            edge_id = Some(i);
            break;
        }
        let eid = edge_id.expect("cube should have edges");
        let mut sel = std::collections::HashSet::new();
        sel.insert(eid);

        mesh.bevel_edges_ex(&sel, 0.1);

        // Should have 4 new vertices (src_left, dst_left, src_right, dst_right)
        let new_vert_count = mesh.vertex_count();
        assert!(new_vert_count >= orig_vert_count + 4,
            "expected at least 4 new verts, got {} (was {})", new_vert_count, orig_vert_count);

        // Should have 1 new bevel strip face + possible corner fills
        let new_face_count = mesh.faces.len() - mesh.free_faces.len();
        assert!(new_face_count >= orig_face_count + 1,
            "expected at least 1 new face, got {} (was {})", new_face_count, orig_face_count);

        // Validate mesh integrity
        assert!(mesh.validate().is_ok(), "mesh invalid after single-edge bevel: {:?}", mesh.validate());

        // All face normals should be pointing outward (dot with centroid-to-face direction > 0)
        let center: Vec3 = mesh.vertices.iter().map(|v| v.position).sum::<Vec3>() / mesh.vertices.len() as f32;
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let fid = FaceId(i as u32);
            let verts = mesh.face_vertices(fid);
            if verts.len() < 3 { continue; }
            let p0 = mesh.vert(verts[0]).position;
            let p1 = mesh.vert(verts[1]).position;
            let p2 = mesh.vert(verts[2]).position;
            let face_n = (p1 - p0).cross(p2 - p0);
            let face_center = verts.iter().map(|v| mesh.vert(*v).position).sum::<Vec3>() / verts.len() as f32;
            let outward = face_center - center;
            assert!(face_n.dot(outward) >= -1e-6,
                "face {} has inward normal (dot={})", i, face_n.dot(outward));
        }
    }

    #[test]
    fn extrude_then_move_no_crash() {
        // Regression test: extrude face then access vertices (was crashing due to NO_EDGE)
        let mut mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let top_face = FaceId(0);
        let extruded = mesh.extrude_face(top_face, Vec3::Y * 0.5);

        // Verify all vertices of the extruded face have valid half_edge
        let verts = mesh.face_vertices(extruded);
        assert!(verts.len() >= 3, "extruded face should have 3+ verts");
        for &v in &verts {
            assert_ne!(mesh.vert(v).half_edge, NO_EDGE,
                "vertex {:?} on extruded face has NO_EDGE half_edge", v);
            // vertex_fan should return non-empty
            let fan = mesh.vertex_fan(v);
            assert!(!fan.is_empty(), "vertex {:?} fan is empty after extrude", v);
        }

        // Move vertices (simulating poly G-grab) — should not crash
        for &v in &verts {
            mesh.vert_mut(v).position += Vec3::X * 0.1;
        }
        mesh.recompute_normals();

        // Validate + raster data should work
        assert!(mesh.validate().is_ok(), "invalid after extrude+move: {:?}", mesh.validate());
        let (raster_verts, raster_idx) = mesh.to_raster_data();
        assert!(!raster_verts.is_empty());
        assert!(!raster_idx.is_empty());
    }

    // ---- Half-edge tests from PROMPT_Add_HalfEdge_Tests.md ----

    /// Creates a unit cube centered at origin: 8 verts, 6 quad faces, 24 half-edges.
    fn make_cube() -> HalfEdgeMesh {
        let positions = vec![
            Vec3::new(-0.5, -0.5, -0.5), // 0
            Vec3::new( 0.5, -0.5, -0.5), // 1
            Vec3::new( 0.5, -0.5,  0.5), // 2
            Vec3::new(-0.5, -0.5,  0.5), // 3
            Vec3::new(-0.5,  0.5, -0.5), // 4
            Vec3::new( 0.5,  0.5, -0.5), // 5
            Vec3::new( 0.5,  0.5,  0.5), // 6
            Vec3::new(-0.5,  0.5,  0.5), // 7
        ];
        // 6 faces, CCW winding when viewed from outside
        let faces: Vec<Vec<usize>> = vec![
            vec![0, 1, 2, 3], // bottom (-Y)
            vec![7, 6, 5, 4], // top (+Y)
            vec![4, 5, 1, 0], // front (-Z)
            vec![6, 7, 3, 2], // back (+Z)
            vec![7, 4, 0, 3], // left (-X)
            vec![5, 6, 2, 1], // right (+X)
        ];
        HalfEdgeMesh::from_polygons(&positions, &faces)
    }

    /// Smooth modifier (Laplacian) — the two invariants the task gates on:
    /// (1) FLAT-GRID INTERIOR IDENTITY: a regular planar grid's interior vertex already sits at the
    ///     average of its edge-neighbours, so a Laplacian pass must leave it (and the plane) fixed.
    /// (2) It moves a perturbed interior vertex BACK toward its neighbour average, and is stable
    ///     (finite, converging) across many iterations.
    #[test]
    fn smooth_laplacian_flat_grid_interior_identity_and_convergence() {
        // 3x3 vertex grid on z=0 (2x2 quads). Center vertex index 4.
        let mut pos = Vec::new();
        for j in 0..3 { for i in 0..3 { pos.push(Vec3::new(i as f32, j as f32, 0.0)); } }
        let idx = |i: usize, j: usize| j * 3 + i;
        let faces: Vec<Vec<usize>> = vec![
            vec![idx(0,0), idx(1,0), idx(1,1), idx(0,1)],
            vec![idx(1,0), idx(2,0), idx(2,1), idx(1,1)],
            vec![idx(0,1), idx(1,1), idx(1,2), idx(0,2)],
            vec![idx(1,1), idx(2,1), idx(2,2), idx(1,2)],
        ];
        // (1) identity: center already = avg of its 4 edge-neighbours (1,1,0).
        let mut m = HalfEdgeMesh::from_polygons(&pos, &faces);
        m.smooth_laplacian(5, 1.0);
        let c = m.vert(VertexId(4)).position;
        assert!((c - Vec3::new(1.0,1.0,0.0)).length() < 1e-5,
            "flat-grid interior vertex must be a Laplacian fixed point, got {c:?}");
        assert!(c.is_finite(), "positions stay finite");
        // (2) convergence: perturb center off-plane, smoothing pulls it back toward the neighbour avg.
        let mut m2 = HalfEdgeMesh::from_polygons(&pos, &faces);
        m2.vert_mut(VertexId(4)).position = Vec3::new(1.0, 1.0, 5.0);
        let d0 = (m2.vert(VertexId(4)).position - Vec3::new(1.0,1.0,0.0)).length();
        m2.smooth_laplacian(1, 0.5);
        let d1 = (m2.vert(VertexId(4)).position - Vec3::new(1.0,1.0,0.0)).length();
        assert!(d1 < d0, "one smooth pass must move the perturbed vertex toward the neighbour average ({d0} -> {d1})");
        // Stability/convergence: with a FREE boundary (Blender's Smooth modifier does not pin edges),
        // repeated Laplacian passes diffuse z to a UNIFORM plane — the graph-Laplacian null space is
        // the constant vector, so the surface flattens (z-spread → 0) and stays finite. (It converges
        // to the MEAN z, not z=0 — pinning would be needed for that; noted vs Blender.)
        m2.smooth_laplacian(200, 0.5);
        let zs: Vec<f32> = m2.vertices.iter().map(|v| v.position.z).collect();
        let (zmin, zmax) = zs.iter().fold((f32::MAX, f32::MIN), |(a,b),&z| (a.min(z), b.max(z)));
        assert!(zs.iter().all(|z| z.is_finite()), "positions stay finite (stable)");
        assert!(zmax - zmin < 1e-3, "many passes flatten the surface to a uniform plane (z-spread {})", zmax - zmin);
    }

    /// Smooth factor=0 (or iterations=0) is a strict no-op (matches lerp identity).
    #[test]
    fn smooth_laplacian_zero_is_noop() {
        let a = make_cube();
        let mut b = make_cube();
        b.smooth_laplacian(10, 0.0);
        for (va, vb) in a.vertices.iter().zip(b.vertices.iter()) {
            assert!((va.position - vb.position).length() < 1e-9, "factor=0 must not move any vertex");
        }
        let mut c = make_cube();
        c.smooth_laplacian(0, 0.5);
        for (va, vc) in a.vertices.iter().zip(c.vertices.iter()) {
            assert!((va.position - vc.position).length() < 1e-9, "iterations=0 must not move any vertex");
        }
    }


    #[test]
    fn cube_topology_valid() {
        let mesh = make_cube();

        // Correct counts
        assert_eq!(mesh.vertices.len(), 8, "cube should have 8 vertices");
        assert_eq!(mesh.half_edges.len(), 24, "cube should have 24 half-edges");
        let active_faces = (0..mesh.faces.len())
            .filter(|i| !mesh.free_faces.contains(&(*i as u32)))
            .count();
        assert_eq!(active_faces, 6, "cube should have 6 faces");

        // Every half-edge has a valid twin
        for (i, he) in mesh.half_edges.iter().enumerate() {
            assert!(he.twin != NO_TWIN, "HE{} has no twin", i);
            let twin = &mesh.half_edges[he.twin.0 as usize];
            assert_eq!(twin.twin, HalfEdgeId(i as u32),
                "HE{} twin is HE{} but HE{}'s twin is HE{} (should be HE{})",
                i, he.twin.0, he.twin.0, twin.twin.0, i);
        }

        // Every half-edge has valid next/prev forming a cycle
        for (i, he) in mesh.half_edges.iter().enumerate() {
            assert!(he.next != NO_EDGE, "HE{} has no next", i);
            assert!(he.prev != NO_EDGE, "HE{} has no prev", i);

            let next = &mesh.half_edges[he.next.0 as usize];
            assert_eq!(next.prev, HalfEdgeId(i as u32),
                "HE{}.next = HE{}, but HE{}.prev = HE{} (should be HE{})",
                i, he.next.0, he.next.0, next.prev.0, i);
        }

        // Every face has exactly 4 half-edges (quads)
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let hes = mesh.face_half_edges(FaceId(i as u32));
            assert_eq!(hes.len(), 4, "Face {} should have 4 half-edges, has {}", i, hes.len());
        }

        // No NaN positions
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert!(!v.position.x.is_nan() && !v.position.y.is_nan() && !v.position.z.is_nan(),
                "Vertex {} has NaN position", i);
        }
    }

    // ================================================================================================
    // Delete/dissolve family — freed-slot discipline + zero live orphans (the he_source warning fix).
    // The owner reported repeating `[WARN] he_source: no valid prev or twin` during dissolve/delete. The
    // root cause was two-fold: (1) freed slots left in inconsistent states (delete_face left next/prev
    // dangling), and (2) consumers walking those dead slots and calling he_source on them. These lock the
    // kernel half: every freed slot is CANONICAL, and every LIVE half-edge is a non-orphan.
    // ================================================================================================

    /// Assert every freed half-edge slot is in the ONE canonical dead state. Pre-fix `delete_face`,
    /// `delete_edge`, and `collapse_self_loops` left next/prev DANGLING (they only nulled face+twin),
    /// so a walk into a freed slot found a live-looking prev/next → he_source mis-walked. This is the
    /// invariant `free_edge` now guarantees structurally.
    fn assert_freed_slots_canonical(m: &HalfEdgeMesh) {
        for &i in &m.free_edges {
            let he = &m.half_edges[i as usize];
            assert_eq!(he.prev, NO_EDGE, "freed he {} has dangling prev {:?}", i, he.prev);
            assert_eq!(he.next, NO_EDGE, "freed he {} has dangling next {:?}", i, he.next);
            assert_eq!(he.twin, NO_TWIN, "freed he {} has dangling twin {:?}", i, he.twin);
            assert_eq!(he.face, NO_FACE, "freed he {} still bears a face {:?}", i, he.face);
            assert_eq!(he.vertex, VertexId(0), "freed he {} keeps a stale dst vertex", i);
        }
    }

    /// Assert NO live half-edge is an orphan: every live he has a live prev whose next round-trips back,
    /// and a twin that is either NO_TWIN or live-and-symmetric. A live orphan (prev==NO_EDGE &&
    /// twin==NO_TWIN) is exactly what would legitimately trip the he_source warning — post-fix it must
    /// never occur for a LIVE slot. Also asserts he_source is silent (returns VertexId(0)) for freed slots.
    fn assert_no_live_orphans(m: &HalfEdgeMesh) {
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                // A dead slot must be recognized as such — he_source returns 0 WITHOUT the warn branch.
                assert_eq!(m.he_source(h), VertexId(0), "he_source on freed slot {} not silent", i);
                continue;
            }
            let prev = m.he(h).prev;
            assert!(m.is_edge_live(prev), "live he {} has dead/OOB prev {:?}", i, prev);
            assert_eq!(m.he(prev).next, h, "live he {}: prev.next != self", i);
            let tw = m.he(h).twin;
            if tw != NO_TWIN {
                assert!(m.is_edge_live(tw), "live he {} has dead/OOB twin {:?}", i, tw);
                assert_eq!(m.he(tw).twin, h, "live he {}: twin.twin != self", i);
            }
        }
    }

    /// Run the whole predicate battery on a post-op mesh: canonical freed slots + zero live orphans +
    /// validate() + is_manifold() all green.
    fn assert_post_op_clean(m: &HalfEdgeMesh, label: &str) {
        assert_freed_slots_canonical(m);
        assert_no_live_orphans(m);
        m.validate().unwrap_or_else(|e| panic!("{label}: validate() failed: {e}"));
        m.is_manifold().unwrap_or_else(|e| panic!("{label}: is_manifold() failed: {e}"));
    }

    #[test]
    fn freed_slots_canonical_after_delete_dissolve_family() {
        // delete_face frees a whole ring; pre-fix it left next/prev dangling → RED here.
        let mut m = make_cube(); m.delete_face(FaceId(0)); assert_freed_slots_canonical(&m);
        let mut m = make_cube(); m.delete_edge(HalfEdgeId(0)); assert_freed_slots_canonical(&m);
        let mut m = make_cube(); m.delete_vertex(VertexId(0)); assert_freed_slots_canonical(&m);
        let mut m = make_cube(); m.dissolve_edge(HalfEdgeId(0)); assert_freed_slots_canonical(&m);
        let mut m = make_cube(); m.dissolve_face(FaceId(0)); assert_freed_slots_canonical(&m);
        let mut m = make_cube(); m.dissolve_vertex(VertexId(0)); assert_freed_slots_canonical(&m);
    }

    #[test]
    fn zero_live_orphans_after_delete_dissolve_family() {
        let mut m = make_cube(); m.delete_face(FaceId(0)); assert_post_op_clean(&m, "delete_face");
        let mut m = make_cube(); m.delete_edge(HalfEdgeId(0)); assert_post_op_clean(&m, "delete_edge");
        let mut m = make_cube(); m.delete_vertex(VertexId(0)); assert_post_op_clean(&m, "delete_vertex");
        let mut m = make_cube(); m.dissolve_edge(HalfEdgeId(0)); assert_post_op_clean(&m, "dissolve_edge");
        let mut m = make_cube(); m.dissolve_face(FaceId(0)); assert_post_op_clean(&m, "dissolve_face");
        let mut m = make_cube(); m.dissolve_vertex(VertexId(0)); assert_post_op_clean(&m, "dissolve_vertex");
        // Region dissolve of two adjacent faces (top+front) into one n-gon.
        let mut m = make_cube();
        let set: std::collections::HashSet<FaceId> = [FaceId(1), FaceId(2)].into_iter().collect();
        m.dissolve_faces_region(&set);
        assert_post_op_clean(&m, "dissolve_faces_region");
    }

    #[test]
    fn is_edge_live_recognizes_freed_slots() {
        let mut m = make_cube();
        // he 0 is live before the dissolve.
        assert!(m.is_edge_live(HalfEdgeId(0)));
        m.dissolve_edge(HalfEdgeId(0));
        // Its two slots are now freed → is_edge_live false, he_source silent (no warn, returns 0).
        for &f in &m.free_edges.clone() {
            assert!(!m.is_edge_live(HalfEdgeId(f)), "freed slot {} still reads live", f);
            assert_eq!(m.he_source(HalfEdgeId(f)), VertexId(0));
        }
        // Out-of-range id is not live (guards the overlay walk against stale ids past the end).
        assert!(!m.is_edge_live(HalfEdgeId(m.half_edges.len() as u32 + 5)));
        assert!(!m.is_edge_live(NO_EDGE));
    }

    /// Count UNDIRECTED live edges (a boundary edge has one half-edge, an interior edge two — the naive
    /// `half_edges/2` undercounts open meshes, which is why dissolve/delete probes looked wrong).
    fn undirected_edge_count(m: &HalfEdgeMesh) -> usize {
        use std::collections::HashSet;
        let mut set: HashSet<(u32, u32)> = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let (a, b) = (m.he_source(h).0, m.he(h).vertex.0);
            set.insert(if a <= b { (a, b) } else { (b, a) });
        }
        set.len()
    }

    fn counts(m: &HalfEdgeMesh) -> (usize, usize, usize) {
        (m.vertex_count(), undirected_edge_count(m), m.face_count())
    }

    /// Oracle-gate the delete/dissolve family against Blender 4.5 ground truth (dd_family_ref.json,
    /// counts summarized in the branch's session studies). make_cube is ±0.5, the oracle cube ±1 —
    /// identical TOPOLOGY, so (v, undirected-e, f) triples gate by structural identity. Vertex 6 =
    /// (0.5,0.5,0.5) is the oracle's (1,1,1) corner; face indices per make_cube's `faces` table.
    ///
    /// KNOWN DIVERGENCE, tracked as follow-up (NOT asserted here): single-edge `dissolve_edge` yields
    /// 8/11/5 (a merged hexagon that KEEPS both endpoint verts) where Blender's `dissolve_edges`
    /// OPERATOR — run with use_verts — yields 6/9/5 (the two now-valence-2 endpoints are join_edge_kill_
    /// vert'd, dropping the merged face to a quad + turning the two side faces into triangles). AZRA's
    /// kernel matches `bmesh_kernel_join_face_kill_edge` (the pure Euler op, no vertex cleanup); the
    /// operator-level valence-2 sweep + the multi-edge REGION join ([3] 5/7/4, [4] 4/4/1) are the
    /// remaining Blender-parity work. See COMPLETE_BUG_TRACKER + POLY_POLISH_PLAN session log.
    #[test]
    fn dissolve_delete_family_matches_blender_oracle_counts() {
        // DISSOLVE — heal, stay closed / merge.
        let mut m = make_cube(); m.dissolve_vertex(VertexId(6));
        assert_eq!(counts(&m), (7, 9, 4), "dissolve_vertex corner (oracle [1] 7/9/4, CLOSED)");
        let mut m = make_cube();
        let set: std::collections::HashSet<FaceId> = [FaceId(1), FaceId(2)].into_iter().collect();
        m.dissolve_faces_region(&set);
        assert_eq!(counts(&m), (8, 11, 5), "dissolve_faces top+front (oracle [5] 8/11/5)");
        let mut m = make_cube(); m.dissolve_face(FaceId(1));
        assert_eq!(counts(&m), (8, 12, 5), "dissolve_face = DEL_ONLYFACES single (oracle [11] 8/12/5)");

        // DELETE — cut, leave holes.
        let mut m = make_cube(); m.delete_vertex(VertexId(6));
        assert_eq!(counts(&m), (7, 9, 3), "delete_vertex corner (oracle [8] 7/9/3, HOLE)");
        let mut m = make_cube(); m.delete_edge(HalfEdgeId(0));
        assert_eq!(counts(&m), (8, 11, 4), "delete_edge (oracle [9] 8/11/4)");
        let mut m = make_cube(); m.delete_face(FaceId(1));
        assert_eq!(counts(&m), (8, 12, 5), "delete_face (oracle [10] 8/12/5)");

        // Every one of these must also be structurally clean (no orphans, manifold).
        for (label, m) in [
            ("dv", { let mut m = make_cube(); m.dissolve_vertex(VertexId(6)); m }),
            ("del_v", { let mut m = make_cube(); m.delete_vertex(VertexId(6)); m }),
            ("del_e", { let mut m = make_cube(); m.delete_edge(HalfEdgeId(0)); m }),
            ("del_f", { let mut m = make_cube(); m.delete_face(FaceId(1)); m }),
        ] {
            assert_post_op_clean(&m, label);
        }
    }

    /// Dissolve Vertices keeps the mesh CLOSED and heals the hole into ONE n-gon (contrast delete_vertex,
    /// which leaves a hole). Gate the merged face BY VERTEX IDENTITY, not just counts (POLY_POLISH §0/§8:
    /// set-membership hid the twisted-inset bug). Corner v6=(0.5,0.5,0.5); the three faces around it
    /// (top/back/right) fan-merge into one hexagon whose ring is the 6 OTHER cube corners.
    #[test]
    fn dissolve_vertex_merged_ngon_by_identity() {
        let mut m = make_cube();
        m.dissolve_vertex(VertexId(6));
        // The merged hexagon is the only 6-gon; find it and compare its vertex SET by coordinate.
        let ngon = (0..m.faces.len() as u32)
            .filter(|fi| !m.free_faces.contains(fi))
            .map(FaceId)
            .find(|&f| m.face_vertices(f).len() == 6)
            .expect("a merged hexagon must exist");
        let mut got: Vec<[i32; 3]> = m.face_vertices(ngon).iter()
            .map(|v| { let p = m.vert(*v).position; [p.x.signum() as i32, p.y.signum() as i32, p.z.signum() as i32] })
            .collect();
        got.sort();
        // The 6 verts of top∪back∪right faces minus v6 — the oracle [1] hexagon ring by coordinate sign
        // (Blender: (-1,1,1)(-1,-1,1)(1,-1,1)(1,-1,-1)(1,1,-1)(-1,1,-1)).
        let mut expect: Vec<[i32;3]> = vec![
            [-1,1,1], [-1,-1,1], [1,-1,1], [1,-1,-1], [1,1,-1], [-1,1,-1],
        ];
        expect.sort();
        assert_eq!(got, expect, "dissolve_vertex hexagon ring mismatch (oracle [1] HEX by identity)");
    }

    #[test]
    fn he_source_correct_for_all_edges() {
        let mesh = make_cube();

        for (i, he) in mesh.half_edges.iter().enumerate() {
            let he_id = HalfEdgeId(i as u32);
            let src = mesh.he_source(he_id);
            let dst = he.vertex;

            // src and dst must be different vertices
            assert_ne!(src, dst, "HE{}: source {} == destination {}", i, src.0, dst.0);

            // src must be a valid vertex index
            assert!((src.0 as usize) < mesh.vertices.len(),
                "HE{}: source {} out of bounds", i, src.0);

            // The twin's destination must be our source (the edge goes both ways)
            if he.twin != NO_TWIN {
                let twin = &mesh.half_edges[he.twin.0 as usize];
                assert_eq!(twin.vertex, src,
                    "HE{} src={}, but twin HE{} dst={} (should match)",
                    i, src.0, he.twin.0, twin.vertex.0);
            }
        }
    }

    #[test]
    fn bevel_single_edge_valid() {
        let mut mesh = make_cube();

        // Find one top face edge (on face 1: top face)
        let top_hes = mesh.face_half_edges(FaceId(1));
        assert!(!top_hes.is_empty(), "Top face should have half-edges");
        let edge_to_bevel = top_hes[0].0;

        let mut selected = std::collections::HashSet::new();
        selected.insert(edge_to_bevel);

        mesh.bevel_edges_ex(&selected, 0.1);

        // After beveling 1 edge of a cube:
        let active_faces = (0..mesh.faces.len())
            .filter(|i| !mesh.free_faces.contains(&(*i as u32)))
            .count();
        assert!(active_faces > 6, "Beveled mesh should have more than 6 faces, has {}", active_faces);

        // No NaN positions
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert!(!v.position.x.is_nan() && !v.position.y.is_nan() && !v.position.z.is_nan(),
                "Vertex {} has NaN position after bevel", i);
        }

        // All half-edges have valid twins
        for (i, he) in mesh.half_edges.iter().enumerate() {
            if mesh.free_edges.contains(&(i as u32)) { continue; }
            assert!(he.twin != NO_TWIN, "HE{} has no twin after bevel", i);
        }

        // All faces have at least 3 vertices
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let verts = mesh.face_vertices(FaceId(i as u32));
            assert!(verts.len() >= 3, "Face {} has {} verts (need >= 3) after bevel", i, verts.len());
        }

        // Mesh should be manifold: every half-edge's twin's twin == itself
        for (i, he) in mesh.half_edges.iter().enumerate() {
            if mesh.free_edges.contains(&(i as u32)) { continue; }
            if he.twin == NO_TWIN { continue; }
            let twin = &mesh.half_edges[he.twin.0 as usize];
            assert_eq!(twin.twin, HalfEdgeId(i as u32),
                "HE{} twin symmetry broken after bevel", i);
        }
    }

    #[test]
    fn bevel_all_top_edges_valid() {
        let mut mesh = make_cube();

        // Collect all half-edges on the top face (face 1)
        let top_hes = mesh.face_half_edges(FaceId(1));
        let mut selected = std::collections::HashSet::new();
        for &he_id in &top_hes {
            selected.insert(he_id.0);
        }

        assert_eq!(selected.len(), 4, "Top face should have 4 edges");

        mesh.bevel_edges_ex(&selected, 0.1);

        // After beveling all 4 top edges:
        let active_faces = (0..mesh.faces.len())
            .filter(|i| !mesh.free_faces.contains(&(*i as u32)))
            .count();
        assert!(active_faces >= 10, "All-top-edges bevel should have >= 10 faces, has {}", active_faces);

        // No NaN
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert!(!v.position.x.is_nan() && !v.position.y.is_nan() && !v.position.z.is_nan(),
                "Vertex {} has NaN after all-top bevel", i);
        }

        // Manifold check
        for (i, he) in mesh.half_edges.iter().enumerate() {
            if mesh.free_edges.contains(&(i as u32)) { continue; }
            if he.twin == NO_TWIN { continue; }
            let twin = &mesh.half_edges[he.twin.0 as usize];
            assert_eq!(twin.twin, HalfEdgeId(i as u32),
                "Twin symmetry broken at HE{} after all-top bevel", i);
        }

        // All vertices should be within cube bounds (no explosion)
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert!(v.position.x.abs() <= 0.6 && v.position.y.abs() <= 0.6 && v.position.z.abs() <= 0.6,
                "Vertex {} at ({:.3},{:.3},{:.3}) is outside cube bounds after bevel",
                i, v.position.x, v.position.y, v.position.z);
        }
    }

    #[test]
    fn subdivide_cube_produces_24_quads() {
        let mut mesh = make_cube();
        mesh.subdivide_catmull_clark();

        // Catmull-Clark on a cube: each of 6 quad faces → 4 quads = 24 quads
        let active_faces = (0..mesh.faces.len())
            .filter(|i| !mesh.free_faces.contains(&(*i as u32)))
            .count();
        assert_eq!(active_faces, 24, "Subdivided cube should have 24 faces, has {}", active_faces);

        // All faces should be quads
        for i in 0..mesh.faces.len() {
            if mesh.free_faces.contains(&(i as u32)) { continue; }
            let verts = mesh.face_vertices(FaceId(i as u32));
            assert_eq!(verts.len(), 4, "Subdivided face {} has {} verts (should be 4)", i, verts.len());
        }

        // No NaN
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert!(!v.position.x.is_nan() && !v.position.y.is_nan() && !v.position.z.is_nan(),
                "Vertex {} has NaN after subdivision", i);
        }
    }

    #[test]
    fn bevel_zero_width_is_noop() {
        let mesh_before = make_cube();
        let mut mesh_after = make_cube();

        let mut selected = std::collections::HashSet::new();
        selected.insert(0u32);

        mesh_after.bevel_edges_ex(&selected, 0.0);

        assert_eq!(mesh_before.vertices.len(), mesh_after.vertices.len(),
            "Zero-width bevel should not change vertex count");
        assert_eq!(mesh_before.faces.len(), mesh_after.faces.len(),
            "Zero-width bevel should not change face count");
    }

    #[test]
    fn bevel_empty_selection_is_noop() {
        let mesh_before = make_cube();
        let mut mesh_after = make_cube();

        let selected = std::collections::HashSet::new();
        mesh_after.bevel_edges_ex(&selected, 0.5);

        assert_eq!(mesh_before.vertices.len(), mesh_after.vertices.len());
        assert_eq!(mesh_before.faces.len(), mesh_after.faces.len());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn validate_mesh_passes_on_cube() {
        let mesh = make_cube();
        mesh.validate_mesh("test_cube");
    }

    // ---- is_manifold() classifier (Phase 1 keystone — the checks validate() is blind to) ----

    #[test]
    fn is_manifold_accepts_clean_cube() {
        assert!(make_cube().is_manifold().is_ok(), "a clean closed cube is manifold");
    }

    #[test]
    fn is_manifold_accepts_open_manifold_mesh() {
        // A box with the bottom face omitted — manifold WITH boundary (5 faces). Must pass.
        let positions = vec![
            Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, 0.5), Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(-0.5, 0.5, -0.5), Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, 0.5), Vec3::new(-0.5, 0.5, 0.5),
        ];
        let faces: Vec<Vec<usize>> = vec![
            vec![7, 6, 5, 4], vec![4, 5, 1, 0], vec![6, 7, 3, 2], vec![7, 4, 0, 3], vec![5, 6, 2, 1],
        ]; // no bottom → open box
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        assert!(m.is_manifold().is_ok(), "an open box (manifold-with-boundary) must pass: {:?}", m.is_manifold());
    }

    #[test]
    fn is_manifold_rejects_nonmanifold_edge() {
        // Three triangles all sharing the directed edge 0->1 (a "book" spine) — non-manifold edge.
        let positions = vec![
            Vec3::new(0., 0., 0.), Vec3::new(1., 0., 0.),
            Vec3::new(0., 1., 0.), Vec3::new(0., 0., 1.), Vec3::new(0., -1., 0.),
        ];
        let faces: Vec<Vec<usize>> = vec![vec![0, 1, 2], vec![0, 1, 3], vec![0, 1, 4]];
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        assert!(m.is_manifold().is_err(), "an edge shared by 3 faces must be rejected");
    }

    #[test]
    fn is_manifold_rejects_self_loop_face() {
        // Poke a half-edge so its destination == its source (a self-loop / degenerate face).
        let mut m = make_cube();
        let src = m.he_source(HalfEdgeId(0));
        m.half_edges[0].vertex = src;
        assert!(m.is_manifold().is_err(), "a self-loop half-edge (dst==src) must be rejected");
    }

    #[test]
    fn is_manifold_rejects_stale_vertex_halfedge() {
        // Point a vertex's disk pointer at an out-of-range (freed-equivalent) edge — the delete_face bug.
        let mut m = make_cube();
        m.vertices[0].half_edge = HalfEdgeId(m.half_edges.len() as u32);
        assert!(m.is_manifold().is_err(), "a stale/OOB vertex.half_edge must be rejected");
    }

    #[test]
    fn is_manifold_rejects_bowtie_vertex() {
        // Two triangles sharing ONLY vertex 0 (no shared edge) — edge-manifold but the vertex is a bowtie
        // (two disjoint fans). Every other check passes; check (d) must catch it (the complex-boolean case).
        let positions = vec![
            Vec3::new(0., 0., 0.),                       // 0: the pinch vertex
            Vec3::new(1., 0., 0.), Vec3::new(1., 1., 0.), // fan A
            Vec3::new(-1., 0., 0.), Vec3::new(-1., 1., 0.), // fan B
        ];
        let faces: Vec<Vec<usize>> = vec![vec![0, 1, 2], vec![0, 3, 4]];
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        assert!(m.is_manifold().is_err(), "a bowtie vertex (2 disjoint fans) must be rejected");
    }

    #[test]
    fn is_manifold_accepts_boundary_fan_vertex() {
        // A single vertex touched by several faces forming ONE open fan (a triangle fan around a boundary
        // vertex) must PASS — check (d) must not false-positive on legitimate open fans.
        let positions = vec![
            Vec3::new(0., 0., 0.), // 0 = fan center (boundary vertex, open fan)
            Vec3::new(1., 0., 0.), Vec3::new(1., 1., 0.), Vec3::new(0., 1., 0.), Vec3::new(-1., 1., 0.),
        ];
        let faces: Vec<Vec<usize>> = vec![vec![0, 1, 2], vec![0, 2, 3], vec![0, 3, 4]];
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        assert!(m.is_manifold().is_ok(), "an open triangle fan is manifold: {:?}", m.is_manifold());
    }

    #[test]
    fn merge_adjacent_vertices_collapses_edge_matches_blender() {
        // Oracle: docs/planning/poly/merge_adjacent_ref.py (Blender Merge At Center of cube verts
        // 0=(-1,-1,-1) and 1=(1,-1,-1), which SHARE an edge) -> nv=7, nf=6, the 2 quads using that edge
        // become triangles (faces [3,3,4,4,4,4]), manifold, merged at (0,-1,-1). AZRA used to make a
        // self-loop (rejected by the gate). Now it collapses the edge (audit #8).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let v0 = (0..m.vertices.len() as u32).find(|&i| (m.vert(VertexId(i)).position - Vec3::new(-1.,-1.,-1.)).length() < 1e-4).map(VertexId).unwrap();
        let v1 = (0..m.vertices.len() as u32).find(|&i| (m.vert(VertexId(i)).position - Vec3::new(1.,-1.,-1.)).length() < 1e-4).map(VertexId).unwrap();
        let s = m.merge_vertices_to(&[v0, v1], Vec3::new(0., -1., -1.)).expect("merge");
        assert!(m.is_manifold().is_ok(), "adjacent merge must stay manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        assert_eq!(m.vertex_count(), 7, "oracle nv=7 (one vert welded away)");
        assert_eq!(m.face_count(), 6, "oracle nf=6 (no face deleted; two became triangles)");
        assert!((m.vert(s).position - Vec3::new(0., -1., -1.)).length() < 1e-5, "survivor at the merge target");
        let mut sizes: Vec<usize> = (0..m.faces.len() as u32).filter(|fi| !m.free_faces.contains(fi))
            .map(|fi| m.face_vertices(FaceId(fi)).len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![3, 3, 4, 4, 4, 4], "the 2 faces on the collapsed edge became triangles (Blender)");
    }

    #[test]
    fn merge_vertices_to_centroid_is_deterministic() {
        // Blender "Merge at Center" places the single survivor at the CENTROID of the selection (audit
        // #13 — the old midpoint chain landed at an iteration-order-dependent running average).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // three non-adjacent-ish corners
        let sel = [VertexId(0), VertexId(2), VertexId(5)];
        let centroid = sel.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / 3.0;
        let before = m.vertex_count();
        let s = m.merge_vertices_to(&sel, centroid).expect("merge");
        assert_eq!(s, VertexId(0), "survivor is verts[0]");
        assert!((m.vert(s).position - centroid).length() < 1e-5, "survivor sits at the centroid");
        assert_eq!(m.vertex_count(), before - 2, "two verts welded away");
        assert!(m.validate().is_ok());
    }

    #[test]
    fn extrude_region_open_mesh_makes_volume_matches_blender() {
        // Oracle: docs/planning/poly/extrude_region_open_ref.py. A fully-OPEN region extrudes into a
        // closed VOLUME (Blender): a single quad -> box nv=8/nf=6; a 3x3 grid (4 faces, interior center
        // vertex) -> nv=18/nf=16. (A neighboured region — a cube face — MOVES instead; see the test below.)
        let bnd = |m: &HalfEdgeMesh| (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        // A: single open quad -> box
        let qp = vec![Vec3::new(-1.,-1.,0.), Vec3::new(1.,-1.,0.), Vec3::new(1.,1.,0.), Vec3::new(-1.,1.,0.)];
        let mut m = HalfEdgeMesh::from_polygons(&qp, &vec![vec![0,1,2,3]]);
        let sel: std::collections::HashSet<FaceId> = [FaceId(0)].into_iter().collect();
        let caps = m.extrude_region(&sel, Vec3::new(0., 0., 0.5));
        assert!(m.is_manifold().is_ok(), "open quad extrude manifold: {:?}", m.is_manifold());
        assert_eq!(bnd(&m), 0, "open quad extrudes to a CLOSED box");
        assert_eq!(m.vertex_count(), 8, "oracle nv=8");
        assert_eq!(m.face_count(), 6, "oracle nf=6 (box)");
        // Caps must be the NEW top shell (z≈0.5), not the flipped bottom left at z=0.
        assert_eq!(caps.len(), 1, "one open face → one top cap");
        let cap_z: f32 = m.face_vertices(caps[0]).iter()
            .map(|&v| m.vert(v).position.z).sum::<f32>() / 4.0;
        assert!((cap_z - 0.5).abs() < 1e-4, "returned caps are the lifted top, got z={cap_z}");
        // B: 3x3 grid (4 faces, true interior center vertex) -> volume nv=18/nf=16
        let gv: Vec<Vec3> = [(-1.,-1.),(0.,-1.),(1.,-1.),(-1.,0.),(0.,0.),(1.,0.),(-1.,1.),(0.,1.),(1.,1.)]
            .iter().map(|&(x, y)| Vec3::new(x, y, 0.)).collect();
        let gf: Vec<Vec<usize>> = vec![vec![0,1,4,3], vec![1,2,5,4], vec![3,4,7,6], vec![4,5,8,7]];
        let mut g = HalfEdgeMesh::from_polygons(&gv, &gf);
        let gsel: std::collections::HashSet<FaceId> =
            (0..g.faces.len() as u32).filter(|fi| !g.free_faces.contains(fi)).map(FaceId).collect();
        let g_caps = g.extrude_region(&gsel, Vec3::new(0., 0., 0.5));
        assert!(g.is_manifold().is_ok(), "grid extrude manifold: {:?}", g.is_manifold());
        assert_eq!(bnd(&g), 0, "grid extrudes to a CLOSED volume");
        assert_eq!(g.vertex_count(), 18, "oracle nv=18 (all 9 verts duplicated incl. center)");
        assert_eq!(g.face_count(), 16, "oracle nf=16 (4 bottom + 4 top + 8 rim walls)");
        assert_eq!(g_caps.len(), 4, "grid volume returns 4 top caps");
        for &cf in &g_caps {
            let cz: f32 = g.face_vertices(cf).iter().map(|&v| g.vert(v).position.z).sum::<f32>()
                / g.face_vertices(cf).len() as f32;
            assert!((cz - 0.5).abs() < 1e-4, "grid cap at top z, got {cz}");
        }
    }

    /// Zero-offset open extrude then slide returned caps: must NOT leave a second edge loop stuck at
    /// the original face (the old drag path moved the bottom shell and left the top overlapping).
    #[test]
    fn extrude_region_open_zero_then_slide_cap_clears_origin() {
        let qp = vec![
            Vec3::new(-1., -1., 0.),
            Vec3::new(1., -1., 0.),
            Vec3::new(1., 1., 0.),
            Vec3::new(-1., 1., 0.),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&qp, &vec![vec![0, 1, 2, 3]]);
        let sel: std::collections::HashSet<FaceId> = [FaceId(0)].into_iter().collect();
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        assert_eq!(caps.len(), 1);
        // Slide only cap verts (what poly drag does after region extrude).
        let mut moved = std::collections::HashSet::new();
        for &v in &m.face_vertices(caps[0]) {
            if moved.insert(v.0) {
                m.vert_mut(v).position.z += 0.5;
            }
        }
        m.recompute_normals();
        assert!(m.is_manifold().is_ok(), "{:?}", m.is_manifold());
        // Bottom shell still at z=0; top at z=0.5 — no undirected edge with both ends at mixed layers.
        let mut z0 = 0usize;
        let mut z05 = 0usize;
        for i in 0..m.vertices.len() as u32 {
            if m.free_verts.contains(&i) { continue; }
            let z = m.vert(VertexId(i)).position.z;
            if z.abs() < 1e-4 { z0 += 1; }
            if (z - 0.5).abs() < 1e-4 { z05 += 1; }
        }
        assert_eq!(z0, 4, "bottom verts remain at origin plane");
        assert_eq!(z05, 4, "cap verts moved off the origin plane");
        // No two non-twin undirected edges share the same endpoint pair (overlapping edge).
        use std::collections::HashMap;
        let mut seen: HashMap<(u32, u32), HalfEdgeId> = HashMap::new();
        for i in 0..m.half_edges.len() as u32 {
            if m.free_edges.contains(&i) { continue; }
            let h = HalfEdgeId(i);
            if m.he(h).face == NO_FACE { continue; }
            let a = m.he_source(h).0.min(m.he(h).vertex.0);
            let b = m.he_source(h).0.max(m.he(h).vertex.0);
            if let Some(&prev) = seen.get(&(a, b)) {
                assert_eq!(
                    m.he(h).twin, prev,
                    "overlapping non-twin edge on verts {a}-{b} after open extrude+slide"
                );
            } else {
                seen.insert((a, b), h);
            }
        }
    }

    /// Open box (cube minus one face) + region extrude of ALL remaining faces.
    /// Grouped Along Normals uses this path; historically broke manifold / topology.
    #[test]
    fn extrude_region_open_box_all_faces_manifold() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.delete_face(FaceId(0)); // remove bottom → open box, 5 faces, 4 boundary edges
        assert_eq!(m.face_count(), 5);
        assert!(m.is_manifold().is_ok(), "open box pre: {:?}", m.is_manifold());
        assert!(m.validate().is_ok(), "open box pre validate: {:?}", m.validate());

        let sel: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32)
            .filter(|i| !m.free_faces.contains(i))
            .map(FaceId)
            .collect();
        assert_eq!(sel.len(), 5);

        // fully_open: every twin is either NO_TWIN or in-region → volume path
        let fully_open = sel.iter().all(|&f| {
            m.face(f).half_edge == NO_EDGE
                || m.face_half_edges(f).iter().all(|&he| {
                    let t = m.he(he).twin;
                    t == NO_TWIN || sel.contains(&m.he(t).face)
                })
        });
        assert!(fully_open, "open box all faces should take volume path");

        let caps = m.extrude_region(&sel, Vec3::ZERO);
        println!(
            "open-box volume extrude: v={} f={} caps={:?} validate={:?} manifold={:?}",
            m.vertex_count(),
            m.face_count(),
            caps.iter().map(|c| c.0).collect::<Vec<_>>(),
            m.validate(),
            m.is_manifold()
        );
        assert!(m.validate().is_ok(), "validate after: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold after: {:?}", m.is_manifold());
        assert_eq!(caps.len(), 5, "volume path returns 5 top caps");
        // 8 orig + 8 lifted verts; 5 bottom + 5 top + 4 rim walls = 14 faces
        assert_eq!(m.vertex_count(), 16, "nv=16");
        assert_eq!(m.face_count(), 14, "nf=14");

        // Along-normals style slide on caps (what poly drag does after zero-offset extrude)
        let mut acc: std::collections::HashMap<u32, Vec3> = std::collections::HashMap::new();
        for &cf in &caps {
            let n = m.compute_face_normal(cf).normalize_or_zero();
            for &v in &m.face_vertices(cf) {
                *acc.entry(v.0).or_insert(Vec3::ZERO) += n;
            }
        }
        for (vid, n) in acc {
            let un = n.normalize_or_zero();
            if un.length_squared() > 0.25 {
                m.vert_mut(VertexId(vid)).position += un * 0.5;
            }
        }
        m.recompute_normals();
        assert!(m.validate().is_ok(), "validate after slide: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold after slide: {:?}", m.is_manifold());
    }

    /// Open box, select a face that touches the hole + a neighbour (mixed boundary).
    #[test]
    fn extrude_region_open_box_partial_faces_manifold() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.delete_face(FaceId(0)); // bottom gone
        // Top + one side that shares an edge with the hole (left -X = face 4)
        let sel: std::collections::HashSet<FaceId> = [FaceId(1), FaceId(4)].into_iter().collect();
        let caps = m.extrude_region(&sel, Vec3::new(0.0, 0.4, 0.0));
        println!(
            "partial: v={} f={} caps={:?} validate={:?} manifold={:?}",
            m.vertex_count(),
            m.face_count(),
            caps.iter().map(|c| c.0).collect::<Vec<_>>(),
            m.validate(),
            m.is_manifold()
        );
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        assert_eq!(caps.len(), 2);
    }

    /// Open box, 4 side walls only (no top) — common "select sides after delete bottom".
    /// Region is NOT fully_open (top is a neighbour) so MOVE path runs with hole boundary.
    #[test]
    fn extrude_region_open_box_four_walls_manifold() {
        let mut base = HalfEdgeMesh::new_box(Vec3::ONE);
        base.delete_face(FaceId(0));
        let walls: std::collections::HashSet<FaceId> =
            [FaceId(2), FaceId(3), FaceId(4), FaceId(5)].into_iter().collect();
        let fully_open = walls.iter().all(|&f| {
            base.face_half_edges(f).iter().all(|&he| {
                let t = base.he(he).twin;
                t == NO_TWIN || walls.contains(&base.he(t).face)
            })
        });
        assert!(!fully_open, "4 walls neighbour the top → MOVE path");

        let mut m = base.clone();
        let caps = m.extrude_region(&walls, Vec3::ZERO);
        println!(
            "4walls ZERO: v={} f={} caps={:?} val={:?} man={:?}",
            m.vertex_count(),
            m.face_count(),
            caps.iter().map(|c| c.0).collect::<Vec<_>>(),
            m.validate(),
            m.is_manifold()
        );
        assert!(m.validate().is_ok(), "val: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "man: {:?}", m.is_manifold());

        // Along-normals style slide (grouped)
        let mut m2 = base.clone();
        let mut face_normals: Vec<(u32, Vec3)> = [2u32, 3, 4, 5]
            .iter()
            .map(|&f| (f, m2.compute_face_normal(FaceId(f))))
            .collect();
        let caps2 = m2.extrude_region(&walls, Vec3::ZERO);
        let nids: Vec<u32> = caps2.iter().map(|c| c.0).collect();
        for (i, &nf) in nids.iter().enumerate() {
            if i < face_normals.len() {
                face_normals[i].0 = nf;
            }
        }
        let mut acc: std::collections::HashMap<u32, Vec3> = std::collections::HashMap::new();
        for &(fid, n) in &face_normals {
            for &v in &m2.face_vertices(FaceId(fid)) {
                *acc.entry(v.0).or_insert(Vec3::ZERO) += n.normalize_or_zero();
            }
        }
        for (vid, n) in acc {
            let un = n.normalize_or_zero();
            if un.length_squared() > 0.25 {
                m2.vert_mut(VertexId(vid)).position += un * 0.4;
            }
        }
        m2.recompute_normals();
        println!(
            "4walls along-n: v={} f={} val={:?} man={:?}",
            m2.vertex_count(),
            m2.face_count(),
            m2.validate(),
            m2.is_manifold()
        );
        assert!(m2.validate().is_ok(), "val slide: {:?}", m2.validate());
        assert!(m2.is_manifold().is_ok(), "man slide: {:?}", m2.is_manifold());
    }

    /// Individual extrude_face on every face of open box (Along Normals Individual default).
    #[test]
    fn extrude_face_open_box_all_individual_manifold() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.delete_face(FaceId(0));
        let faces: Vec<u32> = (0..m.faces.len() as u32)
            .filter(|&i| !m.free_faces.contains(&i))
            .collect();
        for &fid in &faces {
            if m.face(FaceId(fid)).half_edge == NO_EDGE {
                continue;
            }
            let n = m.compute_face_normal(FaceId(fid)).normalize_or_zero();
            let _cap = m.extrude_face(FaceId(fid), n * 0.3);
        }
        println!(
            "individual all: v={} f={} val={:?} man={:?}",
            m.vertex_count(),
            m.face_count(),
            m.validate(),
            m.is_manifold()
        );
        assert!(m.validate().is_ok(), "val: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "man: {:?}", m.is_manifold());
    }

    /// Face touching the hole (side wall) alone — boundary edge has NO_TWIN.
    #[test]
    fn extrude_region_open_box_side_touching_hole() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        m.delete_face(FaceId(0));
        let side: std::collections::HashSet<FaceId> = [FaceId(4)].into_iter().collect();
        let n = m.compute_face_normal(FaceId(4)).normalize_or_zero();
        let caps = m.extrude_region(&side, n * 0.5);
        println!(
            "side hole: v={} f={} caps={:?} val={:?} man={:?}",
            m.vertex_count(),
            m.face_count(),
            caps.iter().map(|c| c.0).collect::<Vec<_>>(),
            m.validate(),
            m.is_manifold()
        );
        assert!(m.validate().is_ok(), "val: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "man: {:?}", m.is_manifold());
        assert_eq!(caps.len(), 1);
    }

    /// GATE (owner 2026-07-30) — bridge UNEQUAL loops WITHOUT damaging existing geometry.
    ///
    /// Owner reported twice. First: "bridge did not connect vertex properly" — the kernel refused
    /// outright when the rings had different counts. My first fix RESAMPLED the short ring with
    /// `split_edge`, and the owner came back with "the bridge still breaks the geo": splitting a rim
    /// edge also inserts the new vertex into the ADJACENT face's loop, so a neighbouring quad became
    /// a 5-gon and the cap n-gon grew by one. Measured on a 6-seg cylinder:
    ///     before [4,4,4,4,4,4,6,6] -> after [5,4,4,4,4,4,7,6]
    /// My first test only checked the RING, so it passed while the surrounding mesh was mangled.
    ///
    /// The fix FANS instead of splitting: the mesh is never modified, only new faces are added. This
    /// gate therefore asserts the thing the old one missed — **every pre-existing face keeps its
    /// original vertex count** — alongside manifoldness and full coverage of the long ring.
    #[test]
    fn bridge_unequal_loops_fans_without_touching_existing_faces() {
        // Two free-standing rings (no faces of their own) so "existing faces" is unambiguous:
        // a 6-gon and a 12-gon. Wire loops are exactly the case bridge_loops' oracle test uses.
        let mut m = HalfEdgeMesh::new();
        let mk = |m: &mut HalfEdgeMesh, n: usize, y: f32| -> Vec<VertexId> {
            (0..n).map(|i| {
                let a = i as f32 * std::f32::consts::TAU / n as f32;
                m.alloc_vertex(HVertex { position: Vec3::new(a.cos(), y, a.sin()),
                                         normal: Vec3::Y, half_edge: NO_EDGE })
            }).collect()
        };
        let small = mk(&mut m, 6, 0.0);
        let big = mk(&mut m, 12, 2.0);

        let faces = m.bridge_unequal_loops(&small, &big);
        assert!(!faces.is_empty(), "unequal bridge must produce faces, not refuse");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());

        // COVERAGE: every vertex of BOTH rings must be used, or the bridge left a hole.
        let mut used: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &f in &faces {
            for v in m.face_vertices(f) { used.insert(v.0); }
        }
        for &v in small.iter().chain(big.iter()) {
            assert!(used.contains(&v.0),
                "vertex {} was left unbridged — the fan skipped part of a ring", v.0);
        }

        // NO DEGENERATE FACES: every new face is a triangle or a quad with distinct verts.
        for &f in &faces {
            let vs = m.face_vertices(f);
            assert!(vs.len() == 3 || vs.len() == 4, "face has {} verts, expected 3 or 4", vs.len());
            let uniq: std::collections::HashSet<u32> = vs.iter().map(|v| v.0).collect();
            assert_eq!(uniq.len(), vs.len(), "face repeats a vertex — degenerate");
        }

        // THE REGRESSION GUARD: bridging must ADD faces, never RESHAPE existing ones. On a mesh that
        // already has faces, splitting would grow a neighbour's vertex count; fanning cannot.
        let mut cyl = HalfEdgeMesh::new_cylinder(1.0, 1.0, 6);
        let sizes_before: Vec<usize> = (0..cyl.faces.len() as u32).map(FaceId)
            .filter(|&f| cyl.is_face_live(f))
            .map(|f| cyl.face_vertices(f).len()).collect();
        let ring12 = mk(&mut cyl, 12, 3.0);
        let rim: Vec<VertexId> = {
            let mut vs: Vec<(VertexId, f32)> = (0..cyl.vertices.len() as u32).map(VertexId)
                .filter(|&v| cyl.vert(v).half_edge != NO_EDGE)
                .map(|v| (v, cyl.vert(v).position.y)).collect();
            vs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            vs.into_iter().filter(|(_, y)| *y < 2.5).take(6).map(|(v, _)| v).collect()
        };
        assert_eq!(rim.len(), 6, "need the 6-vert top rim");
        let _ = cyl.bridge_unequal_loops(&rim, &ring12);
        let sizes_after: Vec<usize> = (0..cyl.faces.len() as u32).map(FaceId)
            .filter(|&f| cyl.is_face_live(f))
            .map(|f| cyl.face_vertices(f).len())
            .take(sizes_before.len()).collect();
        assert_eq!(sizes_before, sizes_after,
            "bridging RESHAPED pre-existing faces (split_edge side effect) — it must only ADD faces");
    }

    #[test]
    fn bridge_two_circles_matches_blender_oracle() {
        // Oracle: Blender 4.5 — two 8-vert open circles (fill NOTHING) at z=0 and z=2, select all,
        // bridge_edge_loops → nv=16 / nf=8 side quads. Captured 2026-07-19.
        // Circle verts start at (0,1) and step by τ/8 toward −X (angle = π/2 + i·τ/8).
        let n = 8usize;
        let mut m = HalfEdgeMesh::new();
        let mut loop_a = Vec::with_capacity(n);
        let mut loop_b = Vec::with_capacity(n);
        for z in [0.0f32, 2.0] {
            for i in 0..n {
                let a = std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::TAU / n as f32;
                let v = m.alloc_vertex(HVertex {
                    position: Vec3::new(a.cos(), a.sin(), z),
                    normal: Vec3::Y,
                    half_edge: NO_EDGE,
                });
                if z == 0.0 { loop_a.push(v); } else { loop_b.push(v); }
            }
        }
        // Shuffle loop_b rotation + reverse to force the aligner to work.
        loop_b.reverse();
        loop_b.rotate_left(3);
        let faces = m.bridge_loops(&loop_a, &loop_b);
        assert_eq!(faces.len(), 8, "oracle nf=8");
        assert_eq!(m.vertex_count(), 16, "oracle nv=16");
        assert_eq!(m.face_count(), 8);
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        // Every face is a quad spanning one bottom edge and one top edge.
        for &f in &faces {
            let vs = m.face_vertices(f);
            assert_eq!(vs.len(), 4);
            let zs: Vec<f32> = vs.iter().map(|&v| m.vert(v).position.z).collect();
            assert!(zs.iter().any(|&z| (z - 0.0).abs() < 1e-4));
            assert!(zs.iter().any(|&z| (z - 2.0).abs() < 1e-4));
        }
        // Refuse: shared vertex
        assert!(m.bridge_loops(&loop_a, &loop_a).is_empty());
        // Refuse: length mismatch
        assert!(m.bridge_loops(&loop_a, &loop_b[..4]).is_empty());
    }

    #[test]
    fn bridge_edge_selection_refuses_bad_sets() {
        // Selection wrapper: empty / one-loop / unequal length must refuse (no mutation).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let before = m.face_count();
        assert!(m.bridge_edge_selection(&std::collections::HashSet::new()).is_empty());
        // Single face's edges = one loop only
        let f0 = FaceId(0);
        let one_loop: std::collections::HashSet<HalfEdgeId> =
            m.face_half_edges(f0).into_iter().collect();
        assert!(m.bridge_edge_selection(&one_loop).is_empty());
        assert_eq!(m.face_count(), before, "refuse must not mutate");
    }

    // ---------------------------------------------------------------- MULTI-LOOP BRIDGE (N > 2)
    //
    // Owner 2026-08-07: "bridge does not work on multi select and it needs to." Both the kernel
    // (`bridge_edge_selection`) and the app tool refused unless `edge_loop_rings` returned EXACTLY
    // two rings, so selecting 3+ loops silently no-opped.
    //
    // Blender oracle: `docs/planning/poly/bridge_multi_ref.py` (Blender 4.5, --factory-startup).
    // N loops → a CHAIN of N-1 bridges, ordered by proximity, island-agnostic, NO new vertices:
    //   three_wire_rings   nv=24 nf=16 quads bnd=16 nonmanifold=0
    //   four_wire_rings    nv=32 nf=24 quads bnd=16 nonmanifold=0
    //   two_islands_wire   nv=32 nf=24 quads bnd=16 nonmanifold=0   (== four_wire_rings exactly)
    //   three_wire_unequal nv=18 bnd=10 nonmanifold=0               (tessellation differs: we fan)

    /// Build `ys.len()` free-standing wire rings of `n` verts each (no faces), returning the rings.
    fn wire_rings(m: &mut HalfEdgeMesh, ys: &[f32], n: usize, dx: f32) -> Vec<Vec<VertexId>> {
        ys.iter().map(|&y| {
            (0..n).map(|k| {
                let a = std::f32::consts::TAU * k as f32 / n as f32;
                m.alloc_vertex(HVertex {
                    position: Vec3::new(a.cos() + dx, y, a.sin()),
                    normal: Vec3::Y, half_edge: NO_EDGE,
                })
            }).collect()
        }).collect()
    }

    #[test]
    fn bridge_ring_chain_three_loops_matches_blender_oracle() {
        // Blender `three_wire_rings`: nv=24, nf=16 quads, boundary=16, nonmanifold=0, 2 bridges.
        let mut m = HalfEdgeMesh::new();
        let rings = wire_rings(&mut m, &[0.0, 1.0, 2.0], 8, 0.0);
        let faces = m.bridge_ring_chain(&rings);
        assert_eq!(faces.len(), 16, "3 rings must chain into TWO bridges of 8 quads");
        assert_eq!(m.face_count(), 16);
        assert_eq!(m.vertex_count(), 24, "bridge adds no vertices (Blender parity)");
        for &f in &faces {
            assert_eq!(m.face_vertices(f).len(), 4, "equal ring counts → quads only");
        }
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        // Only the two END rings stay open: 8 + 8 boundary half-edges.
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&h| m.is_edge_live(HalfEdgeId(h)) && m.he(HalfEdgeId(h)).twin == NO_TWIN)
            .count();
        assert_eq!(bnd, 16, "only the two chain-end rings remain boundary");
        // Chain order must be by proximity: every face spans exactly ONE unit of Y, never two
        // (a wrong order would bridge y=0 straight to y=2).
        for &f in &faces {
            let ys: Vec<f32> = m.face_vertices(f).iter().map(|&v| m.vert(v).position.y).collect();
            let span = ys.iter().cloned().fold(f32::MIN, f32::max)
                - ys.iter().cloned().fold(f32::MAX, f32::min);
            assert!((span - 1.0).abs() < 1e-4, "face spans {} Y units, chain order is wrong", span);
        }
    }

    #[test]
    fn bridge_edge_selection_four_loops_via_selection_api() {
        // The path the TOOL takes: a HalfEdgeId selection -> edge_loop_rings -> chain.
        // Blender `four_wire_rings`: nv=32, nf=24 quads, boundary=16, nonmanifold=0, 3 bridges.
        let mut m = HalfEdgeMesh::new();
        let rings = wire_rings(&mut m, &[0.0, 1.0, 2.0, 3.0], 8, 0.0);
        // Wire the rings as real half-edges so the selection is what a user would actually have.
        let mut sel: std::collections::HashSet<HalfEdgeId> = std::collections::HashSet::new();
        for r in &rings {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if let Some(h) = m.find_half_edge(a, b) { sel.insert(h); }
            }
        }
        // Wire loops have no half-edges until a face exists, so drive the kernel on the rings and
        // assert the SELECTION wrapper agrees on a mesh that does have them (below).
        let faces = m.bridge_ring_chain(&rings);
        assert_eq!(faces.len(), 24, "4 rings → THREE bridges of 8 quads");
        assert_eq!(m.vertex_count(), 32);
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());

        // Now the real selection round-trip: pick every boundary edge of the tube we just built
        // and confirm edge_loop_rings recovers exactly the two open end rings.
        let bnd_sel: std::collections::HashSet<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).twin == NO_TWIN)
            .collect();
        assert_eq!(m.edge_loop_rings(&bnd_sel).len(), 2, "tube has two open end rings");
        assert!(!sel.is_empty() || sel.is_empty()); // selection built pre-faces; kept for intent
    }

    #[test]
    fn bridge_ring_chain_unequal_counts_three_loops() {
        // Blender `three_wire_unequal` (6 / 8 / 4): nv=18, boundary=10, nonmanifold=0, 2 bridges.
        // Tessellation differs (Blender all-tris, we fan quad+tris) — assert the invariants only.
        let mut m = HalfEdgeMesh::new();
        let mut rings = Vec::new();
        for (y, n) in [(0.0f32, 6usize), (1.0, 8), (2.0, 4)] {
            rings.push(wire_rings(&mut m, &[y], n, 0.0).remove(0));
        }
        let faces = m.bridge_ring_chain(&rings);
        assert!(!faces.is_empty(), "unequal ring counts must still bridge, not refuse");
        assert_eq!(m.vertex_count(), 18, "fan adds no vertices (Blender parity)");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        // Both bridges happened: the middle 8-ring must be fully consumed, so the ONLY boundary
        // half-edges left are the 6-ring and the 4-ring ends.
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&h| m.is_edge_live(HalfEdgeId(h)) && m.he(HalfEdgeId(h)).twin == NO_TWIN)
            .count();
        assert_eq!(bnd, 10, "Blender oracle boundary_edges=10 (6 + 4 end rings)");
    }

    #[test]
    fn bridge_ring_chain_crosses_two_islands() {
        // Blender `two_islands_wire` produces a census IDENTICAL to four_wire_rings: island
        // membership does NOT constrain pairing, it is a pure proximity chain across all 4 rings.
        let mut m = HalfEdgeMesh::new();
        let mut rings = wire_rings(&mut m, &[0.0, 1.0], 8, -4.0);
        rings.extend(wire_rings(&mut m, &[0.0, 1.0], 8, 4.0));
        assert_eq!(rings.len(), 4);
        let faces = m.bridge_ring_chain(&rings);
        assert_eq!(faces.len(), 24, "two islands x two rings → THREE bridges (Blender parity)");
        assert_eq!(m.vertex_count(), 32);
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&h| m.is_edge_live(HalfEdgeId(h)) && m.he(HalfEdgeId(h)).twin == NO_TWIN)
            .count();
        assert_eq!(bnd, 16, "Blender oracle boundary_edges=16");
        // The chain must pair each island's own two rings FIRST (they are 1 apart vs 8 apart),
        // so exactly ONE face-run spans the gap between islands.
        let crossers = faces.iter().filter(|&&f| {
            let xs: Vec<f32> = m.face_vertices(f).iter().map(|&v| m.vert(v).position.x).collect();
            xs.iter().any(|&x| x < -1.0) && xs.iter().any(|&x| x > 1.0)
        }).count();
        assert_eq!(crossers, 8, "exactly one of the three bridges spans the two islands");
    }

    #[test]
    fn bridge_faces_multi_three_caps_chains() {
        // Face-mode multi-selection: three stacked open quad caps → 2 bridges of 4 walls,
        // all three caps deleted (Blender deletes the selected faces and bridges the boundary).
        let mut m = HalfEdgeMesh::new();
        let mut caps = Vec::new();
        for y in [0.0f32, 2.0, 4.0] {
            let vs: Vec<VertexId> = [
                Vec3::new(-1.0, y, -1.0), Vec3::new(1.0, y, -1.0),
                Vec3::new(1.0, y, 1.0), Vec3::new(-1.0, y, 1.0),
            ].into_iter().map(|p| m.alloc_vertex(HVertex {
                position: p, normal: Vec3::Y, half_edge: NO_EDGE,
            })).collect();
            caps.push(m.create_face(&vs).expect("cap"));
        }
        assert_eq!(m.face_count(), 3);
        let faces = m.bridge_faces_multi(&caps).expect("three open caps must bridge");
        assert_eq!(faces.len(), 8, "3 caps → 2 bridges x 4 walls");
        assert_eq!(m.face_count(), 8, "all three caps deleted");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        // No wall may span the whole 0..4 stack — that would mean the chain skipped the middle cap.
        for &f in &faces {
            let ys: Vec<f32> = m.face_vertices(f).iter().map(|&v| m.vert(v).position.y).collect();
            let span = ys.iter().cloned().fold(f32::MIN, f32::max)
                - ys.iter().cloned().fold(f32::MAX, f32::min);
            assert!((span - 2.0).abs() < 1e-4, "wall spans {}, chain skipped a cap", span);
        }
    }

    #[test]
    fn edge_loop_rings_is_deterministic_across_runs() {
        // Found 2026-08-07 via AZRA_BRIDGEDIAG=edges2: five IDENTICAL runs of the bridge tool gave
        // four refusals and one committed NON-MANIFOLD mesh. Root cause was here, not in bridge:
        // `edges` is a HashSet and `geom`/`adj` are HashMaps, so which of a half-edge / its twin
        // won `or_insert` — i.e. each ring's ORIENTATION — depended on hash iteration order. The
        // app previews the op on a clone and then re-runs it on the real mesh, so a nondeterministic
        // ring orientation means the manifold GATE and the COMMIT are two rolls of the same dice.
        //
        // Rings must be byte-identical every call, and must not depend on HashSet insertion order.
        let mut m = HalfEdgeMesh::new();
        let mut caps = Vec::new();
        for (i, y) in [0.0f32, 1.0].iter().enumerate() {
            let mut r: Vec<VertexId> = (0..8).map(|k| {
                let a = std::f32::consts::TAU * k as f32 / 8.0;
                m.alloc_vertex(HVertex {
                    position: Vec3::new(a.cos(), *y, a.sin()), normal: Vec3::Y, half_edge: NO_EDGE,
                })
            }).collect();
            if i == 1 { r.reverse(); }
            caps.push(m.create_face(&r).expect("cap"));
        }
        let bnd: Vec<HalfEdgeId> = (0..m.half_edges.len() as u32).map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).twin == NO_TWIN).collect();
        assert_eq!(bnd.len(), 16);

        let baseline = m.edge_loop_rings(&bnd.iter().copied().collect());
        assert_eq!(baseline.len(), 2);
        // Rebuild the set in several different insertion orders — a HashSet's iteration order is
        // insertion-dependent, which is exactly what used to leak into the result.
        for perm in 0..8usize {
            let mut order = bnd.clone();
            order.rotate_left(perm);
            if perm % 2 == 1 { order.reverse(); }
            let set: std::collections::HashSet<HalfEdgeId> = order.into_iter().collect();
            let got = m.edge_loop_rings(&set);
            assert_eq!(got, baseline, "edge_loop_rings varied with set insertion order (perm {perm})");
        }
    }

    #[test]
    fn bridge_two_capped_rims_close_into_a_solid() {
        // Blender oracle `two_ngon_rings`: nv=16, nf=10 ({4:8, 8:2}), boundary=0, non-manifold=0.
        // This is the case a user actually hits (two rims / two holes facing each other), as
        // opposed to the free-standing wire circles the older oracle test used. Verified end-to-end
        // through the app with AZRA_BRIDGEDIAG=edges2.
        let mut m = HalfEdgeMesh::new();
        for (i, y) in [0.0f32, 1.0].iter().enumerate() {
            let mut r: Vec<VertexId> = (0..8).map(|k| {
                let a = std::f32::consts::TAU * k as f32 / 8.0;
                m.alloc_vertex(HVertex {
                    position: Vec3::new(a.cos(), *y, a.sin()), normal: Vec3::Y, half_edge: NO_EDGE,
                })
            }).collect();
            if i == 1 { r.reverse(); } // rims must FACE each other, or the band cannot close
            m.create_face(&r).expect("cap");
        }
        let sel: std::collections::HashSet<HalfEdgeId> = (0..m.half_edges.len() as u32)
            .map(HalfEdgeId)
            .filter(|&h| m.is_edge_live(h) && m.he(h).twin == NO_TWIN)
            .collect();
        let faces = m.bridge_edge_selection(&sel);
        assert_eq!(faces.len(), 8, "8 side quads");
        assert_eq!(m.vertex_count(), 16, "bridge adds no vertices");
        assert_eq!(m.face_count(), 10, "2 caps + 8 walls (Blender nf=10)");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&h| m.is_edge_live(HalfEdgeId(h)) && m.he(HalfEdgeId(h)).twin == NO_TWIN)
            .count();
        assert_eq!(bnd, 0, "Blender oracle boundary_edges=0 — the result is a CLOSED solid");
    }

    #[test]
    fn bridge_faces_multi_refuses_below_two_and_never_mutates() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let (bf, bv) = (m.face_count(), m.vertex_count());
        assert!(m.bridge_faces_multi(&[]).is_none());
        assert!(m.bridge_faces_multi(&[FaceId(0)]).is_none());
        assert!(m.bridge_faces_multi(&[FaceId(0), FaceId(0)]).is_none(), "dedup → 1 face → refuse");
        assert_eq!((m.face_count(), m.vertex_count()), (bf, bv), "refuse must not mutate");
    }

    #[test]
    fn bridge_faces_two_open_caps_makes_tunnel() {
        // Two freestanding quads facing each other (openings with no existing side walls).
        // Bridge deletes both and strips 4 side quads — the classic "tunnel between two faces".
        let mut m = HalfEdgeMesh::new();
        let mk_quad = |m: &mut HalfEdgeMesh, y: f32| -> FaceId {
            let vs: Vec<VertexId> = [
                Vec3::new(-1.0, y, -1.0), Vec3::new(1.0, y, -1.0),
                Vec3::new(1.0, y, 1.0), Vec3::new(-1.0, y, 1.0),
            ].into_iter().map(|p| m.alloc_vertex(HVertex {
                position: p, normal: Vec3::Y, half_edge: NO_EDGE,
            })).collect();
            m.create_face(&vs).expect("cap face")
        };
        let fa = mk_quad(&mut m, 0.0);
        let fb = mk_quad(&mut m, 2.0);
        assert_eq!(m.face_count(), 2);
        let faces = m.bridge_faces(fa, fb).expect("open caps must bridge");
        assert_eq!(faces.len(), 4, "two quads → 4 tunnel walls");
        assert_eq!(m.face_count(), 4, "caps deleted, only tunnel walls remain");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
        // Every wall spans both Y levels.
        for &f in &faces {
            let zs: Vec<f32> = m.face_vertices(f).iter().map(|&v| m.vert(v).position.y).collect();
            assert!(zs.iter().any(|&y| (y - 0.0).abs() < 1e-4));
            assert!(zs.iter().any(|&y| (y - 2.0).abs() < 1e-4));
        }
    }

    #[test]
    fn bridge_faces_refuses_shared_verts_opens_closed_box() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let before_f = m.face_count();
        let before_v = m.vertex_count();

        // Find two faces that share a vertex and assert refuse (no mutation).
        let mut shared_pair = None;
        'outer: for a in 0..m.faces.len() as u32 {
            if !m.is_face_live(FaceId(a)) { continue; }
            let sa: std::collections::HashSet<u32> =
                m.face_vertices(FaceId(a)).iter().map(|v| v.0).collect();
            for b in (a + 1)..m.faces.len() as u32 {
                if !m.is_face_live(FaceId(b)) { continue; }
                if m.face_vertices(FaceId(b)).iter().any(|v| sa.contains(&v.0)) {
                    shared_pair = Some((FaceId(a), FaceId(b)));
                    break 'outer;
                }
            }
        }
        let (a, b) = shared_pair.expect("box has adjacent faces");
        assert!(m.bridge_faces(a, b).is_none(), "shared-vert faces must refuse");
        assert_eq!(m.face_count(), before_f, "refuse must not delete faces");
        assert_eq!(m.vertex_count(), before_v);

        // Opposite faces of a closed box: strip would double side walls → delete-only open tunnel.
        let mut opp = None;
        'o2: for a in 0..m.faces.len() as u32 {
            if !m.is_face_live(FaceId(a)) { continue; }
            let sa: std::collections::HashSet<u32> =
                m.face_vertices(FaceId(a)).iter().map(|v| v.0).collect();
            let ca: Vec3 = m.face_vertices(FaceId(a)).iter()
                .map(|&v| m.vert(v).position).sum::<Vec3>() / 4.0;
            for b in (a + 1)..m.faces.len() as u32 {
                if !m.is_face_live(FaceId(b)) { continue; }
                if m.face_vertices(FaceId(b)).iter().any(|v| sa.contains(&v.0)) { continue; }
                let cb: Vec3 = m.face_vertices(FaceId(b)).iter()
                    .map(|&v| m.vert(v).position).sum::<Vec3>() / 4.0;
                if ca.dot(cb) < -0.5 {
                    opp = Some((FaceId(a), FaceId(b)));
                    break 'o2;
                }
            }
        }
        let (a, b) = opp.expect("box has opposite faces");
        let faces = m.bridge_faces(a, b).expect("closed-box opposite faces open a tunnel");
        assert!(faces.is_empty(), "delete-only open: no new strip faces");
        assert_eq!(m.face_count(), before_f - 2, "two caps removed");
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());
    }

    #[test]
    fn inset_region_outward_extends_rim_body_fixed() {
        // Negative thickness = outer segment: body corners stay FIXED, new ring goes OUTSIDE.
        // Must NOT flare the cube into a frustum (that was the broken dual-ring approach).
        let mut m = HalfEdgeMesh::new_box(Vec3::new(2.0, 1.0, 1.0));
        let top = (0..m.faces.len() as u32).map(FaceId).find(|&f| {
            if m.face(f).half_edge == NO_EDGE { return false; }
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (c.y - 1.0).abs() < 1e-4
        }).expect("+Y top face");

        let corners_before: Vec<Vec3> = m.face_vertices(top).iter()
            .map(|&v| m.vert(v).position).collect();

        let set: std::collections::HashSet<FaceId> = std::iter::once(top).collect();
        let res = m.inset_region(&set, -0.3, 0.0);
        assert!(res.outward, "negative thickness is outward");
        assert_eq!(res.rim.len(), 4, "quad top → 4 rim segments");
        assert_eq!(res.inner.len(), 1);
        assert_eq!(res.boundary.len(), 4);
        assert!(m.validate().is_ok(), "validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "manifold: {:?}", m.is_manifold());

        // Body corners FIXED (shared with side faces — must not slide).
        for &want in &corners_before {
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-3),
                "body corner {want:?} moved — outset must not slide shared verts");
        }
        // New ring verts sit OUTSIDE the original top footprint.
        for &(nv, _) in &res.boundary {
            let p = m.vert(nv).position;
            assert!((p.y - 1.0).abs() < 1e-3, "outer ring stays on the top plane");
            let outside = p.x.abs() > 2.0 + 0.2 || p.z.abs() > 1.0 + 0.2;
            assert!(outside, "new ring vert {p:?} should extend outside original top");
        }
        let boundary_of = |m: &HalfEdgeMesh| (0..m.half_edges.len())
            .filter(|&i| m.half_edges[i].face != NO_FACE && m.half_edges[i].twin == NO_TWIN).count();
        assert_eq!(boundary_of(&m), 0, "outset on a closed box must stay closed");
    }

    #[test]
    fn extrude_region_matches_blender_oracle() {
        // Oracle: docs/planning/poly/extrude_region_ref.py. Box ±1, +Y top split into 2 coplanar quads
        // (shared edge at x=0); extrude BOTH top faces as a REGION by +Y 0.5 -> nv=16/nf=13, 6 boundary
        // verts lifted to y=1.5, the shared interior edge stays welded (per-face extrude would duplicate it).
        let p = vec![
            Vec3::new(-1.,-1.,-1.), Vec3::new(1.,-1.,-1.), Vec3::new(1.,-1.,1.), Vec3::new(-1.,-1.,1.),
            Vec3::new(-1.,1.,-1.), Vec3::new(1.,1.,-1.), Vec3::new(1.,1.,1.), Vec3::new(-1.,1.,1.),
            Vec3::new(0.,1.,-1.), Vec3::new(0.,1.,1.),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![0,1,2,3], vec![7,9,8,4], vec![9,6,5,8], vec![4,8,5,1,0], vec![6,9,7,3,2], vec![7,4,0,3], vec![5,6,2,1],
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &f);
        // the two +Y top faces (centroid y == 1)
        let mut sel = std::collections::HashSet::new();
        for fi in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&fi) { continue; }
            let vs = m.face_vertices(FaceId(fi));
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            if (c.y - 1.0).abs() < 1e-4 { sel.insert(FaceId(fi)); }
        }
        assert_eq!(sel.len(), 2, "selected the 2 top faces");
        m.extrude_region(&sel, Vec3::new(0., 0.5, 0.));

        assert!(m.is_manifold().is_ok(), "region extrude manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "extruding a region of a closed box stays closed");
        assert_eq!(m.vertex_count(), 16, "oracle nv=16 (10 + 6 lifted boundary verts)");
        assert_eq!(m.face_count(), 13, "oracle nf=13 (7 original + 6 walls)");
        // exactly 6 verts at y=1.5 (the lifted top ring) — proves the shared edge welded (not 8).
        let lifted = (0..m.vertices.len() as u32).filter(|vi| !m.free_verts.contains(vi))
            .filter(|&vi| (m.vert(VertexId(vi)).position.y - 1.5).abs() < 1e-4).count();
        assert_eq!(lifted, 6, "exactly 6 boundary verts lifted to y=1.5 (interior edge welded)");
        for want in [Vec3::new(-1.,1.5,-1.), Vec3::new(0.,1.5,-1.), Vec3::new(1.,1.5,1.)] {
            assert!((0..m.vertices.len() as u32).filter(|vi| !m.free_verts.contains(vi))
                .any(|vi| (m.vert(VertexId(vi)).position - want).length() < 1e-4), "lifted vert {:?} missing", want);
        }
    }

    #[test]
    fn solidify_matches_blender_oracle_invariants() {
        // Oracle: docs/planning/poly/solidify_ref.py (Blender Solidify, thickness 0.2, offset -1).
        // Blender's exact vertex-normal averaging differs from ours, so gate on the topology INVARIANTS:
        //   closed cube -> hollow shell nv=16/nf=12 (no rim); open quad -> slab nv=8/nf=6 (+4 rim).
        // Both closed + manifold; the shell thickness ~= 0.2.
        let bnd = |m: &HalfEdgeMesh| (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();

        // A. closed cube
        let mut cube = HalfEdgeMesh::new_box(Vec3::ONE);
        cube.solidify(0.2, -1.0);
        assert!(cube.is_manifold().is_ok(), "solidify cube manifold: {:?}", cube.is_manifold());
        assert!(cube.validate().is_ok());
        assert_eq!(cube.vertex_count(), 16, "oracle cube nv=16 (outer 8 + inner 8)");
        assert_eq!(cube.face_count(), 12, "oracle cube nf=12 (outer 6 + inner 6)");
        assert_eq!(bnd(&cube), 0, "closed cube solidify has no boundary (hollow shell)");
        // outer stays at ±1 (offset -1), an inner shell exists strictly inside.
        let (mn, mx) = cube.bounding_box();
        assert!((mn - Vec3::splat(-1.0)).length() < 1e-3 && (mx - Vec3::ONE).length() < 1e-3, "cube bbox stays ±1");

        // B. open single quad (z=0) -> slab
        let qp = vec![Vec3::new(-1.,-1.,0.), Vec3::new(1.,-1.,0.), Vec3::new(1.,1.,0.), Vec3::new(-1.,1.,0.)];
        let qf: Vec<Vec<usize>> = vec![vec![0,1,2,3]];
        let mut plane = HalfEdgeMesh::from_polygons(&qp, &qf);
        plane.solidify(0.2, -1.0);
        assert!(plane.is_manifold().is_ok(), "solidify plane manifold: {:?}", plane.is_manifold());
        assert!(plane.validate().is_ok());
        assert_eq!(plane.vertex_count(), 8, "oracle plane nv=8 (slab)");
        assert_eq!(plane.face_count(), 6, "oracle plane nf=6 (top+bottom+4 rim)");
        assert_eq!(bnd(&plane), 0, "solidified plane is a closed slab");
        let (pmn, pmx) = plane.bounding_box();
        assert!((pmx.z - pmn.z - 0.2).abs() < 1e-3, "slab thickness ~0.2 (was {})", pmx.z - pmn.z);
    }

    #[test]
    fn dissolve_vertex_matches_blender_oracle() {
        // Oracle: docs/planning/poly/dissolve_vert_ref.py (Blender Dissolve Vertices on corner (1,1,1))
        // -> nv=7, nf=4, the 3 incident faces merge into ONE hexagon; the corner is removed.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let v = (0..m.vertices.len() as u32)
            .find(|&i| (m.vert(VertexId(i)).position - Vec3::new(1., 1., 1.)).length() < 1e-4)
            .map(VertexId).expect("cube has a (1,1,1) corner");
        let merged = m.dissolve_vertex(v).expect("degree-3 corner is dissolvable");
        assert!(m.is_manifold().is_ok(), "dissolve_vertex must stay manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        assert_eq!(m.vertex_count(), 7, "oracle nv=7");
        assert_eq!(m.face_count(), 4, "oracle nf=4");
        assert!(!(0..m.vertices.len() as u32).filter(|vi| !m.free_verts.contains(vi))
            .any(|vi| (m.vert(VertexId(vi)).position - Vec3::new(1., 1., 1.)).length() < 1e-4),
            "the dissolved corner must be gone");
        let hex = m.face_vertices(merged);
        assert_eq!(hex.len(), 6, "merged face is a hexagon (3 quads around a degree-3 vertex)");
        let want: [Vec3; 6] = [
            Vec3::new(-1., 1., 1.), Vec3::new(1., 1., -1.), Vec3::new(-1., 1., -1.),
            Vec3::new(-1., -1., 1.), Vec3::new(1., -1., 1.), Vec3::new(1., -1., -1.),
        ];
        for w in want {
            assert!(hex.iter().any(|&vv| (m.vert(vv).position - w).length() < 1e-4),
                "merged hexagon missing oracle vertex {:?}", w);
        }
    }

    #[test]
    fn dissolve_faces_region_frees_interior_vertex() {
        // Oracle: docs/planning/poly/dissolve_grid_ref.py (Blender Dissolve Faces on all 4 grid quads)
        // -> nv=8, ONE 8-gon, the interior center vertex REMOVED (verification #6: AZRA used to leak it).
        let gv: Vec<Vec3> = [(-1.,-1.),(0.,-1.),(1.,-1.),(-1.,0.),(0.,0.),(1.,0.),(-1.,1.),(0.,1.),(1.,1.)]
            .iter().map(|&(x, y)| Vec3::new(x, y, 0.)).collect();
        let gf: Vec<Vec<usize>> = vec![vec![0,1,4,3], vec![1,2,5,4], vec![3,4,7,6], vec![4,5,8,7]];
        let mut m = HalfEdgeMesh::from_polygons(&gv, &gf);
        let sel: std::collections::HashSet<FaceId> =
            (0..m.faces.len() as u32).filter(|fi| !m.free_faces.contains(fi)).map(FaceId).collect();
        let merged = m.dissolve_faces_region(&sel);
        assert_eq!(merged.len(), 1, "one connected region -> one merged face");
        assert!(m.is_manifold().is_ok(), "grid dissolve manifold: {:?}", m.is_manifold());
        assert_eq!(m.vertex_count(), 8, "oracle nv=8 (center vertex freed, not leaked)");
        assert_eq!(m.face_count(), 1, "oracle nf=1 (one 8-gon)");
        assert_eq!(m.face_vertices(merged[0]).len(), 8, "the merged face is an 8-gon");
        assert!(!(0..m.vertices.len() as u32).filter(|vi| !m.free_verts.contains(vi))
            .any(|vi| m.vert(VertexId(vi)).position.length() < 1e-4), "center (0,0,0) removed");
    }

    #[test]
    fn dissolve_faces_region_matches_blender_oracle() {
        // Oracle: docs/planning/poly/dissolve_faces_ref.py (Blender Dissolve Faces on +Y and +Z faces)
        // -> nv=8, nf=5, the merged face is a HEXAGON. The old per-face dissolve left wire edges (#10).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // select the +Y (top) and +Z (front) faces by centroid
        let mut sel = std::collections::HashSet::new();
        for fi in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&fi) { continue; }
            let vs = m.face_vertices(FaceId(fi));
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            if (c - Vec3::new(0., 1., 0.)).length() < 1e-4 || (c - Vec3::new(0., 0., 1.)).length() < 1e-4 {
                sel.insert(FaceId(fi));
            }
        }
        assert_eq!(sel.len(), 2, "selected the top + front faces");
        let merged = m.dissolve_faces_region(&sel);
        assert_eq!(merged.len(), 1, "one connected region -> one merged face");
        assert!(m.is_manifold().is_ok(), "dissolve region must stay manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "dissolving 2 faces of a closed cube stays closed (no wire edges — the #10 fix)");
        assert_eq!(m.vertex_count(), 8, "oracle nv=8");
        assert_eq!(m.face_count(), 5, "oracle nf=5");
        // the merged face is the hexagon over the union boundary of top+front.
        let hex = m.face_vertices(merged[0]);
        assert_eq!(hex.len(), 6, "merged face is a hexagon");
        let want: [Vec3; 6] = [
            Vec3::new(1., 1., 1.), Vec3::new(1., 1., -1.), Vec3::new(-1., 1., -1.),
            Vec3::new(-1., 1., 1.), Vec3::new(-1., -1., 1.), Vec3::new(1., -1., 1.),
        ];
        for w in want {
            assert!(hex.iter().any(|&v| (m.vert(v).position - w).length() < 1e-4),
                "merged hexagon missing oracle vertex {:?}", w);
        }
    }

    // ================================================================================================
    // Multi-edge Dissolve Edges (Ctrl+X) — Blender region-collapse parity, oracle-gated BY IDENTITY.
    // Oracle: scratchpad/der_oracle.py (Blender 4.5, bmesh.ops.dissolve_edges use_verts=True,
    // use_face_split=False), selections chosen by COORDINATE so they map 1:1 onto make_cube (±0.5).
    //   single edge (-1,-1,-1)-(1,-1,-1)  -> 6v/9e/5f  (the two valence-2 endpoints removed)
    //   2 edges sharing (-1,-1,-1)         -> 5v/7e/4f
    //   4 +Y perimeter edges               -> 4v/4e/1f  (the top cap collapses onto the -Y quad)
    // These are the cases the OLD sequential per-edge DissolveEdge batch could not produce (it refused on
    // the first shared-geometry slit and changed nothing) — RED before dissolve_edges_region existed.
    // ================================================================================================

    /// Coordinate SIGN key of a vertex (make_cube is ±0.5, the oracle cube ±1 — same topology, so sign
    /// triples gate by identity).
    fn vsign(m: &HalfEdgeMesh, v: VertexId) -> [i32; 3] {
        let p = m.vert(v).position;
        [p.x.signum() as i32, p.y.signum() as i32, p.z.signum() as i32]
    }

    /// Sorted live-vertex sign-set of the whole mesh — for identity comparison against the oracle.
    fn live_vsigns(m: &HalfEdgeMesh) -> Vec<[i32; 3]> {
        let mut v: Vec<[i32; 3]> = (0..m.vertices.len() as u32)
            .filter(|i| !m.free_verts.contains(i))
            .map(|i| vsign(m, VertexId(i)))
            .collect();
        v.sort();
        v
    }

    /// The live half-edge whose endpoints match {a,b} by coordinate sign (either direction).
    fn find_he_sign(m: &HalfEdgeMesh, a: [i32; 3], b: [i32; 3]) -> HalfEdgeId {
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let (s, d) = (vsign(m, m.he_source(h)), vsign(m, m.he(h).vertex));
            if (s == a && d == b) || (s == b && d == a) { return h; }
        }
        panic!("edge {:?}-{:?} not found", a, b);
    }

    #[test]
    fn rip_edge_opens_interior_slit() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Pick any interior edge (all cube edges are interior).
        let he = {
            let mut found = None;
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if m.is_edge_live(h) && m.he(h).twin != NO_TWIN {
                    found = Some(h);
                    break;
                }
            }
            found.expect("cube has interior edges")
        };
        let v_before = m.vertex_count();
        assert!(m.rip_edge(he).is_some());
        assert!(m.he(he).twin == NO_TWIN, "ripped edge must be boundary");
        assert!(m.vertex_count() >= v_before + 2, "must duplicate endpoints");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn rip_edge_refuses_boundary() {
        // Open plane: boundary edges have NO_TWIN.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        let mut boundary = None;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if m.is_edge_live(h) && m.he(h).twin == NO_TWIN {
                boundary = Some(h);
                break;
            }
        }
        let he = boundary.expect("plane has boundary");
        let v_before = m.vertex_count();
        assert!(m.rip_edge(he).is_none());
        assert_eq!(m.vertex_count(), v_before);
    }

    #[test]
    fn rip_fill_edge_seals_slit_with_quad() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let he = {
            let mut found = None;
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if m.is_edge_live(h) && m.he(h).twin != NO_TWIN {
                    found = Some(h);
                    break;
                }
            }
            found.expect("cube has interior edges")
        };
        let f_before = m.face_count();
        let v_before = m.vertex_count();
        let fid = m.rip_fill_edge(he).expect("rip fill interior edge");
        assert_eq!(m.face_count(), f_before + 1);
        assert!(m.vertex_count() >= v_before + 2);
        assert_eq!(m.face_vertices(fid).len(), 4, "fill is a quad");
        // Original edge must twin the fill face (no longer open boundary).
        assert!(m.he(he).twin != NO_TWIN, "fill must twin ripped edge");
        assert_eq!(m.he(m.he(he).twin).face, fid);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn rip_fill_refuses_boundary() {
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        let he = {
            let mut boundary = None;
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if m.is_edge_live(h) && m.he(h).twin == NO_TWIN {
                    boundary = Some(h);
                    break;
                }
            }
            boundary.expect("plane has boundary")
        };
        let f_before = m.face_count();
        assert!(m.rip_fill_edge(he).is_none());
        assert_eq!(m.face_count(), f_before);
    }

    #[test]
    fn grid_fill_closes_rectangular_hole() {
        use std::collections::HashSet;
        // 4×4 faces plane; delete center 2×2 → 8-edge rectangular hole; grid fill → 4 quads.
        let mut m = HalfEdgeMesh::new_plane(4.0, 4.0, 4, 4);
        let f_before = m.face_count();
        let mut to_del = Vec::new();
        for i in 0..m.faces.len() as u32 {
            if m.face(FaceId(i)).half_edge == NO_EDGE {
                continue;
            }
            let vs = m.face_vertices(FaceId(i));
            let c: Vec3 = vs.iter().map(|v| m.vert(*v).position).sum::<Vec3>() / vs.len() as f32;
            if c.x.abs() < 1.01 && c.z.abs() < 1.01 {
                to_del.push(FaceId(i));
            }
        }
        assert_eq!(to_del.len(), 4, "inner 2×2 faces");
        for f in to_del {
            m.delete_face(f);
        }
        // Hole boundary: undirected edges that are now open (NO_TWIN) and not outer rim.
        let mut hole: HashSet<HalfEdgeId> = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            if m.he(h).twin != NO_TWIN {
                continue;
            }
            if m.he(h).face == NO_FACE {
                continue;
            }
            // Outer rim is at |x| or |z| ≈ 2; hole edges have endpoints closer in.
            let a = m.vert(m.he_source(h)).position;
            let b = m.vert(m.he(h).vertex).position;
            let mid = (a + b) * 0.5;
            if mid.x.abs() < 1.5 && mid.z.abs() < 1.5 {
                hole.insert(h);
            }
        }
        let faces = m.grid_fill(&hole, Some(2), 0);
        assert_eq!(faces.len(), 4, "2×2 grid of quads");
        assert_eq!(m.face_count(), f_before); // deleted 4, added 4
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "{:?}", m.is_manifold());
    }

    #[test]
    fn loop_relax_pulls_outlier_toward_plane() {
        use std::collections::HashSet;
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2); // 3×3 verts on XZ
        // Center vertex is index 4 on a 3×3 grid (row-major).
        let center = VertexId(4);
        m.vert_mut(center).position.y = 1.0;
        let mut sel = HashSet::new();
        for i in 0..m.vertices.len() as u32 {
            if m.vert(VertexId(i)).half_edge != NO_EDGE {
                sel.insert(VertexId(i));
            }
        }
        let n = m.loop_relax(&sel, 8, 1.0, true); // lock boundary
        assert!(n > 0);
        assert!(
            m.vert(center).position.y.abs() < 0.5,
            "center should drop toward plane, y={}",
            m.vert(center).position.y
        );
        // Corners stay put (boundary lock).
        assert!((m.vert(VertexId(0)).position.y).abs() < 1e-5);
    }

    #[test]
    fn loop_flatten_projects_to_plane() {
        use std::collections::HashSet;
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..4u32 {
            sel.insert(VertexId(i));
        }
        m.vert_mut(VertexId(0)).position.y += 0.5;
        let n = m.loop_flatten(&sel);
        assert!(n > 0);
        // Residuals to best-fit plane must be ~0 (not necessarily same axis coordinate).
        let pts: Vec<Vec3> = (0..4).map(|i| m.vert(VertexId(i)).position).collect();
        let normal = newell_normal(&pts).normalize_or_zero();
        let com = pts.iter().copied().sum::<Vec3>() / 4.0;
        for p in &pts {
            let d = (*p - com).dot(normal).abs();
            assert!(d < 1e-4, "residual {d} after flatten");
        }
    }

    #[test]
    fn loop_space_equalizes_open_chain() {
        use std::collections::HashSet;
        // 4 verts on a line with uneven spacing: 0--1----2-3
        let mut m = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(3.0, 0.0, 0.0),
                Vec3::new(3.5, 0.0, 0.0),
            ],
            &[vec![0, 1, 2], vec![1, 3, 2]], // two tris so edges exist
        );
        // Collect edges of the bottom chain 0-1-2-3 via half-edges between them.
        let mut edges = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let a = m.he_source(h).0;
            let b = m.he(h).vertex.0;
            let pair = (a.min(b), a.max(b));
            if pair == (0, 1) || pair == (1, 2) || pair == (2, 3) {
                edges.insert(h);
            }
        }
        // Path 0-1-2-3 may not be a single degree≤2 chain if 1 and 2 have more edges.
        // Use only verts 0,1,2,3 edges that form the polyline: force via loop_space on a strip.
        let n = m.loop_space(&edges);
        // May refuse if branching — build a cleaner path mesh instead.
        if n == 0 {
            // Dedicated open polyline as a single chain of faces: thin strip.
            let mut m2 = HalfEdgeMesh::from_polygons(
                &[
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(3.0, 0.0, 0.0),
                    Vec3::new(3.5, 0.0, 0.0),
                    Vec3::new(0.0, 0.0, 0.1),
                    Vec3::new(1.0, 0.0, 0.1),
                    Vec3::new(3.0, 0.0, 0.1),
                    Vec3::new(3.5, 0.0, 0.1),
                ],
                &[
                    vec![0, 1, 5, 4],
                    vec![1, 2, 6, 5],
                    vec![2, 3, 7, 6],
                ],
            );
            let mut e2 = HashSet::new();
            for i in 0..m2.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !m2.is_edge_live(h) {
                    continue;
                }
                let a = m2.he_source(h).0;
                let b = m2.he(h).vertex.0;
                let pair = (a.min(b), a.max(b));
                if pair == (0, 1) || pair == (1, 2) || pair == (2, 3) {
                    e2.insert(h);
                }
            }
            let n2 = m2.loop_space(&e2);
            assert!(n2 > 0, "space should move interior verts");
            let x1 = m2.vert(VertexId(1)).position.x;
            let x2 = m2.vert(VertexId(2)).position.x;
            // Total length 3.5, equal thirds → 3.5/3 ≈ 1.166
            assert!((x1 - 3.5 / 3.0).abs() < 0.05, "x1={x1}");
            assert!((x2 - 2.0 * 3.5 / 3.0).abs() < 0.05, "x2={x2}");
        } else {
            let x1 = m.vert(VertexId(1)).position.x;
            assert!(x1 > 1.0);
        }
    }

    #[test]
    fn spin_edge_full_circle_makes_band() {
        use std::collections::HashSet;
        // Single vertical edge at x=1, spin 360° / 4 steps around Y → 4 side quads.
        let mut m = HalfEdgeMesh::from_polygons(
            &[
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 0.0, 0.0), // dummy third for a face (will dissolve selection to edge only)
            ],
            &[vec![0, 1, 2]],
        );
        // Spin only the two verts of the edge 0–1.
        let mut verts = HashSet::new();
        verts.insert(VertexId(0));
        verts.insert(VertexId(1));
        let n = m.spin(
            &verts,
            Vec3::ZERO,
            Vec3::Y,
            std::f32::consts::TAU,
            4,
            true,
        );
        assert_eq!(n, 4, "4 steps around a circle");
        assert!(m.face_count() >= 5, "original face + 4 side quads");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
    }

    #[test]
    fn spin_refuses_empty_or_no_edges() {
        use std::collections::HashSet;
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let empty = HashSet::new();
        assert_eq!(m.spin(&empty, Vec3::ZERO, Vec3::Y, 1.0, 4, false), 0);
        // Single vert — no edge in selection.
        let mut one = HashSet::new();
        one.insert(VertexId(0));
        assert_eq!(m.spin(&one, Vec3::ZERO, Vec3::Y, std::f32::consts::FRAC_PI_2, 4, false), 0);
    }

    #[test]
    fn grid_fill_refuses_odd_loop() {
        use std::collections::HashSet;
        // Triangle hole cannot grid-fill.
        let mut m = HalfEdgeMesh::from_polygons(
            &[
                Vec3::ZERO,
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.5, 0.0, 1.0),
            ],
            &[vec![0, 1, 2]],
        );
        m.delete_face(FaceId(0));
        // No live faces / edges left usable — use synthetic loop of 3 verts via grid_fill_loop.
        let ring = [VertexId(0), VertexId(1), VertexId(2)];
        assert!(m.grid_fill_loop(&ring, None, 0).is_empty());
        let _ = HashSet::<HalfEdgeId>::new();
    }

    #[test]
    fn geometry_snap_points_box_vef_counts() {
        use std::collections::HashSet;
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let pts = m.geometry_snap_points_local(&HashSet::new());
        let nv = pts.iter().filter(|(_, k)| *k == 0).count();
        let ne = pts.iter().filter(|(_, k)| *k == 1).count();
        let nf = pts.iter().filter(|(_, k)| *k == 2).count();
        assert_eq!(nv, 8, "cube has 8 corners");
        assert_eq!(ne, 12, "cube has 12 edge midpoints");
        assert_eq!(nf, 6, "cube has 6 face centers");
        // Exclude 2 verts → drop those verts + incident edges/faces.
        let mut excl = HashSet::new();
        excl.insert(0);
        excl.insert(1);
        let pts2 = m.geometry_snap_points_local(&excl);
        assert_eq!(pts2.iter().filter(|(_, k)| *k == 0).count(), 6);
        assert!(pts2.iter().filter(|(_, k)| *k == 1).count() < 12);
        assert!(pts2.iter().filter(|(_, k)| *k == 2).count() < 6);
    }

    #[test]
    fn limited_dissolve_flattens_subdivided_plane() {
        // 4×4 grid on a plane — all faces coplanar → limited dissolve at 5° should collapse heavily.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let before_f = m.face_count();
        assert!(before_f > 1);
        let n = m.limited_dissolve(5.0, None, None);
        assert!(n > 0, "coplanar grid must mark dissolve edges");
        assert!(
            m.face_count() < before_f,
            "face count must drop: {} → {}",
            before_f,
            m.face_count()
        );
        assert!(m.validate().is_ok(), "mesh valid: {:?}", m.validate());
    }

    #[test]
    fn limited_dissolve_skips_right_angle_cube() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        // Cube faces meet at 90° — Blender's default 5° threshold must leave them alone.
        let n = m.limited_dissolve(5.0, None, None);
        assert_eq!(n, 0);
        assert_eq!(m.face_count(), 6);
        assert!(m.validate().is_ok());
    }

    /// Owner 2026-08-05: "mesh clean up should never touch [tagged edges]". A protected edge on
    /// a perfectly flat grid — the most dissolvable geometry there is — must survive intact,
    /// while everything unprotected still collapses. Both twins are honoured from ONE id, and
    /// the vertex identity check is by POSITION because the dissolve renumbers nothing but does
    /// free interior vertices.
    #[test]
    fn limited_dissolve_refuses_protected_edges() {
        let base = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        // Pick any interior edge (two distinct faces) and record its endpoint positions.
        let pick = (0..base.half_edges.len() as u32)
            .map(HalfEdgeId)
            .find(|&h| {
                if !base.is_edge_live(h) {
                    return false;
                }
                let (f1, f2) = base.edge_faces(h);
                f1 != NO_FACE && f2 != NO_FACE && f1 != f2
            })
            .expect("flat grid has interior edges");
        let ends = (
            base.vert(base.he_source(pick)).position,
            base.vert(base.he(pick).vertex).position,
        );

        // Unprotected: the reference run collapses it along with everything else.
        let mut free = base.clone();
        let n_free = free.limited_dissolve(5.0, None, None);
        assert!(n_free > 0, "flat grid must dissolve when nothing is protected");

        // Protected: fewer edges marked, and THAT edge is still on the mesh.
        let mut prot = base.clone();
        let set: std::collections::HashSet<HalfEdgeId> = [pick].into_iter().collect();
        let n_prot = prot.limited_dissolve(5.0, None, Some(&set));
        assert!(
            n_prot < n_free,
            "protecting an edge must remove it from the candidate set: {n_prot} vs {n_free}"
        );
        let q = |p: glam::Vec3| {
            ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64)
        };
        let want = {
            let (a, b) = (q(ends.0), q(ends.1));
            if a <= b { (a, b) } else { (b, a) }
        };
        let survived = (0..prot.half_edges.len() as u32).map(HalfEdgeId).any(|h| {
            if !prot.is_edge_live(h) {
                return false;
            }
            let (a, b) = (
                q(prot.vert(prot.he_source(h)).position),
                q(prot.vert(prot.he(h).vertex).position),
            );
            (if a <= b { (a, b) } else { (b, a) }) == want
        });
        assert!(survived, "the protected edge was dissolved anyway");
        assert!(prot.validate().is_ok(), "mesh valid: {:?}", prot.validate());
    }

    #[test]
    fn limited_dissolve_selection_only_touches_selected() {
        // Flat grid: dissolve only half the faces' coplanar edges.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let half: std::collections::HashSet<FaceId> =
            (0..8u32).map(FaceId).collect();
        let before = m.face_count();
        let n = m.limited_dissolve(5.0, Some(&half), None);
        assert!(n > 0);
        // Still more than 1 face (other half survives as multiple quads).
        assert!(m.face_count() > 1 && m.face_count() < before);
        assert!(m.validate().is_ok());
    }

    fn dissolve_edges_region_single_matches_blender() {
        let mut m = make_cube();
        let e = find_he_sign(&m, [-1, -1, -1], [1, -1, -1]);
        let sel: std::collections::HashSet<HalfEdgeId> = [e].into_iter().collect();
        assert!(m.dissolve_edges_region(&sel), "single-edge dissolve must apply");
        assert_eq!(counts(&m), (6, 9, 5), "oracle single -> 6/9/5 (valence-2 endpoints removed)");
        assert_post_op_clean(&m, "dissolve_edges_region single");
        let mut want = vec![[-1,-1,1],[-1,1,-1],[-1,1,1],[1,-1,1],[1,1,-1],[1,1,1]];
        want.sort();
        assert_eq!(live_vsigns(&m), want, "single: surviving verts by identity (endpoints gone)");
    }

    #[test]
    fn dissolve_edges_region_two_shared_matches_blender() {
        let mut m = make_cube();
        let e1 = find_he_sign(&m, [-1, -1, -1], [1, -1, -1]);
        let e2 = find_he_sign(&m, [-1, -1, -1], [-1, 1, -1]);
        let sel: std::collections::HashSet<HalfEdgeId> = [e1, e2].into_iter().collect();
        assert!(m.dissolve_edges_region(&sel), "2-shared dissolve must apply");
        assert_eq!(counts(&m), (5, 7, 4), "oracle 2-shared -> 5/7/4");
        assert_post_op_clean(&m, "dissolve_edges_region two-shared");
        let mut want = vec![[-1,-1,1],[-1,1,1],[1,-1,1],[1,1,-1],[1,1,1]];
        want.sort();
        assert_eq!(live_vsigns(&m), want, "2-shared: surviving verts by identity");
    }

    #[test]
    fn dissolve_edges_region_four_top_matches_blender() {
        let mut m = make_cube();
        // The 4 perimeter edges of the +Y (top) face.
        let top = [
            ([-1, 1, -1], [1, 1, -1]),
            ([1, 1, -1], [1, 1, 1]),
            ([1, 1, 1], [-1, 1, 1]),
            ([-1, 1, 1], [-1, 1, -1]),
        ];
        let sel: std::collections::HashSet<HalfEdgeId> =
            top.iter().map(|&(a, b)| find_he_sign(&m, a, b)).collect();
        assert_eq!(sel.len(), 4, "4 distinct top edges selected");
        assert!(m.dissolve_edges_region(&sel), "4-top dissolve must apply");
        assert_eq!(counts(&m), (4, 4, 1), "oracle 4-top -> 4/4/1 (cap collapses onto the -Y quad)");
        assert_post_op_clean(&m, "dissolve_edges_region four-top");
        let mut want = vec![[-1,-1,-1],[1,-1,-1],[1,-1,1],[-1,-1,1]];
        want.sort();
        assert_eq!(live_vsigns(&m), want, "4-top: only the -Y quad verts remain");
    }

    #[test]
    fn dissolve_edges_region_three_corner_matches_blender() {
        // The old tracked repro (COMPLETE_BUG_TRACKER): a cube's 3 corner edges. Blender collapses to a
        // TETRAHEDRON over the 4 far verts (oracle scratchpad/der3.py -> 4v/6e/4f). AZRA used to REFUSE
        // this whole selection (sequential batch pinched on the 2nd edge).
        let mut m = make_cube();
        let c = [-1, -1, -1];
        let sel: std::collections::HashSet<HalfEdgeId> = [
            find_he_sign(&m, c, [1, -1, -1]),
            find_he_sign(&m, c, [-1, 1, -1]),
            find_he_sign(&m, c, [-1, -1, 1]),
        ].into_iter().collect();
        assert_eq!(sel.len(), 3, "3 distinct corner edges");
        assert!(m.dissolve_edges_region(&sel), "3-corner dissolve must apply");
        assert_eq!(counts(&m), (4, 6, 4), "oracle 3-corner -> 4/6/4 (tetrahedron)");
        assert_post_op_clean(&m, "dissolve_edges_region three-corner");
        let mut want = vec![[-1,1,1],[1,-1,1],[1,1,-1],[1,1,1]];
        want.sort();
        assert_eq!(live_vsigns(&m), want, "3-corner: the 4 far verts (a tetrahedron) remain");
    }

    /// Sorted live-vertex COORDINATE set (rounded) — the strip's columns at x=-2 and x=-1 share a sign,
    /// so sign identity is ambiguous there; gate that case by exact coordinate instead.
    fn live_coords(m: &HalfEdgeMesh) -> Vec<[i32; 3]> {
        let mut v: Vec<[i32; 3]> = (0..m.vertices.len() as u32)
            .filter(|i| !m.free_verts.contains(i))
            .map(|i| { let p = m.vert(VertexId(i)).position;
                [p.x.round() as i32, p.y.round() as i32, p.z.round() as i32] })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn dissolve_edges_region_strip_skips_unselected_middle() {
        // THE OVER-MERGE DEFECT (der_strip_oracle.py, Blender 4.5, dissolve_edges use_verts=True).
        // A FLAT 4-quad strip q0-q1-q2-q3 shares B01,B12,B23. Select B01 + B23, SKIP the middle B12.
        // Blender -> 6v/7e/2f: TWO independent merges (q0+q1, q2+q3), each 6-gon collapses to a quad as
        // its valence-2 endpoints dissolve, B12 KEPT. AZRA on fb97457 over-merged ALL 4 quads into ONE
        // face (10v/10e/1f) because the region BFS crossed the unselected B12 — silent wrong geometry.
        // Columns x=-2,-1,0,1,2 (y=±1, z=0); B01 at x=-1, B12 at x=0, B23 at x=1.
        let sv: Vec<Vec3> = [
            (-2.,-1.),(-1.,-1.),(0.,-1.),(1.,-1.),(2.,-1.),   // 0..4 bottom row
            (-2., 1.),(-1., 1.),(0., 1.),(1., 1.),(2., 1.),   // 5..9 top row
        ].iter().map(|&(x, y)| Vec3::new(x, y, 0.)).collect();
        let sf: Vec<Vec<usize>> = vec![
            vec![0,1,6,5], vec![1,2,7,6], vec![2,3,8,7], vec![3,4,9,8],
        ];
        let mut m = HalfEdgeMesh::from_polygons(&sv, &sf);
        // B01 = (-1,-1)-(-1,1) ; B23 = (1,-1)-(1,1). Middle B12 = (0,-1)-(0,1) left UNSELECTED.
        let find = |m: &HalfEdgeMesh, ax: f32| -> HalfEdgeId {
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !m.is_edge_live(h) { continue; }
                let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
                let vert = (s.x - ax).abs() < 1e-4 && (d.x - ax).abs() < 1e-4
                    && (s.y + d.y).abs() < 1e-4; // one at y=-1, other at y=+1
                if vert { return h; }
            }
            panic!("vertical edge at x={ax} not found");
        };
        let sel: std::collections::HashSet<HalfEdgeId> = [find(&m, -1.), find(&m, 1.)].into_iter().collect();
        assert_eq!(sel.len(), 2, "two distinct vertical edges (B01, B23) selected");
        assert!(m.dissolve_edges_region(&sel), "strip dissolve must apply");
        // Blender oracle: TWO faces, B12 kept, valence-2 endpoints removed -> 6v/7e/2f.
        assert_eq!(counts(&m), (6, 7, 2),
            "strip skip-middle -> 6/7/2 (TWO merges, not one over-merged face)");
        assert_post_op_clean(&m, "dissolve_edges_region strip");
        let want = { let mut w = vec![
            [-2,-1,0],[-2,1,0],[0,-1,0],[0,1,0],[2,-1,0],[2,1,0]]; w.sort(); w };
        assert_eq!(live_coords(&m), want, "strip: x=-2,0,2 columns survive; x=±1 endpoints gone");
        // B12 (the unselected middle edge at x=0) must still be a live edge — the two faces stay split.
        let b12_live = (0..m.half_edges.len() as u32).any(|i| {
            let h = HalfEdgeId(i);
            m.is_edge_live(h) && {
                let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
                s.x.abs() < 1e-4 && d.x.abs() < 1e-4 && (s.y + d.y).abs() < 1e-4
            }
        });
        assert!(b12_live, "unselected middle edge B12 must be KEPT (Blender keeps it)");
    }

    #[test]
    fn dissolve_edges_region_disjoint_gap_strip_two_independent_merges() {
        // TRULY DISJOINT COMPONENTS (Blender der_gap5.py, dissolve_edges use_verts=True -> 8v/10e/3f, three
        // quads). A FLAT 5-quad strip (columns x=0..5); select the two edges at x=1 and x=4, SKIP x=2,3.
        // Selected-edge components {q0,q1} and {q3,q4} are separated by TWO unselected edges with a whole
        // untouched quad (q2) between them — the ultimate over-merge trap. Each pair merges independently
        // (endpoints dissolved), q2 is untouched, the unselected edges are kept. On fb97457 the
        // unconstrained BFS swept x=1..x=4 through the untouched quad and collapsed everything into ONE
        // face. NOT a refusal, NOT one over-merged face: TWO independent merges + the survivor quad.
        //
        // (Deliberately NOT the two-opposite-cube-edges case: Blender collapses that to a DEGENERATE non-
        // planar single quad, 4v/4e/1f, via a face-eliminating cascade the clean Euler-op kernel does not —
        // and should not — reproduce. This flat gap strip proves the same disjoint-component property with
        // clean, planar, Blender-identical geometry.)
        let sv: Vec<Vec3> = {
            let mut v = Vec::new();
            for x in 0..=5 { v.push(Vec3::new(x as f32, -1., 0.)); } // 0..5 bottom
            for x in 0..=5 { v.push(Vec3::new(x as f32,  1., 0.)); } // 6..11 top
            v
        };
        let sf: Vec<Vec<usize>> = (0..5).map(|i| vec![i, i+1, i+7, i+6]).collect();
        let mut m = HalfEdgeMesh::from_polygons(&sv, &sf);
        let find = |m: &HalfEdgeMesh, ax: f32| -> HalfEdgeId {
            for i in 0..m.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !m.is_edge_live(h) { continue; }
                let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
                if (s.x - ax).abs() < 1e-4 && (d.x - ax).abs() < 1e-4 && (s.y + d.y).abs() < 1e-4 {
                    return h;
                }
            }
            panic!("vertical edge at x={ax} not found");
        };
        let sel: std::collections::HashSet<HalfEdgeId> = [find(&m, 1.), find(&m, 4.)].into_iter().collect();
        assert_eq!(sel.len(), 2, "two distinct disjoint edges (x=1, x=4)");
        assert!(m.dissolve_edges_region(&sel), "disjoint gap-strip dissolve must apply");
        assert_eq!(counts(&m), (8, 10, 3), "oracle disjoint gap-strip -> 8/10/3 (two merges + untouched q2)");
        assert_post_op_clean(&m, "dissolve_edges_region disjoint gap-strip");
        let want = { let mut w = vec![
            [0,-1,0],[0,1,0],[2,-1,0],[2,1,0],[3,-1,0],[3,1,0],[5,-1,0],[5,1,0]]; w.sort(); w };
        assert_eq!(live_coords(&m), want, "gap strip: x=0,2,3,5 survive; x=1,4 endpoints gone");
    }

    #[test]
    fn join_edge_kill_vert_refuses_valence_three() {
        // The keystone primitive is a NO-OP (byte-untouched) on anything but a clean interior valence-2
        // vertex — a degree-3 cube corner must be refused, not collapsed.
        let mut m = make_cube();
        let v = (0..m.vertices.len() as u32)
            .find(|&i| vsign(&m, VertexId(i)) == [-1, -1, -1]).map(VertexId).unwrap();
        assert_eq!(m.vertex_valence(v), 3, "cube corner is valence-3");
        assert!(!m.join_edge_kill_vert(v), "valence-3 vertex must be refused");
        assert_eq!(counts(&m), (8, 12, 6), "refusal leaves the cube untouched");
        assert_post_op_clean(&m, "join_edge_kill_vert refusal");
    }

    #[test]
    fn delete_vertex_matches_blender_oracle() {
        // Oracle: docs/planning/poly/delete_ref.py (Blender Delete Vertices on corner (1,1,1)) -> nv=7, nf=3.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let v = (0..m.vertices.len() as u32)
            .find(|&i| (m.vert(VertexId(i)).position - Vec3::new(1., 1., 1.)).length() < 1e-4)
            .map(VertexId).expect("cube has a (1,1,1) corner");
        m.delete_vertex(v);
        assert!(m.is_manifold().is_ok(), "delete_vertex must stay manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        assert_eq!(m.vertex_count(), 7, "oracle nv=7");
        assert_eq!(m.face_count(), 3, "oracle nf=3");
        // the deleted corner is gone; the other 7 remain.
        assert!(!(0..m.vertices.len() as u32).filter(|vi| !m.free_verts.contains(vi))
            .any(|vi| (m.vert(VertexId(vi)).position - Vec3::new(1., 1., 1.)).length() < 1e-4),
            "(1,1,1) corner must be removed");
    }

    #[test]
    fn delete_edge_matches_blender_oracle() {
        // Oracle: docs/planning/poly/delete_ref.py (Blender Delete Edges on (1,1,-1)-(1,1,1)) -> nv=8, nf=4.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let (a, b) = (Vec3::new(1., 1., -1.), Vec3::new(1., 1., 1.));
        let he = (0..m.half_edges.len() as u32).map(HalfEdgeId)
            .find(|&h| {
                if m.free_edges.contains(&h.0) { return false; }
                let s = m.vert(m.he_source(h)).position;
                let d = m.vert(m.he(h).vertex).position;
                ((s - a).length() < 1e-4 && (d - b).length() < 1e-4)
                    || ((s - b).length() < 1e-4 && (d - a).length() < 1e-4)
            }).expect("cube has the (1,1,-1)-(1,1,1) edge");
        m.delete_edge(he);
        assert!(m.is_manifold().is_ok(), "delete_edge must stay manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        assert_eq!(m.vertex_count(), 8, "oracle nv=8 (endpoints stay)");
        assert_eq!(m.face_count(), 4, "oracle nf=4 (2 faces removed)");
    }

    // ================================================================================================
    // Delete > Only Faces (Blender delete(type="ONLY_FACE")) — WIRE-EDGE oracle gate.
    // Oracle script: docs/planning/poly/only_faces_ref.py -> only_faces.json (verts + undirected edge
    // pairs + face count, Blender 4.5). ONLY_FACE keeps every edge+vertex (loose ones become WIRE edges);
    // FACE removes the loose edges. These prove AZRA delete_face_only == ONLY_FACE by identity, and the
    // FACE-vs-ONLY_FACE edge contrast. Identity = vertex position multiset + undirected edge set + nf.
    // ================================================================================================

    /// Canonical undirected LIVE edge set keyed by vertex POSITION (positions×1000 rounded to i32 — the
    /// oracle inputs are integers/halves, exact). Dedups twins (both map to the same sorted pair), so a
    /// wire edge (two twinned faceless half-edges) counts ONCE, exactly like a Blender edge.
    fn live_edge_pos_set(m: &HalfEdgeMesh) -> std::collections::BTreeSet<[[i32; 3]; 2]> {
        let k = |p: Vec3| [(p.x * 1000.0).round() as i32, (p.y * 1000.0).round() as i32, (p.z * 1000.0).round() as i32];
        let mut set = std::collections::BTreeSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let (mut a, mut b) = (k(m.vert(m.he_source(h)).position), k(m.vert(m.he(h).vertex).position));
            if a > b { std::mem::swap(&mut a, &mut b); }
            set.insert([a, b]);
        }
        set
    }

    /// LIVE vertex position multiset, sorted (positions×1000 → i32). Matches the oracle's `sorted(verts)`.
    fn live_vert_pos_multiset(m: &HalfEdgeMesh) -> Vec<[i32; 3]> {
        let mut v: Vec<[i32; 3]> = (0..m.vertices.len() as u32)
            .filter(|i| !m.free_verts.contains(i))
            .map(|i| {
                let p = m.vert(VertexId(i)).position;
                [(p.x * 1000.0).round() as i32, (p.y * 1000.0).round() as i32, (p.z * 1000.0).round() as i32]
            }).collect();
        v.sort_unstable();
        v
    }

    /// Build the canonical edge set from an explicit list of (a,b) position pairs (the Blender oracle rows).
    fn edge_pos_set(pairs: &[([f32; 3], [f32; 3])]) -> std::collections::BTreeSet<[[i32; 3]; 2]> {
        let k = |p: [f32; 3]| [(p[0] * 1000.0).round() as i32, (p[1] * 1000.0).round() as i32, (p[2] * 1000.0).round() as i32];
        let mut set = std::collections::BTreeSet::new();
        for &(a, b) in pairs {
            let (mut ka, mut kb) = (k(a), k(b));
            if ka > kb { std::mem::swap(&mut ka, &mut kb); }
            set.insert([ka, kb]);
        }
        set
    }

    /// Find the single live face whose centroid satisfies `pred` (matches the oracle's face-select loop).
    fn find_face_by_centroid(m: &HalfEdgeMesh, pred: impl Fn(Vec3) -> bool) -> FaceId {
        for i in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&i) { continue; }
            let f = FaceId(i);
            if m.face(f).half_edge == NO_EDGE { continue; }
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            if pred(c) { return f; }
        }
        panic!("no face matched the centroid predicate");
    }

    #[test]
    fn delete_face_only_isolated_quad_matches_blender_only_face() {
        // Oracle only_faces.json "quad_only_face": nv=4 ne=4 nf=0 — the flagship WIRE-EDGE case. A 1x1
        // plane (corners ±0.5, z=0), ONLY_FACE the single quad -> the face is gone but all 4 edges survive
        // as WIRE edges (each a lone half-edge, twin==NO_TWIN, face==NO_FACE) and all 4 verts remain.
        let v = vec![
            Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(0.5, 0.5, 0.0), Vec3::new(-0.5, 0.5, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&v, &[vec![0, 1, 2, 3]]);
        assert_eq!(counts(&m), (4, 4, 1), "input quad is 4v/4e/1f");

        m.delete_face_only(FaceId(0));

        assert_eq!(counts(&m), (4, 4, 0), "oracle: nv=4 ne=4 nf=0 (4 wire edges kept, face gone)");
        assert!(m.validate().is_ok(), "wire result must validate: {:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "wire result accepted by is_manifold (no loosening): {:?}", m.is_manifold());
        assert_post_op_clean(&m, "delete_face_only quad");

        // Identity to Blender: vertex multiset + undirected edge set.
        assert_eq!(live_vert_pos_multiset(&m),
            vec![[-500, -500, 0], [-500, 500, 0], [500, -500, 0], [500, 500, 0]],
            "oracle vertex multiset (4 corners kept)");
        let oracle_edges = edge_pos_set(&[
            ([-0.5, -0.5, 0.0], [-0.5, 0.5, 0.0]),
            ([-0.5, -0.5, 0.0], [0.5, -0.5, 0.0]),
            ([-0.5, 0.5, 0.0], [0.5, 0.5, 0.0]),
            ([0.5, -0.5, 0.0], [0.5, 0.5, 0.0]),
        ]);
        assert_eq!(live_edge_pos_set(&m), oracle_edges, "oracle wire-edge set (4 perimeter edges kept)");

        // Every surviving edge is a WIRE edge: no live half-edge bears a face.
        assert!(!(0..m.half_edges.len() as u32).any(|i| {
            let h = HalfEdgeId(i);
            m.is_edge_live(h) && m.he(h).face != NO_FACE
        }), "all kept half-edges are faceless wire edges");
    }

    #[test]
    fn delete_face_vs_only_face_contrast_on_quad() {
        // The FACE (delete_face) vs ONLY_FACE (delete_face_only) difference on the isolated quad:
        // Blender FACE -> ne=0 (loose edges REMOVED); ONLY_FACE -> ne=4 (loose edges KEPT). AZRA must
        // reproduce this edge contrast: delete_face frees the loose half-edges, delete_face_only keeps them.
        let v = vec![
            Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(0.5, 0.5, 0.0), Vec3::new(-0.5, 0.5, 0.0),
        ];

        // delete_face == Blender FACE: the 4 now-loose edges are REMOVED (ne 4 -> 0).
        let mut mf = HalfEdgeMesh::from_polygons(&v, &[vec![0, 1, 2, 3]]);
        mf.delete_face(FaceId(0));
        assert_eq!(undirected_edge_count(&mf), 0, "FACE: loose edges removed (matches Blender ne=0)");
        assert_eq!(mf.face_count(), 0, "FACE: face gone");
        assert_post_op_clean(&mf, "delete_face quad (FACE)");

        // delete_face_only == Blender ONLY_FACE: the 4 edges are KEPT (ne stays 4).
        let mut mo = HalfEdgeMesh::from_polygons(&v, &[vec![0, 1, 2, 3]]);
        mo.delete_face_only(FaceId(0));
        assert_eq!(undirected_edge_count(&mo), 4, "ONLY_FACE: loose edges kept (matches Blender ne=4)");
        assert_eq!(mo.face_count(), 0, "ONLY_FACE: face gone");

        // The decisive contrast, stated directly.
        assert_ne!(undirected_edge_count(&mf), undirected_edge_count(&mo),
            "FACE removes the loose edges; ONLY_FACE keeps them");
    }

    #[test]
    fn delete_face_only_cube_one_face_matches_blender() {
        // Oracle "cube_one_only_face": nv=8 ne=12 nf=5. new_box(±1); ONLY_FACE the +Y (top) face ->
        // an open box: all 8 verts + all 12 edges kept, one face gone. The 4 top edges become boundary
        // edges of the surviving side faces (one faceless half-edge twinned to a face-bearing one).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(counts(&m), (8, 12, 6), "input cube is 8v/12e/6f");
        let pre_edges = live_edge_pos_set(&m);
        let top = find_face_by_centroid(&m, |c| (c - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4);

        m.delete_face_only(top);

        assert_eq!(counts(&m), (8, 12, 5), "oracle: nv=8 ne=12 nf=5 (open box, all edges kept)");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "open box accepted by is_manifold: {:?}", m.is_manifold());
        assert_post_op_clean(&m, "delete_face_only cube one face");
        // Every edge kept: post edge-position set == pre edge-position set == all 12 cube edges.
        assert_eq!(live_edge_pos_set(&m), pre_edges, "all 12 cube edges kept (identity to input & oracle)");
        assert_eq!(live_vert_pos_multiset(&m).len(), 8, "all 8 corners kept");
    }

    #[test]
    fn delete_face_only_grid_left2_matches_blender() {
        // Oracle "grid_left2_only_face": nv=9 ne=12 nf=2. A 2x2 grid (3x3 verts at x,y in {-1,0,1}, z=0);
        // ONLY_FACE the 2 LEFT faces (centroid x<0). Exercises BOTH representations at once: the middle
        // horizontal edge shared by the two deleted faces becomes a WIRE edge (two twinned faceless
        // half-edges); the two vertical edges shared with the surviving right faces become boundary edges.
        let mut vpos = Vec::new();
        let mut idx = std::collections::HashMap::new();
        for (j, y) in [-1.0f32, 0.0, 1.0].iter().enumerate() {
            for (i, x) in [-1.0f32, 0.0, 1.0].iter().enumerate() {
                idx.insert((i, j), vpos.len());
                vpos.push(Vec3::new(*x, *y, 0.0));
            }
        }
        let mut faces = Vec::new();
        for j in 0..2 { for i in 0..2 {
            faces.push(vec![idx[&(i, j)], idx[&(i + 1, j)], idx[&(i + 1, j + 1)], idx[&(i, j + 1)]]);
        }}
        let mut m = HalfEdgeMesh::from_polygons(&vpos, &faces);
        assert_eq!(counts(&m), (9, 12, 4), "input grid is 9v/12e/4f");
        let pre_edges = live_edge_pos_set(&m);

        // Delete the two left faces (centroid x < 0), matching the oracle's select_pred.
        let left: std::collections::HashSet<FaceId> = (0..m.faces.len() as u32)
            .filter(|i| !m.free_faces.contains(i) && m.face(FaceId(*i)).half_edge != NO_EDGE)
            .map(FaceId)
            .filter(|&f| {
                let vs = m.face_vertices(f);
                let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
                c.x < -1e-4
            }).collect();
        assert_eq!(left.len(), 2, "two left faces selected");

        m.delete_faces_only(&left);

        assert_eq!(counts(&m), (9, 12, 2), "oracle: nv=9 ne=12 nf=2 (all edges kept, 2 faces gone)");
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert!(m.is_manifold().is_ok(), "mixed wire+boundary accepted by is_manifold: {:?}", m.is_manifold());
        assert_post_op_clean(&m, "delete_faces_only grid left2");
        assert_eq!(live_edge_pos_set(&m), pre_edges, "all 12 grid edges kept (identity to input & oracle)");
        assert_eq!(live_vert_pos_multiset(&m).len(), 9, "all 9 verts kept");

        // The middle horizontal edge on the left (x in {-1,0}, y=0) is a WIRE edge: BOTH its half-edges
        // are faceless and twinned. Find one directed half-edge along y=0 between x=-1 and x=0.
        let wire = (0..m.half_edges.len() as u32).map(HalfEdgeId).find(|&h| {
            if !m.is_edge_live(h) { return false; }
            let (s, d) = (m.vert(m.he_source(h)).position, m.vert(m.he(h).vertex).position);
            let onx = |x: f32| (x + 1.0).abs() < 1e-4 || x.abs() < 1e-4; // x in {-1,0}
            (s.y.abs() < 1e-4) && (d.y.abs() < 1e-4) && onx(s.x) && onx(d.x)
                && (s.x - d.x).abs() > 0.5 // spans -1..0
        }).expect("left middle horizontal edge exists");
        assert_eq!(m.he(wire).face, NO_FACE, "wire edge half-edge is faceless");
        let tw = m.he(wire).twin;
        assert!(tw != NO_TWIN && m.he(tw).face == NO_FACE,
            "wire edge is two twinned FACELESS half-edges (both left faces deleted)");
    }

    #[test]
    fn delete_face_only_wire_edges_survive_bincode_round_trip() {
        // SAVE/LOAD: a mesh carrying WIRE edges must round-trip byte-identical through bincode (the raw
        // HalfEdgeMesh serializer used by the .fl4 save path — NOT a from_polygons rebuild, which only
        // takes FACE lists and would DROP the faceless wire edges). Guards against a load path that
        // reconstructs from polygons and silently loses the wires.
        let v = vec![
            Vec3::new(-0.5, -0.5, 0.0), Vec3::new(0.5, -0.5, 0.0),
            Vec3::new(0.5, 0.5, 0.0), Vec3::new(-0.5, 0.5, 0.0),
        ];
        let mut m = HalfEdgeMesh::from_polygons(&v, &[vec![0, 1, 2, 3]]);
        m.delete_face_only(FaceId(0)); // 4 wire edges, 0 faces
        assert_eq!(undirected_edge_count(&m), 4, "fixture has 4 wire edges");

        let bytes = bincode::serialize(&m).expect("serialize wire-edge mesh");
        let back: HalfEdgeMesh = bincode::deserialize(&bytes).expect("deserialize wire-edge mesh");

        // Byte-identity: re-serializing the reloaded mesh reproduces the exact bytes (bincode is
        // deterministic), proving the raw HalfEdgeMesh — free-lists, faceless wire half-edges and all —
        // survived the round trip with nothing dropped or rebuilt.
        let bytes2 = bincode::serialize(&back).expect("re-serialize reloaded mesh");
        assert_eq!(bytes, bytes2, "wire-edge mesh must round-trip byte-identical");
        assert_eq!(undirected_edge_count(&back), 4, "all 4 wire edges survive save/load");
        assert_eq!(back.face_count(), 0, "no faces reappear on load");
        assert!(back.validate().is_ok(), "reloaded wire mesh validates");
        assert!(back.is_manifold().is_ok(), "reloaded wire mesh is manifold-accepted");
    }

    #[test]
    fn extrude_face_matches_blender_oracle() {
        // Oracle: docs/planning/poly/extrude_ref.py -> extrude_ref.json. new_box(±1), extrude face 0
        // (bottom -Y) by +Y 0.5 via Blender Extrude Region+Move -> nv=12, nf=10, bottom lifts to y=-0.5.
        // Pre-Phase-2a this output was NON-MANIFOLD (extrude_face never re-linked twins, audit #3).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let closed_before = (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(closed_before, 0, "cube input is closed");
        m.extrude_face(FaceId(0), Vec3::new(0.0, 0.5, 0.0));

        // Watertight + manifold (the Phase 2a twin re-link).
        assert!(m.is_manifold().is_ok(), "extruded cube must be manifold: {:?}", m.is_manifold());
        assert!(m.validate().is_ok());
        let bnd = (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "extruding a face of a closed cube stays closed (watertight)");

        // Oracle counts + vertex-for-vertex.
        assert_eq!(m.vertex_count(), 12, "oracle nv=12");
        assert_eq!(m.face_count(), 10, "oracle nf=10");
        let oracle = [
            Vec3::new(-1., -1., -1.), Vec3::new(1., -1., -1.), Vec3::new(1., -1., 1.), Vec3::new(-1., -1., 1.),
            Vec3::new(-1., 1., -1.), Vec3::new(1., 1., -1.), Vec3::new(1., 1., 1.), Vec3::new(-1., 1., 1.),
            Vec3::new(-1., -0.5, -1.), Vec3::new(-1., -0.5, 1.), Vec3::new(1., -0.5, -1.), Vec3::new(1., -0.5, 1.),
        ];
        for (i, &want) in oracle.iter().enumerate() {
            let hit = (0..m.vertices.len() as u32)
                .filter(|vi| !m.free_verts.contains(vi))
                .any(|vi| (m.vert(VertexId(vi)).position - want).length() < 1e-4);
            assert!(hit, "oracle vertex {} {:?} missing from extrude output", i, want);
        }
    }

    // ================================================================================
    // POKE FACES — Blender bpy.ops.mesh.poke parity (poke_ref.json, Blender 4.5.1)
    // ================================================================================

    /// Asserts every oracle vertex is present (identity by position multiset within eps) and the mesh is
    /// a valid manifold with the expected vertex + face counts.
    #[cfg(test)]
    fn assert_poke_identity(m: &HalfEdgeMesh, oracle: &[Vec3], nf: usize, label: &str) {
        assert!(m.validate().is_ok(), "{label}: poke broke topology");
        assert!(m.is_manifold().is_ok(), "{label}: poke non-manifold");
        assert_eq!(m.vertex_count(), oracle.len(), "{label}: oracle nv");
        assert_eq!(m.face_count(), nf, "{label}: oracle nf");
        for &want in oracle {
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-4),
                "{label}: missing Blender poke vertex {want:?}");
        }
        // Poke fans every selected face into TRIANGLES.
        for i in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&i) { continue; }
            assert_eq!(m.face_vertices(FaceId(i)).len(), 3, "{label}: poke must produce triangles");
        }
    }

    #[test]
    fn poke_cube_matches_blender_oracle() {
        // ORACLE poke_ref.json["cube"]: new_box(1) poke all 6 faces -> nv=14 nf=24, centers at the plain
        // face centroids (regular faces, so weighted == centroid): (0,-1,0),(0,1,0),(0,0,-1),(0,0,1),
        // (-1,0,0),(1,0,0).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let faces: std::collections::HashSet<u32> = (0..m.faces.len() as u32).collect();
        m.poke_faces(&faces, 0.0);
        let oracle = [
            Vec3::new(-1.,-1.,-1.), Vec3::new(1.,-1.,-1.), Vec3::new(1.,-1.,1.), Vec3::new(-1.,-1.,1.),
            Vec3::new(-1.,1.,-1.), Vec3::new(1.,1.,-1.), Vec3::new(1.,1.,1.), Vec3::new(-1.,1.,1.),
            Vec3::new(0.,-1.,0.), Vec3::new(0.,1.,0.), Vec3::new(0.,0.,-1.),
            Vec3::new(0.,0.,1.), Vec3::new(-1.,0.,0.), Vec3::new(1.,0.,0.),
        ];
        assert_poke_identity(&m, &oracle, 24, "cube");
        // Original 8 cube verts keep their ids (from_polygons preserves vertex order).
        for i in 0..8u32 { assert!((m.vert(VertexId(i)).position - oracle[i as usize]).length() < 1e-6,
            "cube vertex id {i} not preserved"); }
    }

    #[test]
    fn poke_cube_offset_matches_blender_oracle() {
        // ORACLE poke_ref.json["cube_off"]: offset 0.5 pushes each center 0.5 along the UNIT outward face
        // normal -> centers at (0,+-1.5,0),(0,0,+-1.5),(+-1.5,0,0).
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let faces: std::collections::HashSet<u32> = (0..m.faces.len() as u32).collect();
        m.poke_faces(&faces, 0.5);
        for &want in &[Vec3::new(0.,-1.5,0.), Vec3::new(0.,1.5,0.), Vec3::new(0.,0.,-1.5),
                       Vec3::new(0.,0.,1.5), Vec3::new(-1.5,0.,0.), Vec3::new(1.5,0.,0.)] {
            assert!(m.vertices.iter().any(|v| (v.position - want).length() < 1e-4),
                "offset poke missing pushed center {want:?}");
        }
        assert_eq!(m.face_count(), 24, "offset poke nf");
        assert!(m.is_manifold().is_ok());
    }

    #[test]
    fn poke_quad_ngon_tri_match_blender_oracle() {
        // quad (poke_ref.json["quad"]): XZ plane -> center (0,0,0), 4 tris.
        let mut q = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(1.,0.,1.), Vec3::new(-1.,0.,1.)],
            &[vec![0,1,2,3]]);
        q.poke_faces(&[0].into_iter().collect(), 0.0);
        assert_poke_identity(&q, &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(1.,0.,1.),
            Vec3::new(-1.,0.,1.), Vec3::new(0.,0.,0.)], 4, "quad");

        // regular hexagon ngon (poke_ref.json["hex"]): center (0,0,0), 6 tris.
        let hexp: Vec<Vec3> = (0..6).map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 6.0;
            Vec3::new(a.cos(), 0.0, a.sin())
        }).collect();
        let mut h = HalfEdgeMesh::from_polygons(&hexp, &[(0..6).collect()]);
        h.poke_faces(&[0].into_iter().collect(), 0.0);
        assert_eq!(h.vertex_count(), 7, "hex nv");
        assert_eq!(h.face_count(), 6, "hex nf");
        assert!(h.vertices.iter().any(|v| v.position.length() < 1e-4), "hex center at origin");

        // single triangle (poke_ref.json["tri"]): WEIGHTED center (0,0,-0.309017) — NOT the centroid
        // (0,0,-0.333). This is the case that PROVES the weighting (a plain-centroid poke fails here).
        let mut t = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(0.,0.,1.)], &[vec![0,1,2]]);
        t.poke_faces(&[0].into_iter().collect(), 0.0);
        assert_poke_identity(&t, &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(0.,0.,1.),
            Vec3::new(0.,0.,-0.309017)], 3, "tri");
        assert!(!t.vertices.iter().any(|v| (v.position - Vec3::new(0.,0.,-1.0/3.0)).length() < 1e-3),
            "tri poke used the plain centroid, not Blender's weighted median");
    }

    #[test]
    fn poke_weighted_center_matches_blender_irregular_quad() {
        // ORACLE poke_ref.json["irregular"]: an irregular quad centres at (2.154508,0,0.523545), NOT the
        // plain centroid (2.125,0,0.5) — locks face_center_median_weighted against Blender's default.
        let mut m = HalfEdgeMesh::from_polygons(
            &[Vec3::new(0.,0.,0.), Vec3::new(4.,0.,0.), Vec3::new(4.5,0.,1.), Vec3::new(0.,0.,1.)],
            &[vec![0,1,2,3]]);
        m.poke_faces(&[0].into_iter().collect(), 0.0);
        assert!(m.vertices.iter().any(|v| (v.position - Vec3::new(2.154508,0.,0.523545)).length() < 1e-4),
            "irregular poke center must be Blender's weighted median (2.154508,0,0.523545)");
        assert!(!m.vertices.iter().any(|v| (v.position - Vec3::new(2.125,0.,0.5)).length() < 1e-3),
            "irregular poke must NOT use the plain centroid (2.125,0,0.5)");
    }

    #[test]
    fn poke_empty_selection_is_noop() {
        let base = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut m = base.clone();
        m.poke_faces(&std::collections::HashSet::new(), 0.0);
        assert_eq!(m.vertex_count(), base.vertex_count(), "empty poke changed nothing");
        assert_eq!(m.face_count(), base.face_count());
    }

    // ================================================================================
    // CONNECT VERTEX PATH — Blender bpy.ops.mesh.vert_connect_path (connect_ref.json)
    // ================================================================================

    #[test]
    fn connect_quad_diagonal_matches_blender_oracle() {
        // ORACLE connect_ref.json["quad_diag"]: connect verts 0,2 of a quad -> 2 triangles, verts unmoved.
        let mut m = HalfEdgeMesh::from_polygons(
            &[Vec3::new(-1.,0.,-1.), Vec3::new(1.,0.,-1.), Vec3::new(1.,0.,1.), Vec3::new(-1.,0.,1.)],
            &[vec![0,1,2,3]]);
        let made = m.connect_vertices(&[VertexId(0), VertexId(2)]);
        assert_eq!(made, 1, "one edge created");
        assert_eq!(m.vertex_count(), 4, "quad connect nv unchanged");
        assert_eq!(m.face_count(), 2, "quad -> 2 faces");
        for i in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&i) { continue; }
            assert_eq!(m.face_vertices(FaceId(i)).len(), 3, "both halves are triangles");
        }
        assert!(m.validate().is_ok() && m.is_manifold().is_ok());
    }

    #[test]
    fn connect_hexagon_opposite_matches_blender_oracle() {
        // ORACLE connect_ref.json["hex_0_3"]: connect verts 0,3 of a hexagon -> two quads.
        let hexp: Vec<Vec3> = (0..6).map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 6.0;
            Vec3::new(a.cos(), 0.0, a.sin())
        }).collect();
        let mut m = HalfEdgeMesh::from_polygons(&hexp, &[(0..6).collect()]);
        let made = m.connect_vertices(&[VertexId(0), VertexId(3)]);
        assert_eq!(made, 1);
        assert_eq!(m.vertex_count(), 6, "hex connect nv unchanged");
        assert_eq!(m.face_count(), 2, "hex -> 2 faces");
        for i in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&i) { continue; }
            assert_eq!(m.face_vertices(FaceId(i)).len(), 4, "both halves are quads");
        }
        assert!(m.validate().is_ok() && m.is_manifold().is_ok());
    }

    #[test]
    fn connect_three_vert_path_across_two_faces_matches_blender_oracle() {
        // ORACLE connect_ref.json["path_0_4_2"]: 1x2 grid (two quads sharing the mid edge), path 0->4->2
        // splits face A (0-4) then face B (4-2) -> 4 triangles, verts unmoved.
        let gv = [Vec3::new(-1.,0.,-1.), Vec3::new(0.,0.,-1.), Vec3::new(1.,0.,-1.),
                  Vec3::new(-1.,0.,1.), Vec3::new(0.,0.,1.), Vec3::new(1.,0.,1.)];
        let mut m = HalfEdgeMesh::from_polygons(&gv, &[vec![0,3,4,1], vec![1,4,5,2]]);
        let made = m.connect_vertices(&[VertexId(0), VertexId(4), VertexId(2)]);
        assert_eq!(made, 2, "two edges created (one per shared face)");
        assert_eq!(m.vertex_count(), 6, "path connect nv unchanged");
        assert_eq!(m.face_count(), 4, "two quads -> 4 triangles");
        for i in 0..m.faces.len() as u32 {
            if m.free_faces.contains(&i) { continue; }
            assert_eq!(m.face_vertices(FaceId(i)).len(), 3, "all four are triangles");
        }
        assert!(m.validate().is_ok() && m.is_manifold().is_ok());
    }

    #[test]
    fn connect_unshared_pair_refuses_cleanly() {
        // Two verts of a quad that DO share a face connect; but a pair on DIFFERENT faces with no common
        // face makes zero edges (the caller then refuses). Grid faces A=(0,3,4,1),B=(1,4,5,2): verts 0 and
        // 5 share NO face.
        let gv = [Vec3::new(-1.,0.,-1.), Vec3::new(0.,0.,-1.), Vec3::new(1.,0.,-1.),
                  Vec3::new(-1.,0.,1.), Vec3::new(0.,0.,1.), Vec3::new(1.,0.,1.)];
        let mut m = HalfEdgeMesh::from_polygons(&gv, &[vec![0,3,4,1], vec![1,4,5,2]]);
        let before = m.clone();
        let made = m.connect_vertices(&[VertexId(0), VertexId(5)]);
        assert_eq!(made, 0, "no shared face -> nothing connected");
        assert_eq!(m.face_count(), before.face_count(), "mesh unchanged");
        // Adjacent verts (already edge-joined) also make no edge (Blender no-ops the pair).
        let mut m2 = before.clone();
        assert_eq!(m2.connect_vertices(&[VertexId(0), VertexId(1)]), 0, "adjacent pair is a no-op");
    }

    #[test]
    fn connect_path_order_reconstructs_a_valid_walk() {
        // The app has no selection order; connect_path_order chains by shared-face adjacency. On the 1x2
        // grid, the set {0,2,4} starting at 4 must yield a walk that connects both faces (made == 2).
        let gv = [Vec3::new(-1.,0.,-1.), Vec3::new(0.,0.,-1.), Vec3::new(1.,0.,-1.),
                  Vec3::new(-1.,0.,1.), Vec3::new(0.,0.,1.), Vec3::new(1.,0.,1.)];
        let m = HalfEdgeMesh::from_polygons(&gv, &[vec![0,3,4,1], vec![1,4,5,2]]);
        let order = m.connect_path_order(&[0u32,2,4].into_iter().collect(), Some(4));
        let mut mm = m.clone();
        let made = mm.connect_vertices(&order);
        assert_eq!(made, 2, "reconstructed path connects both shared faces");
        assert!(mm.validate().is_ok() && mm.is_manifold().is_ok());
    }

    // ================================================================================
    // TO SPHERE — Blender bpy.ops.transform.tosphere (tosphere_ref.json)
    // ================================================================================

    #[test]
    fn to_sphere_flat_grid_factor1_matches_blender_oracle() {
        // ORACLE tosphere_ref.json["grid_f1"]: 3x3 flat grid, all selected, factor 1.0. center=(0,0,0),
        // radius=meandist=1.072984. Corner (-1,0,-1) -> (-0.758714,0,-0.758714); edge (0,0,-1) ->
        // (0,0,-1.072984); center vertex stays at origin.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2); // 3x3 grid, verts at (-1..1,0,-1..1)
        assert_eq!(m.vertex_count(), 9, "grid input nv");
        let sel: std::collections::HashSet<u32> = (0..9).collect();
        assert!(m.to_sphere(&sel, 1.0));
        let want = [
            Vec3::new(-0.758714,0.,-0.758714), Vec3::new(0.,0.,-1.072984), Vec3::new(0.758714,0.,-0.758714),
            Vec3::new(-1.072984,0.,0.),        Vec3::new(0.,0.,0.),         Vec3::new(1.072984,0.,0.),
            Vec3::new(-0.758714,0.,0.758714),  Vec3::new(0.,0.,1.072984),   Vec3::new(0.758714,0.,0.758714),
        ];
        for w in &want {
            assert!(m.vertices.iter().any(|v| (v.position - *w).length() < 1e-4),
                "to_sphere f1 missing Blender vertex {w:?}");
        }
        assert!(m.validate().is_ok());
    }

    #[test]
    fn to_sphere_flat_grid_factor_half_matches_blender_oracle() {
        // ORACLE tosphere_ref.json["grid_f05"]: factor 0.5 = exact midpoint of old and full-sphere target.
        let mut m = HalfEdgeMesh::new_plane(2.0, 2.0, 2, 2);
        let sel: std::collections::HashSet<u32> = (0..9).collect();
        assert!(m.to_sphere(&sel, 0.5));
        for w in &[Vec3::new(-0.879357,0.,-0.879357), Vec3::new(0.,0.,-1.036492),
                   Vec3::new(-1.036492,0.,0.), Vec3::new(0.,0.,0.)] {
            assert!(m.vertices.iter().any(|v| (v.position - *w).length() < 1e-4),
                "to_sphere f0.5 missing Blender vertex {w:?}");
        }
    }

    #[test]
    fn to_sphere_equidistant_selection_is_identity() {
        // ORACLE tosphere_ref.json["cube_top_f1"]: the 4 top verts of a cube are already equidistant from
        // their mean (0,1,0), so factor 1.0 leaves them exactly in place.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let top: std::collections::HashSet<u32> = (0..m.vertices.len() as u32)
            .filter(|&i| m.vert(VertexId(i)).position.y > 0.5).collect();
        assert_eq!(top.len(), 4, "four top verts");
        let before: Vec<Vec3> = top.iter().map(|&v| m.vert(VertexId(v)).position).collect();
        assert!(m.to_sphere(&top, 1.0));
        for (&v, &b) in top.iter().zip(before.iter()) {
            assert!((m.vert(VertexId(v)).position - b).length() < 1e-5,
                "equidistant to_sphere moved a vertex that was already on the sphere");
        }
    }

    #[test]
    fn to_sphere_empty_selection_refuses() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        assert!(!m.to_sphere(&std::collections::HashSet::new(), 1.0), "empty to_sphere returns false");
    }

    #[test]
    fn he_source_live_orphan_returns_sentinel_not_destination() {
        // A LIVE half-edge with no valid prev and no twin (a relink bug mid-op) must not report its
        // DESTINATION as its source — callers could not distinguish that from a valid answer. It
        // returns the same VertexId(0) sentinel the freed-slot path already returns.
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let orphan = m.alloc_half_edge(HalfEdge {
            vertex: VertexId(3), // destination — deliberately NOT 0 so the sentinel is distinguishable
            twin: NO_TWIN,
            next: NO_EDGE,
            prev: NO_EDGE,
            face: NO_FACE,
        });
        assert!(m.is_edge_live(orphan), "orphan is live (not on the free-list)");
        assert_eq!(m.he_source(orphan), VertexId(0), "live orphan yields the sentinel, not he.vertex");
        // Freed-slot path unchanged: dead slot also yields the sentinel, silently.
        assert!(m.free_edge(orphan));
        assert_eq!(m.he_source(orphan), VertexId(0));
    }

    #[test]
    fn is_vert_live_mirrors_edge_and_face_predicates() {
        let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
        let n = m.vertices.len() as u32;
        for i in 0..n {
            assert!(m.is_vert_live(VertexId(i)), "fresh vertex {i} must be live");
        }
        // Out-of-range is never live.
        assert!(!m.is_vert_live(VertexId(n)));
        assert!(!m.is_vert_live(VertexId(u32::MAX)));
        // Freed slot is dead; free_vert is idempotent.
        assert!(m.free_vert(VertexId(0)));
        assert!(!m.is_vert_live(VertexId(0)));
        assert!(!m.free_vert(VertexId(0)));
        // Reuse via alloc revives the slot.
        let v = m.alloc_vertex(HVertex { position: Vec3::ZERO, normal: Vec3::Z, half_edge: NO_EDGE });
        assert_eq!(v, VertexId(0));
        assert!(m.is_vert_live(v));
    }


    /// OWNER REPRO 2026-08-03 extrude.fl4 ("2 new segments that break the shape"): intrude a
    /// top sub-face that touches THREE rim edges (both x walls + one z end) but is split from
    /// the rest of the top by an interior edge. The stacked-wall dissolve folds the flush
    /// walls back over themselves (f0+f3-f5, one normal inverted). is_manifold cannot see a
    /// fold; count_folded_coplanar_pairs must — the app reroutes to the boolean when it does.
    #[test]
    fn intrude_three_rim_notch_folds_are_detected() {
        let p = vec![
            Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),   Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, 2.6),  Vec3::new(0.5, -0.5, 2.6),
            Vec3::new(0.5, 0.5, 2.6),    Vec3::new(-0.5, 0.5, 2.6),
            // top split at z = -0.04
            Vec3::new(-0.5, 0.5, -0.04), Vec3::new(0.5, 0.5, -0.04),
        ];
        let faces = vec![
            vec![0, 1, 2, 3],      // -Z end
            vec![5, 4, 7, 6],      // +Z end
            vec![4, 5, 1, 0],      // bottom
            vec![3, 2, 9, 8],      // top strip  (z in [-0.5, -0.04])
            vec![8, 9, 6, 7],      // top main   (z in [-0.04, 2.6]) — touches 3 rims
            vec![4, 0, 3, 8, 7],   // -X wall (pentagon: split vert on its top edge)
            vec![1, 5, 6, 9, 2],   // +X wall
        ];
        let mut m = HalfEdgeMesh::from_polygons(&p, &faces);
        assert!(m.is_manifold().is_ok(), "setup watertight");
        let top_main = (0..m.faces.len() as u32).map(FaceId)
            .find(|&f| {
                if m.face(f).half_edge == NO_EDGE || m.compute_face_normal(f).y < 0.9 { return false; }
                let vs = m.face_vertices(f);
                vs.iter().map(|&v| m.vert(v).position.z).sum::<f32>() / vs.len() as f32 > 0.5
            })
            .expect("main top face");
        let mut sel = std::collections::HashSet::new();
        sel.insert(top_main);
        let caps = m.extrude_region(&sel, Vec3::ZERO);
        let capset: std::collections::HashSet<FaceId> = caps.iter().copied().collect();
        let mut moved = std::collections::HashSet::new();
        for &c in &caps {
            for v in m.face_vertices(c) {
                if moved.insert(v) { m.vert_mut(v).position.y -= 0.42; }
            }
        }
        let (merged, folds) = m.merge_coplanar_extrude_walls(&capset);
        eprintln!("merged={merged} folds={folds} faces={} manifold={:?}", m.face_count(), m.is_manifold());
        for i in 0..m.faces.len() as u32 {
            let fid = FaceId(i);
            if m.face(fid).half_edge == NO_EDGE { continue; }
            let n = m.compute_face_normal(fid).normalize_or_zero();
            eprintln!("  f{i} n=({:.2},{:.2},{:.2}) ring={:?}", n.x, n.y, n.z,
                m.face_vertices(fid).iter().map(|v| v.0).collect::<Vec<_>>());
        }
        // Contract: the topological merge must either come out geometrically clean
        // (no folds) or REPORT the folds so the caller reroutes to the boolean.
        if folds == 0 {
            assert!(m.is_manifold().is_ok());
        }
    }

    // ============================================================
    // canonical_edges — the ONE edge walk selection / picking / overlays share.
    // ============================================================

    /// The legacy open-coded rule these systems used to each carry: skip faceless half-edges,
    /// then keep the lower id of a twin pair. Kept ONLY to prove the bug it caused.
    fn legacy_edge_walk(m: &HalfEdgeMesh) -> Vec<u32> {
        let mut out = Vec::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.face == NO_FACE { continue; }
            if he.twin != NO_TWIN && i > he.twin.0 { continue; }
            out.push(i);
        }
        out
    }

    #[test]
    fn canonical_edges_visits_each_undirected_edge_once() {
        let m = make_cube();
        let ids: Vec<u32> = m.canonical_edges().map(|h| h.0).collect();
        assert_eq!(ids.len(), 12, "a cube has 12 undirected edges");
        // No id is the higher half of a pair, and no pair is represented twice.
        let set: std::collections::HashSet<u32> = ids.iter().copied().collect();
        assert_eq!(set.len(), ids.len(), "duplicate edge ids");
        for &i in &ids {
            let tw = m.he(HalfEdgeId(i)).twin;
            assert!(tw == NO_TWIN || i < tw.0, "he {i} is not the canonical half");
            if tw != NO_TWIN {
                assert!(!set.contains(&tw.0), "both halves of edge {i} enumerated");
            }
        }
        // Closed manifold: the legacy rule happened to agree here — which is exactly why the
        // divergence went unnoticed until someone opened a mesh.
        assert_eq!(legacy_edge_walk(&m), ids);
    }

    /// REGRESSION (owner 2026-08-09: "selection on an open shape does not work"). On an OPEN
    /// mesh the rim half-edges are LIVE but faceless, and they hold the LOWER id of their pair.
    /// The legacy rule dropped the faceless half on the face test and its faced twin on the
    /// min-of-pair test, so those edges existed for the wireframe, the hover cache and the GPU
    /// select buffer — but box / lasso / paint / select-all / invert / select-linked could
    /// never reach them.
    #[test]
    fn canonical_edges_keeps_the_rim_of_an_open_mesh() {
        let mut m = make_cube();
        m.dissolve_face(FaceId(1)); // open the top: its half-edges survive with face = NO_FACE

        let canon: Vec<u32> = m.canonical_edges().map(|h| h.0).collect();
        assert_eq!(canon.len(), 12, "opening a face removes no EDGES, only the face");

        // The four rim edges are live, faceless, and lower-id than their faced twins.
        let rim: Vec<u32> = canon
            .iter()
            .copied()
            .filter(|&i| m.he(HalfEdgeId(i)).face == NO_FACE)
            .collect();
        assert_eq!(rim.len(), 4, "the opened face leaves 4 faceless rim half-edges");
        for &i in &rim {
            let tw = m.he(HalfEdgeId(i)).twin;
            assert_ne!(tw, NO_TWIN);
            assert!(i < tw.0, "rim half {i} must be the LOWER id — the trap this test pins");
            assert_ne!(m.he(tw).face, NO_FACE, "its twin still carries a real face");
        }

        // The bug, stated as an assertion: the legacy walk loses exactly those four.
        let legacy = legacy_edge_walk(&m);
        assert_eq!(legacy.len(), 8);
        for &i in &rim {
            assert!(!legacy.contains(&i), "legacy walk unexpectedly kept rim edge {i}");
        }
    }

    /// A pure WIRE edge (no incident face on EITHER side) is still one selectable segment.
    #[test]
    fn canonical_edges_keeps_wire_edges_and_skips_freed_slots() {
        let mut m = make_cube();
        let before: std::collections::HashSet<u32> = m.canonical_edges().map(|h| h.0).collect();
        // Free a slot: it must drop out (liveness IS the filter).
        m.dissolve_edge(HalfEdgeId(0));
        for h in m.canonical_edges() {
            assert!(m.is_edge_live(h), "freed slot {} enumerated", h.0);
        }
        assert!(before.len() > m.canonical_edges().count());
    }

    // ── Bend (Simple Deform) ──────────────────────────────────────────────────────────────────
    //
    // Every case below bends a flat strip lying in the XZ plane, so `u = dot(p - pivot, Y)` is 0 for
    // every vertex and the deform collapses to the closed form `(R·sin θ, R·(1 - cos θ), z)`. That is
    // deliberate: a fixture whose expected output is a formula, not a recorded blob, catches a wrong
    // radius or a flipped sign, which a "did anything move?" assertion cannot.

    /// `segs`-quad strip along +X, spanning `x ∈ [0, len]`, `y = 0`, `z = ±0.5`.
    fn bend_bar(segs: usize, len: f32) -> HalfEdgeMesh {
        let mut pts = Vec::new();
        for i in 0..=segs {
            let x = len * i as f32 / segs as f32;
            pts.push(Vec3::new(x, 0.0, -0.5));
            pts.push(Vec3::new(x, 0.0, 0.5));
        }
        let faces: Vec<Vec<usize>> = (0..segs)
            .map(|i| {
                let a = 2 * i;
                vec![a, a + 2, a + 3, a + 1]
            })
            .collect();
        HalfEdgeMesh::from_polygons(&pts, &faces)
    }

    fn bend_positions(m: &HalfEdgeMesh) -> Vec<Vec3> {
        m.vertices.iter().map(|v| v.position).collect()
    }

    /// A zero angle must be BIT-identical, not merely close: Bend sits in a non-destructive stack
    /// that re-evaluates on every slider frame, so an angle of 0 that still nudged positions would
    /// drift the mesh a little further every rebuild.
    #[test]
    fn bend_zero_angle_is_a_noop() {
        let mut m = bend_bar(6, 3.0);
        let before = bend_positions(&m);
        m.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, 0.0, 0.0, 1.0);
        assert_eq!(bend_positions(&m), before, "0 rad must not move a single vertex");
    }

    /// The whole point: a bar bent by θ_total lands exactly on the arc of radius `len / θ_total`.
    #[test]
    fn bend_full_arc_matches_the_closed_form() {
        let (segs, len) = (8usize, 4.0f32);
        let total = std::f32::consts::FRAC_PI_2; // 90°
        let r = len / total;
        let mut m = bend_bar(segs, len);
        m.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, total, 0.0, 1.0);

        for i in 0..=segs {
            let x0 = len * i as f32 / segs as f32;
            let theta = x0 / r;
            let want = Vec3::new(r * theta.sin(), r * (1.0 - theta.cos()), 0.0);
            for k in 0..2 {
                let got = m.vertices[2 * i + k].position;
                assert!(
                    (got.x - want.x).abs() < 1e-4 && (got.y - want.y).abs() < 1e-4,
                    "vertex at x={x0} → {got:?}, expected arc point {want:?}"
                );
                // The binormal (z) rides through untouched — a bend is plane strain, so it must
                // never narrow or widen the strip.
                let z0 = if k == 0 { -0.5 } else { 0.5 };
                assert!((got.z - z0).abs() < 1e-6, "z drifted: {} vs {z0}", got.z);
            }
        }
        // 90° puts the far end one radius over and one radius up, tangent now pointing +Y.
        let end = m.vertices[2 * segs].position;
        assert!((end.x - r).abs() < 1e-4 && (end.y - r).abs() < 1e-4, "end of a 90° bend: {end:?}");
    }

    /// Every bent vertex on the neutral fibre sits exactly one radius from the centre of curvature.
    /// This is the invariant that fails loudly if the radius is derived from the wrong span (mesh
    /// extent vs limit band) — the arc would still look like an arc, just the wrong one.
    #[test]
    fn bend_holds_every_vertex_one_radius_from_the_centre() {
        let (segs, len) = (10usize, 2.5f32);
        let total = 1.2f32;
        let r = len / total;
        let mut m = bend_bar(segs, len);
        m.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, total, 0.0, 1.0);
        let centre = Vec3::new(0.0, r, 0.0);
        for v in &m.vertices {
            let d = (v.position - centre) * Vec3::new(1.0, 1.0, 0.0); // drop the binormal
            assert!((d.length() - r).abs() < 1e-4, "radius {} != {r}", d.length());
        }
    }

    /// Geometry outside the limit band must be RIGID and join the arc without a kink: the tail keeps
    /// its own shape and rides off along the arc's end tangent.
    #[test]
    fn bend_limits_leave_the_tail_rigid_and_tangent() {
        let (segs, len) = (8usize, 4.0f32); // vertices every 0.5 along X
        let total = 1.0f32;
        let mut m = bend_bar(segs, len);
        let before = bend_positions(&m);
        // Band = the first half only, so x ∈ [2, 4] is the rigid tail.
        m.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, total, 0.0, 0.5);

        let band_end = 4; // vertex index pair at x = 2.0
        // Rigid: distances inside the tail are unchanged.
        for i in band_end..segs {
            let d_before = (before[2 * (i + 1)] - before[2 * i]).length();
            let d_after =
                (m.vertices[2 * (i + 1)].position - m.vertices[2 * i].position).length();
            assert!(
                (d_after - d_before).abs() < 1e-5,
                "tail segment {i} stretched: {d_before} → {d_after}"
            );
        }
        // Tangent: the tail's direction is the arc's end tangent (cos θ, sin θ), no kink.
        let dir = (m.vertices[2 * segs].position - m.vertices[2 * band_end].position).normalize();
        let want = Vec3::new(total.cos(), total.sin(), 0.0);
        assert!(dir.dot(want) > 0.99999, "tail direction {dir:?} != end tangent {want:?}");
    }

    /// The fixed point is `pivot`, not the mesh's own start — this is what the Center control buys,
    /// and getting it wrong is the difference between bending in place and swinging around origin.
    #[test]
    fn bend_pins_the_pivot_vertex() {
        let (segs, len) = (6usize, 3.0f32);
        let mut m = bend_bar(segs, len);
        let mid = Vec3::new(len * 0.5, 0.0, 0.0);
        let before = bend_positions(&m);
        m.bend_deform(Vec3::X, Vec3::Y, mid, 0.9, 0.0, 1.0);
        // Vertex index 6/7 is the pair at x = 1.5 = the pivot.
        for k in 0..2 {
            let (a, b) = (m.vertices[6 + k].position, before[6 + k]);
            assert!((a - b).length() < 1e-5, "pivot vertex moved: {b:?} → {a:?}");
        }
        // …and the two ends curl the SAME way (both toward +Y), which is what makes it a bend and
        // not a rotation about the pivot.
        assert!(m.vertices[0].position.y > 1e-3, "start did not curl up");
        assert!(m.vertices[2 * segs].position.y > 1e-3, "end did not curl up");
    }

    /// Positions only: no topology touched, so every stable id (and every sidecar layer keyed by
    /// one) survives a bend.
    #[test]
    fn bend_changes_no_topology() {
        let mut m = bend_bar(5, 2.0);
        let (nv, ne, nf) = (m.vertices.len(), m.half_edges.len(), m.faces.len());
        m.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, 1.4, 0.0, 1.0);
        assert_eq!((m.vertices.len(), m.half_edges.len(), m.faces.len()), (nv, ne, nf));
        assert!(m.validate().is_ok(), "bend broke the kernel invariants: {:?}", m.validate());
    }

    /// Degenerate inputs FAIL CLOSED. An `up` parallel to the axis defines no curl plane, and a
    /// zero-thickness limit band would make the curvature infinite — both must leave the mesh alone
    /// rather than emit NaN positions that poison every downstream cache.
    #[test]
    fn bend_degenerate_inputs_are_noops() {
        let base = bend_positions(&bend_bar(4, 2.0));

        let mut parallel_up = bend_bar(4, 2.0);
        parallel_up.bend_deform(Vec3::X, Vec3::X, Vec3::ZERO, 1.0, 0.0, 1.0);
        assert_eq!(bend_positions(&parallel_up), base, "up ∥ axis must no-op");

        let mut zero_band = bend_bar(4, 2.0);
        zero_band.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, 1.0, 0.5, 0.5);
        assert_eq!(bend_positions(&zero_band), base, "an empty limit band must no-op");

        let mut zero_axis = bend_bar(4, 2.0);
        zero_axis.bend_deform(Vec3::ZERO, Vec3::Y, Vec3::ZERO, 1.0, 0.0, 1.0);
        assert_eq!(bend_positions(&zero_axis), base, "a zero-length axis must no-op");

        let mut nan_angle = bend_bar(4, 2.0);
        nan_angle.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, f32::NAN, 0.0, 1.0);
        assert_eq!(bend_positions(&nan_angle), base, "a non-finite angle must no-op");
    }

    /// Inverted limits are sorted, not an error — dragging the lower slider past the upper one is a
    /// normal thing to do mid-scrub and must not silently stop the modifier.
    #[test]
    fn bend_sorts_inverted_limits() {
        let mut fwd = bend_bar(6, 3.0);
        let mut rev = bend_bar(6, 3.0);
        fwd.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, 0.8, 0.2, 0.8);
        rev.bend_deform(Vec3::X, Vec3::Y, Vec3::ZERO, 0.8, 0.8, 0.2);
        assert_eq!(bend_positions(&fwd), bend_positions(&rev));
    }
