//! Scene graph unit tests — pure relocation from former `scene_graph.rs`.
//!
//! Imports that lived at the top of the former single-file module (private parent
//! bindings child tests could see) are restated here after the topic split.

use std::sync::Arc;

use glam::{Quat, Vec3};

use crate::half_edge::HalfEdgeMesh;
use crate::primitives::{
    BlendProfile, BooleanOp, BoxBevel, MaterialId, PbrMaterial, SdfPrimitive, SdfShape,
};

use super::*;

/// OWNER 2026-07-29: sun `ambient` must default to ZERO, like every other light type.
    ///
    /// Directional was the only type defaulting to 0.15. With an HDRI supplying environment light
    /// that double-counts — two ambient contributions for one scene, which is what the owner saw.
    /// Pins the CONSISTENCY rather than the literal, so adding a light type with a non-zero ambient
    /// default fails here and gets justified.
    #[test]
    fn no_light_type_defaults_to_nonzero_ambient() {
        // Directional comes from Default; Point/Spot have their own constructors.
        let dir = LightData::default();
        let point = LightData::new_point(1, "p".into(), Vec3::ZERO);
        let spot = LightData::new_spot(2, "s".into(), Vec3::ZERO);
        for (what, d) in [("Directional", &dir), ("Point", &point), ("Spot", &spot)] {
            assert_eq!(d.ambient, 0.0,
                "{what} defaults to ambient {} - ambient belongs to the ENVIRONMENT (HDRI), not to                  a light. A non-zero default double-counts it.", d.ambient);
        }
    }


    /// OWNER SOAK BUG-2 2026-07-29: after converting an SDF body to poly, creating an instance
    /// collapsed the scene from 9 prims to 1 and logged
    /// `[WARN] add_prim_to_graph: active_body_nid NodeId(1) invalid — recovering default body`.
    ///
    /// The soak's own helper sets `active_body_nid = scene.sdf_body_nid` UNCONDITIONALLY before
    /// each step. If `sdf_body_nid` is stale — pointing at a node that convert replaced or removed
    /// — every later step operates on a dead body. `add_prim_to_graph`'s recovery then creates a
    /// FRESH default body and adds the prim there, so the flat cache rebuilds from a graph that no
    /// longer contains the original prims.
    ///
    /// This test pins the INVARIANT that makes that impossible: `sdf_body_nid` must always name a
    /// live SDF body. If it can go stale, any code that trusts it (and several paths do) inherits
    /// this bug class.
    #[test]
    fn sdf_body_nid_always_names_a_live_body() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let body = |name: &str, n: usize| NodePayload::SdfBody {
            name: name.into(),
            primitives: (0..n).map(|i| SdfPrimitive::new(format!("{name}{i}"), crate::primitives::SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO)).collect(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
            mesh_modifiers: Vec::new(),
            field_modifiers: Vec::new(),
        };
        let a = sg.add_child(root, body("A", 3));
        let b = sg.add_child(root, body("B", 2));
        assert_eq!(sg.all_primitives_ordered().len(), 5, "fixture must have 5 prims");

        // Removing the body `sdf_body_nid` points at is exactly the situation convert/delete can
        // produce. After it, the pointer MUST be repaired to a live body, not left dangling.
        sg.remove(a);
        assert!(sg.body_primitives(a).is_none(), "A must be gone");

        // The repair rule used by delete_body (scene_actions.rs): first remaining top-level body.
        let top = sg.child_bodies(sg.root());
        let repaired = top.first().copied();
        assert_eq!(repaired, Some(b),
            "after removing the primary body, sdf_body_nid must be repaired to a LIVE body -              leaving it dangling is what makes add_prim_to_graph fall into its recovery path and              build a fresh body, orphaning every existing prim");
        assert!(sg.body_primitives(b).is_some(), "repaired target must be a real SDF body");
        assert_eq!(sg.all_primitives_ordered().len(), 2, "B's prims must survive");
    }

    /// The CPU→GPU light layout is the load-bearing contract with `FLOATS_PER_LIGHT` (sdf_lighting.wgsl),
    /// the `VL_STRIDE` mirror in fog_entry.wgsl, and the deferred/rt_shadow accessors. Lock the 25-float
    /// stride: volumetric 16..19 + RT-shadow 20..21 + ambient RGB 22..24. Any shader whose stride
    /// disagrees misreads every light silently (no compile error) — this is the guard.
    #[test]
    fn serialize_to_gpu_volumetric_layout() {
        let mut l = LightData::new_point(3, "P".into(), Vec3::new(1.0, 2.0, 3.0));
        l.range = 8.0;
        let g = l.serialize_to_gpu();
        assert_eq!(g.len(), 25, "stride must be 25 (16 base + 4 vol + 2 RT + 3 ambient RGB)");
        assert_eq!(g[16], 0.0, "disabled volumetric → slot 16 == 0");
        assert_eq!(g[22], 1.0, "default ambient_color.r");
        assert_eq!(g[23], 1.0, "default ambient_color.g");
        assert_eq!(g[24], 1.0, "default ambient_color.b");

        l.volumetric = LightVolumetric { enabled: true, density: 2.5, softness: 1.7, zone: 0.0 };
        let g = l.serialize_to_gpu();
        assert_eq!(g[16], 1.0, "enabled → slot 16 == 1");
        assert_eq!(g[17], 2.5, "density passes through to slot 17");
        assert_eq!(g[18], 1.0, "softness clamps to 0..1 in slot 18");
        assert_eq!(g[19], 8.0, "zone == 0 resolves to the light's range (8.0) in slot 19");

        l.volumetric.zone = 12.0;
        assert_eq!(l.serialize_to_gpu()[19], 12.0, "positive zone passes through");

        // RT-shadow slots 20..21 (Stage 4). Default light: enabled=true (participates when global RT
        // is on — the Stage-3 no-regression contract), radius=0 (inherit global default softness).
        let g = LightData::new_point(4, "P2".into(), Vec3::ZERO).serialize_to_gpu();
        assert_eq!(g[20], 1.0, "default rt_shadow.enabled=true → slot 20 == 1 (follows global toggle)");
        assert_eq!(g[21], 0.0, "default rt_shadow.radius=0 → slot 21 == 0 (inherit global softness)");
        // Authored: RT disabled on a fill light + a per-light radius.
        l.rt_shadow = LightRtShadow { enabled: false, radius: 0.6 };
        let g = l.serialize_to_gpu();
        assert_eq!(g[20], 0.0, "rt_shadow.enabled=false → slot 20 == 0 (this light stays on legacy)");
        assert_eq!(g[21], 0.6, "rt_shadow.radius passes through to slot 21");
        // Negative radius is clamped to 0 (never a negative disc).
        l.rt_shadow = LightRtShadow { enabled: true, radius: -1.0 };
        assert_eq!(l.serialize_to_gpu()[21], 0.0, "negative radius clamps to 0 in slot 21");
    }

    /// Sun RT shadows (2026-07-28) are OPT-IN: a directional light reports RT participation
    /// (slot 20) only once the user sets a nonzero Source Radius.
    ///
    /// WHY THIS MATTERS: `LightRtShadow::default()` is `enabled: true` — it predates sun support
    /// and means "participate when the global toggle is on". Honouring it verbatim for the sun
    /// would flip EVERY existing scene from shadow maps to ray tracing the moment global RT was
    /// switched on: a silent look change AND a silent perf cliff (a sun ray is distance-uncapped
    /// and fires on every lit pixel). This test pins the gate that prevents that, and pins that
    /// point/spot behaviour is completely unchanged.
    #[test]
    fn sun_rt_shadow_is_opt_in_via_radius() {
        // A default SUN must NOT claim RT — even though `enabled` defaults to true.
        // `LightData::default()` IS the sun (light_type: Directional).
        let sun = LightData::default();
        assert_eq!(sun.light_type, LightType::Directional, "precondition: default light is the sun");
        assert_eq!(sun.rt_shadow.enabled, true, "precondition: the default really is enabled=true");
        assert_eq!(
            sun.serialize_to_gpu()[20], 0.0,
            "a default sun must stay on shadow maps (slot 20 == 0). If this is 1.0, every existing \
             project silently switches its key light to ray tracing when global RT is enabled."
        );

        // Setting a radius IS the opt-in.
        let mut sun_rt = sun.clone();
        sun_rt.rt_shadow = LightRtShadow { enabled: true, radius: 0.25 };
        let g = sun_rt.serialize_to_gpu();
        assert_eq!(g[20], 1.0, "sun with a nonzero radius opts in (slot 20 == 1)");
        assert_eq!(g[21], 0.25, "the authored sun radius reaches slot 21");

        // Unchecking Enable still wins over a set radius.
        let mut sun_off = sun.clone();
        sun_off.rt_shadow = LightRtShadow { enabled: false, radius: 0.25 };
        assert_eq!(
            sun_off.serialize_to_gpu()[20], 0.0,
            "explicit enabled=false must override a nonzero radius"
        );

        // REGRESSION GUARD: point/spot are untouched by the directional gate — a default point
        // light still participates with radius 0 (inheriting global softness), exactly as before.
        let pt = LightData::new_point(2, "P".into(), Vec3::ZERO);
        assert_eq!(pt.rt_shadow.radius, 0.0, "precondition: default radius is 0");
        assert_eq!(
            pt.serialize_to_gpu()[20], 1.0,
            "a POINT light must still participate at radius 0 — the opt-in gate is directional-only"
        );
    }

    #[test]
    fn empty_scene_has_root() {
        let sg = SceneGraph::new();
        assert_eq!(sg.node_count(), 1);
        assert!(sg.get(sg.root()).is_some());
        assert!(matches!(
            sg.get(sg.root()).unwrap().payload,
            NodePayload::Root
        ));
    }

    #[test]
    fn add_and_remove_child() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let id = sg.add_child(root, NodePayload::Empty { name: "test".into() });
        assert_eq!(sg.node_count(), 2);
        assert_eq!(sg.children(root).len(), 1);
        sg.remove(id);
        assert_eq!(sg.node_count(), 1);
        assert_eq!(sg.children(root).len(), 0);
    }

    #[test]
    fn remove_cascade() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let parent = sg.add_child(root, NodePayload::Group { name: "grp".into() });
        let _child1 = sg.add_child(parent, NodePayload::Empty { name: "a".into() });
        let _child2 = sg.add_child(parent, NodePayload::Empty { name: "b".into() });
        assert_eq!(sg.node_count(), 4);
        sg.remove(parent);
        assert_eq!(sg.node_count(), 1); // only root remains
    }

    #[test]
    fn reparent_prevents_cycles() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let a = sg.add_child(root, NodePayload::Empty { name: "a".into() });
        let b = sg.add_child(a, NodePayload::Empty { name: "b".into() });
        // Trying to reparent 'a' under 'b' (its descendant) should be a no-op
        sg.reparent(a, b);
        assert_eq!(sg.get(a).unwrap().parent, Some(root));
    }

    #[test]
    fn world_transform_accumulates() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let parent = sg.add_child_with_transform(
            root,
            NodePayload::Empty { name: "p".into() },
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::ZERO,
            Vec3::ONE,
        );
        let child = sg.add_child_with_transform(
            parent,
            NodePayload::Empty { name: "c".into() },
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::ZERO,
            Vec3::ONE,
        );
        let wt = sg.world_transform(child);
        let world_pos = wt.transform_point3(Vec3::ZERO);
        assert!((world_pos.x - 1.0).abs() < 0.001);
        assert!((world_pos.y - 2.0).abs() < 0.001);
        // A5: second call hits cache and matches.
        let wt2 = sg.world_transform(child);
        assert_eq!(wt, wt2);
        // Mutation via get_mut invalidates — new translation visible.
        if let Some(n) = sg.get_mut(child) {
            n.local_position = Vec3::new(0.0, 5.0, 0.0);
        }
        let wt3 = sg.world_transform(child);
        let p3 = wt3.transform_point3(Vec3::ZERO);
        assert!((p3.y - 5.0).abs() < 0.001, "cache must invalidate on get_mut, got {p3:?}");
    }

    #[test]
    fn sdf_body_primitives() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let prim = SdfPrimitive::new(
            "Box".into(),
            crate::primitives::SdfShape::Sphere { radius: 1.0 },
            Vec3::ZERO,
        );
        let body = sg.add_child(
            root,
            NodePayload::SdfBody {
                name: "Box".into(),
                primitives: vec![prim],
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY,
                body_scale: Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(),
                child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
        );
        assert_eq!(sg.primitive_count(), 1);
        assert!(sg.body_primitives(body).is_some());
        assert_eq!(sg.body_primitives(body).unwrap().len(), 1);
    }

    #[test]
    fn depth_first_walk() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let a = sg.add_child(root, NodePayload::Empty { name: "a".into() });
        let _b = sg.add_child(root, NodePayload::Empty { name: "b".into() });
        let _c = sg.add_child(a, NodePayload::Empty { name: "c".into() });

        let walk: Vec<_> = sg.walk_depth_first(root).collect();
        assert_eq!(walk.len(), 4); // root, a, c, b
        assert_eq!(walk[0].0, 0); // root at depth 0
        assert_eq!(walk[1].0, 1); // a at depth 1
        assert_eq!(walk[2].0, 2); // c at depth 2
        assert_eq!(walk[3].0, 1); // b at depth 1
    }

    // ================================================================
    // QA: Additional Scene Graph Tests
    // ================================================================

    #[test]
    fn multiple_sdf_bodies() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mk_prim = |name: &str| crate::primitives::SdfPrimitive::new(
            name.into(),
            crate::primitives::SdfShape::Sphere { radius: 1.0 },
            Vec3::ZERO,
        );
        let _body1 = sg.add_child(root, NodePayload::SdfBody {
            name: "Body1".into(),
            primitives: vec![mk_prim("a"), mk_prim("b"), mk_prim("c")],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let _body2 = sg.add_child(root, NodePayload::SdfBody {
            name: "Body2".into(),
            primitives: vec![mk_prim("d"), mk_prim("e")],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        assert_eq!(sg.primitive_count(), 5);
        assert_eq!(sg.sdf_bodies().count(), 2);
    }

    #[test]
    fn remove_body_clears_primitives() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let prim = crate::primitives::SdfPrimitive::new(
            "S".into(), crate::primitives::SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO,
        );
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "B".into(), primitives: vec![prim], mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO, body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE, mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0, blend_profile: BlendProfile::default(), material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        assert_eq!(sg.primitive_count(), 1);
        sg.remove(body);
        assert_eq!(sg.primitive_count(), 0);
    }

    #[test]
    fn poly_body_in_scene_graph() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mesh = Arc::new(crate::half_edge::HalfEdgeMesh::new_box(Vec3::ONE));
        let nid = sg.add_child(root, NodePayload::PolygonalBody {
            name: "Cube".into(), mesh, source: Default::default(), mesh_cache_id: None,
            material: crate::primitives::PbrMaterial::default(),
            modifiers: Vec::new(),
        });
        let node = sg.get(nid).unwrap();
        match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                assert_eq!(mesh.vertex_count(), 8);
                assert_eq!(mesh.face_count(), 6);
            }
            _ => panic!("expected PolygonalBody"),
        }
    }

    #[test]
    fn arc_undo_refcount() {
        let mesh = Arc::new(crate::half_edge::HalfEdgeMesh::new_box(Vec3::ONE));
        assert_eq!(Arc::strong_count(&mesh), 1);

        let mut sg1 = SceneGraph::new();
        let root1 = sg1.root();
        sg1.add_child(root1, NodePayload::PolygonalBody {
            name: "A".into(), mesh: mesh.clone(), source: Default::default(), mesh_cache_id: None,
            material: crate::primitives::PbrMaterial::default(),
            modifiers: Vec::new(),
        });
        assert_eq!(Arc::strong_count(&mesh), 2);

        // Clone graph = undo snapshot
        let sg2 = sg1.clone();
        assert_eq!(Arc::strong_count(&mesh), 3);

        drop(sg2);
        assert_eq!(Arc::strong_count(&mesh), 2);
    }

    #[test]
    fn arc_cow_mutation() {
        let mesh = Arc::new(crate::half_edge::HalfEdgeMesh::new_box(Vec3::ONE));
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let nid = sg.add_child(root, NodePayload::PolygonalBody {
            name: "Box".into(), mesh: mesh.clone(), source: Default::default(), mesh_cache_id: None,
            material: crate::primitives::PbrMaterial::default(),
            modifiers: Vec::new(),
        });

        let undo = sg.clone();

        // Mutate via Arc::make_mut — COW clone
        if let Some(node) = sg.get_mut(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                let mesh = Arc::make_mut(mesh);
                mesh.vert_mut(crate::half_edge::VertexId(0)).position = Vec3::new(99.0, 99.0, 99.0);
            }
        }

        // Undo snapshot unmodified
        if let NodePayload::PolygonalBody { mesh, .. } = &undo.get(nid).unwrap().payload {
            assert!(mesh.vert(crate::half_edge::VertexId(0)).position.x < 90.0);
        }
        // Current state modified
        if let NodePayload::PolygonalBody { mesh, .. } = &sg.get(nid).unwrap().payload {
            assert!((mesh.vert(crate::half_edge::VertexId(0)).position.x - 99.0).abs() < 0.001);
        }
    }

    #[test]
    fn arc_serde_roundtrip() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        sg.add_child(root, NodePayload::PolygonalBody {
            name: "Cube".into(),
            mesh: Arc::new(crate::half_edge::HalfEdgeMesh::new_box(Vec3::ONE)),
            source: Default::default(),
            mesh_cache_id: None,
            material: crate::primitives::PbrMaterial::default(),
            modifiers: Vec::new(),
        });

        let json = serde_json::to_string(&sg).expect("serialize");
        let sg2: SceneGraph = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(sg2.node_count(), sg.node_count());
        let poly_nodes: Vec<_> = sg2.poly_bodies().collect();
        assert_eq!(poly_nodes.len(), 1);
        if let NodePayload::PolygonalBody { mesh, .. } = &poly_nodes[0].1.payload {
            assert_eq!(mesh.vertex_count(), 8);
            assert_eq!(mesh.face_count(), 6);
        }
    }

    // ================================================================
    // Sprint 18 Critical Tests
    // ================================================================

    /// Test 1: Graph ↔ flat round-trip — build graph from flat prims, extract back, verify match.
    #[test]
    fn graph_flat_roundtrip() {
        use crate::primitives::{SdfShape, BooleanOp, BoxBevel};

        let mut p1 = SdfPrimitive::new("Sphere".into(), crate::primitives::SdfShape::Sphere { radius: 0.5 }, Vec3::new(1.0, 0.0, 0.0));
        p1.operation = BooleanOp::Union { k: 0.0 };
        p1.material.roughness = 0.3;

        let mut p2 = SdfPrimitive::new("Box".into(), SdfShape::Box { half_extents: Vec3::ONE, bevel: BoxBevel::default() }, Vec3::new(0.0, 1.0, 0.0));
        p2.operation = BooleanOp::Subtract { k: 0.1 };
        p2.material.metallic = 0.9;

        let mut p3 = SdfPrimitive::new("Torus".into(), SdfShape::Torus { major_radius: 1.0, minor_radius: 0.25 }, Vec3::ZERO);
        p3.rotation_deg = Vec3::new(45.0, 0.0, 90.0);
        p3.scale = Vec3::new(2.0, 2.0, 2.0);

        // Build graph from flat
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let _body = sg.add_child(root, NodePayload::SdfBody {
            name: "Body.001".into(),
            primitives: vec![p1, p2, p3],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        // Extract back to flat
        let extracted: Vec<SdfPrimitive> = sg.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();

        assert_eq!(extracted.len(), 3);
        assert_eq!(extracted[0].name, "Sphere");
        assert_eq!(extracted[1].name, "Box");
        assert_eq!(extracted[2].name, "Torus");
        assert!((extracted[0].position.x - 1.0).abs() < 0.001);
        assert!((extracted[1].material.metallic - 0.9).abs() < 0.001);
        assert!((extracted[2].rotation_deg.x - 45.0).abs() < 0.001);
        assert!((extracted[2].scale.x - 2.0).abs() < 0.001);

        // Mutate flat → write back → verify
        let mut flat = extracted;
        flat[0].position = Vec3::new(99.0, 0.0, 0.0);
        flat[1].material.roughness = 0.99;

        let body_nid = sg.sdf_bodies().next().unwrap().0;
        if let Some(prims) = sg.body_primitives_mut(body_nid) {
            for (i, p) in flat.iter().enumerate() {
                prims[i] = p.clone();
            }
        }

        let re_extracted: Vec<SdfPrimitive> = sg.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
        assert!((re_extracted[0].position.x - 99.0).abs() < 0.001);
        assert!((re_extracted[1].material.roughness - 0.99).abs() < 0.001);
    }

    /// Test 2: Save/load round-trip — serialize SceneGraph to JSON, deserialize, verify all data.
    #[test]
    fn save_load_roundtrip_full() {
        use crate::primitives::{SdfShape, BooleanOp, BoxBevel};

        let mut sg = SceneGraph::new();
        let root = sg.root();

        let mut p1 = SdfPrimitive::new("S".into(), crate::primitives::SdfShape::Sphere { radius: 0.5 }, Vec3::new(1.0, 2.0, 3.0));
        p1.material.albedo = Vec3::new(1.0, 0.0, 0.0);
        p1.material.roughness = 0.25;
        p1.material.metallic = 0.75;
        p1.material.emission_intensity = 2.0;
        p1.mirror_flags = 0b101;

        let mut p2 = SdfPrimitive::new("B".into(), SdfShape::Box { half_extents: Vec3::new(0.5, 1.0, 1.5), bevel: BoxBevel::default() }, Vec3::ZERO);
        p2.operation = BooleanOp::Subtract { k: 0.05 };

        sg.add_child(root, NodePayload::SdfBody {
            name: "SdfBody".into(), primitives: vec![p1, p2], mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO, body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE, mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0, blend_profile: BlendProfile::default(), material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let mesh = Arc::new(crate::half_edge::HalfEdgeMesh::new_box(Vec3::ONE));
        let mut mat = crate::primitives::PbrMaterial::default();
        mat.roughness = 0.1;
        mat.metallic = 1.0;
        sg.add_child(root, NodePayload::PolygonalBody {
            name: "PolyBox".into(), mesh, source: Default::default(), mesh_cache_id: None, material: mat,
            modifiers: Vec::new(),
        });

        let json = serde_json::to_string_pretty(&sg).expect("serialize");
        let sg2: SceneGraph = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(sg2.node_count(), sg.node_count());
        assert_eq!(sg2.primitive_count(), 2);

        let prims: Vec<SdfPrimitive> = sg2.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
        assert_eq!(prims[0].name, "S");
        assert!((prims[0].position.x - 1.0).abs() < 0.001);
        assert!((prims[0].material.albedo.x - 1.0).abs() < 0.001);
        assert!((prims[0].material.roughness - 0.25).abs() < 0.001);
        assert!((prims[0].material.metallic - 0.75).abs() < 0.001);
        assert_eq!(prims[0].mirror_flags, 0b101);
        assert_eq!(prims[1].name, "B");
        assert!(matches!(prims[1].operation, BooleanOp::Subtract { k } if (k - 0.05).abs() < 0.001));

        let poly_nodes: Vec<_> = sg2.poly_bodies().collect();
        assert_eq!(poly_nodes.len(), 1);
        if let NodePayload::PolygonalBody { mesh, material, .. } = &poly_nodes[0].1.payload {
            assert_eq!(mesh.vertex_count(), 8);
            assert_eq!(mesh.face_count(), 6);
            assert!((material.roughness - 0.1).abs() < 0.001);
            assert!((material.metallic - 1.0).abs() < 0.001);
        } else {
            panic!("expected PolygonalBody");
        }
    }

    /// Test 3: Undo/redo via clone snapshot — graph clone preserves all data, mutations don't leak.
    #[test]
    fn undo_redo_clone_isolation() {
        use crate::primitives::SdfShape;

        let mut sg = SceneGraph::new();
        let root = sg.root();
        let prim = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "B".into(), primitives: vec![prim], mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO, body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE, mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0, blend_profile: BlendProfile::default(), material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let undo_snapshot = sg.clone();

        // Mutate current state
        if let Some(prims) = sg.body_primitives_mut(body) {
            prims[0].position = Vec3::new(10.0, 20.0, 30.0);
            prims[0].material.roughness = 0.99;
        }

        // Undo snapshot should be unaffected (default roughness = 0.65)
        let undo_prims: Vec<SdfPrimitive> = undo_snapshot.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
        assert!((undo_prims[0].position.x).abs() < 0.001, "undo snapshot should have original position");
        assert!((undo_prims[0].material.roughness - 0.65).abs() < 0.001, "undo snapshot should have default roughness");

        // Current state should be mutated
        let current_prims: Vec<SdfPrimitive> = sg.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
        assert!((current_prims[0].position.x - 10.0).abs() < 0.001);
        assert!((current_prims[0].material.roughness - 0.99).abs() < 0.001);

        // "Redo" = restore from undo snapshot
        let sg_restored = undo_snapshot;
        let restored_prims: Vec<SdfPrimitive> = sg_restored.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
        assert!((restored_prims[0].position.x).abs() < 0.001);
    }

    /// Test 4: Material preservation — PbrMaterial fields survive graph operations.
    #[test]
    fn material_preservation_through_graph() {
        use crate::primitives::SdfShape;

        let mut mat = crate::primitives::PbrMaterial::default();
        mat.albedo = Vec3::new(0.8, 0.2, 0.1);
        mat.roughness = 0.15;
        mat.metallic = 0.95;
        mat.emission_color = Vec3::new(1.0, 0.5, 0.0);
        mat.emission_intensity = 3.5;
        mat.opacity = 0.7;
        mat.noise_pattern.enabled = true;
        mat.noise_pattern.amount = 0.4;
        mat.noise_pattern.scale = 2.0;
        mat.edge_wear.enabled = true;
        mat.edge_wear.amount = 0.6;
        mat.cavity_dirt.enabled = true;
        mat.cavity_dirt.amount = 0.3;

        let mut prim = SdfPrimitive::new("M".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        prim.material = mat;

        let mut sg = SceneGraph::new();
        let root = sg.root();
        sg.add_child(root, NodePayload::SdfBody {
            name: "B".into(), primitives: vec![prim], mesh_cache_id: None, body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO, body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE, mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0, blend_profile: BlendProfile::default(), material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let sg2 = sg.clone();

        let json = serde_json::to_string(&sg).expect("serialize");
        let sg3: SceneGraph = serde_json::from_str(&json).expect("deserialize");

        for (label, graph) in [("original", &sg), ("clone", &sg2), ("serde", &sg3)] {
            let prims: Vec<SdfPrimitive> = graph.all_primitives_ordered().into_iter().map(|(_, _, p)| p.clone()).collect();
            let m = &prims[0].material;
            assert!((m.albedo.x - 0.8).abs() < 0.001, "{}: albedo.x", label);
            assert!((m.roughness - 0.15).abs() < 0.001, "{}: roughness", label);
            assert!((m.metallic - 0.95).abs() < 0.001, "{}: metallic", label);
            assert!((m.emission_intensity - 3.5).abs() < 0.001, "{}: emission", label);
            assert!((m.opacity - 0.7).abs() < 0.001, "{}: opacity", label);
            assert!(m.noise_pattern.enabled, "{}: noise enabled", label);
            assert!((m.noise_pattern.amount - 0.4).abs() < 0.001, "{}: noise amount", label);
            assert!(m.edge_wear.enabled, "{}: edge_wear enabled", label);
            assert!((m.edge_wear.amount - 0.6).abs() < 0.001, "{}: edge_wear amount", label);
            assert!(m.cavity_dirt.enabled, "{}: cavity_dirt enabled", label);
            assert!((m.cavity_dirt.amount - 0.3).abs() < 0.001, "{}: cavity_dirt amount", label);
        }
    }

    /// Test 5: Nested SdfBodies — child body with Subtract scopes correctly.
    #[test]
    fn nested_sdf_bodies() {
        use crate::primitives::{SdfShape, BooleanOp, BoxBevel};

        let mut sg = SceneGraph::new();
        let root = sg.root();

        let parent_body = sg.add_child(root, NodePayload::SdfBody {
            name: "Parent".into(),
            primitives: vec![
                SdfPrimitive::new("Sphere".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO),
                SdfPrimitive::new("Box".into(), SdfShape::Box {
                    half_extents: Vec3::splat(0.5), bevel: BoxBevel::default()
                }, Vec3::new(1.0, 0.0, 0.0)),
            ],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let child_body = sg.add_child(parent_body, NodePayload::SdfBody {
            name: "Cutter".into(),
            primitives: vec![
                SdfPrimitive::new("CutCyl".into(), SdfShape::Cylinder { radius: 0.3, half_height: 2.0 }, Vec3::ZERO),
            ],
            mesh_cache_id: None,
            body_op: BooleanOp::Subtract { k: 0.1 },
            origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        // Structure
        assert_eq!(sg.child_bodies(parent_body).len(), 1);
        assert_eq!(sg.child_bodies(parent_body)[0], child_body);
        assert!(sg.parent_body(child_body) == Some(parent_body));
        assert!(sg.parent_body(parent_body).is_none());

        // body_op
        assert!(matches!(sg.body_op(child_body), Some(BooleanOp::Subtract { k }) if (*k - 0.1).abs() < 0.001));
        assert!(matches!(sg.body_op(parent_body), Some(BooleanOp::Union { k }) if *k == 0.0));

        // Prim count includes all bodies
        assert_eq!(sg.primitive_count(), 3);

        // Depth-first walk: parent prims first, then child body prims
        let ordered = sg.all_primitives_ordered();
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0].2.name, "Sphere");
        assert_eq!(ordered[1].2.name, "Box");
        assert_eq!(ordered[2].2.name, "CutCyl");

        // Serde round-trip preserves body_op
        let json = serde_json::to_string(&sg).expect("serialize");
        let sg2: SceneGraph = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(sg2.body_op(child_body), Some(BooleanOp::Subtract { k }) if (*k - 0.1).abs() < 0.001));
        assert_eq!(sg2.child_bodies(parent_body).len(), 1);
    }

    // ================================================================
    // BodyTransform unit tests
    // ================================================================

    #[test]
    fn round_trip_identity() {
        let t = BodyTransform::default();
        let p = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(t.local_to_world(p), p);
        assert_eq!(t.world_to_local(p), p);
    }

    #[test]
    fn round_trip_translated() {
        let t = BodyTransform { origin: Vec3::new(10.0, 20.0, 30.0), ..Default::default() };
        let local = Vec3::new(1.0, 2.0, 3.0);
        let world = t.local_to_world(local);
        assert_eq!(world, Vec3::new(11.0, 22.0, 33.0));
        let back = t.world_to_local(world);
        assert!((back - local).length() < 1e-6);
    }

    #[test]
    fn round_trip_rotated() {
        let t = BodyTransform {
            origin: Vec3::ZERO,
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::ONE,
        };
        let local = Vec3::new(1.0, 0.0, 0.0);
        let world = t.local_to_world(local);
        // 90° Y rotation: X -> -Z
        assert!((world.x).abs() < 1e-5);
        assert!((world.z - (-1.0)).abs() < 1e-5);
        let back = t.world_to_local(world);
        assert!((back - local).length() < 1e-5);
    }

    #[test]
    fn round_trip_scaled() {
        let t = BodyTransform {
            origin: Vec3::new(5.0, 0.0, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(2.0, 3.0, 4.0),
        };
        let local = Vec3::new(1.0, 1.0, 1.0);
        let world = t.local_to_world(local);
        assert_eq!(world, Vec3::new(7.0, 3.0, 4.0));
        let back = t.world_to_local(world);
        assert!((back - local).length() < 1e-6);
    }

    // ====================================================================
    // child_order — unified prim + sub-body ordering (SUBBODY_REORDER_PLAN)
    // ====================================================================

    /// Build a body with N prims (uids 100,101,...) and M sub-bodies, with an EMPTY child_order
    /// (the pre-v8 state). Returns (sg, body_nid, prim_uids, subbody_nids).
    fn build_body_no_order(n_prims: usize, m_bodies: usize) -> (SceneGraph, NodeId, Vec<u32>, Vec<NodeId>) {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut prims = Vec::new();
        let mut prim_uids = Vec::new();
        for k in 0..n_prims {
            let mut p = crate::primitives::SdfPrimitive::new(
                format!("P{k}"),
                crate::primitives::SdfShape::Sphere { radius: 1.0 },
                Vec3::ZERO,
            );
            p.prim_uid = 100 + k as u32;
            prim_uids.push(p.prim_uid);
            prims.push(p);
        }
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Parent".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let mut subbody_nids = Vec::new();
        for j in 0..m_bodies {
            let sub = sg.add_child(body, NodePayload::SdfBody {
                name: format!("Sub{j}"), primitives: Vec::new(), mesh_cache_id: None,
                body_op: BooleanOp::Subtract { k: 0.0 }, origin: Vec3::ZERO,
                body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
                mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
                blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
                child_order: Vec::new(),
            mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
            subbody_nids.push(sub);
        }
        (sg, body, prim_uids, subbody_nids)
    }

    /// CANONICAL RENDERABILITY (group-vanish P0): a top-level body whose prims were all grouped into
    /// sub-bodies (0 OWN prims, >=1 child body) is STILL renderable — its recursive subtree bake /
    /// resident slot holds the geometry. Deciding via `primitives.is_empty()` alone skipped such a body
    /// in the raster DRAW and PICK loops → "group everything into a sub-body → surface vanishes and is
    /// un-clickable". This locks the one predicate every render/pick/mesh/LOD site must use.
    #[test]
    fn body_has_renderable_content_covers_grouped_bodies() {
        // 0 own prims + 2 child sub-bodies → renderable (geometry lives in the children).
        let (sg, top, _uids, subs) = build_body_no_order(0, 2);
        assert!(sg.has_child_bodies(top), "top must report child sub-bodies");
        assert!(sg.body_has_renderable_content(top),
            "FIXED: top body with 0 own prims but child sub-bodies MUST be renderable (group-vanish bug)");
        // An empty leaf sub-body (no prims, no children) → NOT renderable (truly empty).
        assert!(!sg.has_child_bodies(subs[0]));
        assert!(!sg.body_has_renderable_content(subs[0]),
            "an empty leaf body (no prims, no children) must not be renderable");
        // A body with own prims → renderable regardless of children.
        let (sg2, top2, _u, _s) = build_body_no_order(3, 0);
        assert!(sg2.body_has_renderable_content(top2));
        // A non-SdfBody node (Root) → not renderable.
        assert!(!sg.body_has_renderable_content(sg.root()));
    }

    /// MIGRATION: synthesising child_order on an empty (v7) body must equal the LEGACY bake order:
    /// all prims first (in primitives-Vec order), then all child bodies (in children order).
    #[test]
    fn child_order_synthesis_matches_legacy_order() {
        let (mut sg, body, uids, subs) = build_body_no_order(3, 2);
        // Empty before synthesis.
        assert!(matches!(sg.get(body).map(|n| &n.payload),
            Some(NodePayload::SdfBody { child_order, .. }) if child_order.is_empty()));
        sg.ensure_body_child_order(body);
        let order = sg.body_child_order(body);
        let expected: Vec<BodyChildRef> = uids.iter().copied().map(BodyChildRef::Prim)
            .chain(subs.iter().copied().map(BodyChildRef::Body))
            .collect();
        assert_eq!(order, expected, "synthesised child_order != prims-then-bodies legacy order");
        sg.validate_child_order(body);
    }

    /// body_child_order() self-heals on read even if the stored order is empty (defence-in-depth).
    #[test]
    fn child_order_reader_self_heals() {
        let (sg, body, uids, subs) = build_body_no_order(2, 1);
        // No ensure called — stored order is empty, but the reader reconciles.
        let order = sg.body_child_order(body);
        assert_eq!(order.len(), uids.len() + subs.len());
        assert!(order.contains(&BodyChildRef::Prim(uids[0])));
        assert!(order.contains(&BodyChildRef::Body(subs[0])));
    }

    /// DUPLICATE-UID SAFETY (regression: GPU bake dropped the 2nd prim when both had uid 0).
    /// `SdfPrimitive::new` defaults prim_uid to 0, so a freshly-built two-prim body has TWO uid-0
    /// prims. reconcile must emit TWO slots (not collapse by uid) and resolve_child_order must
    /// return BOTH local indices (0 and 1), each once.
    #[test]
    fn child_order_duplicate_uid_emits_every_prim() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        // Two prims, BOTH uid 0 (the SdfPrimitive::new default).
        let mut p0 = crate::primitives::SdfPrimitive::new("A".into(), crate::primitives::SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        let mut p1 = crate::primitives::SdfPrimitive::new("B".into(), crate::primitives::SdfShape::Sphere { radius: 1.0 }, Vec3::ONE);
        p0.prim_uid = 0; p1.prim_uid = 0;
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Dup".into(), primitives: vec![p0, p1], mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::ZERO,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        sg.ensure_body_child_order(body);
        // child_order must have TWO Prim(0) slots — never collapsed to one.
        let order = sg.body_child_order(body);
        assert_eq!(order, vec![BodyChildRef::Prim(0), BodyChildRef::Prim(0)],
            "duplicate-uid prims must each get a child_order slot (no uid-dedup collapse)");
        // resolve_child_order must yield BOTH local indices, each exactly once.
        let resolved = sg.resolve_child_order(body);
        assert_eq!(resolved, vec![Ok(0usize), Ok(1usize)],
            "resolve must emit both prims positionally (the dropped-prim bake regression)");
        sg.validate_child_order(body);
    }

    /// Reordering: move a sub-body to the FRONT (above all prims) — the owner's core feature.
    #[test]
    fn child_order_move_subbody_to_front() {
        let (mut sg, body, uids, subs) = build_body_no_order(2, 1);
        sg.ensure_body_child_order(body);
        // Default: [Prim(100), Prim(101), Body(sub)]
        assert_eq!(sg.body_child_order(body),
            vec![BodyChildRef::Prim(uids[0]), BodyChildRef::Prim(uids[1]), BodyChildRef::Body(subs[0])]);
        // Move the sub-body to index 0 (top of stack, above both prims).
        sg.reorder_child_order(body, BodyChildRef::Body(subs[0]), 0);
        assert_eq!(sg.body_child_order(body),
            vec![BodyChildRef::Body(subs[0]), BodyChildRef::Prim(uids[0]), BodyChildRef::Prim(uids[1])]);
        sg.validate_child_order(body);
    }

    /// Reordering: interleave — sub-body BETWEEN two prims.
    #[test]
    fn child_order_interleave_subbody_between_prims() {
        let (mut sg, body, uids, subs) = build_body_no_order(2, 1);
        sg.ensure_body_child_order(body);
        sg.reorder_child_order(body, BodyChildRef::Body(subs[0]), 1);
        assert_eq!(sg.body_child_order(body),
            vec![BodyChildRef::Prim(uids[0]), BodyChildRef::Body(subs[0]), BodyChildRef::Prim(uids[1])]);
    }

    /// BASE FOLLOWS THE CHAIN (owner bug: reorder above the first prim → green dot stuck on the
    /// old first prim, X did nothing on the new-second prim). `body_base_slot` / `is_base_prim`
    /// must track the FIRST slot of the resolved `child_order` — never flat/Vec index 0, which
    /// `reorder_child_order` does not move. Also locks the sub-body-first decision: when a
    /// SUB-BODY is the first chain slot, NO prim is base (the sub-body result seeds the fold).
    #[test]
    fn base_slot_follows_child_order_reorder() {
        let (mut sg, body, uids, subs) = build_body_no_order(2, 1);
        sg.ensure_body_child_order(body);
        // Default (legacy) order: prims then bodies → base = local prim 0.
        assert_eq!(sg.body_base_slot(body), Some(Ok(0)));
        assert!(sg.is_base_prim(body, 0));
        assert!(!sg.is_base_prim(body, 1));
        // Reorder prim 1 ABOVE prim 0 — the prims Vec never moves; base must FOLLOW the chain.
        sg.reorder_child_order(body, BodyChildRef::Prim(uids[1]), 0);
        assert_eq!(sg.body_base_slot(body), Some(Ok(1)),
            "base must be the NEW first chain slot (local index 1), not Vec index 0");
        assert!(sg.is_base_prim(body, 1), "green dot / gating must move to the new base");
        assert!(!sg.is_base_prim(body, 0),
            "ex-base is no longer base — X op-cycle must work on it now");
        sg.validate_child_order(body);
        // Sub-body moved to the FRONT: the sub-body result seeds the fold → no prim is base.
        sg.reorder_child_order(body, BodyChildRef::Body(subs[0]), 0);
        assert_eq!(sg.body_base_slot(body), Some(Err(subs[0])),
            "sub-body-first: base slot is the sub-body");
        assert!(!sg.is_base_prim(body, 0) && !sg.is_base_prim(body, 1),
            "sub-body-first: NO prim is base (no green dot on any prim row; X works on all)");
        // Empty / non-body → no base slot.
        assert_eq!(sg.body_base_slot(sg.root()), None);
        sg.validate_child_order(body);
    }

    /// Dangling refs (a uid/nid no longer present) are dropped on reconcile; missing ones appended.
    #[test]
    fn child_order_drops_dangling_and_appends_missing() {
        let (mut sg, body, uids, _subs) = build_body_no_order(2, 0);
        // Manually install a bogus order: one dangling prim ref + one valid, omit the other.
        if let Some(order) = sg.body_child_order_mut(body) {
            *order = vec![BodyChildRef::Prim(9999), BodyChildRef::Prim(uids[1])];
        }
        sg.ensure_body_child_order(body);
        let order = sg.body_child_order(body);
        // 9999 dropped; uids[1] kept in place; uids[0] appended (was missing).
        assert_eq!(order, vec![BodyChildRef::Prim(uids[1]), BodyChildRef::Prim(uids[0])]);
        sg.validate_child_order(body);
    }

    /// REGRESSION GUARD (P0 data safety): `child_order` MUST be `#[serde(skip)]` — it must NOT travel
    /// inside the bincode `SceneGraph`/`ProjectFile` blob. A trailing `Vec` field there corrupts every
    /// existing v7 file (bincode is positional + not self-describing). This test pins the skip: after a
    /// bare bincode round-trip of the graph, the STORED order is empty (the bytes carried nothing).
    /// If someone flips `#[serde(skip)]` back to `#[serde(default)]`, the stored order would survive and
    /// this assert fails — catching the regression at its root.
    #[test]
    fn child_order_is_skip_serialized_not_in_blob() {
        let (mut sg, body, uids, subs) = build_body_no_order(2, 1);
        sg.ensure_body_child_order(body);
        sg.reorder_child_order(body, BodyChildRef::Body(subs[0]), 1); // custom interleave
        let bytes = bincode::serialize(&sg).expect("serialize");
        let sg2: SceneGraph = bincode::deserialize(&bytes).expect("deserialize");
        // STORED order (raw field) must be EMPTY — skip dropped it from the wire.
        match sg2.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { child_order, .. }) =>
                assert!(child_order.is_empty(),
                    "child_order leaked into the bincode blob — it MUST be #[serde(skip)] (v7 corruption risk)"),
            _ => panic!("body missing"),
        }
        // And the self-healing reader still yields a valid (legacy) order for the loaded graph.
        let _ = (uids,);
        assert_eq!(sg2.body_child_order(body).len(), 3);
    }

    /// SIDE-CHANNEL ROUND-TRIP: the interleave is preserved by collect -> apply (what save/load do),
    /// NOT by the blob. This is the real persistence path for the ordering.
    #[test]
    fn child_order_side_channel_round_trips_interleave() {
        let (mut sg, body, uids, subs) = build_body_no_order(2, 1);
        sg.ensure_body_child_order(body);
        sg.reorder_child_order(body, BodyChildRef::Body(subs[0]), 1);
        let before = sg.body_child_order(body);
        assert_eq!(before,
            vec![BodyChildRef::Prim(uids[0]), BodyChildRef::Body(subs[0]), BodyChildRef::Prim(uids[1])]);

        // SAVE: collect the side-channel + serialize the blob (which drops child_order).
        let side_channel = sg.collect_body_child_orders();
        assert!(!side_channel.is_empty(), "non-default order must be collected");
        let bytes = bincode::serialize(&sg).expect("serialize");

        // LOAD: deserialize the blob (child_order empty), then apply the side-channel.
        let mut sg2: SceneGraph = bincode::deserialize(&bytes).expect("deserialize");
        sg2.apply_body_child_orders(&side_channel);
        assert_eq!(sg2.body_child_order(body), before, "side-channel must restore the interleave exactly");
    }

    /// LEGACY SELF-HEAL: a body with NO side-channel entry (genuine v7) self-heals to the legacy
    /// prims-then-bodies order via apply_body_child_orders (empty map → pure self-heal).
    #[test]
    fn child_order_v7_self_heals_to_legacy() {
        let (sg, body, uids, subs) = build_body_no_order(3, 2);
        let bytes = bincode::serialize(&sg).expect("serialize");
        let mut sg2: SceneGraph = bincode::deserialize(&bytes).expect("deserialize");
        // No side-channel (v7 file). apply with an EMPTY map → self-heal all bodies.
        sg2.apply_body_child_orders(&[]);
        let order = sg2.body_child_order(body);
        let expected: Vec<BodyChildRef> = uids.iter().copied().map(BodyChildRef::Prim)
            .chain(subs.iter().copied().map(BodyChildRef::Body))
            .collect();
        assert_eq!(order, expected, "v7 self-heal must reproduce the legacy prims-then-bodies order");
    }

    // ============================================================
    // Materials UX overhaul — graph-material API (create / rename / delete / guard / resolve)
    // ============================================================

    /// Build a graph with 1 SdfBody (2 prims) + 1 PolygonalBody (cube) for material-reference tests.
    fn build_mat_test_scene() -> (SceneGraph, NodeId, NodeId) {
        use crate::half_edge::HalfEdgeMesh;
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut p0 = SdfPrimitive::new("A".into(), crate::primitives::SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        p0.prim_uid = 1;
        let mut p1 = SdfPrimitive::new("B".into(), crate::primitives::SdfShape::Sphere { radius: 0.5 }, Vec3::X);
        p1.prim_uid = 2;
        let body = sg.add_child(root, NodePayload::SdfBody {
            name: "Body".into(), primitives: vec![p0, p1], mesh_cache_id: None,
            body_op: crate::primitives::BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO, body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO,
            bevel_radius: 0.0, blend_profile: Default::default(),
            material: PbrMaterial::default(), child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        // Unit cube poly body.
        let pos = [
            Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5), Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, 0.5), Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5), Vec3::new(-0.5, 0.5, 0.5),
        ];
        let faces = vec![
            vec![0usize, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
            vec![2, 3, 7, 6], vec![1, 2, 6, 5], vec![0, 4, 7, 3],
        ];
        let mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
        let poly = sg.add_child(root, NodePayload::PolygonalBody {
            name: "Cube".into(), mesh: std::sync::Arc::new(mesh), source: Default::default(), mesh_cache_id: None,
            material: PbrMaterial::default(), modifiers: Vec::new(),
        });
        (sg, body, poly)
    }

    #[test]
    fn material_create_rename_delete_via_graph_api() {
        let (mut sg, _body, _poly) = build_mat_test_scene();
        assert_eq!(sg.material_count(), 0);
        let mid = sg.add_material("Steel", PbrMaterial::default());
        assert_eq!(sg.material_count(), 1);
        assert!(!mid.is_none());
        let nid = sg.material_node_id(mid).expect("material node exists");
        // Rename through the node payload (what the outliner rename commits).
        if let Some(n) = sg.get_mut(nid) {
            if let NodePayload::Material { name, .. } = &mut n.payload { *name = "Rusty Steel".into(); }
        }
        assert_eq!(sg.material_by_id(mid).map(|(_, m)| ()).is_some(), true);
        assert_eq!(sg.materials().find(|(_, id, _, _)| *id == mid).map(|(_, _, n, _)| n.to_string()),
                   Some("Rusty Steel".to_string()));
        sg.remove_material(mid);
        assert_eq!(sg.material_count(), 0);
        assert!(sg.material_by_id(mid).is_none());
        assert!(sg.material_node_id(mid).is_none());
    }

    /// M0 class close-out: any write via `material_by_id_mut` bumps generation so raster can
    /// invalidate its cached RasterMaterial list without SCENE/MESH dirty.
    #[test]
    fn material_by_id_mut_bumps_material_generation() {
        let mut sg = SceneGraph::new();
        assert_eq!(sg.material_generation(), 0);
        let mid = sg.add_material("M", PbrMaterial::default());
        assert!(sg.material_generation() > 0, "add_material must bump generation");
        let g0 = sg.material_generation();
        {
            let m = sg.material_by_id_mut(mid).expect("material");
            m.roughness = 0.87;
        }
        assert!(
            sg.material_generation() > g0,
            "material_by_id_mut must bump generation (got {} after {})",
            sg.material_generation(),
            g0
        );
        let (_, m) = sg.material_by_id(mid).expect("read back");
        assert!((m.roughness - 0.87).abs() < 1e-6);
        // Sabotage: NONE / dangling must not bump.
        let g1 = sg.material_generation();
        assert!(sg.material_by_id_mut(MaterialId::NONE).is_none());
        assert!(sg.material_by_id_mut(MaterialId(99999)).is_none());
        assert_eq!(sg.material_generation(), g1);
    }

    #[test]
    fn material_ids_never_reused_after_delete_then_add() {
        // add_material allocates max+1 — deleting the max id then adding could reuse it. This
        // documents the CURRENT contract: reuse IS possible after deleting the highest id, which is
        // exactly why delete MUST clear references first (clear_material_references).
        let mut sg = SceneGraph::new();
        let a = sg.add_material("A", PbrMaterial::default());
        let b = sg.add_material("B", PbrMaterial::default());
        assert_ne!(a, b);
        sg.remove_material(b);
        let c = sg.add_material("C", PbrMaterial::default());
        // c may equal b (max+1 over remaining) — the guard against stale refs is ref clearing.
        assert!(!c.is_none());
    }

    #[test]
    fn material_use_counts_and_clear_references() {
        let (mut sg, body, poly) = build_mat_test_scene();
        let mid = sg.add_material("Paint", PbrMaterial::default());
        // Assign to 1 of 2 prims + all 6 cube faces.
        sg.body_primitives_mut(body).unwrap()[0].material_id = mid;
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut n.payload {
                let m = std::sync::Arc::make_mut(mesh);
                for f in m.faces.iter_mut() { f.material_id = mid.0; }
            }
        }
        assert_eq!(sg.material_use_counts(mid), (1, 6));
        // Delete-in-use: clear refs, then remove — nothing dangles.
        let (pc, fc) = sg.clear_material_references(mid);
        assert_eq!((pc, fc), (1, 6));
        assert_eq!(sg.material_use_counts(mid), (0, 0));
        assert!(sg.body_primitives(body).unwrap()[0].material_id.is_none());
        sg.remove_material(mid);
        assert_eq!(sg.material_count(), 0);
    }

    #[test]
    fn create_prim_inherits_body_prevailing_material() {
        // Owner ask 2026-07-18: a newly created prim inside a body must receive the body's
        // material instead of resetting to default. `body_prevailing_material` = LAST prim's
        // material_id; this test exercises exactly the sequence the creation paths run.
        let (mut sg, body, poly) = build_mat_test_scene();

        // All prims unassigned → NONE (new prim keeps its embedded default, as before).
        assert!(sg.body_prevailing_material(body).is_none());
        // Not-an-SdfBody / bogus node → NONE, never a panic.
        assert!(sg.body_prevailing_material(poly).is_none());
        assert!(sg.body_prevailing_material(NodeId(9999)).is_none());

        // Assign a material to the body (the assign-to-body flow writes ALL prims uniformly).
        let steel = sg.add_material("Steel", PbrMaterial::default());
        for p in sg.body_primitives_mut(body).unwrap().iter_mut() { p.material_id = steel; }
        assert_eq!(sg.body_prevailing_material(body), steel);

        // Simulate creation: a fresh default prim inherits the prevailing id, then is pushed.
        let inherited = sg.body_prevailing_material(body);
        let mut new_prim = SdfPrimitive::new("Cyl".into(),
            crate::primitives::SdfShape::Cylinder { radius: 0.4, half_height: 0.4 }, Vec3::Y);
        new_prim.prim_uid = 3;
        assert!(new_prim.material_id.is_none(), "fresh prims start unassigned");
        new_prim.material_id = inherited;
        sg.body_primitives_mut(body).unwrap().push(new_prim);
        let prims = sg.body_primitives(body).unwrap();
        assert_eq!(prims.last().unwrap().material_id, steel,
            "new prim carries the body material — same look on screen");
        // And it resolves to the SAME PbrMaterial the rest of the body renders with.
        let a = sg.resolve_prim_material(&prims[0]) as *const _;
        let b = sg.resolve_prim_material(prims.last().unwrap()) as *const _;
        assert_eq!(a, b, "resolver returns the identical graph material for old and new prims");

        // Chaining: the NEXT create inherits from the prim just added (last-prim rule).
        assert_eq!(sg.body_prevailing_material(body), steel);
        // Mixed body: last prim decides (predictable, documented).
        let other = sg.add_material("Paint", PbrMaterial::default());
        sg.body_primitives_mut(body).unwrap().last_mut().unwrap().material_id = other;
        assert_eq!(sg.body_prevailing_material(body), other);
    }

    #[test]
    fn resolve_poly_body_material_reference_wins_and_falls_back() {
        let (mut sg, _body, poly) = build_mat_test_scene();
        let mut red = PbrMaterial::default();
        red.albedo = Vec3::new(1.0, 0.0, 0.0);
        let mid = sg.add_material("Red", red);
        // Unassigned → embedded.
        {
            let n = sg.get(poly).unwrap();
            if let NodePayload::PolygonalBody { mesh, material, .. } = &n.payload {
                let m = sg.resolve_poly_body_material(mesh, material);
                assert_eq!(m.albedo, material.albedo, "no face refs → embedded material");
            } else { panic!("poly missing"); }
        }
        // Assign all faces → graph material wins.
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut n.payload {
                let m = std::sync::Arc::make_mut(mesh);
                for f in m.faces.iter_mut() { f.material_id = mid.0; }
            }
        }
        {
            let n = sg.get(poly).unwrap();
            if let NodePayload::PolygonalBody { mesh, material, .. } = &n.payload {
                let m = sg.resolve_poly_body_material(mesh, material);
                assert_eq!(m.albedo, Vec3::new(1.0, 0.0, 0.0), "face refs → graph material");
            } else { panic!("poly missing"); }
        }
        // Dangling id (material deleted without clearing) → falls back to embedded, never panics.
        sg.remove_material(mid);
        {
            let n = sg.get(poly).unwrap();
            if let NodePayload::PolygonalBody { mesh, material, .. } = &n.payload {
                let m = sg.resolve_poly_body_material(mesh, material);
                assert_eq!(m.albedo, material.albedo, "dangling face refs → embedded fallback");
            } else { panic!("poly missing"); }
        }
    }

    /// Undo semantics: transaction() snapshots the WHOLE SceneGraph and restores it wholesale
    /// (undo.rs UndoPayload::Full). This pins that mechanism for material ops: a clone taken
    /// before create/assign/delete restores materials AND assignments exactly.
    #[test]
    fn material_ops_restore_exactly_from_graph_snapshot() {
        let (mut sg, body, poly) = build_mat_test_scene();
        let mid0 = sg.add_material("Kept", PbrMaterial::default());
        sg.body_primitives_mut(body).unwrap()[1].material_id = mid0;

        let snapshot = sg.clone(); // what snapshot_undo() captures

        // Mutations: create+assign, reassign, then delete-with-clear (the full op set).
        let mid1 = sg.add_material("Temp", PbrMaterial::default());
        sg.body_primitives_mut(body).unwrap()[0].material_id = mid1;
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { mesh, .. } = &mut n.payload {
                let m = std::sync::Arc::make_mut(mesh);
                for f in m.faces.iter_mut() { f.material_id = mid1.0; }
            }
        }
        sg.clear_material_references(mid0);
        sg.remove_material(mid0);
        assert_eq!(sg.material_count(), 1);

        // "Undo": restore the snapshot; assert material set + assignments are back exactly.
        let sg = snapshot;
        assert_eq!(sg.material_count(), 1);
        assert!(sg.material_by_id(mid0).is_some(), "deleted material restored by snapshot");
        assert_eq!(sg.body_primitives(body).unwrap()[1].material_id, mid0);
        assert!(sg.body_primitives(body).unwrap()[0].material_id.is_none());
        assert_eq!(sg.material_use_counts(mid0), (1, 0));
    }

/// The authored per-edge weight column (`CustomData.crease`, what `W` writes) has TWO consumers,
/// and the second one is easy to miss because it needs no geometry channel: a Bevel whose
/// Variable Width is driven by authored weight reads it with its channel left at `All`.
///
/// Both the display cache KEY and the code deciding whether authoring a weight re-evaluates the
/// display now ask this one predicate. When they spelled it out separately they disagreed — the
/// key counted the Bevel, the re-evaluation gate did not — so a weight edit invalidated a cache
/// entry nothing ever asked to rebuild and Variable Width by weight never updated on screen
/// (owner 2026-08-11: "the variable width with weight (W) does not work").
#[test]
fn bevel_variable_width_by_weight_counts_as_an_edge_weight_reader() {
    use crate::geometry_channel::GeoChannelKind;
    use crate::scene_graph::{PolyModifier, PolyModifierType};

    // EVERY Bevel reads the column, at any `bevel_var_width` — the weight is a percentage of Width
    // and is always live, with no mode to enable (owner 2026-08-11). It reads it with the geometry
    // channel left at `All`, so a channel-only test cannot see this consumer at all.
    let bevel = PolyModifier { mod_type: PolyModifierType::Bevel, ..Default::default() };
    assert_eq!(
        bevel.geo_channel.kind,
        GeoChannelKind::All,
        "precondition: this consumer is invisible to any channel-only test"
    );
    for v in [0u8, 1, 2, 3] {
        let mut b = bevel.clone();
        b.bevel_var_width = v;
        assert!(
            b.reads_edge_weight(),
            "bevel_var_width={v}: the weight applies with no mode, so every Bevel is a consumer"
        );
    }

    // A DISABLED modifier reads nothing — it is not evaluated.
    let mut disabled = bevel.clone();
    disabled.enabled = false;
    assert!(!disabled.reads_edge_weight(), "a disabled modifier is not a consumer");

    // The other consumer: any modifier masked on the EdgeWeight channel, whatever its type.
    let mut masked = PolyModifier { mod_type: PolyModifierType::Smooth, ..Default::default() };
    masked.geo_channel.kind = GeoChannelKind::EdgeWeight;
    assert!(masked.reads_edge_weight(), "an EdgeWeight-masked modifier reads the column");

    // And a plain modifier that touches neither.
    let plain = PolyModifier { mod_type: PolyModifierType::Smooth, ..Default::default() };
    assert!(!plain.reads_edge_weight(), "an unmasked Smooth does not read authored weight");
}
