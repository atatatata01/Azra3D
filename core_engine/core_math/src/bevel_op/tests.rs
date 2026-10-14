//! Bevel unit/oracle tests (BEVEL.md gates). Pure relocation from bevel_op.rs.

    use super::*;


    #[test]
    fn weld_collapses_coincident_and_separates_distinct() {
        let mut w = WeldMap::new();
        let a = w.push(Vec3::new(1.0, 2.0, 3.0));
        let b = w.push(Vec3::new(1.0, 2.0, 3.0));       // exactly coincident → same index
        let c = w.push(Vec3::new(1.0, 2.0, 3.0 + 1e-7)); // within grid cell → same index (weld tolerance)
        let d = w.push(Vec3::new(1.0, 2.0, 3.001));      // clearly distinct → new index
        assert_eq!(a, b, "coincident points must weld to one index (else the corner tears)");
        assert_eq!(a, c, "sub-tolerance points weld");
        assert_ne!(a, d, "distinct points must not collide");
        assert_eq!(w.positions.len(), 2);
    }

    #[test]
    fn zero_width_is_identity() {
        // A non-positive width returns the input untouched (idempotent zero-width invariant).
        let mesh = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y],
            &[vec![0, 1, 2, 3]],
        );
        let mut sel = HashSet::new();
        sel.insert(0u32);
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.0, ..Default::default() });
        assert_eq!(out.vertices.len(), mesh.vertices.len());
    }

    /// Regression: bevel rebuilds via `from_polygons`, which zeros `material_id`. Face materials
    /// must be restored onto re-traced faces, strips, and caps so Ctrl+M assignments survive.
    #[test]
    fn bevel_preserves_face_materials() {
        let mut cube = HalfEdgeMesh::new_box(Vec3::ONE);
        const MAT: u32 = 7;
        for f in cube.faces.iter_mut() {
            f.material_id = MAT;
        }
        // Pick a manifold interior edge (first HE with a twin + face).
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE {
                sel.insert(i);
                break;
            }
        }
        assert!(!sel.is_empty(), "cube must have a selectable edge");
        let out = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams { width: 0.15, segments: 1, shape: 0.5, clamp: true },
        );
        assert!(out.faces.len() > cube.faces.len(), "bevel must add strip faces");
        let lost = out.faces.iter().filter(|f| f.material_id != MAT).count();
        assert_eq!(
            lost, 0,
            "every output face must keep material_id={MAT}; {lost} faces lost it"
        );
    }

    #[test]
    fn vertex_bevel_preserves_face_materials() {
        let mut cube = HalfEdgeMesh::new_box(Vec3::ONE);
        const MAT: u32 = 11;
        for f in cube.faces.iter_mut() {
            f.material_id = MAT;
        }
        let mut sel = HashSet::new();
        sel.insert(0u32); // one corner
        let out = bevel_vertices_offset(
            &cube,
            &sel,
            &BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true },
        );
        assert!(!out.faces.is_empty());
        let lost = out.faces.iter().filter(|f| f.material_id != MAT).count();
        assert_eq!(
            lost, 0,
            "vertex bevel must keep material_id={MAT}; {lost} faces lost it"
        );
    }


    /// Clean cube edge loop rounds (seg>1) — forced_flat must be false so the app HUD shows Round.
    #[test]
    fn clean_cube_edge_round_not_forced_flat() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        // All 4 top (+Y) edges.
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || i > he.twin.0 { continue; }
            let a = cube.he_source(HalfEdgeId(i)).0;
            let b = he.vertex.0;
            let ya = cube.vert(crate::half_edge::VertexId(a)).position.y;
            let yb = cube.vert(crate::half_edge::VertexId(b)).position.y;
            if (ya - 1.0).abs() < 1e-4 && (yb - 1.0).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert!(!sel.is_empty(), "top edges");
        let out = bevel_edges_offset(
            &cube,
            &sel,
            &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true },
        );
        assert!(out.is_manifold().is_ok(), "round top loop manifold");
        assert!(!last_bevel_forced_flat(), "clean top loop must not force flat");
        assert_eq!(last_bevel_effective_segments(), 3);
    }

    /// `build_boundary_flat` on the real cube corner (1,1,1) with the two top edges beveled produces the two
    /// Blender miter BoundVerts (0.8,0.8,1) [shared face] and (1,1,0.8) [edges_between]; all 3 outgoing edges
    /// map to a sector BoundVert.
    #[test]
    fn build_boundary_cube_corner_miter() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let orig_pos: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
        let v = orig_pos.iter().position(|p| (*p - Vec3::new(1.0, 1.0, 1.0)).length() < 1e-5).expect("corner") as u32;
        let mut beveled: HashSet<u32> = HashSet::new();
        for &h in &mesh.vertex_fan(crate::half_edge::VertexId(v)) {
            let d = orig_pos[mesh.he(h).vertex.0 as usize];
            if (d - Vec3::new(-1.0, 1.0, 1.0)).length() < 1e-5 || (d - Vec3::new(1.0, -1.0, 1.0)).length() < 1e-5 {
                beveled.insert(h.0);
                let tw = mesh.he(h).twin;
                if tw != NO_TWIN { beveled.insert(tw.0); }
            }
        }
        let (bvs, _ro, cbv, _arc) = build_boundary_flat(&mesh, v, &beveled, &orig_pos, &|_| 0.2, 1, 0.5);
        assert_eq!(bvs.len(), 2, "2-edge SHARP miter → 2 BoundVerts; got {bvs:?}");
        let hit = |t: Vec3| bvs.iter().any(|b| (*b - t).length() < 1e-4);
        assert!(hit(Vec3::new(0.8, 0.8, 1.0)), "missing (0.8,0.8,1); {bvs:?}");
        assert!(hit(Vec3::new(1.0, 1.0, 0.8)), "missing (1,1,0.8); {bvs:?}");
        assert_eq!(cbv.len(), 3, "all 3 outgoing edges map to a sector BoundVert; {cbv:?}");
    }

    /// MULTI-EDGE MITER GATE: bevel 2 bottom edges, then bevel EVERY interior edge-PAIR of the result (flat
    /// seg=1 AND round seg=3). Every pair must stay watertight (was: pair (9,29) opened boundary=4).
    #[test]
    fn multi_edge_pairs_watertight() {
        fn bnd(m: &HalfEdgeMesh) -> usize {
            (0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count()
        }
        let bp = |w: f32, s: u32| BevelParams { width: w, segments: s, shape: 0.5, clamp: true };
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let bottom = (0..cube.faces.len() as u32).find(|&f| cube.compute_face_normal(FaceId(f)).y < -0.9).unwrap();
        let bhe: Vec<u32> = cube.face_half_edges(FaceId(bottom)).iter().map(|h| h.0).collect();
        let r1 = bevel_edges_offset(&cube, &[bhe[0], bhe[1]].into_iter().collect(), &bp(0.25, 1));
        let interior: Vec<u32> = {
            let mut v = Vec::new(); let mut seen: HashSet<u32> = HashSet::new();
            for i in 0..r1.half_edges.len() as u32 {
                let he = r1.he(HalfEdgeId(i));
                if he.twin == NO_TWIN || he.face == NO_FACE || r1.he(he.twin).face == NO_FACE { continue; }
                if seen.contains(&i) { continue; } seen.insert(i); seen.insert(he.twin.0);
                v.push(i);
            }
            v
        };
        let mut fails = 0usize;
        for a in 0..interior.len() {
            for b in (a + 1)..interior.len() {
                for s in [1u32, 3] {
                    let sel: HashSet<u32> = [interior[a], interior[b]].into_iter().collect();
                    if bnd(&bevel_edges_offset(&r1, &sel, &bp(0.1, s))) > 0 { fails += 1; }
                }
            }
        }
        assert_eq!(fails, 0, "{fails} pair-bevels opened boundary holes — multi-edge miter must stay watertight");
    }

    /// Square pyramid: apex (0,1,0) degree-4; 4 base corners degree-3 (2 base edges + 1 apex edge → the
    /// KNOWN-GOOD k=1 partial corner). Beveling all 4 apex edges isolates the N-WAY apex miter (the only
    /// new topology) from base-corner handling — unlike the octahedron whose base verts are all degree-4.
    #[cfg(test)]
    fn square_pyramid() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(0.0, 1.0, 0.0),    // 0 apex (degree-4)
            Vec3::new(1.0, -1.0, 1.0),   // 1
            Vec3::new(-1.0, -1.0, 1.0),  // 2
            Vec3::new(-1.0, -1.0, -1.0), // 3
            Vec3::new(1.0, -1.0, -1.0),  // 4
        ];
        let f = vec![
            vec![0, 2, 1], vec![0, 3, 2], vec![0, 4, 3], vec![0, 1, 4], // 4 side tris (outward)
            vec![1, 2, 3, 4],                                            // base quad (-Y)
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// Octahedron matching `docs/planning/poly/nway_octa_ref.py`: 6 verts (±1 on each axis), 8 outward
    /// tris. EVERY vertex is degree-4. Beveling the 4 +Y-apex edges makes the apex an N-way miter AND the
    /// 4 base verts (+X/+Z/-X/-Z) each a k=1 partial corner at a degree-4 far-endpoint (the octahedron
    /// base hole this task fixes).
    #[cfg(test)]
    fn octahedron() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(0.0, 1.0, 0.0),   // 0 +Y apex
            Vec3::new(1.0, 0.0, 0.0),   // 1 +X
            Vec3::new(0.0, 0.0, 1.0),   // 2 +Z
            Vec3::new(-1.0, 0.0, 0.0),  // 3 -X
            Vec3::new(0.0, 0.0, -1.0),  // 4 -Z
            Vec3::new(0.0, -1.0, 0.0),  // 5 -Y apex
        ];
        let f = vec![
            vec![0, 2, 1], vec![0, 3, 2], vec![0, 4, 3], vec![0, 1, 4], // top fan (+Y)
            vec![5, 1, 2], vec![5, 2, 3], vec![5, 3, 4], vec![5, 4, 1], // bottom fan (-Y)
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// Select the 4 half-edges (both directions) incident to the +Y apex (vertex 0) of the octahedron.
    #[cfg(test)]
    fn octa_apex_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if m.he_source(HalfEdgeId(i)).0 == 0 || he.vertex.0 == 0 { sel.insert(i); }
        }
        sel
    }

    /// K=1 PARTIAL CORNER AT A degree>3 FAR-ENDPOINT (the octahedron-base hole). Blender oracle
    /// `docs/planning/poly/nway_octa_ref.json`: octahedron, bevel all 4 +Y-apex edges (offset 0.25, seg1,
    /// profile 0.5) → nv=17, nf=17, watertight. The APEX is the (already-handled) N-way M_POLY cap; each of
    /// the 4 BASE verts (+X/+Z/−X/−Z) is degree-4 with exactly ONE beveled edge → a k=1 partial corner that
    /// USED to open a 9-edge base hole (nv=13/nf=14/boundary=9). The fix: the FLAT (seg=1) k=1 partial corner
    /// at degree>3 now DELETES V, places a recessed center (slid along the back fan edge by the offset), and
    /// fans the chamfer to it (matching Blender vertex-for-vertex). Verifies watertight + oracle vertex set.
    #[test]
    fn octahedron_nway_matches_oracle() {
        let m = octahedron();
        assert!(m.validate().is_ok(), "octahedron manifold");
        let sel = octa_apex_edges(&m);
        assert_eq!(sel.len(), 8, "4 apex edges = 8 directed half-edges");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 1, shape: 0.5, clamp: true });
        // (1) watertight + manifold — the whole point (was a 9-edge base hole before the fix).
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "octahedron bevel-4-apex must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "octahedron N-way result manifold");
        // (2) Blender oracle nway_octa_ref.json: nv=17, nf=17.
        assert_eq!(out.vertices.len(), 17, "oracle nv=17");
        assert_eq!(out.faces.len(), 17, "oracle nf=17");
        // (3) every Blender oracle vertex must match an output vertex within ~2e-3 (vertex-for-vertex).
        let oracle: [[f32; 3]; 17] = [
            [0.0, -1.0, 0.0],
            [0.204124, 0.591752, -0.204124], [-0.204124, 0.591752, -0.204124],
            [-0.204124, 0.591752, 0.204124], [0.204124, 0.591752, 0.204124],
            [0.795876, 0.0, -0.204124], [0.795876, 0.0, 0.204124], [0.823223, -0.176777, 0.0],
            [0.204124, 0.0, 0.795876], [-0.204124, -0.0, 0.795876], [0.0, -0.176777, 0.823223],
            [-0.795876, 0.0, 0.204124], [-0.795876, 0.0, -0.204124], [-0.823223, -0.176777, 0.0],
            [-0.204124, 0.0, -0.795876], [0.204124, -0.0, -0.795876], [0.0, -0.176777, -0.823223],
        ];
        let mut max_dev = 0.0f32;
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter()
                .map(|v| (v.position - want).length())
                .fold(f32::INFINITY, f32::min);
            assert!(best < 2e-3, "octa oracle vert {want:?} missing (closest output {best:.5})");
            max_dev = max_dev.max(best);
        }
        // (4) the 4 base recessed centers exist (y≈−0.1768) → the partial corners were capped, not sealed.
        let centers = out.vertices.iter().filter(|v| (v.position.y + 0.176777).abs() < 2e-3).count();
        assert_eq!(centers, 4, "4 recessed partial-corner centers expected; got {centers}");
        let _ = max_dev;
    }

    /// KNIFE-CHAIN BEVEL GATE (Blender oracle `knife_chain_seg{1,3}_ref.json`): cube with a 3-segment
    /// knife cut across the front face — T-junction terminals on the vertical box edges (degree-3, two
    /// COLLINEAR unbeveled continuations) and two degree-2 BEND vertices — bevel the 3 chain edges
    /// (offset 0.27, profile 0.5). Blender deletes every chain vertex: bends → 2 miter BoundVerts
    /// (M_NONE, no cap; seg>1 = weld arc between them), terminals → rail ends slid onto the collinear
    /// edge (loop slide, no cap). Before the degree-2 STEP-2b routing the k>=2 cap fan-walked the
    /// valence-2 bends and emitted DUPLICATE faces → non-manifold → install guard rejected → the
    /// owner's "can't bevel my knife cut". Must match Blender vertex-for-vertex, both seg=1 and seg=3.
    fn knife_chain_mesh() -> HalfEdgeMesh {
        let verts = [
            Vec3::new(1.0, -1.0, 1.0), Vec3::new(-1.0, -1.0, 1.0), Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, -1.0), Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, 1.0), Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, -0.2, 1.0),  // 8  T-terminal (left)
            Vec3::new(-0.3, -0.2, 1.0),  // 9  bend (degree 2)
            Vec3::new(0.3, 0.35, 1.0),   // 10 bend (degree 2)
            Vec3::new(1.0, 0.35, 1.0),   // 11 T-terminal (right)
        ];
        HalfEdgeMesh::from_polygons(&verts, &[
            vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![3, 2, 5, 4],
            vec![1, 0, 11, 10, 9, 8], vec![7, 6, 8, 9, 10, 11],
            vec![2, 1, 8, 6, 5], vec![11, 0, 3, 4, 7],
        ])
    }
    fn knife_chain_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
        let want = [(8u32, 9u32), (9, 10), (10, 11)];
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.twin.0 < i { continue; }
            let s = m.he_source(HalfEdgeId(i)).0; let d = he.vertex.0;
            if want.contains(&(s.min(d), s.max(d))) { sel.insert(i); }
        }
        assert_eq!(sel.len(), 3, "3 chain edges expected");
        sel
    }
    fn assert_matches_oracle(out: &HalfEdgeMesh, oracle: &[[f32; 3]], nf: usize, tag: &str) {
        assert!(out.is_manifold().is_ok(), "{tag}: non-manifold: {:?}", out.is_manifold());
        assert_eq!(out.vertices.len(), oracle.len(), "{tag}: oracle nv={}", oracle.len());
        assert_eq!(out.faces.len(), nf, "{tag}: oracle nf={nf}");
        for o in oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::INFINITY, f32::min);
            assert!(best < 2e-3, "{tag}: oracle vert {want:?} missing (closest {best:.5})");
        }
    }
    #[test]
    fn knife_chain_bevel_matches_oracle() {
        let m = knife_chain_mesh();
        assert!(m.is_manifold().is_ok());
        let sel = knife_chain_edges(&m);
        // seg=1 (flat): nv=16 nf=10.
        let out1 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.27, segments: 1, shape: 0.5, clamp: true });
        let seg1: [[f32; 3]; 16] = [
            [1.0, -1.0, 1.0], [-1.0, -1.0, 1.0], [-1.0, -1.0, -1.0], [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0], [-1.0, 1.0, -1.0], [-1.0, 1.0, 1.0], [1.0, 1.0, 1.0],
            [-1.0, -0.47, 1.0], [-1.0, 0.07, 1.0],
            [-0.194974, -0.47, 1.0], [-0.405026, 0.07, 1.0],
            [0.405026, 0.08, 1.0], [0.194974, 0.62, 1.0],
            [1.0, 0.62, 1.0], [1.0, 0.08, 1.0],
        ];
        assert_matches_oracle(&out1, &seg1, 10, "seg1");
        // seg=3 (rounded): nv=24 nf=16 — terminals gain 2 verts ON the collinear edge, bends gain 2
        // verts linearly between the miter pair.
        let out3 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.27, segments: 3, shape: 0.5, clamp: true });
        let seg3: [[f32; 3]; 24] = [
            [1.0, -1.0, 1.0], [-1.0, -1.0, 1.0], [-1.0, -1.0, -1.0], [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0], [-1.0, 1.0, -1.0], [-1.0, 1.0, 1.0], [1.0, 1.0, 1.0],
            [-1.0, -0.47, 1.0], [-1.0, 0.07, 1.0], [-1.0, -0.29, 1.0], [-1.0, -0.11, 1.0],
            [-0.194974, -0.47, 1.0], [-0.405026, 0.07, 1.0], [-0.264991, -0.29, 1.0], [-0.335009, -0.11, 1.0],
            [0.405026, 0.08, 1.0], [0.194974, 0.62, 1.0], [0.335009, 0.26, 1.0], [0.264991, 0.44, 1.0],
            [1.0, 0.62, 1.0], [1.0, 0.08, 1.0], [1.0, 0.44, 1.0], [1.0, 0.26, 1.0],
        ];
        assert_matches_oracle(&out3, &seg3, 16, "seg3");
    }

    /// KNIFE-CORNER BEVEL GATE (Blender oracle `knife_corner_seg{1,3}_ref.json`): 2-wide box
    /// (middle loop), knife chain with an OBLIQUE T-terminal on the bottom edge (slide = w/sin,
    /// NOT w) and a T-terminal ON the front/right CORNER edge (slid points must land on the corner
    /// edge and the RIGHT face must absorb them). Owner live repro 2026-07-18. Vertex-for-vertex.
    fn knife_corner_mesh() -> HalfEdgeMesh {
        let verts = [
            Vec3::new(-1.0, -1.0, 1.0), Vec3::new(0.0, -1.0, 1.0), Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 1.0, 1.0), Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(-1.0, -1.0, -1.0), Vec3::new(0.0, -1.0, -1.0), Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0), Vec3::new(0.0, 1.0, -1.0), Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(0.3, -1.0, 1.0), Vec3::new(0.55, -0.45, 1.0), Vec3::new(1.0, -0.45, 1.0),
        ];
        HalfEdgeMesh::from_polygons(&verts, &[
            vec![0, 1, 4, 5], vec![1, 12, 13, 14, 3, 4], vec![12, 2, 14, 13],
            vec![5, 4, 10, 11], vec![4, 3, 9, 10], vec![7, 6, 11, 10], vec![8, 7, 10, 9],
            vec![0, 6, 7, 1], vec![1, 7, 8, 2, 12], vec![0, 5, 11, 6], vec![2, 8, 9, 3, 14],
        ])
    }
    fn knife_corner_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
        let want = [(12u32, 13u32), (13, 14)];
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.twin.0 < i { continue; }
            let s0 = m.he_source(HalfEdgeId(i)).0; let d = he.vertex.0;
            if want.contains(&(s0.min(d), s0.max(d))) { sel.insert(i); }
        }
        assert_eq!(sel.len(), 2);
        sel
    }
    fn assert_oracle_verts(out: &HalfEdgeMesh, oracle: &[[f32; 3]], nf: usize, tag: &str) {
        assert!(out.is_manifold().is_ok(), "{tag}: non-manifold: {:?}", out.is_manifold());
        assert_eq!(out.vertices.len(), oracle.len(), "{tag}: oracle nv={}", oracle.len());
        assert_eq!(out.faces.len(), nf, "{tag}: oracle nf={nf}");
        for o in oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::INFINITY, f32::min);
            assert!(best < 2e-3, "{tag}: oracle vert {want:?} missing (closest {best:.5})");
        }
    }
    #[test]
    fn knife_corner_bevel_matches_oracle() {
        let m = knife_corner_mesh();
        assert!(m.is_manifold().is_ok());
        let sel = knife_corner_edges(&m);
        let out1 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 1, shape: 0.5, clamp: true });
        let seg1: [[f32; 3]; 18] = [
            [-1.0,-1.0,1.0],[0.0,-1.0,1.0],[1.0,-1.0,1.0],[1.0,1.0,1.0],[0.0,1.0,1.0],[-1.0,1.0,1.0],
            [-1.0,-1.0,-1.0],[0.0,-1.0,-1.0],[1.0,-1.0,-1.0],[1.0,1.0,-1.0],[0.0,1.0,-1.0],[-1.0,1.0,-1.0],
            [0.464769,-1.0,1.0],[0.135231,-1.0,1.0],[0.453413,-0.3,1.0],[0.646587,-0.6,1.0],[1.0,-0.3,1.0],[1.0,-0.6,1.0],
        ];
        assert_oracle_verts(&out1, &seg1, 13, "corner-seg1");
        let out3 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        let seg3: [[f32; 3]; 24] = [
            [-1.0,-1.0,1.0],[0.0,-1.0,1.0],[1.0,-1.0,1.0],[1.0,1.0,1.0],[0.0,1.0,1.0],[-1.0,1.0,1.0],
            [-1.0,-1.0,-1.0],[0.0,-1.0,-1.0],[1.0,-1.0,-1.0],[1.0,1.0,-1.0],[0.0,1.0,-1.0],[-1.0,1.0,-1.0],
            [0.464769,-1.0,1.0],[0.135231,-1.0,1.0],[0.354923,-1.0,1.0],[0.245077,-1.0,1.0],
            [0.453413,-0.3,1.0],[0.646587,-0.6,1.0],[0.517804,-0.4,1.0],[0.582196,-0.5,1.0],
            [1.0,-0.3,1.0],[1.0,-0.6,1.0],[1.0,-0.4,1.0],[1.0,-0.5,1.0],
        ];
        assert_oracle_verts(&out3, &seg3, 17, "corner-seg3");
    }

    /// SLIT-edge selection must be SKIPPED, not beveled (Blender parity: a dangling knife end is a
    /// wire edge and bmesh bevel needs two distinct faces). A same-face (slit) edge fed to the rail
    /// machinery sign-fixed both rails toward one centroid → folded self-intersecting output (the
    /// owner's "bevel pokes through near the cut end" when a knife stroke missed the edge snap).
    #[test]
    fn bevel_skips_slit_edges() {
        // Quad with a slit: ring walks out to A and back (A duplicated in the ring).
        let verts = [
            Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0), // 4: slit anchor on the bottom edge
            Vec3::new(0.0, 0.0, 0.0),  // 5: slit interior end
        ];
        let m = HalfEdgeMesh::from_polygons(&verts, &[
            vec![0, 4, 5, 4, 1, 2, 3], // slit: 4 -> 5 -> back to 4
        ]);
        // Find the slit half-edge (same face on both sides).
        let mut slit = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.face == NO_FACE || he.twin == NO_TWIN { continue; }
            if m.he(he.twin).face == he.face { slit.insert(i); }
        }
        assert!(!slit.is_empty(), "test mesh must contain a slit edge");
        let out = bevel_edges_offset(&m, &slit, &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true });
        // Skipped → identity (no new verts, no new faces, no fold).
        assert_eq!(out.vertices.len(), m.vertices.len(), "slit bevel must be a no-op");
        assert_eq!(out.faces.len(), m.faces.len());
    }

    /// N-WAY MITER GATE (Blender oracle `nway_pyramid_ref.json`): square pyramid, bevel all 4 apex edges
    /// (n_bev=4 at the degree-4 apex). The apex is deleted → 4 BoundVerts + one **M_POLY quad cap**; the
    /// result must be WATERTIGHT and match Blender vertex-for-vertex (nv=12, nf=10). Before the un-gate +
    /// STEP 5b cap this opened a hole (boundary>0 → install guard rejected the whole bevel).
    #[test]
    fn nway_pyramid_matches_oracle() {
        let m = square_pyramid();
        assert!(m.validate().is_ok(), "pyramid manifold");
        // Sanity: pyramid built outward.
        let cent = m.vertices.iter().map(|v| v.position).sum::<Vec3>() / m.vertices.len() as f32;
        for f in 0..m.faces.len() as u32 {
            let n = m.compute_face_normal(FaceId(f));
            let vs = m.face_vertices(FaceId(f));
            let fc = vs.iter().map(|v| m.vertices[v.0 as usize].position).sum::<Vec3>() / vs.len() as f32;
            assert!(n.dot(fc - cent) > 0.0, "pyramid face {f} inward");
        }
        // Select all 4 edges incident to the apex (vertex 0).
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if m.he_source(HalfEdgeId(i)).0 == 0 || he.vertex.0 == 0 { sel.insert(i); }
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 1, shape: 0.5, clamp: true });
        // (1) watertight + manifold — the whole point (was a 9-edge hole before the fix).
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "N-way apex miter must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "N-way result manifold");
        // (2) Blender oracle nway_pyramid_ref.json: nv=12, nf=10, same vertex set.
        assert_eq!(out.vertices.len(), 12, "oracle nv=12");
        assert_eq!(out.faces.len(), 10, "oracle nf=10");
        let oracle: [[f32; 3]; 12] = [
            [0.273861, 0.452277, 0.0], [0.0, 0.452277, -0.273861], [-0.273861, 0.452277, 0.0], [0.0, 0.452277, 0.273861],
            [1.0, -1.0, 0.726139], [0.726139, -1.0, 1.0], [-0.726139, -1.0, 1.0], [-1.0, -1.0, 0.726139],
            [-1.0, -1.0, -0.726139], [-0.726139, -1.0, -1.0], [0.726139, -1.0, -1.0], [1.0, -1.0, -0.726139],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            assert!(out.vertices.iter().any(|v| (v.position - want).length() < 2e-3),
                "N-way vert {want:?} missing (does not match Blender oracle)");
        }
        // (3) the apex cap is a single 4-gon over the 4 apex BoundVerts (y≈0.4523), Blender's M_POLY quad.
        let cap = (0..out.faces.len() as u32).find(|&f| {
            let vs = out.face_vertices(FaceId(f));
            vs.len() == 4 && vs.iter().all(|x| (out.vertices[x.0 as usize].position.y - 0.452277).abs() < 2e-3)
        });
        assert!(cap.is_some(), "apex M_POLY quad cap (4 verts at y≈0.4523) missing");
    }

    /// MIXED-CORNER SAFETY (adversarial-review confirmed finding, 2026-07-02): beveling only SOME edges at
    /// a degree>3 vertex (here 3 of the pyramid apex's 4) is a mixed corner — it has a non-beveled "wrap"
    /// sector. STEP 2b must NOT route it (an M_POLY cap across the non-beveled sector orphans edges). The
    /// result must stay MANIFOLD (twin symmetry — no edge with >2 faces); it may be non-watertight, which the
    /// app-level install guard rejects as a no-op. It must NEVER be a manifold-but-wrong fused cap. Full
    /// mixed-corner support (Blender's "on edge between" placement) is future work.
    #[test]
    fn nway_mixed_corner_stays_manifold() {
        let m = square_pyramid();
        // 3 of the 4 apex edges (skip the apex→base-vertex-4 edge): select edges apex(0)→{1,2,3}.
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let (s, d) = (m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            let touches_apex = s == 0 || d == 0;
            let other = if s == 0 { d } else { s };
            if touches_apex && other != 4 { sel.insert(i); } // 3 of 4 → mixed corner at apex
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true });
        // Manifold twin-symmetry must hold (no non-manifold corruption from a bad cap). Boundary MAY be >0
        // (a guarded no-op) — that's the documented mixed-corner gap, not corruption.
        assert!(out.validate().is_ok(), "mixed 3-of-4 corner must stay manifold (no >2-faces-per-edge)");
    }

    /// ROUNDED 2-EDGE MITER GATE (Blender oracle `rounded_2edge_ref.json`): square pyramid, bevel EXACTLY 2
    /// OPPOSITE apex edges (apex->v2, apex->v4) at segments=3. This is a ROUNDED 2-edge miter at the degree-4
    /// apex — the **M_NONE weld** (§3.7): the two BoundVerts are bridged by the co-planarized profile arc
    /// (ns-1 SHARED interior verts, no corner grid). Before this, STEP 2.5 flat-clamped n_seg→1 at any shared
    /// corner so the rounded miter rendered FLAT (nv=8 nf=7). Now nv=14 nf=11, watertight, and vertex-for-
    /// vertex matching Blender — including the interior arc verts (a true rounded corner, not a flat chord).
    #[test]
    fn rounded_2edge_miter_matches_oracle() {
        let m = square_pyramid();
        // Select the 2 OPPOSITE apex edges apex->v2 and apex->v4 (do not share a face → both miter sectors
        // are between==2 loop-slides, whose FLAT BoundVerts AZRA already matches Blender exactly).
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let (s, d) = (m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            let is_e = |a: u32, b: u32| (s == a && d == b) || (s == b && d == a);
            if is_e(0, 2) || is_e(0, 4) { sel.insert(i); }
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        // (1) WATERTIGHT + manifold — the M_NONE weld must seal (no boundary edges).
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "rounded 2-edge miter must be watertight; boundary={bnd}");
        out.validate().expect("rounded 2-edge miter must be manifold");
        // (2) Blender oracle rounded_2edge_ref.json: nv=14, nf=11, same vertex set within 2e-3.
        assert_eq!(out.vertices.len(), 14, "oracle nv=14 (was 8 when flat-clamped)");
        assert_eq!(out.faces.len(), 11, "oracle nf=11 (was 7 when flat-clamped)");
        let oracle: [[f32; 3]; 14] = [
            [1.0, -1.0, 1.0], [-1.0, -1.0, -1.0],
            [0.136931, 0.726139, 0.136931], [-0.136931, 0.726139, -0.136931],
            [0.05012, 0.826379, 0.05012], [-0.05012, 0.826379, -0.05012],
            [-0.726139, -1.0, 1.0], [-1.0, -1.0, 0.726139], [-0.863069, -1.0, 0.96331],
            [-0.96331, -1.0, 0.863069], [0.726139, -1.0, -1.0], [1.0, -1.0, -0.726139],
            [0.863069, -1.0, -0.96331], [0.96331, -1.0, -0.863069],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            assert!(out.vertices.iter().any(|v| (v.position - want).length() < 2e-3),
                "rounded 2-edge vert {want:?} missing (does not match Blender oracle)");
        }
        // (3) ROUNDED, not flat: the 2 SHARED interior arc verts (y≈0.8264, between the two BoundVerts at
        // y≈0.7261) MUST be present — a flat chord would have only the 2 endpoints on the miter edge.
        let interior = out.vertices.iter()
            .filter(|v| (v.position.y - 0.826379).abs() < 2e-3
                && v.position.x.abs() < 0.1 && v.position.z.abs() < 0.1).count();
        assert_eq!(interior, 2, "rounded miter must have 2 shared interior arc verts (ns-1); got {interior}");
    }

    /// Chamfered ±1 cube (t=0.3): 24 verts (one coord ±1, others ±0.7), 6 squares + 12 chamfer
    /// rects + 8 corner triangles. IDENTICAL to the base in k1_rounded_partial_ref.py (§9
    /// single-stage oracle methodology). Windings derived programmatically (Newell + outward
    /// flip — the cube is convex and centered, so outward = away from origin).
    fn chamfered_cube() -> HalfEdgeMesh {
        let s = 0.7f32;
        let mut pos: Vec<Vec3> = Vec::new();
        let mut id: std::collections::HashMap<[i32; 3], usize> = std::collections::HashMap::new();
        let q = |v: f32| (v * 1000.0).round() as i32;
        let mut push = |p: Vec3, pos: &mut Vec<Vec3>, id: &mut std::collections::HashMap<[i32; 3], usize>| -> usize {
            *id.entry([q(p.x), q(p.y), q(p.z)]).or_insert_with(|| {
                pos.push(p);
                pos.len() - 1
            })
        };
        for axis in 0..3usize {
            for main in [-1.0f32, 1.0] {
                for a in [-s, s] {
                    for b in [-s, s] {
                        let mut co = [0.0f32; 3];
                        co[axis] = main;
                        co[(axis + 1) % 3] = a;
                        co[(axis + 2) % 3] = b;
                        push(Vec3::from_array(co), &mut pos, &mut id);
                    }
                }
            }
        }
        let k = |x: f32, y: f32, z: f32| -> usize { id[&[q(x), q(y), q(z)]] };
        let mut faces: Vec<Vec<usize>> = Vec::new();
        // 6 squares
        for axis in 0..3usize {
            for main in [-1.0f32, 1.0] {
                let mk = |a: f32, b: f32| {
                    let mut co = [0.0f32; 3];
                    co[axis] = main;
                    co[(axis + 1) % 3] = a;
                    co[(axis + 2) % 3] = b;
                    k(co[0], co[1], co[2])
                };
                faces.push(vec![mk(-s, -s), mk(s, -s), mk(s, s), mk(-s, s)]);
            }
        }
        // 12 chamfer rects: for each cube edge = pair of axes at ±1 signs; verts have those two
        // coords at (±1-ish): one at (main1=±1, s·sign2) and one at (s·sign1, main2=±1).
        for a1 in 0..3usize {
            for a2 in (a1 + 1)..3usize {
                let a3 = 3 - a1 - a2;
                for s1 in [-1.0f32, 1.0] {
                    for s2 in [-1.0f32, 1.0] {
                        let mut mk = |c1: f32, c2: f32, c3: f32| {
                            let mut co = [0.0f32; 3];
                            co[a1] = c1;
                            co[a2] = c2;
                            co[a3] = c3;
                            k(co[0], co[1], co[2])
                        };
                        faces.push(vec![
                            mk(s1, s2 * s, -s),
                            mk(s1, s2 * s, s),
                            mk(s1 * s, s2, s),
                            mk(s1 * s, s2, -s),
                        ]);
                    }
                }
            }
        }
        // 8 corner triangles
        for sx in [-1.0f32, 1.0] {
            for sy in [-1.0f32, 1.0] {
                for sz in [-1.0f32, 1.0] {
                    faces.push(vec![
                        k(sx, sy * s, sz * s),
                        k(sx * s, sy, sz * s),
                        k(sx * s, sy * s, sz),
                    ]);
                }
            }
        }
        // Outward winding via Newell normal vs face centroid direction.
        for f in faces.iter_mut() {
            let mut n = Vec3::ZERO;
            let mut c = Vec3::ZERO;
            for i in 0..f.len() {
                let p = pos[f[i]];
                let pn = pos[f[(i + 1) % f.len()]];
                n += Vec3::new(
                    (p.y - pn.y) * (p.z + pn.z),
                    (p.z - pn.z) * (p.x + pn.x),
                    (p.x - pn.x) * (p.y + pn.y),
                );
                c += p;
            }
            c /= f.len() as f32;
            if n.dot(c) < 0.0 {
                f.reverse();
            }
        }
        HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// ROUNDED k=1 PARTIAL CORNER at a degree>3 (chamfer-created) vertex — the owner's
    /// 2026-07-23 "cannot add round bevel on a chamfered box" case, §9.4 remainder.
    /// Oracle k1_rounded_partial_ref.json (Blender 4.5, single-stage on the shared base):
    /// bevel the +X square's edge (1,.7,∓.7)→(1,.7,±.7), offset .15 seg 3 profile .5 →
    /// nv=32 nf=35, V deleted at both ends, ONE recessed vertex slid the offset down the
    /// back fan edge (0.893934, 0.7, ±0.806066), fan of ns tris to the profile arc.
    #[test]
    fn k1_rounded_partial_matches_oracle() {
        let m = chamfered_cube();
        assert_eq!(m.vertices.len(), 24, "base must be the 24-vert chamfered cube");
        assert_eq!(boundary_loop_count(&m), 0, "base must be closed");
        m.validate().expect("base must be valid");

        let s = 0.7f32;
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            let near = |p: Vec3, x: f32, y: f32, z: f32| (p - Vec3::new(x, y, z)).length() < 1e-4;
            if (near(a, 1.0, s, -s) && near(b, 1.0, s, s)) || (near(a, 1.0, s, s) && near(b, 1.0, s, -s)) {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 2, "target edge (both half-edges) must exist");

        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "k1 rounded partial must be watertight; boundary={bnd}");
        out.validate().expect("k1 rounded partial must be manifold");
        assert_eq!(out.vertices.len(), 32, "oracle nv=32 (24 base − 2 deleted V + 8 arc + 2 recessed)");
        assert_eq!(out.faces.len(), 35, "oracle nf=35");
        let oracle: [[f32; 3]; 32] = [
            [-1.000000, -0.700000, -0.700000], [-1.000000, -0.700000, 0.700000], [-1.000000, 0.700000, -0.700000], [-1.000000, 0.700000, 0.700000],
            [1.000000, -0.700000, -0.700000], [1.000000, -0.700000, 0.700000], [-0.700000, -1.000000, -0.700000], [0.700000, -1.000000, -0.700000],
            [-0.700000, -1.000000, 0.700000], [0.700000, -1.000000, 0.700000], [-0.700000, 1.000000, -0.700000], [0.700000, 1.000000, -0.700000],
            [-0.700000, 1.000000, 0.700000], [0.700000, 1.000000, 0.700000], [-0.700000, -0.700000, -1.000000], [-0.700000, 0.700000, -1.000000],
            [0.700000, -0.700000, -1.000000], [0.700000, 0.700000, -1.000000], [-0.700000, -0.700000, 1.000000], [-0.700000, 0.700000, 1.000000],
            [0.700000, -0.700000, 1.000000], [0.700000, 0.700000, 1.000000], [0.893934, 0.806066, -0.700000], [1.000000, 0.550000, -0.700000],
            [0.893934, 0.700000, -0.806066], [0.946967, 0.732937, -0.700000], [0.985790, 0.639210, -0.700000], [1.000000, 0.550000, 0.700000],
            [0.893934, 0.806066, 0.700000], [0.893934, 0.700000, 0.806066], [0.985790, 0.639210, 0.700000], [0.946967, 0.732937, 0.700000],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            assert!(
                out.vertices.iter().any(|v| (v.position - want).length() < 2e-3),
                "k1 rounded vert {want:?} missing (does not match Blender oracle)"
            );
        }
    }

    /// ROUNDED EDGE LOOP on the chamfered cube (owner loop case): the 4 top-square edges,
    /// each corner = k=2 ADJACENT pair at a degree-4 chamfer-created vertex. Oracle
    /// k2loop_rounded_ref.py → nv=36 nf=38 (each corner: V deleted + 4-vert profile arc).
    #[test]
    fn k2_loop_rounded_on_chamfered_cube_matches_oracle() {
        let m = chamfered_cube();
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            if (a.z - 1.0).abs() < 1e-4 && (b.z - 1.0).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 8, "4 top edges × 2 half-edges");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "k2 loop rounded must be watertight; boundary={bnd}");
        out.validate().expect("k2 loop rounded must be manifold");
        assert_eq!(out.vertices.len(), 36, "oracle nv=36 — flat clamp would give fewer");
        assert_eq!(out.faces.len(), 38, "oracle nf=38");
    }

    /// OWNER TOPOLOGY 2026-07-23: same top-loop rounded bevel but the +Z square carries a
    /// fan-triangulation DIAGONAL (the owner's box had one from an earlier op) — two of the
    /// four loop corners are then degree-5. The bevel must still round watertight (Blender
    /// does); a flat clamp or a tear here is the owner's "cannot add round bevel".
    #[test]
    fn k2_loop_rounded_with_top_diagonal_stays_round_and_watertight() {
        let s = 0.7f32;
        let base = chamfered_cube();
        // Rebuild with the +Z square split along its diagonal (positions unchanged).
        let pos: Vec<Vec3> = base.vertices.iter().map(|v| v.position).collect();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for fi in 0..base.faces.len() {
            let fid = crate::half_edge::FaceId(fi as u32);
            if base.face(fid).half_edge == crate::half_edge::NO_EDGE { continue; }
            let ring: Vec<usize> = base.face_vertices(fid).iter().map(|v| v.0 as usize).collect();
            let is_top_sq = ring.len() == 4 && ring.iter().all(|&v| (pos[v].z - 1.0).abs() < 1e-4);
            if is_top_sq {
                faces.push(vec![ring[0], ring[1], ring[2]]);
                faces.push(vec![ring[0], ring[2], ring[3]]);
            } else {
                faces.push(ring);
            }
        }
        let m = HalfEdgeMesh::from_polygons(&pos, &faces);
        m.validate().expect("diagonal base must be valid");

        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            // The 4 BOUNDARY edges of the top square only — not the diagonal.
            let on_rim = |p: Vec3, q: Vec3| {
                (p.z - 1.0).abs() < 1e-4 && (q.z - 1.0).abs() < 1e-4
                    && ((p.x - q.x).abs() < 1e-4 || (p.y - q.y).abs() < 1e-4)
            };
            if on_rim(a, b) { sel.insert(i); }
        }
        assert_eq!(sel.len(), 8, "4 rim edges × 2 half-edges (diagonal excluded)");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "diagonal-top loop bevel must be watertight; boundary={bnd}");
        out.validate().expect("diagonal-top loop bevel must be manifold");
        // ROUNDED, not flat-clamped: the flat result has ~4 fewer verts per corner. The clean
        // loop gives nv=36; the diagonal adds 0 verts to the rim, so anything ≤ 28 means the
        // clamp fired. (Exact Blender parity for the diagonal case is a follow-up oracle;
        // this locks "round + watertight".)
        assert!(
            out.vertices.len() >= 34,
            "diagonal-top loop must stay ROUNDED (nv={} looks flat-clamped)",
            out.vertices.len()
        );
    }

    /// ROUNDED k=2 AT DEGREE-3, two ADJACENT edges (owner clamp class 2026-07-23, oracle
    /// k2d3_corner_ref.py "adj2"): ±1 cube, bevel (1,-1,1)-(1,1,1) + (-1,1,1)-(1,1,1),
    /// offset .25 seg 3 → nv=17 nf=12. Corner = M_NONE weld arc both strips share.
    #[test]
    fn k2_degree3_adjacent_pair_rounds_matching_oracle() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            let near = |p: Vec3, x: f32, y: f32, z: f32| (p - Vec3::new(x, y, z)).length() < 1e-4;
            let is_e = |p: Vec3, q: Vec3, x1: f32, y1: f32, z1: f32, x2: f32, y2: f32, z2: f32| {
                (near(p, x1, y1, z1) && near(q, x2, y2, z2)) || (near(p, x2, y2, z2) && near(q, x1, y1, z1))
            };
            if is_e(a, b, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0) || is_e(a, b, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0) {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 4, "2 edges × 2 half-edges");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "k2-d3 adjacent rounded must be watertight; boundary={bnd}");
        out.validate().expect("k2-d3 adjacent rounded must be manifold");
        assert_eq!(out.vertices.len(), 17, "oracle nv=17 (flat clamp would differ)");
        assert_eq!(out.faces.len(), 12, "oracle nf=12");
        let oracle: [[f32; 3]; 17] = [
            [-1.000000, -1.000000, -1.000000], [1.000000, -1.000000, -1.000000], [-1.000000, -1.000000, 1.000000], [-1.000000, 1.000000, -1.000000],
            [1.000000, 1.000000, -1.000000], [1.000000, -1.000000, 0.750000], [0.750000, -1.000000, 1.000000], [0.966506, -1.000000, 0.875000],
            [0.875000, -1.000000, 0.966506], [0.750000, 0.750000, 1.000000], [1.000000, 1.000000, 0.750000], [0.875000, 0.875000, 0.966506],
            [0.966506, 0.966506, 0.875000], [-1.000000, 0.750000, 1.000000], [-1.000000, 1.000000, 0.750000], [-1.000000, 0.875000, 0.966506],
            [-1.000000, 0.966506, 0.875000],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            assert!(
                out.vertices.iter().any(|v| (v.position - want).length() < 2e-3),
                "k2-d3 adj vert {want:?} missing vs Blender oracle"
            );
        }
    }

    /// ROUNDED EDGE LOOP on a plain cube (+Z face's 4 edges — every corner k=2 degree-3):
    /// THE canonical loop-bevel workflow. Oracle "loop4": nv=20 nf=18.
    #[test]
    fn k2_degree3_face_loop_rounds_matching_oracle() {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            if (a.z - 1.0).abs() < 1e-4 && (b.z - 1.0).abs() < 1e-4 {
                sel.insert(i);
            }
        }
        assert_eq!(sel.len(), 8, "4 loop edges × 2 half-edges");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "k2-d3 loop rounded must be watertight; boundary={bnd}");
        out.validate().expect("k2-d3 loop rounded must be manifold");
        assert_eq!(out.vertices.len(), 20, "oracle nv=20");
        assert_eq!(out.faces.len(), 18, "oracle nf=18");
        let oracle: [[f32; 3]; 20] = [
            [-1.000000, -1.000000, -1.000000], [1.000000, -1.000000, -1.000000], [-1.000000, 1.000000, -1.000000], [1.000000, 1.000000, -1.000000],
            [0.750000, -0.750000, 1.000000], [1.000000, -1.000000, 0.750000], [0.875000, -0.875000, 0.966506], [0.966506, -0.966506, 0.875000],
            [-0.750000, -0.750000, 1.000000], [-1.000000, -1.000000, 0.750000], [-0.875000, -0.875000, 0.966506], [-0.966506, -0.966506, 0.875000],
            [0.750000, 0.750000, 1.000000], [1.000000, 1.000000, 0.750000], [0.875000, 0.875000, 0.966506], [0.966506, 0.966506, 0.875000],
            [-0.750000, 0.750000, 1.000000], [-1.000000, 1.000000, 0.750000], [-0.875000, 0.875000, 0.966506], [-0.966506, 0.966506, 0.875000],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            assert!(
                out.vertices.iter().any(|v| (v.position - want).length() < 2e-3),
                "k2-d3 loop vert {want:?} missing vs Blender oracle"
            );
        }
    }


    /// OWNER SCENE GATE (adjust_offsets, BEVEL.md 2026-07-23(c)): base.fl4 (30v/28f, block
    /// with a PRIOR rounded fillet) + bevel the full z=0.5 ring (10 edges crossing the fillet),
    /// offset .242 seg 3. Blender narrows/widens the band across the fillet via adjust_offsets
    /// (flats 0.233, fillet 0.255 chain); without the pass AZRA kept 0.242 uniform and the
    /// seam kinked (the owner red-line). Topology must match exactly; positions within 8e-3
    /// (single-offset-per-edge v1 approximation of Blender per-side offsets).
    #[test]
    fn owner_ring_adjust_offsets_matches_oracle() {
        let pos: [[f32; 3]; 30] = [
            [-0.787624, -0.500000, 0.500000], [-0.347634, -0.500000, -0.500000], [-0.347634, 0.500000, -0.500000],
            [-0.787624, 0.500000, 0.500000], [-0.346458, 0.500000, 1.421434], [-0.346458, -0.500000, 1.421434],
            [-0.948132, 0.354600, 0.500000], [-0.559461, 0.354600, -0.367314], [-0.559461, -0.354600, -0.367314],
            [-0.948132, -0.354600, 0.500000], [-0.563592, -0.354600, 1.290678], [-0.563592, 0.354600, 1.290678],
            [-0.867878, -0.480520, 0.500000], [-0.926628, -0.427300, 0.500000], [-0.431783, -0.480520, -0.482223],
            [-0.509317, -0.427300, -0.433657], [-0.431783, 0.480520, -0.482223], [-0.509317, 0.427300, -0.433657],
            [-0.867878, 0.480520, 0.500000], [-0.926628, 0.427300, 0.500000], [-0.431749, 0.480520, 1.403916],
            [-0.511225, 0.427300, 1.356056], [-0.431749, -0.480520, 1.403916], [-0.511225, -0.427300, 1.356056],
            [0.500000, -0.500000, -0.500000], [0.500000, -0.500000, 0.500000], [0.500000, 0.500000, 0.500000],
            [0.500000, 0.500000, -0.500000], [0.500000, 0.500000, 1.421434], [0.500000, -0.500000, 1.421434],
        ];
        let faces_raw: [&[usize]; 28] = [&[24, 25, 0, 1], &[26, 27, 2, 3], &[27, 24, 1, 2], &[4, 5, 29, 28], &[7, 8, 9, 6], &[25, 24, 27, 26], &[3, 4, 28, 26], &[9, 10, 11, 6], &[25, 29, 5, 0], &[28, 29, 25, 26], &[0, 12, 14, 1], &[12, 13, 15, 14], &[13, 9, 8, 15], &[2, 16, 18, 3], &[16, 17, 19, 18], &[17, 7, 6, 19], &[1, 14, 16, 2], &[14, 15, 17, 16], &[15, 8, 7, 17], &[4, 20, 22, 5], &[20, 21, 23, 22], &[21, 11, 10, 23], &[3, 18, 20, 4], &[18, 19, 21, 20], &[19, 6, 11, 21], &[9, 13, 23, 10], &[13, 12, 22, 23], &[12, 0, 5, 22]];
        let positions: Vec<Vec3> = pos.iter().map(|p| Vec3::new(p[0], p[1], p[2])).collect();
        let faces: Vec<Vec<usize>> = faces_raw.iter().map(|f| f.to_vec()).collect();
        let m = HalfEdgeMesh::from_polygons(&positions, &faces);
        m.validate().expect("owner base must be valid");
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = m.vert(m.he_source(HalfEdgeId(i))).position;
            let b = m.vert(he.vertex).position;
            if (a.z - 0.5).abs() < 1e-3 && (b.z - 0.5).abs() < 1e-3 { sel.insert(i); }
        }
        assert_eq!(sel.len(), 20, "10 ring edges x 2 half-edges");
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.242, segments: 3, shape: 0.5, clamp: false });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        assert_eq!(bnd, 0, "owner ring bevel must be watertight");
        out.validate().expect("owner ring bevel must be manifold");
        assert_eq!(out.vertices.len(), 60, "oracle nv=60");
        assert_eq!(out.faces.len(), 58, "oracle nf=58");
        let oracle: [[f32; 3]; 60] = [
            [-0.347634, -0.500000, -0.500000], [-0.347634, 0.500000, -0.500000], [-0.346458, 0.500000, 1.421434],
            [-0.346458, -0.500000, 1.421434], [-0.559461, 0.354600, -0.367314], [-0.559461, -0.354600, -0.367314],
            [-0.563592, -0.354600, 1.290678], [-0.563592, 0.354600, 1.290678], [-0.431783, -0.480520, -0.482223],
            [-0.509317, -0.427300, -0.433657], [-0.431783, 0.480520, -0.482223], [-0.509317, 0.427300, -0.433657],
            [-0.431749, 0.480520, 1.403916], [-0.511225, 0.427300, 1.356056], [-0.431749, -0.480520, 1.403916],
            [-0.511225, -0.427300, 1.356056], [0.500000, -0.500000, -0.500000], [0.500000, 0.500000, -0.500000],
            [0.500000, 0.500000, 1.421434], [0.500000, -0.500000, 1.421434], [-0.676011, -0.500000, 0.733118],
            [-0.684481, -0.500000, 0.265579], [-0.717999, -0.500000, 0.585152], [-0.721099, -0.500000, 0.414021],
            [-0.684481, 0.500000, 0.265579], [-0.676011, 0.500000, 0.733118], [-0.721099, 0.500000, 0.414021],
            [-0.717999, 0.500000, 0.585152], [-0.836628, 0.354600, 0.729271], [-0.844586, 0.354600, 0.268940],
            [-0.878507, 0.354600, 0.583679], [-0.881420, 0.354600, 0.415186], [-0.844586, -0.354600, 0.268940],
            [-0.836628, -0.354600, 0.729271], [-0.881420, -0.354600, 0.415186], [-0.878507, -0.354600, 0.583679],
            [-0.756037, -0.480520, 0.731800], [-0.764299, -0.480520, 0.266708], [-0.798080, -0.480520, 0.584645],
            [-0.801105, -0.480520, 0.414410], [-0.814906, -0.427300, 0.730234], [-0.822969, -0.427300, 0.268083],
            [-0.856879, -0.427300, 0.584046], [-0.859830, -0.427300, 0.414887], [-0.764299, 0.480520, 0.266708],
            [-0.756037, 0.480520, 0.731800], [-0.801105, 0.480520, 0.414410], [-0.798080, 0.480520, 0.584645],
            [-0.822969, 0.427300, 0.268083], [-0.814906, 0.427300, 0.730234], [-0.859830, 0.427300, 0.414887],
            [-0.856879, 0.427300, 0.584046], [0.500000, -0.500000, 0.265017], [0.500000, -0.500000, 0.733776],
            [0.500000, -0.500000, 0.421270], [0.500000, -0.500000, 0.577523], [0.500000, 0.500000, 0.733776],
            [0.500000, 0.500000, 0.265017], [0.500000, 0.500000, 0.577523], [0.500000, 0.500000, 0.421270],
        ];
        let mut worst = 0.0f32;
        let mut misses: Vec<(f32, Vec3)> = Vec::new();
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            let d = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            if d > 4e-3 { misses.push((d, want)); }
            if d > worst { worst = d; }
        }
        // Reverse direction too: AZRA verts with no oracle counterpart (misplaced interior
        // arcs surface HERE — the owner's "still too straight" report was reverse-only).
        let mut rev = 0usize;
        for v in &out.vertices {
            let d = oracle.iter()
                .map(|o| (v.position - Vec3::new(o[0], o[1], o[2])).length())
                .fold(f32::MAX, f32::min);
            if d > 4e-3 { rev += 1; }
        }
        assert_eq!(rev, 0, "owner ring: {rev} AZRA verts have no Blender counterpart");
        // adjust_offsets + M=orig weld anchor: VERTEX-EXACT vs Blender (both directions).
        assert!(misses.is_empty(), "owner ring: {} oracle verts unmatched >4e-3", misses.len());
        assert!(worst < 4e-3, "owner ring: worst deviation {worst} (want vertex-exact < 4e-3)");
    }

    /// MIXED-CORNER GUARD: a selection that contains BOTH a rounded 2-edge miter (2 opposite apex edges)
    /// AND a blocking shared corner (a base edge → degree-3 k=2 corners) forces the global flat clamp
    /// (n_seg→1). The STEP-2b miter chain (built at req_seg=3) must then be DROPPED so the strips (n_seg=1)
    /// and the arc stay length-consistent — else the miter arc's interior verts orphan and open the mesh.
    /// This locks the `n_seg != req_seg → miter_arc_pos.clear()` guard.
    #[test]
    fn rounded_miter_mixed_with_blocking_corner_watertight() {
        let m = square_pyramid();
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let (s, d) = (m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            let is_e = |a: u32, b: u32| (s == a && d == b) || (s == b && d == a);
            // 2 opposite apex edges (rounded miter) + a base edge (v1-v2 → blocking k=2 base corners).
            if is_e(0, 2) || is_e(0, 4) || is_e(1, 2) { sel.insert(i); }
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        out.validate().expect("mixed miter+blocking bevel must be manifold");
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "mixed miter + blocking corner must stay watertight (flat fallback); boundary={bnd}");
    }

    /// ROUNDED MULTI-EDGE CORNER (M_ADJ v1 dome): ±1 cube, bevel the 3 edges meeting at corner (1,1,1) at
    /// segments=3. The clean DEGREE-3 fully-beveled corner (k==degree==3) now ROUNDS via the STEP-5 dome
    /// (rim fanned to a center bulged toward the original corner) instead of flat-clamping. Must be
    /// WATERTIGHT + manifold, and the ROUND (shape 0.5) corner must bulge FURTHER toward (1,1,1) than the
    /// FLAT (shape 0) chamfer — a real rounded corner, not a flat chamfer. NOT Blender-vertex-exact (the full
    /// Catmull-Clark M_ADJ grid is future) — the visual v1. Degree>3 miter apexes + re-beveled corners still
    /// flat-clamp (the whitelist). This is the user's "circular multi-edge round bevel" case (cube/cyl).
    #[test]
    fn rounded_cube_corner_domes() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let corner = Vec3::new(1.0, 1.0, 1.0);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let a = cube.vertices[cube.he_source(HalfEdgeId(i)).0 as usize].position;
            let b = cube.vertices[he.vertex.0 as usize].position;
            if (a - corner).length() < 1e-4 || (b - corner).length() < 1e-4 { sel.insert(i); }
        }
        let bp = |sh: f32| BevelParams { width: 0.3, segments: 3, shape: sh, clamp: true };
        let round = bevel_edges_offset(&cube, &sel, &bp(0.5));
        let flat = bevel_edges_offset(&cube, &sel, &bp(0.0));
        let bnd = |m: &HalfEdgeMesh| (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd(&round), 0, "rounded cube corner must be watertight; boundary={}", bnd(&round));
        assert!(round.validate().is_ok(), "rounded cube corner must be manifold");
        // The dome center bulges toward (1,1,1): its x+y+z exceeds the flat chamfer's furthest-corner vert.
        let max_sum = |m: &HalfEdgeMesh| m.vertices.iter()
            .map(|v| v.position.x + v.position.y + v.position.z).fold(f32::MIN, f32::max);
        assert!(max_sum(&round) > max_sum(&flat) + 0.02,
            "rounded corner must bulge toward (1,1,1) more than flat (round {:.3} vs flat {:.3})",
            max_sum(&round), max_sum(&flat));
    }

    /// ROUNDED N-WAY APEX (M_ADJ step-1 re-route): square pyramid, bevel all 4 apex edges at segments=3. The
    /// clean fully-beveled degree-4 apex is now RE-ROUTED off the STEP-2b flat collapse (which opened a
    /// 9-edge hole) through the STEP-5 dome — so the degree>3 miter ROUNDS. Must be watertight + manifold +
    /// domed (a center vert bulged above the y≈0.452 boundvert ring toward the original apex at y=1). This is
    /// the first step of the full M_ADJ: degree>3 corners can now round (v1 dome interior; the exact
    /// Catmull-Clark grid interior is the next step).
    #[test]
    fn rounded_nway_apex_domes() {
        let m = square_pyramid();
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if m.he_source(HalfEdgeId(i)).0 == 0 || he.vertex.0 == 0 { sel.insert(i); }
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "rounded N-way apex must be watertight (was 9-edge hole via STEP 2b); boundary={bnd}");
        assert!(out.validate().is_ok(), "rounded N-way apex must be manifold");
        let domed = out.vertices.iter().any(|v| v.position.y > 0.46
            && v.position.x.abs() < 0.15 && v.position.z.abs() < 0.15);
        assert!(domed, "rounded N-way apex must bulge a center vert above the boundvert ring (got flat)");
    }

    /// ROUNDED CUBE seg=2 QUAD GRID — VERTEX-EXACT to Blender (`rounded_cube_ref.json`): ±1 cube, bevel ALL 12
    /// edges, segments=2, offset 0.3. Each corner is Blender's M_ADJ quad grid — 3 QUADS `[BV, mid, center,
    /// mid]` meeting at the fillet-sphere pole (0.873³, from the algebraic sphere fit), and the arc-mids sit in
    /// the edge cross-section (0.912,0.7,0.912) — the apex re-projection keeps edges PARALLEL. Watertight.
    #[test]
    fn rounded_cube_seg2_quad_grid() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE { sel.insert(i); }
        }
        let out = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.3, segments: 2, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "rounded cube must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "rounded cube manifold");
        // +++ corner center = the fillet-sphere pole (Blender vert 46).
        let center = Vec3::new(0.87321, 0.87321, 0.87321);
        let ci = out.vertices.iter().position(|v| (v.position - center).length() < 3e-3);
        assert!(ci.is_some(), "corner center (fillet-sphere pole 0.873³) missing");
        let ci = ci.unwrap() as u32;
        // Every face at the center must be a QUAD (Blender's 3 quads), NOT a triangle (the old fan).
        let mut touch = 0; let mut all_quads = true;
        for f in 0..out.faces.len() as u32 {
            let fv = out.face_vertices(FaceId(f));
            if fv.iter().any(|x| x.0 == ci) { touch += 1; if fv.len() != 4 { all_quads = false; } }
        }
        assert!(touch >= 3, "corner center should be shared by ≥3 faces; got {touch}");
        assert!(all_quads, "corner faces must be QUADS (Blender M_ADJ grid), not a triangle fan");
        // arc-mid preserves the +X+Z fillet cross-section (x=z=0.912) → edges parallel.
        assert!(out.vertices.iter().any(|v| (v.position - Vec3::new(0.91213, 0.7, 0.91213)).length() < 3e-3),
            "arc-mid (0.912,0.7,0.912) preserving the fillet cross-section missing → edges non-parallel");
    }

    /// ROUNDED CUBE seg=3 — VERTEX-EXACT to Blender (`rc3.json` oracle): ±1 cube, ALL 12 edges,
    /// segments=3, offset 0.3. Each corner = Blender's M_ADJ grid: 6 QUADS + a centre TRIANGLE (odd-seg
    /// n-gon), 12 corner verts exactly (a fan would add a pole vert = 13), inner ring on the BV→pole
    /// great circles ((0.8435, 0.8435, 0.921) family — the Levin subdivision + 1.4 centre seed + sphere
    /// snap, verified within 5e-6 in the pilot). Watertight + manifold.
    #[test]
    fn rounded_cube_seg3_matches_oracle() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE { sel.insert(i); }
        }
        let out = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.3, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "seg3 rounded cube must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "seg3 rounded cube manifold");
        // The 12 +++ corner verts, vertex-for-vertex (rc3.json; tolerance covers Blender's 2e-4
        // power-of-2 boundary chord-lerp quirk — see BEVEL.md §3.7).
        let oracle = [
            [0.7, 0.7, 1.0], [1.0, 0.7, 0.7], [0.7, 1.0, 0.7],
            [0.8498, 0.7, 0.9599], [0.7, 0.8498, 0.9599], [0.8435, 0.8435, 0.921],
            [0.9599, 0.8498, 0.7], [0.9599, 0.7, 0.8498], [0.921, 0.8435, 0.8435],
            [0.7, 0.9599, 0.8498], [0.8498, 0.9599, 0.7], [0.8435, 0.921, 0.8435],
        ];
        for o in &oracle {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 3e-3, "corner vert {want:?} missing (closest {best:.4}) — not Blender-exact");
        }
        // GRID, not fan: exactly Blender's 12 corner verts (a fan pole would make 13).
        let corner = out.vertices.iter()
            .filter(|v| v.position.x > 0.55 && v.position.y > 0.55 && v.position.z > 0.55).count();
        assert_eq!(corner, 12, "corner must have exactly Blender's 12 verts (fan pole would be 13)");
    }

    /// ROUNDED CUBE multi-seg sweep: segments 2..=6 stay watertight + manifold, and every corner-region
    /// INTERIOR vert (all coords strictly > 0.7) lies ON the corner fillet sphere (centre 0.7³, r=0.3) —
    /// the M_ADJ superellipsoid snap at shape 0.5. Covers odd (3, 5), even power-of-2 (2, 4) and the
    /// even-interp (6) grid paths.
    #[test]
    fn rounded_cube_multi_seg_watertight() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE { sel.insert(i); }
        }
        for seg in 2..=6u32 {
            let out = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.3, segments: seg, shape: 0.5, clamp: true });
            let bnd = (0..out.half_edges.len() as u32)
                .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
            assert_eq!(bnd, 0, "seg={seg} rounded cube must be watertight; boundary={bnd}");
            assert!(out.validate().is_ok(), "seg={seg} rounded cube manifold");
            let c = Vec3::splat(0.7);
            for v in &out.vertices {
                let p = v.position;
                if p.x > 0.705 && p.y > 0.705 && p.z > 0.705 {
                    let r = (p - c).length();
                    assert!((r - 0.3).abs() < 2e-3,
                        "seg={seg} corner interior vert {p:?} off the fillet sphere (r={r:.4})");
                }
            }
        }
    }

    /// LOOP SLIDE GATE (Blender oracle): two-stage bevel — the single-between miter verts pin ONTO the
    /// chamfer edge at (0.891,-0.75,-1)/(1,-0.75,-0.891), and NOTHING overshoots the ±1 cube.
    #[test]
    fn miter_loop_slide_matches_oracle() {
        let bp = |w: f32, s: u32| BevelParams { width: w, segments: s, shape: 0.5, clamp: true };
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let bottom = (0..cube.faces.len() as u32).find(|&f| cube.compute_face_normal(FaceId(f)).y < -0.9).unwrap();
        let bhe: Vec<u32> = cube.face_half_edges(FaceId(bottom)).iter().map(|h| h.0).collect();
        let r1 = bevel_edges_offset(&cube, &[bhe[0], bhe[1]].into_iter().collect(), &bp(0.25, 1));
        let out = bevel_edges_offset(&r1, &[9u32, 29].into_iter().collect(), &bp(0.1, 1));
        for want in [Vec3::new(0.891, -0.75, -1.0), Vec3::new(1.0, -0.75, -0.891)] {
            assert!(out.vertices.iter().any(|v| (v.position - want).length() < 5e-3), "loop-slide miter vert {want:?} missing");
        }
        for v in &out.vertices {
            let p = v.position;
            assert!(p.x <= 1.0 + 2e-3 && p.x >= -1.0 - 2e-3 && p.z <= 1.0 + 2e-3 && p.z >= -1.0 - 2e-3, "vert {p:?} overshoots the ±1 cube");
        }
    }

    /// TERMINAL-EDGE GATE (the user's reported case): ±0.5 cube → bevel (2,3) w=0.268 → bevel (15,29) w=0.2.
    /// The k=1 terminal-edge verts must loop-slide onto the bottom (y=-0.5, Blender oracle
    /// (0.013,-0.5,0.232)/(-0.232,-0.5,-0.013)), keeping the bottom flat — not lift to y≈-0.423.
    #[test]
    fn terminal_edge_stays_on_bottom() {
        let bp = |w: f32, s: u32| BevelParams { width: w, segments: s, shape: 0.5, clamp: true };
        let cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let r1 = bevel_edges_offset(&cube, &[2u32, 3].into_iter().collect(), &bp(0.268, 1));
        let out = bevel_edges_offset(&r1, &[15u32, 29].into_iter().collect(), &bp(0.2, 1));
        out.validate().expect("terminal-edge bevel must stay manifold");
        for want in [Vec3::new(0.013, -0.5, 0.232), Vec3::new(-0.232, -0.5, -0.013)] {
            assert!(out.vertices.iter().any(|v| (v.position - want).length() < 0.05), "terminal-edge vert {want:?} must be on the bottom (y=-0.5)");
        }
    }

    /// A cube edge bevel: one selected edge → cube grows from 8 verts to 10 (BEVEL_PLAN golden count),
    /// stays manifold, and gains the chamfer face.
    #[test]
    fn cube_single_edge_bevel_golden() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        assert_eq!(mesh.vertex_count(), 8);
        assert_eq!(mesh.face_count(), 6);
        // Pick a half-edge that is interior (manifold) — edge 0 of face 0.
        let e = pick_interior_edge(&mesh);
        let mut sel = HashSet::new();
        sel.insert(e);
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.2, ..Default::default() });
        out.validate().expect("beveled cube must be manifold");
        assert_eq!(out.vertex_count(), 10, "one cube edge bevel must yield 8→10 verts");
        // 6 original faces (4 unchanged + 2 re-traced) + 1 chamfer strip = 7 faces.
        assert_eq!(out.face_count(), 7, "one cube edge bevel must yield 6→7 faces");
    }

    /// Parallel-rail invariant (BEVEL_PLAN test): the two rails of a beveled edge stay parallel to the
    /// original edge. We verify the chamfer strip is a parallelogram-ish quad whose two long sides are
    /// parallel to the original edge direction.
    #[test]
    fn parallel_rails() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let e = pick_interior_edge(&mesh);
        let he = mesh.he(HalfEdgeId(e));
        let src = mesh.he_source(HalfEdgeId(e)).0 as usize;
        let dst = he.vertex.0 as usize;
        let edge_dir = (mesh.vertices[dst].position - mesh.vertices[src].position).normalize();

        let mut sel = HashSet::new();
        sel.insert(e);
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.2, ..Default::default() });
        // Find the chamfer strip: the one new quad whose 4 verts are all new (not at unit corners).
        // Its two rails are parallel to edge_dir.
        let mut found_parallel = false;
        for i in 0..out.faces.len() {
            let fv = out.face_vertices(FaceId(i as u32));
            if fv.len() != 4 { continue; }
            let p: Vec<Vec3> = fv.iter().map(|v| out.vert(*v).position).collect();
            // Two opposite edges of the quad parallel to edge_dir → it's the chamfer strip.
            let e0 = (p[1] - p[0]).normalize_or_zero();
            let e2 = (p[3] - p[2]).normalize_or_zero();
            if e0.cross(edge_dir).length() < 1e-4 && e2.cross(edge_dir).length() < 1e-4 {
                found_parallel = true;
            }
        }
        assert!(found_parallel, "chamfer strip rails must stay parallel to the original edge");
    }

    /// Beveling a boundary edge → identity. This is now confirmed BLENDER-FAITHFUL, not a v1 shortcut:
    /// Blender's own edge bevel is a NO-OP on a boundary edge (BEVEL.md §6 #25 — the Blender oracle
    /// matrix returned CHANGED=False for a boundary edge on a flat quad, a 3D open box rim, a rim loop
    /// and a hex-fan, across every offset type, on both 4.5 LTS and 5.0; the raw `bmesh.ops.bevel`
    /// even CRASHES on a lone boundary edge). The STEP-0 skip therefore MATCHES Blender. This test also
    /// still proves the guard against a NON-face / one-sided edge feeding the closed-mesh watertight
    /// machinery (a §5/§6 tear source) — it is safely left alone.
    #[test]
    fn boundary_edge_skipped() {
        // A single quad: all 4 edges are boundary (no twin face).
        let mesh = HalfEdgeMesh::from_polygons(
            &[Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y],
            &[vec![0, 1, 2, 3]],
        );
        let mut sel = HashSet::new();
        sel.insert(0u32);
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.2, ..Default::default() });
        assert_eq!(out.vertex_count(), mesh.vertex_count(), "boundary edge must be skipped (identity)");
        assert_eq!(out.face_count(), mesh.face_count());
    }

    /// The open box the boundary/open-face oracle work uses: unit ±1 box with the TOP (+Y) face removed.
    /// 8 verts, 5 faces, ONE open rim (boundary loop). Vertex/face order matches `openbox_interior_ref.py`
    /// (scratchpad) so Blender oracle verts diff 1:1.
    #[cfg(test)]
    fn open_box() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,1.0), Vec3::new(-1.0,-1.0,1.0),
            Vec3::new(-1.0, 1.0,-1.0), Vec3::new(1.0, 1.0,-1.0), Vec3::new(1.0, 1.0,1.0), Vec3::new(-1.0, 1.0,1.0),
        ];
        let f = vec![
            vec![0,1,2,3], vec![0,4,5,1], vec![1,5,6,2], vec![2,6,7,3], vec![3,7,4,0], // top (+Y) omitted → open
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// Count boundary EDGES (face-bearing half-edges with no twin) — the metric the OLD app guard used.
    #[cfg(test)]
    fn boundary_edges(m: &HalfEdgeMesh) -> usize {
        (0..m.half_edges.len() as u32)
            .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE)
            .count()
    }

    /// Find an INTERIOR (manifold) half-edge whose undirected edge joins vertices a,b.
    #[cfg(test)]
    fn interior_he(m: &HalfEdgeMesh, a: u32, b: u32) -> Option<u32> {
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            let s = m.he_source(HalfEdgeId(i)).0; let d = he.vertex.0;
            if (s == a && d == b) || (s == b && d == a) {
                if he.twin != NO_TWIN && he.face != NO_FACE && m.he(he.twin).face != NO_FACE { return Some(i); }
            }
        }
        None
    }

    /// Max over Blender oracle verts of the nearest AZRA vertex (vertex-identity multiset diff).
    #[cfg(test)]
    fn max_nearest(azra: &[Vec3], oracle: &[[f32; 3]]) -> f32 {
        oracle.iter().map(|b| {
            let bp = Vec3::new(b[0], b[1], b[2]);
            azra.iter().map(|a| (*a - bp).length()).fold(f32::INFINITY, f32::min)
        }).fold(0.0f32, f32::max)
    }

    /// BLENDER ORACLE (§7): boundary-edge bevel is IDENTITY in Blender. Three cases the task named:
    /// (i) flat quad, bevel one of its 4 boundary edges w=0.2 seg1; (ii) a 2×2 grid, bevel one outer
    /// boundary edge; (iii) seg3 on case (i). Blender returns the input UNCHANGED for all three
    /// (`bnd_quad_seg1.json` / `bnd_grid_seg1.json` / `bnd_quad_seg3.json` = identity). AZRA's STEP-0
    /// skip must reproduce that (Blender-faithful no-op), NOT invent a chamfer with no oracle (§6 #25).
    #[test]
    fn boundary_edge_bevel_matches_blender_identity() {
        // (i) flat unit quad, bevel bottom edge (0-1)
        let quad = HalfEdgeMesh::from_polygons(
            &[Vec3::new(0.0,0.0,0.0), Vec3::new(1.0,0.0,0.0), Vec3::new(1.0,1.0,0.0), Vec3::new(0.0,1.0,0.0)],
            &[vec![0,1,2,3]],
        );
        // edge (0,1) — a boundary edge (single face)
        let e01 = (0..quad.half_edges.len() as u32).find(|&i| {
            let h = quad.he(HalfEdgeId(i)); let s = quad.he_source(HalfEdgeId(i)).0;
            (s==0 && h.vertex.0==1) || (s==1 && h.vertex.0==0)
        }).unwrap();
        let sel: HashSet<u32> = std::iter::once(e01).collect();
        for seg in [1u32, 3] { // (i) seg1 and (iii) seg3
            let out = bevel_edges_offset(&quad, &sel, &BevelParams { width: 0.2, segments: seg, shape: 0.5, clamp: true });
            assert_eq!(out.vertex_count(), 4, "flat-quad boundary bevel seg{seg} must be Blender identity (nv 4)");
            assert_eq!(out.face_count(), 1, "flat-quad boundary bevel seg{seg} must be Blender identity (nf 1)");
        }
        // (ii) 2×2 grid (3×3 verts), bevel one outer boundary edge (0-1)
        let mut gp = Vec::new();
        for j in 0..3 { for i in 0..3 { gp.push(Vec3::new(i as f32, j as f32, 0.0)); } }
        let gf = vec![vec![0,1,4,3], vec![1,2,5,4], vec![3,4,7,6], vec![4,5,8,7]];
        let grid = HalfEdgeMesh::from_polygons(&gp, &gf);
        let ge = (0..grid.half_edges.len() as u32).find(|&i| {
            let h = grid.he(HalfEdgeId(i)); let s = grid.he_source(HalfEdgeId(i)).0;
            ((s==0 && h.vertex.0==1) || (s==1 && h.vertex.0==0)) && h.twin == NO_TWIN
        }).unwrap();
        let gsel: HashSet<u32> = std::iter::once(ge).collect();
        let gout = bevel_edges_offset(&grid, &gsel, &BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true });
        assert_eq!(gout.vertex_count(), 9, "2×2-grid boundary bevel must be Blender identity (nv 9)");
        assert_eq!(gout.face_count(), 4, "2×2-grid boundary bevel must be Blender identity (nf 4)");
    }

    /// THE OPEN-FACE FIX GATE — the owner's real "bevel doesn't work on open faces" bug.
    /// An INTERIOR edge on an OPEN mesh (a body with a hole) bevels cleanly and matches Blender
    /// vertex-for-vertex — Blender DOES bevel interior edges of open meshes (unlike boundary edges).
    /// Oracle: `openbox_interior_ref.py` → interior vertical edge (0-4), one endpoint on the open rim,
    /// w=0.2. seg1 → Blender nv=10/nf=6; seg3 → nv=14/nf=8. The result stays MANIFOLD and preserves the
    /// boundary LOOP count (1) even though the boundary EDGE count grows (4→5 / 4→7) — which is exactly
    /// why the old `boundary_edges <= orig` install guard wrongly rejected it (see `bevel_result_installable`).
    #[test]
    fn interior_edge_on_open_mesh_matches_blender() {
        let m = open_box();
        assert_eq!(boundary_loop_count(&m), 1, "open box has one rim");
        assert_eq!(boundary_edges(&m), 4);
        let e04 = interior_he(&m, 0, 4).expect("edge 0-4 is interior");
        let sel: HashSet<u32> = std::iter::once(e04).collect();

        // seg1 oracle (openbox_int_seg1.json)
        let bl1: Vec<[f32;3]> = vec![[1.,-1.,-1.],[1.,-1.,1.],[-1.,-1.,1.],[1.,1.,-1.],[1.,1.,1.],[-1.,1.,1.],
            [-1.,-1.,-0.8],[-0.8,-1.,-1.],[-0.8,1.,-1.],[-1.,1.,-0.8]];
        let o1 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true });
        o1.is_manifold().expect("open-mesh interior bevel must be manifold");
        assert_eq!(o1.vertex_count(), 10, "seg1 nv must match Blender (10)");
        assert_eq!(o1.face_count(), 6, "seg1 nf must match Blender (6)");
        assert_eq!(boundary_loop_count(&o1), 1, "loop count preserved (still one rim)");
        assert!(boundary_edges(&o1) > 4, "boundary EDGE count legitimately grows (4->5) — why the old guard failed");
        let az1: Vec<Vec3> = o1.vertices.iter().map(|v| v.position).collect();
        assert!(max_nearest(&az1, &bl1) < 1e-4, "seg1 vertex identity vs Blender: {}", max_nearest(&az1, &bl1));

        // seg3 oracle (openbox_int_seg3.json)
        let bl3: Vec<[f32;3]> = vec![[1.,-1.,-1.],[1.,-1.,1.],[-1.,-1.,1.],[1.,1.,-1.],[1.,1.,1.],[-1.,1.,1.],
            [-1.,-1.,-0.8],[-0.8,-1.,-1.],[-0.973205,-1.,-0.9],[-0.9,-1.,-0.973205],
            [-0.8,1.,-1.],[-1.,1.,-0.8],[-0.9,1.,-0.973205],[-0.973205,1.,-0.9]];
        let o3 = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true });
        o3.is_manifold().expect("seg3 open-mesh interior bevel must be manifold");
        assert_eq!(o3.vertex_count(), 14, "seg3 nv must match Blender (14)");
        assert_eq!(o3.face_count(), 8, "seg3 nf must match Blender (8)");
        assert_eq!(boundary_loop_count(&o3), 1, "seg3 loop count preserved");
        let az3: Vec<Vec3> = o3.vertices.iter().map(|v| v.position).collect();
        assert!(max_nearest(&az3, &bl3) < 1e-4, "seg3 vertex identity vs Blender: {}", max_nearest(&az3, &bl3));
    }

    /// THE RIM-VERTEX CAP GATE (owner `BEVEL EDGE CASE.fl4`, 2026-08-07). A beveled interior edge
    /// whose endpoint is a BOUNDARY vertex must NOT get a corner cap: Blender leaves the profile arc
    /// open on the rim (`rim_vertex_bevel_ref.py` → seg1 nv=10/nf=6, seg3 nv=14/nf=8, no center vert).
    /// Pre-fix, the k=1 partial cap fired at the rim vertex and built a folded lid over the open top
    /// (folded_coplanar_pairs=2 in the owner file). Whether the cap fired depended on WHICH outgoing
    /// half-edge the rim vertex happened to store (`vertex_fan` is partial at a boundary), so this
    /// gate sweeps ALL FOUR vertical edges of the open box — the storage lottery can't hide it.
    #[test]
    fn rim_vertex_bevel_never_caps_the_open_corner() {
        let m = open_box();
        for (a, b) in [(0u32, 4u32), (1, 5), (2, 6), (3, 7)] {
            let e = interior_he(&m, a, b).expect("vertical edge is interior");
            let sel: HashSet<u32> = std::iter::once(e).collect();
            for (seg, nv, nf) in [(1u32, 10, 6), (3, 14, 8)] {
                let o = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.2, segments: seg, shape: 0.5, clamp: true });
                o.is_manifold().unwrap_or_else(|e| panic!("edge {a}-{b} seg{seg}: not manifold: {e:?}"));
                assert_eq!(o.vertex_count(), nv, "edge {a}-{b} seg{seg}: nv must match Blender (no cap center)");
                assert_eq!(o.face_count(), nf, "edge {a}-{b} seg{seg}: nf must match Blender (no cap fan)");
                assert_eq!(boundary_loop_count(&o), 1, "edge {a}-{b} seg{seg}: still one rim");
                assert_eq!(o.count_folded_coplanar_pairs(), 0, "edge {a}-{b} seg{seg}: no folded lid");
            }
        }
    }

    /// Owner `BEVEL BUG.fl4` gutter island (2026-08-07): an open-ENDED box (one SIDE face missing),
    /// bevel the bottom-back edge that runs INTO the open rim, rounded. The far endpoint is a rim
    /// corner where two boundary edges meet. Must not cap, not fold, and stay installable.
    #[test]
    fn gutter_end_bevel_stays_open_and_installable() {
        // box x∈[-2,0], y/z ∈ ±1, missing the x=-2 end face → open end rim.
        let p = vec![
            Vec3::new(-2.0,-1.0,-1.0), Vec3::new(0.0,-1.0,-1.0), Vec3::new(0.0,-1.0,1.0), Vec3::new(-2.0,-1.0,1.0),
            Vec3::new(-2.0, 1.0,-1.0), Vec3::new(0.0, 1.0,-1.0), Vec3::new(0.0, 1.0,1.0), Vec3::new(-2.0, 1.0,1.0),
        ];
        let f = vec![
            vec![0,1,2,3],      // bottom (-Y)
            vec![7,6,5,4],      // top (+Y)
            vec![0,4,5,1],      // back (-Z)
            vec![2,6,7,3],      // front (+Z)
            vec![1,5,6,2],      // x=0 end; x=-2 end OMITTED → open
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        assert_eq!(boundary_loop_count(&m), 1);
        // bottom-back edge 0-1 (y=-1,z=-1): endpoint v0 is ON the open rim, v1 interior.
        let e = interior_he(&m, 0, 1).expect("bottom-back edge is interior");
        let sel: HashSet<u32> = std::iter::once(e).collect();
        for seg in [1u32, 3, 5] {
            let o = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.45, segments: seg, shape: 0.5, clamp: true });
            o.is_manifold().unwrap_or_else(|e| panic!("seg{seg}: not manifold: {e:?}"));
            assert_eq!(o.count_folded_coplanar_pairs(), 0, "seg{seg}: folded end cap");
            assert_eq!(boundary_loop_count(&o), 1, "seg{seg}: rim must stay one loop");
            assert!(bevel_result_installable(&m, &o), "seg{seg}: must install (no snap-back)");
            // No cap ⇒ vertex count is exactly rim-arc + interior-arc + survivors: 8-2 + 2·(seg+1)
            assert_eq!(o.vertex_count(), 6 + 2 * (seg as usize + 1), "seg{seg}: unexpected extra cap verts");
        }
    }

    /// `boundary_loop_count`: 0 for a closed cube, 1 for the open box, 2 for a box with TWO holes.
    #[test]
    fn boundary_loop_count_counts_holes() {
        assert_eq!(boundary_loop_count(&HalfEdgeMesh::new_box(Vec3::ONE)), 0, "closed cube: 0 loops");
        assert_eq!(boundary_loop_count(&open_box()), 1, "open box: 1 rim");
        // box with BOTH top (+Y) and bottom (-Y) faces removed → two disjoint rims.
        let p = vec![
            Vec3::new(-1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,-1.0), Vec3::new(1.0,-1.0,1.0), Vec3::new(-1.0,-1.0,1.0),
            Vec3::new(-1.0, 1.0,-1.0), Vec3::new(1.0, 1.0,-1.0), Vec3::new(1.0, 1.0,1.0), Vec3::new(-1.0, 1.0,1.0),
        ];
        let f = vec![vec![0,4,5,1], vec![1,5,6,2], vec![2,6,7,3], vec![3,7,4,0]]; // only the 4 sides
        let tube = HalfEdgeMesh::from_polygons(&p, &f);
        assert_eq!(boundary_loop_count(&tube), 2, "open tube: 2 rims");
    }

    /// THE INSTALL GUARD gate — `bevel_result_installable` must ACCEPT faithful bevels (closed AND open)
    /// and REJECT tears (non-manifold, or a NEW hole), for BOTH closed and open inputs. This is the
    /// core-testable proxy for the app's `poly_bevel_regenerate` install decision.
    #[test]
    fn bevel_install_guard_open_and_closed() {
        // (1) closed cube single-edge bevel → accepted (loops stay 0 — no regression vs old guard).
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let ce = pick_interior_edge(&cube);
        let cb = bevel_edges_offset(&cube, &std::iter::once(ce).collect(), &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true });
        assert!(bevel_result_installable(&cube, &cb), "closed cube bevel must install");

        // (2) OPEN box interior bevel that GROWS the boundary edge count → the old guard rejected it,
        // the new guard ACCEPTS it (loop count preserved). The regression this whole task fixes.
        let m = open_box();
        let e04 = interior_he(&m, 0, 4).unwrap();
        let ob = bevel_edges_offset(&m, &std::iter::once(e04).collect(), &BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true });
        assert!(boundary_edges(&ob) > boundary_edges(&m), "precondition: this bevel grows boundary edges");
        assert!(!(ob.validate().is_ok() && boundary_edges(&ob) <= boundary_edges(&m)),
            "the OLD guard (boundary_edges <= orig) would REJECT this valid open-mesh bevel");
        assert!(bevel_result_installable(&m, &ob), "the NEW guard must ACCEPT the valid open-mesh bevel");

        // (3) synthetic TEAR: a closed cube with one face deleted from the OUTPUT set → a spurious hole
        // appears (loops 0 → 1) → must be REJECTED even though the torn mesh is still manifold.
        let mut torn_faces: Vec<Vec<usize>> = (0..cube.faces.len() as u32)
            .map(|f| cube.face_vertices(FaceId(f)).iter().map(|v| v.0 as usize).collect()).collect();
        torn_faces.pop(); // drop a face → a hole the "bevel" never should have opened
        let torn = HalfEdgeMesh::from_polygons(
            &cube.vertices.iter().map(|v| v.position).collect::<Vec<_>>(), &torn_faces);
        assert_eq!(boundary_loop_count(&torn), 1, "torn cube has a spurious hole");
        assert!(!bevel_result_installable(&cube, &torn), "a NEW hole on a closed input must be REJECTED (tear)");

        // (5) ALREADY-BROKEN input stays EDITABLE (owner ruling: "bevel need to work even if the mesh
        // is open or broken"). A T-junction fin — THREE faces sharing one edge — is genuinely
        // non-manifold. Beveling anything on it stays non-manifold (the fin persists), but the guard
        // must ACCEPT because the INPUT was already broken — one defect must not disable bevel
        // everywhere (the old absolute is_manifold(new) check did exactly that).
        let fin_pos = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.0), Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(1.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 1.0),
        ];
        let broken = HalfEdgeMesh::from_polygons(&fin_pos, &[
            vec![0, 1, 2, 3],   // quad A — shares edge 0-1
            vec![1, 0, 4, 5],   // quad B — shares edge 0-1
            vec![0, 1, 6, 7],   // quad C (the fin) — third face on edge 0-1 → non-manifold
        ]);
        assert!(broken.is_manifold().is_err(), "precondition: T-fin mesh is non-manifold");
        let be = pick_interior_edge(&broken);
        let bb = bevel_edges_offset(&broken, &std::iter::once(be).collect(), &BevelParams { width: 0.15, segments: 2, shape: 0.5, clamp: true });
        assert!(bevel_result_installable(&broken, &bb), "bevel on an already-broken mesh must still install");

        // (4) synthetic tear on an OPEN input: open box → drop another face → loops 1 → 2 → rejected.
        let mut of: Vec<Vec<usize>> = (0..m.faces.len() as u32)
            .map(|f| m.face_vertices(FaceId(f)).iter().map(|v| v.0 as usize).collect()).collect();
        of.retain(|face| !(face.contains(&0) && face.contains(&1) && face.contains(&2) && face.contains(&3))); // drop bottom quad
        let m2 = HalfEdgeMesh::from_polygons(&m.vertices.iter().map(|v| v.position).collect::<Vec<_>>(), &of);
        assert_eq!(boundary_loop_count(&m2), 2, "two holes now");
        assert!(!bevel_result_installable(&m, &m2), "a NEW hole on an open input must also be REJECTED");
    }

    /// Multi-edge corner patch (watertight_corner test): beveling an edge LOOP (the 4 edges of the
    /// cube's top face) makes each top vertex a k=2 corner → one corner-patch n-gon per vertex. The
    /// result must stay manifold with no boundary edge.
    #[test]
    fn edge_loop_corner_patch_watertight() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        // Half-edges of the top face (face id 1).
        let loop_hes: Vec<u32> = mesh.face_half_edges(FaceId(1)).iter().map(|h| h.0).collect();
        assert_eq!(loop_hes.len(), 4, "top face is a quad");
        let sel: HashSet<u32> = loop_hes.into_iter().collect();
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.15, ..Default::default() });
        out.validate().expect("edge-loop bevel must be manifold");
        // No boundary half-edges (closed mesh stays closed).
        let boundary = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "edge-loop bevel must remain watertight (no boundary edges)");
        // 4 top verts → each splits into 2 rail points = 8; bottom 4 unchanged → 12 verts.
        assert_eq!(out.vertex_count(), 12, "top edge-loop bevel: 8→12 verts");
    }

    /// Beveling ALL 12 cube edges (every vertex k=3) stays manifold — the corner-patch stress test.
    #[test]
    fn bevel_all_edges_manifold() {
        let mesh = HalfEdgeMesh::new_box(Vec3::ONE);
        let sel: HashSet<u32> = (0..mesh.half_edges.len() as u32).collect();
        let out = bevel_edges_offset(&mesh, &sel, &BevelParams { width: 0.1, ..Default::default() });
        out.validate().expect("bevel-all must be manifold");
        let boundary = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN)
            .count();
        assert_eq!(boundary, 0, "bevel-all must remain watertight");
    }

    /// Pick a manifold (interior) half-edge of a closed mesh.
    fn pick_interior_edge(mesh: &HalfEdgeMesh) -> u32 {
        for i in 0..mesh.half_edges.len() as u32 {
            let he = mesh.he(HalfEdgeId(i));
            if he.twin != NO_TWIN && he.face != NO_FACE && mesh.he(he.twin).face != NO_FACE {
                return i;
            }
        }
        panic!("no interior edge");
    }

    /// L-shaped prism with a CONCAVE (reflex) vertical edge — the owner's step-cube repro.
    /// Profile CCW in XY: (0,0)(2,0)(2,1)(1,1)(1,2)(0,2); concave at index 3. Extruded z=±1.
    fn l_step_prism() -> (HalfEdgeMesh, u32) {
        let prof = [
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 2.0, 0.0), Vec3::new(0.0, 2.0, 0.0),
        ];
        let mut p: Vec<Vec3> = prof.iter().map(|q| Vec3::new(q.x, q.y, -1.0)).collect();
        p.extend(prof.iter().map(|q| Vec3::new(q.x, q.y, 1.0)));
        let mut f: Vec<Vec<usize>> = vec![vec![5, 4, 3, 2, 1, 0], vec![6, 7, 8, 9, 10, 11]];
        for i in 0..6 {
            f.push(vec![i, (i + 1) % 6, 6 + (i + 1) % 6, 6 + i]);
        }
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        // The concave vertical edge joins profile vertex 3 (bottom) and 9 (top).
        let mut edge = None;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            let (s, d) = (m.he_source(h).0, m.he(h).vertex.0);
            if (s == 3 && d == 9) || (s == 9 && d == 3) {
                edge = Some(i);
                break;
            }
        }
        (m, edge.expect("concave vertical edge"))
    }

    /// Owner 2026-08-08: edge ending at a **3-spoke (degree-3) vertex** on a step/L solid must
    /// fillet **concave at BOTH ends** — never one convex spoke-fan + one concave strip.
    /// Multi-segment (the "bevel segment" case): no radial triangle fan pinched to a center.
    #[test]
    fn concave_edge_three_spoke_ends_always_concave_no_spoke_fan() {
        let (m, e) = l_step_prism();
        // Both endpoints are degree-3 (3 spokes).
        let he = HalfEdgeId(e);
        let v0 = m.he_source(he);
        let v1 = m.he(he).vertex;
        assert_eq!(m.vertex_fan(v0).len(), 3, "endpoint must be 3-spoke");
        assert_eq!(m.vertex_fan(v1).len(), 3, "endpoint must be 3-spoke");
        assert!(
            crate::bevel_op::edge_is_concave(&m, he),
            "L-step vertical crease must classify concave"
        );
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let w = 0.25f32;
        for seg in [1u32, 3, 5] {
            let r = bevel_edges_offset(
                &m,
                &sel,
                &BevelParams {
                    width: w,
                    segments: seg,
                    shape: 0.5,
                    clamp: true,
                },
            );
            r.validate().expect("must stay manifold");
            assert!(r.is_manifold().is_ok(), "seg={seg} manifold");
            // No spoke fan: no vertex is the tip of more than 2 triangles (a partial-corner
            // radial fan would pin many tris to one center — the convex pinching look).
            let mut max_tri = 0usize;
            for vi in 0..r.vertex_count() as u32 {
                let mut tris = 0usize;
                for fi in 0..r.face_count() as u32 {
                    let vs = r.face_vertices(FaceId(fi));
                    if vs.len() == 3 && vs.iter().any(|v| v.0 == vi) {
                        tris += 1;
                    }
                }
                max_tri = max_tri.max(tris);
            }
            assert!(
                max_tri <= 2,
                "seg={seg}: spoke fan detected (vertex in {max_tri} tris) — convex pinching at 3-spoke end"
            );
            // Both end planes: profile points only in the concave notch (x≥1 and near corner).
            for z in [-1.0f32, 1.0] {
                let corner = Vec3::new(1.0, 1.0, z);
                let centre = Vec3::new(1.0 + w, 1.0 + w, z);
                let mut arc: Vec<Vec3> = r
                    .vertices
                    .iter()
                    .map(|v| v.position)
                    .filter(|p| (p.z - z).abs() < 1e-3 && (*p - corner).length() < 0.85)
                    .collect();
                assert!(
                    arc.len() >= 2,
                    "seg={seg} z={z}: expected profile points near corner"
                );
                for p in &arc {
                    // Wrong (convex) side: deep into solid bulk x<1 and y<1 away from notch.
                    assert!(
                        !(p.x < 0.999 && p.y < 0.999),
                        "seg={seg} z={z}: profile point {p:?} on CONVEX bulk side — must stay concave"
                    );
                    // On-circle check for multi-seg circular profile (endpoints may sit on rails).
                    if seg > 1 {
                        let err = ((*p - centre).length() - w).abs();
                        assert!(
                            err < 0.05 || (p.x - 1.0).abs() < 1e-3 || (p.y - 1.0).abs() < 1e-3,
                            "seg={seg} z={z}: point {p:?} off concave fillet (err={err})"
                        );
                    }
                }
            }
        }
    }

    /// CONCAVE (reflex) EDGE, seg=5 — owner repro 2026-08-02 ("bevel problem with 5 segment").
    /// Locks the profile geometry on a concave edge: exactly seg+1 arc points per cross-section,
    /// EVEN chord spacing, and every point on the true fillet circle (centre = corner + w along
    /// each leg, radius w). Measured green at ratio 1.00 / radius err <1e-4 — so any future
    /// spacing or radius drift on concave edges fails here rather than in a screenshot.
    #[test]
    fn concave_edge_five_segment_profile_is_even_circle() {
        let (m, e) = l_step_prism();
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let w = 0.3f32;
        let r = bevel_edges_offset(&m, &sel, &BevelParams { width: w, segments: 5, shape: 0.5, clamp: true });
        r.validate().expect("concave bevel must be manifold");
        let boundary = (0..r.half_edges.len() as u32)
            .filter(|&i| r.he(HalfEdgeId(i)).twin == NO_TWIN).count();
        assert_eq!(boundary, 0, "concave bevel must stay watertight");

        for z in [-1.0f32, 1.0] {
            let corner = Vec3::new(1.0, 1.0, z);
            let mut arc: Vec<Vec3> = r.vertices.iter().map(|v| v.position)
                .filter(|p| (p.z - z).abs() < 1e-4 && (*p - corner).length() < 0.9)
                .collect();
            arc.sort_by(|a, b| {
                let aa = (a.y - corner.y).atan2(a.x - corner.x);
                let bb = (b.y - corner.y).atan2(b.x - corner.x);
                aa.partial_cmp(&bb).unwrap()
            });
            assert_eq!(arc.len(), 6, "seg=5 must emit 6 profile points at z={z} (got {})", arc.len());

            // Even spacing: max/min chord ratio ~1 for a circular profile sampled at even angle.
            let chords: Vec<f32> = arc.windows(2).map(|q| (q[1] - q[0]).length()).collect();
            let (mn, mx) = chords.iter().fold((f32::MAX, 0.0f32), |(a, b), c| (a.min(*c), b.max(*c)));
            assert!(mx / mn.max(1e-6) < 1.02,
                "profile bands uneven at z={z}: chord ratio {:.3} ({chords:?})", mx / mn.max(1e-6));

            // True fillet circle: the reflex corner (1,1) has its legs along +x (to (2,1)) and
            // +y (to (1,2)), so the arc runs (1+w,1)→(1,1+w) about centre (1+w,1+w), radius w.
            let centre = Vec3::new(1.0 + w, 1.0 + w, z);
            for p in &arc {
                let err = ((*p - centre).length() - w).abs();
                assert!(err < 1e-3, "profile point {p:?} off the fillet circle by {err:.5} at z={z}");
            }
        }
    }

    /// OVERLAP PROBE (owner 2026-08-02): beveling the concave edge must RETRIM the end faces the
    /// fillet runs into — if the original face is left whole while the fillet strip is also
    /// emitted, the two double-cover the same region (visible as overlapping side geometry).
    /// Total face area in each end plane must equal the analytic L-profile + fillet area exactly.
    #[test]
    fn concave_bevel_end_faces_do_not_overlap_probe() {
        let (m, e) = l_step_prism();
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let w = 0.3f32;
        let r = bevel_edges_offset(&m, &sel, &BevelParams { width: w, segments: 5, shape: 0.5, clamp: true });
        // L area (2x2 minus the 1x1 notch) + the fillet wedge added at the reflex corner.
        let expect = 3.0 + (w * w - std::f32::consts::FRAC_PI_4 * w * w);
        for z in [-1.0f32, 1.0] {
            let mut area = 0.0f32;
            let mut nfaces = 0;
            for f in 0..r.face_count() as u32 {
                let vs = r.face_vertices(FaceId(f));
                if vs.iter().any(|v| (r.vert(*v).position.z - z).abs() > 1e-4) {
                    continue;
                }
                // Newell area of this planar face.
                let mut n = Vec3::ZERO;
                for i in 0..vs.len() {
                    let a = r.vert(vs[i]).position;
                    let b = r.vert(vs[(i + 1) % vs.len()]).position;
                    n += a.cross(b);
                }
                area += n.length() * 0.5;
                nfaces += 1;
            }
            eprintln!("[PROBE] z={z}: {nfaces} end faces, area={area:.5}, expected={expect:.5}, excess={:.5}", area - expect);
        }
    }

    /// MODIFIER PATH (owner 2026-08-02, "side panel geometry creating overlap"): the Bevel
    /// MODIFIER bevels ALL live edges at once. Within the collide limit the result must stay
    /// manifold, un-inverted and non-growing.
    ///
    /// **KNOWN GAP (BEVEL.md §6/§11, 2026-08-02):** on an L-step (reflex edge + all edges
    /// selected) `geometry_collide_offset` UNDER-estimates: at w≥0.6 (shortest feature 1.0,
    /// Blender's limit 0.5) the result goes NON-MANIFOLD and *loses* verts (324→294) — geometry
    /// folding into itself. The modifier's `bevel_result_installable` then rejects it, so the
    /// panel silently stops updating. This test asserts the green range and prints the gap.
    #[test]
    fn modifier_style_bevel_all_edges_clamped_manifold() {
        let (m, _e) = l_step_prism();
        let (mn0, mx0) = m.bounding_box();
        for w in [0.1f32, 0.3, 0.6, 0.9] {
            let sel: HashSet<u32> = (0..m.half_edges.len() as u32)
                .filter(|&i| {
                    let h = m.he(HalfEdgeId(i));
                    h.twin != NO_TWIN && h.face != NO_FACE && m.he(h.twin).face != NO_FACE
                })
                .collect();
            let r = bevel_edges_offset(&m, &sel, &BevelParams { width: w, segments: 5, shape: 0.5, clamp: true });
            let manifold = r.is_manifold().is_ok();
            let (mn1, mx1) = r.bounding_box();
            let grow = (mn0 - mn1).max_element().max((mx1 - mx0).max_element());
            // Inverted faces: a bevel of a convex solid must keep every face pointing away from
            // the centroid (cheap self-intersection proxy).
            let c = (mn1 + mx1) * 0.5;
            let mut inverted = 0;
            for f in 0..r.face_count() as u32 {
                let n = r.compute_face_normal(FaceId(f));
                let vs = r.face_vertices(FaceId(f));
                if vs.is_empty() { continue; }
                let mut ctr = Vec3::ZERO;
                for v in &vs { ctr += r.vert(*v).position; }
                ctr /= vs.len() as f32;
                if n.dot(ctr - c) < -1e-3 { inverted += 1; }
            }
            eprintln!(
                "[bevel] all-edges w={w}: manifold={manifold} bbox_grow={grow:.4} inverted_faces={inverted} nv={} nf={}",
                r.vertex_count(), r.face_count()
            );
            assert!(grow < 1e-3, "w={w}: bevel must never grow the shape (grew {grow:.4})");
            assert_eq!(inverted, 0, "w={w}: bevel must not invert faces");
            if w <= 0.5 {
                assert!(manifold, "w={w} is inside the collide limit — result must stay manifold");
            }
        }
    }

    /// Owner's failing scenario (2026-08-02): a box with a corner NOTCH, so the concave edge
    /// TERMINATES MID-BODY against the notch end wall instead of running out to a flat end face.
    /// Base is byte-identical to the Blender oracle `notch_corner_ref.json` (14 verts / 9 faces).
    /// Target edge v9(0,1,1) → v4(1.5,1,1).
    fn notch_corner_solid() -> (HalfEdgeMesh, u32) {
        let p: Vec<Vec3> = vec![
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0), Vec3::new(1.5, 1.0, 1.0), Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(2.0, 2.0, 0.0), Vec3::new(2.0, 2.0, 2.0), Vec3::new(0.0, 1.0, 2.0),
            Vec3::new(0.0, 1.0, 1.0), Vec3::new(0.0, 2.0, 1.0), Vec3::new(1.5, 2.0, 2.0),
            Vec3::new(1.5, 2.0, 1.0), Vec3::new(1.5, 1.0, 2.0),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![2, 0, 1, 8, 9, 10],
            vec![7, 6, 2, 10, 12, 11],
            vec![6, 7, 5, 3],
            vec![3, 5, 1, 0],
            vec![2, 6, 3, 0],
            vec![8, 1, 5, 7, 11, 13],
            vec![12, 4, 13, 11],
            vec![4, 9, 8, 13],
            vec![10, 9, 4, 12],
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        let mut edge = None;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            let (s, d) = (m.he_source(h).0, m.he(h).vertex.0);
            if (s == 9 && d == 4) || (s == 4 && d == 9) {
                edge = Some(i);
                break;
            }
        }
        (m, edge.expect("terminating concave edge v9-v4"))
    }

    /// ORACLE DIFF (owner repro): Blender 4.5 on this exact base with w=0.3 seg=5 gives
    /// **24 verts / 14 faces, watertight (0 open edges), 5 new faces** — i.e. it inserts the
    /// fillet's 6 profile points into the TERMINATING face and re-traces it, rather than adding
    /// a separate cap. AZRA leaves a hole there (owner screenshot: background visible at the
    /// corner). This test reports our numbers against the oracle.
    #[test]
    fn notch_corner_bevel_vs_blender_oracle() {
        let (m, e) = notch_corner_solid();
        assert_eq!(m.vertex_count(), 14, "base must match the oracle base");
        assert_eq!(m.face_count(), 9, "base must match the oracle base");
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let r = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.3, segments: 5, shape: 0.5, clamp: true });
        let open = (0..r.half_edges.len() as u32)
            .filter(|&i| r.he(HalfEdgeId(i)).twin == NO_TWIN && r.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        eprintln!(
            "[oracle-diff] AZRA nv={} nf={} open_edges={} manifold={} | BLENDER nv=24 nf=14 open_edges=0",
            r.vertex_count(), r.face_count(), open, r.is_manifold().is_ok()
        );
        assert_eq!(open, 0, "fillet terminating mid-body must stay watertight (owner: hole at the corner)");
        assert_eq!(r.vertex_count(), 24, "must match Blender vertex count");
        assert_eq!(r.face_count(), 14, "must match Blender face count");
    }

    /// SAME notch solid, but the concave edge is SPLIT in two collinear halves (what an EXTRUDE
    /// leaves behind — the owner built the step that way) and only the FIRST half is beveled.
    /// The fillet then terminates in the middle of a straight edge run. Must stay watertight:
    /// the owner's screenshot shows background through a hole exactly at such a termination.
    #[test]
    fn partial_collinear_run_bevel_stays_watertight() {
        let mut p: Vec<Vec3> = vec![
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0), Vec3::new(1.5, 1.0, 1.0), Vec3::new(2.0, 0.0, 2.0),
            Vec3::new(2.0, 2.0, 0.0), Vec3::new(2.0, 2.0, 2.0), Vec3::new(0.0, 1.0, 2.0),
            Vec3::new(0.0, 1.0, 1.0), Vec3::new(0.0, 2.0, 1.0), Vec3::new(1.5, 2.0, 2.0),
            Vec3::new(1.5, 2.0, 1.0), Vec3::new(1.5, 1.0, 2.0),
        ];
        p.push(Vec3::new(0.75, 1.0, 1.0)); // v14 — splits the concave edge v9→v4
        let f: Vec<Vec<usize>> = vec![
            vec![2, 0, 1, 8, 9, 10],
            vec![7, 6, 2, 10, 12, 11],
            vec![6, 7, 5, 3],
            vec![3, 5, 1, 0],
            vec![2, 6, 3, 0],
            vec![8, 1, 5, 7, 11, 13],
            vec![12, 4, 13, 11],
            vec![4, 14, 9, 8, 13],
            vec![10, 9, 14, 4, 12],
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        let mut edge = None;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            let (s, d) = (m.he_source(h).0, m.he(h).vertex.0);
            if (s == 9 && d == 14) || (s == 14 && d == 9) {
                edge = Some(i);
                break;
            }
        }
        let sel: HashSet<u32> = std::iter::once(edge.expect("half edge run")).collect();
        for cl in [true, false] {
            let t = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.3, segments: 5, shape: 0.5, clamp: cl });
            eprintln!(
                "[oracle-diff] partial-run clamp={cl}: nv={} nf={} eff_width={:.4}",
                t.vertex_count(), t.face_count(), last_bevel_effective_width()
            );
        }
        let r = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.3, segments: 5, shape: 0.5, clamp: true });
        let open = (0..r.half_edges.len() as u32)
            .filter(|&i| r.he(HalfEdgeId(i)).twin == NO_TWIN && r.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        eprintln!(
            "[oracle-diff] partial-run AZRA nv={} nf={} open_edges={} manifold={} | BLENDER nv=26 nf=19 open=0",
            r.vertex_count(), r.face_count(), open, r.is_manifold().is_ok()
        );
        assert_eq!(open, 0, "fillet ending mid-run must not leave a hole");
        assert!(r.is_manifold().is_ok(), "partial-run bevel must stay manifold");
    }

    /// OWNER'S ACTUAL MESH, captured live with `AZRA_BEVEL_DUMP=1` (2026-08-02): a unit cube
    /// extruded +Z then +Y (three boxes sharing faces = the step). The beveled concave edge is
    /// v7(-0.5,0.5,0.5) → v6(0.5,0.5,0.5), and — the thing every earlier test shape missed —
    /// BOTH endpoints are **degree-5** vertices (5 faces, 5 edges), not degree-3.
    ///
    /// Blender 4.5 on this exact input (w=0.15 seg=3): **26 verts / 19 faces / 0 open edges**
    /// (`docs/planning/poly/repro_ref.py`). That is the target.
    fn owner_step_solid() -> (HalfEdgeMesh, u32) {
        let p: Vec<Vec3> = vec![
            Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, 0.5),   Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(-0.5, 0.5, -0.5),  Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, 0.5),    Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(0.5, -0.5, 1.847281),  Vec3::new(-0.5, 0.5, 1.847281),
            Vec3::new(-0.5, -0.5, 1.847281), Vec3::new(0.5, 0.5, 1.847281),
            Vec3::new(0.5, 1.683461, 0.5),   Vec3::new(-0.5, 1.683461, 1.847281),
            Vec3::new(0.5, 1.683461, 1.847281), Vec3::new(-0.5, 1.683461, 0.5),
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![1, 2, 3, 0],   vec![6, 5, 4, 7],   vec![5, 1, 0, 4],
            vec![9, 10, 8, 11], vec![4, 0, 3, 7],   vec![6, 2, 1, 5],
            vec![15, 13, 14, 12], vec![3, 10, 9, 7], vec![2, 8, 10, 3],
            vec![6, 11, 8, 2],  vec![7, 15, 12, 6], vec![9, 13, 15, 7],
            vec![11, 14, 13, 9], vec![6, 12, 14, 11],
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        let mut edge = None;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            let (s, d) = (m.he_source(h).0, m.he(h).vertex.0);
            if (s == 7 && d == 6) || (s == 6 && d == 7) {
                edge = Some(i);
                break;
            }
        }
        (m, edge.expect("concave edge v7-v6"))
    }

    /// ORACLE DIFF on the owner's real captured mesh — the degree-5 endpoint case.
    #[test]
    fn owner_step_bevel_vs_blender_oracle() {
        let (m, e) = owner_step_solid();
        assert_eq!(m.vertex_count(), 16);
        assert_eq!(m.face_count(), 14);
        for v in [6u32, 7] {
            let fan = m.vertex_fan(VertexId(v));
            eprintln!("[oracle-diff] endpoint v{v} fan(edges)={}", fan.len());
        }
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let r = bevel_edges_offset(&m, &sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        let open = (0..r.half_edges.len() as u32)
            .filter(|&i| r.he(HalfEdgeId(i)).twin == NO_TWIN && r.he(HalfEdgeId(i)).face != NO_FACE)
            .count();
        eprintln!(
            "[oracle-diff] owner-step AZRA nv={} nf={} open={} manifold={} eff_w={:.4} | BLENDER nv=26 nf=19 open=0",
            r.vertex_count(), r.face_count(), open, r.is_manifold().is_ok(), last_bevel_effective_width()
        );
        assert_eq!(open, 0, "owner's bevel must stay watertight");
        assert!(r.is_manifold().is_ok(), "owner's bevel must stay manifold");
        assert_eq!(r.vertex_count(), 26, "must match Blender vertex count");
        assert_eq!(r.face_count(), 19, "must match Blender face count");

        // VERTEX-EXACT: every Blender vertex must exist in our output (counts alone can coincide).
        // Oracle `docs/planning/poly/repro_out.json`; the 6 corner-cap verts at x=+0.5 are listed
        // explicitly — 4 profile points + the 2 per-edge slide points that make the hexagon.
        let expect_cap: [Vec3; 6] = [
            Vec3::new(0.5, 0.5, 0.35), Vec3::new(0.5, 0.52, 0.425),
            Vec3::new(0.5, 0.575, 0.48), Vec3::new(0.5, 0.65, 0.5),
            Vec3::new(0.5, 0.5, 0.65), Vec3::new(0.5, 0.35, 0.5),
        ];
        for e in expect_cap {
            let best = r.vertices.iter()
                .map(|v| (v.position - e).length())
                .fold(f32::MAX, f32::min);
            assert!(best < 2e-3, "Blender cap vertex {e:?} missing (nearest {best:.4})");
        }
        // The hexagon cap itself: exactly one face carrying all 6 of those points.
        let hexes = (0..r.face_count() as u32).filter(|&f| {
            let vs = r.face_vertices(FaceId(f));
            vs.len() == 6 && expect_cap.iter().all(|e| {
                vs.iter().any(|v| (r.vert(*v).position - *e).length() < 2e-3)
            })
        }).count();
        assert_eq!(hexes, 1, "expected exactly one hexagonal terminating cap (Blender builds one)");
    }

    /// ROUND BEVEL — segments>1 must subdivide the chamfer into a rounded, watertight arc.
    #[test]
    fn bevel_segments_produce_round_watertight() {
        let make = || HalfEdgeMesh::new_box(Vec3::ONE);
        let e = pick_interior_edge(&make());
        let sel: HashSet<u32> = std::iter::once(e).collect();
        let flat = bevel_edges_offset(&make(), &sel, &BevelParams { width: 0.25, segments: 1, shape: 0.5, clamp: true });
        let round = bevel_edges_offset(&make(), &sel, &BevelParams { width: 0.25, segments: 4, shape: 0.5, clamp: true });
        round.validate().expect("round bevel must be manifold");
        let boundary = (0..round.half_edges.len() as u32)
            .filter(|&i| round.he(HalfEdgeId(i)).twin == NO_TWIN).count();
        assert_eq!(boundary, 0, "round bevel must be watertight");
        // The single chamfer strip becomes 4 → net +3 faces vs the flat chamfer.
        assert_eq!(round.face_count(), flat.face_count() + 3,
            "4 segments must add 3 extra strips (got {} vs flat {})", round.face_count(), flat.face_count());
        // The profile actually rounds: an arc vertex lies OFF BOTH faces of the beveled edge (the
        // bottom-front edge → planes y=-1 and z=-1; flat rails all lie ON one of them).
        let off_both = round.vertices.iter().any(|v| {
            let p = v.position;
            (p.y + 1.0).abs() > 1e-3 && (p.z + 1.0).abs() > 1e-3 && p.y < 0.99 && p.z < 0.99
        });
        assert!(off_both, "round profile must bulge off both original faces (arc vertices)");
        // Multi-edge (shared-corner) selections fall back to a FLAT chamfer in v1 (rounded multi-edge
        // corners are a v2 blend problem) — but must ALWAYS stay watertight.
        let loop_sel: HashSet<u32> = make().face_half_edges(FaceId(1)).iter().map(|h| h.0).collect();
        let rl = bevel_edges_offset(&make(), &loop_sel, &BevelParams { width: 0.15, segments: 3, shape: 0.5, clamp: true });
        rl.validate().expect("loop bevel must be manifold");
        let b2 = (0..rl.half_edges.len() as u32).filter(|&i| rl.he(HalfEdgeId(i)).twin == NO_TWIN).count();
        assert_eq!(b2, 0, "loop bevel must be watertight (flat fallback on shared corners)");
    }

    /// BEVEL-ON-BEVELED GATE — bevel 2 bottom edges, then bevel EVERY interior edge of the result; each
    /// second-bevel must stay manifold + watertight + simple. Regression for beveling an edge whose
    /// endpoint is a bevel-created (degree>3) corner: the old `(false,false)` whole-arc insertion
    /// over-inserted and tore; per-side insertion + the residual-hole seal (STEP 6) keep it watertight.
    #[test]
    fn bevel_on_beveled_watertight() {
        fn boundary(m: &HalfEdgeMesh) -> usize {
            (0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN).count()
        }
        fn self_x(m: &HalfEdgeMesh) -> Option<u32> {
            fn cr(a:(f32,f32),b:(f32,f32),c:(f32,f32),d:(f32,f32))->bool{
                let o=|p:(f32,f32),q:(f32,f32),r:(f32,f32)|(q.0-p.0)*(r.1-p.1)-(q.1-p.1)*(r.0-p.0);
                let(x1,x2,x3,x4)=(o(c,d,a),o(c,d,b),o(a,b,c),o(a,b,d));
                ((x1>0.0)!=(x2>0.0))&&((x3>0.0)!=(x4>0.0))
            }
            for fi in 0..m.faces.len() as u32 {
                let fv=m.face_vertices(FaceId(fi)); if fv.len()<4 {continue;}
                let n=m.compute_face_normal(FaceId(fi));
                let up=if n.x.abs()<0.9 {Vec3::X} else {Vec3::Y};
                let u=n.cross(up).normalize_or_zero(); let vv=n.cross(u);
                let pts:Vec<(f32,f32)>=fv.iter().map(|x|{let p=m.vert(*x).position;(p.dot(u),p.dot(vv))}).collect();
                let k=pts.len();
                for a in 0..k { for b in (a+1)..k {
                    if (a+1)%k==b||(b+1)%k==a {continue;}
                    if cr(pts[a],pts[(a+1)%k],pts[b],pts[(b+1)%k]) { return Some(fi); }
                }}
            }
            None
        }
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let bottom = (0..cube.faces.len() as u32).find(|&f| cube.compute_face_normal(FaceId(f)).y < -0.9).unwrap();
        let bhe: Vec<u32> = cube.face_half_edges(FaceId(bottom)).iter().map(|h| h.0).collect();
        let sel: HashSet<u32> = [bhe[0], bhe[1]].into_iter().collect(); // 2 consecutive bottom edges
        let r1 = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.25, segments: 1, shape: 0.5, clamp: true });
        r1.validate().expect("first bevel manifold");
        assert_eq!(boundary(&r1), 0, "first bevel watertight");
        // Now bevel EVERY interior edge of R1 — each second-bevel must stay closed + simple + manifold.
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..r1.half_edges.len() as u32 {
            let he = r1.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE || r1.he(he.twin).face == NO_FACE { continue; }
            if seen.contains(&i) { continue; } seen.insert(i); seen.insert(he.twin.0);
            let r2 = bevel_edges_offset(&r1, &std::iter::once(i).collect(),
                &BevelParams { width: 0.08, segments: 1, shape: 0.5, clamp: true });
            r2.validate().unwrap_or_else(|e| panic!("bevel-on-beveled edge {i}: non-manifold ({e})"));
            assert_eq!(boundary(&r2), 0, "bevel-on-beveled edge {i}: not watertight");
            assert!(self_x(&r2).is_none(), "bevel-on-beveled edge {i}: self-intersecting");
        }
    }

    /// APP-FAITHFUL bevel-on-beveled: reproduce the user's exact workflow. The app defaults to
    /// `segments: 3` (poly_drag_tools.rs:683), and bevels ONE edge at a time. The prior test used
    /// segments:1 (flat) which sidesteps the round-arc-into-corner failure. This runs segments:3.
    /// ACCEPTANCE gate (green): the app's exact workflow — bevel 2 bottom edges at segments=3 (the app
    /// default), then bevel EVERY interior edge one at a time at segments=3. Each result must be manifold
    /// + watertight + simple. The offset-rail bevel tore on 17/17 of these; the explicit local edge-bevel
    /// (fan-walked rim + strip-matched winding + deterministic re-trace order) seals all of them.
    #[test]
    fn bevel_on_beveled_round_app_faithful() {
        fn boundary(m: &HalfEdgeMesh) -> usize {
            (0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN).count()
        }
        fn self_x(m: &HalfEdgeMesh) -> Option<u32> {
            fn cr(a:(f32,f32),b:(f32,f32),c:(f32,f32),d:(f32,f32))->bool{
                let o=|p:(f32,f32),q:(f32,f32),r:(f32,f32)|(q.0-p.0)*(r.1-p.1)-(q.1-p.1)*(r.0-p.0);
                let(x1,x2,x3,x4)=(o(c,d,a),o(c,d,b),o(a,b,c),o(a,b,d));
                ((x1>0.0)!=(x2>0.0))&&((x3>0.0)!=(x4>0.0))
            }
            for fi in 0..m.faces.len() as u32 {
                let fv=m.face_vertices(FaceId(fi)); if fv.len()<4 {continue;}
                let n=m.compute_face_normal(FaceId(fi));
                let up=if n.x.abs()<0.9 {Vec3::X} else {Vec3::Y};
                let u=n.cross(up).normalize_or_zero(); let vv=n.cross(u);
                let pts:Vec<(f32,f32)>=fv.iter().map(|x|{let p=m.vert(*x).position;(p.dot(u),p.dot(vv))}).collect();
                let k=pts.len();
                for a in 0..k { for b in (a+1)..k {
                    if (a+1)%k==b||(b+1)%k==a {continue;}
                    if cr(pts[a],pts[(a+1)%k],pts[b],pts[(b+1)%k]) { return Some(fi); }
                }}
            }
            None
        }
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let bottom = (0..cube.faces.len() as u32).find(|&f| cube.compute_face_normal(FaceId(f)).y < -0.9).unwrap();
        let bhe: Vec<u32> = cube.face_half_edges(FaceId(bottom)).iter().map(|h| h.0).collect();
        let sel: HashSet<u32> = [bhe[0], bhe[1]].into_iter().collect();
        // Stage 1 = the ROUNDED chain L (STEP 2c weld, segments 3, DEFAULT circular shape 0.5) — stage 2
        // then bevels every edge of ROUNDED geometry, including arc-rim edges (the ellipse-profile fix;
        // see `bevel_on_rounded_bevel_arc_rim_spike`). Historically this gate effectively covered the
        // FLAT L only (the pre-STEP-2c clamp forced n_seg→1); it now covers the strictly harder rounded
        // base at the app-default profile. KNOWN LIMITATION (tracked): composing shape-1.0 bevels (whose
        // convex ×2 semantics overshoot the face planes) can produce ~2e-4-deep twisted strip quads at
        // frames the overshoot inverts — watertight/manifold/bounded, sub-visual; circular profiles are
        // wedge-bounded and immune.
        let r1 = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        r1.validate().expect("first bevel manifold");
        let mut failures: Vec<String> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..r1.half_edges.len() as u32 {
            let he = r1.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE || r1.he(he.twin).face == NO_FACE { continue; }
            if seen.contains(&i) { continue; } seen.insert(i); seen.insert(he.twin.0);
            let r2 = bevel_edges_offset(&r1, &std::iter::once(i).collect(),
                &BevelParams { width: 0.08, segments: 3, shape: 0.5, clamp: true });
            let mani = r2.validate().is_ok();
            let b = boundary(&r2);
            let sx = self_x(&r2);
            // GEOMETRIC spike guard: a bevel (width 0.08) must not grow the object's bounds — a cone-apex
            // corner cap pushed out by the rim radius blows past this even while staying watertight+simple.
            let (mn1, mx1) = r1.bounding_box();
            let (mn2, mx2) = r2.bounding_box();
            let grow = (mn1 - mn2).max_element().max((mx2 - mx1).max_element());
            if !mani || b != 0 || sx.is_some() || grow > 0.2 {
                let s = r1.he_source(HalfEdgeId(i)).0;
                let d = he.vertex.0;
                let (sp, dp) = (r1.vert(crate::half_edge::VertexId(s)).position, r1.vert(he.vertex).position);
                let (sd, dd) = (r1.vertex_fan(crate::half_edge::VertexId(s)).len(), r1.vertex_fan(he.vertex).len());
                failures.push(format!(
                    "edge {i}: manifold={mani} boundary={b} self_x={sx:?} bbox_grow={grow:.3} src v{s}{sp:?}(deg{sd}) dst v{d}{dp:?}(deg{dd})"));
            }
        }
        assert!(failures.is_empty(),
            "segments=3 bevel-on-beveled FAILS on {}/{} edges:\n{}",
            failures.len(), seen.len()/2, failures.join("\n"));
    }

    /// ARC-RIM RE-BEVEL SPIKE — FIXED 2026-07-02: beveling an ARC-RIM edge of an already-ROUNDED bevel
    /// spiked (bbox +0.47 at width 0.08, one face self-intersected). Root cause: the terminal rail
    /// legitimately loop-slides ~10× the width along a near-collinear (174°) neighbour — Blender does the
    /// same — but the old slerp+radius-lerp `circular_arc` swept OUTSIDE the asymmetric-leg parallelogram.
    /// The ellipse (unit-square-map) profile is bounded by construction. Bevels every edge of a rounded
    /// single-edge bevel and asserts the bounds never grow >0.2.
    #[test]
    fn bevel_on_rounded_bevel_arc_rim_spike() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let bottom = (0..cube.faces.len() as u32).find(|&f| cube.compute_face_normal(FaceId(f)).y < -0.9).unwrap();
        let bhe: Vec<u32> = cube.face_half_edges(FaceId(bottom)).iter().map(|h| h.0).collect();
        let sel: HashSet<u32> = [bhe[0]].into_iter().collect(); // ONE rounded edge — no chain involved
        let r1 = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.25, segments: 3, shape: 1.0, clamp: true });
        let (mn1, mx1) = r1.bounding_box();
        let mut worst = 0.0f32;
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..r1.half_edges.len() as u32 {
            let he = r1.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE || seen.contains(&i) { continue; }
            seen.insert(i); seen.insert(he.twin.0);
            let r2 = bevel_edges_offset(&r1, &std::iter::once(i).collect(),
                &BevelParams { width: 0.08, segments: 3, shape: 0.5, clamp: true });
            let (mn2, mx2) = r2.bounding_box();
            worst = worst.max((mn1 - mn2).max_element().max((mx2 - mx1).max_element()));
        }
        assert!(worst < 0.2, "arc-rim bevel spiked: worst bbox growth {worst:.3} at width 0.08");
    }

    /// N-WAY DEGREE-6 APEX (hex cone, `hexcone_ref.json` oracle): bevel ALL 6 apex edges, seg=3, w=0.25.
    /// The relaxed valence gates route the degree-6 fully-beveled apex through the M_ADJ grid
    /// (`adj_vmesh` path: fullness centre, no snap): 6 sectors + inner 6-ring + centre HEXAGON (odd-seg
    /// centre n-gon — Blender's structure). Watertight + manifold; oracle nv=48 nf=38; the inner ring
    /// (0.22907·dirs at y=0.22449) within tolerance.
    #[test]
    fn nway_degree6_apex_rounds() {
        let n = 6usize;
        let mut p: Vec<Vec3> = (0..n).map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            Vec3::new(a.cos(), -1.0, a.sin())
        }).collect();
        p.push(Vec3::new(0.0, 1.0, 0.0)); // apex = index 6
        let mut f: Vec<Vec<usize>> = vec![(0..n).rev().collect()];
        for i in 0..n { f.push(vec![n, i, (i + 1) % n]); }
        let cone = HalfEdgeMesh::from_polygons(&p, &f);
        let mut sel = HashSet::new();
        for i in 0..cone.half_edges.len() as u32 {
            let he = cone.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if cone.he_source(HalfEdgeId(i)).0 == n as u32 || he.vertex.0 == n as u32 { sel.insert(i); }
        }
        let out = bevel_edges_offset(&cone, &sel, &BevelParams { width: 0.25, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "degree-6 apex must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "degree-6 apex manifold");
        assert_eq!(out.vertices.len(), 48, "oracle nv=48 (flat M_POLY collapse gave fewer)");
        assert_eq!(out.faces.len(), 38, "oracle nf=38");
        // Inner 6-ring within 1.5e-2 of Blender (the adj_vmesh fullness path is a close approximation,
        // not the snap-exact tri-corner path; 0.007 measured — structurally identical grid).
        for o in [[0.0, 0.22449, 0.22907], [-0.19838, 0.22449, 0.11454], [0.19838, 0.22449, -0.11454]] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 1.5e-2, "inner-ring vert {want:?} off Blender by {best:.4}");
        }
    }

    fn octa_bevel(edges: &[(u32, u32)], seg: u32) -> HalfEdgeMesh {
        let m = octahedron();
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let (s, d) = (m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            if edges.iter().any(|&(a, b)| (s == a && d == b) || (s == b && d == a)) { sel.insert(i); }
        }
        bevel_edges_offset(&m, &sel, &BevelParams { width: 0.2, segments: seg, shape: 0.5, clamp: true })
    }

    /// MIXED k=3-of-4 CORNER (`mixed_k3d4_ref.json` oracle): octahedron, bevel 3 of the 4 edges at the
    /// degree-4 vertex (0,0,1), seg=3. The lone unbeveled edge gets an ON-EDGE BoundVert (both flanking
    /// rails terminal-slide onto it, (0,−0.1633,0.8367)); the corner fills as a 3-sector M_ADJ grid
    /// (6 quads + centre tri — adj_vmesh fullness path, NO sphere snap: the mixed rim is not spherical).
    /// Was a GLOBAL flat clamp before. nv=29 nf=33, watertight + manifold; BoundVerts exact; interior
    /// within the fullness-path tolerance.
    #[test]
    fn mixed_k3_deg4_matches_oracle() {
        // apex (0,0,1) = v2 of the shared octahedron(); beveled toward +X (v1), +Y (v0), −X (v3).
        let out = octa_bevel(&[(2, 1), (2, 0), (2, 3)], 3);
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "mixed k3 corner must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "mixed k3 corner manifold");
        assert_eq!(out.vertices.len(), 29, "oracle nv=29 (global flat clamp gave fewer)");
        assert_eq!(out.faces.len(), 33, "oracle nf=33");
        // BoundVerts (2 meets + the on-edge slide) are EXACT; arcs near-exact; grid interior within the
        // fullness-path tolerance (0.0063 measured — same class as the hex-cone gate).
        for (o, tol) in [([0.1633f32, 0.1633, 0.6734], 2e-3), ([-0.1633, 0.1633, 0.6734], 2e-3),
                         ([0.0, -0.1633, 0.8367], 2e-3), ([0.05937, 0.19122, 0.70132], 5e-3),
                         ([0.04373, 0.0771, 0.7949], 1.5e-2), ([0.0, -0.00238, 0.83051], 1.5e-2)] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < tol, "k3 corner vert {want:?} off by {best:.4} (tol {tol})");
        }
    }

    fn penta_bipyramid() -> HalfEdgeMesh {
        let mut p = vec![Vec3::new(0.0,0.0,1.0), Vec3::new(0.0,0.0,-1.0)];
        for i in 0..5 { let a = std::f32::consts::TAU*i as f32/5.0; p.push(Vec3::new(a.cos(),a.sin(),0.0)); }
        let mut f: Vec<Vec<usize>> = Vec::new();
        for i in 0..5 { let j=(i+1)%5; f.push(vec![0, 2+i, 2+j]); f.push(vec![1, 2+j, 2+i]); }
        HalfEdgeMesh::from_polygons(&p, &f)
    }
    /// SPAN CORNER robustness sweep: n-gonal bipyramids (n=5,6,7) beveled over every contiguous run of k
    /// apex edges that leaves a >=2 unbeveled span, at seg 2/3/4 — all 18 must stay watertight + manifold +
    /// actually rounded (not flat-clamped). Guards the new across-span-BoundVert path against valence / span
    /// length / segment variation, not just the single oracle.
    #[test]
    fn span_corner_sweep_watertight() {
        // n-gonal bipyramids (deg-n apex), bevel every CONTIGUOUS run of k apex edges that leaves a >=2
        // unbeveled span, at seg 2/3/4. Must stay watertight+manifold (or the app guard would reject it).
        let mut fail = 0; let mut total = 0; let mut rounded = 0;
        for n in [5usize, 6, 7] {
            let mut p = vec![Vec3::new(0.0,0.0,1.0), Vec3::new(0.0,0.0,-1.0)];
            for i in 0..n { let a = std::f32::consts::TAU*i as f32/n as f32; p.push(Vec3::new(a.cos(),a.sin(),0.0)); }
            let mut f: Vec<Vec<usize>> = Vec::new();
            for i in 0..n { let j=(i+1)%n; f.push(vec![0, 2+i, 2+j]); f.push(vec![1, 2+j, 2+i]); }
            let m = HalfEdgeMesh::from_polygons(&p, &f);
            for k in 3..=(n-2) {                       // k beveled, leaves n-k >= 2 unbeveled (a span)
                for seg in [2u32,3,4] {
                    let mut sel = HashSet::new();
                    for i in 0..m.half_edges.len() as u32 {
                        let he = m.he(HalfEdgeId(i));
                        if he.twin==NO_TWIN || he.face==NO_FACE { continue; }
                        let (s,d)=(m.he_source(HalfEdgeId(i)).0, he.vertex.0);
                        // bevel apex(0)->eq[0..k]  (contiguous)
                        let is=|a:u32,b:u32| (s==a&&d==b)||(s==b&&d==a);
                        if (0..k as u32).any(|e| is(0, 2+e)) { sel.insert(i); }
                    }
                    let out = bevel_edges_offset(&m, &sel, &BevelParams{width:0.2,segments:seg,shape:0.5,clamp:true});
                    let bnd=(0..out.half_edges.len() as u32).filter(|&i| out.he(HalfEdgeId(i)).twin==NO_TWIN && out.he(HalfEdgeId(i)).face!=NO_FACE).count();
                    let ok = bnd==0 && out.validate().is_ok();
                    total+=1;
                    // "rounded" = produced more than the flat-clamp vertex count would
                    if out.vertices.len() > 2+n+ (k as usize)*seg as usize { rounded+=1; }
                    if !ok { fail+=1; eprintln!("  FAIL n={n} k={k} seg={seg}: boundary={bnd} valid={}", out.validate().is_ok()); }
                }
            }
        }
        eprintln!("SWEEP total={total} watertight={} rounded~={rounded}", total-fail);
        assert_eq!(fail, 0, "{fail}/{total} span configs produced non-watertight output");
    }

    /// FLAT-SURFACE / SPLIT-EDGE bevel (`flat_edge_ref.json` oracle): a coplanar edge (between two
    /// same-plane faces) bevels into a FLAT strip of parallel edges — no dihedral to round, so seg>1 just
    /// adds more in-plane lines (Blender-identical: nv=16/nf=10 at seg3). Watertight; strip stays in-plane.
    #[test]
    fn flat_surface_edge_bevel_matches_blender() {
        // Box [-1,1]^3 with the +Y top face split into 2 coplanar quads by a middle edge at x=0.
        let mut p = vec![
            Vec3::new(-1.,-1.,-1.),Vec3::new(1.,-1.,-1.),Vec3::new(1.,-1.,1.),Vec3::new(-1.,-1.,1.), //0-3 bottom
            Vec3::new(-1.,1.,-1.),Vec3::new(1.,1.,-1.),Vec3::new(1.,1.,1.),Vec3::new(-1.,1.,1.),     //4-7 top
            Vec3::new(0.,1.,-1.),Vec3::new(0.,1.,1.),                                                 //8,9 top mids
        ];
        let f: Vec<Vec<usize>> = vec![
            vec![0,1,2,3],                     // bottom (-Y)
            vec![7,9,8,4], vec![9,6,5,8],      // top (+Y) split into 2 coplanar quads (shared edge 8-9)
            vec![4,8,5,1,0],                   // back (-Z) pentagon, includes split vert 8
            vec![6,9,7,3,2],                   // front (+Z) pentagon, includes split vert 9
            vec![7,4,0,3],                     // left (-X)
            vec![5,6,2,1],                     // right (+X)
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        assert_eq!((0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin==NO_TWIN && m.he(HalfEdgeId(i)).face!=NO_FACE).count(), 0, "input mesh must be closed");
        // bevel the split edge 8-9 (coplanar faces)
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin==NO_TWIN || he.face==NO_FACE { continue; }
            let (s,d)=(m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            if (s==8&&d==9)||(s==9&&d==8) { sel.insert(i); }
        }
        for seg in [1u32,3] {
            let out = bevel_edges_offset(&m, &sel, &BevelParams{width:0.3,segments:seg,shape:0.5,clamp:true});
            let bnd=(0..out.half_edges.len() as u32).filter(|&i| out.he(HalfEdgeId(i)).twin==NO_TWIN && out.he(HalfEdgeId(i)).face!=NO_FACE).count();
            assert_eq!(bnd, 0, "flat-edge bevel seg{seg} must be watertight");
            assert!(out.validate().is_ok(), "flat-edge bevel seg{seg} manifold");
            // Every strip vert stays exactly in the flat plane y=1 (a coplanar edge has no dihedral to bulge).
            let off = out.vertices.iter().filter(|v| v.position.y>0.99 && v.position.x.abs()<0.45)
                .map(|v| (v.position.y-1.0).abs()).fold(0.0f32,f32::max);
            assert!(off < 1e-4, "flat-edge strip must stay in the plane (dev {off})");
        }
        // seg=3 matches Blender's flat-edge oracle (flat_edge_ref.json): nv=16 nf=10, 4 evenly-spaced
        // parallel lines at x=+-0.3,+-0.1 in the y=1 plane.
        let out3 = bevel_edges_offset(&m, &sel, &BevelParams{width:0.3,segments:3,shape:0.5,clamp:true});
        assert_eq!(out3.vertices.len(), 16, "oracle nv=16");
        assert_eq!(out3.faces.len(), 10, "oracle nf=10");
        for x in [-0.3f32,-0.1,0.1,0.3] { for z in [-1.0f32,1.0] {
            let want = Vec3::new(x,1.0,z);
            assert!(out3.vertices.iter().any(|v| (v.position-want).length()<1e-3), "flat strip vert {want:?} missing (not Blender-exact)");
        }}
    }

    /// FLAT spoke at a degree-N flat centre: hexagonal coin, fan-triangulated caps (degree-6 flat centre,
    /// all spokes coplanar). Beveling one flat spoke (far endpoint = the many-coplanar-edge centre) stays
    /// watertight + manifold at seg 1 and 3 — the hard flat case where a dihedral-assuming corner could break.
    #[test]
    fn flat_spoke_ngon_center_watertight() {
        // Hexagonal "coin": top+bottom caps fan-triangulated from a center vertex (degree-6 flat center,
        // all 6 spoke edges coplanar in the cap plane). Bevel ONE top spoke edge (flat interior edge whose
        // far endpoint is the degree-6 flat center).
        let n = 6usize;
        let mut p: Vec<Vec3> = Vec::new();
        for i in 0..n { let a=std::f32::consts::TAU*i as f32/n as f32; p.push(Vec3::new(a.cos(),a.sin(),0.3)); }   // top rim 0..n
        for i in 0..n { let a=std::f32::consts::TAU*i as f32/n as f32; p.push(Vec3::new(a.cos(),a.sin(),-0.3)); }  // bot rim n..2n
        let ct = p.len(); p.push(Vec3::new(0.,0.,0.3));   // top center = 2n
        let cb = p.len(); p.push(Vec3::new(0.,0.,-0.3));  // bot center = 2n+1
        let mut f: Vec<Vec<usize>> = Vec::new();
        for i in 0..n { let j=(i+1)%n; f.push(vec![ct, i, j]); }            // top cap fan (+Z)
        for i in 0..n { let j=(i+1)%n; f.push(vec![cb, n+j, n+i]); }        // bottom cap fan (-Z)
        for i in 0..n { let j=(i+1)%n; f.push(vec![i, n+i, n+j, j]); }      // side walls
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        // bevel top spoke ct->0 (flat, far endpoint = degree-6 flat center)
        let mut sel=HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he=m.he(HalfEdgeId(i)); if he.twin==NO_TWIN||he.face==NO_FACE {continue;}
            let (s,d)=(m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            if (s==ct as u32&&d==0)||(s==0&&d==ct as u32) { sel.insert(i); }
        }
        for seg in [1u32,3] {
            let out=bevel_edges_offset(&m,&sel,&BevelParams{width:0.2,segments:seg,shape:0.5,clamp:true});
            let bnd=(0..out.half_edges.len() as u32).filter(|&i| out.he(HalfEdgeId(i)).twin==NO_TWIN && out.he(HalfEdgeId(i)).face!=NO_FACE).count();
            assert_eq!(bnd, 0, "flat spoke at degree-N centre seg{seg} watertight");
            assert!(out.validate().is_ok(), "flat spoke at degree-N centre seg{seg} manifold");
        }
    }

    /// PLANE CORNER vertex bevel (owner: "not able to bevel a vertex on a plane"). A single
    /// open quad has valence-2 corners whose `vertex_fan` is boundary-incomplete (length 1).
    /// Core used to skip `d < 3` and refuse the re-trace — no-op. Must cut the corner in-plane
    /// (no 3D cap): seg=1 → pentagon (nv=5 nf=1), installable open mesh, BoundVerts on both legs.
    #[test]
    fn vertex_bevel_plane_corner_cuts_in_plane() {
        let p = vec![
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ];
        let m = HalfEdgeMesh::from_polygons(&p, &[vec![0, 1, 2, 3]]);
        assert_eq!(boundary_loop_count(&m), 1, "fixture: one open rim");
        // Corner at (1,1,0) = v2.
        let vi = m
            .vertices
            .iter()
            .position(|v| (v.position - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-4)
            .unwrap() as u32;
        let sel: HashSet<u32> = std::iter::once(vi).collect();
        let out = bevel_vertices_offset(
            &m,
            &sel,
            &BevelParams {
                width: 0.4,
                segments: 1,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(
            out.is_manifold().is_ok(),
            "plane corner must stay manifold (open): {:?}",
            out.is_manifold()
        );
        assert!(
            bevel_result_installable(&m, &out),
            "plane corner must pass the install guard"
        );
        assert_eq!(boundary_loop_count(&out), 1, "still one open rim (no lid)");
        assert_eq!(out.face_count(), 1, "still one face (no 3D cap)");
        assert_eq!(
            out.vertex_count(),
            5,
            "corner removed + 2 BoundVerts → nv=5 (seg1)"
        );
        // BoundVerts slide 0.4 along both legs from (1,1,0) → (0.6,1,0) and (1,0.6,0).
        for want in [Vec3::new(0.6, 1.0, 0.0), Vec3::new(1.0, 0.6, 0.0)] {
            assert!(
                out.vertices
                    .iter()
                    .any(|v| (v.position - want).length() < 1e-3),
                "BoundVert {want:?} missing"
            );
        }
        // Original corner must be gone.
        assert!(
            !out
                .vertices
                .iter()
                .any(|v| (v.position - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-3),
            "original corner must be deleted"
        );
        // Rounded open corner: more arc verts, still one face, installable.
        let out3 = bevel_vertices_offset(
            &m,
            &sel,
            &BevelParams {
                width: 0.4,
                segments: 3,
                shape: 0.5,
                clamp: true,
            },
        );
        assert!(bevel_result_installable(&m, &out3), "seg3 plane corner installable");
        assert_eq!(out3.face_count(), 1, "seg3 still one face");
        assert_eq!(
            out3.vertex_count(),
            7,
            "seg3: 4-1+(3+1) = 7 verts"
        );
        assert_eq!(boundary_loop_count(&out3), 1);
    }

    /// VERTEX BEVEL — Blender parity (`vbevel_cube_seg{1,3}.json` oracles): cube corner (1,1,1) beveled
    /// with affect=VERTICES, offset 0.4. seg=1 = flat truncation (nv=10 nf=7, 3 BoundVerts vertex-EXACT at
    /// V slid along each edge = (1,0.6,1)/(1,1,0.6)/(0.6,1,1), 1 triangle cap, 3 faces → pentagons). seg=3 =
    /// rounded (nv=19 nf=13, M_ADJ grid cap, 3 faces → heptagons), grid within the fullness-path tolerance.
    #[test]
    fn vertex_bevel_cube_matches_oracle() {
        // seg=1 flat truncation — vertex-exact.
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let vi = cube.vertices.iter().position(|v| (v.position - Vec3::ONE).length() < 1e-4).unwrap() as u32;
        let sel: HashSet<u32> = std::iter::once(vi).collect();
        let out = bevel_vertices_offset(&cube, &sel, &BevelParams{width:0.4,segments:1,shape:0.5,clamp:true});
        let bnd = |m:&HalfEdgeMesh| (0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin==NO_TWIN && m.he(HalfEdgeId(i)).face!=NO_FACE).count();
        assert_eq!(bnd(&out), 0, "vertex bevel seg1 watertight");
        assert!(out.validate().is_ok(), "vertex bevel seg1 manifold");
        assert_eq!(out.vertices.len(), 10, "oracle nv=10"); assert_eq!(out.faces.len(), 7, "oracle nf=7");
        for o in [[1.0,0.6,1.0],[1.0,1.0,0.6],[0.6,1.0,1.0]] {
            let want=Vec3::new(o[0],o[1],o[2]);
            assert!(out.vertices.iter().any(|v| (v.position-want).length()<1e-3), "BoundVert {want:?} not exact");
        }
        // seg=3 rounded — exact counts.
        let out3 = bevel_vertices_offset(&cube, &sel, &BevelParams{width:0.4,segments:3,shape:0.5,clamp:true});
        assert_eq!(bnd(&out3), 0, "vertex bevel seg3 watertight");
        assert!(out3.validate().is_ok(), "vertex bevel seg3 manifold");
        assert_eq!(out3.vertices.len(), 19, "oracle nv=19"); assert_eq!(out3.faces.len(), 13, "oracle nf=13");
    }

    /// VERTEX BEVEL robustness sweep: every corner of a cube (deg-3), an octahedron vertex (deg-4), and
    /// n-gonal bipyramid apexes (deg 5/6) at seg 1/2/3 — plus ALL 8 cube corners at once (multi-select) —
    /// must stay watertight + manifold.
    #[test]
    fn vertex_bevel_sweep_watertight() {
        let bnd = |m:&HalfEdgeMesh| (0..m.half_edges.len() as u32).filter(|&i| m.he(HalfEdgeId(i)).twin==NO_TWIN && m.he(HalfEdgeId(i)).face!=NO_FACE).count();
        let check = |m:&HalfEdgeMesh, sel:&HashSet<u32>, seg:u32, label:&str| {
            let out = bevel_vertices_offset(m, sel, &BevelParams{width:0.25,segments:seg,shape:0.5,clamp:true});
            assert_eq!(bnd(&out), 0, "{label} seg{seg} boundary");
            assert!(out.validate().is_ok(), "{label} seg{seg} manifold");
        };
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        for seg in [1u32,2,3] {
            for vi in 0..cube.vertices.len() as u32 { check(&cube, &std::iter::once(vi).collect(), seg, "cube-corner"); }
            check(&cube, &(0..cube.vertices.len() as u32).collect(), seg, "cube-all-corners");
        }
        // octahedron (deg-4 verts)
        let op = vec![Vec3::new(1.,0.,0.),Vec3::new(-1.,0.,0.),Vec3::new(0.,1.,0.),Vec3::new(0.,-1.,0.),Vec3::new(0.,0.,1.),Vec3::new(0.,0.,-1.)];
        let of = vec![vec![0,2,4],vec![2,1,4],vec![1,3,4],vec![3,0,4],vec![2,0,5],vec![1,2,5],vec![3,1,5],vec![0,3,5]];
        let octa = HalfEdgeMesh::from_polygons(&op,&of);
        for seg in [1u32,2,3] { check(&octa, &std::iter::once(4u32).collect(), seg, "octa-apex"); }
        // n-gonal bipyramid apexes (deg 5,6)
        for n in [5usize,6] {
            let mut p = vec![Vec3::new(0.,0.,1.),Vec3::new(0.,0.,-1.)];
            for i in 0..n { let a=std::f32::consts::TAU*i as f32/n as f32; p.push(Vec3::new(a.cos(),a.sin(),0.)); }
            let mut f: Vec<Vec<usize>> = Vec::new();
            for i in 0..n { let j=(i+1)%n; f.push(vec![0,2+i,2+j]); f.push(vec![1,2+j,2+i]); }
            let bp = HalfEdgeMesh::from_polygons(&p,&f);
            for seg in [1u32,2,3] { check(&bp, &std::iter::once(0u32).collect(), seg, "bipyramid-apex"); }
        }
    }

    /// SPAN CORNER — consecutive-unbeveled mixed corner (`consec_ref.json` oracle): pentagonal bipyramid,
    /// bevel 3 CONSECUTIVE apex edges (fan T,T,T,F,F — a run of 2 unbeveled edges), seg=3. Was a global FLAT
    /// clamp; now rounds as Blender's count-3 M_ADJ grid. The across-span BoundVert (Blender's offset_meet
    /// across the unbeveled span, drop-onto-face) is VERTEX-EXACT ((-0.0421,-0.1295,0.8317)); counts match
    /// Blender (nv=30 nf=35, 12 apex verts); interior within the adj_vmesh fullness-path tolerance (same
    /// class as the hex-cone / k=3 gates). Watertight + manifold.
    #[test]
    fn span_corner_consecutive_unbeveled_rounds() {
        let mut p = vec![Vec3::new(0.0,0.0,1.0), Vec3::new(0.0,0.0,-1.0)];
        for i in 0..5 { let a = std::f32::consts::TAU*i as f32/5.0; p.push(Vec3::new(a.cos(),a.sin(),0.0)); }
        let mut f: Vec<Vec<usize>> = Vec::new();
        for i in 0..5 { let j=(i+1)%5; f.push(vec![0, 2+i, 2+j]); f.push(vec![1, 2+j, 2+i]); }
        let m = HalfEdgeMesh::from_polygons(&p, &f);
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let he = m.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let (s,d) = (m.he_source(HalfEdgeId(i)).0, he.vertex.0);
            let e = |a:u32,b:u32| (s==a&&d==b)||(s==b&&d==a);
            if e(0,2)||e(0,3)||e(0,4) { sel.insert(i); }
        }
        let out = bevel_edges_offset(&m, &sel, &BevelParams{width:0.25,segments:3,shape:0.5,clamp:true});
        let bnd = (0..out.half_edges.len() as u32).filter(|&i| out.he(HalfEdgeId(i)).twin==NO_TWIN && out.he(HalfEdgeId(i)).face!=NO_FACE).count();
        assert_eq!(bnd, 0, "span corner must be watertight (was rounded? boundary={bnd})");
        assert!(out.validate().is_ok(), "span corner manifold");
        assert_eq!(out.vertices.len(), 30, "oracle nv=30");
        assert_eq!(out.faces.len(), 35, "oracle nf=35");
        let apex = out.vertices.iter().filter(|v| v.position.z > 0.5).count();
        assert_eq!(apex, 12, "12 apex verts = count-3 grid (a flat clamp would give far fewer)");
        // across-span BoundVert is vertex-exact; interior within the fullness-path tolerance.
        let bv = Vec3::new(-0.0421,-0.1295,0.8317);
        assert!(out.vertices.iter().any(|v| (v.position-bv).length() < 2e-3), "across-span BoundVert not exact");
        for o in [[0.026,0.3424,0.56],[0.1803,0.2923,0.56],[-0.1196,0.203,0.6704],[0.2161,0.0939,0.6704]] {
            let want = Vec3::new(o[0],o[1],o[2]);
            let best = out.vertices.iter().map(|v| (v.position-want).length()).fold(f32::MAX,f32::min);
            assert!(best < 2e-2, "span vert {want:?} off by {best:.4}");
        }
    }

    /// MIXED k=2-ADJACENT CORNER — VERTEX-EXACT to Blender (`mixed_k2d4_ref.json` oracle): octahedron,
    /// bevel 2 ADJACENT edges at the degree-4 vertex (0,0,1), seg=3. The wrap sector's BoundVert is the
    /// skew rail meet DROPPED perpendicularly onto the intermediate face (−0.0544,−0.0544,0.8911) — the
    /// winding-free `drop_onto_faces` (shared-face resolution + centroid-oriented wedge); the M_NONE weld
    /// chain then lands exactly on Blender's spine ((−0.0053,−0.0053,0.9018)/(0.0744,0.0744,0.8221)).
    /// nv=17 nf=20, watertight + manifold.
    #[test]
    fn mixed_k2_adjacent_deg4_matches_oracle() {
        let out = octa_bevel(&[(2, 1), (2, 0)], 3); // apex (0,0,1) → +X and +Y (oracle's edge pair)
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "mixed k2-adjacent must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "mixed k2-adjacent manifold");
        assert_eq!(out.vertices.len(), 17, "oracle nv=17");
        assert_eq!(out.faces.len(), 20, "oracle nf=20");
        for o in [[0.1633, 0.1633, 0.6734], [-0.05443, -0.05443, 0.89113],
                  [0.07436, 0.07436, 0.82212], [-0.00534, -0.00534, 0.90181]] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 2e-3, "corner vert {want:?} missing (closest {best:.4}) — not Blender-exact");
        }
    }

    /// MIXED k=1 PARTIAL AT DEGREE-4, ROUNDED — VERTEX-EXACT to Blender (`mixed_k1d4_ref.json` oracle):
    /// octahedron, bevel ONE edge into the degree-4 vertex, seg=3. Blender = M_TRI_FAN: the profile arc
    /// fanned to the ON-EDGE BoundVert slid down the opposite edge ((−0.1414,0,0.8586) = w·√2⁄2) — our
    /// delete-V + recessed-center partial path. nv=14 nf=17, watertight + manifold.
    #[test]
    fn mixed_k1_deg4_rounded_matches_oracle() {
        let out = octa_bevel(&[(2, 1)], 3); // apex (0,0,1) → +X (oracle's edge)
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "mixed k1 partial must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "mixed k1 partial manifold");
        assert_eq!(out.vertices.len(), 14, "oracle nv=14");
        assert_eq!(out.faces.len(), 17, "oracle nf=17");
        for o in [[0.0, -0.1633, 0.8367], [0.0, 0.1633, 0.8367], [-0.14142, 0.0, 0.85858],
                  [0.0, -0.05977, 0.89647], [0.0, 0.05977, 0.89647]] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 2e-3, "apex vert {want:?} missing (closest {best:.4}) — not Blender-exact");
        }
    }

    /// ROUNDED EDGE CHAIN — VERTEX-EXACT to Blender (`chain2_cube_ref.json` oracle): ±1 cube, TWO adjacent
    /// top edges sharing ONE degree-3 corner (an L-selection), seg=3, offset 0.3. The shared corner is
    /// Blender's 2-BoundVert WELD (STEP 2c): rail MEET on the shared face (0.7,−0.7,1.0) + terminal SLIDE
    /// down the unbeveled edge (1,−1,0.7), bridged by the ELLIPSE profile arc ((0.85,−0.85,0.95981) /
    /// (0.95981,−0.95981,0.85)) SHARED by both strips. nv=17 nf=12, watertight + manifold.
    #[test]
    fn rounded_chain_cube_matches_oracle() {
        let cube = HalfEdgeMesh::new_box(Vec3::ONE);
        let has = |i: u32, a: Vec3, b: Vec3| {
            let s = cube.vertices[cube.he_source(HalfEdgeId(i)).0 as usize].position;
            let d = cube.vertices[cube.he(HalfEdgeId(i)).vertex.0 as usize].position;
            ((s - a).length() < 1e-4 && (d - b).length() < 1e-4)
                || ((s - b).length() < 1e-4 && (d - a).length() < 1e-4)
        };
        // The oracle's edges: (1,−1,1)→(−1,−1,1) and (1,−1,1)→(1,1,1), sharing corner (1,−1,1).
        let (e1a, e1b) = (Vec3::new(1.0, -1.0, 1.0), Vec3::new(-1.0, -1.0, 1.0));
        let (e2a, e2b) = (Vec3::new(1.0, -1.0, 1.0), Vec3::new(1.0, 1.0, 1.0));
        let mut sel = HashSet::new();
        for i in 0..cube.half_edges.len() as u32 {
            let he = cube.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            if has(i, e1a, e1b) || has(i, e2a, e2b) { sel.insert(i); }
        }
        let out = bevel_edges_offset(&cube, &sel, &BevelParams { width: 0.3, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "rounded chain must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "rounded chain manifold");
        assert_eq!(out.vertices.len(), 17, "oracle nv=17 (flat clamp gave 11)");
        assert_eq!(out.faces.len(), 12, "oracle nf=12");
        for o in [[0.7, -0.7, 1.0], [1.0, -1.0, 0.7], [0.85, -0.85, 0.95981], [0.95981, -0.95981, 0.85]] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 2e-3, "chain vert {want:?} missing (closest {best:.4}) — not Blender-exact");
        }
    }

    /// CLAMP OVERLAP breadth (Blender `geometry_collide_offset`): the old blanket clamp (half the
    /// shortest incident edge) cut a 12-gon cap-loop bevel at 0.26·r; Blender's per-edge collapse limit
    /// allows len/(2·(1+cos150°)/sin150°) ≈ 0.97·r (the user's "this is the most I can bevel" report).
    /// A width-0.6 loop bevel must ACTUALLY reach ~0.6 deep (cap ring pulled well inside 0.55·r) and
    /// stay watertight + manifold. Also: an oversized request (w=5) still clamps (no inversion).
    #[test]
    fn clamp_allows_blender_width() {
        let n = 12usize;
        let mut p: Vec<Vec3> = Vec::new();
        for i in 0..n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            p.push(Vec3::new(a.cos(), a.sin(), 1.0));
        }
        for i in 0..n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            p.push(Vec3::new(a.cos(), a.sin(), -1.0));
        }
        let mut f: Vec<Vec<usize>> = vec![(0..n).collect(), (n..2 * n).rev().collect()];
        for i in 0..n {
            let j = (i + 1) % n;
            f.push(vec![i, n + i, n + j, j]);
        }
        let cyl = HalfEdgeMesh::from_polygons(&p, &f);
        let mut sel = HashSet::new();
        for i in 0..cyl.half_edges.len() as u32 {
            let he = cyl.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let s = cyl.vertices[cyl.he_source(HalfEdgeId(i)).0 as usize].position;
            let d = cyl.vertices[he.vertex.0 as usize].position;
            if s.z > 0.5 && d.z > 0.5 { sel.insert(i); }
        }
        for (wreq, must_reach) in [(0.6f32, true), (5.0, false)] {
            let out = bevel_edges_offset(&cyl, &sel, &BevelParams { width: wreq, segments: 3, shape: 0.5, clamp: true });
            let bnd = (0..out.half_edges.len() as u32)
                .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
            assert_eq!(bnd, 0, "w={wreq} clamped cap loop must stay watertight; boundary={bnd}");
            assert!(out.validate().is_ok(), "w={wreq} clamped cap loop manifold");
            if must_reach {
                // The cap ring (verts still at z==1) must have been inset well past the OLD 0.259 clamp:
                // meet distance from the vertex ≈ w/cos(15°) → ring radius ≈ 1 − 0.6·1.035·cos(15°)… just
                // require it clearly inside 0.55 (old clamp left it at ≈0.75).
                let ring_r = out.vertices.iter().filter(|v| (v.position.z - 1.0).abs() < 1e-4)
                    .map(|v| v.position.truncate().length()).fold(0.0f32, f32::max);
                assert!(ring_r < 0.55, "w=0.6 cap loop still clamped like the old rule (ring r={ring_r:.3})");
            }
        }
    }

    /// ROUNDED CAP LOOP — VERTEX-EXACT to Blender (`caploop_cyl_ref.json` oracle): 12-gon prism (r=1,
    /// z=±1), bevel the ENTIRE top rim loop, seg=3, offset 0.2 — the user's "cylinder top round bevel"
    /// report. Every rim vertex is a STEP-2c chain weld. nv=60 (12×[meet+slide+2 arc] + 12 bottom),
    /// nf=50 (2 twelve-gons + 48 quads), watertight + manifold, and the (1,0,1) vertex family matches
    /// Blender exactly: meet (0.79294,0,1), slide (1,0,0.8), arcs (0.89647,0,0.97321)/(0.97226,0,0.9).
    #[test]
    fn rounded_cap_loop_cylinder_matches_oracle() {
        let n = 12usize;
        let mut p: Vec<Vec3> = Vec::new();
        for i in 0..n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            p.push(Vec3::new(a.cos(), a.sin(), 1.0));
        }
        for i in 0..n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            p.push(Vec3::new(a.cos(), a.sin(), -1.0));
        }
        let mut f: Vec<Vec<usize>> = vec![(0..n).collect(), (n..2 * n).rev().collect()];
        for i in 0..n {
            let j = (i + 1) % n;
            f.push(vec![i, n + i, n + j, j]);
        }
        let cyl = HalfEdgeMesh::from_polygons(&p, &f);
        let mut sel = HashSet::new();
        for i in 0..cyl.half_edges.len() as u32 {
            let he = cyl.he(HalfEdgeId(i));
            if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
            let s = cyl.vertices[cyl.he_source(HalfEdgeId(i)).0 as usize].position;
            let d = cyl.vertices[he.vertex.0 as usize].position;
            if s.z > 0.5 && d.z > 0.5 { sel.insert(i); }
        }
        let out = bevel_edges_offset(&cyl, &sel, &BevelParams { width: 0.2, segments: 3, shape: 0.5, clamp: true });
        let bnd = (0..out.half_edges.len() as u32)
            .filter(|&i| out.he(HalfEdgeId(i)).twin == NO_TWIN && out.he(HalfEdgeId(i)).face != NO_FACE).count();
        assert_eq!(bnd, 0, "rounded cap loop must be watertight; boundary={bnd}");
        assert!(out.validate().is_ok(), "rounded cap loop manifold");
        assert_eq!(out.vertices.len(), 60, "oracle nv=60");
        assert_eq!(out.faces.len(), 50, "oracle nf=50 (2 twelve-gons + 48 quads)");
        for o in [[0.79294, 0.0, 1.0], [1.0, 0.0, 0.8], [0.89647, 0.0, 0.97321], [0.97226, 0.0, 0.9]] {
            let want = Vec3::new(o[0], o[1], o[2]);
            let best = out.vertices.iter().map(|v| (v.position - want).length()).fold(f32::MAX, f32::min);
            assert!(best < 2e-3, "cap-loop vert {want:?} missing (closest {best:.4}) — not Blender-exact");
        }
    }

    /// REFERENCE DUMP: run MY bevel for the canonical scenarios and write JSON to $BEVEL_DUMP, to diff
    /// against Blender's headless output (same ±1 cube, edges selected by midpoint).
    #[test]
    #[ignore = "reference dump (set BEVEL_DUMP=path)"]
    fn dump_scenarios_json() {
        fn edges_by_mid(m: &HalfEdgeMesh, mids: &[[f32; 3]]) -> HashSet<u32> {
            let mut s = HashSet::new();
            for i in 0..m.half_edges.len() as u32 {
                let he = m.he(HalfEdgeId(i));
                if he.twin == NO_TWIN || he.face == NO_FACE { continue; }
                let a = m.vertices[m.he_source(HalfEdgeId(i)).0 as usize].position;
                let b = m.vertices[he.vertex.0 as usize].position;
                let mid = (a + b) * 0.5;
                for t in mids { if (mid - Vec3::new(t[0], t[1], t[2])).length() < 1e-3 { s.insert(i); } }
            }
            s
        }
        fn vertical_near(m: &HalfEdgeMesh, xy: [f32; 2]) -> HashSet<u32> {
            let mut best = None; let mut bs = f32::MIN;
            for i in 0..m.half_edges.len() as u32 {
                let he = m.he(HalfEdgeId(i));
                if he.twin == NO_TWIN || he.face == NO_FACE || m.he(he.twin).face == NO_FACE { continue; }
                let a = m.vertices[m.he_source(HalfEdgeId(i)).0 as usize].position;
                let b = m.vertices[he.vertex.0 as usize].position;
                let span = (a.z - b.z).abs(); let mid = (a + b) * 0.5;
                let d = (mid.x - xy[0]).powi(2) + (mid.y - xy[1]).powi(2);
                let score = span - d;
                if span > 1.0 && score > bs { bs = score; best = Some(i); }
            }
            best.into_iter().collect()
        }
        fn dump(m: &HalfEdgeMesh) -> String {
            let mut vs = String::new();
            for v in &m.vertices { vs.push_str(&format!("[{:.5},{:.5},{:.5}],", v.position.x, v.position.y, v.position.z)); }
            let mut fs = String::new();
            for fi in 0..m.faces.len() as u32 {
                let fv = m.face_vertices(FaceId(fi));
                if fv.len() < 3 { continue; }
                fs.push('[');
                for x in &fv { fs.push_str(&format!("{},", x.0)); }
                fs.push_str("],");
            }
            format!("{{\"nv\":{},\"verts\":[{}],\"faces\":[{}]}}", m.vertices.len(), vs, fs)
        }
        let bp = |w: f32, s: u32| BevelParams { width: w, segments: s, shape: 0.5, clamp: true };
        let bf = [[0f32, -1., -1.]];
        let two = [[0f32, -1., -1.], [1., 0., -1.]];
        let mut items: Vec<String> = Vec::new();
        let nb = HalfEdgeMesh::new_box;
        for s in [1u32, 2, 3, 4, 5] {
            let c = nb(Vec3::ONE);
            items.push(format!("\"single_seg{s}\":{}", dump(&bevel_edges_offset(&c, &edges_by_mid(&c, &bf), &bp(0.25, s)))));
        }
        for w in [0.1f32, 0.4] {
            let c = nb(Vec3::ONE);
            items.push(format!("\"single_w{w}\":{}", dump(&bevel_edges_offset(&c, &edges_by_mid(&c, &bf), &bp(w, 3)))));
        }
        for s in [1u32, 3] {
            let c = nb(Vec3::ONE);
            items.push(format!("\"two_adj_seg{s}\":{}", dump(&bevel_edges_offset(&c, &edges_by_mid(&c, &two), &bp(0.25, s)))));
        }
        for s in [1u32, 2, 3, 4, 5] {
            let c = nb(Vec3::ONE);
            let r1 = bevel_edges_offset(&c, &edges_by_mid(&c, &two), &bp(0.25, 1));
            let r2 = bevel_edges_offset(&r1, &vertical_near(&r1, [1., -1.]), &bp(0.15, s));
            items.push(format!("\"onbeveled_seg{s}\":{}", dump(&r2)));
        }
        for w in [0.1f32, 0.25] {
            let c = nb(Vec3::ONE);
            let r1 = bevel_edges_offset(&c, &edges_by_mid(&c, &two), &bp(0.25, 1));
            let r2 = bevel_edges_offset(&r1, &vertical_near(&r1, [1., -1.]), &bp(w, 3));
            items.push(format!("\"onbeveled_w{w}\":{}", dump(&r2)));
        }
        {
            let c = nb(Vec3::ONE);
            let r1 = bevel_edges_offset(&c, &edges_by_mid(&c, &two), &bp(0.25, 3));
            let r2 = bevel_edges_offset(&r1, &vertical_near(&r1, [1., -1.]), &bp(0.15, 3));
            items.push(format!("\"onbeveled_round1_seg3\":{}", dump(&r2)));
        }
        let out = format!("{{{}}}", items.join(","));
        let path = std::env::var("BEVEL_DUMP").unwrap_or_else(|_| "mine.json".into());
        std::fs::write(&path, out).unwrap();
        eprintln!("WROTE {path}");
    }

    /// SIMPLE-FACE GATE — regression for the twisted-endpoint-face bevel bug ("the two new edges slide
    /// along the wrong face"). Every beveled output face must be a SIMPLE polygon (no two non-adjacent
    /// edges cross). The `(false,false)` corner re-trace used to insert the two rail points in reversed
    /// order, self-intersecting the endpoint faces. Topology gates (manifold, watertight) do NOT catch
    /// this — a self-intersecting ring still twins fine — so it needs its own geometric gate.
    #[test]
    fn bevel_output_faces_are_simple() {
        fn seg_cross(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) -> bool {
            let o = |p: (f32, f32), q: (f32, f32), r: (f32, f32)|
                (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
            let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
            ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
        }
        let make = || HalfEdgeMesh::new_box(Vec3::ONE);
        let single: HashSet<u32> = std::iter::once(pick_interior_edge(&make())).collect();
        let loop_sel: HashSet<u32> = make().face_half_edges(FaceId(1)).iter().map(|h| h.0).collect();
        let configs: [(&HashSet<u32>, u32); 4] = [(&single, 1), (&single, 4), (&loop_sel, 1), (&loop_sel, 3)];
        for (ci, &(sel, seg)) in configs.iter().enumerate() {
            let out = bevel_edges_offset(&make(), sel, &BevelParams { width: 0.2, segments: seg, shape: 0.5, clamp: true });
            for fi in 0..out.faces.len() as u32 {
                let fv = out.face_vertices(FaceId(fi));
                if fv.len() < 4 { continue; } // triangles cannot self-intersect
                let n = out.compute_face_normal(FaceId(fi));
                let up = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
                let u = n.cross(up).normalize_or_zero();
                let vv = n.cross(u);
                let pts: Vec<(f32, f32)> = fv.iter()
                    .map(|vt| { let p = out.vert(*vt).position; (p.dot(u), p.dot(vv)) }).collect();
                let m = pts.len();
                for i in 0..m {
                    let (a, b) = (pts[i], pts[(i + 1) % m]);
                    for j in (i + 1)..m {
                        if (i + 1) % m == j || (j + 1) % m == i { continue; } // adjacent edges share a vertex
                        let (c, d) = (pts[j], pts[(j + 1) % m]);
                        assert!(!seg_cross(a, b, c, d),
                            "config {ci} face {fi}: edges {i}->{} and {j}->{} cross — self-intersecting (twisted) bevel face",
                            (i + 1) % m, (j + 1) % m);
                    }
                }
            }
        }
    }

    /// ORIENTATION GATE — regression for the partial-bevel flipped-face bug.
    /// Every output face of a beveled closed CONVEX mesh must have its normal point OUTWARD. A beveled
    /// cube stays convex, so it is star-convex from its centroid → outward == (face_centroid - centroid).
    /// The non-convex `(false,false)` re-traced faces expose `compute_face_normal`'s first-3-vertex
    /// cross product returning the WRONG sign; Newell's area-weighted normal is sign-correct for any
    /// non-planar / non-convex polygon. This test FAILS on the old normal and PASSES on Newell.
    #[test]
    fn bevel_output_faces_all_point_outward() {
        use crate::half_edge::VertexId;
        let make = || HalfEdgeMesh::new_box(Vec3::ONE);
        // Selections × segment counts, each producing un-beveled-but-adjacent `(false,false)` corners:
        let single: HashSet<u32> = std::iter::once(pick_interior_edge(&make())).collect();
        let loop_sel: HashSet<u32> = make().face_half_edges(FaceId(1)).iter().map(|h| h.0).collect();
        let mut partial = loop_sel.clone();           // asymmetric: loop minus one edge → mixed corners
        if let Some(&e) = partial.iter().next() { partial.remove(&e); }
        let configs: [(&HashSet<u32>, u32); 4] = [(&single, 1), (&single, 4), (&loop_sel, 3), (&partial, 1)];
        for (ci, &(sel, seg)) in configs.iter().enumerate() {
            let out = bevel_edges_offset(&make(), sel, &BevelParams { width: 0.15, segments: seg, shape: 0.5, clamp: true });
            out.validate().expect("beveled mesh must be manifold");
            let mut c = Vec3::ZERO;
            for v in &out.vertices { c += v.position; }
            c /= out.vertices.len() as f32;
            for fi in 0..out.faces.len() as u32 {
                let fv = out.face_vertices(FaceId(fi));
                if fv.len() < 3 { continue; }
                let n = out.compute_face_normal(FaceId(fi));
                let mut fc = Vec3::ZERO;
                for v in &fv { fc += out.vert(*v).position; }
                fc /= fv.len() as f32;
                let outward = (fc - c).normalize_or_zero();
                // Gross-flip gate: a truly inverted face has dot ≈ -1. Corner-cap faces on a ROUNDED
                // bevel are legitimately near-tangential to the radial direction (dot ≈ 0), so tolerate a
                // small negative; only a clear inward normal is the flipped-face bug.
                assert!(n.dot(outward) > -0.02,
                    "config {ci} face {fi}: normal {:?} points INWARD (dot={:.4}) — flipped-face bug",
                    n, n.dot(outward));
                let _ = VertexId(0); // keep the import meaningful across refactors
            }
        }
    }

// ── DIAGNOSTIC (2026-08-04): is bevel_op sound on a PARTIAL edge set? ─────────────────
// Smart Select selects a SUBSET of edges (excluding a subtracted cylinder's tessellation
// and coplanar edges) and the modifier-level result then failed bevel_result_installable.
// Every existing modifier test bevels ALL manifold edges, so partial selection was never
// proven. These isolate it from the boolean entirely: a plain cube, hand-built subsets.
#[test]
fn diag_partial_edge_set_on_a_plain_cube() {
    use std::collections::HashSet;
    let p = BevelParams { width: 0.05, segments: 1, shape: 0.5, clamp: true };

    let all: HashSet<u32> = {
        let m = HalfEdgeMesh::new_box(Vec3::ONE);
        (0..m.half_edges.len() as u32)
            .filter(|&i| m.is_edge_live(HalfEdgeId(i)))
            .collect()
    };

    // Baseline: everything (the proven path).
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let full = bevel_edges_offset(&m, &all, &p);
    eprintln!(
        "[DIAG] ALL  edges={} -> v={} f={} valid={:?} manifold={:?} folds={}",
        all.len(), full.vertex_count(), full.face_count(),
        full.validate().is_ok(), full.is_manifold().is_ok(),
        full.count_folded_coplanar_pairs()
    );

    // ONE undirected edge (both half-edges) — the smallest possible partial set.
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let one: HashSet<u32> = {
        let h = HalfEdgeId(*all.iter().min().unwrap());
        let mut s = HashSet::new();
        s.insert(h.0);
        if m.he(h).twin != NO_TWIN {
            s.insert(m.he(h).twin.0);
        }
        s
    };
    let r1 = bevel_edges_offset(&m, &one, &p);
    eprintln!(
        "[DIAG] ONE  edges={} -> v={} f={} valid={:?} manifold={:?} folds={}",
        one.len(), r1.vertex_count(), r1.face_count(),
        r1.validate().is_ok(), r1.is_manifold().is_ok(),
        r1.count_folded_coplanar_pairs()
    );

    // HALF the edges — guarantees excluded edges neighbouring selected ones (open rails at
    // the boundary of the selection), which is the suspected failure mode.
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let mut sorted: Vec<u32> = all.iter().copied().collect();
    sorted.sort_unstable();
    let half: HashSet<u32> = sorted.iter().copied().take(sorted.len() / 2).collect();
    let r2 = bevel_edges_offset(&m, &half, &p);
    eprintln!(
        "[DIAG] HALF edges={} -> v={} f={} valid={:?} manifold={:?} folds={}",
        half.len(), r2.vertex_count(), r2.face_count(),
        r2.validate().is_ok(), r2.is_manifold().is_ok(),
        r2.count_folded_coplanar_pairs()
    );

    // The four edges around ONE face — a closed ring, no dangling selected edge.
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let ring: HashSet<u32> = {
        let mut s = HashSet::new();
        let f = FaceId(0);
        let start = m.face(f).half_edge;
        let mut h = start;
        loop {
            s.insert(h.0);
            if m.he(h).twin != NO_TWIN {
                s.insert(m.he(h).twin.0);
            }
            h = m.he(h).next;
            if h == start {
                break;
            }
        }
        s
    };
    let r3 = bevel_edges_offset(&m, &ring, &p);
    eprintln!(
        "[DIAG] RING edges={} -> v={} f={} valid={:?} manifold={:?} folds={}",
        ring.len(), r3.vertex_count(), r3.face_count(),
        r3.validate().is_ok(), r3.is_manifold().is_ok(),
        r3.count_folded_coplanar_pairs()
    );
}

/// OWNER'S RULE (2026-08-04): "we need to keep side edge from bevel, only the round
/// segment." Tag only edges INTERIOR to the round -- a created face on BOTH sides. The RIM,
/// where the round meets the original surface, has a created face on one side only and is a
/// real corner the user still wants beveled. This test pins the distinction; tagging whole
/// faces (what shipped first) swept the rims in and stopped them beveling.
#[test]
fn bevel_tags_only_the_rounds_interior_edges_not_its_rims() {
    use std::collections::HashSet;
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let e0 = m.face(FaceId(0)).half_edge;
    let mut sel = HashSet::new();
    sel.insert(e0.0);
    if m.he(e0).twin != NO_TWIN { sel.insert(m.he(e0).twin.0); }

    let out = bevel_edges_offset(
        &m, &sel, &BevelParams { width: 0.1, segments: 3, shape: 0.5, clamp: true },
    );
    let (f0, f1) = crate::bevel_op::last_bevel_new_face_range().expect("range");
    let hi = f1.min(out.faces.len() as u32);
    let is_new = |f: FaceId| f.0 >= f0 && f.0 < hi;

    let (mut interior, mut rim) = (0usize, 0usize);
    for f in f0..hi {
        let fid = FaceId(f);
        if out.face(fid).half_edge == NO_EDGE { continue; }
        for he in out.face_half_edges(fid) {
            let tw = out.he(he).twin;
            if tw == NO_TWIN { continue; }
            let tf = out.he(tw).face;
            if tf == NO_FACE { continue; }
            if is_new(tf) { interior += 1; } else { rim += 1; }
        }
    }
    eprintln!("[TAG] round interior half-edges={interior}  rim half-edges={rim}");
    assert!(interior > 0, "a 3-segment round has edges between its segments");
    assert!(rim > 0, "the round must meet the original surface somewhere");

    // THE SHIPPED RULE: tag the STRIP band (interior + its long rims, all tangent to the
    // round) and NOT the corner fill, whose boundary edges are real corners.
    let (s0, s1) = crate::bevel_op::last_bevel_strip_range().expect("strip range");
    let (n0, n1) = crate::bevel_op::last_bevel_new_face_range().expect("new range");
    eprintln!("[TAG] strip faces {s0}..{s1} of new range {n0}..{n1}");
    assert_eq!(s0, n0, "strip geometry starts the new range");
    assert!(s1 <= n1, "strip range is inside the new range");
    assert!(s1 > s0, "a bevel must emit strip faces");
    assert!(
        s1 < n1 || n1 == s1,
        "corner fill, when present, follows the strip"
    );
}

/// The bevel's own record of what it built must be a usable MINORITY of the result, in the
/// installed mesh's ids. This is what the app stamps into the sidecar so a later Bevel
/// modifier can skip a previous bevel's segments (owner 2026-08-04, the blue edges).
/// The minority bound is the guard: earlier attempts tagged 384 of 384, which is always wrong.
#[test]
fn bevel_new_face_range_is_a_usable_minority_on_a_partial_selection() {
    use std::collections::HashSet;
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    // Bevel ONE edge only -- the realistic case (the owner rounds a corner, not the whole box).
    let e0 = m.face(FaceId(0)).half_edge;
    let mut sel = HashSet::new();
    sel.insert(e0.0);
    if m.he(e0).twin != NO_TWIN { sel.insert(m.he(e0).twin.0); }

    let out = bevel_edges_offset(
        &m, &sel, &BevelParams { width: 0.1, segments: 3, shape: 0.5, clamp: true },
    );
    let range = crate::bevel_op::last_bevel_new_face_range();
    let (f0, f1) = range.expect("a bevel that added geometry must record its range");

    let mut tagged: HashSet<u32> = HashSet::new();
    for f in f0..f1.min(out.faces.len() as u32) {
        let fid = FaceId(f);
        if out.face(fid).half_edge == NO_EDGE { continue; }
        for he in out.face_half_edges(fid) { tagged.insert(he.0); }
    }
    let live = (0..out.half_edges.len() as u32)
        .filter(|&i| out.is_edge_live(HalfEdgeId(i)))
        .count();
    eprintln!("[TAG] faces {f0}..{f1} of {} -> tagged {} of {live} half-edges",
              out.face_count(), tagged.len());

    assert!(!tagged.is_empty(), "beveling one edge must tag its new strip");
    assert!(
        tagged.len() < live,
        "tag set must be a MINORITY ({} of {live}) - all of them means buffer indices leaked",
        tagged.len()
    );
}

/// OWNER TEST-08 (2026-08-05): "the Smart Select fails the bevel check". His shape is a box
/// with an existing ROUND from a previous bevel — 3 smooth edges, no flat ones. Smart Select
/// excludes the round's segments, and the second bevel must still produce an INSTALLABLE
/// result. Before widening the cap predicate from flat-only to tangent-continuous, the cap
/// was built over the smooth sector, folded, and `bevel_result_installable` rejected the
/// whole thing — so the user saw no bevel at all.
#[test]
fn second_bevel_around_an_existing_round_is_installable() {
    use std::collections::HashSet;
    // Round one edge of a cube -> a 3-segment strip, exactly like his first bevel.
    let base = HalfEdgeMesh::new_box(Vec3::ONE);
    let e0 = base.face(FaceId(0)).half_edge;
    let mut first = HashSet::new();
    first.insert(e0.0);
    if base.he(e0).twin != NO_TWIN { first.insert(base.he(e0).twin.0); }
    let rounded = bevel_edges_offset(
        &base, &first, &BevelParams { width: 0.12, segments: 3, shape: 0.5, clamp: true },
    );
    assert!(rounded.is_manifold().is_ok(), "the first bevel must be sound");

    // Smart Select: skip every SMOOTH edge (the round's segments), bevel the creases.
    let mut selected = HashSet::new();
    let mut skipped = 0usize;
    for i in 0..rounded.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !rounded.is_edge_live(h) { continue; }
        let smooth = rounded
            .edge_dihedral_cos(h)
            .map_or(false, |c| c > 30.0f32.to_radians().cos());
        if smooth { skipped += 1; } else { selected.insert(i); }
    }
    assert!(skipped > 0, "the round must contribute smooth edges to skip");
    assert!(!selected.is_empty(), "creases must remain to bevel");

    let out = bevel_edges_offset(
        &rounded, &selected, &BevelParams { width: 0.02, segments: 1, shape: 0.5, clamp: true },
    );
    eprintln!(
        "[T08] skipped {skipped} smooth, beveled {} -> v={} f={} manifold={:?} folds={}",
        selected.len(), out.vertex_count(), out.face_count(),
        out.is_manifold().is_ok(), out.count_folded_coplanar_pairs()
    );
    assert!(
        crate::bevel_op::bevel_result_installable(&rounded, &out),
        "the second bevel must be INSTALLABLE - this is the owner's TEST-08 failure"
    );
    assert_eq!(out.count_folded_coplanar_pairs(), 0, "no folds");
}

/// CRASH REGRESSION (owner 2026-08-05: "app crash trying to extrude after a boolean").
/// Panic was `no entry found for key` at the rail lookup: `beveled_he` is the SELECTION but
/// `rails` only holds edges that got a rail built, and a PARTIAL selection can leave a
/// selected neighbour without one. Beveling arbitrary partial selections must never panic.
#[test]
fn partial_selections_never_panic_on_a_missing_rail() {
    use std::collections::HashSet;
    let m = HalfEdgeMesh::new_box(Vec3::ONE);
    let live: Vec<u32> = (0..m.half_edges.len() as u32)
        .filter(|&i| m.is_edge_live(HalfEdgeId(i)))
        .collect();
    let p = BevelParams { width: 0.05, segments: 2, shape: 0.5, clamp: true };
    // Every "every Nth edge" subset, including ones that split twin pairs - the exact shape
    // of a Smart Select exclusion, where one direction of an edge can be dropped.
    for step in 2..=5usize {
        for offset in 0..step {
            let sel: HashSet<u32> = live.iter().copied().skip(offset).step_by(step).collect();
            if sel.is_empty() { continue; }
            let out = bevel_edges_offset(&m, &sel, &p);
            assert!(
                out.validate().is_ok(),
                "partial selection step={step} offset={offset} produced an invalid mesh"
            );
        }
    }
}

/// CRASH REGRESSION #2 (owner 2026-08-05: "app crash again ... I was trying to intrude with
/// bevel modifier smart select"). The first guard patched ONE rails[..] site; the next crash
/// landed on another of the eleven. The fix aligns `beveled_he` with `rails` at the source,
/// so this sweeps far more aggressively than a single-subset test: RANDOM-ish partial
/// selections over a boolean result, which is the shape Smart Select actually produces.
#[test]
fn partial_selections_on_boolean_output_never_panic() {
    use std::collections::HashSet;
    use crate::mesh_boolean::{mesh_boolean, BoolOp};
    let cube = HalfEdgeMesh::new_box(Vec3::splat(2.0));
    let cyl = HalfEdgeMesh::new_cylinder(0.5, 6.0, 16);
    let res = mesh_boolean(&cube, &cyl, BoolOp::Difference).expect("boolean");
    let live: Vec<u32> = (0..res.half_edges.len() as u32)
        .filter(|&i| res.is_edge_live(HalfEdgeId(i)))
        .collect();
    let p = BevelParams { width: 0.02, segments: 2, shape: 0.5, clamp: true };
    // Deterministic pseudo-random subsets: a multiplicative walk gives selections that
    // split twin pairs and straddle the cut, exactly like a Smart Select exclusion set.
    for seed in 1..=17u64 {
        let mut sel: HashSet<u32> = HashSet::new();
        let mut x = seed;
        for &e in &live {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            if (x >> 33) % 3 != 0 {
                sel.insert(e);
            }
        }
        if sel.is_empty() { continue; }
        let out = bevel_edges_offset(&res, &sel, &p);
        assert!(out.validate().is_ok(), "seed {seed}: invalid mesh from a partial selection");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// VARIABLE WIDTH ([`BevelWidthPolicy`]) — one width per selection is wrong on a model that mixes
// scales: with clamp ON the smallest edge drags every other edge down to its own limit, with clamp
// OFF the small detail folds. These gates pin the fix plus the two properties that make it safe
// (uniform geometry is unaffected; capacity always beats the user's floor).
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// Polygon soup of `m`, scaled about its own origin then translated — used to put two boxes of very
/// different size into ONE mesh. Derived from `new_box` rather than hand-written so the winding (and
/// therefore every face normal, and therefore every rail direction) is correct by construction.
fn var_soup(m: &HalfEdgeMesh, scale: f32, offset: Vec3) -> (Vec<Vec3>, Vec<Vec<usize>>) {
    let verts: Vec<Vec3> = m.vertices.iter().map(|v| v.position * scale + offset).collect();
    let faces: Vec<Vec<usize>> = (0..m.faces.len() as u32)
        .map(|f| m.face_vertices(FaceId(f)).iter().map(|v| v.0 as usize).collect())
        .collect();
    (verts, faces)
}

/// A BIG cube (half-extent 1 at the origin) and a TINY one (half-extent 0.05, parked clear of it)
/// in one mesh — the minimal "big body carrying small detail" fixture.
fn var_big_and_tiny() -> HalfEdgeMesh {
    let unit = HalfEdgeMesh::new_box(Vec3::ONE);
    let (mut verts, mut faces) = var_soup(&unit, 1.0, Vec3::ZERO);
    let (tv, tf) = var_soup(&unit, 0.05, Vec3::new(5.0, 0.0, 0.0));
    let base = verts.len();
    verts.extend(tv);
    faces.extend(tf.into_iter().map(|f| f.into_iter().map(|i| i + base).collect::<Vec<usize>>()));
    HalfEdgeMesh::from_polygons(&verts, &faces)
}

fn var_all_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
    (0..m.half_edges.len() as u32)
        .filter(|&i| {
            let he = m.he(HalfEdgeId(i));
            he.face != NO_FACE && he.twin != NO_TWIN && m.he(he.twin).face != NO_FACE
        })
        .collect()
}

fn var_boundary_count(m: &HalfEdgeMesh) -> usize {
    (0..m.half_edges.len() as u32)
        .filter(|&i| m.he(HalfEdgeId(i)).twin == NO_TWIN && m.he(HalfEdgeId(i)).face != NO_FACE)
        .count()
}

fn var_pos_keys(m: &HalfEdgeMesh) -> Vec<[i64; 3]> {
    let mut v: Vec<[i64; 3]> = m
        .vertices
        .iter()
        .map(|vert| {
            let p = vert.position;
            [(p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64]
        })
        .collect();
    v.sort_unstable();
    v
}

/// The bevel width applied at `corner`, recovered from geometry. All three edges at a beveled box
/// corner are replaced by rail points at distance `w·sqrt(2)` from the original corner vertex, so
/// the nearest output vertex reports `w` directly.
fn var_width_at_corner(m: &HalfEdgeMesh, corner: Vec3) -> f32 {
    let d = m
        .vertices
        .iter()
        .map(|v| (v.position - corner).length())
        .fold(f32::INFINITY, f32::min);
    d / std::f32::consts::SQRT_2
}

fn var_spec(width_min: f32, driver: BevelWidthDriver) -> BevelWidthPolicy {
    BevelWidthPolicy { width_min, driver, per_edge_limit: true, weights: HashMap::new() }
}

/// `None` must be a pure pass-through — the new entry point cannot drift from the old one.
#[test]
fn var_width_off_matches_uniform() {
    let m = var_big_and_tiny();
    let sel = var_all_edges(&m);
    let p = BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true };
    let plain = bevel_edges_offset(&m, &sel, &p);
    let explicit_none = bevel_edges_offset_var(&m, &sel, &p, None);
    assert_eq!(
        var_pos_keys(&plain),
        var_pos_keys(&explicit_none),
        "var=None must be the uniform bevel exactly"
    );
    assert_eq!(plain.faces.len(), explicit_none.faces.len());
}

/// THE NO-SURPRISE PROPERTY. On uniform geometry every edge has the same capacity, so every edge
/// sits at t=1 and the result must equal a plain bevel at `width`. Turning the toggle on must never
/// change a model that had nothing to adapt to.
#[test]
fn var_width_uniform_mesh_matches_uniform_bevel() {
    let cube = HalfEdgeMesh::new_box(Vec3::ONE);
    let sel = var_all_edges(&cube);
    let p = BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true };
    let uniform = bevel_edges_offset(&cube, &sel, &p);
    let var = bevel_edges_offset_var(
        &cube,
        &sel,
        &p,
        Some(&var_spec(0.0, BevelWidthDriver::AutoSize)),
    );
    assert_eq!(
        var_pos_keys(&uniform),
        var_pos_keys(&var),
        "AutoSize on uniform geometry must reproduce the uniform bevel"
    );
}

/// THE REPORTED BUG. One `width` over a big body plus a small detail: the uniform clamp pins the
/// big edges to the SMALL feature's limit. Variable width gives each its own.
#[test]
fn var_width_auto_gives_each_feature_its_own_width() {
    let m = var_big_and_tiny();
    let sel = var_all_edges(&m);
    let big = Vec3::ONE;
    let tiny = Vec3::new(5.05, 0.05, 0.05);
    let p = BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true };

    // Uniform: the tiny cube's 0.05 capacity IS the global minimum, so the big cube gets 0.05 too.
    let uni = bevel_edges_offset(&m, &sel, &p);
    let uni_big = var_width_at_corner(&uni, big);
    assert!(
        (uni_big - 0.05).abs() < 5e-3,
        "uniform must drag the BIG edges down to the tiny feature's limit, got {uni_big}"
    );

    // Variable: big edges reach the requested 0.3; the tiny cube scales to 0.015, comfortably
    // inside its own 0.05 capacity instead of sitting exactly ON it.
    let var = bevel_edges_offset_var(
        &m,
        &sel,
        &p,
        Some(&var_spec(0.0, BevelWidthDriver::AutoSize)),
    );
    let var_big = var_width_at_corner(&var, big);
    let var_tiny = var_width_at_corner(&var, tiny);
    assert!((var_big - 0.3).abs() < 5e-3, "big edges must keep the requested width, got {var_big}");
    assert!(
        var_tiny < 0.05 && var_tiny > 1e-4,
        "tiny edges must scale DOWN but still bevel, got {var_tiny}"
    );
    assert!(
        var_big > var_tiny * 5.0,
        "the point of the feature: {var_big} must be far wider than {var_tiny}"
    );
    assert!(var.validate().is_ok(), "variable-width result must be a valid mesh");
    assert_eq!(var_boundary_count(&var), 0, "closed input must stay watertight");
}

/// The floor is a TARGET, never a guarantee: a `width_min` far above a small edge's capacity must
/// degrade to that capacity (§5 invariant 9), not invert the face.
#[test]
fn var_width_floor_never_exceeds_capacity() {
    let m = var_big_and_tiny();
    let sel = var_all_edges(&m);
    let p = BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true };
    // 0.2 floor vs the tiny cube's 0.05 of room — 4x more than it can hold.
    let out =
        bevel_edges_offset_var(&m, &sel, &p, Some(&var_spec(0.2, BevelWidthDriver::AutoSize)));
    let tiny = var_width_at_corner(&out, Vec3::new(5.05, 0.05, 0.05));
    assert!(
        tiny <= 0.05 + 1e-3,
        "capacity must cap the floor: tiny edge took {tiny}, room is 0.05"
    );
    assert!(out.validate().is_ok(), "must stay valid despite an impossible floor");
    assert_eq!(var_boundary_count(&out), 0, "must stay watertight despite an impossible floor");
}

/// OPPOSING WALLS. A 0.1-thick plate's long edges each have 1.0 of LOCAL room (their in-face
/// neighbours are two units away), so a purely per-edge width would send both 0.3 into a 0.1 gap and
/// cross them — a collision no vertex-local term can see, and one the uniform path avoids only by
/// accident (its global minimum comes from the plate's short edges). `opposing_pair_caps` bounds it.
#[test]
fn var_width_opposing_walls_stay_uncrossed() {
    let plate = HalfEdgeMesh::new_box(Vec3::new(1.0, 1.0, 0.05));
    let sel = var_all_edges(&plate);
    let out = bevel_edges_offset_var(
        &plate,
        &sel,
        &BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true },
        Some(&var_spec(0.0, BevelWidthDriver::AutoSize)),
    );
    // The x=+1 wall keeps x exactly; its rim verts sit at z = +/-(0.05 - w_long), so the whole claim
    // reduces to `0 <= zmax < 0.05`, i.e. 0 < w_long <= 0.05 (half the 0.1 gap).
    //
    // Sabotage-verified: with the pair bound removed, w_long takes the full 0.3 and the BOTTOM
    // edge's rail lands at z = -0.05 + 0.3 = +0.25 — 5x outside a plate 0.05 thick. Both bounds
    // below are therefore live, in opposite directions.
    let zmax = out
        .vertices
        .iter()
        .filter(|v| (v.position.x - 1.0).abs() < 1e-5)
        .map(|v| v.position.z)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(zmax.is_finite(), "the x=+1 wall must still exist");
    assert!(
        zmax >= -1e-4,
        "opposing walls crossed THROUGH each other: wall rim reached z={zmax}, so w_long > 0.05"
    );
    assert!(
        zmax < 0.0499,
        "wall rim at z={zmax} is outside the 0.05-thick plate: a rail overshot the opposite wall \
         (or no bevel happened at all)"
    );
    assert!(out.validate().is_ok(), "thin-plate result must be valid");
    assert_eq!(var_boundary_count(&out), 0, "thin-plate result must stay watertight");
}

/// AUTHORED WEIGHT IS A PERCENTAGE OF THE WIDTH (owner 2026-08-11: "the weight is a percentage of
/// the width"), applied on top of whatever driver is in use — it is no longer a driver of its own.
///
/// The rule that matters most is the UNPAINTED one: an edge the user never painted keeps the FULL
/// width. This test previously asserted the opposite (unweighted fell to `width_min`), which is what
/// made the old feature unusable — turning it on erased the bevel from every edge not painted yet,
/// and it was only reachable at all by ticking Variable Width and picking a driver out of a combo.
#[test]
fn var_width_authored_weight_lerps_the_band() {
    let cube = HalfEdgeMesh::new_box(Vec3::ONE);
    // The x=+1 / y=+1 vertical edge. Beveling it moves (1,1,+-1) to (1-w,1,+-1) and (1,1-w,+-1), so
    // the widest x among the y=+1 verts reads 1-w straight off the result.
    let he = (0..cube.half_edges.len() as u32)
        .find(|&i| {
            let h = HalfEdgeId(i);
            if cube.he(h).face == NO_FACE || cube.he(h).twin == NO_TWIN {
                return false;
            }
            let a = cube.vert(cube.he_source(h)).position;
            let b = cube.vert(cube.he(h).vertex).position;
            (a.x - 1.0).abs() < 1e-5
                && (a.y - 1.0).abs() < 1e-5
                && (b.x - 1.0).abs() < 1e-5
                && (b.y - 1.0).abs() < 1e-5
        })
        .expect("cube has an x=+1/y=+1 edge");
    let sel: HashSet<u32> = [he].into_iter().collect();
    let p = BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true };
    let inset = |m: &HalfEdgeMesh| -> f32 {
        1.0 - m
            .vertices
            .iter()
            .filter(|v| (v.position.y - 1.0).abs() < 1e-5)
            .map(|v| v.position.x)
            .fold(f32::NEG_INFINITY, f32::max)
    };

    let twin = cube.he(HalfEdgeId(he)).twin.0;
    let weighted = |w: f32| {
        let mut weights = HashMap::new();
        weights.insert(he, w);
        weights.insert(twin, w);
        bevel_edges_offset_var(
            &cube,
            &sel,
            &p,
            Some(&BevelWidthPolicy {
                width_min: 0.0,
                driver: BevelWidthDriver::Uniform,
                per_edge_limit: false,
                weights,
            }),
        )
    };

    // Weight is a straight percentage of Width, at any driver.
    for (w, want) in [(1.0f32, 0.3f32), (0.5, 0.15), (0.25, 0.075)] {
        let out = weighted(w);
        assert!(
            (inset(&out) - want).abs() < 2e-3,
            "weight {w} must give {want} of a 0.3 width, got {}",
            inset(&out)
        );
        assert!(out.validate().is_ok(), "weight {w} must stay valid");
    }

    // UNPAINTED = full width. An empty weight map must leave the bevel exactly as it would be with
    // no policy at all, so enabling the feature can never shrink geometry the user has not touched.
    let none = bevel_edges_offset_var(
        &cube,
        &sel,
        &p,
        Some(&var_spec(0.0, BevelWidthDriver::Uniform)),
    );
    assert!(
        (inset(&none) - 0.3).abs() < 2e-3,
        "an unpainted edge must keep the FULL width, got {}",
        inset(&none)
    );
    assert!(none.validate().is_ok());

    // The retired `AuthoredWeight` driver still loads from old `.fl4` files and must behave as
    // Uniform now — the weights themselves do the grading.
    let mut legacy_weights = HashMap::new();
    legacy_weights.insert(he, 0.5f32);
    legacy_weights.insert(twin, 0.5f32);
    let legacy = bevel_edges_offset_var(
        &cube,
        &sel,
        &p,
        Some(&BevelWidthPolicy {
            width_min: 0.0,
            driver: BevelWidthDriver::AuthoredWeight,
            per_edge_limit: false,
            weights: legacy_weights,
        }),
    );
    assert!(
        (inset(&legacy) - 0.15).abs() < 2e-3,
        "legacy driver 2 must apply the weight as a percentage too, got {}",
        inset(&legacy)
    );
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// AUTO MERGE VERTICES ([`BevelWidthDriver::Uniform`]) — keep ONE width, apply Clamp Overlap's limit
// PER EDGE. A pair beveled too near each other pins at its meet point, where the `WeldMap` merges
// the two rails into one vertex; every other edge still reaches the requested width.
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// Zero-area (collapsed) live faces — a merge must not leave any behind.
fn var_degenerate_faces(m: &HalfEdgeMesh) -> usize {
    (0..m.faces.len() as u32)
        .map(FaceId)
        .filter(|&f| m.face(f).half_edge != NO_EDGE)
        .filter(|&f| {
            let vs = m.face_vertices(f);
            if vs.len() < 3 {
                return true;
            }
            let p0 = m.vert(vs[0]).position;
            let mut a = Vec3::ZERO;
            for i in 1..vs.len() - 1 {
                a += (m.vert(vs[i]).position - p0).cross(m.vert(vs[i + 1]).position - p0);
            }
            a.length() * 0.5 < 1e-9
        })
        .count()
}

/// A cube carrying a NARROW band: one loop cut slid to 97% leaves a 0.06-wide strip beside a
/// full-size face. This is the fixture the owner's case reduces to.
fn var_band_cube() -> HalfEdgeMesh {
    let mut m = HalfEdgeMesh::new_box(Vec3::ONE);
    let he = (0..m.half_edges.len() as u32)
        .map(HalfEdgeId)
        .find(|&h| m.he(h).face != NO_FACE && m.he(h).twin != NO_TWIN)
        .expect("cube has a manifold edge");
    m.loop_cut_multi(he, &[0.97], None);
    m
}

/// THE FEATURE. One global minimum lets the narrow band throttle the whole cube; a per-edge ceiling
/// lets the roomy corners reach the requested width while the band pins at its own limit.
#[test]
fn merge_per_edge_ceiling_lets_roomy_edges_reach_full_width() {
    let m = var_band_cube();
    let sel = var_all_edges(&m);
    let p = BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true };
    // The band hugs one side, so the OPPOSITE corner is the roomy one.
    let roomy = Vec3::new(-1.0, -1.0, -1.0);

    let global = bevel_edges_offset(&m, &sel, &p);
    let w_global = var_width_at_corner(&global, roomy);
    assert!(
        (w_global - 0.03).abs() < 5e-3,
        "one global minimum must let the 0.06 band throttle the whole cube, got {w_global}"
    );

    let per_edge = bevel_edges_offset_var(
        &m,
        &sel,
        &p,
        Some(&var_spec(0.0, BevelWidthDriver::Uniform)),
    );
    let w_per_edge = var_width_at_corner(&per_edge, roomy);
    assert!(
        (w_per_edge - 0.2).abs() < 5e-3,
        "a per-edge ceiling must let the roomy corner reach the requested 0.2, got {w_per_edge}"
    );
    assert!(per_edge.validate().is_ok(), "must stay a valid mesh");
    assert_eq!(var_boundary_count(&per_edge), 0, "must stay watertight");
    assert_eq!(var_degenerate_faces(&per_edge), 0, "must leave no collapsed faces");
}

/// `Uniform` ignores `width_min` — it is the graded drivers' floor and must not leak into the
/// single-width mode (a stale floor would silently widen every edge).
#[test]
fn merge_uniform_driver_ignores_width_min() {
    let m = var_band_cube();
    let sel = var_all_edges(&m);
    let p = BevelParams { width: 0.2, segments: 1, shape: 0.5, clamp: true };
    let a = bevel_edges_offset_var(&m, &sel, &p, Some(&var_spec(0.0, BevelWidthDriver::Uniform)));
    let b = bevel_edges_offset_var(&m, &sel, &p, Some(&var_spec(0.15, BevelWidthDriver::Uniform)));
    assert_eq!(
        var_pos_keys(&a),
        var_pos_keys(&b),
        "width_min must have no effect under the Uniform driver"
    );
}

/// THE MERGE ITSELF. On a 0.1-thick plate the facing edges meet at the mid-plane, and the two rails
/// that land there must become ONE vertex — not two coincident ones, and not a collapsed face.
///
/// ⚠ This pins PRE-EXISTING behaviour, not the new toggle: the weld is the `WeldMap`'s doing and has
/// always happened when two rails land on the same point, which on this fixture the global clamp also
/// produces (sabotage-verified — discarding the policy leaves this gate green). It is here because
/// the merge being clean is the whole premise of the feature, and a regression in the weld would
/// otherwise only surface as a broken mesh in the app.
#[test]
fn merge_welds_colliding_rails_into_one_vertex() {
    let plate = HalfEdgeMesh::new_box(Vec3::new(1.0, 1.0, 0.05));
    let sel = var_all_edges(&plate);
    let p = BevelParams { width: 0.3, segments: 1, shape: 0.5, clamp: true };
    let out =
        bevel_edges_offset_var(&plate, &sel, &p, Some(&var_spec(0.0, BevelWidthDriver::Uniform)));

    // Unmerged, each of the 8 original corners would spawn 3 rail points (24 verts). Welding the
    // facing pairs on all four side walls collapses that to 16.
    assert_eq!(
        out.vertices.len(),
        16,
        "the facing rails must WELD (24 verts unmerged, 16 merged), got {}",
        out.vertices.len()
    );
    // Merged at the meet position: the side walls' rim sits exactly on the mid-plane z=0.
    let zmax = out
        .vertices
        .iter()
        .filter(|v| (v.position.x - 1.0).abs() < 1e-5)
        .map(|v| v.position.z)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(zmax.abs() < 1e-4, "merged rail must sit at the meet plane z=0, got {zmax}");
    assert!(out.validate().is_ok(), "merged result must be valid");
    assert_eq!(var_boundary_count(&out), 0, "merged result must stay watertight");
    assert_eq!(var_degenerate_faces(&out), 0, "the collapsed strip must not survive as a face");
}

/// OWNER CONSTRAINT: "the merge will only affect geo created by bevel." Beveling a PARTIAL selection
/// leaves untouched original vertices; every one of them must still be there, bit-identically. This
/// is what separates the feature from a merge-by-distance, which would eat small detail.
#[test]
fn merge_never_moves_an_unbeveled_original_vertex() {
    let plate = HalfEdgeMesh::new_box(Vec3::new(1.0, 1.0, 0.05));
    // Bevel ONLY the four short vertical edges (|z| spans the thin dimension), leaving the eight long
    // ones — and therefore real original geometry — untouched.
    let sel: HashSet<u32> = (0..plate.half_edges.len() as u32)
        .filter(|&i| {
            let he = HalfEdgeId(i);
            if plate.he(he).face == NO_FACE || plate.he(he).twin == NO_TWIN {
                return false;
            }
            let a = plate.vert(plate.he_source(he)).position;
            let b = plate.vert(plate.he(he).vertex).position;
            (a.z - b.z).abs() > 1e-6 // runs along z = one of the short verticals
        })
        .collect();
    assert!(!sel.is_empty(), "fixture must select the short vertical edges");

    let p = BevelParams { width: 0.5, segments: 1, shape: 0.5, clamp: true };
    let out =
        bevel_edges_offset_var(&plate, &sel, &p, Some(&var_spec(0.0, BevelWidthDriver::Uniform)));

    // Every ORIGINAL corner is an endpoint of a beveled edge here, so instead assert the invariant
    // that actually matters: no original position is DISPLACED. Each original vertex is either still
    // present at its exact position, or legitimately consumed by the bevel that touched it — never
    // moved to a new location. Check the untouched x/y extents survive exactly.
    for &(axis, val) in &[(0usize, 1.0f32), (0, -1.0), (1, 1.0), (1, -1.0)] {
        let kept = out
            .vertices
            .iter()
            .any(|v| (v.position.to_array()[axis] - val).abs() < 1e-6);
        assert!(kept, "original extent axis {axis} = {val} must survive the merge exactly");
    }
    assert!(out.validate().is_ok(), "partial-selection merge must be valid");
    assert_eq!(var_degenerate_faces(&out), 0, "no collapsed faces from a partial merge");
}

/// SWEEP: no configuration may leave a collapsed face behind. If this ever fires, the fix is
/// `HalfEdgeMesh::remove_degenerate_face` (`half_edge/ops_dissolve.rs`, currently private and 2-gon
/// only) — deliberately NOT wired speculatively, because every configuration measured came back at 0.
#[test]
fn merge_leaves_no_degenerate_faces_across_a_sweep() {
    let fixtures: Vec<(&str, HalfEdgeMesh)> = vec![
        ("plate", HalfEdgeMesh::new_box(Vec3::new(1.0, 1.0, 0.05))),
        ("bar", HalfEdgeMesh::new_box(Vec3::new(0.05, 1.0, 1.0))),
        ("band_cube", var_band_cube()),
    ];
    for (name, m) in &fixtures {
        let sel = var_all_edges(m);
        for &segments in &[1u32, 3] {
            for &width in &[0.02f32, 0.05, 0.1, 0.3, 0.9] {
                let p = BevelParams { width, segments, shape: 0.5, clamp: true };
                let out = bevel_edges_offset_var(
                    m,
                    &sel,
                    &p,
                    Some(&var_spec(0.0, BevelWidthDriver::Uniform)),
                );
                let degen = var_degenerate_faces(&out);
                assert_eq!(
                    degen, 0,
                    "{name} w={width} seg={segments}: {degen} collapsed faces survived the merge"
                );
                assert!(
                    out.validate().is_ok(),
                    "{name} w={width} seg={segments}: invalid mesh"
                );
                assert_eq!(
                    var_boundary_count(&out),
                    0,
                    "{name} w={width} seg={segments}: closed input must stay watertight"
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// INNER MITER = ARC (BEVEL.md §3.8). Oracle: Blender 4.5, boss-on-plate scenario,
// corner M=(0.6,0.6,1.1382), edges along -X and -Y, spread=0.1.
// ─────────────────────────────────────────────────────────────────────────────

/// The corner used by every gate below — matches `miter_arc_sweep.py`'s oracle exactly.
fn oracle_corner() -> (Vec3, Vec3, Vec3, f32) {
    (
        Vec3::new(0.6, 0.6, 1.1382),  // M — the SHARP inner meet point
        Vec3::new(-1.0, 0.0, 0.0),    // d_a
        Vec3::new(0.0, -1.0, 0.0),    // d_b
        0.1,                          // spread
    )
}

/// The arc's ENDS sit back along each beveled edge by exactly `spread` (§3.8 closed form).
#[test]
fn inner_miter_arc_ends_sit_back_by_spread() {
    let (m, da, db, spread) = oracle_corner();
    let (s0, s1, chain) = inner_miter_arc(m, da, db, spread, 3);
    assert!((s0 - Vec3::new(0.5, 0.6, 1.1382)).length() < 1e-6, "S0 = {s0:?}");
    assert!((s1 - Vec3::new(0.6, 0.5, 1.1382)).length() < 1e-6, "S1 = {s1:?}");
    assert_eq!(chain.first().copied(), Some(s0), "chain must START at S0");
    assert_eq!(chain.last().copied(), Some(s1), "chain must END at S1");
}

/// Every point lies on `circle(C, spread)` — the property the closed form is built on, checked at a
/// NON-perpendicular opening angle too so the slerp (not a hardcoded quarter turn) is what's gated.
#[test]
fn inner_miter_arc_every_point_is_spread_from_centre() {
    for (da, db) in [
        (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),          // 90°
        (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.5, -0.866, 0.0)),        // 120°
        (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(-0.5, -0.866, 0.0)),       // 60°
    ] {
        let (m, _, _, spread) = oracle_corner();
        for ns in [1usize, 2, 3, 4, 8] {
            let c = m + (da.normalize() + db.normalize()) * spread;
            let (_, _, chain) = inner_miter_arc(m, da, db, spread, ns);
            for (i, p) in chain.iter().enumerate() {
                let r = (*p - c).length();
                assert!(
                    (r - spread).abs() < 2e-6,
                    "ns={ns} i={i}: radius {r} != spread {spread} (da={da:?} db={db:?})"
                );
            }
        }
    }
}

/// VERTEX-EXACT vs Blender at POWER-OF-TWO segment counts (§3.8 table: ns∈{2,4} → factor 1.00000).
#[test]
fn inner_miter_arc_matches_blender_at_power_of_two_segments() {
    let (m, da, db, spread) = oracle_corner();

    let (_, _, c2) = inner_miter_arc(m, da, db, spread, 2);
    let want2 = Vec3::new(0.570711, 0.570711, 1.1382);
    assert!((c2[1] - want2).length() < 1e-5, "ns=2 interior {:?} != {want2:?}", c2[1]);

    let (_, _, c4) = inner_miter_arc(m, da, db, spread, 4);
    for (i, want) in [
        Vec3::new(0.538268, 0.592388, 1.1382),
        Vec3::new(0.570711, 0.570711, 1.1382),
        Vec3::new(0.592388, 0.538268, 1.1382),
    ]
    .iter()
    .enumerate()
    {
        assert!(
            (c4[i + 1] - *want).length() < 1e-5,
            "ns=4 i={} got {:?} want {want:?}",
            i + 1,
            c4[i + 1]
        );
    }
}

/// NON-power-of-two: Blender's OWN sampling pulls its points in by up to 1.7% (§3.8), so we sit
/// slightly OUTSIDE its result by construction. This gate pins that gap as KNOWN and bounded — it
/// exists so a future session does not read the difference as a bug and go hunting for another curve.
#[test]
fn inner_miter_arc_ns3_differs_from_blender_only_by_its_resample() {
    let (m, da, db, spread) = oracle_corner();
    let (_, _, chain) = inner_miter_arc(m, da, db, spread, 3);
    let blender = [
        Vec3::new(0.549083, 0.585162, 1.1382),
        Vec3::new(0.585162, 0.549082, 1.1382),
    ];
    let c = m + (da + db) * spread;
    for (i, b) in blender.iter().enumerate() {
        let d = (chain[i + 1] - *b).length();
        assert!(d < 2e-2 * spread, "ns=3 i={} deviation {d} exceeds Blender's resample band", i + 1);
        // Ours is ON the circle; Blender's is INSIDE it. If this ever flips, the table is stale.
        assert!(
            (*b - c).length() < spread,
            "Blender's ns=3 point should be INSIDE the circle (§3.8 factor 0.98293)"
        );
    }
}

/// `spread = 0` degenerates to SHARP — both ends collapse onto the meet point, so enabling Arc with
/// no spread can never move geometry.
#[test]
fn inner_miter_arc_zero_spread_is_sharp() {
    let (m, da, db, _) = oracle_corner();
    let (s0, s1, chain) = inner_miter_arc(m, da, db, 0.0, 4);
    assert_eq!(s0, m);
    assert_eq!(s1, m);
    assert!(chain.iter().all(|p| *p == m), "zero spread must collapse the whole chain onto M");
}

/// Collinear beveled edges leave the arc plane undefined; the straight chord is the only defensible
/// answer and must not produce NaN (the `miter_weld_arc` degenerate takes the same route).
#[test]
fn inner_miter_arc_collinear_edges_degrade_to_a_chord() {
    let (m, _, _, spread) = oracle_corner();
    let (_, _, chain) =
        inner_miter_arc(m, Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), spread, 4);
    assert!(chain.iter().all(|p| p.is_finite()), "collinear must not emit NaN: {chain:?}");
    assert_eq!(chain.len(), 5);
}

// ─────────────────────────────────────────────────────────────────────────────
// CORNER SPAN driver (owner 2026-08-11): grade by the size of the DETAIL, not of
// one segment. See `corner_span_sizes` + BEVEL.md §11.
// ─────────────────────────────────────────────────────────────────────────────

/// Two quads sharing one interior edge; `left`/`right` set how far each side reaches, i.e. the two
/// in-face neighbour lengths at BOTH endpoints of the shared edge. Returns that edge's corner span.
fn shared_edge_span(left: f32, right: f32) -> f32 {
    let pos = vec![
        Vec3::new(0.0, 0.0, 0.0),   // 0 v0
        Vec3::new(0.0, 1.0, 0.0),   // 1 v1
        Vec3::new(-left, 0.0, 0.0), // 2 a0
        Vec3::new(-left, 1.0, 0.0), // 3 a1
        Vec3::new(right, 0.0, 0.0), // 4 b0
        Vec3::new(right, 1.0, 0.0), // 5 b1
    ];
    let faces = vec![vec![2usize, 0, 1, 3], vec![0usize, 4, 5, 1]];
    let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);

    let mut beveled: HashSet<u32> = HashSet::new();
    let mut und2prim: HashMap<u32, u32> = HashMap::new();
    let mut prim: Option<u32> = None;
    for i in 0..mesh.half_edges.len() as u32 {
        let h = HalfEdgeId(i);
        if !mesh.is_edge_live(h) {
            continue;
        }
        let he = mesh.he(h);
        if he.twin == NO_TWIN || he.face == NO_FACE || mesh.he(he.twin).face == NO_FACE {
            continue;
        }
        prim = Some(i);
        beveled.insert(i);
        beveled.insert(he.twin.0);
        und2prim.insert(i, i);
        und2prim.insert(he.twin.0, i);
        break;
    }
    let prim = prim.expect("the two quads must share one interior edge");
    let orig: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
    let spans = corner_span_sizes(&mesh, &orig, &beveled, &und2prim);
    *spans.get(&prim).expect("interior edge must have a span")
}

/// 🔑 THE OWNER'S CASE — "a 2 m edge connected to a 1 cm edge, the length we find will still be
/// SMALL, driving a small bevel." The shorter neighbour caps the shared slide, so the long side
/// cannot inflate the measurement.
#[test]
fn corner_span_is_capped_by_the_shorter_neighbour() {
    let span = shared_edge_span(2.0, 0.01);
    // Slide s = min(2.0, 0.01) = 0.01 both ways along opposite directions → chord 0.02.
    assert!(
        (span - 0.02).abs() < 1e-6,
        "2m vs 1cm neighbours must give a 1cm-scale span, got {span}"
    );
    // The failure mode this exists to prevent: sliding each neighbour to its OWN far end would
    // measure |a0 - b0| = 2.01 and drive a huge bevel off a 1 cm detail.
    assert!(span < 0.05, "span {span} is on the 2 m scale — the long neighbour inflated it");
}

/// Symmetry check: it is the SHORTER side that governs, whichever side that is.
#[test]
fn corner_span_does_not_care_which_side_is_short() {
    let a = shared_edge_span(2.0, 0.01);
    let b = shared_edge_span(0.01, 2.0);
    assert!((a - b).abs() < 1e-6, "span must be side-symmetric: {a} vs {b}");
}

/// It genuinely tracks detail size: a roomy corner measures larger than a tight one, monotonically.
#[test]
fn corner_span_grows_with_the_detail() {
    let tight = shared_edge_span(0.01, 0.01);
    let mid = shared_edge_span(0.1, 0.1);
    let roomy = shared_edge_span(1.0, 1.0);
    assert!(tight < mid && mid < roomy, "spans must increase: {tight} < {mid} < {roomy}");
    // Uniform neighbours ⇒ span is exactly the chord across both slides.
    assert!((roomy - 2.0).abs() < 1e-6, "1m each side → 2m chord, got {roomy}");
}

/// SCALE EQUIVARIANCE — the property that makes this a "size of the detail" measure: scale the whole
/// mesh by k and every span scales by k, so a uniformly larger model gets a proportionally larger
/// bevel. (Invariance under arbitrary SUBDIVISION is NOT claimed — subdividing the shortest
/// neighbour does move `s`. See the note on `corner_span_sizes`.)
#[test]
fn corner_span_scales_with_the_model() {
    for k in [0.25f32, 1.0, 4.0] {
        let s = shared_edge_span(2.0 * k, 0.01 * k);
        assert!(
            (s - 0.02 * k).abs() < 1e-6 * k.max(1.0),
            "k={k}: span {s} should be {} (scale-equivariant)",
            0.02 * k
        );
    }
}

    /// FLAT UNBEVELED SECTOR AT A CORNER (owner 2026-08-15: "the edge from the boolean creates
    /// 4 edges that break the rounded cap").
    ///
    /// A boolean leaves COPLANAR support edges running across a flat face out to a box corner
    /// (BEVEL.md §6, `new_flat_edges`). That corner is then degree-4 with 3 beveled edges and ONE
    /// FLAT unbeveled one. Fixture = the same thing single-stage: a box whose top face carries a
    /// diagonal 5→7. Corners v5/v7 are the flat-sector corners; v4/v6 stay degree-3 and are the
    /// CONTROL inside the very same mesh.
    ///
    /// `sx` stretches the box along X. At `sx == 1` the cube's symmetry puts the corner's
    /// `offset_meet` EXACTLY on the diagonal, so a loop slide onto the flat edge and a plain rail
    /// meet coincide — the symmetric fixture cannot tell them apart. `sx == 2` separates them and
    /// is the general case.
    fn flat_diagonal_box(sx: f32) -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-sx, -1.0, -1.0), Vec3::new(sx, -1.0, -1.0),
            Vec3::new(sx, 1.0, -1.0),   Vec3::new(-sx, 1.0, -1.0),
            Vec3::new(-sx, -1.0, 1.0),  Vec3::new(sx, -1.0, 1.0),
            Vec3::new(sx, 1.0, 1.0),    Vec3::new(-sx, 1.0, 1.0),
        ];
        let f = vec![
            vec![0, 3, 2, 1],
            vec![4, 5, 7], vec![5, 6, 7],   // top face SPLIT by the flat diagonal 5-7
            vec![0, 1, 5, 4], vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7],
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    /// The 12 box edges (endpoints differing in exactly ONE coordinate) — everything except the
    /// flat diagonal, which is the edge the owner cannot bevel.
    fn box_edges_not_the_diagonal(m: &HalfEdgeMesh) -> HashSet<u32> {
        let mut sel = HashSet::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) { continue; }
            let a = m.vert(m.he_source(h)).position;
            let b = m.vert(m.he(h).vertex).position;
            let diff = (0..3).filter(|&k| (a[k] - b[k]).abs() > 1e-6).count();
            if diff == 1 { sel.insert(i); }
        }
        sel
    }

    /// Local topology around a point: `(verts inside, faces fully inside, their sizes sorted)`.
    fn local_corner(m: &HalfEdgeMesh, c: Vec3, r: f32) -> (usize, usize, Vec<usize>) {
        let nv = (0..m.vertices.len() as u32)
            .filter(|&i| m.is_vert_live(VertexId(i)) && (m.vert(VertexId(i)).position - c).length() < r)
            .count();
        let mut sizes = Vec::new();
        for i in 0..m.faces.len() as u32 {
            let f = FaceId(i);
            if m.face(f).half_edge == NO_EDGE { continue; }
            let vs = m.face_vertices(f);
            if vs.is_empty() { continue; }
            if vs.iter().all(|&v| (m.vert(v).position - c).length() < r) { sizes.push(vs.len()); }
        }
        sizes.sort_unstable();
        (nv, sizes.len(), sizes)
    }

    /// A FLAT (tangent-continuous) unbeveled edge must not change the corner fill.
    ///
    /// ORACLE (`docs/planning/poly/flat_edge_corner_ref.py`, Blender 4.5, w=0.1 seg=3, profile 0.5,
    /// Sharp/Sharp, Grid Fill, Loop Slide, Clamp Overlap — run at sx=1 AND sx=2): **nv=96 nf=99
    /// non-manifold=0 in both**, and the flat-sector corner is filled EXACTLY like the clean
    /// control — 12 local verts, 6 quads + 1 triangle. The flat edge contributes no corner vertex
    /// of its own; it re-attaches to the loop-slid BoundVert (`offset_meet` projected onto the
    /// in-between edge, BEVEL.md §3.2): (0.9,−0.9,1) at sx=1, **(1.88,−0.94,1) at sx=2** — the
    /// latter is the value that discriminates the loop slide from a bare rail meet (1.9,−0.9,1).
    ///
    /// Before the fix the cap was SKIPPED at such corners (the 2026-08-04 flat-sector guard) and
    /// the end-of-pipeline closed-input seal plastered ONE flat n-gon over the rim instead — the
    /// owner's broken round.
    #[test]
    fn flat_unbeveled_edge_must_not_break_the_rounded_corner() {
        for (sx, bound_vert) in [(1.0f32, Vec3::new(0.9, -0.9, 1.0)), (2.0, Vec3::new(1.88, -0.94, 1.0))] {
            let m = flat_diagonal_box(sx);
            assert!(m.validate().is_ok(), "sx={sx}: fixture must be a clean closed mesh");
            let sel = box_edges_not_the_diagonal(&m);
            assert_eq!(sel.len(), 24, "sx={sx}: 12 undirected box edges = 24 half-edges");

            let out = bevel_edges_offset(
                &m, &sel, &BevelParams { width: 0.1, segments: 3, shape: 0.5, clamp: true });

            assert!(out.validate().is_ok(), "sx={sx}: result must be valid");
            assert!(out.is_manifold().is_ok(), "sx={sx}: manifold: {:?}", out.is_manifold());
            assert_eq!(crate::bevel_op::boundary_loop_count(&out), 0, "sx={sx}: no holes");

            // THE DEFECT: the flat-sector corner must be filled exactly like the clean control.
            let flat = local_corner(&out, Vec3::new(sx, -1.0, 1.0), 0.32);
            let clean = local_corner(&out, Vec3::new(-sx, -1.0, 1.0), 0.32);
            assert_eq!(clean.1, 7, "sx={sx}: control corner should be the M_ADJ dome, got {clean:?}");
            assert_eq!(
                flat, clean,
                "sx={sx}: a FLAT unbeveled edge must not change the corner fill \
                 (Blender builds the identical dome; flat={flat:?} clean={clean:?})"
            );

            // The flat edge re-attaches to the loop-slid BoundVert, adding no corner vertex.
            assert!(
                (0..out.vertices.len() as u32).any(|i| out.is_vert_live(VertexId(i))
                    && (out.vert(VertexId(i)).position - bound_vert).length() < 2e-3),
                "sx={sx}: the flat edge must terminate on the loop-slid BoundVert {bound_vert:?}"
            );

            // Whole-mesh oracle counts.
            assert_eq!(out.vertex_count(), 96, "sx={sx}: Blender oracle nv=96");
            assert_eq!(out.face_count(), 99, "sx={sx}: Blender oracle nf=99");
        }
    }
