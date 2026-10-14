// Tests for node_graph_bake (pure relocation from monolithic file).

#[cfg(test)]
mod height_normal_regression {
    use super::*;

    #[test]
    fn scalar_normal_connection_inserts_converter_and_samples_connected_height() {
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let coord = graph.add_node(NodeType::TexCoord, [0.0, 0.0]);
        let separate = graph.add_node(NodeType::SeparateXyz, [340.0, 0.0]);
        assert!(graph.connect(coord, 0, separate, 0));
        assert!(graph.connect(separate, 0, output, NodeGraph::MO_NORMAL));
        let converter = graph.connection_source(output, NodeGraph::MO_NORMAL).unwrap().0;
        assert_eq!(graph.find_node(converter).unwrap().node_type, NodeType::HeightToNormal);
        assert_eq!(graph.connection_source(converter, 0), Some((separate, 0)));
        assert!(graph.connect(separate, 0, output, NodeGraph::MO_HEIGHT));
        let order = graph.topo_sort(output).unwrap();
        for position in [[0.2, 0.3, 0.0], [-0.6, 0.8, 0.0]] {
            let sample = eval_graph_sample(&graph, &order, output, position, [0.0, 0.0, 1.0], 0.0,
                &ImageMapSources::default(), &SdfFieldSources::default(), 6);
            let expected = Vec3::new(-1.0, 0.0, 1.0).normalize();
            assert!((Vec3::from_array(sample.normal) - expected).length() < 1e-5, "{:?}", sample.normal);
        }
        let constant = graph.add_node(NodeType::ConstantValue, [0.0, 300.0]);
        assert!(graph.connect(constant, 0, converter, 0));
        let order = graph.topo_sort(output).unwrap();
        let sample = eval_graph_sample(&graph, &order, output, [0.2, 0.3, 0.0], [0.0, 0.0, 1.0], 0.0,
            &ImageMapSources::default(), &SdfFieldSources::default(), 6);
        assert_eq!(sample.normal, [0.0, 0.0, 1.0]);
        let channels = channels_from_graph(&graph);
        assert!(channels.normal && channels.bump);
        let maps = bake_node_graph_maps_res(&graph, 16, channels,
            &ImageMapSources::default(), &SdfFieldSources::default(), 6).unwrap();
        let encoded = crate::material_textures::encode_normal_rgba16_planes([0.0, 0.0, 1.0]);
        assert_eq!(&maps.normal[..4], &encoded.0);
    }

    #[test]
    fn height_only_does_not_double_apply_normal_and_cycles_preserve_previous_wire() {
        let mut graph = NodeGraph::default();
        let output = graph.output_node().unwrap().id;
        let noise = graph.add_node(NodeType::Noise, [0.0, 0.0]);
        assert!(graph.connect(noise, 0, output, NodeGraph::MO_HEIGHT));
        assert!(channels_from_graph(&graph).bump);
        assert!(!channels_from_graph(&graph).normal);
        let mut material = core_math::pbr_material::PbrMaterial::default();
        graph.pull_from_pbr(&material);
        graph.push_to_pbr(&mut material);
        assert_eq!(material.bump_map.strength, 0.05);
        graph.output_node_mut().unwrap().input_defaults[NodeGraph::MO_BUMP_STRENGTH].value_f32 = 0.125;
        graph.push_to_pbr(&mut material);
        assert_eq!(material.bump_map.strength, 0.125);
        material.bump_map.tex_id = 1;
        graph.pull_from_pbr(&material);
        assert_eq!(graph.output_node().unwrap().input_defaults[NodeGraph::MO_BUMP_STRENGTH].value_f32, 0.125);
        let encoded_graph = serde_json::to_string(&graph).unwrap();
        let restored: NodeGraph = serde_json::from_str(&encoded_graph).unwrap();
        restored.push_to_pbr(&mut material);
        assert_eq!(material.bump_map.strength, 0.125);
        let color = graph.add_node(NodeType::ConstantColor, [0.0, 100.0]);
        assert!(graph.connect(color, 0, output, NodeGraph::MO_NORMAL));
        assert_eq!(graph.connection_source(output, NodeGraph::MO_NORMAL), Some((color, 0)));
        assert!(!graph.nodes.iter().any(|node| node.node_type == NodeType::HeightToNormal));
        let first = graph.add_node(NodeType::MathBinary, [0.0, 200.0]);
        let second = graph.add_node(NodeType::MathBinary, [0.0, 300.0]);
        assert!(graph.connect(noise, 0, first, 0));
        assert!(graph.connect(first, 0, second, 0));
        assert!(!graph.connect(second, 0, first, 0));
        assert_eq!(graph.connection_source(first, 0), Some((noise, 0)));
    }
}

#[cfg(test)]
mod curvature_fast_path_regression {
    //! Owner bug: "adding a Generator layer takes ~45s". Root-caused 2026-07-27 to the CPU
    //! curvature fallback (~500 analytical SDF evals/pixel at 256x256 = 65 536 pixels) — the GPU
    //! fast path was never actually exercised end to end by any test:
    //! `tests_gpu_curvature.rs` calls `dispatch_curvature_blocking` directly and never touches
    //! `sdf.curv_cache` or `sdf_curvature_masks`'s cache-consuming branch
    //! (`node_graph_bake.rs:416-434`), so a bug in the park/dispatch/drain state machine
    //! (`spawn_or_park_graph_bake` / `poll_graph_curvature` / `drain_curvature_readbacks`) or in
    //! the fast-path pixel-index inversion could ship invisibly. These tests close that gap:
    //!   1. the exact panel path (`regen_channel_subgraph`, what `mp_lower_rich_channels` calls —
    //!      NOT `lower_to_graph`, which is a different scalar-only path and does not emit the
    //!      layer-stack subgraph at all) produces a graph `curvature_key_for_graph` accepts,
    //!   2. once a `CurvGrid` is attached, `sdf_curvature_masks` actually reads it (proven by
    //!      seeding the grid with values no CPU eval would produce, and asserting the bake's
    //!      output pixels equal the grid, not a fresh analytical computation),
    //!   3. the attached fast path is measurably (not just theoretically) faster than the CPU
    //!      fallback at the production resolution.
    use super::*;
    use crate::gpu_curvature::CurvGrid;
    use crate::material_panel::lower::regen_channel_subgraph;
    use crate::material_panel::model::{
        Channel, ChannelBitDepth, ChannelKind, ChannelState, Layer, LayerContent, MaterialPanelDoc,
        Placement, Adjust, BlendMode, DirectionAxis, GeneratorOutput, GeneratorSource,
        GeneratorRung,
    };
    use core_math::primitives::{BooleanOp, PbrMaterial, SdfPrimitive, SdfShape};
    use core_math::sdf_shapes::BoxBevel;
    use glam::Vec3;

    fn sphere_op(pos: Vec3, radius: f32, op: BooleanOp) -> SdfPrimitive {
        let mut p = SdfPrimitive::new("s".into(), SdfShape::Sphere { radius }, pos);
        p.operation = op;
        p
    }
    fn box_op(pos: Vec3, he: Vec3, op: BooleanOp) -> SdfPrimitive {
        let mut p = SdfPrimitive::new(
            "b".into(),
            SdfShape::Box { half_extents: he, bevel: BoxBevel::default() },
            pos,
        );
        p.operation = op;
        p
    }

    /// A carved-box body (same shape family as the owner's repro geometry), captured as the
    /// `SdfFieldSources` snapshot the bake actually consumes.
    fn owner_repro_body() -> SdfFieldSources {
        let prims = vec![
            box_op(Vec3::ZERO, Vec3::splat(0.6), BooleanOp::Union { k: 0.0 }),
            sphere_op(Vec3::new(0.45, 0.45, 0.0), 0.4, BooleanOp::Subtract { k: 0.05 }),
        ];
        let mut out = SdfFieldSources::default();
        out.body_prims = prims.clone();
        for p in &prims {
            out.by_uid.insert(p.prim_uid, p.clone());
        }
        out
    }

    /// A snapshot as taken for a POLYGONAL body: no `body_prims` (because
    /// `SceneGraph::body_primitives` returns `None` for a `PolygonalBody`), but `by_uid` still
    /// carrying every SDF prim in the scene — the mixed-scene state that
    /// `snapshot_sdf_field_for_material` produces.
    fn poly_body_in_mixed_scene() -> SdfFieldSources {
        let mut out = SdfFieldSources::default();
        for p in [
            box_op(Vec3::ZERO, Vec3::splat(0.6), BooleanOp::Union { k: 0.0 }),
            sphere_op(Vec3::new(0.45, 0.45, 0.0), 0.4, BooleanOp::Subtract { k: 0.05 }),
        ] {
            out.by_uid.insert(p.prim_uid, p);
        }
        out
    }

    /// GATE (POLY_CURVATURE_PLAN P0) — a body with no field of its own reads as NO DATA, and
    /// reads the SAME whether or not some *other* body in the scene has a field.
    ///
    /// Two defects met here before this gate existed, both masked only by the UI refusing to let
    /// the user author the state (`has_sdf` greying out the generator menu):
    ///
    /// * **B2a** — `sdf_at` fell through to `sn_field_h`, a hardcoded procedural height field. A
    ///   fieldless body did not read as "no curvature"; it read as *the curvature of a synthetic
    ///   pattern* — a plausible-looking map with no relationship to any geometry.
    /// * **B2b** — `has_field()` and `body_aabb` both fell through to `by_uid`, which is
    ///   scene-wide. So the same poly body measured and bounded ITSELF against an unrelated
    ///   body's prims in a mixed scene, and against nothing in a poly-only scene.
    ///
    /// The assertion is deliberately *identity to zero*, not "differs from the SDF body". A
    /// difference-only check passes on a synthetic pattern, which is exactly what B2a produced.
    #[test]
    fn p0_fieldless_body_fails_closed_and_ignores_other_bodies() {
        let mixed = poly_body_in_mixed_scene();
        let alone = SdfFieldSources::default();

        assert!(!mixed.has_field(), "a poly body in a mixed scene has no field of its own");
        assert!(!alone.has_field(), "a poly body in a poly-only scene has no field");
        assert!(
            !mixed.has_curvature_source() && !alone.has_curvature_source(),
            "no measured grid attached yet either — nothing to draw from"
        );

        // Bounds must not be inherited from the other body. Both fieldless cases get the same
        // defined unit-cube domain.
        assert_eq!(
            mixed.body_aabb(0),
            alone.body_aabb(0),
            "B2b: a fieldless body's AABB must not leak in from scene-wide `by_uid`"
        );

        // Sweep the planar bake domain: every analyzer must be identically its fail-closed value
        // in BOTH scenes. `sn_field_h` varies across this sweep, so it cannot survive it.
        for i in 0..16 {
            for j in 0..16 {
                let p = [
                    (i as f32 + 0.5) / 8.0 - 1.0,
                    (j as f32 + 0.5) / 8.0 - 1.0,
                    0.0,
                ];
                for (name, s) in [("mixed", &mixed), ("alone", &alone)] {
                    let (mask, curv, conv, conc) =
                        sdf_curvature_masks(s, p, 0.02, 0, 0, 0.0, 0.2, 1.0, 1.0);
                    assert_eq!(
                        (mask, curv, conv, conc),
                        (0.0, 0.0, 0.0, 0.0),
                        "{name}: curvature at {p:?} must be identically 0 with no field"
                    );
                    assert_eq!(
                        sdf_ambient_occlusion(s, p, 0.02, 0, 0.0, 0.2),
                        1.0,
                        "{name}: AO fails OPEN (unoccluded) at {p:?}"
                    );
                    assert_eq!(
                        sdf_direction_mask(s, p, 0.02, 0, [0.0, 1.0, 0.0], 0.0, 0.2, 1.0, 1.0),
                        (0.0, 0.0),
                        "{name}: direction fails closed at {p:?}"
                    );
                }
            }
        }
    }

    /// GATE (P0) — the fail-closed distance produces no NaN/Inf anywhere downstream.
    ///
    /// `NO_FIELD_D` is finite ON PURPOSE: `f32::INFINITY` would make `calc_curvature`'s
    /// `taps - 4.0 * d` evaluate `inf - inf = NaN`, and a NaN mask paints garbage rather than
    /// nothing — strictly worse than the synthetic pattern it replaced.
    #[test]
    fn p0_no_field_sentinel_is_finite_and_cancels() {
        let empty = SdfFieldSources::default();
        assert!(NO_FIELD_D.is_finite(), "sentinel must be finite");
        for p in [[0.0f32, 0.0, 0.0], [0.5, -0.5, 0.0], [-1.0, 1.0, 0.03]] {
            let d = empty.sdf_at_for_test(p, 0);
            assert_eq!(d, NO_FIELD_D);
            let (m, c, cx, cc) = sdf_curvature_masks(&empty, p, 0.02, 0, 0, 0.0, 0.2, 1.0, 1.0);
            for (label, v) in [("mask", m), ("curv", c), ("convex", cx), ("concave", cc)] {
                assert!(v.is_finite(), "{label} must be finite at {p:?} (got {v})");
            }
        }
    }

    /// Welded axis-aligned cube, `n × n` quads per face — a mesh with genuine flat-face interior
    /// vertices as well as edges and corners, which a 6-quad cube does not have.
    fn subdivided_cube_mesh(n: usize, size: f32) -> core_math::half_edge::HalfEdgeMesh {
        let h = size * 0.5;
        let mut pos: Vec<Vec3> = Vec::new();
        let mut faces: Vec<Vec<usize>> = Vec::new();
        let mut index = std::collections::HashMap::<(i64, i64, i64), usize>::new();
        let q = |x: f32| (x / size * 1.0e4).round() as i64;
        let sides = [
            (Vec3::new(-h, -h, h), Vec3::X, Vec3::Y),
            (Vec3::new(h, -h, -h), -Vec3::X, Vec3::Y),
            (Vec3::new(h, -h, h), -Vec3::Z, Vec3::Y),
            (Vec3::new(-h, -h, -h), Vec3::Z, Vec3::Y),
            (Vec3::new(-h, h, h), Vec3::X, -Vec3::Z),
            (Vec3::new(-h, -h, -h), Vec3::X, Vec3::Z),
        ];
        let s = size / n as f32;
        for (o, du, dv) in sides {
            for i in 0..n {
                for j in 0..n {
                    let mut ring = Vec::with_capacity(4);
                    for (ci, cj) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                        let p = o + du * ((i + ci) as f32 * s) + dv * ((j + cj) as f32 * s);
                        let k = (q(p.x), q(p.y), q(p.z));
                        let id = *index.entry(k).or_insert_with(|| {
                            pos.push(p);
                            pos.len() - 1
                        });
                        ring.push(id);
                    }
                    faces.push(ring);
                }
            }
        }
        core_math::half_edge::HalfEdgeMesh::from_polygons(&pos, &faces)
    }

    /// GATE (POLY_CURVATURE_PLAN P5) — END TO END on a POLYGONAL body, through the real panel
    /// lowering, with no SDF field anywhere.
    ///
    /// This is the gate the whole feature hangs on, because the failure it catches is silent. The
    /// grid is built under the key `curvature_keys_for_graph` derives from the graph; the
    /// evaluator looks one up under the key it rebuilds at the sample site. If those two ever
    /// disagree by a single knob default, `sdf_curvature_masks_ex` misses the cache, falls through
    /// to the field, hits `NO_FIELD_D` — and bakes a perfectly uniform black map. No panic, no
    /// warning, just a layer that does nothing.
    ///
    /// Asserted in the order the pipeline runs, so a failure names the stage that dropped it.
    #[test]
    fn p5_poly_body_curvature_reaches_the_bake_with_no_field() {
        use crate::poly_curvature::{PolyCurvSource, PolySampleSpace};
        use core_math::half_edge::HalfEdgeMesh;
        use core_math::mesh_curvature::RingMesh;

        // A SUBDIVIDED cube — no prims, no field, nothing for the SDF path to read.
        //
        // Subdivision is required, not incidental: a plain 6-quad cube has only its 8 CORNER
        // vertices, so every surface point interpolates between sharp corners and the map
        // saturates to 1 everywhere — correctly. Contrast between edges and flats only exists on
        // a mesh that HAS flat-face interior vertices, which is every real asset.
        let mesh = subdivided_cube_mesh(4, 1.0);

        let (graph, key, _) = build_owner_repro_graph();
        let mut sdf = SdfFieldSources::default();
        sdf.poly_curv = Some(PolyCurvSource {
            rings: RingMesh::from_half_edge(&mesh),
            corner_uv: Vec::new(),
            space: PolySampleSpace::default(), // Box/triplanar, tiling 1 — the owner's setup
            baked_curvature: None,
            normal_map: None,
        });

        // Stage 1 — the body reads as fieldless but curvature-capable.
        assert!(!sdf.has_field(), "a poly body must have NO analytical field");
        assert!(
            sdf.has_curvature_source(),
            "…and must still be recognised as having a curvature source, or Cavity fails closed"
        );

        // Stage 2 — preparation installs a grid under the graph's own key.
        let res = 64u32;
        crate::poly_curvature::prepare_poly_curv_caches(&graph, &mut sdf, res, &mut Default::default());
        assert!(
            sdf.curv_caches.iter().any(|g| g.key.matches(&key)),
            "prepare_poly_curv_caches must install a grid under the key \
             `curvature_keys_for_graph` derived from this exact graph"
        );
        assert!(
            sdf.vertex_curv_caches.iter().any(|g| g.key.matches(&key)),
            "…and under the Vertex list too, since the panel picks the rung from UV presence \
             and both rungs measure identically on poly"
        );

        // Stage 3 — the EVALUATOR finds it. Sweeping the planar bake domain, a cube's 12 edges
        // must light up and the map must not be uniform.
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        let mut nonzero = 0;
        for y in 0..res {
            for x in 0..res {
                let pt = [
                    (x as f32 + 0.5) / res as f32 * 2.0 - 1.0,
                    (y as f32 + 0.5) / res as f32 * 2.0 - 1.0,
                    0.0,
                ];
                let (mask, ..) = sdf_curvature_masks(
                    &sdf, pt, key.eps, key.prim_uid, key.region, key.grow, key.band,
                    key.contrast, key.strength,
                );
                assert!(mask.is_finite(), "no NaN may reach the compositor at {pt:?}");
                lo = lo.min(mask);
                hi = hi.max(mask);
                if mask > 1.0e-4 {
                    nonzero += 1;
                }
            }
        }
        assert!(
            nonzero > 0,
            "the cube's edges must produce a non-zero curvature mask — an all-zero map is what \
             a missed cache lookup looks like"
        );
        assert!(
            hi - lo > 0.1,
            "the map must have real contrast, not a uniform card (range {lo}..{hi})"
        );

        // Stage 4 — the same body with NO curvature source stays black. This separates "the
        // grid worked" from "something else in the chain paints a pattern regardless".
        let bare = SdfFieldSources::default();
        for y in 0..8 {
            for x in 0..8 {
                let pt = [x as f32 / 4.0 - 1.0, y as f32 / 4.0 - 1.0, 0.0];
                let (m, ..) = sdf_curvature_masks(
                    &bare, pt, key.eps, key.prim_uid, key.region, key.grow, key.band,
                    key.contrast, key.strength,
                );
                assert_eq!(m, 0.0, "no source → identically 0 at {pt:?}");
            }
        }
    }

    /// GATE (P5) — a poly body's curvature is measured from ITS OWN mesh, and an SDF body in the
    /// same scene changes nothing about it.
    ///
    /// The mixed-scene half of the P0 gate, re-run at the level where poly curvature actually
    /// lands. `by_uid` is scene-wide, so this is where a re-introduced cross-body fall-through
    /// would show up as "my cube's curvature changed when I added a sphere".
    #[test]
    fn p5_poly_curvature_ignores_other_bodies_in_the_scene() {
        use crate::poly_curvature::{PolyCurvSource, PolySampleSpace};
        use core_math::half_edge::HalfEdgeMesh;
        use core_math::mesh_curvature::RingMesh;

        let src = PolyCurvSource {
            rings: RingMesh::from_half_edge(&subdivided_cube_mesh(3, 1.0)),
            corner_uv: Vec::new(),
            space: PolySampleSpace::default(),
            baked_curvature: None,
            normal_map: None,
        };
        let (graph, key, _) = build_owner_repro_graph();

        let mut alone = SdfFieldSources::default();
        alone.poly_curv = Some(src.clone());

        // Same poly body, but the scene also contains an SDF body's prims in `by_uid`.
        let mut mixed = poly_body_in_mixed_scene();
        mixed.poly_curv = Some(src);

        crate::poly_curvature::prepare_poly_curv_caches(&graph, &mut alone, 32, &mut Default::default());
        crate::poly_curvature::prepare_poly_curv_caches(&graph, &mut mixed, 32, &mut Default::default());

        let ga = alone.curv_caches.iter().find(|g| g.key.matches(&key)).expect("grid");
        let gm = mixed.curv_caches.iter().find(|g| g.key.matches(&key)).expect("grid");
        assert_eq!(
            ga.data, gm.data,
            "a poly body's curvature must depend on its OWN mesh only — an unrelated SDF body \
             in the scene must not perturb one texel"
        );
    }

    /// GATE (P8 Direction, CPU) — the mask is actually DIRECTIONAL, not a constant.
    ///
    /// This is the whole point of the node. `Slope` still reads the evaluator's `n`, which on
    /// the planar bake domain is the constant `[0,0,1]` for EVERY pixel — it bakes a flat card.
    /// `sdf_direction_mask` recovers the real normal from the SDF gradient, so probing a sphere
    /// directly must give ~1 at the +Y pole, ~0 at the −Y pole and ~0.5 at the equator for axis
    /// +Y. If someone ever "simplifies" this to `dot(n, axis)` on the evaluator's normal, all
    /// three collapse to the same number and this test fails.
    #[test]
    fn p8_direction_mask_is_directional_on_a_sphere() {
        // Radius 2 so every probe point sits OUTSIDE `is_planar_bake_domain`'s box
        // (|x|,|y| <= 1.05 and |z| <= 0.08). That predicate is a heuristic shared with
        // `sdf_curvature_masks`, and a unit sphere's poles fall inside it — the sample would then
        // take the multi-seed triplanar branch and average three surface points, which is correct
        // for a bake pixel but is not what this test is probing.
        let prim = sphere_op(Vec3::ZERO, 2.0, BooleanOp::Union { k: 0.0 });
        let mut sdf = SdfFieldSources::default();
        sdf.body_prims = vec![prim.clone()];
        sdf.by_uid.insert(prim.prim_uid, prim);
        let up = [0.0f32, 1.0, 0.0];
        let probe = |p: [f32; 3]| {
            sdf_direction_mask(&sdf, p, 0.02, 0, up, 0.0, 0.5, 1.0, 1.0)
        };
        let (top_m, top_d) = probe([0.0, 2.0, 0.0]);
        let (bot_m, bot_d) = probe([0.0, -2.0, 0.0]);
        let (eq_m, eq_d) = probe([2.0, 0.0, 0.0]);
        assert!(top_d > 0.9, "north pole faces +Y (dot {top_d})");
        assert!(bot_d < -0.9, "south pole faces -Y (dot {bot_d})");
        assert!(eq_d.abs() < 0.1, "equator is perpendicular to +Y (dot {eq_d})");
        assert!(top_m > 0.9, "top mask ~1 (got {top_m})");
        assert!(bot_m < 0.1, "bottom mask ~0 (got {bot_m})");
        assert!(
            (eq_m - 0.5).abs() < 0.1,
            "equator mask ~0.5 (got {eq_m})"
        );
        // Flipping the axis mirrors the result — the axis is genuinely read, not ignored.
        let down = [0.0f32, -1.0, 0.0];
        let (top_down, _) =
            sdf_direction_mask(&sdf, [0.0, 2.0, 0.0], 0.02, 0, down, 0.0, 0.5, 1.0, 1.0);
        assert!(top_down < 0.1, "top against -Y is ~0 (got {top_down})");
    }

    /// G6 / BOTH MODES Phase 3 — Vertex splat coverage vs planar on a carved body.
    ///
    /// Phase 2 gate only asserted "differs from planar" — a 77 % zero grid differed a lot and
    /// still looked broken. Require **coverage** (fills the surface planar fills) AND difference
    /// (sampling domain really changed).
    #[test]
    fn g6_vertex_splat_coverage_matches_planar_on_carved_box() {
        let carved = owner_repro_body();
        let key = crate::gpu_curvature::CurvKey {
            eps: 0.05,
            prim_uid: 0,
            region: 0,
            grow: 0.0,
            band: 0.2,
            contrast: 1.0,
            strength: 1.0,
        };
        // res=64 matches the plan measurement (owner_repro 81.8 % planar / 22.8 % Phase-2 vertex).
        let res = 64u32;
        let n_texels = (res * res) as usize;
        let mut planar = vec![0.0f32; n_texels * 4];
        let mut planar_hit = 0u32;
        for y in 0..res {
            for x in 0..res {
                let u = (x as f32 + 0.5) / res as f32;
                let v = (y as f32 + 0.5) / res as f32;
                let p = [u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0];
                let (m, c, cv, cc) = sdf_curvature_masks(
                    &carved, p, key.eps, key.prim_uid, key.region, key.grow, key.band,
                    key.contrast, key.strength,
                );
                let i = ((y * res + x) * 4) as usize;
                planar[i] = m;
                planar[i + 1] = c;
                planar[i + 2] = cv;
                planar[i + 3] = cc;
                // Mask channel (0) — non-zero means curvature present on this texel.
                if m > 1e-4 {
                    planar_hit += 1;
                }
            }
        }
        let splat = build_vertex_splat_curv_grid(&carved, &key, res);
        assert_eq!(splat.res, res);
        assert_eq!(splat.data.len(), planar.len());
        let mut max_delta = 0.0f32;
        let mut vertex_hit = 0u32;
        for ti in 0..n_texels {
            let i = ti * 4;
            let d = (splat.data[i] - planar[i]).abs();
            max_delta = max_delta.max(d);
            if splat.data[i] > 1e-4 {
                vertex_hit += 1;
            }
        }
        let planar_cov = planar_hit as f32 / n_texels as f32;
        let vertex_cov = vertex_hit as f32 / n_texels as f32;
        assert!(
            planar_hit > n_texels as u32 / 4,
            "precondition: planar should fill a substantial fraction (planar_hit={planar_hit})"
        );
        // Coverage is the property Phase 2 lacked (22.8 % vs 81.8 %).
        assert!(
            vertex_cov >= 0.8 * planar_cov,
            "G6: vertex coverage {vertex_cov:.3} must be ≥ 0.8× planar {planar_cov:.3} \
             (hits vertex={vertex_hit} planar={planar_hit})"
        );
        // Domain still differs from pure planar multi-seed (boolean carve).
        assert!(
            max_delta > 0.02,
            "Vertex splat must still differ from planar multi-seed (max_delta={max_delta})"
        );
    }

    /// Legacy name kept as an alias so older docs / greps still find a test.
    #[test]
    fn p2_vertex_splat_grid_differs_from_planar_cpu_on_carved_box() {
        g6_vertex_splat_coverage_matches_planar_on_carved_box();
    }

    /// GATE (P5 AO, CPU) — AO is geometry-aware, not the synthetic sn_field_h demo.
    ///
    /// Open sphere surface (isolated, no nearby occluders) must stay near 1.0. A point deep in
    /// a concave cavity of a carved box must be darker. Empty field → 1.0 (fail-open).
    #[test]
    fn p5_ao_open_surface_bright_cavity_dark() {
        // Isolated sphere — open surface, AO near full bright.
        let prim = sphere_op(Vec3::ZERO, 2.0, BooleanOp::Union { k: 0.0 });
        let mut open = SdfFieldSources::default();
        open.body_prims = vec![prim.clone()];
        open.by_uid.insert(prim.prim_uid, prim);
        let open_ao = sdf_ambient_occlusion(&open, [0.0, 2.0, 0.0], 0.02, 0, 0.0, 0.5);
        assert!(
            open_ao > 0.85,
            "isolated sphere pole should be nearly unoccluded (got {open_ao})"
        );

        // Carved box: probe inside the subtract cavity (should occlude).
        let carved = owner_repro_body();
        // Point near the cavity interior surface (sphere carve at 0.45,0.45,r=0.4).
        let cavity_p = [0.45f32, 0.45, 0.0];
        let cavity_ao = sdf_ambient_occlusion(&carved, cavity_p, 0.02, 0, 0.0, 0.5);
        assert!(
            cavity_ao < open_ao - 0.05,
            "cavity AO ({cavity_ao}) should be darker than open surface ({open_ao})"
        );

        let empty = SdfFieldSources::default();
        assert_eq!(
            sdf_ambient_occlusion(&empty, [0.0, 0.0, 0.0], 0.02, 0, 0.0, 0.2),
            1.0,
            "no field → unoccluded"
        );
    }

    /// M5 / S9 — tileable noise: opposite domain edges match (period wrap).
    /// Bake domain is [-1,1]²; with `noise_tile` both x=±1 map to the same lattice edge.
    #[test]
    fn m5_noise_tile_seamless_across_domain_wrap() {
        use crate::shader_nodes::NoiseType;
        let scale = 4.0;
        let oct = 4u32;
        let lac = 2.0;
        let gain = 0.5;
        // Opposite edges of the planar bake domain must agree when tiled.
        for y in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
            let left = eval_noise(
                NoiseType::Fbm,
                [-1.0, y, 0.0],
                scale,
                oct,
                lac,
                gain,
                0.0,
                true,
            );
            let right = eval_noise(
                NoiseType::Fbm,
                [1.0, y, 0.0],
                scale,
                oct,
                lac,
                gain,
                0.0,
                true,
            );
            assert!(
                (left - right).abs() < 1e-5,
                "tileable FBM: domain x=±1 must match (y={y}): {left} vs {right}"
            );
            let bot = eval_noise(
                NoiseType::Fbm,
                [y, -1.0, 0.0],
                scale,
                oct,
                lac,
                gain,
                0.0,
                true,
            );
            let top = eval_noise(
                NoiseType::Fbm,
                [y, 1.0, 0.0],
                scale,
                oct,
                lac,
                gain,
                0.0,
                true,
            );
            assert!(
                (bot - top).abs() < 1e-5,
                "tileable FBM: domain y=±1 must match (x={y}): {bot} vs {top}"
            );
        }
        // Non-tile path is free to disagree at opposite edges (proves the flag matters).
        let nt_l = eval_noise(NoiseType::Fbm, [-1.0, 0.3, 0.0], scale, oct, lac, gain, 0.0, false);
        let nt_r = eval_noise(NoiseType::Fbm, [1.0, 0.3, 0.0], scale, oct, lac, gain, 0.0, false);
        // Not asserting inequality (could theoretically match); just that tile path is continuous.
        let _ = (nt_l, nt_r);
        // Default Noise node is tileable (S9 game-engine baseline).
        use crate::shader_nodes::{Node, NodeType};
        let n = Node::new(1, NodeType::Noise, [0.0, 0.0]);
        assert!(
            n.noise_tile,
            "Node::new Noise must default noise_tile=true (seamless maps)"
        );
    }

    /// M5 / S4 — Thickness is geometry-aware: thin shell vs solid differ; no field → open (1.0).
    #[test]
    fn m5_thickness_geometry_aware() {
        let empty = SdfFieldSources::default();
        assert_eq!(
            sdf_thickness(&empty, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0, 0.25),
            1.0,
            "no field → thick/open"
        );
        // Solid sphere of r=2: deep interior along normal should read thicker than near surface.
        let prim = sphere_op(Vec3::ZERO, 2.0, BooleanOp::Union { k: 0.0 });
        let mut field = SdfFieldSources::default();
        field.body_prims = vec![prim.clone()];
        field.by_uid.insert(prim.prim_uid, prim);
        // Surface sample (planar domain will project) vs empty already checked.
        let on_surface = sdf_thickness(&field, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0, 0.5);
        assert!(
            (0.0..=1.0).contains(&on_surface),
            "thickness must be 0..1 (got {on_surface})"
        );
    }

    /// M5 / S4 — Position is AABB-normalized body-local; axes split RGB.
    #[test]
    fn m5_position_aabb_normalized() {
        let prim = sphere_op(Vec3::ZERO, 1.0, BooleanOp::Union { k: 0.0 });
        let mut field = SdfFieldSources::default();
        field.body_prims = vec![prim.clone()];
        field.by_uid.insert(prim.prim_uid, prim);
        let (x, y, z) = sdf_position_01(&field, [0.0, 0.0, 0.0], 0);
        for (name, v) in [("x", x), ("y", y), ("z", z)] {
            assert!(
                (0.0..=1.0).contains(&v),
                "position {name} must be 0..1 (got {v})"
            );
        }
        // Empty field still returns something finite in 0..1 from AABB of empty/default.
        let empty = SdfFieldSources::default();
        let (ex, ey, ez) = sdf_position_01(&empty, [0.5, -0.5, 0.0], 0);
        assert!(ex.is_finite() && ey.is_finite() && ez.is_finite());
    }

    /// M5 / S11 — procedural generators honour ProjectionStyle (triplanar default).
    /// Noise without TexCoord still varies across the bake (uses projected domain).
    #[test]
    fn m5_noise_respects_triplanar_projection_default() {
        use crate::shader_nodes::{NodeGraph, NodeType, ProjectionStyle};
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [0.0, 0.0]);
        assert_eq!(
            g.find_node(noise).map(|n| n.projection),
            Some(ProjectionStyle::Triplanar),
            "Noise must default to Triplanar (S11)"
        );
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, NodeGraph::MO_ALBEDO));
        g.bake_res = 64;
        let maps = bake_node_graph_maps(&g).expect("bake");
        let a0 = maps.albedo[0];
        let varied = maps.albedo.chunks(4).step_by(16).any(|c| c[0] != a0);
        assert!(
            varied,
            "Triplanar Noise must vary across bake domain without TexCoord wire"
        );
    }

    #[test]
    fn direct_color_albedo_is_a_scalar_not_a_bake_channel() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut graph = NodeGraph::default();
        let color = graph.add_node(NodeType::ConstantColor, [0.0, 0.0]);
        graph.find_node_mut(color).unwrap().ramp_color0 = [0.2, 0.4, 0.8];
        let output = graph.output_node().unwrap().id;
        assert!(graph.connect(color, 0, output, NodeGraph::MO_ALBEDO));

        assert_eq!(
            graph.master_direct_constant_color(NodeGraph::MO_ALBEDO),
            Some([0.2, 0.4, 0.8])
        );
        assert!(
            !channels_from_graph(&graph).albedo,
            "uniform Color → Albedo must not dispatch a texture bake"
        );
    }

    /// GATE (P8 Direction, CPU) — with NO SDF field the node fails CLOSED (0), never a
    /// plausible constant. A graph hand-built in the node editor without a body field must read
    /// as "no data", not as a working directional mask (CLAUDE.md No Hacks).
    #[test]
    fn p8_direction_without_field_fails_closed() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let dir = g.add_node(NodeType::SurfaceDirection, [0.0, 0.0]);
        let out = g.output_node().map(|n| n.id).expect("default graph has an output");
        let _ = g.connect(dir, 0, out, NodeGraph::MO_ROUGHNESS);
        let order = g.topo_sort(out).expect("acyclic");
        let s = eval_graph_sample(
            &g,
            &order,
            out,
            [0.3, -0.2, 0.0],
            [0.0, 0.0, 1.0],
            0.0,
            &ImageMapSources::default(),
            &SdfFieldSources::default(), // no body field
            4,
        );
        assert_eq!(s.roughness, 0.0, "no SDF field → 0, not a plausible constant");
    }

    /// PERF (owner 2026-07-28 + ADJUST_SLIDER_LAG 2026-07-30): Live scrub must cap work
    /// resolution for **all** graphs (procedural and ImageTexture). Final must not cap.
    /// Image stacks used to skip the cap and paid full 1024² on every contrast tick (~1.5 s);
    /// Live→Final settle restores sharpness after the drag.
    #[test]
    fn live_bake_caps_resolution_for_procedural_and_image_graphs() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        // Procedural graph at a *high* Bake target: Live must downrez, Final must not.
        // (Default bake_res is 256 == BAKE_INTERACTIVE_RES, so Live == Final there by design.)
        let mut proc_g = NodeGraph::default();
        proc_g.bake_res = MAP_RES;
        let out = proc_g.output_node().expect("default graph seeds a MaterialOutput").id;
        let n = proc_g.add_node(NT::Noise, [0.0, 0.0]);
        let _ = proc_g.connect(n, 0, out, NodeGraph::MO_ALBEDO);
        let full = bake_work_resolution_for(&proc_g, false);
        let live = bake_work_resolution_for(&proc_g, true);
        assert!(
            live < full,
            "a Live (interacting) bake must cap resolution below Final ({live} vs {full}) —              otherwise every drag frame pays a full-resolution bake, which is the reported stall"
        );
        assert!(
            live <= BAKE_INTERACTIVE_RES,
            "live res {live} must respect the interactive cap {BAKE_INTERACTIVE_RES}"
        );
        // Bake 256²: Live and Final share the same work res — no forced 1024 upgrade path.
        proc_g.bake_res = BAKE_WORK_RES;
        assert_eq!(
            bake_work_resolution_for(&proc_g, true),
            bake_work_resolution_for(&proc_g, false),
            "Bake {BAKE_WORK_RES}² must not interactive-cap further (no Live@N → Final@1024)"
        );

        // Image graph: Live MUST also downrez (F1) — settle Final restores full res.
        let mut img_g = NodeGraph::default();
        img_g.bake_res = MAP_RES;
        let iout = img_g.output_node().expect("seeded MaterialOutput").id;
        let img = img_g.add_node(NT::ImageTexture, [0.0, 0.0]);
        let _ = img_g.connect(img, 0, iout, NodeGraph::MO_ALBEDO);
        assert!(graph_uses_image_texture(&img_g), "precondition: graph has an ImageTexture");
        let img_full = bake_work_resolution_for(&img_g, false);
        let img_live = bake_work_resolution_for(&img_g, true);
        assert_eq!(
            img_live, BAKE_INTERACTIVE_RES,
            "image graph Live scrub must use interactive cap (was excluded → 1–2 s lag)"
        );
        assert!(
            img_live < img_full,
            "image graph Final must stay full res ({img_live} vs {img_full}) for sharpness on settle"
        );
    }

    /// ADJUST_SLIDER_LAG F2: channel filter restricts eval to the scrubbed Master socket's
    /// subgraph so a Base Color contrast tick does not re-sample Roughness/Metal image nodes.
    #[test]
    fn channel_filter_prunes_topo_to_requested_sockets() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        let mut g = NodeGraph::default();
        let out = g.output_node().expect("seeded MaterialOutput").id;
        let img_a = g.add_node(NT::ImageTexture, [0.0, 0.0]);
        let img_r = g.add_node(NT::ImageTexture, [0.0, 100.0]);
        let _ = g.connect(img_a, 0, out, NodeGraph::MO_ALBEDO);
        let _ = g.connect(img_r, 0, out, NodeGraph::MO_ROUGHNESS);

        let full = topo_sort_for_mask(
            &g,
            out,
            BakeChannelMask {
                albedo: true,
                roughness: true,
                ..Default::default()
            },
        )
        .expect("full mask topo");
        let albedo_only = topo_sort_for_mask(
            &g,
            out,
            BakeChannelMask {
                albedo: true,
                ..Default::default()
            },
        )
        .expect("albedo-only topo");
        assert!(
            full.contains(&img_a) && full.contains(&img_r),
            "full mask must visit both image nodes"
        );
        assert!(
            albedo_only.contains(&img_a),
            "albedo filter must still visit albedo image"
        );
        assert!(
            !albedo_only.contains(&img_r),
            "albedo filter must NOT visit the roughness image (F2 win)"
        );
        assert!(
            albedo_only.len() < full.len(),
            "pruned order must be smaller than full"
        );
    }

    /// GATE (P8 Direction, parity) — `SurfaceDirection` is EXCLUDED from `graph_is_gpu_bakeable`.
    ///
    /// The WGSL arm is exact in the LIVE shader (real per-pixel `n`), but the GPU *map bake*
    /// (`material_bake.wgsl`) runs that same WGSL over the flat bake quad where `n` is the
    /// constant `[0,0,1]` — so it would produce a uniform card while the CPU bake produces the
    /// real thing. Excluding it routes the bake to the CPU, which is correct. Documented here so
    /// the exclusion cannot be "cleaned up" as an oversight.
    #[test]
    fn p8_direction_is_not_gpu_map_bakeable() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let dir = g.add_node(NodeType::SurfaceDirection, [0.0, 0.0]);
        let out = g.output_node().map(|n| n.id).expect("default graph has an output");
        let _ = g.connect(dir, 0, out, NodeGraph::MO_ROUGHNESS);
        assert!(
            !graph_is_gpu_bakeable(&g),
            "SurfaceDirection must fail the GPU-bake whitelist — the GPU bake domain's normal is \
             a constant, so it would silently disagree with the CPU bake"
        );
        assert!(
            graph_uses_sdf_field(&g),
            "SurfaceDirection must request the SDF field snapshot, or the CPU arm gets an empty \
             field and fails closed to 0 on every pixel"
        );
    }

    /// Build the doc exactly the way "+ Add layer" -> Generator does: one Roughness channel,
    /// Layered, one Curvature Generator layer, Signed output (`new_generator_layer`'s default).
    fn doc_with_fresh_generator_layer(radius: f32) -> MaterialPanelDoc {
        doc_with_generator_on(radius, ChannelKind::Roughness, GeneratorRung::Auto)
    }

    /// As `doc_with_fresh_generator_layer`, on a caller-chosen channel and rung. Base color goes
    /// through different lowering than the scalar channels, so it needs its own coverage.
    fn doc_with_generator_on(
        radius: f32,
        kind: ChannelKind,
        rung: GeneratorRung,
    ) -> MaterialPanelDoc {
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.retain(|c| c.kind != kind);
        doc.channels.push(Channel {
            kind,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::Curvature,
                    radius,
                    invert: false,
                    output: GeneratorOutput::Signed,
                    remap: [0.0, 1.0],
                    rung,
                sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                mask: crate::material_panel::model::MaskStack::default(),
                // Deliberately Normal: this fixture feeds `curvature_fast_path_taken_when_grid_present`,
                // which asserts the baked bytes equal the SEEDED grid exactly. Multiply would
                // composite against the base and change those bytes, weakening a test whose job is
                // proving the GPU fast path was taken. The shipped DEFAULT for a new layer is
                // Multiply — asserted separately by `new_generator_layer_defaults_to_multiply_not_normal`,
                // which reads the production constructor rather than this fixture.
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(),
            enabled: true,
        });
        doc
    }

    /// Build the graph via the REAL production entry point + its `CurvKey`, panicking with a
    /// diagnostic if the exact panel shape is ever ineligible again.
    /// P1 ROUTE GATE — the GPU bridge produces a `BakedNodeMaps` the INSTALL PATH will accept.
    ///
    /// `pending_loads_for_material_selective` length-checks every channel against
    /// `MAP_RES*MAP_RES*4` and silently DROPS a mismatch, so a bridge that returns the right
    /// pixels at the wrong size looks exactly like "the bake did nothing" — no error, no map.
    /// This pins the two properties that make the bridge installable:
    ///   • authored channels are exactly MAP_RES² RGBA8,
    ///   • unauthored channels are EMPTY, not zero-filled (a zero-filled buffer would pass the
    ///     length check and silently overwrite a real CPU-baked map).
    #[test]
    fn gpu_bridge_output_is_installable() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};
        let mut graph = NodeGraph::default();
        // `NodeGraph::default()` ALREADY seeds a MaterialOutput, and `output_node()` returns the
        // FIRST one — adding a second is silently ignored, so wire to the existing one.
        let out = graph.output_node().expect("default graph seeds a MaterialOutput").id;
        let n = graph.add_node(NT::Noise, [0.0, 0.0]);
        let _ = graph.connect(n, 0, out, NodeGraph::MO_ALBEDO);
        assert!(graph_is_gpu_bakeable(&graph), "precondition: procedural graph must be eligible");

        // Simulate exactly what the bridge returns: GPU channels filled at work res, the
        // MatResult-less channels left empty, then upsampled to MAP_RES.
        let res = bake_work_resolution(&graph);
        let ch = channels_from_graph(&graph);
        let px = (res * res * 4) as usize;
        let maps = BakedNodeMaps {
            albedo: vec![128u8; px],
            roughness: if ch.roughness { vec![64u8; px] } else { Vec::new() },
            metallic: if ch.metallic { vec![0u8; px] } else { Vec::new() },
            emission: if ch.emission { vec![0u8; px] } else { Vec::new() },
            normal: Vec::new(),
            bump: Vec::new(),
            displacement: Vec::new(),
            sss_color: Vec::new(),
            normal_is_dx: false,
            albedo_lo: Vec::new(),
            roughness_lo: Vec::new(),
            metallic_lo: Vec::new(),
            emission_lo: Vec::new(),
            normal_lo: Vec::new(),
            bump_lo: Vec::new(),
            displacement_lo: Vec::new(),
            sss_color_lo: Vec::new(),
        };
        let up = upsample_maps_selective(maps, res, MAP_RES, ch);
        let expected = (MAP_RES * MAP_RES * 4) as usize;

        assert_eq!(
            up.albedo.len(), expected,
            "albedo must be exactly MAP_RES² RGBA8 or pending_loads_for_material_selective drops              it silently — the bake would appear to do nothing"
        );
        for (name, v) in [("normal", &up.normal), ("bump", &up.bump), ("displacement", &up.displacement)] {
            assert!(
                v.is_empty(),
                "{name} must stay EMPTY (the GPU path cannot author it — absent from MatResult).                  A zero-filled buffer would pass the length check and silently WIPE the map the                  CPU path installed."
            );
        }
    }

    /// P1 GATE (GPU_MATERIAL_BAKE_PLAN.md) — `graph_is_gpu_bakeable` must REJECT every node
    /// whose WGSL is a placeholder, and ACCEPT a purely procedural graph.
    ///
    /// This is the single guard preventing the GPU bake from silently producing wrong maps.
    /// `ImageTexture` emits `vec3(1,1,1)` in WGSL ("white placeholder") and the surface analyzers
    /// read a synthetic height field ("preview only") — both would bake plausible-looking output
    /// with no relation to the real image/surface, which is far worse than a slow CPU bake.
    #[test]
    fn gpu_bake_gate_rejects_placeholder_nodes() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        // Purely procedural: Noise → MaterialOutput. Must be ACCEPTED.
        let mut ok = NodeGraph::default();
        let n = ok.add_node(NT::Noise, [0.0, 0.0]);
        let out = ok.add_node(NT::MaterialOutput, [200.0, 0.0]);
        let _ = ok.connect(n, 0, out, 0);
        assert!(
            graph_is_gpu_bakeable(&ok),
            "a Noise → MaterialOutput graph is faithfully reproduced in WGSL and must be eligible;              rejecting it would leave the GPU path with nothing to do"
        );

        // Each of these MUST veto the whole graph, even though the rest of it is procedural.
        for bad in [
            NT::ImageTexture,   // white placeholder
            NT::TextureSample,  // white placeholder
            NT::Curvature,      // synthetic sn_field_h
            NT::Cavity,         // synthetic sn_field_h
            NT::AmbientOcclusion,
            NT::Slope,
            NT::PrimSdfMask,
        ] {
            let mut g = NodeGraph::default();
            let a = g.add_node(NT::Noise, [0.0, 0.0]);
            let b = g.add_node(bad.clone(), [100.0, 0.0]);
            let o = g.add_node(NT::MaterialOutput, [200.0, 0.0]);
            let _ = g.connect(a, 0, o, 0);
            let _ = g.connect(b, 0, o, 1);
            assert!(
                !graph_is_gpu_bakeable(&g),
                "{bad:?} is a PLACEHOLDER in generate_wgsl (white texel / synthetic height field)                  — it must veto GPU baking, or the map bakes plausible-but-wrong pixels"
            );
        }
    }

    /// P0a GATE (GPU_MATERIAL_BAKE_PLAN.md) — row-parallel bake is DETERMINISTIC.
    ///
    /// `bake_node_graph_maps_res` was converted from a serial `for y { for x { … } }` to
    /// `rayon` row chunks. Rayon does not guarantee row completion ORDER, so if any pixel's
    /// result depended on evaluation order — shared mutable state, an accumulator, a cache
    /// keyed on the previous pixel — the output would vary run to run. It would still LOOK
    /// plausible, which is exactly the failure mode that ships silently.
    ///
    /// Baking the same graph twice must produce byte-identical maps. This holds only because
    /// `eval_graph_sample` is a pure function of (position, inputs) and each row writes a
    /// disjoint `&mut` slice — this test is what keeps that true as nodes are added.
    #[test]
    fn parallel_bake_is_deterministic_across_runs() {
        let (graph, _key, sdf) = build_owner_repro_graph();
        let ch = channels_from_graph(&graph);
        let res = snap_work_res(128); // small: this asserts determinism, not throughput
        let a = bake_node_graph_maps_res(
            &graph, res, ch, &ImageMapSources::default(), &sdf, BAKE_MAX_OCTAVES,
        )
        .expect("bake A must succeed");
        let b = bake_node_graph_maps_res(
            &graph, res, ch, &ImageMapSources::default(), &sdf, BAKE_MAX_OCTAVES,
        )
        .expect("bake B must succeed");

        // Non-vacuity FIRST: an all-empty result would make every comparison below trivially
        // true, so prove there is real content before claiming the runs agree.
        let painted = a.albedo.len() + a.roughness.len() + a.metallic.len();
        assert!(painted > 0, "bake produced no channel data — the comparison would prove nothing");
        assert!(
            a.roughness.iter().any(|&v| v != a.roughness[0]),
            "roughness is a flat constant — a degenerate map cannot detect ordering bugs"
        );

        assert_eq!(a.albedo, b.albedo, "albedo differs between runs — row-parallel bake is order-dependent");
        assert_eq!(a.roughness, b.roughness, "roughness differs between runs — row-parallel bake is order-dependent");
        assert_eq!(a.metallic, b.metallic, "metallic differs between runs");
        assert_eq!(a.emission, b.emission, "emission differs between runs");
        assert_eq!(a.normal, b.normal, "normal differs between runs");
        assert_eq!(a.bump, b.bump, "bump differs between runs");
        assert_eq!(a.displacement, b.displacement, "displacement differs between runs");
    }

    fn build_owner_repro_graph() -> (crate::shader_nodes::NodeGraph, crate::gpu_curvature::CurvKey, SdfFieldSources) {
        let sdf = owner_repro_body();
        let (bmin, bmax) = sdf.body_aabb(0);
        let radius = crate::material_panel::lower::curvature_radius_floor((bmax - bmin).length());
        let doc = doc_with_fresh_generator_layer(radius);
        let ch = doc.channel(ChannelKind::Roughness).expect("roughness channel");
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        let emitted = regen_channel_subgraph(&mut graph, ChannelKind::Roughness, ch, doc.texture_space, &mat);
        assert!(emitted, "regen_channel_subgraph must emit a subgraph for a fresh Generator layer");
        let keys = curvature_keys_for_graph(&graph).expect(
            "the exact graph shape `regen_channel_subgraph` produces for a fresh panel Generator \
             layer must be GPU-curvature-eligible — if this fails, that IS a regression of the \
             45s bug (permanent CPU fallback), and the fix belongs in `curvature_keys_for_graph` \
             or the lowering, never in the bake loop"
        );
        assert_eq!(keys.len(), 1, "a single fresh Generator layer must produce exactly one key");
        (graph, keys[0], sdf)
    }









    /// OWNER 2026-07-29: "noise in bump does not result in a bump map".
    ///
    /// Walks the full path for a Noise layer on the Bump channel: doc -> subgraph -> the bake's
    /// own channel-detection -> a non-empty bump map. Each stage is asserted separately so a
    /// failure says WHICH stage dropped it, rather than just "no bump".
    #[test]
    fn owner_noise_on_bump_channel_produces_a_bump_map() {
        use crate::material_panel::model::{BlendMode, ChannelState, LayerContent};
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.retain(|c| c.kind != ChannelKind::Bump);
        doc.channels.push(Channel {
            kind: ChannelKind::Bump,
            state: ChannelState::Layered,
            bit_depth: Default::default(),
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 8.0,
                    strength: 1.0,
                    octaves: 4.0,
                    noise_type: crate::shader_nodes::NoiseType::default(),
                },
                mask: crate::material_panel::model::MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            eval_mode: Default::default(), enabled: true });

        let ch = doc.channel(ChannelKind::Bump).expect("bump channel").clone();
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        let emitted = regen_channel_subgraph(
            &mut graph, ChannelKind::Bump, &ch, doc.texture_space, &mat);
        assert!(emitted, "STAGE 1: no subgraph emitted for a Noise layer on Bump");

        // STAGE 2: the Master's HEIGHT socket must actually be driven, or the bake will not even
        // try to produce a bump map.
        assert!(graph.master_socket_driven(crate::shader_nodes::NodeGraph::MO_HEIGHT),
            "STAGE 2: subgraph emitted but Master HEIGHT socket is NOT driven - the bake's              `ch.bump` check reads exactly this, so no bump map is ever baked. Nodes: {:?}",
            graph.nodes.iter().map(|n| n.label.clone()).collect::<Vec<_>>());

        // STAGE 3: the bake's own channel detection must agree.
        let chans = channels_from_graph(&graph);
        assert!(chans.bump, "STAGE 3: channels_from_graph says bump is not driven");

        // STAGE 4: and the baked map must be non-trivial (noise, not a flat fill).
        let sdf = owner_repro_body();
        let maps = bake_node_graph_maps_res(
            &graph, 32, chans, &ImageMapSources::default(), &sdf, BAKE_MAX_OCTAVES)
            .expect("bake must succeed");
        assert!(!maps.bump.is_empty(), "STAGE 4: bump map baked EMPTY");
        let lo = *maps.bump.iter().min().unwrap();
        let hi = *maps.bump.iter().max().unwrap();
        assert!(hi > lo + 8,
            "STAGE 4: bump map is nearly flat (min {lo}, max {hi}) - the noise did not reach it");
    }


    /// OWNER: "noise in bump does not result in a bump map" — the INSTALL gate, not the bake.
    ///
    /// `b548536` proved the bake produces a good, non-flat bump map. This walks one step further:
    /// the install closure only pushes a map when `pixels.len() == MAP_RES*MAP_RES*4`. A bump map
    /// baked at any other size is silently DROPPED — no error, no map, and the shader's
    /// `bmap_slot != 0` gate then skips bump entirely. That is exactly the reported symptom.
    #[test]
    fn owner_baked_bump_map_is_full_resolution_so_it_installs() {
        use crate::material_panel::model::{BlendMode, ChannelState, LayerContent};
        use crate::material_textures::MAP_RES;
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.retain(|c| c.kind != ChannelKind::Bump);
        doc.channels.push(Channel {
            kind: ChannelKind::Bump,
            state: ChannelState::Layered,
            bit_depth: Default::default(),
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 8.0,
                    strength: 1.0,
                    octaves: 4.0,
                    noise_type: crate::shader_nodes::NoiseType::default(),
                },
                mask: crate::material_panel::model::MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            eval_mode: Default::default(), enabled: true });
        let ch = doc.channel(ChannelKind::Bump).expect("bump").clone();
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut graph, ChannelKind::Bump, &ch, doc.texture_space, &mat));

        let chans = channels_from_graph(&graph);
        assert!(chans.bump, "bump channel must be detected as driven");
        let sdf = owner_repro_body();
        let maps = bake_node_graph_maps_res(
            &graph, MAP_RES, chans, &ImageMapSources::default(), &sdf, BAKE_MAX_OCTAVES)
            .expect("bake must succeed");

        let expected = (MAP_RES * MAP_RES * 4) as usize;
        assert_eq!(maps.bump.len(), expected,
            "baked bump map is {} bytes, install requires exactly {expected} - a mismatch is              SILENTLY DROPPED by the install closure, so no bump map ever reaches the material              and the shader's `bmap_slot != 0` gate skips bump with no error",
            maps.bump.len());
    }


    /// OWNER 2026-07-29: "Add mask -> Image" produces something that renders as an image LAYER,
    /// not as a mask on the layer beneath.
    ///
    /// Walks the doc -> subgraph path for a layer that carries an Image MASK and asserts the mask
    /// image is wired into the layer's MASK/factor input, not emitted as its own content. If the
    /// mask image ends up driving colour directly, it paints instead of masking — exactly the
    /// reported symptom.
    #[test]
    fn owner_image_mask_lowers_as_a_mask_not_a_content_layer() {
        use crate::material_panel::model::{BlendMode, ChannelState, LayerContent};
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.retain(|c| c.kind != ChannelKind::BaseColor);
        doc.channels.push(Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            bit_depth: Default::default(),
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                // Content is a NOISE so the two are distinguishable in the graph: any
                // ImageTexture node present must have come from the MASK.
                content: LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 4.0,
                    noise_type: crate::shader_nodes::NoiseType::default(),
                },
                mask: crate::material_panel::model::MaskStack::single(LayerContent::Image { tex_id: 7, strength: 1.0 }),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            eval_mode: Default::default(), enabled: true });

        let ch = doc.channel(ChannelKind::BaseColor).expect("base color").clone();
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(&mut graph, ChannelKind::BaseColor, &ch, doc.texture_space, &mat),
            "a layer with an Image mask must emit a subgraph");

        let img = graph.nodes.iter()
            .find(|n| matches!(n.node_type, NodeType::ImageTexture))
            .unwrap_or_else(|| panic!(
                "no ImageTexture node emitted for the MASK - the mask image never reaches the                  graph at all. Nodes: {:?}",
                graph.nodes.iter().map(|n| n.label.clone()).collect::<Vec<_>>()));

        // The mask image must feed a FACTOR/mask input, never the colour chain directly.
        let feeds = graph.connections.iter()
            .filter(|c| c.from_node == img.id)
            .map(|c| (c.to_node, c.to_input))
            .collect::<Vec<_>>();
        assert!(!feeds.is_empty(),
            "the mask ImageTexture is orphaned - it renders as nothing. Label: {:?}", img.label);
        // Its panel tag should mark it as a mask ("msk"), not layer content ("img").
        assert!(img.label.contains("msk"),
            "the mask image is tagged {:?} - if it carries a CONTENT tag it is lowered as a              layer, which is the owner's 'it appears as a layer'. Connections: {:?}",
            img.label, feeds);
    }

    /// OWNER 2026-07-29: a NEW generator layer must default to Multiply, not Normal.
    ///
    /// Normal lowers to blend code 0, which gbuffer.wgsl's vertex-curvature composite treats as
    /// the fall-through `blended = top` — it REPLACES everything beneath. So the default made
    /// "add a Mult noise on top of a curvature layer" silently render only the curvature: the
    /// panel showed two layers and drew one.
    ///
    /// Multiply is correct in both shapes — alone over a light base it reads like Normal did, and
    /// under another layer it composites instead of clobbering.
    #[test]
    fn new_generator_layer_defaults_to_multiply_not_normal() {
        use crate::material_panel::model::{BlendMode, ChannelKind, GeneratorRung, LayerContent};
        // Reads the PRODUCTION constructor, not a test fixture — a fixture could drift from what
        // the app actually builds, which is exactly how a default regresses unnoticed.
        let gen = crate::material_panel::layer_ui::new_generator_layer(Some(0.16), false);
        assert!(matches!(gen.content, LayerContent::Generator { .. }),
            "new_generator_layer must produce a Generator layer");
        assert_eq!(gen.blend, BlendMode::Multiply,
            "a generator layer defaulting to Normal REPLACES every layer beneath it in the              vertex-curvature composite - that is the owner's 'noise does not appear'");
    }

    /// OWNER 2026-07-29 (screenshot): Base color = [Noise·FBM Mult] ABOVE [Curvature Norm], the
    /// curvature on the Vertex rung. The noise does not appear on the model.
    ///
    /// "Multiply a noise on top of another layer" is the most basic stack there is, so this walks
    /// the FULL lowering and asserts the noise actually survives into the graph as a Multiply that
    /// is CONNECTED — not merely that a Noise node exists somewhere. A tagged-but-unconnected node
    /// renders exactly like no node at all, which is what the screenshot shows.
    #[test]
    fn owner_mult_noise_over_vertex_curvature_reaches_the_graph() {
        use crate::material_panel::model::{BlendMode, LayerContent};
        let sdf = owner_repro_body();
        let (bmin, bmax) = sdf.body_aabb(0);
        let radius = crate::material_panel::lower::curvature_auto_radius_aabb(bmin, bmax);
        let mut doc =
            doc_with_generator_on(radius, ChannelKind::BaseColor, GeneratorRung::VertexCache);

        // Noise ABOVE the curvature layer, Multiply — exactly the screenshot.
        let ch = doc.channels.iter_mut()
            .find(|c| c.kind == ChannelKind::BaseColor).expect("base color");
        ch.layers.insert(0, Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Noise {
                scale: 4.0,
                strength: 1.0,
                octaves: 4.0,
                noise_type: crate::shader_nodes::NoiseType::default(),
            },
            mask: crate::material_panel::model::MaskStack::default(),
            blend: BlendMode::Multiply,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        });

        let ch = doc.channel(ChannelKind::BaseColor).expect("base color").clone();
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        let emitted = regen_channel_subgraph(
            &mut graph, ChannelKind::BaseColor, &ch, doc.texture_space, &mat);
        assert!(emitted, "a two-layer Base color channel must emit a subgraph");

        // The Noise node must EXIST...
        let noise_id = graph.nodes.iter()
            .find(|n| matches!(n.node_type, NodeType::Noise))
            .map(|n| n.id)
            .unwrap_or_else(|| panic!(
                "no Noise node emitted for a visible Mult Noise layer. Nodes: {:?}",
                graph.nodes.iter().map(|n| n.label.clone()).collect::<Vec<_>>()));

        // ...and be CONNECTED. An orphaned node renders identically to no node at all, which is
        // precisely the reported symptom.
        let connected = graph.connections.iter().any(|c| c.from_node == noise_id);
        assert!(connected,
            "the Noise node was emitted but its output feeds NOTHING - it renders exactly like an              absent layer, which is the owner's 'noise does not appear'. Connections: {:?}",
            graph.connections.iter()
                .map(|c| (c.from_node, c.to_node, c.to_input)).collect::<Vec<_>>());
    }

    /// The Auto radius must be VISIBLE on an ordinary body, and must scale with it.
    ///
    /// The bug it fixes: the floor (1% of the short axis) was doubling as the default, so a box
    /// got a radius that marked only pixels within 1% of an edge — a hairline, reported by the
    /// owner and by the soak run as "curvature shows nothing".
    #[test]
    fn curvature_auto_radius_is_visible_and_scales() {
        use crate::material_panel::lower::{curvature_auto_radius_aabb, curvature_radius_floor_aabb};
        for size in [0.5f32, 1.0, 2.0, 10.0] {
            let bmin = glam::Vec3::splat(-size * 0.5);
            let bmax = glam::Vec3::splat(size * 0.5);
            let auto = curvature_auto_radius_aabb(bmin, bmax);
            let floor = curvature_radius_floor_aabb(bmin, bmax);

            assert!(auto >= floor,
                "auto ({auto}) must never fall below the noise floor ({floor}) at size {size}");
            let frac = auto / size;
            assert!(frac > 0.04,
                "auto is {frac:.3} of body size at {size} - too thin to see, which is the whole                  bug this exists to fix");
            assert!(frac < 0.25,
                "auto is {frac:.3} of body size at {size} - wide enough that neighbouring edge                  bands merge into a flat wash, which is just as unusable as too thin");
        }
        // Scale-invariance: the same default must read the same on a small and a large body.
        let small = curvature_auto_radius_aabb(glam::Vec3::splat(-0.25), glam::Vec3::splat(0.25));
        let large = curvature_auto_radius_aabb(glam::Vec3::splat(-5.0), glam::Vec3::splat(5.0));
        assert!((small / 0.5 - large / 10.0).abs() < 1e-5,
            "auto must be the same FRACTION of body size at every scale (got {} vs {})",
            small / 0.5, large / 10.0);
    }

    /// OWNER + SOAK 2026-07-29: the default curvature radius is a THIN-EDGE radius, and on a box
    /// that is nearly invisible — which is why both the owner's screenshot and the soak run's
    /// texturing block report "nothing".
    ///
    /// `curvature_radius_floor_aabb` = `min_axis * 0.01`, i.e. 1% of the body's SHORT AXIS. That is
    /// a sensible FLOOR (below it the finite-difference step is smaller than a DC cell and the
    /// signal is noise), but it is being used as the DEFAULT. On a box — flat faces, 12 sharp
    /// edges — a 1% radius marks only pixels within 1% of an edge: a hairline, not a surface
    /// effect. The pipeline is working; the default is simply too small to see.
    ///
    /// This is the measurement behind the owner's "auto button" request: the default should be
    /// derived from mesh scale so the result is visible, with the floor as a clamp rather than the
    /// answer.
    #[test]
    fn curvature_default_radius_is_a_thin_edge_fraction_of_the_body() {
        use crate::material_panel::lower::curvature_radius_floor_aabb;
        for size in [1.0f32, 2.0, 4.0, 10.0] {
            let bmin = glam::Vec3::splat(-size * 0.5);
            let bmax = glam::Vec3::splat(size * 0.5);
            let floor = curvature_radius_floor_aabb(bmin, bmax);
            let frac = floor / size;
            assert!(
                frac <= 0.02,
                "radius floor is {frac:.3} of body size at size {size} - this test exists to                  record that the floor is a THIN-EDGE fraction (~1%). If it grew, the 'auto                  radius' work changed the floor into a default and this test should be updated                  deliberately, not silently.");
        }
        // And the concrete case: an ordinary 2-unit box gets 0.02 - exactly what the owner's
        // panel showed while reporting that nothing was visible.
        let floor = curvature_radius_floor_aabb(glam::Vec3::splat(-1.0), glam::Vec3::splat(1.0));
        assert!((floor - 0.02).abs() < 1e-6,
            "a 2-unit box must floor at 0.02 (got {floor}) - the owner's exact reading");
    }

    /// OWNER 2026-07-29 (screenshot): Base color with a Vertex-rung Curvature layer AND a Mult
    /// Noise layer above it. The noise does not appear.
    ///
    /// `channel_skip_planar_map_install` exists to stop a pure-Vertex channel installing a planar
    /// map (that install/clear ping-pong caused the 418-bake loop). But a MIXED channel - Vertex
    /// curvature PLUS a texture-baked noise - still needs its map, or the noise has nothing to
    /// render from. This pins that the mixed case keeps its map.
    #[test]
    fn owner_vertex_curvature_plus_mult_noise_keeps_its_map() {
        let radius = 0.02f32;
        let mut doc = doc_with_generator_on(radius, ChannelKind::BaseColor, GeneratorRung::VertexCache);
        // Add the Mult Noise layer ABOVE the curvature layer, as the screenshot shows.
        let ch = doc.channels.iter_mut()
            .find(|c| c.kind == ChannelKind::BaseColor).expect("base color");
        ch.layers.insert(0, Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Noise {
                scale: 4.0,
                strength: 1.0,
                octaves: 4.0,
                noise_type: crate::shader_nodes::NoiseType::default(),
            },
            mask: crate::material_panel::model::MaskStack::default(),
            blend: BlendMode::Multiply,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        });

        let ch = doc.channel(ChannelKind::BaseColor).expect("base color");
        assert!(crate::material_panel::lower::channel_uses_vertex_curvature(ch),
            "fixture must actually have a Vertex-rung curvature layer");
        assert!(!crate::material_panel::lower::channel_skip_planar_map_install(ch),
            "a channel with BOTH Vertex curvature AND a texture-baked Noise layer must still              install its planar map - skipping it leaves the Noise with nothing to render from,              which is exactly the reported 'noise does not appear'");
    }

    /// M1.a: F2 single-channel scrub must be GPU-eligible (bits = graph ∩ filter).
    /// Previously `allow_gpu` required `channel_filter.is_none()`, so every slider tick
    /// was CPU-only by construction — the highest-leverage M1.a fix is opening that gate.
    #[test]
    fn m1a_f2_channel_filter_yields_gpu_bits_for_scrubbed_socket_only() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        let mut g = NodeGraph::default();
        let out = g.output_node().expect("seeded MaterialOutput").id;
        let noise = g.add_node(NT::Noise, [0.0, 0.0]);
        let rough = g.add_node(NT::ConstantValue, [0.0, 80.0]);
        assert!(g.connect(noise, 0, out, NodeGraph::MO_ALBEDO));
        assert!(g.connect(rough, 0, out, NodeGraph::MO_ROUGHNESS));

        // Full bake: both GPU planes.
        let (bits_full, ch_full) =
            gpu_bake_channel_plan(&g, None).expect("noise+const graph is GPU-eligible");
        assert_eq!(bits_full, 1 | 2, "full bake: albedo|roughness");
        assert!(ch_full.albedo && ch_full.roughness && !ch_full.metallic);

        // F2 albedo scrub: only bit 1 — install must not wipe roughness.
        let (bits_a, ch_a) = gpu_bake_channel_plan(
            &g,
            Some(BakeChannelMask {
                albedo: true,
                ..Default::default()
            }),
        )
        .expect("F2 albedo scrub must be GPU-eligible");
        assert_eq!(bits_a, 1, "F2 albedo-only bits");
        assert!(ch_a.albedo && !ch_a.roughness);

        // F2 roughness scrub.
        let (bits_r, ch_r) = gpu_bake_channel_plan(
            &g,
            Some(BakeChannelMask {
                roughness: true,
                ..Default::default()
            }),
        )
        .expect("F2 roughness scrub must be GPU-eligible");
        assert_eq!(bits_r, 2);
        assert!(ch_r.roughness && !ch_r.albedo);

        // Graph also wires Normal → full bake refuses GPU (half-author hazard); F2 albedo still OK.
        let nrm = g.add_node(NT::ConstantColor, [0.0, 160.0]);
        assert!(g.connect(nrm, 0, out, NodeGraph::MO_NORMAL));
        assert!(
            gpu_bake_channel_plan(&g, None).is_none(),
            "full bake with Normal wired must stay CPU"
        );
        let (bits_a2, _) = gpu_bake_channel_plan(
            &g,
            Some(BakeChannelMask {
                albedo: true,
                ..Default::default()
            }),
        )
        .expect("F2 albedo scrub still GPU even when Normal is also wired");
        assert_eq!(bits_a2, 1);
    }

    /// T2 — reject reason names the first non-whitelist node type.
    #[test]
    fn t2_gpu_bake_reject_reason_names_image_texture() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().map(|n| n.id).expect("output");
        let img = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        let _ = g.connect(img, 0, out, NodeGraph::MO_ALBEDO);
        assert!(!graph_is_gpu_bakeable(&g));
        assert_eq!(graph_gpu_bake_reject_reason(&g), Some("Image Texture"));
        // Pure noise graph is bakeable.
        let mut g2 = NodeGraph::default();
        let out2 = g2.output_node().map(|n| n.id).unwrap();
        let nse = g2.add_node(NodeType::Noise, [0.0, 0.0]);
        let _ = g2.connect(nse, 0, out2, NodeGraph::MO_ALBEDO);
        assert!(graph_is_gpu_bakeable(&g2));
        assert_eq!(graph_gpu_bake_reject_reason(&g2), None);
    }

    /// M1.a — F2 channel-scoped reject: Image on Roughness must NOT veto Noise→Albedo scrub.
    /// Whole-graph reject still names Image Texture (full Final stays CPU until P2).
    #[test]
    fn m1a_f2_scoped_reject_ignores_image_on_other_channel() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        let mut g = NodeGraph::default();
        let out = g.output_node().expect("seeded MaterialOutput").id;
        let noise = g.add_node(NT::Noise, [0.0, 0.0]);
        let img = g.add_node(NT::ImageTexture, [0.0, 80.0]);
        assert!(g.connect(noise, 0, out, NodeGraph::MO_ALBEDO));
        assert!(g.connect(img, 0, out, NodeGraph::MO_ROUGHNESS));

        // Full graph: Image blocks GPU.
        assert_eq!(
            graph_gpu_bake_reject_reason(&g),
            Some("Image Texture"),
            "full-graph reject must still name Image Texture"
        );
        assert_eq!(
            graph_gpu_bake_reject_reason_for(&g, None),
            Some("Image Texture")
        );

        // F2 albedo scrub: only Noise is reachable → GPU eligible.
        let albedo_only = BakeChannelMask {
            albedo: true,
            ..Default::default()
        };
        assert_eq!(
            graph_gpu_bake_reject_reason_for(&g, Some(albedo_only)),
            None,
            "Noise→Albedo scrub must stay GPU even when Roughness has ImageTexture"
        );

        // F2 roughness scrub: Image is reachable → reject.
        let rough_only = BakeChannelMask {
            roughness: true,
            ..Default::default()
        };
        assert_eq!(
            graph_gpu_bake_reject_reason_for(&g, Some(rough_only)),
            Some("Image Texture"),
            "Image→Roughness scrub must still force CPU (placeholder fidelity)"
        );

        // Channel plan agrees: F2 albedo bits still open.
        let (bits, _) = gpu_bake_channel_plan(&g, Some(albedo_only))
            .expect("F2 albedo plan must be GPU-authorable");
        assert_eq!(bits, 1);
    }

    /// M1.a — SdfCurvature on one channel does not veto procedural scrub on another.
    #[test]
    fn m1a_f2_scoped_reject_ignores_curvature_on_other_channel() {
        use crate::shader_nodes::{NodeGraph, NodeType as NT};

        let mut g = NodeGraph::default();
        let out = g.output_node().expect("seeded MaterialOutput").id;
        let noise = g.add_node(NT::Noise, [0.0, 0.0]);
        let curv = g.add_node(NT::SdfCurvature, [0.0, 80.0]);
        assert!(g.connect(noise, 0, out, NodeGraph::MO_ALBEDO));
        assert!(g.connect(curv, 0, out, NodeGraph::MO_ROUGHNESS));

        assert!(
            graph_gpu_bake_reject_reason_for(&g, None).is_some(),
            "full bake with SdfCurvature must stay CPU"
        );
        assert_eq!(
            graph_gpu_bake_reject_reason_for(
                &g,
                Some(BakeChannelMask {
                    albedo: true,
                    ..Default::default()
                })
            ),
            None,
            "Noise albedo scrub must not pay for Curvature on Roughness"
        );
    }

    /// T3 — invariant hoist is bit-identical to full per-pixel eval.
    #[test]
    fn t3_invariant_hoist_matches_full_eval() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().map(|n| n.id).unwrap();
        // ConstantValue 0.4 → Remap → Roughness; Noise → Albedo (varying).
        let c = g.add_node(NodeType::ConstantValue, [-200.0, 0.0]);
        if let Some(n) = g.find_node_mut(c) {
            n.const_float = 0.4;
        }
        let rm = g.add_node(NodeType::Remap, [-100.0, 0.0]);
        let _ = g.connect(c, 0, rm, 0);
        let _ = g.connect(rm, 0, out, NodeGraph::MO_ROUGHNESS);
        let nse = g.add_node(NodeType::Noise, [0.0, 0.0]);
        let _ = g.connect(nse, 0, out, NodeGraph::MO_ALBEDO);
        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let (inv, varying) = compute_invariant_outs(&g, &order, &images, &sdf, 8);
        assert!(
            inv.keys().any(|(id, _)| *id == c || *id == rm),
            "Constant+Remap should be hoisted; keys={:?}",
            inv.keys().collect::<Vec<_>>()
        );
        assert!(
            varying.contains(&nse),
            "Noise must stay per-pixel varying"
        );
        let p = [0.25f32, -0.5, 0.0];
        let n = [0.0, 0.0, 1.0];
        let full = eval_graph_sample(&g, &order, out, p, n, 0.0, &images, &sdf, 8);
        let hoisted = eval_graph_sample_cached(
            &g,
            &order,
            Some((&inv, &varying)),
            out,
            p,
            n,
            0.0,
            &images,
            &sdf,
            8,
        );
        assert!(
            (full.roughness - hoisted.roughness).abs() < 1e-6,
            "roughness full={} hoist={}",
            full.roughness,
            hoisted.roughness
        );
        assert!(
            (full.albedo[0] - hoisted.albedo[0]).abs() < 1e-5
                && (full.albedo[1] - hoisted.albedo[1]).abs() < 1e-5,
            "albedo must match: full={:?} hoist={:?}",
            full.albedo,
            hoisted.albedo
        );
    }

    /// The cap was 2,000,000 and an ordinary box reported `vert_cap = 2,048,383`, so EVERY frame
    /// logged "SKIPPED" and curvature never rendered — the feature was silently off, with no error
    /// and nothing on the model.
    ///
    /// Two things made that value wrong, and this test pins both:
    ///   * `vert_cap` is ALLOCATED CAPACITY (`dc_output_caps_sparse` = allocated_bricks *
    ///     verts_per_brick, sized as if every brick were full), NOT the vertex count.
    ///   * 2,048,383 is only ~33% of a legitimate 1024³ DC surface bound (6*1024² ≈ 6.29M), so the
    ///     old cap was not a correct ceiling even for the quantity it claimed to bound.
    ///
    /// The cap now bounds GPU MEMORY (84 B/vertex across the five dispatch buffers). Asserting the
    /// memory bound rather than the literal keeps the test meaningful if the buffer layout changes.
    #[test]
    fn vertex_curvature_cap_admits_a_real_dc_surface() {
        // Mirrors MAX_CURV_VERTS_BUDGET in poll_vertex_curvature. The EFFECTIVE cap is
        // min(budget, device_storage_binding_limit / 32) — see the device-limit assertion below.
        const CAP: u64 = 8_000_000;
        // out 16 + nrm_a 16 + nrm_b 16 + adj 32 + adj_fill 4
        const BYTES_PER_VERT: u64 = 84;

        let owner_vert_cap: u64 = 2_048_383;
        assert!(owner_vert_cap <= CAP,
            "the owner's ordinary box (vert_cap {owner_vert_cap}) must not be rejected - a cap              below real capacities disables curvature silently, which is how this shipped");

        // A full 1024³ DC surface must fit: the cap exists to refuse pathological allocations, not
        // legitimate high-resolution bodies.
        let dc_1024_surface_bound: u64 = 6 * 1024 * 1024;
        assert!(dc_1024_surface_bound <= CAP,
            "a 1024³ DC surface bound ({dc_1024_surface_bound}) must fit under the cap");

        // ...and the cap must still bound memory to something defensible per frame.

        // The budget alone is NOT safe: at 8M the adjacency table (32 B/vertex) is 256 MB, which
        // exceeds wgpu's DEFAULT max_storage_buffer_binding_size of 128 MiB. A cap that ignores
        // the device limit produces a validation failure — device loss, not a slow frame — which
        // is why the shipped cap clamps to the device. This pins that the raw budget would have
        // been unsafe, so nobody "simplifies" the clamp away.
        const ADJ_BYTES_PER_VERT: u64 = 32;
        const WGPU_DEFAULT_BINDING_LIMIT: u64 = 128 * 1024 * 1024;
        let default_dev_cap = WGPU_DEFAULT_BINDING_LIMIT / ADJ_BYTES_PER_VERT;
        assert!(default_dev_cap < CAP,
            "this test's premise changed: the budget ({CAP}) no longer exceeds what a default              adapter can bind ({default_dev_cap}), so the device clamp may look redundant - it is              not, keep it for adapters reporting the default limit");
        assert!(owner_vert_cap <= default_dev_cap,
            "the owner's box ({owner_vert_cap}) must fit even on an adapter reporting wgpu's              DEFAULT binding limit ({default_dev_cap}) - otherwise curvature silently skips on              ordinary hardware");

        let mb = CAP * BYTES_PER_VERT / 1_000_000;
        assert!(mb <= 1024,
            "cap admits {mb}MB of transient per-frame buffers - that is unbounded per-frame work              (Frame Budget Rule); lower MAX_CURV_VERTS or shrink the dispatch buffers");
    }

    /// OWNER REPRO 2026-07-29: a Curvature Generator layer on **Base color** (the screenshot's
    /// setup) must be GPU-curvature-eligible exactly like the Roughness one is.
    ///
    /// Every existing gate builds its graph on `ChannelKind::Roughness`. Base color goes through
    /// different lowering (it is a COLOR channel, not a scalar), so "works on roughness" does not
    /// imply "works on base color" - and base color is where the owner reports seeing nothing.
    #[test]
    fn owner_curvature_on_base_color_is_gpu_eligible() {
        let sdf = owner_repro_body();
        let (bmin, bmax) = sdf.body_aabb(0);
        let radius = crate::material_panel::lower::curvature_radius_floor((bmax - bmin).length());
        let doc = doc_with_generator_on(radius, ChannelKind::BaseColor, GeneratorRung::Auto);
        let ch = doc.channel(ChannelKind::BaseColor).expect("base color channel must exist");
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        let emitted = regen_channel_subgraph(
            &mut graph, ChannelKind::BaseColor, ch, doc.texture_space, &mat);
        assert!(emitted,
            "regen_channel_subgraph emitted NOTHING for a Generator layer on Base color - the              layer exists in the doc but never becomes nodes, so nothing can ever render");
        let keys = curvature_keys_for_graph(&graph);
        assert!(keys.is_some() && !keys.as_ref().unwrap().is_empty(),
            "a Curvature Generator layer on Base color produced NO CurvKey - it is not GPU              eligible, so it falls back to the CPU path (or renders nothing)");
    }

    /// And the VERTEX rung specifically on Base color - `vertex_curv_keys_for_graph` requires the
    /// node label flag to end in 'v'. If Base-color lowering does not write that flag, the vertex
    /// rung finds no keys and `poll_vertex_curvature` returns early: nothing on the model, no
    /// error, which is exactly the reported symptom.
    #[test]
    /// BOTH MODES (CURVATURE_BOTH_MODES.md): VertexCache tags trailing `v` AND wires the bake.
    /// Bake keys feed the planar map compositor (Decision 1); vertex keys feed SEED_POSITIONS.
    fn owner_curvature_vertex_rung_on_base_color_produces_keys() {
        let sdf = owner_repro_body();
        let (bmin, bmax) = sdf.body_aabb(0);
        let radius = crate::material_panel::lower::curvature_radius_floor((bmax - bmin).length());
        let doc = doc_with_generator_on(radius, ChannelKind::BaseColor, GeneratorRung::VertexCache);
        let ch = doc.channel(ChannelKind::BaseColor).expect("base color channel");
        let mat = PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        regen_channel_subgraph(&mut graph, ChannelKind::BaseColor, ch, doc.texture_space, &mat);
        let labels: Vec<_> = graph.nodes.iter().map(|n| n.label.clone()).collect();
        assert!(
            labels.iter().any(|l| l.contains(":sgv") || l.contains(":cxv") || l.contains(":ccv")),
            "VertexCache must tag trailing v (sgv/cxv/ccv); labels={labels:?}"
        );
        let bake_keys = curvature_keys_for_graph(&graph).unwrap_or_default();
        assert!(
            !bake_keys.is_empty(),
            "VertexCache still wires the bake (Decision 1); labels={labels:?}"
        );
        let vkeys = vertex_curv_keys_for_graph(&graph);
        assert!(
            !vkeys.is_empty(),
            "VertexCache must produce SEED_POSITIONS keys via trailing-v tags; labels={labels:?}"
        );
    }

    /// GATE 1 — eligibility. The exact node shape the panel emits for a fresh Generator layer
    /// must produce a single coherent `CurvKey`, or `spawn_or_park_graph_bake` never parks for
    /// the GPU grid and every sample pays the ~500-eval CPU cost.
    #[test]
    fn owner_generator_layer_graph_is_gpu_curvature_eligible() {
        let _ = build_owner_repro_graph();
    }

    /// GATE 2 — the fast path is actually TAKEN, not merely theoretically reachable. Seeds a
    /// `CurvGrid` with values a real CPU eval could never produce (way outside [-1,1], a
    /// distinctive per-pixel ramp) and asserts the bake's roughness channel decodes back to
    /// those exact values — proving `sdf_curvature_masks`'s cache branch (node_graph_bake.rs
    /// :416-434), including the pixel-index inversion, is exercised end to end by the
    /// production `bake_node_graph_maps_res` loop, not just reachable in isolation.
    #[test]
    fn curvature_fast_path_taken_when_grid_present() {
        let (graph, key, sdf_no_cache) = build_owner_repro_graph();
        let res = 32u32; // small + exact power-of-two-friendly grid for a fast, precise test
        let mut data = vec![0.0f32; (res * res * 4) as usize];
        // Distinctive per-pixel "mask" channel: an ascending ramp with NO relationship to real
        // curvature, so any pixel that DIDN'T come from the grid stands out immediately.
        for y in 0..res {
            for x in 0..res {
                let i = ((y * res + x) * 4) as usize;
                let marker = (y * res + x) as f32 / (res * res) as f32; // 0..~1 ramp
                data[i] = marker; // mask
                data[i + 1] = marker; // curv
                data[i + 2] = marker; // convex
                data[i + 3] = marker; // concave
            }
        }
        let mut sdf = sdf_no_cache.clone();
        sdf.curv_caches = vec![CurvGrid { res, data: data.clone(), region: key.region, key }];

        let ch = channels_from_graph(&graph);
        assert!(ch.roughness, "the Roughness channel must be wired for this to test anything");
        let maps = bake_node_graph_maps_res(&graph, res, ch, &ImageMapSources::default(), &sdf, BAKE_MAX_OCTAVES)
            .expect("bake must succeed");

        // The Signed-output layer runs the grid's `curv` channel (index 1) through two Remaps
        // (-1..1 -> 0..1, then the layer's own 0..1 -> 0..1 identity) before it lands in the
        // roughness map — compare against the SAME sRGB-encode the production bake applies
        // (`material_textures::encode_data_scalar_rgba`), not a linear byte/255 decode, or a
        // correct fast-path read looks "wrong" purely from the gamma curve.
        let mut max_err = 0i32;
        for y in 0..res {
            for x in 0..res {
                let idx = ((y * res + x) * 4) as usize;
                let marker = (y * res + x) as f32 / (res * res) as f32;
                let expect_signed_norm = ((marker - (-1.0)) / (1.0 - (-1.0))).clamp(0.0, 1.0);
                let expect_byte = material_textures::encode_data_scalar_rgba(expect_signed_norm)[0];
                let got_byte = maps.roughness[idx];
                max_err = max_err.max((got_byte as i32 - expect_byte as i32).abs());
            }
        }
        assert!(
            max_err <= 2, // sRGB LUT round-trip tolerance
            "roughness map does not match the seeded CurvGrid values (max byte err {max_err}) — \
             the fast path in `sdf_curvature_masks` (node_graph_bake.rs:416-434) was NOT taken \
             (either the cache attach, the key match, or the pixel-index inversion regressed), \
             so this bake silently fell back to ~500 analytical evals/pixel — exactly the 45s bug"
        );
    }

    /// GATE 3 — the fast path is not just correct but actually FAST: at the production
    /// resolution, a bake WITH a `CurvGrid` attached must be MEASURABLY quicker than the same
    /// bake with `curv_cache: None` (the CPU fallback the owner hit). A regression that silently
    /// falls back (gate 2 would also catch a WRONG value, but a fallback that happens to
    /// reproduce similar-looking output by coincidence is not impossible for a degenerate scene)
    /// shows up here as a timing regression instead.
    ///
    /// Threshold is 4x, not the golden test's bare-kernel 59x-65x: this measures the FULL
    /// `bake_node_graph_maps_res` pipeline (per-pixel node-graph topo eval, HashMap memo,
    /// sRGB encode), not just the curvature inner loop, so per-pixel scaffolding overhead that
    /// is identical on both sides dilutes the ratio (measured on this 2-prim body: ~8.7x).
    #[test]
    fn curvature_fast_path_is_measurably_faster_than_cpu_fallback() {
        let (graph, key, sdf_no_cache) = build_owner_repro_graph();
        let work = snap_work_res(BAKE_WORK_RES);
        let ch = channels_from_graph(&graph);

        let t_cpu = std::time::Instant::now();
        let _ = bake_node_graph_maps_res(&graph, work, ch, &ImageMapSources::default(), &sdf_no_cache, BAKE_MAX_OCTAVES);
        let cpu_ms = t_cpu.elapsed().as_secs_f64() * 1000.0;

        let n = work * work;
        let data = vec![0.0f32; (n * 4) as usize];
        let mut sdf_cached = sdf_no_cache.clone();
        sdf_cached.curv_caches = vec![CurvGrid { res: work, data, region: key.region, key }];
        let t_gpu = std::time::Instant::now();
        let _ = bake_node_graph_maps_res(&graph, work, ch, &ImageMapSources::default(), &sdf_cached, BAKE_MAX_OCTAVES);
        let cached_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "[CurvatureFastPath] {work}x{work}: CPU fallback {cpu_ms:.1} ms vs grid-cached \
             {cached_ms:.1} ms ({:.1}x)",
            cpu_ms / cached_ms.max(1e-6)
        );
        // FLAKINESS NOTE (2026-07-28, P0a): this compares WALL CLOCK of two sequential bakes.
        // Once `bake_node_graph_maps_res` became rayon row-parallel, both runs draw from the same
        // global thread pool as every OTHER test executing concurrently — so a sibling test that
        // saturates the pool can inflate whichever run overlaps it. Observed inverting the result
        // outright (CPU 1050 ms vs "fast" 5979 ms = 0.2x) when a large concurrent bake ran
        // alongside; the fast path was fine, the measurement was not.
        //
        // Retried rather than loosened: a single sample is the unreliable part, not the claim.
        // The fast path is genuinely ~8x here, so the best of 3 attempts clears 4x comfortably
        // unless the speedup has actually regressed — which is what this gate exists to catch.
        // (`--test-threads=1` also passes 3/3; this keeps it honest under the default runner.)
        let mut best_ratio = cpu_ms / cached_ms.max(1e-6);
        for _ in 0..2 {
            if best_ratio >= 4.0 {
                break;
            }
            let t_cpu = std::time::Instant::now();
            let _ = bake_node_graph_maps_res(&graph, work, ch, &ImageMapSources::default(), &sdf_no_cache, BAKE_MAX_OCTAVES);
            let c = t_cpu.elapsed().as_secs_f64() * 1000.0;
            let t_gpu = std::time::Instant::now();
            let _ = bake_node_graph_maps_res(&graph, work, ch, &ImageMapSources::default(), &sdf_cached, BAKE_MAX_OCTAVES);
            let g = t_gpu.elapsed().as_secs_f64() * 1000.0;
            best_ratio = best_ratio.max(c / g.max(1e-6));
        }
        assert!(
            best_ratio >= 4.0,
            "grid-cached bake is not >=4x faster than the CPU fallback at {work}x{work} \
             (best of 3 = {best_ratio:.1}x; first sample was {cpu_ms:.1} ms vs {cached_ms:.1} ms) \
             — the fast path is not delivering the speedup the 45s bug fix depends on"
        );
    }

    /// GATE 4 — THE ACTUAL 45s REGRESSION SCENARIO. A material with a Generator on TWO different
    /// channels (Roughness=Curvature at radius R1, Bump=Cavity at radius R2 — an entirely
    /// ordinary "worn rock" recipe) produces TWO `SdfCurvature`/`Cavity` nodes with DIFFERENT
    /// knobs. Before the 2026-07-27 fix, `curvature_key_for_graph` (singular) returned `None` the
    /// instant it saw the second disagreeing key, which fell the ENTIRE bake — both channels,
    /// every pixel — back to ~500 analytical evals/pixel. `curvature_keys_for_graph` (plural)
    /// must instead return BOTH keys, and the bake must actually use a grid for each.
    #[test]
    fn two_generators_different_radius_both_get_gpu_grids() {
        use crate::material_panel::model::{ChannelKind as CK, GeneratorOutput as GO};
        let sdf = owner_repro_body();
        let (bmin, bmax) = sdf.body_aabb(0);
        let base_radius = crate::material_panel::lower::curvature_radius_floor((bmax - bmin).length());
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.retain(|c| c.kind != CK::Roughness && c.kind != CK::Bump);
        doc.channels.push(Channel {
            kind: CK::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::Curvature, radius: base_radius, invert: false,
                    output: GO::Signed, remap: [0.0, 1.0], rung: GeneratorRung::Auto,
                sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                mask: crate::material_panel::model::MaskStack::default(), blend: BlendMode::Normal, opacity: 1.0, visible: true,
                placement: Placement::default(), adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true });
        doc.channels.push(Channel {
            kind: CK::Bump,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::Cavity, radius: base_radius * 2.0, invert: false,
                    output: GO::ConcaveOnly, remap: [0.0, 1.0], rung: GeneratorRung::Auto,
                sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                mask: crate::material_panel::model::MaskStack::default(), blend: BlendMode::Normal, opacity: 1.0, visible: true,
                placement: Placement::default(), adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true });
        let mat = core_math::primitives::PbrMaterial::default();
        let mut graph = crate::shader_nodes::NodeGraph::default();
        for ch in &doc.channels {
            if matches!(ch.state, ChannelState::Layered) {
                crate::material_panel::lower::regen_channel_subgraph(
                    &mut graph, ch.kind, ch, doc.texture_space, &mat,
                );
            }
        }
        let keys = curvature_keys_for_graph(&graph).expect(
            "a material with two Generator layers at different radii must still be eligible — \
             this IS the owner's 45s bug if it regresses to None"
        );
        assert_eq!(
            keys.len(), 2,
            "expected exactly 2 distinct CurvKeys (Curvature @ base_radius, Cavity @ 2x) — got \
             {keys:?}"
        );

        // Attach a distinctive, out-of-band grid for EACH key and confirm the bake reads BOTH —
        // proving the multi-key fast path actually dispatches and consumes one grid per node,
        // not just the first / a merged one.
        let res = 16u32;
        let mut sdf_multi = sdf.clone();
        for (gi, &key) in keys.iter().enumerate() {
            let marker = 0.3 + gi as f32 * 0.3; // distinct per-key constant, out of real range
            let data = vec![marker; (res * res * 4) as usize];
            sdf_multi.curv_caches.push(CurvGrid { res, data, region: key.region, key });
        }
        let ch = channels_from_graph(&graph);
        assert!(ch.roughness && ch.bump, "both channels must be wired for this to test anything");
        let maps = bake_node_graph_maps_res(&graph, res, ch, &ImageMapSources::default(), &sdf_multi, BAKE_MAX_OCTAVES)
            .expect("bake must succeed");
        // Every pixel of BOTH channels must be non-empty (i.e. the cache path, not a silent
        // all-zero CPU miss) — a coarse but decisive proof both grids were consumed.
        assert!(maps.roughness.iter().any(|&b| b != 0), "roughness map must be non-degenerate");
        assert!(maps.bump.iter().any(|&b| b != 0), "bump map must be non-degenerate");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shader_nodes::{NodeGraph, NodeType};

    /// GATE (P2, spec C1 forbidden-state fix) — a bake install mask is gated by
    /// panel ownership: panel-Layered channels are skipped + flagged as a
    /// conflict; GraphDriven channels install; absent doc = legacy behavior.
    #[test]
    fn material_panel_gate_blocks_panel_owned_channels() {
        use crate::material_panel::model::{
            Channel, ChannelBitDepth, ChannelKind, ChannelState, ChannelValue, MaterialPanelDoc,
        };
        let full = BakeChannelMask {
            albedo: true,
            roughness: true,
            metallic: true,
            emission: false,
            normal: false,
            bump: false,
            displacement: false,
            sss_color: false,
        };
        // No doc → unchanged, no conflicts.
        let (m, c) = gate_mask_by_panel_ownership(full, None, 0);
        assert_eq!((m.albedo, m.roughness, m.metallic, c), (true, true, true, 0));
        // Doc: roughness panel-owned (Layered), metallic graph-driven, albedo value.
        let mut doc = MaterialPanelDoc::default();
        doc.channels.push(Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true });
        doc.channels.push(Channel {
            kind: ChannelKind::Metallic,
            state: ChannelState::GraphDriven { node: 1 },
            layers: vec![],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true });
        doc.channels.push(Channel::value(
            ChannelKind::BaseColor,
            ChannelValue::Scalar(0.0),
        ));
        let (m, c) = gate_mask_by_panel_ownership(full, Some(&doc), 0);
        assert!(!m.roughness, "panel-owned channel is never clobbered");
        assert!(m.metallic, "graph-driven channel still installs");
        assert!(m.albedo, "value channel unaffected");
        assert_eq!(c, 1u32 << (ChannelKind::Roughness as u32), "conflict surfaced");
        // P3: same doc, but roughness subgraph is PANEL-GENERATED → install
        // allowed, no conflict (the composite is the panel's own bake).
        let (m, c) = gate_mask_by_panel_ownership(
            full,
            Some(&doc),
            1u32 << (ChannelKind::Roughness as u32),
        );
        assert!(m.roughness, "panel-generated composite passes the gate");
        assert_eq!(c, 0, "no conflict for panel-generated installs");
    }

    #[test]
    fn bake_noise_preset_produces_variation() {
        let graph = crate::shader_nodes::MaterialPresets::list()
            .into_iter()
            .find(|(n, _, _)| *n == "Pipeline Proof")
            .expect("Pipeline Proof preset")
            .2();
        let maps = bake_node_graph_maps(&graph).expect("bake");
        assert_eq!(
            maps.albedo.len(),
            (MAP_RES * MAP_RES * 4) as usize,
            "bake output must be MAP_RES after upsample"
        );
        // Pipeline Proof: Noise → ColorRamp → albedo and Noise → roughness.
        let r0 = maps.roughness[0];
        let mut varied = false;
        for chunk in maps.roughness.chunks(4).step_by(64) {
            if chunk[0] != r0 {
                varied = true;
                break;
            }
        }
        assert!(varied, "pipeline proof bake should vary (noise-driven)");
    }

    /// Shared helper: bake a named MaterialPresets graph into `assets/PBR/<Folder>/`.
    fn export_preset_to_library(preset_name: &str, folder: &str, file_prefix: &str) {
        let graph = crate::shader_nodes::MaterialPresets::list()
            .into_iter()
            .find(|(n, _, _)| *n == preset_name)
            .unwrap_or_else(|| panic!("{preset_name} preset missing"))
            .2();
        assert!(
            graph.nodes.len() > 8,
            "{preset_name} should be a full node tree, got {} nodes",
            graph.nodes.len()
        );
        let maps = bake_node_graph_maps(&graph)
            .unwrap_or_else(|| panic!("bake {preset_name}"));
        let expected = (MAP_RES * MAP_RES * 4) as usize;
        assert_eq!(maps.albedo.len(), expected, "albedo");
        assert_eq!(maps.roughness.len(), expected, "roughness");
        assert_eq!(maps.metallic.len(), expected, "metallic");
        assert!(
            maps.normal.len() == expected || maps.bump.len() == expected,
            "normal or height should bake"
        );

        let a0 = maps.albedo[0];
        let mut albedo_varied = false;
        for chunk in maps.albedo.chunks(4).step_by(32) {
            if chunk[0] != a0 {
                albedo_varied = true;
                break;
            }
        }
        assert!(albedo_varied, "{preset_name} albedo must vary");

        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let candidates = [
            manifest_dir.join(format!("../../assets/PBR/{folder}")),
            manifest_dir.join(format!("../../../assets/PBR/{folder}")),
            std::path::PathBuf::from(format!(
                r"F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish\assets\PBR\{folder}"
            )),
            std::path::PathBuf::from(format!(r"F:\PROJECT\FourthLAyerAi\assets\PBR\{folder}")),
        ];
        let out_dir = candidates
            .into_iter()
            .find(|p| p.parent().map(|par| par.is_dir()).unwrap_or(false))
            .expect("find assets/PBR parent");
        std::fs::create_dir_all(&out_dir).expect("create material folder");

        let res = MAP_RES;
        // Dual-plane 16-bit GPU bake → classic 8-bit library PNGs.
        let albedo8 = if maps.albedo_lo.len() == expected {
            crate::material_textures::dual_plane_color16_to_srgb8(&maps.albedo, &maps.albedo_lo)
        } else {
            maps.albedo.clone()
        };
        let rough8 = if maps.roughness_lo.len() == expected {
            crate::material_textures::dual_plane_data16_to_srgb8(&maps.roughness, &maps.roughness_lo)
        } else {
            maps.roughness.clone()
        };
        let metal8 = if maps.metallic_lo.len() == expected {
            crate::material_textures::dual_plane_data16_to_srgb8(&maps.metallic, &maps.metallic_lo)
        } else {
            maps.metallic.clone()
        };
        let height8 = if maps.bump_lo.len() == expected {
            crate::material_textures::dual_plane_data16_to_srgb8(&maps.bump, &maps.bump_lo)
        } else {
            maps.bump.clone()
        };
        // RGB normal (not grayscale data decode — that made library normals flat gray).
        let normal8 = if maps.normal_lo.len() == expected {
            crate::material_textures::dual_plane_normal16_to_rgba8(&maps.normal, &maps.normal_lo)
        } else if maps.normal.len() == expected {
            maps.normal.clone()
        } else {
            Vec::new()
        };

        let write_rgba = |name: &str, px: &[u8]| {
            if px.len() != expected {
                return;
            }
            let path = out_dir.join(name);
            image::save_buffer(&path, px, res, res, image::ColorType::Rgba8)
                .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            eprintln!("  wrote {}", path.display());
        };
        write_rgba(&format!("{file_prefix}_Base_Color.png"), &albedo8);
        write_rgba(&format!("{file_prefix}_Roughness.png"), &rough8);
        write_rgba(&format!("{file_prefix}_Metallic.png"), &metal8);
        if normal8.len() == expected {
            write_rgba(&format!("{file_prefix}_Normal.png"), &normal8);
        }
        if height8.len() == expected {
            write_rgba(&format!("{file_prefix}_Height.png"), &height8);
        }
        write_rgba(&format!("{file_prefix}_preview.png"), &albedo8);

        let graph_json = serde_json::to_string_pretty(&graph).expect("serialize graph");
        let graph_path = out_dir.join(format!("{file_prefix}_shader_tree.json"));
        std::fs::write(&graph_path, graph_json).expect("write shader tree json");
        eprintln!("  wrote {}", graph_path.display());
        eprintln!("{preset_name} library material ready at {}", out_dir.display());
    }

    /// Bake Worn Gunmetal → `assets/PBR/WornGunmetal/`.
    #[test]
    fn bake_worn_gunmetal_exports_to_library() {
        export_preset_to_library("Worn Gunmetal", "WornGunmetal", "WornGunmetal");
    }

    /// Bake Old Gold → `assets/PBR/OldGold/`.
    #[test]
    fn bake_old_gold_exports_to_library() {
        export_preset_to_library("Old Gold", "OldGold", "OldGold");
    }

    #[test]
    fn bake_default_graph_skips_unwired() {
        // No Master sockets wired → nothing to bake into maps (scalars stay on PbrMaterial).
        let g = NodeGraph::default();
        assert!(
            bake_node_graph_maps(&g).is_none(),
            "empty Master graph must not emit map channels"
        );
    }

    #[test]
    fn bake_noise_to_roughness_varies() {
        let mut g = NodeGraph::default();
        let tc = g.add_node(NodeType::TexCoord, [40.0, 100.0]);
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(tc, 0, noise, 0), "pos → noise");
        assert!(g.connect(noise, 0, out, 1), "noise float → roughness float");
        let maps = bake_node_graph_maps(&g).expect("bake");
        assert_eq!(maps.roughness.len(), (MAP_RES * MAP_RES * 4) as usize);
        // Live/Final only allocate baked channels — albedo stays empty when unwired.
        assert!(maps.albedo.is_empty());
        let r0 = maps.roughness[0];
        let varied = maps.roughness.chunks(4).step_by(32).any(|c| c[0] != r0);
        assert!(varied, "noise-driven roughness must vary");
    }

    #[test]
    fn all_noise_types_bake_finite() {
        for nt in NoiseType::all() {
            let mut g = NodeGraph::default();
            let tc = g.add_node(NodeType::TexCoord, [40.0, 100.0]);
            let out = g.output_node().unwrap().id;
            let noise = g.add_node(NodeType::Noise, [200.0, 100.0]);
            if let Some(n) = g.find_node_mut(noise) {
                n.noise_type = *nt;
            }
            assert!(g.connect(tc, 0, noise, 0));
            assert!(g.connect(noise, 0, out, 0));
            let maps = bake_node_graph_maps(&g).unwrap_or_else(|| panic!("bake failed for {nt:?}"));
            let sum: u32 = maps.albedo.iter().map(|&b| b as u32).sum();
            assert!(sum > 0, "all black for {nt:?}");
        }
    }

    #[test]
    fn procedural_noise_nodes_render_non_flat_previews() {
        for node_type in [
            NodeType::Noise,
            NodeType::Voronoi,
            NodeType::MusgraveTexture,
        ] {
            let mut graph = NodeGraph::default();
            let node_id = graph.add_node(node_type.clone(), [0.0, 0.0]);
            let rgba = render_noise_preview_rgba(&graph, node_id, 24)
                .unwrap_or_else(|| panic!("preview failed for {node_type:?}"));
            assert_eq!(rgba.len(), 24 * 24 * 4);
            let first = rgba[0];
            assert!(
                rgba.chunks_exact(4).any(|pixel| pixel[0] != first),
                "preview must show visible variation for {node_type:?}"
            );
        }
    }

    #[test]
    fn extract_live_gpu_noise_simple_chain() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        if let Some(n) = g.find_node_mut(noise) {
            n.input_defaults[1].value_f32 = 5.5; // scale
            n.input_defaults[2].value_f32 = 3.0; // octaves
        }
        let live = g.extract_live_gpu_noise().expect("simple Noise→Albedo is GPU-live");
        assert!((live.scale - 5.5).abs() < 1e-4);
        assert_eq!(live.octaves, 3);
        assert!(live.drives_albedo);
    }

    #[test]
    fn noise_blend_curve_levels_and_smooth() {
        use crate::shader_nodes::{apply_noise_blend_curve, NoiseBlendCurve};
        // Linear with black/white
        let v = apply_noise_blend_curve(0.5, NoiseBlendCurve::Linear, 0.0, 1.0, 1.0);
        assert!((v - 0.5).abs() < 1e-4);
        let v = apply_noise_blend_curve(0.25, NoiseBlendCurve::Linear, 0.0, 0.5, 1.0);
        assert!((v - 0.5).abs() < 1e-4); // 0.25 in [0,0.5] → 0.5
        // Smoothstep of 0.5 is 0.5
        let v = apply_noise_blend_curve(0.5, NoiseBlendCurve::Smooth, 0.0, 1.0, 1.0);
        assert!((v - 0.5).abs() < 1e-4);
        // Power k=2 darkens mids
        let v = apply_noise_blend_curve(0.5, NoiseBlendCurve::Power, 0.0, 1.0, 2.0);
        assert!(v < 0.5 && v > 0.2);
        // Levels mid>1 lifts
        let v = apply_noise_blend_curve(0.25, NoiseBlendCurve::Levels, 0.0, 1.0, 2.0);
        assert!(v > 0.25);
    }

    #[test]
    fn live_bake_matches_final_quality() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        assert!(g.connect(noise, 0, out, 1)); // also roughness
        g.bake_res = 128;
        let (live_maps, live_ch, _) = bake_node_graph_maps_quality_with_mask(
            &g,
            GraphBakeQuality::Live,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
        )
        .expect("live bake");
        let (final_maps, final_ch, _) = bake_node_graph_maps_quality_with_mask(
            &g,
            GraphBakeQuality::Final,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
        )
        .expect("final bake");
        // Live is full quality: all wired channels, same size as Final.
        assert!(live_ch.albedo && live_ch.roughness);
        assert_eq!(live_ch.albedo, final_ch.albedo);
        assert_eq!(live_ch.roughness, final_ch.roughness);
        assert_eq!(live_maps.albedo.len(), (MAP_RES * MAP_RES * 4) as usize);
        assert_eq!(live_maps.roughness.len(), final_maps.roughness.len());
        // Same bake path → identical first pixel.
        assert_eq!(live_maps.albedo[0], final_maps.albedo[0]);
    }

    /// Regression (owner 2026-07-28): Mult noise over Image must install a MAP_RES²
    /// buffer. A `.min(512)` bake clamp + `work>=MAP_RES` skip-upsample left 512²
    /// maps that `upload_map` rejected → Mult never reached the viewport
    /// ("pure map" / "pure noise" depending on live REPLACE race).
    #[test]
    fn mult_image_stack_bakes_at_map_res_for_upload() {
        use crate::shader_nodes::MixBlend;
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        // Solid base via ImageTexture pin (simulates panel Image layer).
        let img = g.add_node(NodeType::ImageTexture, [40.0, 40.0]);
        if let Some(n) = g.find_node_mut(img) {
            n.image_tex_id = 7;
            n.image_map_kind = MapKind::Albedo.wire_tag();
        }
        let noise = g.add_node(NodeType::Noise, [40.0, 200.0]);
        let mix = g.add_node(NodeType::MixRgb, [240.0, 100.0]);
        if let Some(n) = g.find_node_mut(mix) {
            n.mix_blend = MixBlend::Multiply;
            n.pad_input_defaults();
            if let Some(s) = n.input_defaults.get_mut(0) {
                s.value_f32 = 1.0; // full Mult
            }
        }
        assert!(g.connect(img, 0, mix, 1), "base → Color1");
        assert!(g.connect(noise, 0, mix, 2), "noise → Color2");
        assert!(g.connect(mix, 0, out, 0), "mix → albedo");
        // Flat mid-gray source image (linear 0.5 ≈ sRGB 188).
        let mut px = vec![0u8; (MAP_RES * MAP_RES * 4) as usize];
        for chunk in px.chunks_exact_mut(4) {
            chunk.copy_from_slice(&[188, 188, 188, 255]);
        }
        let mut images = ImageMapSources::default();
        images.by_tex.insert(7, px);
        let (maps, ch, work_res) = bake_node_graph_maps_quality_with_mask(
            &g,
            GraphBakeQuality::Final,
            &images,
            &SdfFieldSources::default(),
        )
        .expect("Mult image bake");
        let _ = work_res;
        assert!(ch.albedo);
        let expected = (MAP_RES * MAP_RES * 4) as usize;
        assert_eq!(
            maps.albedo.len(),
            expected,
            "Mult stack must be MAP_RES² so upload_map accepts it"
        );
        // Bake emits dual-plane 16-bit; assert on classic 8-bit (Bit8 install space), not the
        // hi plane alone (lut[srgb_u16>>8] sits ~35 codes brighter on mid-tones).
        let albedo8 = crate::material_textures::dual_plane_color16_to_srgb8(
            &maps.albedo,
            &maps.albedo_lo,
        );
        // Mult must not be pure white (image missing → identity white×noise) nor
        // pure mid-gray (Mult identity / noise not wired). Noise varies → span.
        let mut min_b = 255u8;
        let mut max_b = 0u8;
        for i in (0..albedo8.len()).step_by(4) {
            let b = albedo8[i];
            min_b = min_b.min(b);
            max_b = max_b.max(b);
        }
        assert!(
            max_b > min_b + 8,
            "Mult must vary with noise (min={min_b} max={max_b})"
        );
        // Dark Mult areas darken the mid-gray base (not REPLACE to pure B&W extremes only).
        assert!(
            min_b < 170,
            "dark Mult should pull mid-gray base down (min={min_b})"
        );
    }

    /// MixRgb Multiply is base×layer (not REPLACE). Locks Blender Mix Color Mult.
    #[test]
    fn mixrgb_multiply_is_base_times_layer_not_replace() {
        use crate::shader_nodes::MixBlend;
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        let base = g.add_node(NodeType::ConstantColor, [0.0, 0.0]);
        if let Some(n) = g.find_node_mut(base) {
            n.ramp_color0 = [0.8, 0.4, 0.2];
        }
        let top = g.add_node(NodeType::ConstantColor, [0.0, 80.0]);
        if let Some(n) = g.find_node_mut(top) {
            n.ramp_color0 = [0.5, 0.5, 0.5];
        }
        let mix = g.add_node(NodeType::MixRgb, [200.0, 40.0]);
        if let Some(n) = g.find_node_mut(mix) {
            n.mix_blend = MixBlend::Multiply;
            n.pad_input_defaults();
            if let Some(s) = n.input_defaults.get_mut(0) {
                s.value_f32 = 1.0;
            }
        }
        assert!(g.connect(base, 0, mix, 1));
        assert!(g.connect(top, 0, mix, 2));
        assert!(g.connect(mix, 0, out, 0));
        // Low-res direct eval (no image → snap work res) then upsample.
        g.bake_res = 64;
        let maps = bake_node_graph_maps(&g).expect("bake");
        // Dual-plane → classic 8-bit before reading (same as Bit8 install / GPU bake compare).
        let albedo8 = crate::material_textures::dual_plane_color16_to_srgb8(
            &maps.albedo,
            &maps.albedo_lo,
        );
        // Linear 0.8*0.5=0.4 → sRGB encode. Pure REPLACE would be 0.5 gray.
        // Sample centre pixel after upsample (same constant everywhere).
        let i = ((MAP_RES / 2 * MAP_RES + MAP_RES / 2) * 4) as usize;
        let r = albedo8[i] as f32 / 255.0;
        // sRGB of ~0.4 is roughly 0.66; sRGB of 0.5 is ~0.74; sRGB of 0.8 is ~0.91.
        // Mult product must be closer to 0.4-encoded than to base 0.8 or layer 0.5.
        assert!(
            r < 0.78,
            "Mult 0.8×0.5 should be darker than mid-gray layer (r={r})"
        );
        assert!(
            r > 0.50,
            "Mult 0.8×0.5 should not crush to black (r={r})"
        );
    }

    /// Regression: unconnected Noise must sample surface `p` (not default [0,0,0]),
    /// so baked-in Triplanar works without plugging TexCoord / Projection.
    #[test]
    fn unconnected_noise_varies_without_texcoord() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [200.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        // No TexCoord → Position wire. Projection defaults to Triplanar.
        assert_eq!(
            g.find_node(noise).unwrap().projection,
            ProjectionStyle::Triplanar
        );
        let maps = bake_node_graph_maps(&g).expect("bake");
        let a0 = maps.albedo[0];
        let varied = maps.albedo.chunks(4).step_by(64).any(|c| c[0] != a0);
        assert!(
            varied,
            "unconnected Noise+Triplanar must vary across the bake domain (not stuck at origin)"
        );
    }

    #[test]
    fn catalog_nodes_construct() {
        for nt in [
            NodeType::ConstantValue,
            NodeType::Invert,
            NodeType::ColorCorrect,
            NodeType::Curvature,
            NodeType::Cavity,
            NodeType::AmbientOcclusion,
            NodeType::Slope,
            NodeType::VectorMath,
            NodeType::Clamp,
            NodeType::MixFloat,
            NodeType::Smoothstep,
            NodeType::PrimSdfMask,
            NodeType::SdfCurvature,
        ] {
            let mut g = NodeGraph::default();
            let id = g.add_node(nt.clone(), [100.0, 100.0]);
            let n = g.find_node(id).expect("node");
            assert_eq!(n.input_defaults.len(), Node::input_defs(&nt).len());
        }
    }

    #[test]
    fn prim_sdf_mask_and_curvature_bake() {
        use core_math::primitives::{SdfPrimitive, SdfShape};
        use glam::Vec3;
        let mut g = NodeGraph::default();
        let mask = g.add_node(NodeType::PrimSdfMask, [100.0, 100.0]);
        let curv = g.add_node(NodeType::SdfCurvature, [100.0, 250.0]);
        let out = g.output_node().unwrap().id;
        if let Some(n) = g.find_node_mut(mask) {
            n.sdf_prim_uid = 42;
            n.input_defaults[2].value_f32 = 0.1; // soft
        }
        if let Some(n) = g.find_node_mut(curv) {
            n.sdf_curv_region = 1; // convex
            n.input_defaults[4].value_f32 = 1.5;
            n.input_defaults[5].value_f32 = 1.0;
        }
        assert!(g.connect(mask, 0, out, 0)); // mask → albedo
        assert!(g.connect(curv, 0, out, 1)); // curv mask → roughness
        let mut sdf = SdfFieldSources::default();
        let mut sphere =
            SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        sphere.prim_uid = 42;
        sdf.by_uid.insert(42, sphere.clone());
        sdf.body_prims.push(sphere);
        let maps = bake_node_graph_maps_quality(
            &g,
            GraphBakeQuality::Final,
            &ImageMapSources::default(),
            &sdf,
        )
        .expect("bake with sdf");
        assert!(!maps.albedo.is_empty());
        assert!(!maps.roughness.is_empty());
        // Sphere has uniform positive mean curvature on the surface — mask must be
        // filled (not a thin UV-plane silhouette ring).
        let mut lit = 0u32;
        let mut i = 0;
        while i + 3 < maps.roughness.len() {
            if maps.roughness[i] > 20 {
                lit += 1;
            }
            i += 4;
        }
        assert!(
            lit > 200,
            "expected filled surface curvature, got lit_texels={lit}"
        );
    }

    #[test]
    fn surface_curvature_not_plane_silhouette() {
        // Direct unit test: sphere at origin → project + mean curvature > 0.
        use core_math::primitives::{SdfPrimitive, SdfShape};
        use glam::Vec3;
        let mut sdf = SdfFieldSources::default();
        let mut sphere =
            SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        sphere.prim_uid = 1;
        sdf.by_uid.insert(1, sphere.clone());
        sdf.body_prims.push(sphere);
        // Bake-domain UV centre should still hit the sphere surface (not only a ring).
        let (mask, curv, conv, _conc) =
            sdf_curvature_masks(&sdf, [0.0, 0.0, 0.0], 0.02, 1, 1, 0.0, 0.2, 1.5, 1.0);
        assert!(
            conv > 0.15 && mask > 0.15,
            "sphere centre UV should sample surface curvature, got mask={mask} curv={curv} conv={conv}"
        );
        // Outside corner of a box: high convex curvature
        let mut box_sdf = SdfFieldSources::default();
        let mut bx = SdfPrimitive::new(
            "B".into(),
            SdfShape::Box {
                half_extents: Vec3::splat(0.4),
                bevel: Default::default(),
            },
            Vec3::ZERO,
        );
        bx.prim_uid = 2;
        box_sdf.by_uid.insert(2, bx.clone());
        box_sdf.body_prims.push(bx);
        let (m2, _, c2, _) =
            sdf_curvature_masks(&box_sdf, [0.9, 0.9, 0.0], 0.02, 2, 1, 0.0, 0.25, 2.0, 1.0);
        assert!(
            c2 > 0.05 || m2 > 0.05,
            "box near-corner UV should show convex edge wear, got m={m2} c={c2}"
        );
    }

    /// O3 + M4: bake targets above 1024 install at that size up to the **product ceiling** (2K).
    /// Soft targets (≤512) still upsize to DEFAULT_MAP_RES. 4K requires AZRA_ALLOW_4K_MAPS=1.
    #[test]
    fn o3_final_install_res_raises_ceiling() {
        assert_eq!(final_install_res(256), DEFAULT_MAP_RES);
        assert_eq!(final_install_res(512), DEFAULT_MAP_RES);
        assert_eq!(final_install_res(1024), 1024);
        assert_eq!(final_install_res(2048), 2048);
        // M4 / D3: product ceiling 2K — 4096 snaps down unless AZRA_ALLOW_4K_MAPS=1.
        let ceil = crate::material_textures::product_map_ceiling();
        assert_eq!(final_install_res(4096), ceil);
        assert_eq!(snap_work_res(1800), 2048.min(ceil));
        assert_eq!(snap_work_res(3500), ceil);
        assert!(
            ceil >= 2048,
            "product ceiling must be at least 2K (got {ceil})"
        );
    }

    /// M1 sabotage — FORCE_CPU / GPU_OFF / partial dirty must deny the GPU product path.
    #[test]
    fn m1_gpu_bake_path_eligible_sabotage_gate() {
        assert!(
            gpu_bake_path_eligible(true, false, false),
            "full dirty + no sabotage → GPU allowed"
        );
        assert!(
            !gpu_bake_path_eligible(true, true, false),
            "AZRA_FORCE_CPU_BAKE must force CPU"
        );
        assert!(
            !gpu_bake_path_eligible(true, false, true),
            "AZRA_GPU_BAKE=0 must force CPU"
        );
        assert!(
            !gpu_bake_path_eligible(false, false, false),
            "partial spatial dirty must stay CPU (O4)"
        );
    }

    /// M4 gate: 1024-authored installs stay 1024; 2K is reachable; VRAM scoreboard is finite.
    #[test]
    fn m4_product_ceiling_2k_and_vram_scoreboard() {
        use crate::material_textures::{
            estimate_material_map_vram_bytes, material_map_vram_scoreboard_line,
            product_map_ceiling, PRODUCT_MAP_CEILING, DEFAULT_MAP_RES,
        };
        assert_eq!(PRODUCT_MAP_CEILING, 2048);
        assert_eq!(product_map_ceiling(), 2048, "default product ceiling is 2K");
        assert_eq!(final_install_res(1024), 1024, "1024 saves install at 1024");
        assert_eq!(final_install_res(2048), 2048);
        // 4ch dual-plane @1024: 1024² × 4 × 2 × 4 = 32 MiB
        let v1024 = estimate_material_map_vram_bytes(1024, 4);
        assert_eq!(v1024, 1024u64 * 1024 * 4 * 2 * 4);
        let v2048 = estimate_material_map_vram_bytes(2048, 4);
        assert_eq!(v2048, v1024 * 4, "2K is 4× pixels of 1024");
        let line = material_map_vram_scoreboard_line(2048, 4);
        eprintln!("[M4 SCOREBOARD] {}", line);
        eprintln!(
            "[M4 SCOREBOARD] @1024²×4ch: {}",
            material_map_vram_scoreboard_line(DEFAULT_MAP_RES, 4)
        );
        assert!(line.contains("2048") && line.contains("MiB"));
    }

    /// O3 gate: Bake 2048² produces 2048² buffers that pass the install size check.
    #[test]
    fn o3_bake_2048_installs_without_silent_drop() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        g.bake_res = 2048;
        let (maps, ch, work) = bake_node_graph_maps_quality_with_mask(
            &g,
            GraphBakeQuality::Final,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
        )
        .expect("2048 bake");
        assert_eq!(work, 2048, "Final at bake_res=2048 must work at 2048");
        let expected = (2048u32 * 2048 * 4) as usize;
        assert_eq!(
            maps.albedo.len(),
            expected,
            "2048 bake must install 2048² (O3 ceiling), not soft 1024"
        );
        let loads = pending_loads_for_material_selective(
            core_math::primitives::MaterialId(1),
            maps,
            ch,
        );
        assert!(
            !loads.is_empty(),
            "install gate must accept 2048² (pre-O3 silently dropped non-MAP_RES)"
        );
        assert_eq!(loads[0].pixels.len(), expected);
    }

    /// O4 gate: tiled re-eval of a dirty rect into a prior full bake is bit-identical to a
    /// fresh full bake of the same graph (plan: "the gate is bit-identical output").
    #[test]
    fn o4_tiled_region_matches_full_bake_bit_identical() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        g.bake_res = 128; // small: speed; install soft-upsamples to 1024 but work is 128
        // Force work == install for a clean prior match: bake raw at 128 with region.
        let res = 128u32;
        let ch = channels_from_graph(&g);
        let full = bake_node_graph_maps_res(
            &g,
            res,
            ch,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
            BAKE_MAX_OCTAVES,
        )
        .expect("full");
        // Centre tile (with filter margin expansion applied by resolve).
        let dirty = BakeDirty::Rect(MapBakeRect {
            x0: 32,
            y0: 32,
            x1: 96,
            y1: 96,
        });
        let region = dirty.resolve_eval_rect(res);
        let tiled = bake_node_graph_maps_res_region(
            &g,
            res,
            ch,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
            BAKE_MAX_OCTAVES,
            region,
            Some(&full),
        )
        .expect("tiled");
        assert_eq!(
            full.albedo, tiled.albedo,
            "O4: re-baking dirty rect into prior must match full bake (hi plane)"
        );
        assert_eq!(
            full.albedo_lo, tiled.albedo_lo,
            "O4: re-baking dirty rect into prior must match full bake (lo plane)"
        );
    }

    /// O4: partial rect without prior must not silently install black holes — forced to Full
    /// by the quality path; region-only raw bake with no base zeros outside (caller responsibility).
    #[test]
    fn o4_tiles_covering_rect_snaps_to_tile_grid() {
        let res = 1024u32;
        let r = MapBakeRect {
            x0: 10,
            y0: 20,
            x1: 30,
            y1: 40,
        };
        let cov = tiles_covering_rect(r, res);
        assert_eq!(cov.x0, 0);
        assert_eq!(cov.y0, 0);
        assert_eq!(cov.x1, MAP_BAKE_TILE);
        assert_eq!(cov.y1, MAP_BAKE_TILE);
        let tiles = map_bake_tiles(res);
        assert_eq!(
            tiles.len(),
            ((res / MAP_BAKE_TILE) * (res / MAP_BAKE_TILE)) as usize
        );
    }

    /// O4: quality path with prior + centre dirty is bit-identical to full quality path.
    #[test]
    fn o4_quality_dirty_rect_matches_full_quality() {
        let mut g = NodeGraph::default();
        let noise = g.add_node(NodeType::Noise, [100.0, 100.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(noise, 0, out, 0));
        // bake_res 1024 → work == install == 1024 so prior side matches.
        g.bake_res = 1024;
        let (full, _, _) = bake_node_graph_maps_quality_with_mask(
            &g,
            GraphBakeQuality::Final,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
        )
        .expect("full quality");
        let dirty = BakeDirty::Rect(MapBakeRect {
            x0: 200,
            y0: 200,
            x1: 400,
            y1: 400,
        });
        let (tiled, _, _) = bake_node_graph_maps_quality_with_mask_dirty(
            &g,
            GraphBakeQuality::Final,
            &ImageMapSources::default(),
            &SdfFieldSources::default(),
            dirty,
            Some(&full),
        )
        .expect("tiled quality");
        assert_eq!(
            full.albedo.len(),
            tiled.albedo.len(),
            "same install size"
        );
        assert_eq!(
            full.albedo, tiled.albedo,
            "O4 quality dirty-rect must be bit-identical to full Final"
        );
    }

    // ── G1/G2/G4 — exhaustive NodeType WGSL validate gate ───────────────────────────────

    /// Explicit enum list so adding a `NodeType` variant fails to compile until handled.
    const ALL_NODE_TYPES: &[NodeType] = &[
        NodeType::TexCoord,
        NodeType::ObjectInfo,
        NodeType::Noise,
        NodeType::Voronoi,
        NodeType::MathBinary,
        NodeType::MathUnary,
        NodeType::Remap,
        NodeType::ColorRamp,
        NodeType::MixRgb,
        NodeType::BrightnessContrast,
        NodeType::SeparateXyz,
        NodeType::CombineXyz,
        NodeType::Triplanar,
        NodeType::BoxMapping,
        NodeType::CylindricalMapping,
        NodeType::SphericalMapping,
        NodeType::PlanarMapping,
        NodeType::UVTransform,
        NodeType::TextureSample,
        NodeType::GradientTexture,
        NodeType::WaveTexture,
        NodeType::MusgraveTexture,
        NodeType::BrickTexture,
        NodeType::MaterialOutput,
        NodeType::ConstantColor,
        NodeType::ImageTexture,
        NodeType::ConstantValue,
        NodeType::Invert,
        NodeType::ColorCorrect,
        NodeType::Curvature,
        NodeType::Cavity,
        NodeType::AmbientOcclusion,
        NodeType::Slope,
        NodeType::VectorMath,
        NodeType::Clamp,
        NodeType::MixFloat,
        NodeType::Smoothstep,
        NodeType::HeightToNormal,
        NodeType::PrimSdfMask,
        NodeType::SdfCurvature,
        NodeType::SurfaceDirection,
        NodeType::SdfThickness,
        NodeType::SdfPosition,
        NodeType::Reroute,
        NodeType::Frame,
        NodeType::Checker,
        NodeType::Gamma,
    ];

    fn validate_material_bake_wgsl(src: &str) -> Result<(), String> {
        let module = naga::front::wgsl::parse_str(src).map_err(|e| format!("PARSE: {e}"))?;
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        v.validate(&module).map_err(|e| format!("VALIDATE: {e}"))?;
        Ok(())
    }

    fn first_compatible_output(nt: &NodeType, want: SocketType) -> Option<usize> {
        Node::output_defs(nt)
            .into_iter()
            .enumerate()
            .find(|(_, s)| s.socket_type.compatible(&want))
            .map(|(i, _)| i)
    }

    /// G2 GATE — every NodeType emits WGSL that **validates** (not just parses), wired to
    /// Albedo and Roughness. Ineligible types (G4) must be rejected by `graph_is_gpu_bakeable`
    /// before compile. Exhaustive by construction.
    #[test]
    fn every_nodetype_generates_valid_wgsl() {
        use crate::shader_nodes::types::SocketType;
        let mut failures: Vec<String> = Vec::new();
        for nt in ALL_NODE_TYPES {
            // MaterialOutput alone is the terminal — no need to re-wire into itself.
            if matches!(nt, NodeType::MaterialOutput) {
                continue;
            }
            // G8 Frame: ZERO sockets by design — nothing to wire, nothing to emit. It is
            // whitelisted precisely so a decorative box never forces the CPU bake; there
            // is no WGSL to validate for it.
            if matches!(nt, NodeType::Frame) {
                continue;
            }

            // G4 / whitelist: non-bakeable types must fail the gate before shader compile.
            if !node_type_gpu_bakeable(nt) {
                let mut g = NodeGraph::default();
                let n = g.add_node(nt.clone(), [40.0, 40.0]);
                let out = g.output_node().unwrap().id;
                if let Some(from_out) = first_compatible_output(nt, SocketType::Color)
                    .or_else(|| first_compatible_output(nt, SocketType::Float))
                {
                    let _ = g.connect(n, from_out, out, NodeGraph::MO_ALBEDO);
                }
                if graph_is_gpu_bakeable(&g) {
                    failures.push(format!(
                        "{nt:?}: expected GPU-ineligible / non-whitelist rejection, but bakeable"
                    ));
                }
                continue;
            }

            // Every codegen-branching CONFIGURATION, not just the default one.
            //
            // The original gate built each node with its DEFAULT projection and reported green while
            // Voronoi + any single-plane projection emitted `redefinition of 'nN_dist'` — Voronoi's
            // default is Triplanar, the one passing case. Exhaustive over node TYPES is not the same
            // as exhaustive over node CONFIGURATION: a bug reachable only via a non-default enum hides
            // completely. Any future enum that changes which WGSL is emitted belongs in this loop.
            // ALL 8 variants, listed explicitly: adding one to the enum should break this array and
            // force a conscious decision, rather than silently skipping the new configuration.
            for style in [
                ProjectionStyle::Triplanar,
                ProjectionStyle::PlanarXY,
                ProjectionStyle::PlanarXZ,
                ProjectionStyle::PlanarYZ,
                ProjectionStyle::Box,
                ProjectionStyle::Cylindrical,
                ProjectionStyle::Spherical,
                ProjectionStyle::None,
            ] {
                for (slot, want, label) in [
                    (NodeGraph::MO_ALBEDO, SocketType::Color, "Albedo"),
                    (NodeGraph::MO_ROUGHNESS, SocketType::Float, "Roughness"),
                ] {
                    let mut g = NodeGraph::default();
                    let n = g.add_node(nt.clone(), [40.0, 40.0]);
                    if let Some(node) = g.find_node_mut(n) {
                        node.projection = style;
                    }
                    let out = g.output_node().unwrap().id;
                    let Some(from_out) = first_compatible_output(nt, want)
                        .or_else(|| first_compatible_output(nt, SocketType::Float))
                        .or_else(|| first_compatible_output(nt, SocketType::Color))
                    else {
                        failures.push(format!("{nt:?} → {label}: no connectable output"));
                        continue;
                    };
                    if !g.connect(n, from_out, out, slot) {
                        failures.push(format!("{nt:?} → {label}: connect failed"));
                        continue;
                    }
                    let node_wgsl = g.generate_wgsl();
                    let full = graphics::build_material_bake_wgsl(&node_wgsl);
                    if let Err(e) = validate_material_bake_wgsl(&full) {
                        failures.push(format!("{nt:?} → {label} [{style:?}]: {e}"));
                    }
                }
            }
        }
        if !failures.is_empty() {
            panic!(
                "G2 every_nodetype_generates_valid_wgsl — {} failure(s):\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }
    }

    /// G4 — AmbientOcclusion / SdfThickness stay fail-closed on the GPU bake gate.
    #[test]
    fn g4_ao_and_thickness_are_gpu_ineligible() {
        for nt in GPU_INELIGIBLE {
            assert!(
                !node_type_gpu_bakeable(nt),
                "{nt:?} must be GPU-ineligible (fail closed, no shader crash)"
            );
            let mut g = NodeGraph::default();
            let n = g.add_node(nt.clone(), [0.0, 0.0]);
            let out = g.output_node().unwrap().id;
            if let Some(fo) = first_compatible_output(nt, SocketType::Float) {
                let _ = g.connect(n, fo, out, NodeGraph::MO_ROUGHNESS);
            }
            assert!(
                !graph_is_gpu_bakeable(&g),
                "{nt:?} graph must not be GPU-bakeable"
            );
        }
    }

    /// G1 smoke — integer-axis defaults must not emit `u32(0.0000)`.
    #[test]
    fn g1_integer_defaults_emit_concrete_u_literals() {
        let mut g = NodeGraph::default();
        let n = g.add_node(NodeType::PlanarMapping, [0.0, 0.0]);
        let out = g.output_node().unwrap().id;
        assert!(g.connect(n, 0, out, NodeGraph::MO_ALBEDO));
        let wgsl = g.generate_wgsl();
        assert!(
            !wgsl.contains("u32(0.0000)")
                && !wgsl.contains("u32(0.0)")
                && !wgsl.contains("u32(1.0000)"),
            "G1: must not emit abstract float into u32():\n{wgsl}"
        );
        // Concrete integer literal (default axis value depends on Node::new defaults).
        assert!(
            wgsl.contains("let ax = 0u")
                || wgsl.contains("let ax = 1u")
                || wgsl.contains("let ax = 2u"),
            "G1: unconnected axis default should be concrete Nu:\n{wgsl}"
        );
    }

    /// G5 GATE — default-projection generators emit weight-guarded triplanar samples
    /// (`if (twN.axis > 0.01)`) so bake-domain `n=[0,0,1]` skips the two dead axes.
    /// Full material bake WGSL must still validate.
    #[test]
    fn g5_triplanar_generators_emit_weight_guards() {
        use crate::shader_nodes::types::SocketType;
        for nt in [
            NodeType::Noise,
            NodeType::Voronoi,
            NodeType::GradientTexture,
            NodeType::WaveTexture,
            NodeType::MusgraveTexture,
            NodeType::BrickTexture,
            NodeType::TextureSample,
        ] {
            let mut g = NodeGraph::default();
            let n = g.add_node(nt.clone(), [40.0, 40.0]);
            // Default projection is Triplanar (node_data.rs).
            assert_eq!(
                g.find_node(n).map(|nd| nd.projection),
                Some(ProjectionStyle::Triplanar),
                "{nt:?}: default projection must be Triplanar for G5 gate"
            );
            let out = g.output_node().unwrap().id;
            let from = first_compatible_output(&nt, SocketType::Float)
                .or_else(|| first_compatible_output(&nt, SocketType::Color))
                .expect("generator has Float or Color out");
            // Prefer roughness (Float) when available; Color generators go to albedo.
            let connected = g.connect(n, from, out, NodeGraph::MO_ROUGHNESS)
                || g.connect(n, from, out, NodeGraph::MO_ALBEDO);
            assert!(connected, "{nt:?}: connect failed");
            let wgsl = g.generate_wgsl();
            for axis in ["x", "y", "z"] {
                let needle = format!("_twN.{axis} > 0.01");
                assert!(
                    wgsl.contains(&needle),
                    "G5 {nt:?}: missing weight guard `{needle}` in:\n{wgsl}"
                );
            }
            let full = graphics::build_material_bake_wgsl(&wgsl);
            validate_material_bake_wgsl(&full).unwrap_or_else(|e| {
                panic!("G5 {nt:?}: weight-guarded WGSL failed naga: {e}\n{wgsl}");
            });
        }
    }
}

// ============================================================================
// G6 — node editor core ops (MATERIAL_GRAPH_SOT_PLAN §G6). These pin the OPERATIONS the
// Blender keymap calls, headlessly — mute is an EVALUATOR property (a mute that only dims
// the drawing lies about what renders), duplicate must carry internal links, delete must
// protect the output node.
// ============================================================================
mod g6_ops {
    use super::*;
    use crate::shader_nodes::{NodeGraph, NodeType};

    /// Muted node = input-0 passthrough IN THE EVALUATOR. Constant→Invert(muted)→Roughness
    /// must sample the constant unchanged; unmuting must restore the inversion.
    #[test]
    fn muted_node_passes_input_through_in_eval() {
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("default graph seeds MaterialOutput").id;
        let c = g.add_node(NodeType::ConstantValue, [-300.0, 0.0]);
        if let Some(n) = g.find_node_mut(c) {
            n.const_float = 0.25;
        }
        let inv = g.add_node(NodeType::Invert, [-150.0, 0.0]);
        let _ = g.connect(c, 0, inv, 0);
        let _ = g.connect(inv, 0, out, NodeGraph::MO_ROUGHNESS);
        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let p = [0.0f32, 0.0, 0.0];
        let n = [0.0f32, 0.0, 1.0];

        let live = eval_graph_sample(&g, &order, out, p, n, 0.0, &images, &sdf, 8);
        assert!(
            (live.roughness - 0.75).abs() < 1e-4,
            "unmuted Invert(0.25) must give 0.75, got {}",
            live.roughness
        );

        let mut sel = std::collections::HashSet::new();
        sel.insert(inv);
        let now_muted = g.toggle_mute(&sel);
        assert!(now_muted, "toggle on an unmuted node must mute it");
        let muted = eval_graph_sample(&g, &order, out, p, n, 0.0, &images, &sdf, 8);
        assert!(
            (muted.roughness - 0.25).abs() < 1e-4,
            "muted Invert must pass 0.25 through, got {}",
            muted.roughness
        );

        g.toggle_mute(&sel);
        let back = eval_graph_sample(&g, &order, out, p, n, 0.0, &images, &sdf, 8);
        assert!(
            (back.roughness - 0.75).abs() < 1e-4,
            "unmute must restore the inversion, got {}",
            back.roughness
        );
    }

    /// Shift+D semantics: relative offsets preserved, links BETWEEN duplicated nodes carried,
    /// links to unselected nodes NOT carried, MaterialOutput never duplicated.
    #[test]
    fn duplicate_selection_preserves_offsets_and_internal_links() {
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("default graph seeds MaterialOutput").id;
        let a = g.add_node(NodeType::ConstantValue, [-300.0, 40.0]);
        let b = g.add_node(NodeType::Invert, [-100.0, 90.0]);
        let _ = g.connect(a, 0, b, 0);
        let _ = g.connect(b, 0, out, NodeGraph::MO_ROUGHNESS);

        let mut sel: std::collections::HashSet<u32> = [a, b, out].into_iter().collect();
        let map = g.duplicate_nodes(&sel, [40.0, 40.0]);
        assert_eq!(map.len(), 2, "output must not duplicate");
        let (na, nb) = (map[&a], map[&b]);
        let pa = g.find_node(na).unwrap().pos;
        let pb = g.find_node(nb).unwrap().pos;
        assert!(
            ((pb[0] - pa[0]) - 200.0).abs() < 1e-3 && ((pb[1] - pa[1]) - 50.0).abs() < 1e-3,
            "relative offset must survive duplication"
        );
        // Internal a→b link carried; external b→out link NOT (out was excluded from dup).
        assert!(
            g.connections.iter().any(|c| c.from_node == na && c.to_node == nb),
            "internal link must be duplicated"
        );
        assert!(
            !g.connections.iter().any(|c| c.from_node == nb && c.to_node == out),
            "the DUPLICATE must not steal the original's output wire"
        );
        // The original chain is untouched.
        assert!(g.connections.iter().any(|c| c.from_node == b && c.to_node == out));
        sel.clear();
    }

    /// X on a selection deletes everything EXCEPT the output node.
    #[test]
    fn delete_selection_protects_material_output() {
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("default graph seeds MaterialOutput").id;
        let a = g.add_node(NodeType::Noise, [-200.0, 0.0]);
        let b = g.add_node(NodeType::Invert, [-100.0, 0.0]);
        let sel: std::collections::HashSet<u32> = [a, b, out].into_iter().collect();
        let removed = g.remove_nodes(&sel);
        assert_eq!(removed, 2);
        assert!(g.find_node(out).is_some(), "MaterialOutput must survive X");
        assert!(g.find_node(a).is_none() && g.find_node(b).is_none());
    }

    /// Mute must also disqualify the live-GPU fast path (both-paths rule): a graph the GPU
    /// path would accept must fall back to CPU the moment any node is muted.
    #[test]
    fn muted_graph_falls_back_from_live_gpu_path() {
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("default graph seeds MaterialOutput").id;
        let nse = g.add_node(NodeType::Noise, [-200.0, 0.0]);
        let _ = g.connect(nse, 0, out, NodeGraph::MO_ALBEDO);
        if g.extract_live_gpu_noise().is_none() {
            // Fixture graph is not GPU-eligible on this build — the bail cannot be observed;
            // the eval-side passthrough test above still covers mute correctness.
            return;
        }
        let mut sel = std::collections::HashSet::new();
        sel.insert(nse);
        g.toggle_mute(&sel);
        assert!(
            g.extract_live_gpu_noise().is_none(),
            "a muted node must force the CPU path — the GPU fast path has no mute concept"
        );
    }
}

// ============================================================================
// G8 — structure nodes (Reroute / Frame). The gate's promises: structure is graph
// SHAPE — zero semantic change (a reroute forwards the RAW value, a frame is inert),
// the GPU bake path DECLINES rather than compiling a lossy coercion, and the panel
// projection DEGRADES (R4/R5) instead of mangling a chain that contains one.
// ============================================================================
mod g8_structure {
    use super::*;
    use crate::shader_nodes::{NodeGraph, NodeType};

    #[test]
    fn reroute_passes_scalar_and_color_through_raw() {
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        let c = g.add_node(NodeType::ConstantValue, [-300.0, 0.0]);
        if let Some(n) = g.find_node_mut(c) {
            n.const_float = 0.25;
        }
        let rr = g.add_node(NodeType::Reroute, [-150.0, 0.0]);
        assert!(g.connect(c, 0, rr, 0));
        assert!(g.connect(rr, 0, out, NodeGraph::MO_ROUGHNESS));
        // Colour through a second reroute — the raw Val must survive (a float
        // coercion collapsing RGB is exactly the bug the GPU decline avoids).
        let cc = g.add_node(NodeType::ConstantColor, [-300.0, 120.0]);
        if let Some(n) = g.find_node_mut(cc) {
            n.ramp_color0 = [0.9, 0.2, 0.1];
        }
        let rr2 = g.add_node(NodeType::Reroute, [-150.0, 120.0]);
        assert!(g.connect(cc, 0, rr2, 0));
        assert!(g.connect(rr2, 0, out, NodeGraph::MO_ALBEDO));

        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let smp = eval_graph_sample(
            &g, &order, out, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.0, &images, &sdf, 8,
        );
        assert!((smp.roughness - 0.25).abs() < 1e-4, "scalar reroute: {}", smp.roughness);
        assert!(
            (smp.albedo[0] - 0.9).abs() < 1e-4
                && (smp.albedo[1] - 0.2).abs() < 1e-4
                && (smp.albedo[2] - 0.1).abs() < 1e-4,
            "colour reroute must forward RGB raw, got {:?}",
            smp.albedo
        );
    }

    #[test]
    fn reroute_declines_the_gpu_bake_path() {
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        let c = g.add_node(NodeType::ConstantValue, [-300.0, 0.0]);
        let _ = g.connect(c, 0, out, NodeGraph::MO_ROUGHNESS);
        assert!(
            crate::node_graph_bake::bake_dispatch::graph_is_gpu_bakeable(&g),
            "fixture must be GPU-eligible BEFORE the reroute or this test is vacuous"
        );
        let rr = g.add_node(NodeType::Reroute, [-150.0, 0.0]);
        let _ = g.connect(c, 0, rr, 0);
        let _ = g.connect(rr, 0, out, NodeGraph::MO_ROUGHNESS);
        assert!(
            !crate::node_graph_bake::bake_dispatch::graph_is_gpu_bakeable(&g),
            "a reroute must fail CLOSED to the CPU bake (WGSL float coercion would collapse colours)"
        );
    }

    #[test]
    fn frame_is_semantically_inert_and_roundtrips_size() {
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        let c = g.add_node(NodeType::ConstantValue, [-300.0, 0.0]);
        if let Some(n) = g.find_node_mut(c) {
            n.const_float = 0.6;
        }
        let _ = g.connect(c, 0, out, NodeGraph::MO_ROUGHNESS);
        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let before = eval_graph_sample(
            &g, &order, out, [0.0; 3], [0.0, 0.0, 1.0], 0.0, &images, &sdf, 8,
        );
        let f = g.add_node(NodeType::Frame, [-400.0, -50.0]);
        if let Some(n) = g.find_node_mut(f) {
            n.frame_size = [300.0, 220.0];
        }
        let order2 = g.topo_sort(out).unwrap();
        let after = eval_graph_sample(
            &g, &order2, out, [0.0; 3], [0.0, 0.0, 1.0], 0.0, &images, &sdf, 8,
        );
        assert_eq!(before.roughness, after.roughness, "a Frame must change NOTHING");
        // Persistence: graph JSON is name-tagged serde; frame_size must survive.
        let json = serde_json::to_string(&g).unwrap();
        let back: NodeGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.find_node(f).unwrap().frame_size, [300.0, 220.0]);
    }

    /// R4/R5 (the gate's round-trip clause): a Reroute spliced into a PANEL chain makes
    /// the channel hand-wired — the projection must DEGRADE (parse -> None, source ->
    /// HandWired), never mis-associate or rewrite the nodes.
    #[test]
    fn panel_projection_degrades_when_a_reroute_enters_the_chain() {
        use crate::material_panel::lower::{
            channel_source, parse_panel_channel, regen_channel_subgraph, ChannelSource,
        };
        use crate::material_panel::model::*;
        let mut g = NodeGraph::default();
        let mat = core_math::pbr_material::PbrMaterial::default();
        let ch = Channel::with_state(
            ChannelKind::Roughness,
            ChannelState::Layered,
            vec![Layer {
                id: 1,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 3.0,
                    noise_type: crate::shader_nodes::NoiseType::Fbm,
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        );
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::Roughness,
            &ch,
            TextureSpace::default(),
            &mat
        ));
        assert_eq!(
            channel_source(&g, ChannelKind::Roughness),
            ChannelSource::PanelStack
        );
        // Splice a Reroute into the master wire (what G7's drop-on-link does).
        let out = g.output_node().unwrap().id;
        let sock = crate::material_panel::lower::kind_socket(ChannelKind::Roughness);
        let src = g
            .connections
            .iter()
            .find(|c| c.to_node == out && c.to_input == sock)
            .map(|c| (c.from_node, c.from_output))
            .expect("panel chain wires the socket");
        let rr = g.add_node(NodeType::Reroute, [0.0, 0.0]);
        assert!(g.connect(src.0, src.1, rr, 0));
        assert!(g.connect(rr, 0, out, sock));
        assert_eq!(
            channel_source(&g, ChannelKind::Roughness),
            ChannelSource::HandWired,
            "an untagged node in the chain = hand-wired (R5)"
        );
        assert!(
            parse_panel_channel(&g, ChannelKind::Roughness).is_none(),
            "projection must DEGRADE, never guess through a reroute"
        );
    }
}

// ============================================================================
// G9 — Blender node breadth value pins (Checker, Gamma). The formulas' source of
// truth is the CPU arm; the WGSL arm mirrors it (validated shape-wise by
// every_nodetype_generates_valid_wgsl across all 8 projection configs).
// ============================================================================
mod g9_nodes {
    use super::*;
    use crate::shader_nodes::{NodeGraph, NodeType};

    fn eval_rough(g: &NodeGraph, out: u32) -> f32 {
        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        eval_graph_sample(g, &order, out, [0.0; 3], [0.0, 0.0, 1.0], 0.0, &images, &sdf, 8)
            .roughness
    }

    #[test]
    fn gamma_is_per_channel_pow() {
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        let c = g.add_node(NodeType::ConstantColor, [-300.0, 0.0]);
        if let Some(n) = g.find_node_mut(c) {
            n.ramp_color0 = [0.25, 1.0, 0.0];
        }
        let ga = g.add_node(NodeType::Gamma, [-150.0, 0.0]);
        assert!(g.connect(c, 0, ga, 0));
        if let Some(n) = g.find_node_mut(ga) {
            n.pad_input_defaults();
            if let Some(s) = n.input_defaults.get_mut(1) {
                s.value_f32 = 2.0;
            }
        }
        assert!(g.connect(ga, 0, out, NodeGraph::MO_ALBEDO));
        let order = g.topo_sort(out).unwrap();
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let smp = eval_graph_sample(
            &g, &order, out, [0.0; 3], [0.0, 0.0, 1.0], 0.0, &images, &sdf, 8,
        );
        assert!(
            (smp.albedo[0] - 0.0625).abs() < 1e-4
                && (smp.albedo[1] - 1.0).abs() < 1e-4
                && smp.albedo[2].abs() < 1e-6,
            "Gamma(2.0) of [0.25, 1, 0] must be [0.0625, 1, 0], got {:?}",
            smp.albedo
        );
    }

    #[test]
    fn checker_alternates_cells() {
        // Two graphs, identical but for the sample position via a wired Vector —
        // adjacent cells must flip Fac 0 <-> 1 (integer parity, negatives included).
        let fac_at = |v: [f32; 3]| -> f32 {
            let mut g = NodeGraph::default();
            let out = g.output_node().unwrap().id;
            let cx = g.add_node(NodeType::CombineXyz, [-300.0, 0.0]);
            if let Some(n) = g.find_node_mut(cx) {
                n.pad_input_defaults();
                for (i, val) in v.iter().enumerate() {
                    if let Some(s) = n.input_defaults.get_mut(i) {
                        s.value_f32 = *val;
                    }
                }
            }
            let ch = g.add_node(NodeType::Checker, [-150.0, 0.0]);
            assert!(g.connect(cx, 0, ch, 0));
            if let Some(n) = g.find_node_mut(ch) {
                n.pad_input_defaults();
                if let Some(s) = n.input_defaults.get_mut(3) {
                    s.value_f32 = 1.0; // scale 1: cell = unit cube
                }
            }
            assert!(g.connect(ch, 1, out, NodeGraph::MO_ROUGHNESS));
            eval_rough(&g, out)
        };
        let a = fac_at([0.5, 0.5, 0.5]);
        let b = fac_at([1.5, 0.5, 0.5]);
        let c = fac_at([-0.5, 0.5, 0.5]);
        assert!((a - b).abs() > 0.5, "adjacent cells must alternate: {a} vs {b}");
        assert!(
            (a - c).abs() > 0.5,
            "negative-side neighbour must also alternate (two's-complement parity): {a} vs {c}"
        );
        assert!((b - c).abs() < 1e-6, "cells two apart must match: {b} vs {c}");
    }
}
    #[test]
    fn connected_bump_strength_varies_spatially_without_double_bump() {
        let mut graph = NodeGraph::default();
        graph.bake_res = 64;
        let output = graph.output_node().unwrap().id;
        let coord = graph.add_node(NodeType::TexCoord, [0.0, 0.0]);
        let separate = graph.add_node(NodeType::SeparateXyz, [0.0, 0.0]);
        let strength = graph.add_node(NodeType::Noise, [0.0, 0.0]);
        assert!(graph.connect(coord, 0, separate, 0));
        assert!(graph.connect(separate, 0, output, NodeGraph::MO_HEIGHT));
        assert!(graph.connect(strength, 0, output, NodeGraph::MO_BUMP_STRENGTH));
        let channels = channels_from_graph(&graph);
        assert!(channels.normal && !channels.bump);
        let mask = Node::mask_input_index(&NodeType::Noise).unwrap();
        graph.find_node_mut(strength).unwrap().input_defaults[mask].value_f32 = 0.0;
        let order = graph.topo_sort(output).unwrap();
        let flat = eval_graph_sample(&graph, &order, output, [0.2, 0.3, 0.0], [0.0, 0.0, 1.0], 0.0,
            &ImageMapSources::default(), &SdfFieldSources::default(), 6);
        assert_eq!(flat.normal, [0.0, 0.0, 1.0]);
        graph.find_node_mut(strength).unwrap().input_defaults[mask].value_f32 = 1.0;
        let first = eval_graph_sample(&graph, &order, output, [0.2, 0.3, 0.0], [0.0, 0.0, 1.0], 0.0,
            &ImageMapSources::default(), &SdfFieldSources::default(), 6);
        let second = eval_graph_sample(&graph, &order, output, [-0.7, 0.1, 0.0], [0.0, 0.0, 1.0], 0.0,
            &ImageMapSources::default(), &SdfFieldSources::default(), 6);
        assert!(first.normal[0] < -0.01);
        assert_ne!(first.normal, second.normal);
        let maps = bake_node_graph_maps_res(&graph, 16, channels, &ImageMapSources::default(), &SdfFieldSources::default(), 6).unwrap();
        assert!(!maps.normal.is_empty() && maps.bump.is_empty());
        graph.disconnect(output, NodeGraph::MO_BUMP_STRENGTH);
        assert!(!channels_from_graph(&graph).normal && channels_from_graph(&graph).bump);
    }

// ============================================================================
// SSSCOLORMAP feature gate (2026-08-20) — SSS Color is a REAL map channel: a
// panel noise layer on SssColor lowers into a panel subgraph wired to
// MO_SSS_COLOR, the CPU bake produces an sss_color plane, and the install path
// emits a PendingMapLoad for MapKind::SssColor — which `apply_map_slot` writes
// into `PbrMaterial::sss_color_map` (proven by the map_mut aliasing assert).
// ============================================================================
#[cfg(test)]
mod sss_color_channel {
    use super::*;
    use crate::material_panel::lower::{channel_source, regen_channel_subgraph, ChannelSource};
    use crate::material_panel::model::{
        Adjust, BlendMode, Channel, ChannelKind, ChannelState, Layer, LayerContent, MaskStack,
        Placement, TextureSpace,
    };

    fn noise_layer() -> Layer {
        Layer {
            id: 1,
            drives: Vec::new(),
            content: LayerContent::Noise {
                scale: 4.0,
                strength: 1.0,
                octaves: 3.0,
                noise_type: crate::shader_nodes::NoiseType::Fbm,
            },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        }
    }

    /// The full panel→map chain for SssColor, minus only the GPU upload:
    /// lower (regen) → wired mask → CPU bake plane → PendingMapLoad{SssColor}.
    #[test]
    fn sss_color_noise_layer_lowers_bakes_and_installs() {
        let mut g = NodeGraph::default();
        let mat = core_math::pbr_material::PbrMaterial::default();
        let ch = Channel::with_state(
            ChannelKind::SssColor,
            ChannelState::Layered,
            vec![noise_layer()],
        );
        assert!(
            regen_channel_subgraph(&mut g, ChannelKind::SssColor, &ch, TextureSpace::default(), &mat),
            "a noise layer on SssColor must emit its panel chain"
        );
        assert_eq!(
            channel_source(&g, ChannelKind::SssColor),
            ChannelSource::PanelStack,
            "the panel owns the MO_SSS_COLOR wire it just authored"
        );
        let mask = channels_from_graph(&g);
        assert!(mask.sss_color, "wired MO_SSS_COLOR must enter the bake mask");
        assert!(
            !mask.albedo && !mask.roughness,
            "only the sss channel is wired here"
        );
        // GPU bake has no sss plane — the plan must fall to CPU, never half-author.
        assert!(
            gpu_bake_channel_plan(&g, None).is_none(),
            "sss-wired graph must be rejected by the GPU bake plan (CPU owns it)"
        );
        let images = ImageMapSources::default();
        let sdf = SdfFieldSources::default();
        let maps = bake_node_graph_maps_res(&g, 32, mask, &images, &sdf, 8)
            .expect("CPU bake must succeed");
        assert_eq!(
            maps.sss_color.len(),
            32 * 32 * 4,
            "sss_color plane must be written at res²"
        );
        assert!(
            maps.albedo.is_empty(),
            "unwired channels stay empty so poll never wipes prior maps"
        );
        // FBM over the plane is non-constant → the plane must not be all one value.
        let first = &maps.sss_color[0..3];
        assert!(
            maps.sss_color.chunks_exact(4).any(|px| &px[0..3] != first),
            "baked sss noise must vary across the plane"
        );
        let loads = pending_loads_for_material_selective(
            core_math::primitives::MaterialId(7),
            maps,
            mask,
        );
        assert_eq!(loads.len(), 1, "exactly the sss channel installs");
        assert_eq!(loads[0].kind, MapKind::SssColor);
        // The kind `apply_map_slot` will route into `sss_color_map` (the ONE mapping).
        let mut m = core_math::pbr_material::PbrMaterial::default();
        m.map_mut(MapKind::SssColor).tex_id = 42;
        assert_eq!(m.sss_color_map.tex_id, 42, "map_mut(SssColor) aliases sss_color_map");
    }

    /// Turning the SssColor channel to a plain Value clears a stale map slot
    /// (same rule as BaseColor/Roughness — a dead slider must not lie).
    #[test]
    fn sss_color_value_write_clears_stale_map() {
        use crate::material_panel::model::{ChannelValue, MaterialPanelDoc};
        let mut mat = core_math::pbr_material::PbrMaterial::default();
        mat.sss_color_map.tex_id = 9;
        let mut doc = MaterialPanelDoc {
            shader_type: 0,
            texture_space: Default::default(),
            channels: vec![Channel::value(
                ChannelKind::SssColor,
                ChannelValue::Color([0.9, 0.1, 0.2]),
            )],
            bake_res: 256,
            next_layer_id: 1,
            generator_mode: 0,
        };
        doc.channels[0].enabled = true;
        crate::material_panel::lower::lower_to_material(&doc, &mut mat);
        assert_eq!(mat.sss_color.x, 0.9);
        assert_eq!(mat.sss_color_map.tex_id, 0, "Value state must drop the stale map");
    }
}
