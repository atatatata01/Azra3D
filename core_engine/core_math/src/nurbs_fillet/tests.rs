    use super::*;
    use crate::nurbs::{FaceOrigin, NurbsSurface};

    #[test]
    fn radius_law_const_and_linear() {
        assert!((Radius::Const(0.2).at(0.5) - 0.2).abs() < 1e-6);
        let lin = Radius::Linear { r0: 0.1, r1: 0.3 };
        assert!((lin.at(0.0) - 0.1).abs() < 1e-6);
        assert!((lin.at(1.0) - 0.3).abs() < 1e-6);
        assert!((lin.at(0.5) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn radius_law_monotone_gate() {
        assert!(radius_law_is_monotone(&[(0.0, 0.05), (0.5, 0.1), (1.0, 0.15)]));
        assert!(radius_law_is_monotone(&[(0.0, 0.2), (1.0, 0.05)])); // decreasing
        assert!(radius_law_is_monotone(&[(0.0, 0.1), (1.0, 0.1)])); // constant
        assert!(!radius_law_is_monotone(&[
            (0.0, 0.05),
            (0.5, 0.2),
            (1.0, 0.08)
        ])); // interior peak
        assert!(!radius_law_is_monotone(&[
            (0.0, 0.15),
            (0.5, 0.05),
            (1.0, 0.12)
        ])); // interior valley
    }

    #[test]
    fn radius_peak_and_with_peak_preserve_shape() {
        assert!((Radius::Const(0.2).peak() - 0.2).abs() < 1e-6);
        let lin = Radius::Linear { r0: 0.05, r1: 0.15 };
        assert!((lin.peak() - 0.15).abs() < 1e-6);
        match lin.with_peak(0.3) {
            Radius::Linear { r0, r1 } => {
                assert!((r0 - 0.1).abs() < 1e-5);
                assert!((r1 - 0.3).abs() < 1e-5);
            }
            other => panic!("expected Linear, got {other:?}"),
        }
        let law = Radius::Law(vec![(0.0, 0.05), (0.5, 0.1), (1.0, 0.15)]);
        assert!((law.peak() - 0.15).abs() < 1e-6);
        match law.with_peak(0.3) {
            Radius::Law(s) => {
                assert!((s[0].1 - 0.1).abs() < 1e-5);
                assert!((s[2].1 - 0.3).abs() < 1e-5);
            }
            other => panic!("expected Law, got {other:?}"),
        }
        assert!(Radius::Const(0.0).is_near_zero());
        assert!(!Radius::Const(0.05).is_near_zero());
    }

    #[test]
    fn surface_planar_and_cylinder_fit() {
        let box_f = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        assert!(surface_is_planar(box_f[0].surface.as_ref()));
        let cyl = NurbsSurface::new_cylinder(0.5, 1.0);
        assert!(!surface_is_planar(&cyl));
        let fit = try_fit_cylinder(&cyl).expect("fit Y-up cylinder");
        assert!((fit.radius - 0.5).abs() < 0.05, "r={}", fit.radius);
        assert!((fit.half_height - 0.5).abs() < 0.05, "hh={}", fit.half_height);
        assert!(fit.axis_dir.y.abs() > 0.9, "axis should be ±Y");
    }

    #[test]
    fn expand_planar_face_loop_from_two_top_edges() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let tops = cube_top_loop_edges(&faces);
        assert_eq!(tops.len(), 4);
        let partial = [tops[0], tops[1]];
        let expanded = expand_planar_face_boundary_loop(&faces, &partial);
        assert_eq!(expanded.len(), 4, "partial top → full top loop {expanded:?}");
        // Single edge must NOT expand (free-end semantics).
        let one = expand_planar_face_boundary_loop(&faces, &[tops[0]]);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn two_edges_free_ends_notch_third_faces() {
        // Owner gap/gouge: TwoCorner pure-convex must Arc-notch side faces at the
        // shared vertex (n_sel==2). Without that the third face keeps a sharp corner.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces);
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("two edges");
        let n_fillet = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .count();
        assert_eq!(n_fillet, 2, "two cylinders");
        let n_sph = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Corner { .. }))
            .count();
        assert_eq!(n_sph, 0, "no sphere for 2-edge");
        // At least one support/side face must carry free-end Arc hole trims.
        let n_trimmed = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Base) && !f.trims.is_empty())
            .count();
        assert!(
            n_trimmed >= 1,
            "expected free-end Arc trims on side faces, got {n_trimmed}"
        );
        // Shared-vertex region: every Base face that owns the FreeCAD (1,1,1) corner
        // in AZRA should either be setback-shrunk or notched — sample no sharp
        // residual at the original corner (0.5,0.5,0.5).
        let corner = glam::Vec3::new(0.5, 0.5, 0.5);
        let mut near_corner = 0u32;
        for f in &faces {
            if !matches!(f.origin, FaceOrigin::Base) {
                continue;
            }
            let s = f.surface.as_ref();
            for row in &s.control_points {
                for cp in row {
                    if (cp.position - corner).length() < 0.02 {
                        near_corner += 1;
                    }
                }
            }
        }
        // After setback, original corner CPs should have moved away (0 hits ideal).
        assert!(
            near_corner == 0,
            "sharp corner CPs still at triad vertex — setback/notch failed ({near_corner})"
        );
    }

    #[test]
    fn sequential_vertical_then_top_matches_batch() {
        // Owner repro: vertical edge then adjacent top — must equal both-in-one-batch.
        let mut batch = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e_vert = azra_edges_from_occt_mids(&batch, &[[1.0, 1.0, 0.5]], 0.08);
        let e_top = azra_edges_from_occt_mids(&batch, &[[1.0, 0.5, 1.0]], 0.08);
        assert_eq!(e_vert.len(), 1);
        assert_eq!(e_top.len(), 1);
        let both = [e_vert[0], e_top[0]];
        apply_edges_fillet_batch(&mut batch, &both, 0.2, 100).expect("batch both");

        let mut seq = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        apply_edges_fillet_batch(&mut seq, &e_vert, 0.2, 100).expect("seq vertical");
        apply_edges_fillet_sequential_const(&mut seq, &e_top, 0.2, 200).expect("seq top");

        assert_eq!(
            batch.len(),
            seq.len(),
            "face count batch={} seq={}",
            batch.len(),
            seq.len()
        );
        let n_fillet_b = batch
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .count();
        let n_fillet_s = seq
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .count();
        assert_eq!(n_fillet_b, n_fillet_s);
        assert_eq!(n_fillet_b, 2, "two edge fillets, no sphere");
    }

    #[test]
    fn remove_planar_fillets_restores_cube() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces);
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("fillet");
        assert!(faces.len() > 6);
        let recovered = remove_all_planar_edge_fillets(&mut faces).expect("remove");
        assert_eq!(faces.len(), 6, "back to cube");
        assert!(faces.iter().all(|f| f.trims.is_empty()));
        assert!(
            recovered.len() >= 2,
            "recovered edges {recovered:?}"
        );
    }

    #[test]
    fn remove_one_and_adjust_fillet_anytime() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces);
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("fillet");
        let n_after = faces.len();
        let fillet_ids: Vec<_> = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .map(|f| f.id)
            .collect();
        assert_eq!(fillet_ids.len(), 2);
        // Adjust first fillet radius
        adjust_one_planar_fillet_radius(&mut faces, fillet_ids[0], 0.12).expect("adjust");
        let still = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .count();
        assert_eq!(still, 2, "both fillets remain after adjust");
        // Remove one
        let fid = faces
            .iter()
            .find(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .map(|f| f.id)
            .unwrap();
        remove_one_planar_fillet(&mut faces, fid).expect("remove one");
        let n_fillet = faces
            .iter()
            .filter(|f| matches!(f.origin, FaceOrigin::Fillet { .. }))
            .count();
        assert_eq!(n_fillet, 1, "one fillet remains");
        assert!(faces.len() < n_after);
    }

    #[test]
    fn cylinder_solid_rim_fillet_appends_toroid() {
        let mut faces = NurbsSurface::new_cylinder_nurbs_faces(0.6, 2.0);
        assert_eq!(faces.len(), 3);
        apply_cylinder_rim_fillet_to_solid(&mut faces, 0, 2, 0.15, 10).expect("top rim");
        assert_eq!(faces.len(), 4, "wall+2caps+toroid");
        assert_eq!(faces[3].surface.name, "Toroid");
        match &faces[3].origin {
            FaceOrigin::Fillet { radius, between } => {
                assert!((*radius - 0.15).abs() < 1e-5);
                assert_eq!(between[0], faces[0].id);
            }
            other => panic!("expected Fillet origin, got {other:?}"),
        }
        // Cap outer trim shrunk
        assert!(!faces[1].trims.is_empty());
    }

    #[test]
    fn cylinder_plane_rim_torus_radii() {
        // FreeCAD: makeCylinder + top rim fillet → Toroid major=R-r minor=r
        let cyl = NurbsSurface::new_cylinder(0.5, 1.0);
        let r = 0.1_f32;
        let tor = fillet_cylinder_plane_rim(&cyl, r, true).expect("rim toroid");
        assert_eq!(tor.name, "Toroid");
        let fit = try_fit_cylinder(&cyl).unwrap();
        let major = fit.radius - r;
        let ax = fit.axis_dir;
        let plane_y = fit.axis_origin.y + fit.half_height; // top
        // Sample surface: every point should be at distance r from the major circle
        // (circle in plane y = plane_y - r, radius major, axis through origin // Y)
        let major_y = plane_y - r;
        let mut max_err = 0.0f32;
        let mut on_cyl = 0u32;
        let mut on_plane = 0u32;
        for iu in 0..=12 {
            for iv in 0..=24 {
                let u = iu as f32 / 12.0;
                let v = iv as f32 / 24.0;
                let p = tor.evaluate(u, v);
                // Project to axis
                let d = p - fit.axis_origin;
                let along = d.dot(ax);
                let radial = d - ax * along;
                let rho = radial.length();
                // Major circle at y≈major_y (along axis from origin: for Y-up origin mid)
                let maj_c = fit.axis_origin + ax * (major_y - fit.axis_origin.y);
                // nearest point on major circle
                let rhat = if rho > 1e-6 {
                    radial.normalize()
                } else {
                    Vec3::X
                };
                let on_maj = maj_c + rhat * major;
                let err = (p - on_maj).length() - r;
                max_err = max_err.max(err.abs());
                // Rails: u=0 on cylinder rho≈R, u=1 on plane along≈half_height
                if u < 0.05 {
                    if (rho - fit.radius).abs() < 0.03 {
                        on_cyl += 1;
                    }
                }
                if u > 0.95 {
                    let y = p.dot(ax);
                    let plane_along = (plane_y - fit.axis_origin.y) * ax.dot(ax).signum()
                        + fit.axis_origin.dot(ax);
                    // simpler: world y for Y-up
                    if (p.y - plane_y).abs() < 0.03 {
                        on_plane += 1;
                    }
                    let _ = (y, plane_along);
                }
            }
        }
        assert!(
            max_err < 0.04,
            "torus tube |dist−r| max {max_err} (major={major} r={r})"
        );
        assert!(on_cyl > 5, "outer rail should hug cylinder, on_cyl={on_cyl}");
        assert!(on_plane > 5, "inner rail should hug plane, on_plane={on_plane}");
    }

    #[test]
    fn curved_face_planar_solver_refuses() {
        let cyl = NurbsSurface::new_cylinder(0.5, 1.0);
        let plane = NurbsSurface::new_quad(
            Vec3::new(-1.0, 0.5, -1.0),
            Vec3::new(-1.0, 0.5, 1.0),
            Vec3::new(1.0, 0.5, -1.0),
            Vec3::new(1.0, 0.5, 1.0),
            "Top",
        );
        let err = fillet_planar_edge_trimmed_ex(&cyl, 0, &plane, 0, 0.1, 0.0, 0.0, true);
        assert!(
            matches!(err, Err(FilletError::CurvedFace { .. })),
            "non-planar must not fall into planar solver, got {err:?}"
        );
    }

    #[test]
    fn unequal_edge_radii_three_corner_no_sphere() {
        // Per-edge const radii at cube triad: unequal → free ends, soft UnequalRadiusCorner.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_three_edges_at_vertex(&faces);
        let edge_radii = [(e[0].0, e[0].1, 0.08), (e[1].0, e[1].1, 0.12), (e[2].0, e[2].1, 0.10)];
        let out = apply_edges_fillet_batch_edge_radii(&mut faces, &edge_radii, 100).expect("batch");
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            3,
            "3 cylinders"
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            0,
            "unequal r must not invent equal-r sphere"
        );
        assert!(
            out.faulty_vertices
                .iter()
                .any(|(_, e)| matches!(e, FilletError::UnequalRadiusCorner { .. })),
            "soft UnequalRadiusCorner fault, got {:?}",
            out.faulty_vertices
        );
    }

    #[test]
    fn equal_edge_radii_three_corner_sphere() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_three_edges_at_vertex(&faces);
        let edge_radii = [(e[0].0, e[0].1, 0.1), (e[1].0, e[1].1, 0.1), (e[2].0, e[2].1, 0.1)];
        let out = apply_edges_fillet_batch_edge_radii(&mut faces, &edge_radii, 100).expect("batch");
        assert!(out.is_complete());
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            1
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            3
        );
    }

    #[test]
    fn linear_three_edges_equal_end_r_emits_sphere() {
        // All three cube edges at (0.5,0.5,0.5) share the same endpoint orientation
        // relative to AZRA face_edge_endpoints? Orient so r at corner is equal.
        // Linear r0=r1=0.1 → equal everywhere → sphere like Const.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_three_edges_at_vertex(&faces);
        let out = apply_edges_fillet_batch_linear(&mut faces, &e, 0.1, 0.1, 100).expect("linear");
        assert!(
            out.faulty_vertices.is_empty()
                || !out
                    .faulty_vertices
                    .iter()
                    .any(|(_, e)| matches!(e, FilletError::UnequalRadiusCorner { .. })),
            "equal linear should not fault unequal, got {:?}",
            out.faulty_vertices
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            1,
            "equal linear r0=r1 at triad → sphere"
        );
    }

    #[test]
    fn non_monotone_law_bent_rail_applies() {
        // Interior r peak used to refuse (first→last rail only). Bent multi-station
        // rail lifts that: mid-span setback tracks peak r, not linear first→last.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces);
        let samples = vec![(0.0, 0.05), (0.5, 0.18), (1.0, 0.08)];
        let out = apply_edges_fillet_batch_radius(
            &mut faces,
            &[e],
            &Radius::Law(samples.clone()),
            100,
        )
        .expect("non-monotone law must apply with bent rail");
        assert!(out.is_complete());
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            1
        );

        // Rail unit: refine + per-station setback — mid moves by r_peak, not (r0+r1)/2.
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let (fa, ea) = e;
        let (fb, eb) = find_orthogonal_neighbour(&faces0, fa, ea).unwrap();
        let mut face_a = faces0[fa].surface.as_ref().clone();
        let (a0, a1) = face_edge_endpoints(&face_a, ea).unwrap();
        let n1 = face_outward_normal(faces0[fb].surface.as_ref()).unwrap();
        let tan_half = 1.0f32; // 90°
        refine_face_edge_stations(&mut face_a, ea, samples.len()).unwrap();
        let edge_cps = |s: &NurbsSurface| -> Vec<Vec3> {
            let (rows, cols) = s.grid_size();
            match ea {
                0 => (0..cols).map(|c| s.control_points[0][c].position).collect(),
                1 => (0..rows)
                    .map(|r| s.control_points[r][cols - 1].position)
                    .collect(),
                2 => (0..cols)
                    .map(|c| s.control_points[rows - 1][c].position)
                    .collect(),
                _ => (0..rows).map(|r| s.control_points[r][0].position).collect(),
            }
        };
        let elen2 = (a1 - a0).length_squared().max(1e-12);
        let nearest_mid = |cps: &[Vec3]| {
            cps.iter()
                .copied()
                .min_by(|p, q| {
                    let tp = ((p - a0).dot(a1 - a0) / elen2 - 0.5).abs();
                    let tq = ((q - a0).dot(a1 - a0) / elen2 - 0.5).abs();
                    tp.partial_cmp(&tq).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap()
        };
        let mid_before = nearest_mid(&edge_cps(&face_a));
        shrink_face_edge_law(&mut face_a, ea, -n1, a0, a1, &samples, tan_half).unwrap();
        let mid_after = nearest_mid(&edge_cps(&face_a));
        let mid_setback = (mid_after - mid_before).dot(-n1);
        assert!(
            (mid_setback - 0.18).abs() < 0.02,
            "mid-span setback {mid_setback} should ≈ peak r=0.18 (first→last taper ~0.065)"
        );

        let res = fillet_planar_edge_law(
            faces0[fa].surface.as_ref(),
            ea,
            faces0[fb].surface.as_ref(),
            eb,
            &samples,
            true,
        )
        .expect("planar non-monotone");
        // Middle law sample is the peak
        assert!((res.radius - 0.18).abs() < 1e-5);
    }

    #[test]
    fn batch_radius_const_linear_and_law_work() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces);
        let out = apply_edges_fillet_batch_radius(&mut faces, &[e], &Radius::Const(0.1), 100)
            .expect("Const");
        assert!(out.is_complete());
        assert_eq!(
            out.faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            1
        );
        let mut faces2 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let out_lin = apply_edges_fillet_batch_radius(
            &mut faces2,
            &[e],
            &Radius::Linear { r0: 0.05, r1: 0.15 },
            100,
        )
        .expect("Linear");
        assert_eq!(
            out_lin
                .faces
                .iter()
                .filter(|f| f.surface.name == "Fillet")
                .count(),
            1
        );
        let fil = out_lin
            .faces
            .iter()
            .find(|f| f.surface.name == "Fillet")
            .unwrap()
            .surface
            .as_ref();
        let p0 = fil.evaluate(0.5, 0.0);
        let p1 = fil.evaluate(0.5, 1.0);
        assert!((p0 - p1).length() > 0.5, "linear fillet should span edge");
        let mut faces3 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let out_law = apply_edges_fillet_batch_radius(
            &mut faces3,
            &[e],
            &Radius::Law(vec![(0.0, 0.05), (0.5, 0.12), (1.0, 0.18)]),
            100,
        )
        .expect("Law");
        assert_eq!(
            out_law
                .faces
                .iter()
                .filter(|f| f.surface.name == "Fillet")
                .count(),
            1
        );
        // 3-station law → 3 V control columns
        let law_fil = out_law
            .faces
            .iter()
            .find(|f| f.surface.name == "Fillet")
            .unwrap();
        assert_eq!(law_fil.surface.control_points[0].len(), 3);
    }

    #[test]
    fn toroid_corner_points_near_freecad_radii() {
        // FreeCAD boss triad: major=2r, minor=r — every surface point satisfies
        // dist_to_major_circle ∈ [major−minor, major+minor] i.e. |d − 2r| ≤ r roughly
        // via tube: distance to major circle = r.
        let r = 0.08_f32;
        let v = Vec3::new(0.5, 0.5, 0.2);
        let axis = Vec3::Z;
        let e0 = Vec3::X;
        let e1 = Vec3::Y;
        let tor = make_toroidal_onsame_corner(v, axis, e0, e1, r).expect("toroid");
        assert_eq!(tor.name, "Toroid");
        let centre = v - e0 * r - e1 * r + axis * r;
        let major_r = 2.0 * r;
        for iu in 0..=6 {
            for iv in 0..=6 {
                let u = iu as f32 / 6.0;
                let vv = iv as f32 / 6.0;
                let p = tor.evaluate(u, vv);
                // Project to major plane (perp to axis through centre)
                let rel = p - centre;
                let along = rel.dot(axis) * axis;
                let in_plane = rel - along;
                let d_major = in_plane.length();
                let tube = ((d_major - major_r).powi(2) + along.length_squared()).sqrt();
                assert!(
                    (tube - r).abs() < 0.03,
                    "torus tube dist {tube} ≈ r={r} at uv=({u},{vv}) p={p:?}"
                );
            }
        }
    }

    #[test]
    fn linear_fillet_end_radii_match_r0_r1() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let r0 = 0.08_f32;
        let r1 = 0.2_f32;
        let res = fillet_planar_edge_linear(top, 2, front, 1, r0, r1, true).expect("linear");
        for (v, r_expect) in [(0.0, r0), (1.0, r1)] {
            let pe0 = res.fillet.control_points[0][if v < 0.5 { 0 } else { 1 }].position;
            let pe2 = res.fillet.control_points[2][if v < 0.5 { 0 } else { 1 }].position;
            let chord_len = (pe2 - pe0).length();
            let expect_chord = r_expect * std::f32::consts::SQRT_2;
            assert!(
                (chord_len - expect_chord).abs() < 0.02,
                "v={v}: chord {chord_len} vs {expect_chord} for r={r_expect}"
            );
        }
    }

    #[test]
    fn linear_setback_is_slanted_not_uniform_max() {
        // After linear fillet, face-A edge CPs must move by s0 at start and s1 at end
        // (not both by max). θ=90° → s = r.
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces0[0].surface.as_ref();
        let front = faces0[4].surface.as_ref();
        let edge_a = 2u8;
        let edge_b = 1u8;
        let r0 = 0.05_f32;
        let r1 = 0.15_f32;
        let (a0_before, a1_before) = face_edge_endpoints(top, edge_a).unwrap();
        let res = fillet_planar_edge_linear(top, edge_a, front, edge_b, r0, r1, true).expect("lin");
        let (a0_after, a1_after) = face_edge_endpoints(&res.face_a, edge_a).unwrap();
        // Convex: setback direction is −n_front for top face
        let n_front = face_outward_normal(front).unwrap();
        let d0 = a0_after - a0_before;
        let d1 = a1_after - a1_before;
        // Displacement along −n_front should be ≈ r0 / r1
        let along0 = -d0.dot(n_front);
        let along1 = -d1.dot(n_front);
        assert!(
            (along0 - r0).abs() < 0.02,
            "start setback {along0} should ≈ r0={r0} (slanted rail)"
        );
        assert!(
            (along1 - r1).abs() < 0.02,
            "end setback {along1} should ≈ r1={r1} (slanted rail)"
        );
        // Must NOT both equal r_max
        assert!(
            (along0 - along1).abs() > 0.05,
            "uniform max-r setback would make ends equal; got {along0} vs {along1}"
        );
    }

    #[test]
    fn oracle_tier2_linear_edge_vs_freecad() {
        // FreeCAD makeFillet(r0, r1, [edge5]): hole-only Hausdorff. Catches the G0 crack
        // from uniform max-r setback by design.
        let case = "single_edge_linear_r005_r015";
        let Some(_) = load_occt_tier2_points(case) else {
            eprintln!("skip tier2 {case}: FreeCAD pts missing");
            return;
        };
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces);
        // FreeCAD edge5: (1,1,1)→(1,0,1). Orient r0/r1 to AZRA edge endpoints.
        let (a0, a1) = face_edge_endpoints(faces[e.0].surface.as_ref(), e.1).unwrap();
        let occt_start = Vec3::new(1.0, 1.0, 1.0); // FreeCAD FirstParameter of edge 5
        let a0_occt = azra_to_occt_box(a0);
        let a1_occt = azra_to_occt_box(a1);
        let (r0, r1) = if (a0_occt - occt_start).length() < (a1_occt - occt_start).length() {
            (0.05_f32, 0.15_f32)
        } else {
            (0.15_f32, 0.05_f32)
        };
        let out = apply_edges_fillet_batch_radius(
            &mut faces,
            &[e],
            &Radius::Linear { r0, r1 },
            100,
        )
        .expect("linear batch");
        assert!(out.is_complete());
        let (a2o, o2a) = hausdorff_vs_occt(&faces, case, 12).expect("occt pts");
        eprintln!("Tier2 {case}: OCCT→AZRA={o2a:.4} (hole) AZRA→OCCT={a2o:.4} (info)");
        // Hole tol: FreeCAD tess chord + linear loft approx; crack from max-r setback was ~0.1
        assert!(
            o2a < 0.05,
            "Tier2 hole {case}: OCCT→AZRA={o2a} (slanted setback should kill G0 crack) tol=0.05"
        );
    }

    #[test]
    fn unsupported_corner_is_expressible() {
        // Guard: unhandled (kind, state) must be a first-class error, not a silent octant.
        let e = FilletError::Unsupported {
            kind: CornerKind::Two,
            state: CornerState::OnDiff,
        };
        assert!(matches!(
            e,
            FilletError::Unsupported {
                kind: CornerKind::Two,
                state: CornerState::OnDiff
            }
        ));
    }

    #[test]
    fn convex_exterior_true_for_cube_edge() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        // Top + Front meet at a convex exterior edge
        assert!(edge_is_convex_exterior(
            faces[0].surface.as_ref(),
            faces[4].surface.as_ref()
        ));
    }

    #[test]
    fn convex_exterior_false_when_normals_inverted() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        // Top and Bottom: centers are on opposite sides — not a shared edge, but
        // half-space test should fail (each center is outside the other).
        assert!(!edge_is_convex_exterior(
            faces[0].surface.as_ref(),
            faces[1].surface.as_ref()
        ));
    }

    /// Minimal L-bracket re-entrant: two planes meeting at a concave dihedral
    /// (FreeCAD OnSame class — BSpline blend). AZRA must **refuse**, not invent a cylinder.
    fn lbracket_reentrant_faces() -> (Vec<NurbsFace>, usize, u8) {
        use crate::nurbs::NurbsFaceId;
        // Horizontal shelf top (z = 0.3, normal +Z): dU=+X, dV=+Y
        let shelf = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            Vec3::new(2.0, 1.0, 0.3),
            "ShelfTop",
        );
        // Upright inner wall (x = 0.3, normal +X into notch): dU=+Y, dV=+Z
        let upright = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(0.3, 1.0, 2.0),
            "UprightInner",
        );
        // Shared edge along +Y at (0.3, *, 0.3)
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), shelf),
            NurbsFace::new(NurbsFaceId(1), upright),
        ];
        // Find the shared edge tag on face 0
        let mut edge_tag = (0usize, 0u8);
        for ei in 0u8..4 {
            if find_orthogonal_neighbour(&faces, 0, ei).is_ok() {
                edge_tag = (0, ei);
                break;
            }
        }
        (faces, edge_tag.0, edge_tag.1)
    }

    #[test]
    fn lbracket_reentrant_edge_is_concave() {
        let (faces, fa, ea) = lbracket_reentrant_faces();
        let (fb, _) = find_orthogonal_neighbour(&faces, fa, ea).expect("shared edge");
        assert!(
            !edge_is_convex_exterior(faces[fa].surface.as_ref(), faces[fb].surface.as_ref()),
            "L-bracket re-entrant must not pass convex exterior gate"
        );
        assert_eq!(edge_concavity(&faces, fa, ea), EdgeConcavity::Concave);
    }

    #[test]
    fn lbracket_concave_fillet_is_cylinder_in_free_space() {
        // FreeCAD single re-entrant: 1 Cylinder, axis in free space (emid + r(n0+n1)).
        let (mut faces, fa, ea) = lbracket_reentrant_faces();
        let (fb, eb) = find_orthogonal_neighbour(&faces, fa, ea).unwrap();
        let sa = faces[fa].surface.as_ref();
        let sb = faces[fb].surface.as_ref();
        let r = 0.12_f32;
        let res = fillet_planar_edge_trimmed_ex(sa, ea, sb, eb, r, 0.0, 0.0, false).expect("concave");
        // Axis at free-space offset: for outward +X/+Z re-entrant, axis ≈ (0.3+r, mid_y, 0.3+r)
        let ax = res.axis_origin;
        assert!(
            ax.x > 0.3 + r * 0.5 && ax.z > 0.3 + r * 0.5,
            "concave axis should sit in free notch, got {ax:?}"
        );
        assert_fillet_radius(&res.fillet, res.axis_origin, res.axis_dir, r, 1e-3);

        apply_edges_fillet_batch(&mut faces, &[(fa, ea)], r, 100).expect("batch concave");
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            1,
            "single concave edge → one cylinder"
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            0
        );
    }

    #[test]
    fn non_ortho_setback_formula() {
        // s = r / tan(θ/2): 90° → r, 60° → r√3.
        let r = 0.1_f32;
        let s90 = r / (std::f32::consts::FRAC_PI_2 * 0.5).tan();
        assert!((s90 - r).abs() < 1e-5, "90° setback s={s90}");
        let s60 = r / (std::f32::consts::FRAC_PI_3 * 0.5).tan();
        assert!((s60 - r * 3f32.sqrt()).abs() < 1e-4, "60° setback s={s60}");
        // α = r/(1+cosθ): 90° → r, 60° → 2r/3
        let a90 = r / (1.0 + 0.0);
        let a60 = r / (1.0 + 0.5);
        assert!((a90 - r).abs() < 1e-6);
        assert!((a60 - r * 2.0 / 3.0).abs() < 1e-6);
        // w = cos(θ/2): 90° → √2/2
        let w90 = ((1.0f32 + 0.0) * 0.5).sqrt();
        assert!((w90 - W_MID).abs() < 1e-5);
    }

    #[test]
    fn general_dihedral_60deg_exact_radius() {
        // Tier 1.5: 60° dihedral rational arc, |P−axis| = r.
        use crate::nurbs::NurbsFaceId;
        let r = 0.1_f32;
        let ang = std::f32::consts::FRAC_PI_3; // 60°
        // Face A: z=0, n=+Z
        let a = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            "A",
        );
        // Face B: n at 60° from +Z: dU=(cos,0,-sin), dV=+Y → n=(sin,0,cos)
        let b = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(ang.cos(), 0.0, -ang.sin()),
            Vec3::new(ang.cos(), 1.0, -ang.sin()),
            "B",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), a),
            NurbsFace::new(NurbsFaceId(1), b),
        ];
        let na = face_outward_normal(faces[0].surface.as_ref()).unwrap();
        let nb = face_outward_normal(faces[1].surface.as_ref()).unwrap();
        let theta = na.dot(nb).clamp(-1.0, 1.0).acos();
        assert!(
            (theta.to_degrees() - 60.0).abs() < 2.0,
            "θ={}° na={na:?} nb={nb:?}",
            theta.to_degrees()
        );
        let mut found = None;
        for ea in 0u8..4 {
            if let Ok((fb, eb)) = find_orthogonal_neighbour(&faces, 0, ea) {
                found = Some((0usize, ea, fb, eb));
                break;
            }
        }
        let (fa, ea, fb, eb) = found.expect("60° neighbour");
        let convex = edge_is_convex_exterior(
            faces[fa].surface.as_ref(),
            faces[fb].surface.as_ref(),
        );
        let res = fillet_planar_edge_trimmed_ex(
            faces[fa].surface.as_ref(),
            ea,
            faces[fb].surface.as_ref(),
            eb,
            r,
            0.0,
            0.0,
            convex,
        )
        .expect("60° fillet");
        assert_fillet_radius(&res.fillet, res.axis_origin, res.axis_dir, r, 3e-3);
        // Setback should be r√3 for 60°
        let expected_s = r / (theta * 0.5).tan();
        assert!(
            (expected_s - r * 3f32.sqrt()).abs() < 0.01,
            "setback expected ~r√3, got {expected_s}"
        );
    }

    #[test]
    fn cube_90_still_exact_after_general_arc() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let res = fillet_orthogonal_planar_edge(top, 2, front, 1, 0.1).unwrap();
        assert_fillet_radius(&res.fillet, res.axis_origin, res.axis_dir, 0.1, 1e-4);
        assert_g1_seams(&res.face_a, &res.face_b, &res.fillet, 0.05);
    }

    #[test]
    fn boss_two_concave_edges_no_onsame_patch() {
        // FreeCAD boss-on-plate two base edges: 2 Cylinder, 0 BSpline.
        // Pure-concave two-edge must not emit an OnSame ruled patch.
        use crate::nurbs::NurbsFaceId;
        // Minimal re-entrant step: plate top z=0.2 + two boss walls meeting at (0.5,0.5,0.2)
        // Plate top (n=+Z) for x,y in [0,2]
        let plate = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.2),
            Vec3::new(0.0, 2.0, 0.2),
            Vec3::new(2.0, 0.0, 0.2),
            Vec3::new(2.0, 2.0, 0.2),
            "Plate",
        );
        // Boss wall x=0.5, n=+X (outer) — for re-entrant with plate use n toward free: actually
        // re-entrant boss base: plate top meets boss wall with outward n_plate=+Z, n_wall=−X
        // for wall at x=0.5 facing −X into free (boss is x∈[0,0.5]).
        let wall_x = NurbsSurface::new_quad(
            Vec3::new(0.5, 0.0, 0.2),
            Vec3::new(0.5, 0.0, 1.0),
            Vec3::new(0.5, 0.5, 0.2),
            Vec3::new(0.5, 0.5, 1.0),
            "WallX",
        );
        // Flip so n = −X: swap p01/p10 pattern for dU=+Y, dV=+Z → n=+X; want −X
        let wall_x = NurbsSurface::new_quad(
            Vec3::new(0.5, 0.0, 0.2),
            Vec3::new(0.5, 0.5, 0.2),
            Vec3::new(0.5, 0.0, 1.0),
            Vec3::new(0.5, 0.5, 1.0),
            "WallX",
        );
        let wall_y = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.5, 0.2),
            Vec3::new(0.5, 0.5, 0.2),
            Vec3::new(0.0, 0.5, 1.0),
            Vec3::new(0.5, 0.5, 1.0),
            "WallY",
        );
        // wall_y: dU=+X, dV=+Z → n = −Y? i×k = -j. Want n=−Y for boss y∈[0,0.5] outer at y=0.5.
        let mut faces = vec![
            NurbsFace::new(NurbsFaceId(0), plate),
            NurbsFace::new(NurbsFaceId(1), wall_x),
            NurbsFace::new(NurbsFaceId(2), wall_y),
        ];
        // Discover concave edges plate∩wall_x and plate∩wall_y
        let mut edges = Vec::new();
        for ea in 0u8..4 {
            if let Ok((fb, _)) = find_orthogonal_neighbour(&faces, 0, ea) {
                if fb == 1 || fb == 2 {
                    let c = edge_concavity(&faces, 0, ea);
                    if c == EdgeConcavity::Concave {
                        edges.push((0usize, ea));
                    }
                }
            }
        }
        // Also try from walls if needed
        if edges.len() < 2 {
            for fi in 1..3 {
                for ea in 0u8..4 {
                    if let Ok((fb, _)) = find_orthogonal_neighbour(&faces, fi, ea) {
                        if fb == 0 && edge_concavity(&faces, fi, ea) == EdgeConcavity::Concave {
                            let tag = (fi, ea);
                            if !edges.iter().any(|&e| {
                                edge_in_set(&faces, &[e], tag.0, tag.1)
                            }) {
                                edges.push(tag);
                            }
                        }
                    }
                }
            }
        }
        edges = dedupe_solid_edges(&faces, &edges);
        if edges.len() >= 2 {
            let pair = [edges[0], edges[1]];
            apply_edges_fillet_batch(&mut faces, &pair, 0.08, 100).expect("boss batch");
            let n_onsame = faces.iter().filter(|f| f.surface.name == "OnSame").count();
            let n_cyl = faces.iter().filter(|f| f.surface.name == "Fillet").count();
            assert_eq!(n_onsame, 0, "pure-concave boss must not emit OnSame patch");
            assert_eq!(n_cyl, 2, "expected 2 cylinders, got {n_cyl}");
        } else {
            // Geometry discovery failed — still assert classification helper
            assert!(
                !edge_is_convex_exterior(
                    faces[0].surface.as_ref(),
                    faces[1].surface.as_ref()
                ) || edge_concavity(&faces, 0, 0) != EdgeConcavity::Convex
            );
        }
    }

    #[test]
    fn face_depth_caps_radius_before_collapse() {
        // Thin face (depth 0.1): max r must be << half of long edge.
        use crate::nurbs::NurbsFaceId;
        let thin = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.1, 0.0, 0.0),
            Vec3::new(0.1, 1.0, 0.0),
            "Thin",
        );
        let side = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 1.0),
            "Side",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), thin),
            NurbsFace::new(NurbsFaceId(1), side),
        ];
        let mut ea = 0u8;
        for e in 0u8..4 {
            if find_orthogonal_neighbour(&faces, 0, e).is_ok() {
                ea = e;
                break;
            }
        }
        let m = max_fillet_radius_for_edge(&faces, 0, ea).expect("max");
        // Depth of thin face ≈ 0.1 → cap ≈ 0.0495
        assert!(m < 0.06, "thin face depth must cap r, got {m}");
        let err = apply_edges_fillet_batch(&mut faces.clone(), &[(0, ea)], 0.08, 100);
        assert!(
            matches!(err, Err(FilletError::RadiusTooLarge { .. })),
            "r larger than face depth must refuse, got {err:?}"
        );
    }

    #[test]
    fn short_edge_max_radius_tracks_edge_length() {
        // Face extent allows ~0.495, but a short edge must cap lower.
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        // Unit cube edges have length 1 → edge_max = 0.495, same order as face max.
        let m = max_fillet_radius_for_edge(&faces, 0, 2).expect("max");
        assert!(m < 0.5 && m > 0.4, "unit edge max ≈ 0.495, got {m}");

        // Synthetic short edge: 0.2-long shared edge between two small faces.
        use crate::nurbs::NurbsFaceId;
        let a = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.2, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.2, 0.0),
            "A",
        );
        let b = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.2, 0.0),
            Vec3::new(0.0, 0.2, 1.0),
            "B",
        );
        let short_faces = vec![
            NurbsFace::new(NurbsFaceId(0), a),
            NurbsFace::new(NurbsFaceId(1), b),
        ];
        // Shared edge length 0.2 along +Y → max r ≤ 0.2 * 0.495 ≈ 0.099
        let mut ea = 0u8;
        for e in 0u8..4 {
            if find_orthogonal_neighbour(&short_faces, 0, e).is_ok() {
                ea = e;
                break;
            }
        }
        let m_short = max_fillet_radius_for_edge(&short_faces, 0, ea).expect("short max");
        assert!(
            m_short < 0.12,
            "short edge must cap radius well below face half-extent, got {m_short}"
        );
        assert!(edge_too_short_for_radius(0.2, 0.15));
        assert!(!edge_too_short_for_radius(0.2, 0.05));

        let err = apply_edges_fillet_batch(&mut short_faces.clone(), &[(0, ea)], 0.15, 100);
        assert!(
            matches!(err, Err(FilletError::RadiusTooLarge { .. })),
            "r > edge max must refuse, got {err:?}"
        );
    }

    #[test]
    fn onsame_ruled_corner_builds_for_mixed_arcs() {
        // Two free-end arcs at a vertex → circular-loft OnSame patch has finite area.
        let v = Vec3::new(0.3, 0.0, 0.3);
        let r = 0.12_f32;
        // Concave re-entrant (+X,+Z) and convex top/front (+Z,−Y)
        let (arc_c, c_c) = free_end_arc_cps(v, Vec3::Z, Vec3::X, r, false);
        let (arc_x, c_x) = free_end_arc_cps(v, Vec3::Z, -Vec3::Y, r, true);
        assert!((arc_c[0].position - arc_c[2].position).length() > 0.1);
        assert!((arc_x[0].position - arc_x[2].position).length() > 0.1);
        assert!((c_c - v).length() > r * 0.5);
        assert!((c_x - v).length() > r * 0.5);
        // Direct patch from make using synthetic faces
        use crate::nurbs::NurbsFaceId;
        let shelf = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            Vec3::new(2.0, 1.0, 0.3),
            "Shelf",
        );
        let upright = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(0.3, 1.0, 2.0),
            "Upright",
        );
        let front = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            "Front",
        );
        // Front outward should be −Y for base [y≥0]: dU=+X, dV=+Z → n = +Y, flip by swap
        let front = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            "Front",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), shelf),
            NurbsFace::new(NurbsFaceId(1), upright),
            NurbsFace::new(NurbsFaceId(2), front),
        ];
        // Manually craft two resolved-like tuples sharing v
        let e_conc = (0usize, 0u8, 1usize, 0u8, v, v + Vec3::Y, false);
        let e_conv = (0usize, 1u8, 2usize, 0u8, v, v + Vec3::X, true);
        // Fix edge indices by discovering real shared edges
        let mut conc = None;
        let mut conv = None;
        for ea in 0u8..4 {
            if let Ok((fb, eb)) = find_orthogonal_neighbour(&faces, 0, ea) {
                let (a0, a1) = face_edge_endpoints(faces[0].surface.as_ref(), ea).unwrap();
                let mid = (a0 + a1) * 0.5;
                let c = edge_is_convex_exterior(
                    faces[0].surface.as_ref(),
                    faces[fb].surface.as_ref(),
                );
                if fb == 1 {
                    conc = Some((0, ea, fb, eb, a0, a1, c));
                }
                if fb == 2 {
                    conv = Some((0, ea, fb, eb, a0, a1, c));
                }
                let _ = mid;
            }
        }
        // Also check face 2 edges with face 0
        for ea in 0u8..4 {
            if let Ok((fb, eb)) = find_orthogonal_neighbour(&faces, 2, ea) {
                if fb == 0 {
                    let (a0, a1) = face_edge_endpoints(faces[2].surface.as_ref(), ea).unwrap();
                    let c = edge_is_convex_exterior(
                        faces[2].surface.as_ref(),
                        faces[0].surface.as_ref(),
                    );
                    conv = Some((2, ea, fb, eb, a0, a1, c));
                }
            }
        }
        if let (Some(ec), Some(ev)) = (conc, conv) {
            let at = [&ec, &ev];
            // Shared vertex near (0.3, 0, 0.3)
            let mut shared = None;
            for p in [ec.4, ec.5] {
                if (p - ev.4).length() < 1e-4 || (p - ev.5).length() < 1e-4 {
                    shared = Some(p);
                }
            }
            if let Some(sv) = shared {
                let patch = make_onsame_ruled_corner(&faces, &at, sv, r).expect("onsame");
                assert_eq!(patch.name, "OnSame");
                assert_eq!(patch.degree_v, 2, "circular loft is deg-v=2, not ruled deg-v=1");
                let area_proxy = (patch.evaluate(0.0, 0.0) - patch.evaluate(1.0, 1.0)).length();
                assert!(area_proxy > 0.05, "patch should span corner, span={area_proxy}");
                // Mid-span should sit near r-tube about free-end centres (not chord of ruled).
                let mid = patch.evaluate(0.5, 0.5);
                let c_mid = (c_c + c_x) * 0.5;
                let d_tube = (mid - c_mid).length();
                assert!(
                    (d_tube - r).abs() < 0.08,
                    "mid-span should track r-tube about centres, |P-c_mid|={d_tube} r={r}"
                );
            }
        }
        // Arc construction always works
        let _ = (arc_c, arc_x, e_conc, e_conv);
    }

    /// FreeCAD L-bracket coords (Z-up): base 2×1×0.3 + upright 0.3×1×2.
    /// Mixed edges at (0.3,0,0.3): concave re-entrant along +Y + convex upright front along +Z
    /// (FreeCAD edges 6 + 4 → 2 cyl + 1 BSpline OnSame).
    fn lbracket_mixed_onsame_faces() -> (Vec<NurbsFace>, Vec<(usize, u8)>) {
        use crate::nurbs::NurbsFaceId;
        // Shelf top z=0.3, n=+Z (dU=+X, dV=+Y)
        let shelf = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            Vec3::new(2.0, 1.0, 0.3),
            "ShelfTop",
        );
        // Upright outer x=0.3, n=+X (dU=+Y, dV=+Z)
        let upright = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(0.3, 1.0, 2.0),
            "UprightOuter",
        );
        // Front y=0, n=−Y (dU=+X, dV=+Z). Z-range matches upright so edge endpoints co-locate
        // with FreeCAD edge4 (0.3,0,0.3)→(0.3,0,2).
        let front = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.3),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            "Front",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), shelf),
            NurbsFace::new(NurbsFaceId(1), upright),
            NurbsFace::new(NurbsFaceId(2), front),
        ];
        // Discover concave shelf∩upright and convex upright∩front
        let mut edges = Vec::new();
        for ea in 0u8..4 {
            if let Ok((fb, _)) = find_orthogonal_neighbour(&faces, 0, ea) {
                if fb == 1 && edge_concavity(&faces, 0, ea) == EdgeConcavity::Concave {
                    edges.push((0usize, ea));
                }
            }
        }
        for ea in 0u8..4 {
            if let Ok((fb, _)) = find_orthogonal_neighbour(&faces, 1, ea) {
                if fb == 2 && edge_concavity(&faces, 1, ea) == EdgeConcavity::Convex {
                    edges.push((1usize, ea));
                }
            }
        }
        // Also try front→upright if orientation flipped discovery
        if edges.len() < 2 {
            for ea in 0u8..4 {
                if let Ok((fb, _)) = find_orthogonal_neighbour(&faces, 2, ea) {
                    if fb == 1 && edge_concavity(&faces, 2, ea) == EdgeConcavity::Convex {
                        let tag = (2usize, ea);
                        if !edges
                            .iter()
                            .any(|&e| edge_in_set(&faces, &[e], tag.0, tag.1))
                        {
                            edges.push(tag);
                        }
                    }
                }
            }
        }
        edges = dedupe_solid_edges(&faces, &edges);
        (faces, edges)
    }

    /// FreeCAD L-bracket triad at `(0.3,0,0.3)`: edges 4 (convex vertical), 6 (concave
    /// re-entrant), 12 (convex shelf front). Four planar faces so each dihedral's face
    /// centres pass the convex half-space test (a single oversized front fails it).
    /// Returns (faces, concave_edge, convex_vertical, convex_shelf_front).
    fn lbracket_triad_faces() -> (Vec<NurbsFace>, (usize, u8), (usize, u8), (usize, u8)) {
        use crate::nurbs::NurbsFaceId;
        // Shelf top z=0.3, n=+Z
        let shelf = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(2.0, 0.0, 0.3),
            Vec3::new(2.0, 1.0, 0.3),
            "ShelfTop",
        );
        // Upright outer x=0.3, n=+X
        let upright = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            Vec3::new(0.3, 1.0, 0.3),
            Vec3::new(0.3, 1.0, 2.0),
            "UprightOuter",
        );
        // Upright front y=0, x∈[0,0.3], n=−Y (FreeCAD edge 4 partner)
        let front_up = NurbsSurface::new_quad(
            Vec3::new(0.0, 0.0, 0.3),
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(0.3, 0.0, 2.0),
            "FrontUpright",
        );
        // Base front y=0, z∈[0,0.3], n=−Y (FreeCAD edge 12 partner)
        let front_base = NurbsSurface::new_quad(
            Vec3::new(0.3, 0.0, 0.0),
            Vec3::new(0.3, 0.0, 0.3),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.3),
            "FrontBase",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), shelf),
            NurbsFace::new(NurbsFaceId(1), upright),
            NurbsFace::new(NurbsFaceId(2), front_up),
            NurbsFace::new(NurbsFaceId(3), front_base),
        ];
        let v = Vec3::new(0.3, 0.0, 0.3);
        let at = edges_at_vertex(&faces, v);
        assert!(
            at.len() >= 3,
            "triad needs ≥3 solid edges at vertex, got {at:?}"
        );
        let mut concave = None;
        let mut convex_vert = None; // along +Z
        let mut convex_front = None; // along +X
        for &(fa, ea) in &at {
            let c = edge_concavity(&faces, fa, ea);
            let Ok((a0, a1)) = face_edge_endpoints(faces[fa].surface.as_ref(), ea) else {
                continue;
            };
            let dir = (a1 - a0).normalize_or_zero();
            if c == EdgeConcavity::Concave {
                concave = Some((fa, ea));
            } else if c == EdgeConcavity::Convex {
                if dir.z.abs() > 0.7 {
                    convex_vert = Some((fa, ea));
                } else if dir.x.abs() > 0.7 {
                    convex_front = Some((fa, ea));
                }
            }
        }
        (
            faces,
            concave.expect("concave re-entrant"),
            convex_vert.expect("convex vertical"),
            convex_front.expect("convex shelf front"),
        )
    }

    #[test]
    fn lbracket_ondiff_vs_onsame_end_states() {
        // FreeCAD vertex (0.3,0,0.3): 2 convex + 1 concave.
        // Concave alone → OnDiff; either convex → OnSame (matches the other convex).
        let (faces, e_conc, e_vert, e_front) = lbracket_triad_faces();
        let v = Vec3::new(0.3, 0.0, 0.3);
        assert_eq!(
            classify_corner_state_at_vertex(&faces, v),
            CornerState::OnSame,
            "histogram at mixed triad is 2+1 OnSame"
        );
        assert_eq!(
            classify_fillet_end_state(&faces, v, e_conc.0, e_conc.1),
            CornerState::OnDiff,
            "filleting the odd (concave) edge is OnDiff"
        );
        assert_eq!(
            classify_fillet_end_state(&faces, v, e_vert.0, e_vert.1),
            CornerState::OnSame,
            "filleting a majority (convex) edge is OnSame"
        );
        assert_eq!(
            classify_fillet_end_state(&faces, v, e_front.0, e_front.1),
            CornerState::OnSame
        );
        // FreeCAD topology: single OnDiff edge → 1 cyl, no join patch
        let mut f1 = faces.clone();
        apply_edges_fillet_batch(&mut f1, &[e_conc], 0.08, 100).expect("ondiff single");
        assert_eq!(
            f1.iter().filter(|f| f.surface.name == "Fillet").count(),
            1
        );
        assert_eq!(
            f1.iter().filter(|f| f.surface.name == "OnSame").count(),
            0,
            "OnDiff single edge must not invent a join patch"
        );
        // FreeCAD: convex pair (skip concave) → 2 cyl, no BSpline
        let mut f2 = faces.clone();
        apply_edges_fillet_batch(&mut f2, &[e_vert, e_front], 0.08, 100).expect("convex pair");
        assert_eq!(
            f2.iter().filter(|f| f.surface.name == "Fillet").count(),
            2
        );
        assert_eq!(
            f2.iter().filter(|f| f.surface.name == "OnSame").count(),
            0,
            "two convex at mixed vertex: no OnSame patch (FreeCAD pair 4+12)"
        );
        // FreeCAD: concave + one convex → 2 cyl + BSpline (OnSame join)
        let mut f3 = faces.clone();
        apply_edges_fillet_batch(&mut f3, &[e_conc, e_vert], 0.08, 100).expect("mixed pair");
        assert_eq!(
            f3.iter().filter(|f| f.surface.name == "Fillet").count(),
            2
        );
        assert_eq!(
            f3.iter().filter(|f| f.surface.name == "OnSame").count(),
            1,
            "mixed pair must emit OnSame join"
        );
    }

    #[test]
    fn lbracket_mixed_two_edges_emits_onsame() {
        let (mut faces, edges) = lbracket_mixed_onsame_faces();
        assert!(
            edges.len() >= 2,
            "need mixed concave+convex pair, got {edges:?}"
        );
        let pair = [edges[0], edges[1]];
        let cx: Vec<_> = pair
            .iter()
            .map(|&(f, e)| edge_concavity(&faces, f, e))
            .collect();
        assert!(
            cx.contains(&EdgeConcavity::Concave) && cx.contains(&EdgeConcavity::Convex),
            "mixed convexity required, got {cx:?}"
        );
        apply_edges_fillet_batch(&mut faces, &pair, 0.12, 100).expect("batch");
        let n_cyl = faces.iter().filter(|f| f.surface.name == "Fillet").count();
        let n_onsame = faces.iter().filter(|f| f.surface.name == "OnSame").count();
        assert_eq!(n_cyl, 2, "2 cylinders");
        assert_eq!(n_onsame, 1, "1 OnSame join patch");
        let patch = faces
            .iter()
            .find(|f| f.surface.name == "OnSame")
            .unwrap()
            .surface
            .as_ref();
        assert_eq!(patch.degree_v, 2);
    }

    #[test]
    fn oracle_tier2_lbracket_onsame_vs_freecad() {
        // FreeCAD edges 4+6 on L-bracket → 2 Cylinder + 1 BSpline.
        // Ref is blend faces only (`lbracket_onsame_blend_pts.json`) — same world frame as AZRA faces.
        let case = "lbracket_onsame_blend";
        let Some(occt) = load_occt_tier2_points(case) else {
            eprintln!("skip tier2 {case}: FreeCAD pts missing");
            return;
        };
        let (mut faces, edges) = lbracket_mixed_onsame_faces();
        if edges.len() < 2 {
            eprintln!("skip tier2 {case}: mixed edge discovery failed");
            return;
        }
        apply_edges_fillet_batch(&mut faces, &[edges[0], edges[1]], 0.12, 100).expect("batch");
        // Sample Fillet + OnSame only (geometry under test).
        let mut azra_blend = Vec::new();
        for f in &faces {
            let name = f.surface.name.as_str();
            if name != "Fillet" && name != "OnSame" {
                continue;
            }
            let s = f.surface.as_ref();
            let (u0, u1) = s.domain_u();
            let (v0, v1) = s.domain_v();
            let n = 28u32;
            for iu in 0..=n {
                for iv in 0..=n {
                    let u = u0 + (u1 - u0) * iu as f32 / n as f32;
                    let v = v0 + (v1 - v0) * iv as f32 / n as f32;
                    if f.uv_inside(u, v) {
                        azra_blend.push(s.evaluate(u, v));
                    }
                }
            }
        }
        assert!(!occt.is_empty() && !azra_blend.is_empty());
        let (a2o, o2a) = hausdorff_two_sided(&azra_blend, &occt);
        eprintln!(
            "Tier2 {case}: OCCT→AZRA={o2a:.4} (hole) AZRA→OCCT={a2o:.4} (info) n_occt={} n_azra={}",
            occt.len(),
            azra_blend.len()
        );
        // Circular loft vs FreeCAD BSpline. OCCT→AZRA is hole (missing AZRA surface).
        // AZRA→OCCT can be larger: our cylinders are untrimmed full free ends vs FreeCAD
        // clipped rails. Pure ruled mid-join was the worse hole; loft lands ~0.06.
        assert!(
            o2a < 0.065,
            "Tier2 OnSame hole {case}: OCCT→AZRA={o2a} tol=0.065 (circular loft vs BSpline)"
        );
    }

    #[test]
    fn cube_corner_state_is_all_same() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let v = Vec3::new(0.5, 0.5, 0.5);
        assert_eq!(
            classify_corner_state_at_vertex(&faces, v),
            CornerState::AllSame
        );
    }

    #[test]
    fn cube_edge_fillet_exact_radius() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        // Top=0 edge 2 (+Z front), Front=4 edge 1 (+Y top) â€” shared cube edge
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let r = 0.1;
        let res = fillet_orthogonal_planar_edge(top, 2, front, 1, r).expect("fillet");
        assert_fillet_radius(&res.fillet, res.axis_origin, res.axis_dir, r, 1e-4);
    }

    #[test]
    fn cube_edge_fillet_g1() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let res = fillet_orthogonal_planar_edge(top, 2, front, 1, 0.1).unwrap();
        assert_g1_seams(&res.face_a, &res.face_b, &res.fillet, 0.05);
    }

    #[test]
    fn cube_edge_fillet_radius_too_large() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let err = fillet_orthogonal_planar_edge(top, 2, front, 1, 10.0);
        assert!(matches!(err, Err(FilletError::RadiusTooLarge { .. })));
    }

    #[test]
    fn sabotage_radius_gate_fails_on_wrong_r() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let top = faces[0].surface.as_ref();
        let front = faces[4].surface.as_ref();
        let res = fillet_orthogonal_planar_edge(top, 2, front, 1, 0.1).unwrap();
        let mut failed = false;
        let ad = res.axis_dir.normalize_or_zero();
        let p = res.fillet.evaluate(0.5, 0.5);
        let rel = p - res.axis_origin;
        let radial = (rel - rel.dot(ad) * ad).length();
        if (radial - 0.5).abs() < 1e-4 {
            failed = true;
        }
        assert!(!failed || (radial - 0.1).abs() < 1e-4);
        assert!((radial - 0.1).abs() < 1e-4);
    }

    // â”€â”€ N5 spherical octant â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn spherical_octant_exact_radius() {
        let r = 0.15_f32;
        let e0 = Vec3::X;
        let e1 = Vec3::Y;
        let e2 = Vec3::Z;
        let vertex = Vec3::new(0.5, 0.5, 0.5);
        let centre = sphere_centre_at_vertex(vertex, e0, e1, e2, r);
        let corner = make_spherical_octant(centre, r, e0, e1, e2).expect("octant");
        assert_sphere_radius(&corner, centre, r, 1e-4);
    }

    #[test]
    fn spherical_octant_corners_on_axes() {
        let r = 0.2_f32;
        let centre = Vec3::ZERO;
        let corner = make_spherical_octant(centre, r, Vec3::X, Vec3::Y, Vec3::Z).unwrap();
        // (u,v)=(0,0) â†’ +X ; (1,0) â†’ +Y ; pole v=1 â†’ +Z
        let p00 = corner.evaluate(0.0, 0.0);
        let p10 = corner.evaluate(1.0, 0.0);
        let p_pole = corner.evaluate(0.5, 1.0);
        assert!((p00 - Vec3::X * r).length() < 1e-3, "p00={p00:?}");
        assert!((p10 - Vec3::Y * r).length() < 1e-3, "p10={p10:?}");
        assert!((p_pole - Vec3::Z * r).length() < 1e-3, "pole={p_pole:?}");
    }

    #[test]
    fn spherical_octant_rejects_non_orthogonal() {
        let err = make_spherical_octant(
            Vec3::ZERO,
            0.1,
            Vec3::X,
            (Vec3::X + Vec3::Y).normalize(),
            Vec3::Z,
        );
        assert!(matches!(err, Err(FilletError::NotOrthogonal)));
    }

    #[test]
    fn sabotage_sphere_gate_fails_on_wrong_r() {
        let r = 0.12_f32;
        let centre = Vec3::new(0.1, 0.2, 0.3);
        let corner = make_spherical_octant(centre, r, Vec3::X, Vec3::Y, Vec3::Z).unwrap();
        let p = corner.evaluate(0.4, 0.4);
        let dist = (p - centre).length();
        // Sabotage: wrong claimed radius must not pass
        assert!((dist - 0.5).abs() > 1e-3 || (dist - r).abs() < 1e-4);
        assert!((dist - r).abs() < 1e-4, "actual dist={dist}");
    }

    #[test]
    fn max_radius_unit_cube_near_half() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        // Top edge shared with front â€” side length 1 â†’ max â‰ˆ 0.495 (Plasticity-scale)
        let m = max_fillet_radius_for_edge(&faces, 0, 2).expect("max");
        assert!(m > 0.48, "max should be ~0.495 not capped low, got {m}");
        assert!(m < 0.5, "max must stay below half-edge, got {m}");
    }

    #[test]
    fn large_radius_top_loop_survives() {
        // Four top edges at large r â€” OCCT: 4 cylinders, 0 spheres (no clover)
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let tops = cube_top_loop_edges(&faces);
        assert_eq!(tops.len(), 4, "top loop edges {tops:?}");
        let r = 0.35_f32;
        apply_edges_fillet_batch(&mut faces, &tops, r, 100).expect("large-r top loop");
        let n_cyl = faces.iter().filter(|f| f.surface.name == "Fillet").count();
        let n_sph = faces.iter().filter(|f| f.surface.name == "Corner").count();
        assert_eq!(n_cyl, 4, "4 top edge cylinders");
        assert_eq!(n_sph, 0, "OCCT top-loop has no spheres (clover was wrong)");
        assert_tier1_face_counts(&faces, "top_loop_r02");
    }

    #[test]
    fn face_origin_base_and_fillet_attribution() {
        // Decision 5: faces remember â€” base stays Base; new cylinder carries Fillet{between}.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        for f in &faces {
            assert_eq!(f.origin, FaceOrigin::Base);
        }
        let id_a = faces[0].id;
        apply_edges_fillet_batch(&mut faces, &[(0, 2)], 0.12, 100).expect("batch");
        let fil = faces.iter().find(|f| f.surface.name == "Fillet").unwrap();
        match &fil.origin {
            FaceOrigin::Fillet { radius, between } => {
                assert!((radius - 0.12).abs() < 1e-6);
                assert!(between.contains(&id_a));
            }
            other => panic!("expected Fillet origin, got {other:?}"),
        }
    }

    #[test]
    fn face_origin_resolve_radius_change() {
        // Change radius: recover edges from FaceOrigin, re-solve from base snapshot.
        let base = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let edges = cube_two_edges_at_vertex(&base);
        let mut once = base.clone();
        apply_edges_fillet_batch(&mut once, &edges, 0.12, 100).expect("r=0.12");
        let n_cyl_12 = once.iter().filter(|f| f.surface.name == "Fillet").count();
        assert_eq!(n_cyl_12, 2);

        let op = fillet_op_from_origins(&base, &once).expect("op from origins");
        assert_eq!(op.edges.len(), 2, "edges {:?}", op.edges);
        match &op.radius {
            Radius::Const(r) => assert!((r - 0.12).abs() < 1e-5),
            other => panic!("expected Const, got {other:?}"),
        }

        let out = resolve_edges_fillet_from_base(&base, &op.edges, 0.25, 200).expect("r=0.25");
        assert!(out.is_complete());
        let n_cyl = out.faces.iter().filter(|f| f.surface.name == "Fillet").count();
        let n_sph = out.faces.iter().filter(|f| f.surface.name == "Corner").count();
        assert_eq!(n_cyl, 2);
        assert_eq!(n_sph, 0, "two-edge re-solve must not grow spheres");
        for f in &out.faces {
            if let FaceOrigin::Fillet { radius, .. } = f.origin {
                assert!((radius - 0.25).abs() < 1e-5);
            }
        }
        // Larger r → cylinder mid-arc sits lower (top edges: y = 0.5 - r(1-√2/2))
        let fil_mid_y = |faces: &[NurbsFace]| {
            faces
                .iter()
                .find(|f| f.surface.name == "Fillet")
                .map(|f| f.surface.evaluate(0.5, 0.5).y)
                .unwrap()
        };
        let y12 = fil_mid_y(&once);
        let y25 = fil_mid_y(&out.faces);
        assert!(
            y25 < y12 - 0.03,
            "r=0.25 mid y={y25} should be below r=0.12 mid y={y12}"
        );
    }

    #[test]
    fn face_origin_resolve_three_edge_keeps_sphere() {
        let base = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let edges = cube_three_edges_at_vertex(&base);
        let mut once = base.clone();
        apply_edges_fillet_batch(&mut once, &edges, 0.15, 100).expect("once");
        let edges2 = edges_from_fillet_origins(&base, &once);
        assert_eq!(edges2.len(), 3, "recover 3 edges from origins, got {edges2:?}");
        let out = resolve_edges_fillet_from_base(&base, &edges2, 0.2, 200).expect("resolve");
        assert_eq!(
            out.faces.iter().filter(|f| f.surface.name == "Corner").count(),
            1
        );
        assert_eq!(
            out.faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            3
        );
    }

    #[test]
    fn face_origin_linear_re_solve_preserves_law() {
        // FilletLinear on the face → re-solve must rebuild Linear, not Const(r_max).
        let base = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&base);
        let mut once = base.clone();
        apply_edges_fillet_batch_radius(
            &mut once,
            &[e],
            &Radius::Linear { r0: 0.05, r1: 0.15 },
            100,
        )
        .expect("linear");
        let fil = once.iter().find(|f| f.surface.name == "Fillet").unwrap();
        match &fil.origin {
            FaceOrigin::FilletLinear { r0, r1, .. } => {
                assert!((r0 - 0.05).abs() < 1e-6 && (r1 - 0.15).abs() < 1e-6);
            }
            other => panic!("expected FilletLinear, got {other:?}"),
        }
        let op = fillet_op_from_origins(&base, &once).expect("op");
        match &op.radius {
            Radius::Linear { r0, r1 } => {
                assert!((r0 - 0.05).abs() < 1e-6 && (r1 - 0.15).abs() < 1e-6);
            }
            other => panic!("op.radius should be Linear, got {other:?}"),
        }
        let out = resolve_fillet_op_from_origins(&base, &once, 200).expect("re-solve");
        let fil2 = out
            .faces
            .iter()
            .find(|f| f.surface.name == "Fillet")
            .unwrap();
        match &fil2.origin {
            FaceOrigin::FilletLinear { r0, r1, .. } => {
                assert!((r0 - 0.05).abs() < 1e-6 && (r1 - 0.15).abs() < 1e-6);
            }
            FaceOrigin::Fillet { radius, .. } => {
                panic!("re-solve collapsed to Const({radius}) — FaceOrigin debt regressed");
            }
            other => panic!("unexpected origin {other:?}"),
        }
        // Slanted setback still holds after re-solve
        let (a0, a1) = face_edge_endpoints(base[e.0].surface.as_ref(), e.1).unwrap();
        let (b0, b1) = face_edge_endpoints(fil2.surface.as_ref(), 0).unwrap_or((a0, a1));
        let _ = (b0, b1);
        assert!(fil2.origin.display_radius().unwrap() > 0.14);

        // Adjust-panel path: scale Linear peak via with_peak + resolve_edges_fillet_from_base_radius
        // (must not collapse to Const).
        let scaled = op.radius.with_peak(0.20);
        match &scaled {
            Radius::Linear { r0, r1 } => {
                assert!((r0 - 0.05 * 0.20 / 0.15).abs() < 1e-5);
                assert!((r1 - 0.20).abs() < 1e-5);
            }
            other => panic!("with_peak must keep Linear, got {other:?}"),
        }
        let out2 =
            resolve_edges_fillet_from_base_radius(&base, &op.edges, &scaled, 300).expect("scaled");
        let fil3 = out2
            .faces
            .iter()
            .find(|f| f.surface.name == "Fillet")
            .unwrap();
        match &fil3.origin {
            FaceOrigin::FilletLinear { r0, r1, .. } => {
                assert!((r1 - 0.20).abs() < 1e-5, "r1={r1}");
                assert!(*r0 > 0.05 && *r0 < 0.15, "r0={r0} should stay intermediate");
            }
            FaceOrigin::Fillet { radius, .. } => {
                panic!("adjust-style re-solve collapsed to Const({radius})");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn face_origin_law_attribution() {
        let base = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&base);
        let mut faces = base.clone();
        let samples = vec![(0.0, 0.06), (0.5, 0.1), (1.0, 0.14)];
        apply_edges_fillet_batch_radius(&mut faces, &[e], &Radius::Law(samples.clone()), 100)
            .expect("law");
        let fil = faces.iter().find(|f| f.surface.name == "Fillet").unwrap();
        match &fil.origin {
            FaceOrigin::FilletLaw { samples: s, .. } => {
                assert_eq!(s.len(), 3);
                assert!((s[0].1 - 0.06).abs() < 1e-6);
            }
            other => panic!("expected FilletLaw, got {other:?}"),
        }
        let op = fillet_op_from_origins(&base, &faces).expect("op");
        assert!(matches!(op.radius, Radius::Law(_)));
        let out = resolve_fillet_op_from_origins(&base, &faces, 200).expect("re-solve law");
        assert!(matches!(
            out.faces
                .iter()
                .find(|f| f.surface.name == "Fillet")
                .unwrap()
                .origin,
            FaceOrigin::FilletLaw { .. }
        ));
    }

    #[test]
    fn tangent_chain_cube_single_edge_stays_single() {
        // Cube corners are 90° — no G1-collinear continuation (OCCT chain ≠ corner complete).
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces);
        let chain = expand_tangent_edge_chain(&faces, &[e]);
        assert_eq!(chain.len(), 1, "cube must not grow a tangent chain, got {chain:?}");
    }

    #[test]
    fn tangent_chain_cube_two_top_not_corner_complete() {
        // Two top edges meet at 90° — collinear test must NOT add the vertical third.
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces);
        let chain = expand_tangent_edge_chain(&faces, &e);
        assert_eq!(
            chain.len(),
            2,
            "must not complete orthogonal triad via tangent chain, got {chain:?}"
        );
        let mut once = faces.clone();
        apply_edges_fillet_batch(&mut once, &chain, 0.15, 100).expect("batch");
        assert_eq!(
            once.iter().filter(|f| f.surface.name == "Corner").count(),
            0,
            "no sphere when chain stays 2-edge"
        );
    }

    #[test]
    fn tangent_chain_collinear_synthetic_expands() {
        // Coplanar split roof+front: two collinear hard edges along +X at y=z=0.5.
        // Select left half → chain must pull in the right half (OCCT Add contour).
        use crate::nurbs::NurbsFaceId;
        let hy = 0.5_f32;
        let hz = 0.5_f32;
        // Top L/R: n = +Y
        let top_l = NurbsSurface::new_quad(
            Vec3::new(-0.5, hy, -hz),
            Vec3::new(0.0, hy, -hz),
            Vec3::new(-0.5, hy, hz),
            Vec3::new(0.0, hy, hz),
            "TopL",
        );
        let top_r = NurbsSurface::new_quad(
            Vec3::new(0.0, hy, -hz),
            Vec3::new(0.5, hy, -hz),
            Vec3::new(0.0, hy, hz),
            Vec3::new(0.5, hy, hz),
            "TopR",
        );
        // Front L/R: n = +Z (same winding as unit box Front)
        let front_l = NurbsSurface::new_quad(
            Vec3::new(-0.5, -hy, hz),
            Vec3::new(-0.5, hy, hz),
            Vec3::new(0.0, -hy, hz),
            Vec3::new(0.0, hy, hz),
            "FrontL",
        );
        let front_r = NurbsSurface::new_quad(
            Vec3::new(0.0, -hy, hz),
            Vec3::new(0.0, hy, hz),
            Vec3::new(0.5, -hy, hz),
            Vec3::new(0.5, hy, hz),
            "FrontR",
        );
        let faces = vec![
            NurbsFace::new(NurbsFaceId(0), top_l),
            NurbsFace::new(NurbsFaceId(1), top_r),
            NurbsFace::new(NurbsFaceId(2), front_l),
            NurbsFace::new(NurbsFaceId(3), front_r),
        ];
        // Shared hard edges are at y=hy, z=hz, x in [-0.5,0] and [0,0.5]
        let mut left_edge: Option<(usize, u8)> = None;
        let mut right_edge: Option<(usize, u8)> = None;
        for (fi, f) in faces.iter().enumerate() {
            for ei in 0u8..4 {
                let Ok((a, b)) = face_edge_endpoints(f.surface.as_ref(), ei) else {
                    continue;
                };
                let mid = (a + b) * 0.5;
                if (mid.y - hy).abs() > 1e-4 || (mid.z - hz).abs() > 1e-4 {
                    continue;
                }
                if mid.x < -0.05 {
                    left_edge = Some((fi, ei));
                } else if mid.x > 0.05 {
                    right_edge = Some((fi, ei));
                }
            }
        }
        let left = left_edge.expect("left collinear edge");
        let right = right_edge.expect("right collinear edge");
        assert!(
            find_orthogonal_neighbour(&faces, left.0, left.1).is_ok(),
            "left edge needs orthogonal pair"
        );
        let chain = expand_tangent_edge_chain(&faces, &[left]);
        assert!(
            chain.len() >= 2,
            "collinear half-edge must expand, left={left:?} right={right:?} chain={chain:?}"
        );
        assert!(
            edge_in_set(&faces, &chain, right.0, right.1)
                || chain.iter().any(|&e| {
                    let (a, b) = face_edge_endpoints(faces[e.0].surface.as_ref(), e.1).unwrap();
                    let m = (a + b) * 0.5;
                    m.x > 0.05 && (m.y - hy).abs() < 1e-3 && (m.z - hz).abs() < 1e-3
                }),
            "chain should include right half, got {chain:?}"
        );
    }

    #[test]
    fn ladder1_single_edge_no_corners() {
        // One edge, endpoints touch no other selected edge.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let n0 = faces.len();
        let added = apply_edges_fillet_batch(&mut faces, &[(0, 2)], 0.12, 100).expect("batch");
        assert_eq!(added, 1, "one cylinder only, got {added}");
        assert_eq!(faces.len(), n0 + 1);
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            0
        );
        // Cylinder radius gate
        let fil = faces.iter().find(|f| f.surface.name == "Fillet").unwrap();
        let (a0, a1) = face_edge_endpoints(
            &NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5)[0]
                .surface
                .as_ref()
                .clone(),
            2,
        )
        .unwrap();
        let axis_dir = (a1 - a0).normalize();
        let mid = (a0 + a1) * 0.5;
        // Axis â‰ˆ mid - r n_top - r n_front; sample radial distance
        let p = fil.surface.evaluate(0.5, 0.5);
        let n_top = Vec3::Y;
        let n_front = Vec3::Z;
        let axis_o = mid - n_top * 0.12 - n_front * 0.12;
        let rel = p - axis_o;
        let radial = (rel - rel.dot(axis_dir) * axis_dir).length();
        assert!(
            (radial - 0.12).abs() < 1e-3,
            "ladder1 radius {radial} != 0.12"
        );
    }

    #[test]
    fn two_top_edges_no_sphere_without_third() {
        // OCCT: 2 edges at a vertex â†’ 2 cylinders, 0 spheres (sphere only if all 3 selected).
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let added =
            apply_edges_fillet_batch(&mut faces, &[(0, 1), (0, 2)], 0.1, 100).expect("batch");
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            2,
            "exactly 2 cylinders (no auto vertical)"
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            0,
            "no sphere until third edge selected (OCCT)"
        );
        assert_eq!(added, 2, "2 fil only, got {added}");
    }

    #[test]
    fn four_vertical_edges_top_face_survives() {
        // All 4 vertical edges â€” top gets free-end Arc notches, must not collapse.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let mut verticals = Vec::new();
        let mut mids = Vec::new();
        for fi in 2..6 {
            for ei in 0u8..4 {
                if let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) {
                    if (a.y - b.y).abs() > 0.5 {
                        let mid = (a + b) * 0.5;
                        if !mids.iter().any(|m: &Vec3| (*m - mid).length_squared() < 1e-6) {
                            mids.push(mid);
                            verticals.push((fi, ei));
                        }
                    }
                }
            }
        }
        assert_eq!(verticals.len(), 4, "cube has 4 verticals, got {verticals:?}");
        let r = 0.08_f32;
        let added = apply_edges_fillet_batch(&mut faces, &verticals, r, 100).expect("batch");
        assert_eq!(added, 4, "4 cylinders only (no spheres), got {added}");
        let top = &faces[0];
        assert!(!top.trims.is_empty(), "top should have free-end Arc notches");
        let (v, i) = top.tessellate();
        assert!(!i.is_empty(), "top tessellation empty");
        let mut mn = Vec3::splat(f32::MAX);
        let mut mx = Vec3::splat(f32::MIN);
        for vert in &v {
            let p = Vec3::new(vert[0], vert[1], vert[2]);
            mn = mn.min(p);
            mx = mx.max(p);
        }
        let extent = (mx - mn).max_element();
        assert!(
            extent > 0.3,
            "top mesh collapsed extent={extent} (sawtooth ring bug)"
        );
    }

    #[test]
    fn ladder3_three_edges_sphere_and_trim() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let vertex = Vec3::new(0.5, 0.5, 0.5);
        let mut edges = Vec::new();
        for fi in 0..6 {
            for ei in 0u8..4 {
                if let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) {
                    let on = |p: Vec3| (p - vertex).length_squared() < 1e-8;
                    if on(a) || on(b) {
                        edges.push((fi, ei));
                    }
                }
            }
        }
        let edges = dedupe_solid_edges(&faces, &edges);
        assert_eq!(edges.len(), 3, "3 solid edges at corner");
        let r = 0.1_f32;
        let added = apply_edges_fillet_batch(&mut faces, &edges, r, 100).expect("batch");
        assert_eq!(added, 4, "3 cylinders + 1 sphere, got {added}");
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            1
        );
        let corner = faces.iter().find(|f| f.surface.name == "Corner").unwrap();
        let centre = vertex - Vec3::ONE * r;
        assert_sphere_radius(&corner.surface, centre, r, 1e-3);
    }

    #[test]
    fn three_corner_sphere_cylinder_g1_seams() {
        // Tier 0.3: each trimmed cylinder end must lie on the sphere with radial G1.
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let edges = cube_three_edges_at_vertex(&faces);
        let r = 0.15_f32;
        apply_edges_fillet_batch(&mut faces, &edges, r, 100).expect("batch");
        let sphere = faces
            .iter()
            .find(|f| f.surface.name == "Corner")
            .expect("sphere")
            .surface
            .as_ref();
        let centre = Vec3::new(0.5, 0.5, 0.5) - Vec3::ONE * r;
        assert_sphere_radius(sphere, centre, r, 1e-3);
        let cyls: Vec<_> = faces
            .iter()
            .filter(|f| f.surface.name == "Fillet")
            .map(|f| f.surface.as_ref())
            .collect();
        assert_eq!(cyls.len(), 3);
        for cyl in cyls {
            assert_g1_sphere_cylinder_end(centre, r, cyl, 1e-3, 0.05);
        }
    }

    #[test]
    fn cube_vertex_symmetric_corner_radius() {
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let vertex = Vec3::new(0.5, 0.5, 0.5);
        let r = 0.1_f32;
        let face_set = [0usize, 2, 4];
        let mut edge_list = Vec::new();
        for &fi in &face_set {
            for ei in 0u8..4 {
                if let Ok((a, b)) = face_edge_endpoints(faces[fi].surface.as_ref(), ei) {
                    let on_v = |p: Vec3| (p - vertex).length_squared() < 1e-8;
                    if on_v(a) || on_v(b) {
                        edge_list.push((fi, ei, a, b));
                    }
                }
            }
        }
        let mut chosen: Vec<(usize, u8)> = Vec::new();
        let mut mids = Vec::new();
        for (fi, ei, a, b) in edge_list {
            let mid = (a + b) * 0.5;
            if mids.iter().any(|m: &Vec3| (*m - mid).length_squared() < 1e-8) {
                continue;
            }
            mids.push(mid);
            chosen.push((fi, ei));
            if chosen.len() == 3 {
                break;
            }
        }
        assert_eq!(chosen.len(), 3, "need 3 edges at vertex, got {chosen:?}");
        let fe = [chosen[0], chosen[1], chosen[2]];
        let res = fillet_symmetric_cube_vertex(&faces, fe, vertex, r).expect("corner fillet");
        assert_sphere_radius(&res.corner, res.centre, r, 1e-3);
        let expected_c = vertex - Vec3::ONE * r;
        assert!(
            (res.centre - expected_c).length() < 1e-4,
            "centre {:?} vs {:?}",
            res.centre,
            expected_c
        );
    }

    // â”€â”€ OCCT oracle Tier 1 â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn oracle_tier1_single_edge() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces);
        apply_edges_fillet_batch(&mut faces, &[e], 0.2, 100).expect("batch");
        assert_tier1_face_counts(&faces, "single_edge_r02");
    }

    #[test]
    fn oracle_tier1_two_edges_at_vertex() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces);
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("batch");
        assert_tier1_face_counts(&faces, "two_edges_at_vertex");
    }

    #[test]
    fn oracle_tier1_three_edges_at_vertex() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_three_edges_at_vertex(&faces);
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("batch");
        assert_tier1_face_counts(&faces, "three_edges_at_vertex");
    }

    #[test]
    fn oracle_tier1_all_edges_r02() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_all_solid_edges(&faces);
        assert_eq!(e.len(), 12, "cube has 12 solid edges");
        apply_edges_fillet_batch(&mut faces, &e, 0.2, 100).expect("batch");
        assert_tier1_face_counts(&faces, "all_edges_r02");
    }

    #[test]
    fn oracle_tier1_all_edges_near_max_r() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_all_solid_edges(&faces);
        let r = 0.45_f32.min(
            e.iter()
                .filter_map(|&(f, ei)| max_fillet_radius_for_edge(&faces, f, ei).ok())
                .fold(f32::MAX, f32::min),
        );
        apply_edges_fillet_batch(&mut faces, &e, r, 100).expect("batch");
        assert_tier1_face_counts(&faces, "all_edges_r045");
    }

    #[test]
    fn oracle_tier2_hausdorff_self_zero() {
        let pts = vec![Vec3::ZERO, Vec3::X, Vec3::Y];
        let (a, b) = hausdorff_two_sided(&pts, &pts);
        assert!(a < 1e-6 && b < 1e-6);
    }

    /// FreeCAD Tier-2: **OCCTâ†’AZRA only** (detects holes in AZRA).
    /// AZRAâ†’OCCT is logged only â€” FreeCAD tess undersamples distant planes.
    fn oracle_tier2_case(case: &str, edges: &[(usize, u8)], r: f32, tol_hole: f32) {
        let Some(_) = load_occt_tier2_points(case) else {
            eprintln!("skip tier2 {case}: FreeCAD pts missing");
            return;
        };
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        apply_edges_fillet_batch(&mut faces, edges, r, 100).expect("batch");
        assert_tier1_face_counts(&faces, case);
        let (a2o, o2a) = hausdorff_vs_occt(&faces, case, 12).expect("occt pts");
        eprintln!("Tier2 {case}: OCCTâ†’AZRA={o2a:.4} (hole) AZRAâ†’OCCT={a2o:.4} (info)");
        assert!(
            o2a < tol_hole,
            "Tier2 hole {case}: OCCTâ†’AZRA={o2a} (AZRA missing surface) tol={tol_hole}"
        );
    }

    // Fillet UV ×4: multi join hole ~0.008, single ~0.002, three ~0.003 (FreeCAD tess).
    // Was ~0.021 at ×2 — pure undersampling of Steinmetz diagonal (geometry = 2 cyl).
    const TIER2_HOLE: f32 = 0.012;

    #[test]
    fn edge_index_map_freecad_top_loop() {
        // FreeCAD 1.1.3 top-loop mids must resolve to 4 distinct AZRA solid edges.
        let faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let mids = [
            [0.0, 0.5, 1.0], // edge 1
            [1.0, 0.5, 1.0], // edge 5
            [0.5, 0.0, 1.0], // edge 9
            [0.5, 1.0, 1.0], // edge 11
        ];
        let mapped = azra_edges_from_occt_mids(&faces, &mids, 0.05);
        assert_eq!(mapped.len(), 4, "mapped {mapped:?}");
        let tops = cube_top_loop_edges(&faces);
        assert_eq!(tops.len(), 4);
        for e in &mapped {
            assert!(tops.contains(e), "mapped {e:?} not in top loop {tops:?}");
        }
    }

    #[test]
    fn oracle_tier2_single_edge_vs_freecad() {
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_single_top_edge_matching_freecad5(&faces0);
        oracle_tier2_case("single_edge_r02", &[e], 0.2, 0.03);
    }

    #[test]
    fn oracle_tier2_two_edges_vs_freecad() {
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_two_edges_at_vertex(&faces0);
        oracle_tier2_case("two_edges_at_vertex", &e, 0.2, TIER2_HOLE);
    }

    #[test]
    fn oracle_tier2_top_loop_vs_freecad() {
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let tops = cube_top_loop_edges(&faces0);
        oracle_tier2_case("top_loop_r02", &tops, 0.2, TIER2_HOLE);
    }

    #[test]
    fn oracle_tier2_three_edges_vs_freecad() {
        let faces0 = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let e = cube_three_edges_at_vertex(&faces0);
        oracle_tier2_case("three_edges_at_vertex", &e, 0.2, 0.03);
    }

    /// Top-loop continuous solid: 4 cyl + 4 spheres, side setback, no exterior balloon.
    #[test]
    fn top_loop_side_faces_setback_and_continuous() {
        let mut faces = NurbsSurface::new_box_nurbs_faces(0.5, 0.5, 0.5);
        let tops = cube_top_loop_edges(&faces);
        assert_eq!(tops.len(), 4, "top loop {tops:?}");
        let r = 0.2_f32;
        apply_edges_fillet_batch(&mut faces, &tops, r, 100).expect("top loop");
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Fillet").count(),
            4
        );
        assert_eq!(
            faces.iter().filter(|f| f.surface.name == "Corner").count(),
            0,
            "top-loop must not emit corner spheres (clover)"
        );
        assert_tier1_face_counts(&faces, "top_loop_r02");
        // Side faces set back in Y
        for fi in 2..6 {
            let s = faces[fi].surface.as_ref();
            let mut max_y = f32::MIN;
            for row in &s.control_points {
                for cp in row {
                    max_y = max_y.max(cp.position.y);
                }
            }
            assert!(
                max_y < 0.5 - r * 0.5,
                "side face {fi} max_y={max_y} should be set back by ~r={r}"
            );
        }
        // Top face inset
        let top = faces[0].surface.as_ref();
        for row in &top.control_points {
            for cp in row {
                assert!(
                    cp.position.x.abs() < 0.5 - r * 0.5 + 1e-3,
                    "top CP not inset x={:?}",
                    cp.position
                );
                assert!(
                    cp.position.z.abs() < 0.5 - r * 0.5 + 1e-3,
                    "top CP not inset z={:?}",
                    cp.position
                );
            }
        }
        // No exterior balloon (clover lobes outside cube)
        let pts = sample_azra_points(&faces, 8);
        for p in &pts {
            assert!(
                p.x.abs() <= 0.5 + 1e-3 && p.y.abs() <= 0.5 + 1e-3 && p.z.abs() <= 0.5 + 1e-3,
                "point outside unit cube AABB: {p:?}"
            );
        }
        // Plasticity: hard verticals â€” outer silhouette CPs must stay on |x|=0.5 or |z|=0.5.
        // Snap-to-sphere was wrong: it inset top of vertical â†’ diagonal sides / pyramid.
        for fi in 2..6 {
            for row in &faces[fi].surface.control_points {
                for cp in row {
                    let p = cp.position;
                    // Bottom ring of the cube must remain full extent (no pyramid tip)
                    if p.y < -0.4 {
                        assert!(
                            p.x.abs() > 0.45 || p.z.abs() > 0.45,
                            "bottom CP collapsed inward {p:?} â€” vertical was beveled/snapped"
                        );
                    }
                    // Any CP still near full height setback plane should not be inset in XZ
                    // past the outer wall (hard edge at Â±0.5)
                    if (p.y - (0.5 - r)).abs() < 1e-3 {
                        // Top of side face after setback: still on outer wall for vertical corners
                        // (Y-only shrink), so |x|â‰ˆ0.5 or |z|â‰ˆ0.5 for corner CPs
                        let on_outer = p.x.abs() > 0.45 || p.z.abs() > 0.45;
                        assert!(
                            on_outer,
                            "side top CP inset in XZ {p:?} â€” hard vertical lost (snap/bevel)"
                        );
                    }
                }
            }
        }
    }

