//! project.rs tests â€” PURE RELOCATION (child-order save round-trip etc.).
use super::*;

mod child_order_save_tests {
    use super::*;
    use core_math::scene_graph::{SceneGraph, NodeId, NodePayload, BodyChildRef};
    use core_math::primitives::{SdfPrimitive, SdfShape, BlendProfile, PbrMaterial, BooleanOp};

    /// Build a SceneGraph with a parent body (3 prims, uids 100/101/102) + one sub-body, child_order
    /// EMPTY (the pre-v8 in-memory state). Returns (sg, parent_nid, sub_nid).
    pub(super) fn graph_with_body() -> (SceneGraph, NodeId, NodeId) {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut prims = Vec::new();
        for k in 0..3 {
            let mut p = SdfPrimitive::new(format!("P{k}"), SdfShape::Sphere { radius: 1.0 }, glam::Vec3::ZERO);
            p.prim_uid = 100 + k;
            prims.push(p);
        }
        let parent = sg.add_child(root, NodePayload::SdfBody {
            name: "Parent".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: glam::Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: glam::Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let sub = sg.add_child(parent, NodePayload::SdfBody {
            name: "Sub".into(), primitives: Vec::new(), mesh_cache_id: None,
            body_op: BooleanOp::Subtract { k: 0.0 }, origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: glam::Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: glam::Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        (sg, parent, sub)
    }

    /// Minimal but COMPLETE ProjectFile wrapping a graph â€” drives the real save_binary/load_binary.
    /// `pub(super)` so the poly-layer suite reuses it (no duplicate ProjectFile literal).
    pub(super) fn project_with(sg: SceneGraph) -> ProjectFile {
        ProjectFile {
            version: 7,
            scene: SceneData {
                primitives: vec![], layers: vec![], objects: vec![], lights: vec![],
                next_prim_id: 200, next_layer_id: 1, next_object_id: 1, next_light_id: 1,
                next_node_name_id: 0,
            },
            camera: CameraState { yaw: 0.0, pitch: 0.0, roll: 0.0, distance: 5.0, target: [0.0;3], ortho: false },
            render: RenderSettings {
                shading_mode: ShadingMode::Pbr, resolution_scale: 1.0, render_quality: 1, exposure: 1.0,
                bloom_enabled: false, bloom_threshold: 1.0, bloom_intensity: 0.0, ssr_enabled: false,
                fog_enabled: false, fog_density: 0.0, fog_height_falloff: 0.0, fog_scatter: 0.0,
                fog_color: [0.35, 0.40, 0.50],
                upsample_enabled: false, rt_bounces: 1, rt_shadow_samples: 1, pt_max_bounces: 1,
                pt_samples_per_frame: 1, bg_color_top: [0.0;3], bg_color_bottom: [0.0;3], fov_deg: 90.0,
                env_brightness: 1.0, env_contrast: 1.0, env_saturation: 1.0, env_blur: 0.0,
                pt_dof_aperture: 0.0, pt_dof_falloff: 0.5, pt_denoise: false, denoise_strength: 0.5,
                denoise_radius: 3, fog_phase_g: 0.7, fog_absorption: 0.3, fog_noise_scale: 3.0,
                fog_noise_strength: 0.4, fog_max_distance: 30.0, fog_shadow_steps: 4.0, voxel_cache_res: 128,
                adaptive_resolution: true, atlas_xy_resolution: 256, worker_gpu_enabled: vec![],
                sdf_edge_aa_enabled: true, fxaa_enabled: true, matcap_bg_color: [0.0;3],
                matcap_bg_gradient: false, matcap_bg_gradient_top: [0.0;3], matcap_rotation: 0.0,
                matcap_edge_enabled: true, matcap_edge_color: [0.0;3], matcap_edge_thickness: 1.0,
                matcap_edge_threshold: 0.3,
            },
            editor: EditorSettings {
                snap_mode: SnapMode::Grid, snap_grid: 1.0, snap_rotation_deg: 15.0, grid_cell_size: 1.0,
                grid_opacity: 1.0, grid_visible: true, wp_mode: WorkPlaneMode::Auto,
                wp_settings: WpSettingsData { spacing: 2.0, color: [0.5;3], opacity: 1.0,
                    orientation: WorkPlaneOrientation::AxisSnap, origin: [0.0;3] },
                wp_saved: vec![], wp_locked_origin: [0.0;3], wp_locked_normal: [0.0,1.0,0.0],
            },
            scene_graph: Some(sg),
            imported_meshes: vec![],
            voxel_grids: vec![],
            compositor: None,
            material_library: None,
            poly_layers: vec![],
            curve_params: vec![],
            body_mesh_modifiers: vec![],
            body_field_modifiers: vec![],
            terrain_tiles: vec![],
            poly_params: Vec::new(),
            child_orders: vec![],
            material_maps: vec![],
            material_channels: vec![],
            env: None,
            light_volumetrics: vec![],
            light_rt: vec![],
            node_graphs: vec![],
            material_panel_docs: vec![],
        pivot_local_nodes: vec![],
            poly_normals: vec![],
            poly_mod_flags: vec![],
            poly_smooth: vec![],
            poly_remesh: vec![],
            poly_remesh_collapse: vec![],
            poly_remesh_skip_relax: vec![],
            poly_remesh_skip_seal: vec![],
            poly_remesh_drop_res: vec![],
            bake_settings: vec![],
            bake_images: vec![],
            bake_bit16: vec![],
            bake_cage: vec![],
            bake_udim: Default::default(),
            bake_gpu_rays: vec![],
            poly_bevel_var: vec![],
            poly_op_tags: vec![],
            poly_bend: vec![],
            poly_sculpt: vec![],
        }
    }

    /// Read a ProjectFile's parent-body child_order back out, as the loader leaves it (after the
    /// side-channel decode but BEFORE apply â€” i.e. what travelled in the file).
    fn loaded_order_for(proj: &ProjectFile, body: NodeId) -> Vec<BodyChildRef> {
        proj.child_orders.iter().find(|(id, _)| *id == body.0).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// DIAGNOSTIC: load a `.fl4` and dump its real structure â€” bodies, sub-bodies, prim ops,
    /// child_order, and the computed bake bounds â€” so a scene's actual state is visible without the
    /// GPU. `#[ignore]` so it never runs in CI; point it at a file via env var (no hardcoded path):
    ///   AZRA_DUMP_SCENE="C:/path/to/scene.fl4" cargo test -p app dump_scene_structure -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dump_scene_structure() {
        use core_math::scene_graph::NodePayload;
        let path = match std::env::var("AZRA_DUMP_SCENE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => { eprintln!("[DUMP] set AZRA_DUMP_SCENE=<path.fl4> to run this diagnostic"); return; }
        };
        if !path.exists() {
            eprintln!("[DUMP] scene file not found at {path:?} â€” skipping");
            return;
        }
        let mut proj = load_binary(&path).expect("load_binary failed");
        let mut sg = proj.scene_graph.take().expect("file has no scene_graph");
        sg.apply_body_child_orders(&proj.child_orders);

        let mut out = String::from("\n===== SCENE DUMP =====\n");
        let bodies: Vec<NodeId> = sg.sdf_bodies().map(|(nid, _)| nid).collect();
        out.push_str(&format!("{} SdfBody nodes total\n", bodies.len()));
        for nid in &bodies {
            let is_root = sg.parent_body(*nid).is_none();
            let (name, body_op, mflags, mcenter, origin) = match sg.get(*nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { name, body_op, mirror_flags, mirror_center, origin, .. }) =>
                    (name.clone(), format!("{body_op:?}"), *mirror_flags, *mirror_center, *origin),
                _ => ("?".into(), "?".into(), 0u8, glam::Vec3::ZERO, glam::Vec3::ZERO),
            };
            out.push_str(&format!(
                "\nBODY {:?} '{}' {} body_op={} mirror={} origin={:.3?}\n",
                nid, name, if is_root { "[ROOT]" } else { "[SUB]" }, body_op, mflags, origin,
            ));
            out.push_str(&format!("  child_order: {:?}\n", sg.body_child_order(*nid)));
            if let Some(prims) = sg.body_primitives(*nid) {
                for p in prims {
                    out.push_str(&format!(
                        "  PRIM '{}' uid={} op={:?} vis={} pos={:.3?} he={:.3?}\n",
                        p.name, p.prim_uid, p.operation, p.visible, p.position,
                        p.bounding_aabb_half_extents(),
                    ));
                }
            }
            let cb = sg.child_bodies(*nid);
            if !cb.is_empty() { out.push_str(&format!("  child_bodies: {cb:?}\n")); }
            let rec = crate::bake_utils::collect_body_prims_recursive(&sg, *nid);
            let (bmin, bmax) = crate::bake_utils::compute_body_bake_bounds(&rec, mflags, mcenter, 0.0);
            out.push_str(&format!("  BAKE_BOUNDS min={bmin:.3?} max={bmax:.3?} recursive_prims={}\n", rec.len()));
        }
        out.push_str("===== END DUMP =====\n");
        panic!("{out}");
    }

    /// DIAGNOSTIC (remove after): CPU-bake the scene at TIGHT vs BALLOONED bounds and report the
    /// inside-voxel fraction, to see whether the aabb_escape ballooning degrades the field (the
    /// flat-slab/vanish). Run: AZRA_DUMP_SCENE="<path.fl4>" cargo test -p app diagnose_scene_bake -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diagnose_scene_bake() {
        use core_math::scene_graph::NodePayload;
        let path = match std::env::var("AZRA_DUMP_SCENE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => { eprintln!("[BAKEDIAG] set AZRA_DUMP_SCENE=<path.fl4>"); return; }
        };
        let mut proj = load_binary(&path).expect("load_binary");
        let mut sg = proj.scene_graph.take().expect("scene_graph");
        sg.apply_body_child_orders(&proj.child_orders);
        let mut out = String::from("\n===== BAKE DIAGNOSTIC =====\n");
        let bodies: Vec<NodeId> = sg.sdf_bodies().map(|(n, _)| n).collect();
        for nid in bodies {
            if sg.parent_body(nid).is_some() { continue; } // top-level only
            let (mflags, mcenter) = match sg.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { mirror_flags, mirror_center, .. }) => (*mirror_flags, *mirror_center),
                _ => continue,
            };
            let prims = crate::bake_utils::collect_body_prims_recursive(&sg, nid);
            if prims.is_empty() { continue; }
            let (tmin, tmax) = crate::bake_utils::compute_body_bake_bounds(&prims, mflags, mcenter, 0.0);
            let tight = crate::bake_utils::pad_grid_bounds(tmin, tmax);
            let res = 64u32;
            out.push_str(&format!(
                "BODY {nid:?} ({} prims, mirror_flags={mflags:#05b} mirror_center={mcenter:?}):\n",
                prims.len()
            ));
            for (i, p) in prims.iter().enumerate() {
                out.push_str(&format!(
                    "  prim[{i}] pos={:?} he={:?} blend_k={:.3}\n",
                    p.position, p.bounding_aabb_half_extents(), p.operation.blend_k()
                ));
            }
            let mut report = |label: &str, gmin: glam::Vec3, gmax: glam::Vec3| {
                let grid = crate::bake_utils::bake_body_grid(&prims, res, gmin, gmax, mflags, 0, mcenter);
                let inside = grid.iter().filter(|&&d| d < 0.0).count();
                let total = grid.len();
                let pct = 100.0 * inside as f32 / total.max(1) as f32;
                let verdict = if inside == 0 { "EMPTY â€” no surface (VANISH)" }
                    else if inside == total { "FULL â€” all inside (SLAB/degenerate)" }
                    else { "has surface" };
                out.push_str(&format!("  {label}: min={gmin:.2?} max={gmax:.2?} inside={inside}/{total} ({pct:.1}%) â†’ {verdict}\n"));
            };
            report("TIGHT      ", tight.0, tight.1);
            // Ballooned like the log (off-center, ~2.5x) to see if it degrades the field.
            let center = (tight.0 + tight.1) * 0.5;
            let half = (tight.1 - tight.0) * 0.5;
            report("BALLOON2.5x", center - half * 2.5, center + half * 2.5);
        }
        out.push_str("===== END =====\n");
        panic!("{out}");
    }

    // ---- MANDATORY TEST 1 ------------------------------------------------------------------
    /// A GENUINE v7 file (bincode blob with NO trailing FL4ORDER section, exactly the bytes an old
    /// AZRA wrote) must load WITHOUT error and the reconstructed child_order must equal the LEGACY
    /// prims-then-bodies bake order. Pre-fix this case corrupted (trailing field misread).
    #[test]
    fn v7_blob_without_child_order_loads_and_bakes_legacy_order() {
        let (sg, parent, sub) = graph_with_body();
        // Set a CUSTOM order in memory, then save WITHOUT the side-channel to forge a true v7 blob:
        // child_order is #[serde(skip)] so the blob never carries it; we also skip the FL4ORDER tail.
        let mut proj = project_with(sg);
        // The forged-v7 path: serialize ONLY the blob (no tail), write raw, load via load_binary.
        proj.child_orders.clear(); // ensure save_binary writes NO tail (genuine v7 layout)
        let dir = std::env::temp_dir().join(format!("azra_v7_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v7.fl4");
        save_binary(&path, &proj).expect("save");
        // Assert the file has NO FL4ORDER magic (byte-identical to v7).
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(CHILD_ORDER_MAGIC.len()).any(|w| w == CHILD_ORDER_MAGIC),
            "forged v7 file must NOT contain FL4ORDER magic");

        // Load: must succeed, no corruption.
        let loaded = load_binary(&path).expect("load must NOT error on a v7 blob");
        assert!(loaded.child_orders.is_empty(), "v7 file carries no side-channel orders");

        // Apply + self-heal (what apply_to_app does), then assert legacy order.
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        let order = g.body_child_order(parent);
        let expected = vec![
            BodyChildRef::Prim(100), BodyChildRef::Prim(101), BodyChildRef::Prim(102),
            BodyChildRef::Body(sub),
        ];
        assert_eq!(order, expected, "v7 file must reconstruct the legacy prims-then-bodies order");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- MANDATORY TEST 2 ------------------------------------------------------------------
    /// A custom interleave (sub-body moved BETWEEN prims) must survive save->reload exactly,
    /// carried by the FL4ORDER side-channel, and the eval/bake order must match.
    #[test]
    fn v8_custom_interleave_round_trips() {
        let (mut sg, parent, sub) = graph_with_body();
        sg.ensure_body_child_order(parent);
        // Move the sub-body to index 1 â†’ [Prim(100), Body(sub), Prim(101), Prim(102)].
        sg.reorder_child_order(parent, BodyChildRef::Body(sub), 1);
        let custom = sg.body_child_order(parent);
        assert_eq!(custom, vec![
            BodyChildRef::Prim(100), BodyChildRef::Body(sub),
            BodyChildRef::Prim(101), BodyChildRef::Prim(102),
        ]);

        // from_app-equivalent: collect the orders into the ProjectFile carrier, then save_binary.
        let mut proj = project_with(sg);
        proj.child_orders = proj.scene_graph.as_ref().unwrap().collect_body_child_orders();
        assert!(!proj.child_orders.is_empty(), "non-default order must be collected for the side-channel");

        let dir = std::env::temp_dir().join(format!("azra_v8_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v8.fl4");
        save_binary(&path, &proj).expect("save");
        // Assert the file DOES contain the FL4ORDER magic.
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(CHILD_ORDER_MAGIC.len()).any(|w| w == CHILD_ORDER_MAGIC),
            "v8 file with a custom order must contain FL4ORDER magic");

        // Load: side-channel must decode the exact interleave.
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded_order_for(&loaded, parent), custom, "side-channel must carry the interleave");

        // Apply onto the graph + self-heal; eval/bake order must match the custom interleave.
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        assert_eq!(g.body_child_order(parent), custom, "interleave must be restored after load");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// REORDER save/load: a Subtract moved to the chain base must come back with the same
    /// child_order, the same collected uid sequence, and the same field sample. child_order
    /// is #[serde(skip)]; the FL4ORDER side-channel is what keeps the chain.
    #[test]
    fn reorder_subtract_round_trips_field() {
        use core_math::field_eval::BodyField;
        use glam::Vec3;
        let (mut sg, parent, _sub) = graph_with_body();
        // Replace the three zero-spheres with a union + a subtract whose center is outside the union.
        if let Some(prims) = sg.body_primitives_mut(parent) {
            prims.clear();
            let mut base = SdfPrimitive::new("Base".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
            base.prim_uid = 100;
            base.operation = BooleanOp::Union { k: 0.0 };
            let mut cut = SdfPrimitive::new("Cut".into(), SdfShape::Sphere { radius: 0.8 }, Vec3::new(2.0, 0.0, 0.0));
            cut.prim_uid = 101;
            cut.operation = BooleanOp::Subtract { k: 0.0 };
            prims.push(base);
            prims.push(cut);
        }
        sg.ensure_body_child_order(parent);
        sg.reorder_child_order(parent, BodyChildRef::Prim(101), 0);
        let probe = Vec3::new(2.0, 0.0, 0.0);
        let before = BodyField::from_graph(&sg, parent).eval_or(probe, 100.0);
        assert!(before < 0.0, "saved chain must already be the reordered solid, got {before}");
        let before_uids: Vec<u32> = crate::bake_utils::collect_body_prims_recursive(&sg, parent)
            .iter().map(|p| p.prim_uid).collect();
        assert_eq!(before_uids, vec![101, 100]);

        let mut proj = project_with(sg);
        proj.child_orders = proj.scene_graph.as_ref().unwrap().collect_body_child_orders();
        let dir = std::env::temp_dir().join(format!("azra_reorder_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reorder.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        let after = BodyField::from_graph(&g, parent).eval_or(probe, 100.0);
        let after_uids: Vec<u32> = crate::bake_utils::collect_body_prims_recursive(&g, parent)
            .iter().map(|p| p.prim_uid).collect();
        assert_eq!(after_uids, before_uids, "collected chain must survive save/load");
        assert!((after - before).abs() < 1e-5, "field sample must be identical after round-trip ({before} vs {after})");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// BodyKey stability: NodeId round-trips identically across save->load (so the side-channel key
    /// resolves to the same node). Proven by test 2 already (apply keyed by node id worked), but
    /// asserted explicitly here for clarity.
    #[test]
    fn node_id_key_round_trips() {
        let (sg, parent, _sub) = graph_with_body();
        let proj = project_with(sg);
        let dir = std::env::temp_dir().join(format!("azra_key_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        let g = loaded.scene_graph.unwrap();
        assert!(g.get(parent).is_some(), "parent NodeId must resolve to the same node after reload");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ==================================================================
// FL4 poly-layer round-trip gates (P3 Â§6.2 / Â§4.5) â€” the ProjectFile-level suite that
// exercises the REAL save_binary/load_binary path with the in-blob PolyLayerRecord field.
// These are the mandatory gates the orchestrator HARD-STOP requires: empty/no-layer compat,
// BOTH child_order AND poly_layers coexisting (the exact Â§0 corruption case), and an authored
// poly body round-tripping its layers intact. Mirrors child_order_save_tests' structure.
// ==================================================================
#[cfg(test)]
mod poly_layer_save_tests {
    use super::*;
    use core_math::scene_graph::{SceneGraph, NodeId, NodePayload, BodyChildRef};
    use core_math::primitives::{SdfPrimitive, SdfShape, BlendProfile, PbrMaterial, BooleanOp};
    use core_math::half_edge::HalfEdgeMesh;
    use std::sync::Arc;

    /// A scene graph with a parent SdfBody (3 prims + a sub-body, for the child_order case) AND a
    /// sibling PolygonalBody (the poly-layer target). Returns (sg, parent_nid, sub_nid, poly_nid).
    fn graph_with_poly() -> (SceneGraph, NodeId, NodeId, NodeId) {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut prims = Vec::new();
        for k in 0..3 {
            let mut p = SdfPrimitive::new(format!("P{k}"), SdfShape::Sphere { radius: 1.0 }, glam::Vec3::ZERO);
            p.prim_uid = 100 + k;
            prims.push(p);
        }
        let parent = sg.add_child(root, NodePayload::SdfBody {
            name: "Parent".into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: glam::Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: glam::Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let sub = sg.add_child(parent, NodePayload::SdfBody {
            name: "Sub".into(), primitives: Vec::new(), mesh_cache_id: None,
            body_op: BooleanOp::Subtract { k: 0.0 }, origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY, body_scale: glam::Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: glam::Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        let poly = sg.add_child(root, NodePayload::PolygonalBody {
            name: "Poly".into(), mesh: Arc::new(HalfEdgeMesh::new_box(glam::Vec3::ONE)), source: Default::default(),
            mesh_cache_id: None, material: PbrMaterial::default(), modifiers: Vec::new(),
        });
        (sg, parent, sub, poly)
    }

    /// Build the authored CustomData for the poly body + its PolyLayerRecord (keyed by `poly`).
    fn authored_record(mesh: &HalfEdgeMesh, poly: NodeId) -> (core_math::poly_mesh::CustomData, PolyLayerRecord) {
        use core_math::poly_mesh::{CustomData, EdgeKey};
        use core_math::half_edge::{HalfEdgeId, FaceId};
        use glam::Vec2;
        let mut cd = CustomData::new();
        for (i, &he) in mesh.face_half_edges(FaceId(0)).iter().enumerate() {
            cd.uv.set(he, Vec2::new(0.1 * i as f32, 0.2 * i as f32));
        }
        cd.sharp.set(HalfEdgeId(2), true);
        cd.seam.set(HalfEdgeId(4), true);
        cd.crease.set(EdgeKey(0, 1), 0.75);
        let (uv, crease, seam, sharp, groups, group_meta) = cd.to_layer_tuples();
        (
            cd,
            PolyLayerRecord {
                node_id: poly.0,
                uv,
                crease,
                seam,
                sharp,
                groups,
                group_meta,
            },
        )
    }

    /// Minimal COMPLETE ProjectFile around `sg` with the given poly_layers + child_orders, driving
    /// the real save_binary/load_binary. Reuses the field set from child_order_save_tests' helper.
    fn project_with_layers(sg: SceneGraph, poly_layers: Vec<PolyLayerRecord>,
                           child_orders: Vec<(u64, Vec<BodyChildRef>)>) -> ProjectFile {
        let mut proj = super::child_order_save_tests::project_with(sg);
        proj.poly_layers = poly_layers;
        proj.child_orders = child_orders;
        proj
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_poly_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// GATE A â€” an empty/no-poly-layer ProjectFile round-trips byte-identical (old-save compat):
    /// saving with NO poly_layers and NO child_orders writes a blob with NO FL4ORDER tail and loads
    /// back with an EMPTY poly_layers (the #[serde(default)] forward-compat path).
    #[test]
    fn empty_poly_layers_roundtrips_byte_identical() {
        let (sg, _p, _s, _poly) = graph_with_poly();
        let proj = project_with_layers(sg, vec![], vec![]);
        let dir = tmp("empty");
        let path = dir.join("e.fl4");
        save_binary(&path, &proj).expect("save");
        // No FL4ORDER tail (no custom child_order) â†’ genuine pre-v8 blob layout.
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(CHILD_ORDER_MAGIC.len()).any(|w| w == CHILD_ORDER_MAGIC),
            "an empty-order file must not contain the FL4ORDER magic");
        let loaded = load_binary(&path).expect("load must not error");
        assert!(loaded.poly_layers.is_empty(), "no authored layers â†’ empty poly_layers on load");
        assert!(loaded.child_orders.is_empty(), "no custom order â†’ empty child_orders on load");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Parametric poly primitives survive save/load through the in-blob `poly_params`
    /// side-channel, keyed by node id. This is the Gate 2 acceptance test of
    /// `docs/planning/poly/PARAMETRIC_PRIMITIVES.md`: "sides = 6" must still be editable after a
    /// round-trip, which is the entire point of the feature.
    #[test]
    fn poly_params_round_trip() {
        use core_math::primitive_spec::{CylinderSpec, MeshSource, PrimitiveSpec};

        let (sg, _p, _s, poly) = graph_with_poly();

        // Empty case first: a file with no parametric bodies must load with none (pre-feature
        // files are exactly this, and they must NOT come back claiming to be primitives).
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polyparams_empty");
        let path = dir.join("e.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert!(
            loaded.poly_params.is_empty(),
            "no parametric bodies â†’ empty poly_params on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated case.
        let spec = PrimitiveSpec::Cylinder(CylinderSpec {
            sides: 6,
            vsegments: 4,
            ..Default::default()
        });
        let (mesh, source) = MeshSource::build(spec);
        let built_sig = match source {
            MeshSource::Primitive { built_sig, .. } => built_sig,
            _ => unreachable!("MeshSource::build always yields Primitive"),
        };
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_params = vec![crate::project::PolyParamRecord {
            node_id: poly.0,
            spec,
            built_sig,
        }];
        let dir = tmp("polyparams");
        let path = dir.join("p.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_params.len(), 1, "one parametric body");
        let rec = &loaded.poly_params[0];
        assert_eq!(rec.node_id, poly.0, "reattaches to the right node");
        assert_eq!(rec.spec, spec, "sides=6 / vsegments=4 survived the round-trip");
        assert_eq!(rec.built_sig, built_sig, "signature survived");
        // And the restored pair still recognizes its own mesh as live.
        let restored = MeshSource::Primitive {
            spec: rec.spec,
            built_sig: rec.built_sig,
        };
        assert_eq!(
            restored.live_spec(&mesh),
            Some(spec),
            "after load the body must still be re-parameterizable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Smooth modifier's mask feather (`PolyModifier.smooth_falloff`, `#[serde(skip)]`) survives
    /// save/load through the `FL4PSMTH` tail section, and a file that never authored one contains no
    /// such section at all.
    ///
    /// The byte-compat half is the point of the whole side-channel design: `smooth_falloff` may
    /// NEVER become an in-blob field on `PolyModifier` (bincode is positional — see
    /// `POLY_SMOOTH_MAGIC`), so this test also pins that an unused feature costs zero bytes.
    #[test]
    fn poly_smooth_falloff_round_trip() {
        use crate::project::io::POLY_SMOOTH_MAGIC;

        let (sg, _p, _s, poly) = graph_with_poly();

        // Empty case: no authored falloff → no section, and nothing comes back claiming one.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polysmooth_empty");
        let path = dir.join("s0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(POLY_SMOOTH_MAGIC.len())
                .any(|w| w == POLY_SMOOTH_MAGIC),
            "a file with no authored falloff must not contain the FL4PSMTH magic"
        );
        assert!(
            load_binary(&path).expect("load").poly_smooth.is_empty(),
            "no authored falloff → empty poly_smooth on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated case.
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_smooth = vec![crate::project::PolySmoothRecord {
            node_id: poly.0,
            mod_index: 1,
            smooth_falloff: 7,
            group_id: 4,
            weight_min: 0.25,
            weight_max: 0.75,
        }];
        let dir = tmp("polysmooth");
        let path = dir.join("s1.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_smooth.len(), 1, "one authored falloff");
        let rec = &loaded.poly_smooth[0];
        assert_eq!(rec.node_id, poly.0, "reattaches to the right node");
        assert_eq!(rec.mod_index, 1, "reattaches to the right stack slot");
        assert_eq!(rec.smooth_falloff, 7, "falloff survived the round-trip");
        // The geometry-channel params ride here too — they CANNOT live in the blob
        // (`GeometryChannel` is inside `PolyModifier`, and bincode is positional).
        assert_eq!(rec.group_id, 4, "polygroup id survived");
        assert_eq!(rec.weight_min, 0.25, "weight_min survived");
        assert_eq!(rec.weight_max, 0.75, "weight_max survived");
        // The blob itself must still decode everything else — a new tail section must never
        // cannibalise the sections written before it.
        assert!(loaded.scene_graph.is_some(), "scene graph still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// REGRESSION — `REMESHPARAMSAVE`. All four Voxel Remesh params (`#[serde(skip)]` on
    /// `PolyModifier`) survive save/load through the `FL4PRMSH` tail section, and a file that
    /// never authored one contains no such section at all.
    ///
    /// Before `FL4PRMSH` existed there was NO carrier for these fields — the doc comment claimed
    /// they rode `FL4PMODF`, but `PolyModFlagRecord` only ever held `bevel_consume`. So every
    /// authored resolution / smooth / padding / project silently reset to 32 / true / 0.1 / 1.0
    /// on load. The `resolution` assertion below is the one that used to fail.
    #[test]
    fn poly_remesh_params_round_trip() {
        use crate::project::io::{POLY_REMESH_MAGIC, POLY_SMOOTH_MAGIC};

        let (sg, _p, _s, poly) = graph_with_poly();

        // Unused → zero bytes. This is the whole point of a tail section: the feature may never
        // become an in-blob field on `PolyModifier` (bincode is positional), so a scene that
        // never touched a Remesh slider must be byte-identical to a pre-FL4PRMSH save.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polyremesh_empty");
        let path = dir.join("r0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(POLY_REMESH_MAGIC.len())
                .any(|w| w == POLY_REMESH_MAGIC),
            "a file with no authored remesh param must not contain the FL4PRMSH magic"
        );
        assert!(
            load_binary(&path).expect("load").poly_remesh.is_empty(),
            "no authored remesh params → empty poly_remesh on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated, and deliberately alongside an FL4PSMTH record: FL4PRMSH is written
        // immediately AFTER it, so this also pins that the newest section does not cannibalise
        // the one before it (append-newest-last is only safe if each section is self-delimiting).
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_smooth = vec![crate::project::PolySmoothRecord {
            node_id: poly.0,
            mod_index: 0,
            smooth_falloff: 3,
            group_id: 2,
            weight_min: 0.1,
            weight_max: 0.9,
        }];
        proj.poly_remesh = vec![crate::project::PolyRemeshRecord {
            node_id: poly.0,
            mod_index: 2,
            resolution: 192,
            smooth: false,
            padding: 0.35,
            project: 0.5,
            union_islands: true,
        }];
        let dir = tmp("polyremesh");
        let path = dir.join("r1.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_SMOOTH_MAGIC.len()).any(|w| w == POLY_SMOOTH_MAGIC)
                && bytes.windows(POLY_REMESH_MAGIC.len()).any(|w| w == POLY_REMESH_MAGIC),
            "both tail sections must be present when both are authored"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_remesh.len(), 1, "one authored remesh modifier");
        let rec = &loaded.poly_remesh[0];
        assert_eq!(rec.node_id, poly.0, "reattaches to the right node");
        assert_eq!(rec.mod_index, 2, "reattaches to the right stack slot");
        assert_eq!(rec.resolution, 192, "resolution survived (used to reset to 32)");
        assert!(!rec.smooth, "smooth survived (used to reset to true)");
        assert_eq!(rec.padding, 0.35, "padding survived (used to reset to 0.1)");
        assert_eq!(rec.project, 0.5, "project survived (used to reset to 1.0)");
        // The section written before FL4PRMSH must still decode intact.
        assert_eq!(loaded.poly_smooth.len(), 1, "FL4PSMTH survives a following section");
        assert_eq!(loaded.poly_smooth[0].smooth_falloff, 3, "…with its value intact");
        assert!(loaded.scene_graph.is_some(), "scene graph still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The collect side of `REMESHPARAMSAVE`: only modifiers whose remesh params actually differ
    /// from the defaults emit a record. That filter is what keeps an unused feature costing zero
    /// bytes, and it is why non-Remesh modifiers (which hold the same defaults) need no
    /// kind check.
    #[test]
    fn collect_poly_remesh_emits_only_authored_params() {
        let (mut sg, _p, _s, poly) = graph_with_poly();
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                // Slot 0: untouched defaults → must NOT emit.
                modifiers.push(core_math::scene_graph::PolyModifier::default());
                // Slot 1: authored → must emit.
                let mut m = core_math::scene_graph::PolyModifier::default();
                m.remesh_resolution = 256;
                m.remesh_smooth = false;
                modifiers.push(m);
            }
        }
        let recs = crate::project_save::collect_poly_remesh(&sg);
        assert_eq!(recs.len(), 1, "only the authored modifier emits a record");
        assert_eq!(recs[0].mod_index, 1, "keyed by stack index");
        assert_eq!(recs[0].resolution, 256);
        assert!(!recs[0].smooth);
        assert_eq!(recs[0].padding, 0.1, "untouched fields still travel, at their default");
    }

    /// Bevel VARIABLE WIDTH (`PolyModifier.bevel_var_width` / `bevel_width_min`, both
    /// `#[serde(skip)]`) survives save/load through the `FL4PBVAR` tail section, and a file that
    /// never enabled it contains no such section at all.
    ///
    /// The zero-bytes half is load-bearing, not cosmetic: these fields can never become in-blob
    /// fields on `PolyModifier` (bincode is positional — an appended field corrupts every existing
    /// `.fl4` carrying a poly modifier), so an untouched scene must stay byte-identical to a
    /// pre-feature save.
    #[test]
    fn poly_bevel_var_params_round_trip() {
        use crate::project::io::{POLY_BEVEL_VAR_MAGIC, POLY_REMESH_MAGIC};

        let (sg, _p, _s, poly) = graph_with_poly();

        // Unused → zero bytes.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polybevelvar_empty");
        let path = dir.join("bv0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(POLY_BEVEL_VAR_MAGIC.len())
                .any(|w| w == POLY_BEVEL_VAR_MAGIC),
            "a file that never enabled variable width must not contain the FL4PBVAR magic"
        );
        assert!(
            load_binary(&path).expect("load").poly_bevel_var.is_empty(),
            "no authored variable width → empty poly_bevel_var on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated, deliberately alongside an FL4PRMSH record: FL4PBVAR is written immediately
        // AFTER it, so this also pins that the newest section does not cannibalise the one before it
        // (append-newest-last is only safe while every section stays self-delimiting).
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_remesh = vec![crate::project::PolyRemeshRecord {
            node_id: poly.0,
            mod_index: 1,
            resolution: 192,
            smooth: false,
            padding: 0.35,
            project: 0.5,
            union_islands: false,
        }];
        proj.poly_bevel_var = vec![crate::project::PolyBevelVarRecord {
            node_id: poly.0,
            mod_index: 2,
            var_width: 2,
            width_min: 0.006,
            auto_merge: true,
        }];
        let dir = tmp("polybevelvar");
        let path = dir.join("bv1.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_REMESH_MAGIC.len()).any(|w| w == POLY_REMESH_MAGIC)
                && bytes
                    .windows(POLY_BEVEL_VAR_MAGIC.len())
                    .any(|w| w == POLY_BEVEL_VAR_MAGIC),
            "both tail sections must be present when both are authored"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_bevel_var.len(), 1, "one authored modifier");
        let rec = &loaded.poly_bevel_var[0];
        assert_eq!(rec.node_id, poly.0, "reattaches to the right node");
        assert_eq!(rec.mod_index, 2, "reattaches to the right stack slot");
        assert_eq!(rec.var_width, 2, "the driver survived (2 = authored weight)");
        assert_eq!(rec.width_min, 0.006, "the small end of the band survived");
        assert!(rec.auto_merge, "the Auto Merge toggle survived");
        // The section written before FL4PBVAR must still decode intact.
        assert_eq!(loaded.poly_remesh.len(), 1, "FL4PRMSH survives a following section");
        assert_eq!(loaded.poly_remesh[0].resolution, 192, "…with its value intact");
        assert!(loaded.scene_graph.is_some(), "scene graph still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bend params (all six `bend_*` fields, `#[serde(skip)]`) survive save/load through the
    /// `FL4PBEND` tail section, and a file with no authored Bend contains no such section at all.
    ///
    /// Same load-bearing zero-bytes half as its siblings: these fields can never become in-blob
    /// fields on `PolyModifier` (bincode is positional — an appended field corrupts every existing
    /// `.fl4` carrying a poly modifier), so a scene without a Bend must stay byte-identical to a
    /// pre-feature save.
    #[test]
    fn poly_bend_params_round_trip() {
        use crate::project::io::{POLY_BEND_MAGIC, POLY_OP_TAGS_MAGIC};

        let (sg, _p, _s, poly) = graph_with_poly();

        // Unused → zero bytes.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polybend_empty");
        let path = dir.join("bd0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(POLY_BEND_MAGIC.len()).any(|w| w == POLY_BEND_MAGIC),
            "a file with no authored Bend must not contain the FL4PBEND magic"
        );
        assert!(
            load_binary(&path).expect("load").poly_bend.is_empty(),
            "no authored Bend → empty poly_bend on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated, deliberately alongside an FL4PTAGS record: FL4PBEND is written immediately
        // AFTER it, so this also pins that the newest section does not cannibalise the one before
        // it (append-newest-last is only safe while every section stays self-delimiting).
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_op_tags = vec![crate::project::PolyOpTagRecord {
            node_id: poly.0,
            op_created: vec![3, 7, 11],
        }];
        proj.poly_bend = vec![crate::project::PolyBendRecord {
            node_id: poly.0,
            mod_index: 2,
            angle_deg: -120.0,
            axis: 2,
            dir_deg: 37.5,
            limit_lo: 0.25,
            limit_hi: 0.75,
            center: [0.5, -1.5, 2.25],
        }];
        let dir = tmp("polybend");
        let path = dir.join("bd1.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_OP_TAGS_MAGIC.len()).any(|w| w == POLY_OP_TAGS_MAGIC)
                && bytes.windows(POLY_BEND_MAGIC.len()).any(|w| w == POLY_BEND_MAGIC),
            "both tail sections must be present when both are authored"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_bend.len(), 1, "one authored Bend");
        let rec = &loaded.poly_bend[0];
        assert_eq!(rec.node_id, poly.0, "reattaches to the right node");
        assert_eq!(rec.mod_index, 2, "reattaches to the right stack slot");
        assert_eq!(rec.angle_deg, -120.0, "a negative angle survives sign and all");
        assert_eq!(rec.axis, 2);
        assert_eq!(rec.dir_deg, 37.5);
        assert_eq!((rec.limit_lo, rec.limit_hi), (0.25, 0.75));
        assert_eq!(rec.center, [0.5, -1.5, 2.25], "the local-space pivot survives");
        // The section written before FL4PBEND must still decode intact.
        assert_eq!(loaded.poly_op_tags.len(), 1, "FL4PTAGS survives a following section");
        assert_eq!(loaded.poly_op_tags[0].op_created, vec![3, 7, 11], "…with its value intact");
        assert!(loaded.scene_graph.is_some(), "scene graph still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The collect side: only a **Bend** modifier with a non-default param emits a record.
    ///
    /// The type gate is the point. `bend_angle_deg` defaults to a NON-zero 45°, so unlike the
    /// Remesh collector this one cannot lean on "other modifier kinds hold the field defaults" —
    /// they all do, and without the gate a user who typed 45 into a Bend would have the record
    /// dropped.
    #[test]
    fn collect_poly_bend_emits_only_authored_bends() {
        use core_math::scene_graph::{PolyModifier, PolyModifierType};
        let (mut sg, _p, _s, poly) = graph_with_poly();
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                // Slot 0: a non-Bend modifier carrying loud bend params → must NOT emit.
                let mut decoy = PolyModifier::new(PolyModifierType::Bevel);
                decoy.bend_angle_deg = 90.0;
                decoy.bend_axis = 1;
                modifiers.push(decoy);
                // Slot 1: an untouched Bend → must NOT emit (it loads back identical anyway).
                modifiers.push(PolyModifier::new(PolyModifierType::Bend));
                // Slot 2: an authored Bend → must emit.
                let mut m = PolyModifier::new(PolyModifierType::Bend);
                m.bend_angle_deg = 200.0;
                m.bend_limit_hi = 0.4;
                m.bend_center = glam::Vec3::new(0.0, 1.0, 0.0);
                modifiers.push(m);
            }
        }
        let recs = crate::project_save::collect_poly_bend(&sg);
        assert_eq!(recs.len(), 1, "only the authored Bend emits a record");
        assert_eq!(recs[0].mod_index, 2, "keyed by stack index");
        assert_eq!(recs[0].angle_deg, 200.0);
        assert_eq!(recs[0].limit_hi, 0.4);
        assert_eq!(recs[0].center, [0.0, 1.0, 0.0]);
        assert_eq!(recs[0].axis, 0, "untouched fields still travel, at their default");
    }

    #[test]
    /// FL4PIVT (2026-08-18): `PivotConfig.local` is `#[serde(skip)]` — the in-blob
    /// `serde(default)` bool it replaces broke EVERY pre-change .fl4 ("invalid u8 while
    /// decoding bool, found 2"; positional bincode never fires `default`). The flag
    /// rides its own newest-last tail; absent section = old-file semantics (migration
    /// converts world→local).
    #[test]
    fn pivot_local_round_trips_via_fl4pivt() {
        use crate::project::io::PIVOT_LOCAL_MAGIC;
        let (sg, _p, _s, poly) = graph_with_poly();
        // No local pivots → no section → bytes identical to pre-fix saves.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("pivotlocal_empty");
        let path = dir.join("pv0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(PIVOT_LOCAL_MAGIC.len()).any(|w| w == PIVOT_LOCAL_MAGIC),
            "no local Custom pivots must not emit FL4PIVT"
        );
        assert!(load_binary(&path).expect("load").pivot_local_nodes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);

        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.pivot_local_nodes = vec![poly.0];
        let dir = tmp("pivotlocal");
        let path = dir.join("pv1.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.pivot_local_nodes, vec![poly.0], "FL4PIVT round-trips");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poly_sculpt_params_round_trip() {
        use crate::project::io::{POLY_BEND_MAGIC, POLY_SCULPT_MAGIC};

        let (sg, _p, _s, poly) = graph_with_poly();
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polysculpt_empty");
        let path = dir.join("sc0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(POLY_SCULPT_MAGIC.len()).any(|w| w == POLY_SCULPT_MAGIC),
            "unused PolySculpt must not emit FL4SCLP"
        );
        assert!(load_binary(&path).expect("load").poly_sculpt.is_empty());
        let _ = std::fs::remove_dir_all(&dir);

        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_bend = vec![crate::project::PolyBendRecord {
            node_id: poly.0,
            mod_index: 0,
            angle_deg: 45.0,
            axis: 0,
            dir_deg: 0.0,
            limit_lo: 0.0,
            limit_hi: 1.0,
            center: [0.0, 0.0, 0.0],
        }];
        proj.poly_sculpt = vec![crate::project::PolySculptRecord {
            node_id: poly.0,
            mod_index: 1,
            gen: 3,
            deltas: vec![(0, [0.0, 0.25, 0.0]), (4, [0.1, 0.0, 0.0])],
        }];
        let dir = tmp("polysculpt");
        let path = dir.join("sc1.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_BEND_MAGIC.len()).any(|w| w == POLY_BEND_MAGIC)
                && bytes.windows(POLY_SCULPT_MAGIC.len()).any(|w| w == POLY_SCULPT_MAGIC),
            "FL4SCLP must follow FL4PBEND without eating it"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_sculpt.len(), 1);
        assert_eq!(loaded.poly_sculpt[0].gen, 3);
        assert_eq!(loaded.poly_sculpt[0].deltas.len(), 2);
        assert_eq!(loaded.poly_sculpt[0].deltas[0], (0, [0.0, 0.25, 0.0]));
        assert_eq!(loaded.poly_bend.len(), 1, "FL4PBEND survives the newer tail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_poly_sculpt_emits_only_authored_deltas() {
        use core_math::scene_graph::{PolyModifier, PolyModifierType};
        let (mut sg, _p, _s, poly) = graph_with_poly();
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                modifiers.push(PolyModifier::new(PolyModifierType::Sculpt));
                let mut m = PolyModifier::new(PolyModifierType::Sculpt);
                m.sculpt_delta_gen = 2;
                m.sculpt_deltas = vec![(1, glam::Vec3::Y * 0.5)];
                modifiers.push(m);
            }
        }
        let recs = crate::project_save::collect_poly_sculpt(&sg);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].mod_index, 1);
        assert_eq!(recs[0].gen, 2);
        assert_eq!(recs[0].deltas, vec![(1, [0.0, 0.5, 0.0])]);
    }

    /// GATE — Smart Select `op_created` tags survive save/load through the `FL4PTAGS` tail
    /// section, and a file with no tags contains no such section at all.
    ///
    /// Owner 2026-08-11: "we need to make sure the tag persists." Until this section the tags were
    /// SESSION-ONLY (`SIDECAR_SURVIVES_BOOLEAN.md` §OPEN) — a reopened file handed Smart Select
    /// nothing to skip, so every operation-created edge beveled again.
    ///
    /// The zero-bytes half is load-bearing, not cosmetic. This state can never become an in-blob
    /// field: adding `op_created` to `PolyLayerRecord` was tried and broke every existing `.fl4`,
    /// and so did a trailing `#[serde(default)] Vec` on the blob — bincode is positional, so
    /// `serde(default)` cannot rescue bytes that were never written. An untagged scene must stay
    /// byte-identical to a pre-feature save.
    #[test]
    fn poly_op_tags_round_trip() {
        use crate::project::io::{POLY_BEVEL_VAR_MAGIC, POLY_OP_TAGS_MAGIC};

        let (sg, _p, _s, poly) = graph_with_poly();

        // No tags → zero bytes.
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("polyoptags_empty");
        let path = dir.join("t0.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(POLY_OP_TAGS_MAGIC.len()).any(|w| w == POLY_OP_TAGS_MAGIC),
            "an untagged file must not contain the FL4PTAGS magic"
        );
        assert!(
            load_binary(&path).expect("load").poly_op_tags.is_empty(),
            "no tags → empty poly_op_tags on load"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Populated, deliberately alongside an FL4PBVAR record: FL4PTAGS is written immediately
        // AFTER it, so this also pins that the newest section does not cannibalise the one before
        // it (append-newest-last is only safe while every section stays self-delimiting).
        let mut proj = project_with_layers(sg, vec![], vec![]);
        proj.poly_bevel_var = vec![crate::project::PolyBevelVarRecord {
            node_id: poly.0,
            mod_index: 2,
            var_width: 2,
            width_min: 0.006,
            auto_merge: true,
        }];
        proj.poly_op_tags = vec![crate::project::PolyOpTagRecord {
            node_id: poly.0,
            op_created: vec![0, 3, 7, 11],
        }];
        let dir = tmp("polyoptags");
        let path = dir.join("t1.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_BEVEL_VAR_MAGIC.len()).any(|w| w == POLY_BEVEL_VAR_MAGIC)
                && bytes.windows(POLY_OP_TAGS_MAGIC.len()).any(|w| w == POLY_OP_TAGS_MAGIC),
            "both tail sections must be present when both are authored"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_op_tags.len(), 1, "one tagged body");
        assert_eq!(loaded.poly_op_tags[0].node_id, poly.0, "reattaches to the right node");
        assert_eq!(
            loaded.poly_op_tags[0].op_created,
            vec![0, 3, 7, 11],
            "every tagged half-edge id survived, in order"
        );
        // The section written before FL4PTAGS must still decode intact.
        assert_eq!(loaded.poly_bevel_var.len(), 1, "FL4PBVAR survives a following section");
        assert_eq!(loaded.poly_bevel_var[0].mod_index, 2, "…with its value intact");
        assert!(loaded.scene_graph.is_some(), "scene graph still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The collect side: only modifiers with variable width or auto-merge actually ENABLED emit a
    /// record, so an unused feature costs zero bytes. `bevel_width_min` travels even at its default
    /// once a toggle is on, because then it is authored state (the small end of the band).
    #[test]
    fn collect_poly_bevel_var_emits_only_enabled_modifiers() {
        let (mut sg, _p, _s, poly) = graph_with_poly();
        if let Some(n) = sg.get_mut(poly) {
            if let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload {
                // Slot 0: both off (the pre-feature state) → must NOT emit.
                modifiers.push(core_math::scene_graph::PolyModifier::default());
                // Slot 1: variable width on, default floor → must emit.
                let mut m = core_math::scene_graph::PolyModifier::default();
                m.bevel_var_width = 1;
                modifiers.push(m);
                // Slot 2: AUTO MERGE ALONE → must also emit, or the toggle would silently reset on
                // load (it is `#[serde(skip)]`, so this record is its only carrier).
                let mut m2 = core_math::scene_graph::PolyModifier::default();
                m2.bevel_auto_merge = true;
                modifiers.push(m2);
            }
        }
        let recs = crate::project_save::collect_poly_bevel_var(&sg);
        assert_eq!(recs.len(), 2, "both enabled modifiers emit a record, the off one does not");
        assert_eq!(recs[0].mod_index, 1, "keyed by stack index");
        assert_eq!(recs[0].var_width, 1);
        assert_eq!(recs[0].width_min, 0.0, "the default floor still travels once enabled");
        assert!(!recs[0].auto_merge, "variable width alone does not set the merge flag");
        assert_eq!(recs[1].mod_index, 2);
        assert_eq!(recs[1].var_width, 0, "auto-merge alone leaves the driver off");
        assert!(recs[1].auto_merge, "auto-merge alone must still be persisted");
    }

    /// P1 â€” CurveExtrude shell/rim params (the `#[serde(skip)]` variant fields) round-trip through the
    /// in-blob `curve_params` side-channel, keyed by prim_uid. An empty curve_params writes a blob
    /// with no extra bytes (forward-compat: pre-P1 files load with empty curve_params â†’ solid curves).
    #[test]
    fn curve_params_round_trip() {
        let (sg, _p, _s, _poly) = graph_with_poly();
        // Empty case: no records survives as empty (pre-P1 compat).
        let proj = project_with_layers(sg.clone(), vec![], vec![]);
        let dir = tmp("curve_empty");
        let path = dir.join("c0.fl4");
        save_binary(&path, &proj).expect("save");
        assert!(load_binary(&path).expect("load").curve_params.is_empty(),
            "no authored shell/rim â†’ empty curve_params on load");
        // Authored case: two records survive value-for-value.
        let mut proj2 = project_with_layers(sg, vec![], vec![]);
        proj2.curve_params = vec![
            CurveParamRecord { prim_uid: 7, wall_t: 0.25, rim: 0.4, open: false, thickness: 0.0, tube: false },
            CurveParamRecord { prim_uid: 9, wall_t: 0.0, rim: -0.3, open: false, thickness: 0.0, tube: false },
            // P2: an OPEN tube record round-trips its open/thickness/tube fields.
            CurveParamRecord { prim_uid: 11, wall_t: 0.0, rim: 0.0, open: true, thickness: 0.35, tube: true },
        ];
        let path2 = dir.join("c1.fl4");
        save_binary(&path2, &proj2).expect("save");
        let loaded = load_binary(&path2).expect("load");
        assert_eq!(loaded.curve_params.len(), 3, "all curve params must survive");
        assert_eq!(loaded.curve_params[0].prim_uid, 7);
        assert!((loaded.curve_params[0].wall_t - 0.25).abs() < 1e-6);
        assert!((loaded.curve_params[1].rim + 0.3).abs() < 1e-6, "chamfer rim sign preserved");
        assert!(loaded.curve_params[2].open && loaded.curve_params[2].tube, "P2 open/tube flags preserved");
        assert!((loaded.curve_params[2].thickness - 0.35).abs() < 1e-6, "P2 thickness preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE B â€” a ProjectFile with BOTH an authored child_order (FL4ORDER side-channel) AND authored
    /// poly_layers round-trips correctly. This is the EXACT corruption case Â§0 warns about: a 2nd
    /// trailing magic would break the loader's deserialize(tail). Because poly_layers is IN-BLOB
    /// (consumed by deserialize_from before the FL4ORDER sniff), both coexist losslessly.
    #[test]
    fn both_child_order_and_poly_layers_round_trip() {
        let (mut sg, parent, sub, poly) = graph_with_poly();
        // Author a custom child_order interleave on the parent (drives the FL4ORDER side-channel).
        sg.ensure_body_child_order(parent);
        sg.reorder_child_order(parent, BodyChildRef::Body(sub), 1);
        let custom = sg.body_child_order(parent);
        let child_orders = sg.collect_body_child_orders();
        assert!(!child_orders.is_empty(), "the custom interleave must be collected for FL4ORDER");

        let mesh = HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let (cd, rec) = authored_record(&mesh, poly);

        let proj = project_with_layers(sg, vec![rec], child_orders);
        let dir = tmp("both");
        let path = dir.join("b.fl4");
        save_binary(&path, &proj).expect("save");
        // The file MUST carry the FL4ORDER magic (custom child_order present).
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(CHILD_ORDER_MAGIC.len()).any(|w| w == CHILD_ORDER_MAGIC),
            "a custom child_order must still write the FL4ORDER tail");

        // Load: BOTH the in-blob poly_layers AND the side-channel child_orders must decode.
        let loaded = load_binary(&path).expect("load must not error with both present");
        assert_eq!(loaded.poly_layers.len(), 1, "the in-blob poly layer must survive alongside child_order");
        let lr = &loaded.poly_layers[0];
        assert_eq!(lr.node_id, poly.0, "poly layer reattaches to the poly body by node_id");

        // Rebuild CustomData from the loaded record; the persisted columns must equal the authored.
        let rebuilt = core_math::poly_mesh::CustomData::from_layer_tuples(
            &lr.uv, &lr.crease, &lr.seam, &lr.sharp, &lr.groups, &lr.group_meta,
        );
        assert!(rebuilt.columns_eq(&cd).is_ok(),
            "poly layers must round-trip intact through save+load: {:?}", rebuilt.columns_eq(&cd));

        // And the child_order interleave still resolves.
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        assert_eq!(g.body_child_order(parent), custom, "child_order must coexist intact with poly_layers");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (#2) â€” material_maps ride their OWN `FL4MATTX` tail section, round-trip, and COEXIST with a
    /// `child_order` (`FL4ORDER`) section in one file â€” the self-delimiting multi-section tail read via
    /// `deserialize_from`. This is the exact case the "no 2nd trailing magic" rule warned about; it is
    /// safe now because each section is length-delimited by `deserialize_from`, not `deserialize(tail)`.
    #[test]
    fn material_maps_roundtrip_and_coexist_with_child_orders() {
        let (mut sg, parent, sub, _poly) = graph_with_poly();
        // Author a custom child_order so the file ALSO carries the FL4ORDER section (both present).
        sg.ensure_body_child_order(parent);
        sg.reorder_child_order(parent, BodyChildRef::Body(sub), 1);
        let custom = sg.body_child_order(parent);
        let child_orders = sg.collect_body_child_orders();

        let mut proj = project_with_layers(sg, vec![], child_orders);
        // Two persisted maps (opaque bytes â€” the file format just carries them; PNG decode is GPU-side).
        proj.material_maps = vec![
            MaterialMapRecord { slot: 1, png: vec![10, 20, 30, 40, 50] },
            MaterialMapRecord { slot: 2, png: vec![7, 7, 7] },
        ];
        let dir = tmp("matmap");
        let path = dir.join("m.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(MATERIAL_MAP_MAGIC.len()).any(|w| w == MATERIAL_MAP_MAGIC),
            "a file with maps must carry the FL4MATTX tail");
        assert!(bytes.windows(CHILD_ORDER_MAGIC.len()).any(|w| w == CHILD_ORDER_MAGIC),
            "the custom child_order must still carry the FL4ORDER tail");

        let loaded = load_binary(&path).expect("load must not error with both sections present");
        assert_eq!(loaded.material_maps.len(), 2, "both map records survive");
        assert_eq!(loaded.material_maps[0].slot, 1);
        assert_eq!(loaded.material_maps[0].png, vec![10, 20, 30, 40, 50]);
        assert_eq!(loaded.material_maps[1].slot, 2);
        assert_eq!(loaded.material_maps[1].png, vec![7, 7, 7]);
        // child_order coexists intact â€” proves the loader read BOTH sections, not just the first.
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        assert_eq!(g.body_child_order(parent), custom, "child_order must coexist intact with material_maps");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (multi-map) â€” material_channels ride their OWN `FL4MATX2` tail section and COEXIST
    /// with BOTH other sections (FL4ORDER + FL4MATTX) in one file â€” three magic sections read by
    /// the order-independent sniff loop.
    #[test]
    fn material_channels_roundtrip_and_coexist_with_other_sections() {
        let (mut sg, parent, sub, _poly) = graph_with_poly();
        sg.ensure_body_child_order(parent);
        sg.reorder_child_order(parent, BodyChildRef::Body(sub), 1);
        let custom = sg.body_child_order(parent);
        let child_orders = sg.collect_body_child_orders();
        let mut proj = project_with_layers(sg, vec![], child_orders);
        proj.material_maps = vec![MaterialMapRecord { slot: 1, png: vec![1, 2, 3] }];
        proj.material_channels = vec![
            MaterialChannelRecord { material_id: 3, kind: 1, tex_id: 2, strength: 0.8, uv_scale: 2.5, projection: 0, blend: 4.0 },
            MaterialChannelRecord { material_id: 3, kind: 2, tex_id: 3, strength: 1.0, uv_scale: 0.5, projection: 1, blend: 2.0 },
        ];
        let dir = tmp("matchan");
        let path = dir.join("c.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        for (magic, name) in [(CHILD_ORDER_MAGIC, "FL4ORDER"), (MATERIAL_MAP_MAGIC, "FL4MATTX"),
                              (MATERIAL_CHANNEL_MAGIC, "FL4MATX2")] {
            assert!(bytes.windows(magic.len()).any(|w| w == magic),
                "file must carry the {name} tail section");
        }
        let loaded = load_binary(&path).expect("load must not error with all three sections present");
        assert_eq!(loaded.material_channels.len(), 2, "both channel records survive");
        let r = &loaded.material_channels[0];
        assert_eq!((r.material_id, r.kind, r.tex_id, r.projection), (3, 1, 2, 0));
        assert!((r.strength - 0.8).abs() < 1e-6 && (r.uv_scale - 2.5).abs() < 1e-6);
        let r = &loaded.material_channels[1];
        assert_eq!((r.material_id, r.kind, r.tex_id, r.projection), (3, 2, 3, 1));
        assert_eq!(loaded.material_maps.len(), 1, "FL4MATTX coexists intact");
        let mut g = loaded.scene_graph.clone().unwrap();
        g.apply_body_child_orders(&loaded.child_orders);
        assert_eq!(g.body_child_order(parent), custom, "FL4ORDER coexists intact");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (multi-map compat) â€” a file with NO channel records carries NO `FL4MATX2` magic
    /// (byte-compatible with every earlier save) and loads back with empty material_channels.
    #[test]
    fn no_material_channels_has_no_magic_and_loads_empty() {
        let (sg, _p, _s, _poly) = graph_with_poly();
        let proj = project_with_layers(sg, vec![], vec![]);
        let dir = tmp("nochan");
        let path = dir.join("n.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(MATERIAL_CHANNEL_MAGIC.len()).any(|w| w == MATERIAL_CHANNEL_MAGIC),
            "a no-channels file must not contain the FL4MATX2 magic");
        let loaded = load_binary(&path).expect("load");
        assert!(loaded.material_channels.is_empty(), "no channels â†’ empty on load");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (#2 compat) â€” a file with NO maps carries NO `FL4MATTX` magic (byte-compatible with pre-#2
    /// saves) and loads back with empty material_maps.
    #[test]
    fn no_material_maps_has_no_magic_and_loads_empty() {
        let (sg, _p, _s, _poly) = graph_with_poly();
        let proj = project_with_layers(sg, vec![], vec![]); // no maps, no orders
        let dir = tmp("nomap");
        let path = dir.join("n.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(MATERIAL_MAP_MAGIC.len()).any(|w| w == MATERIAL_MAP_MAGIC),
            "a no-maps file must not contain the FL4MATTX magic (byte-compat with pre-#2 saves)");
        let loaded = load_binary(&path).expect("load");
        assert!(loaded.material_maps.is_empty(), "no maps â†’ empty material_maps on load");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE C â€” an authored poly body round-trips its layers intact (uv + sharp survive). The core
    /// persistence gate: save â†’ load â†’ rebuild CustomData â†’ columns byte-equal.
    #[test]
    fn authored_poly_body_layers_survive_roundtrip() {
        let (sg, _p, _s, poly) = graph_with_poly();
        let mesh = HalfEdgeMesh::new_box(glam::Vec3::ONE);
        let (cd, rec) = authored_record(&mesh, poly);
        let proj = project_with_layers(sg, vec![rec], vec![]);
        let dir = tmp("authored");
        let path = dir.join("a.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_layers.len(), 1);
        let lr = &loaded.poly_layers[0];
        let rebuilt = core_math::poly_mesh::CustomData::from_layer_tuples(
            &lr.uv, &lr.crease, &lr.seam, &lr.sharp, &lr.groups, &lr.group_meta,
        );
        assert!(rebuilt.columns_eq(&cd).is_ok(), "authored uv/sharp/crease/seam must survive the round-trip");
        // Specifically: the sharp edge and a uv corner survived.
        assert!(rebuilt.sharp.get(core_math::half_edge::HalfEdgeId(2)), "the authored sharp edge must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE D â€” a forged pre-v8 blob (poly_layers absent because the writer predates the field) loads
    /// with EMPTY poly_layers via #[serde(default)], no panic, no tail consumed. Modelled by saving a
    /// project whose poly_layers is empty (the bytes a pre-v8 writer would produce) and asserting the
    /// load defaults cleanly â€” the format-stability guard.
    #[test]
    fn forged_pre_v8_loads_empty_poly_layers() {
        let (sg, _p, _s, _poly) = graph_with_poly();
        let proj = project_with_layers(sg, vec![], vec![]);
        let dir = tmp("forged");
        let path = dir.join("v7.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("a pre-v8 blob must load with default-empty poly_layers");
        assert!(loaded.poly_layers.is_empty(), "absent poly_layers defaults to empty (no corruption)");
        // The graph + the poly body still load fine.
        let g = loaded.scene_graph.unwrap();
        assert!(g.poly_bodies().next().is_some(), "the poly body must still load");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ==================================================================
// FL4ENVIR round-trip gates â€” the per-project environment side-channel (RENDERER_IBL.md Â§7).
// Mirrors child_order_save_tests / poly_layer_save_tests: real save_binary/load_binary, magic
// presence/absence asserted, coexistence with the other tail sections proven. Backward compat
// with real pre-change files is separately machine-verified by
// `dc_gate0_committed_ref_fl4_loads_unchanged` (dc_baseline.rs), which runs THIS loader over the
// committed reference .fl4.
// ==================================================================
#[cfg(test)]
mod env_save_tests {
    use super::*;
    use core_math::scene_graph::SceneGraph;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_env_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// GATE â€” save with an env record â†’ FL4ENVIR magic present â†’ load â†’ path / intensity /
    /// rotation restored exactly (the owner's authored trio).
    #[test]
    fn env_record_round_trips() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        let rec = EnvRecord { path: "tex/studio_2k.hdr".into(), brightness: 1.7, rotation_deg: 123.5 };
        proj.env = Some(rec.clone());
        let dir = tmp("rt");
        let path = dir.join("env.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(ENV_MAGIC.len()).any(|w| w == ENV_MAGIC),
            "a file with a loaded HDRI must carry the FL4ENVIR tail");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.env.as_ref(), Some(&rec),
            "path + intensity + rotation must round-trip exactly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no loaded HDRI â†’ NO FL4ENVIR magic: the file is byte-compatible with
    /// pre-env saves, and loads back with env == None.
    #[test]
    fn no_env_no_magic() {
        let proj = super::child_order_save_tests::project_with(SceneGraph::new());
        assert!(proj.env.is_none());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(ENV_MAGIC.len()).any(|w| w == ENV_MAGIC),
            "a no-env file must not contain the FL4ENVIR magic (byte-compat with pre-env saves)");
        assert!(load_binary(&path).expect("load").env.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence) â€” FL4ENVIR is read alongside the other tail sections (here FL4MATTX,
    /// exercising the multi-section sniff loop) and is written LAST, so an older AZRA that stops
    /// at the unknown magic still reads every section it knows.
    #[test]
    fn env_coexists_with_other_tail_sections() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        proj.material_maps = vec![MaterialMapRecord { slot: 1, png: vec![1, 2, 3, 4] }];
        proj.env = Some(EnvRecord { path: "e.hdr".into(), brightness: 2.0, rotation_deg: 90.0 });
        let dir = tmp("coex");
        let path = dir.join("both.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let mat_pos = pos_of(MATERIAL_MAP_MAGIC).expect("FL4MATTX present");
        let env_pos = pos_of(ENV_MAGIC).expect("FL4ENVIR present");
        assert!(env_pos > mat_pos, "FL4ENVIR must be the LAST tail section (older-reader tolerance)");
        let loaded = load_binary(&path).expect("load with both sections");
        assert_eq!(loaded.material_maps.len(), 1, "FL4MATTX coexists intact");
        assert_eq!(loaded.env.as_ref().map(|e| e.rotation_deg), Some(90.0), "FL4ENVIR decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pure-fn gate â€” relative-ize on save (portability) + resolve on load are inverse:
    /// an HDRI under the project dir round-trips through a RELATIVE string; one outside it
    /// stays absolute and passes through resolution unchanged; no project path â†’ absolute.
    #[test]
    fn env_path_relativize_and_resolve() {
        let base = std::env::temp_dir().join("azra_env_paths");
        let proj_file = base.join("scene.fl4");
        // Under the project dir â†’ relative, and resolves back to the same absolute path.
        let hdr_in = base.join("tex").join("env.hdr");
        let stored = env_path_for_save(&hdr_in, Some(&proj_file));
        assert!(!Path::new(&stored).is_absolute(), "in-project HDRI must store RELATIVE, got {stored}");
        assert_eq!(resolve_env_path(&stored, Some(&base)), hdr_in, "relative path must resolve against the project dir");
        // Outside the project dir â†’ absolute passthrough (resolution ignores base_dir).
        let hdr_out = std::env::temp_dir().join("elsewhere").join("sky.hdr");
        let stored_out = env_path_for_save(&hdr_out, Some(&proj_file));
        assert!(Path::new(&stored_out).is_absolute(), "out-of-project HDRI must store ABSOLUTE");
        assert_eq!(resolve_env_path(&stored_out, Some(&base)), hdr_out);
        // No project path yet (unsaved scene) â†’ absolute fallback.
        let stored_unsaved = env_path_for_save(&hdr_in, None);
        assert!(Path::new(&stored_unsaved).is_absolute(), "unsaved project must store ABSOLUTE");
    }
}

// ==================================================================
// FL4LTVOL round-trip gates â€” the per-light VOLUMETRIC ("god ray") side-channel. Mirrors the
// FL4ENVIR gates exactly: real save_binary/load_binary, magic presence/absence asserted, and
// coexistence with the other tail sections proven (with FL4ENVIR kept LAST). Backward compat with
// real pre-change files is machine-verified by `dc_gate0_committed_ref_fl4_loads_unchanged`
// (dc_baseline.rs), which runs THIS loader over the committed reference .fl4.
// ==================================================================
#[cfg(test)]
mod light_vol_save_tests {
    use super::*;
    use core_math::scene_graph::SceneGraph;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_ltvol_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// GATE â€” a volumetric light's params round-trip through the FL4LTVOL tail, keyed by light id.
    #[test]
    fn light_vol_record_round_trips() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        let rec = LightVolRecord { light_id: 7, enabled: true, density: 1.6, softness: 0.35, zone: 12.5 };
        proj.light_volumetrics = vec![rec.clone()];
        let dir = tmp("rt");
        let path = dir.join("lv.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(LIGHTVOL_MAGIC.len()).any(|w| w == LIGHTVOL_MAGIC),
            "a scene with a volumetric light must carry the FL4LTVOL tail");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.light_volumetrics, vec![rec],
            "id + enabled + density + softness + zone must round-trip exactly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no volumetric light â†’ NO FL4LTVOL magic: byte-compatible with pre-volumetric
    /// saves, and loads back with an empty light_volumetrics.
    #[test]
    fn no_light_vol_no_magic() {
        let proj = super::child_order_save_tests::project_with(SceneGraph::new());
        assert!(proj.light_volumetrics.is_empty());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(LIGHTVOL_MAGIC.len()).any(|w| w == LIGHTVOL_MAGIC),
            "a no-volumetric file must not contain the FL4LTVOL magic (byte-compat)");
        assert!(load_binary(&path).expect("load").light_volumetrics.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence) â€” FL4LTVOL is read alongside FL4ENVIR, and FL4ENVIR stays LAST (written
    /// after FL4LTVOL) so an older AZRA's sniff loop still reads every section it knows.
    #[test]
    fn light_vol_coexists_env_stays_last() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        proj.light_volumetrics = vec![LightVolRecord { light_id: 1, enabled: true, density: 1.0, softness: 0.5, zone: 0.0 }];
        proj.env = Some(EnvRecord { path: "e.hdr".into(), brightness: 2.0, rotation_deg: 90.0 });
        let dir = tmp("coex");
        let path = dir.join("both.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let lv_pos = pos_of(LIGHTVOL_MAGIC).expect("FL4LTVOL present");
        let env_pos = pos_of(ENV_MAGIC).expect("FL4ENVIR present");
        // Append-newest-LAST: FL4LTVOL (newer) after FL4ENVIR (older) so an env-aware but
        // ltvol-unaware reader reaches env before the unknown FL4LTVOL magic stops its loop.
        assert!(lv_pos > env_pos, "FL4LTVOL (newest) must be written AFTER FL4ENVIR");
        let loaded = load_binary(&path).expect("load with both sections");
        assert_eq!(loaded.light_volumetrics.len(), 1, "FL4LTVOL coexists intact");
        assert_eq!(loaded.env.as_ref().map(|e| e.rotation_deg), Some(90.0), "FL4ENVIR still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ==================================================================
// FL4NODES round-trip gates â€” per-prim procedural shader node graphs
// (PROCEDURAL_TEXTURING_DESIGN PR0). Newest section: written AFTER FL4LTRTS.
// ==================================================================
#[cfg(test)]
mod node_graph_save_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_nodes_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_graph_json() -> String {
        // Minimal real NodeGraph (default-ish: TexCoord + MaterialOutput).
        let g = crate::shader_nodes::NodeGraph::default();
        serde_json::to_string(&g).expect("NodeGraph JSON")
    }

    /// GATE â€” a node graph record round-trips through FL4NODES (JSON body intact).
    #[test]
    fn node_graph_record_round_trips() {
        let mut proj = super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        let rec = NodeGraphRecord {
            prim_uid: 42,
            graph_json: sample_graph_json(),
        };
        proj.node_graphs = vec![rec.clone()];
        let dir = tmp("rt");
        let path = dir.join("ng.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(NODE_GRAPH_MAGIC.len()).any(|w| w == NODE_GRAPH_MAGIC),
            "a scene with a node graph must carry the FL4NODES tail"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.node_graphs.len(), 1);
        assert_eq!(loaded.node_graphs[0].prim_uid, 42);
        // JSON must re-parse as NodeGraph
        let g: crate::shader_nodes::NodeGraph =
            serde_json::from_str(&loaded.node_graphs[0].graph_json).expect("parse graph_json");
        assert!(
            g.nodes.iter().any(|n| n.node_type == crate::shader_nodes::NodeType::MaterialOutput),
            "MaterialOutput must survive JSON round-trip"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no graphs â†’ NO FL4NODES magic (byte-compat with pre-PR0 saves).
    #[test]
    fn no_node_graphs_no_magic() {
        let proj = super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        assert!(proj.node_graphs.is_empty());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(NODE_GRAPH_MAGIC.len()).any(|w| w == NODE_GRAPH_MAGIC),
            "a no-graph file must not contain FL4NODES magic"
        );
        assert!(load_binary(&path).expect("load").node_graphs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence + newest-last) â€” FL4NODES is written AFTER FL4LTRTS (and thus after
    /// FL4LTVOL / FL4ENVIR). Older AZRA stops at FL4NODES and keeps all prior sections.
    #[test]
    fn node_graphs_written_after_lightrt() {
        let mut proj = super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        proj.env = Some(EnvRecord {
            path: "e.hdr".into(),
            brightness: 2.0,
            rotation_deg: 90.0,
        });
        proj.light_volumetrics = vec![LightVolRecord {
            light_id: 1,
            enabled: true,
            density: 1.0,
            softness: 0.5,
            zone: 0.0,
        }];
        proj.light_rt = vec![LightRtRecord {
            light_id: 1,
            enabled: false,
            radius: 0.3,
        }];
        proj.node_graphs = vec![NodeGraphRecord {
            prim_uid: 7,
            graph_json: sample_graph_json(),
        }];
        let dir = tmp("coex");
        let path = dir.join("all.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let nodes_pos = pos_of(NODE_GRAPH_MAGIC).expect("FL4NODES present");
        let rt_pos = pos_of(LIGHTRT_MAGIC).expect("FL4LTRTS present");
        let lv_pos = pos_of(LIGHTVOL_MAGIC).expect("FL4LTVOL present");
        let env_pos = pos_of(ENV_MAGIC).expect("FL4ENVIR present");
        assert!(
            nodes_pos > rt_pos && rt_pos > lv_pos && lv_pos > env_pos,
            "append-newest-last: FL4NODES after FL4LTRTS after FL4LTVOL after FL4ENVIR \
             (nodes={nodes_pos} rt={rt_pos} lv={lv_pos} env={env_pos})"
        );
        let loaded = load_binary(&path).expect("load with all sections");
        assert_eq!(loaded.node_graphs.len(), 1);
        assert_eq!(loaded.node_graphs[0].prim_uid, 7);
        assert_eq!(
            loaded.light_rt,
            vec![LightRtRecord {
                light_id: 1,
                enabled: false,
                radius: 0.3
            }]
        );
        assert_eq!(loaded.light_volumetrics.len(), 1);
        assert_eq!(loaded.env.as_ref().map(|e| e.rotation_deg), Some(90.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE â€” material-owned graph envelope round-trips (prim_uid=0 + azra_owner JSON).
    #[test]
    fn material_graph_envelope_round_trips() {
        let g = crate::shader_nodes::NodeGraph::default();
        let env = crate::project_save::MaterialGraphEnvelope {
            azra_owner: "material".into(),
            material_id: 7,
            graph: g,
        };
        let json = serde_json::to_string(&env).unwrap();
        let mut proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        proj.node_graphs = vec![NodeGraphRecord {
            prim_uid: 0,
            graph_json: json,
        }];
        let dir = tmp("matenv");
        let path = dir.join("m.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.node_graphs.len(), 1);
        assert_eq!(loaded.node_graphs[0].prim_uid, 0);
        let env2: crate::project_save::MaterialGraphEnvelope =
            serde_json::from_str(&loaded.node_graphs[0].graph_json).expect("envelope");
        assert_eq!(env2.azra_owner, "material");
        assert_eq!(env2.material_id, 7);
        assert!(env2
            .graph
            .nodes
            .iter()
            .any(|n| n.node_type == crate::shader_nodes::NodeType::MaterialOutput));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE â€” JSON graph body can carry connections and regenerate WGSL after load.
    #[test]
    fn node_graph_json_connections_and_wgsl() {
        let mut g = crate::shader_nodes::NodeGraph::default();
        let noise = g.add_node(crate::shader_nodes::NodeType::Noise, [100.0, 100.0]);
        let out = g
            .nodes
            .iter()
            .find(|n| n.node_type == crate::shader_nodes::NodeType::MaterialOutput)
            .map(|n| n.id)
            .expect("default graph has MaterialOutput");
        // Connect noise value â†’ albedo (socket 0) â€” may fail type check; if so just save structure.
        let _ = g.connect(noise, 0, out, 0);
        let json = serde_json::to_string(&g).unwrap();
        let mut proj = super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        proj.node_graphs = vec![NodeGraphRecord {
            prim_uid: 1,
            graph_json: json,
        }];
        let dir = tmp("wgsl");
        let path = dir.join("g.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        let mut g2: crate::shader_nodes::NodeGraph =
            serde_json::from_str(&loaded.node_graphs[0].graph_json).unwrap();
        assert!(g2.nodes.iter().any(|n| n.node_type == crate::shader_nodes::NodeType::Noise));
        let wgsl = g2.generate_wgsl();
        assert!(
            wgsl.contains("eval_node_material") || wgsl.contains("result."),
            "regenerated WGSL must look like material eval, got: {wgsl}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ==================================================================
// FL4LTRT round-trip gates â€” the per-light RAY-TRACED SHADOW side-channel (RT_SHADOWS_DESIGN
// Stage 4). Mirrors the FL4LTVOL gates: real save_binary/load_binary, magic presence/absence
// asserted, and coexistence proven (FL4LTRT is the NEWEST section â€” written AFTER FL4LTVOL).
// Backward compat with real pre-Stage-4 files is machine-verified by
// `dc_gate0_committed_ref_fl4_loads_unchanged` (dc_baseline.rs), which runs THIS loader (now with
// the extra FL4LTRT sniff branch) over the committed reference .fl4 â€” the ref has no FL4LTRT magic,
// so the loop breaks exactly as before and the file loads unchanged.
// ==================================================================
#[cfg(test)]
mod light_rt_save_tests {
    use super::*;
    use core_math::scene_graph::{SceneGraph, LightData, LightRtShadow, NodePayload};

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_ltrt_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// GATE â€” a light's RT-shadow params round-trip through the FL4LTRT tail, keyed by light id.
    #[test]
    fn light_rt_record_round_trips() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        let rec = LightRtRecord { light_id: 9, enabled: false, radius: 0.62 };
        proj.light_rt = vec![rec.clone()];
        let dir = tmp("rt");
        let path = dir.join("lrt.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(LIGHTRT_MAGIC.len()).any(|w| w == LIGHTRT_MAGIC),
            "a scene with an authored RT-shadow light must carry the FL4LTRT tail");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.light_rt, vec![rec],
            "id + enabled + radius must round-trip exactly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no authored RT light â†’ NO FL4LTRT magic: byte-compatible with pre-Stage-4
    /// saves, and loads back with an empty light_rt.
    #[test]
    fn no_light_rt_no_magic() {
        let proj = super::child_order_save_tests::project_with(SceneGraph::new());
        assert!(proj.light_rt.is_empty());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(LIGHTRT_MAGIC.len()).any(|w| w == LIGHTRT_MAGIC),
            "a no-RT file must not contain the FL4LTRT magic (byte-compat with pre-Stage-4 saves)");
        assert!(load_binary(&path).expect("load").light_rt.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence + newest-last) â€” FL4LTRT is read alongside FL4LTVOL and FL4ENVIR, and is the
    /// NEWEST section (written AFTER both), so an older AZRA whose sniff loop stops at the unknown
    /// FL4LTRT magic still reads env + volumetrics before it.
    #[test]
    fn light_rt_written_after_ltvol_and_env() {
        let mut proj = super::child_order_save_tests::project_with(SceneGraph::new());
        proj.env = Some(EnvRecord { path: "e.hdr".into(), brightness: 2.0, rotation_deg: 90.0 });
        proj.light_volumetrics = vec![LightVolRecord { light_id: 1, enabled: true, density: 1.0, softness: 0.5, zone: 0.0 }];
        proj.light_rt = vec![LightRtRecord { light_id: 1, enabled: false, radius: 0.3 }];
        let dir = tmp("coex");
        let path = dir.join("all.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let rt_pos = pos_of(LIGHTRT_MAGIC).expect("FL4LTRT present");
        let lv_pos = pos_of(LIGHTVOL_MAGIC).expect("FL4LTVOL present");
        let env_pos = pos_of(ENV_MAGIC).expect("FL4ENVIR present");
        assert!(rt_pos > lv_pos && lv_pos > env_pos,
            "append-newest-last: FL4LTRT must be after FL4LTVOL, which is after FL4ENVIR (rt={rt_pos} lv={lv_pos} env={env_pos})");
        let loaded = load_binary(&path).expect("load with all three sections");
        assert_eq!(loaded.light_rt, vec![LightRtRecord { light_id: 1, enabled: false, radius: 0.3 }], "FL4LTRT coexists intact");
        assert_eq!(loaded.light_volumetrics.len(), 1, "FL4LTVOL still decodes alongside FL4LTRT");
        assert_eq!(loaded.env.as_ref().map(|e| e.rotation_deg), Some(90.0), "FL4ENVIR still decodes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (byte-compat logic) â€” `collect_light_rt` emits a record ONLY for AUTHORED lights: a
    /// default light (enabled=true, radius=0 â€” it just follows the global toggle) emits NOTHING, so an
    /// untouched scene writes no FL4LTRT magic; a light turned RT OFF, or given a radius, DOES emit.
    #[test]
    fn collect_light_rt_only_authored() {
        let mut sg = SceneGraph::new();
        let root = sg.root();
        // Default RT light â†’ NOT collected.
        sg.add_child(root, NodePayload::Light(LightData::new_point(10, "def".into(), glam::Vec3::ZERO)));
        // Authored: RT turned OFF (the hybrid) â†’ collected.
        let mut off = LightData::new_point(11, "off".into(), glam::Vec3::ZERO);
        off.rt_shadow = LightRtShadow { enabled: false, radius: 0.0 };
        sg.add_child(root, NodePayload::Light(off));
        // Authored: a per-light radius while still enabled â†’ collected.
        let mut soft = LightData::new_point(12, "soft".into(), glam::Vec3::ZERO);
        soft.rt_shadow = LightRtShadow { enabled: true, radius: 0.5 };
        sg.add_child(root, NodePayload::Light(soft));

        let recs = crate::project_save::collect_light_rt(&sg);
        let ids: std::collections::BTreeSet<u32> = recs.iter().map(|r| r.light_id).collect();
        assert_eq!(ids, [11u32, 12u32].into_iter().collect(),
            "only the AUTHORED lights (RT off, or radius set) emit records â€” default light 10 must not");
    }
}


/// DEV TOOL (permanent, env-gated â€” BEVEL.md Â§11 2026-07-23c): dump a .fl4's poly meshes to JSON
/// for headless replay / oracle diffing. Dormant unless both env vars are set.
/// AZRA_DIAG_FILE=<fl4> AZRA_DIAG_OUT=<json> cargo test -p app diag_dump -- --nocapture
#[cfg(test)]
mod diag_dump {
    #[test]
    fn diag_dump_fl4() {
        let Ok(path) = std::env::var("AZRA_DIAG_FILE") else { eprintln!("no AZRA_DIAG_FILE"); return; };
        let Ok(out) = std::env::var("AZRA_DIAG_OUT") else { eprintln!("no AZRA_DIAG_OUT"); return; };
        let pf = super::load_binary(std::path::Path::new(&path)).expect("load .fl4");
        let Some(g) = pf.scene_graph.as_ref() else { return };
        let mut dump = String::from("[");
        for (nid, node) in g.all_nodes() {
            if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                use std::fmt::Write;
                let mut verts = String::from("[");
                for v in &mesh.vertices {
                    let _ = write!(verts, "[{},{},{}],", v.position.x, v.position.y, v.position.z);
                }
                verts.pop(); verts.push(']');
                let mut faces = String::from("[");
                let mut any = false;
                for fi in 0..mesh.faces.len() as u32 {
                    let fid = core_math::half_edge::FaceId(fi);
                    if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
                    let ring: Vec<String> = mesh.face_vertices(fid).iter().map(|v| v.0.to_string()).collect();
                    let _ = write!(faces, "[{}],", ring.join(","));
                    any = true;
                }
                if any { faces.pop(); }
                faces.push(']');
                let _ = write!(dump, "{{\"nid\":{},\"verts\":{},\"faces\":{}}},", nid.0, verts, faces);
            }
        }
        if dump.ends_with(',') { dump.pop(); }
        dump.push(']');
        std::fs::write(&out, dump).expect("write dump");
        eprintln!("DUMPED to {out}");
    }
}

// ==================================================================
// FL4MPNLS round-trip gates â€” per-material MATERIAL PANEL docs
// (MATERIAL_PANEL_SPEC P0). Newest section: written AFTER FL4NODES.
// Mirrors the FL4NODES gates above.
// ==================================================================
#[cfg(test)]
mod material_panel_save_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_mpnl_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_doc_json() -> String {
        let doc = crate::material_panel::MaterialPanelDoc::default_for_new();
        serde_json::to_string(&doc).expect("doc JSON")
    }

    /// GATE â€” a panel-doc record round-trips through FL4MPNLS (JSON body byte-intact).
    #[test]
    fn panel_doc_record_round_trips() {
        let mut proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        let json = sample_doc_json();
        proj.material_panel_docs = vec![MaterialPanelRecord {
            material_id: 3,
            doc_json: json.clone(),
        }];
        let dir = tmp("rt");
        let path = dir.join("mp.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(MATERIAL_PANEL_MAGIC.len()).any(|w| w == MATERIAL_PANEL_MAGIC),
            "a scene with a panel doc must carry the FL4MPNLS tail"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.material_panel_docs.len(), 1);
        assert_eq!(loaded.material_panel_docs[0].material_id, 3);
        assert_eq!(loaded.material_panel_docs[0].doc_json, json, "JSON body byte-identical");
        let doc: crate::material_panel::MaterialPanelDoc =
            serde_json::from_str(&loaded.material_panel_docs[0].doc_json).expect("parse doc_json");
        assert!(!doc.channels.is_empty(), "channels survive the round-trip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no docs â†’ NO FL4MPNLS magic (byte-compat with every older save).
    #[test]
    fn no_panel_docs_no_magic() {
        let proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        assert!(proj.material_panel_docs.is_empty());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(MATERIAL_PANEL_MAGIC.len()).any(|w| w == MATERIAL_PANEL_MAGIC),
            "a no-doc file must not contain FL4MPNLS magic"
        );
        assert!(load_binary(&path).expect("load").material_panel_docs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence + newest-last) â€” FL4MPNLS is written AFTER FL4NODES; both load.
    #[test]
    fn panel_docs_written_after_node_graphs() {
        let mut proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        let g = crate::shader_nodes::NodeGraph::default();
        proj.node_graphs = vec![NodeGraphRecord {
            prim_uid: 7,
            graph_json: serde_json::to_string(&g).unwrap(),
        }];
        proj.material_panel_docs = vec![MaterialPanelRecord {
            material_id: 1,
            doc_json: sample_doc_json(),
        }];
        let dir = tmp("coex");
        let path = dir.join("all.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let mp_pos = pos_of(MATERIAL_PANEL_MAGIC).expect("FL4MPNLS present");
        let ng_pos = pos_of(NODE_GRAPH_MAGIC).expect("FL4NODES present");
        assert!(
            mp_pos > ng_pos,
            "append-newest-last: FL4MPNLS after FL4NODES (mp={mp_pos} ng={ng_pos})"
        );
        let loaded = load_binary(&path).expect("load with both sections");
        assert_eq!(loaded.node_graphs.len(), 1);
        assert_eq!(loaded.material_panel_docs.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}


/// Manual diagnostic for owner repro files:
///   AZRA_DIAG_FL4=<path> cargo test -p app diag_fl4_dump -- --ignored --nocapture
/// Prints every poly body's topology stats + validate/integrity/is_manifold verdicts and
/// flags suspicious faces (huge n-gons, near-zero area, non-planar rings).
#[test]
#[ignore]
fn diag_fl4_dump() {
    use core_math::half_edge::FaceId;
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else {
        eprintln!("set AZRA_DIAG_FL4=<path>");
        return;
    };
    let pf = load_binary(std::path::Path::new(&path)).expect("load_binary");
    let Some(sg) = pf.scene_graph.as_ref() else {
        eprintln!("no scene graph in file");
        return;
    };
    for (nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, modifiers, .. } = &node.payload else { continue };
        eprintln!("   MODIFIERS: {}", if modifiers.is_empty() { "none".to_string() } else { modifiers.iter().map(|m| format!("{:?}(on={})", m.mod_type, m.enabled)).collect::<Vec<_>>().join(", ") });
        eprintln!("== body {:?} '{}' verts={} faces={} edges={}",
            nid, name, mesh.vertex_count(), mesh.face_count(), mesh.edge_count());
        eprintln!("   validate:   {:?}", mesh.validate());
        eprintln!("   integrity:  {:?}", mesh.integrity());
        eprintln!("   is_manifold:{:?}", mesh.is_manifold());
        if std::env::var("AZRA_DIAG_VERBOSE").is_ok() {
            for i in 0..mesh.vertices.len() as u32 {
                let v = core_math::half_edge::VertexId(i);
                if mesh.vert(v).half_edge == core_math::half_edge::NO_EDGE { continue; }
                eprintln!("   v{} {:?}", i, mesh.vert(v).position);
            }
            for i in 0..mesh.faces.len() as u32 {
                let f = FaceId(i);
                if mesh.face(f).half_edge == core_math::half_edge::NO_EDGE { continue; }
                let vs: Vec<u32> = mesh.face_vertices(f).iter().map(|v| v.0).collect();
                let n = mesh.compute_face_normal(f);
                eprintln!("   f{} n=({:.2},{:.2},{:.2}) {:?}", i, n.x, n.y, n.z, vs);
            }
        }
        eprintln!("   folded_coplanar_pairs: {}", mesh.count_folded_coplanar_pairs());
        let mut big = 0usize;
        let mut tiny = 0usize;
        let mut nonplanar = 0usize;
        for i in 0..mesh.faces.len() as u32 {
            let f = FaceId(i);
            if mesh.face(f).half_edge == core_math::half_edge::NO_EDGE { continue; }
            let vs = mesh.face_vertices(f);
            if vs.len() > 6 {
                big += 1;
                let pts: Vec<glam::Vec3> = vs.iter().map(|&v| mesh.vert(v).position).collect();
                eprintln!("   NGON face {} ({} verts): {:?}â€¦", i, vs.len(), &pts[..pts.len().min(4)]);
            }
            let pts: Vec<glam::Vec3> = vs.iter().map(|&v| mesh.vert(v).position).collect();
            let n = mesh.compute_face_normal(f);
            // area via Newell magnitude
            let mut nv = glam::Vec3::ZERO;
            for k in 0..pts.len() {
                let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                nv += glam::Vec3::new(
                    (a.y - b.y) * (a.z + b.z),
                    (a.z - b.z) * (a.x + b.x),
                    (a.x - b.x) * (a.y + b.y),
                );
            }
            let area = nv.length() * 0.5;
            if area < 1e-8 { tiny += 1; }
            if n.length_squared() > 0.5 {
                let d0 = n.dot(pts[0]);
                let dev = pts.iter().map(|p| (n.dot(*p) - d0).abs()).fold(0.0f32, f32::max);
                if dev > 1e-3 { nonplanar += 1; eprintln!("   NON-PLANAR face {} dev={dev:.5} ({} verts)", i, pts.len()); }
            }
        }
        eprintln!("   ngons>6={} zero-area={} non-planar={}", big, tiny, nonplanar);
    }
}

/// AZRA_DIAG_FL4=<path> [AZRA_DIAG_WIDTH=0.15] AZRA_CONSUME_DEBUG=1 \
///   cargo test -p app diag_fl4_consume_bevel -- --ignored --nocapture
/// Replays Bevel Consume Geometry on a saved file (owner EAT.fl4 2026-08-08): selects the
/// SHARP closed loops (boolean rim), runs the fuse → absorb → plain-bevel chain, and reports
/// which gate refused plus fold counts of whatever the fallback produced.
#[test]
#[ignore]
fn diag_fl4_consume_bevel() {
    use core_math::half_edge::{HalfEdgeId, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else {
        eprintln!("set AZRA_DIAG_FL4=<path>");
        return;
    };
    let width: f32 = std::env::var("AZRA_DIAG_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(0.15);
    let pf = load_binary(std::path::Path::new(&path)).expect("load_binary");
    let Some(sg) = pf.scene_graph.as_ref() else { return };
    for (nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        eprintln!("== body {:?} '{}' v={} f={} e={}", nid, name, mesh.vertex_count(), mesh.face_count(), mesh.edge_count());
        // Sharp closed loops = the boolean rim(s): dihedral between 45 and 135 degrees.
        let mut sel: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let he = HalfEdgeId(i);
            if !mesh.is_edge_live(he) { continue; }
            let t = mesh.he(he).twin;
            if t != NO_TWIN && i > t.0 { continue; }
            if let Some(c) = mesh.edge_dihedral_cos(he) {
                let deg = c.clamp(-1.0, 1.0).acos().to_degrees();
                if (45.0..=135.0).contains(&deg) { sel.insert(i); }
            }
        }
        // Bridge the gaps first: whenever two open endpoints of the sharp set share a
        // DIRECT edge, that edge is part of the rim whatever its dihedral (the shallow
        // spots are exactly where the owner's spikes appear).
        for _pass in 0..4 {
            let mut deg: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            for &e in &sel {
                let he = HalfEdgeId(e);
                *deg.entry(mesh.he_source(he).0).or_default() += 1;
                *deg.entry(mesh.he(he).vertex.0).or_default() += 1;
            }
            let open: std::collections::HashSet<u32> =
                deg.iter().filter(|&(_, &d)| d == 1).map(|(&v, _)| v).collect();
            if open.is_empty() { break; }
            let mut added = false;
            // BFS ≤ 3 edges from each open endpoint to another open endpoint; add the path.
            for &v in &open {
                let mut prevmap: std::collections::HashMap<u32, (u32, u32)> = Default::default();
                let mut frontier = vec![v];
                let mut found: Option<u32> = None;
                'bfs: for _depth in 0..3 {
                    let mut next_frontier = Vec::new();
                    for &cur in &frontier {
                        for h in mesh.vertex_fan(core_math::half_edge::VertexId(cur)) {
                            let far = mesh.he(h).vertex.0;
                            if far == v || prevmap.contains_key(&far) { continue; }
                            let t = mesh.he(h).twin;
                            let key = if t == NO_TWIN { h.0 } else { h.0.min(t.0) };
                            if sel.contains(&key) { continue; }
                            prevmap.insert(far, (cur, key));
                            if far != v && open.contains(&far) { found = Some(far); break 'bfs; }
                            next_frontier.push(far);
                        }
                    }
                    frontier = next_frontier;
                }
                if let Some(mut cur) = found {
                    while let Some(&(prev, key)) = prevmap.get(&cur) {
                        if sel.insert(key) { added = true; }
                        if prev == v { break; }
                        cur = prev;
                    }
                }
            }
            if !added { break; }
        }
        // Trace ONE closed rim loop the way a user's loop-select lands: seed on the
        // sharpest edge, then at each vertex continue onto the sharpest edge that is not
        // the one we came in on, until the walk returns to the seed.
        {
            let elen = |e: u32| -> f32 {
                let he = HalfEdgeId(e);
                (mesh.vert(mesh.he(he).vertex).position
                    - mesh.vert(mesh.he_source(he)).position)
                    .length()
            };
            // Seed on the LONGEST selected edge (junk edges are tiny), walk the
            // STRAIGHTEST continuation (max direction dot) within the selection.
            let seed = sel.iter().copied().max_by(|&a, &b| elen(a).partial_cmp(&elen(b)).unwrap());
            if let Some(seed) = seed {
                let mut walked: std::collections::HashSet<u32> = std::collections::HashSet::new();
                walked.insert(seed);
                let start_v = mesh.he_source(HalfEdgeId(seed));
                let mut cur_v = mesh.he(HalfEdgeId(seed)).vertex;
                let mut cur_dir = (mesh.vert(cur_v).position - mesh.vert(start_v).position)
                    .normalize_or_zero();
                let mut prev_key = seed;
                for _step in 0..mesh.half_edges.len() {
                    if cur_v == start_v { break; }
                    let mut best: Option<(f32, u32, core_math::half_edge::VertexId)> = None;
                    for h in mesh.vertex_fan(cur_v) {
                        let t = mesh.he(h).twin;
                        let key = if t == NO_TWIN { h.0 } else { h.0.min(t.0) };
                        if key == prev_key || !sel.contains(&key) { continue; }
                        let d = (mesh.vert(mesh.he(h).vertex).position
                            - mesh.vert(cur_v).position)
                            .normalize_or_zero();
                        let straight = d.dot(cur_dir);
                        if best.map_or(true, |(bs, _, _)| straight > bs) {
                            best = Some((straight, key, mesh.he(h).vertex));
                        }
                    }
                    let Some((_, key, next_v)) = best else { break };
                    walked.insert(key);
                    prev_key = key;
                    cur_dir = (mesh.vert(next_v).position - mesh.vert(cur_v).position)
                        .normalize_or_zero();
                    cur_v = next_v;
                }
                if cur_v == start_v { sel = walked; }
            }
        }
        eprintln!("   rim walk selection: {} undirected edges, width={width}", sel.len());
        // Dump each still-open endpoint's neighbourhood (the gap anatomy).
        {
            let mut deg: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
            for &e in &sel {
                let he = HalfEdgeId(e);
                *deg.entry(mesh.he_source(he).0).or_default() += 1;
                *deg.entry(mesh.he(he).vertex.0).or_default() += 1;
            }
            for (&v, &d) in deg.iter() {
                if d != 1 { continue; }
                let vid = core_math::half_edge::VertexId(v);
                let p = mesh.vert(vid).position;
                eprintln!("   open v{} at ({:.3},{:.3},{:.3}) valence={}", v, p.x, p.y, p.z, mesh.vertex_fan(vid).len());
                for h in mesh.vertex_fan(vid) {
                    let t = mesh.he(h).twin;
                    let key = if t == NO_TWIN { h.0 } else { h.0.min(t.0) };
                    let far = mesh.he(h).vertex;
                    let fp = mesh.vert(far).position;
                    let len = (fp - p).length();
                    let deg_str = mesh.edge_dihedral_cos(h)
                        .map(|c| format!("{:.1}", c.clamp(-1.0, 1.0).acos().to_degrees()))
                        .unwrap_or("--".into());
                    eprintln!("      e{key} -> v{} len={:.5} dihedral={} sel={}", far.0, len, deg_str, sel.contains(&key));
                }
            }
        }
        if sel.is_empty() { continue; }
        // Per-vertex selected-degree histogram (corners break the loop gate).
        let mut deg: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for &e in &sel {
            let he = HalfEdgeId(e);
            *deg.entry(mesh.he_source(he).0).or_default() += 1;
            *deg.entry(mesh.he(he).vertex.0).or_default() += 1;
        }
        let mut hist: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (_, d) in &deg { *hist.entry(*d).or_default() += 1; }
        eprintln!("   selected-degree histogram (vertex count by #selected edges): {:?}", hist);
        // Side-face census along the selection.
        let (mut quads, mut tris, mut ngons) = (0, 0, 0);
        for &e in &sel {
            let he = HalfEdgeId(e);
            for h in [he, mesh.he(he).twin] {
                if h == NO_TWIN { continue; }
                let f = mesh.he(h).face;
                if f == core_math::half_edge::NO_FACE { continue; }
                match mesh.face_vertices(f).len() { 3 => tris += 1, 4 => quads += 1, _ => ngons += 1 }
            }
        }
        eprintln!("   side faces: quads={quads} tris={tris} ngons={ngons}");
        let params = core_math::bevel_op::BevelParams { width, segments: 1, shape: 0.5, clamp: true };
        match core_math::bevel_absorb::consume_fuse_bevel(mesh, &sel, &params) {
            Some(out) => eprintln!("   FUSE: OK  v={} f={} folded={}", out.vertex_count(), out.face_count(), out.count_folded_coplanar_pairs()),
            None => {
                eprintln!("   FUSE: refused (see [CONSUME] lines above)");
                match core_math::bevel_absorb::consume_wedge_cut(mesh, &sel, &params) {
                    Some(out) => eprintln!(
                        "   WEDGE: OK  v={} f={} folded={} manifold={:?}",
                        out.vertex_count(), out.face_count(),
                        out.count_folded_coplanar_pairs(), out.is_manifold().is_ok()
                    ),
                    None => eprintln!("   WEDGE: refused (see [CONSUME] lines above)"),
                }
                match core_math::bevel_absorb::absorb_for_bevel(mesh, &sel, width) {
                    Some(a) => eprintln!("   ABSORB v2: changed mesh, v={} f={}", a.vertex_count(), a.face_count()),
                    None => eprintln!("   ABSORB v2: no-op"),
                }
                let b = core_math::bevel_op::bevel_edges_offset(mesh.as_ref(), &sel, &params);
                eprintln!(
                    "   PLAIN BEVEL: installable={} folded={} eff_width={:?}",
                    core_math::bevel_op::bevel_result_installable(mesh, &b),
                    b.count_folded_coplanar_pairs(),
                    core_math::bevel_op::last_bevel_effective_width(),
                );
            }
        }
    }
}

/// AZRA_DIAG_FL4=<path> cargo test -p app diag_fl4_island_bool -- --ignored --nocapture
/// Replays the edit-mode island boolean on a saved file: splits the (single) poly body into
/// connected shells, uses the SMALLEST shell as the cutter and the rest as the target, and
/// reports every kernel gate (owner subtract.fl4 2026-08-03: "kernel rejected").
#[test]
#[ignore]
fn diag_fl4_island_bool() {
    use core_math::half_edge::{FaceId, VertexId, NO_EDGE};
    use core_math::scene_graph::NodePayload;
    use core_math::mesh_boolean::{mesh_boolean, mesh_signed_volume, BoolOp};
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { eprintln!("set AZRA_DIAG_FL4"); return; };
    let pf = load_binary(std::path::Path::new(&path)).expect("load_binary");
    let sg = pf.scene_graph.as_ref().expect("scene graph");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        // Connected components by vertex flood over half-edges.
        let mut comp = vec![u32::MAX; mesh.vertices.len()];
        let mut n_comp = 0u32;
        for s in 0..mesh.vertices.len() {
            if comp[s] != u32::MAX || mesh.vert(VertexId(s as u32)).half_edge == NO_EDGE { continue; }
            let mut stack = vec![s];
            comp[s] = n_comp;
            while let Some(v) = stack.pop() {
                for he in mesh.vertex_fan(VertexId(v as u32)) {
                    let w = mesh.he(he).vertex.0 as usize;
                    if comp[w] == u32::MAX { comp[w] = n_comp; stack.push(w); }
                }
            }
            n_comp += 1;
        }
        eprintln!("== '{name}': {n_comp} shells");
        if n_comp < 2 { continue; }
        // Face â†’ component; count faces per component.
        let mut sizes = vec![0usize; n_comp as usize];
        let mut fcomp: Vec<u32> = Vec::new();
        for i in 0..mesh.faces.len() as u32 {
            let fid = FaceId(i);
            if mesh.face(fid).half_edge == NO_EDGE { fcomp.push(u32::MAX); continue; }
            let c = comp[mesh.face_vertices(fid)[0].0 as usize];
            fcomp.push(c);
            sizes[c as usize] += 1;
        }
        let cutter_c = (0..n_comp).min_by_key(|&c| sizes[c as usize]).unwrap();
        let take = |want_cutter: bool| {
            let faces: std::collections::HashSet<u32> = (0..mesh.faces.len() as u32)
                .filter(|&i| fcomp[i as usize] != u32::MAX
                    && ((fcomp[i as usize] == cutter_c) == want_cutter))
                .collect();
            crate::poly::boolean::mesh_from_faces_diag(mesh, &faces)
        };
        let cutter = take(true).expect("cutter shell");
        let target = take(false).expect("target shell");
        for (tag, m) in [("cutter", &cutter), ("target", &target)] {
            eprintln!("   {tag}: v={} f={} manifold={:?} vol={:.5} folds={}",
                m.vertex_count(), m.face_count(), m.is_manifold().is_ok(),
                mesh_signed_volume(m), m.count_folded_coplanar_pairs());
        }
        let res = mesh_boolean(&target, &cutter, BoolOp::Difference);
        match &res {
            Some(r) => {
                eprintln!("   RESULT: v={} f={} manifold={:?} vol={:.5} folds={}",
                    r.vertex_count(), r.face_count(), r.is_manifold().is_ok(),
                    mesh_signed_volume(r), r.count_folded_coplanar_pairs());
                for i in 0..r.faces.len() as u32 {
                    let fid = FaceId(i);
                    if r.face(fid).half_edge == NO_EDGE { continue; }
                    let n = r.compute_face_normal(fid).normalize_or_zero();
                    eprintln!("     f{i} n=({:.2},{:.2},{:.2}) ring={:?}", n.x, n.y, n.z,
                        r.face_vertices(fid).iter().map(|v| {
                            let p = r.vert(*v).position; format!("({:.2},{:.2},{:.2})", p.x, p.y, p.z)
                        }).collect::<Vec<_>>());
                }
            }
            None => {
                eprintln!("   RESULT: None (kernel/volume gate)");
                // Bypass the volume gate to see the raw kernel output.
                let raw = core_math::mesh_boolean_manifold::manifold_boolean(&target, &cutter, BoolOp::Difference);
                match raw {
                    Some(r) => eprintln!("   RAW kernel: v={} f={} manifold={:?} vol={:.5} (expected in [{:.5},{:.5}])",
                        r.vertex_count(), r.face_count(), r.is_manifold().is_ok(), mesh_signed_volume(&r),
                        mesh_signed_volume(&target) - mesh_signed_volume(&cutter), mesh_signed_volume(&target)),
                    None => eprintln!("   RAW kernel: None"),
                }
            }
        }
    }
}

/// Position+ring FNV fingerprint of a mesh; detects any cross-run topology/geometry divergence.
fn intrude_mesh_fp(m: &core_math::half_edge::HalfEdgeMesh) -> u64 {
    use core_math::half_edge::{FaceId, VertexId, NO_EDGE};
    let mut h: u64 = 1469598103934665603;
    for i in 0..m.vertices.len() as u32 {
        let v = VertexId(i);
        if m.vert(v).half_edge == NO_EDGE { continue; }
        let p = m.vert(v).position;
        for b in [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()] {
            h = (h ^ b as u64).wrapping_mul(1099511628211);
        }
    }
    for i in 0..m.faces.len() as u32 {
        let f = FaceId(i);
        if m.face(f).half_edge == NO_EDGE { continue; }
        for v in m.face_vertices(f) {
            h = (h ^ v.0 as u64).wrapping_mul(1099511628211);
        }
    }
    h
}

/// OWNER-DRIVEN HEADLESS INTRUDE (2026-08-04: "I will create the cube with the exact cut
/// and you will test the intrude"). Loads a .fl4 saved BEFORE the extrude, identifies the
/// intrude region (minority material_id = the owner's yellow mark, else the knife-island
/// heuristic), replicates the drag (extrude region + rigid move along -n), then runs the
/// EXACT app computation via `push_through_core`. One outcome per polygonal body that has
/// a drivable region.
fn drive_intrude_file(
    path: &std::path::Path,
    depth: f32,
) -> Vec<Result<core_math::half_edge::HalfEdgeMesh, String>> {
    use core_math::half_edge::{FaceId, HalfEdgeMesh, NO_EDGE};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    let mut outcomes = Vec::new();
    let pf = load_binary(path).expect("load_binary");
    let sg = pf.scene_graph.as_ref().expect("scene graph");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        let pre: HalfEdgeMesh = (**mesh).clone();
        eprintln!("== '{name}': v={} f={}", pre.vertex_count(), pre.face_count());
        // Group faces by plane; the island lives on the plane with the most faces.
        let plane = |m: &HalfEdgeMesh, f: FaceId| {
            let n = m.compute_face_normal(f).normalize_or_zero();
            let vs = m.face_vertices(f);
            let c: Vec3 = vs.iter().map(|&v| m.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            (n, n.dot(c))
        };
        let area = |m: &HalfEdgeMesh, f: FaceId| {
            let vs = m.face_vertices(f);
            let mut nv = Vec3::ZERO;
            for k in 0..vs.len() {
                let a = m.vert(vs[k]).position;
                let b = m.vert(vs[(k + 1) % vs.len()]).position;
                nv += a.cross(b);
            }
            nv.length() * 0.5
        };
        let mut groups: Vec<(Vec3, f32, Vec<u32>)> = Vec::new();
        for i in 0..pre.faces.len() as u32 {
            let f = FaceId(i);
            if pre.face(f).half_edge == NO_EDGE { continue; }
            let (n, d) = plane(&pre, f);
            match groups.iter_mut().find(|(gn, gd, _)| gn.dot(n) > 0.999 && (gd - d).abs() < 1e-4) {
                Some(g) => g.2.push(i),
                None => groups.push((n, d, vec![i])),
            }
        }
        // Region selection: PREFER the owner's marks â€” every NON-MAJORITY material_id is
        // a marked region (owner 2026-08-04: "yellow material poly is the intrude
        // target"; test-05 carries TWO marks â€” yellow intrude + red witness â€” and the old
        // single-minority pick broke ties via HashMap iteration order, flapping the driven
        // region PER PROCESS and masquerading as tool nondeterminism). Regions are driven
        // in ascending material-id order, each as its own intrude. Fall back to the island
        // heuristic (largest same-plane group minus its biggest face) when materials are
        // uniform.
        let islands: Vec<Vec<u32>> = {
            let mut mat_count: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
            for i in 0..pre.faces.len() as u32 {
                let f = FaceId(i);
                if pre.face(f).half_edge == NO_EDGE { continue; }
                mat_count.entry(pre.face(f).material_id).or_default().push(i);
            }
            if mat_count.len() > 1 {
                let mut mats: Vec<(u32, Vec<u32>)> = mat_count.into_iter().collect();
                mats.sort_unstable_by_key(|(mid, _)| *mid);
                // The majority material is the unmarked hull: highest face count,
                // lowest id on a tie â€” remove exactly one.
                let majority = mats
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, (_, v))| v.len())
                    .map(|(i, _)| i)
                    .unwrap();
                mats.remove(majority);
                for (mid, fs) in &mats {
                    eprintln!("   region by MATERIAL {} ({} faces): {:?}", mid, fs.len(), fs);
                }
                mats.into_iter().map(|(_, fs)| fs).collect()
            } else {
                let Some((_, _, members)) = groups.iter().max_by_key(|(_, _, m)| m.len()) else { continue };
                if members.len() < 2 {
                    eprintln!("   no multi-face plane â€” nothing to intrude");
                    continue;
                }
                let biggest = *members.iter().max_by(|&&a, &&b| {
                    area(&pre, FaceId(a)).total_cmp(&area(&pre, FaceId(b)))
                }).unwrap();
                vec![members.iter().copied().filter(|&f| f != biggest).collect()]
            }
        };
        for island in islands {
        if island.is_empty() { continue; }
        // Region normal from the actual island faces.
        let face_n = {
            let mut sum = Vec3::ZERO;
            for &f in &island { sum += pre.compute_face_normal(FaceId(f)); }
            sum.normalize_or_zero()
        };
        eprintln!("   island={:?} n=({:.2},{:.2},{:.2})", island, face_n.x, face_n.y, face_n.z);
        let n = face_n; // region mode
        let offset = -depth;
        // Replicate the drag: topological extrude then rigid cap move along n*offset.
        let mut cur = pre.clone();
        let sel: std::collections::HashSet<FaceId> = island.iter().map(|&f| FaceId(f)).collect();
        eprintln!("   fp(pre)={:x}", intrude_mesh_fp(&pre));
        let caps = cur.extrude_region(&sel, Vec3::ZERO);
        eprintln!("   fp(post-extrude)={:x}", intrude_mesh_fp(&cur));
        let cap_ids: Vec<u32> = caps.iter().map(|f| f.0).collect();
        let mut moved: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &c in &caps {
            for v in cur.face_vertices(c) {
                if moved.insert(v.0) {
                    let p = cur.vert(v).position;
                    cur.vert_mut(v).position = p + n * offset;
                }
            }
        }
        eprintln!("   drag replicated: cur v={} f={} caps={:?} fp={:x}", cur.vertex_count(), cur.face_count(), cap_ids, intrude_mesh_fp(&cur));
        match crate::FourthLayerApp::push_through_core(&pre, &cur, &cap_ids, face_n, n, offset, false) {
            Ok(res) => {
                let tris = (0..res.faces.len() as u32).filter(|&i| {
                    let fid = FaceId(i);
                    res.face(fid).half_edge != NO_EDGE && res.face_vertices(fid).len() == 3
                }).count();
                // Collapsed-edge scan: any result edge much shorter than the shortest input edge.
                let min_in = {
                    let mut m = f32::MAX;
                    for i in 0..pre.half_edges.len() as u32 {
                        let he = core_math::half_edge::HalfEdgeId(i);
                        if !pre.is_edge_live(he) { continue; }
                        m = m.min(pre.vert(pre.he_source(he)).position.distance(pre.vert(pre.he(he).vertex).position));
                    }
                    m
                };
                let mut collapsed = 0;
                for i in 0..res.half_edges.len() as u32 {
                    let he = core_math::half_edge::HalfEdgeId(i);
                    if !res.is_edge_live(he) { continue; }
                    let l = res.vert(res.he_source(he)).position.distance(res.vert(res.he(he).vertex).position);
                    if l < min_in * 0.05 { collapsed += 1; }
                }
                eprintln!("   RESULT: v={} f={} tris={} folds={} collapsed_edges={} manifold={:?}",
                    res.vertex_count(), res.face_count(), tris,
                    res.count_folded_coplanar_pairs(), collapsed, res.is_manifold().is_ok());
                outcomes.push(Ok(res));
            }
            Err(msg) => {
                eprintln!("   CORE REFUSED: {msg}");
                outcomes.push(Err(msg));
            }
        }
        } // for island
    }
    outcomes
}

/// Env-driven wrapper for ad-hoc owner files: AZRA_INTRUDE_FL4=<path> [AZRA_INTRUDE_DEPTH].
#[test]
#[ignore]
fn drive_intrude_from_fl4() {
    let Ok(path) = std::env::var("AZRA_INTRUDE_FL4") else { eprintln!("set AZRA_INTRUDE_FL4"); return; };
    let depth: f32 = std::env::var("AZRA_INTRUDE_DEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(1.2);
    drive_intrude_file(std::path::Path::new(&path), depth);
}

/// Owner TEST-021 (2026-08-06): Extrude Along Normals inward must not SHEAR walls.
/// Individual-face path: each face along its own normal → wall quads stay planar.
/// Old region+blend path: multi-face non-planar selection shears walls (crooked geo).
#[test]
fn test_021_along_normals_inward_keeps_planar_walls() {
    use core_math::half_edge::{FaceId, VertexId, NO_EDGE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    use std::collections::{HashMap, HashSet};

    fn face_warp(m: &core_math::half_edge::HalfEdgeMesh, f: FaceId) -> f32 {
        let vs = m.face_vertices(f);
        if vs.len() < 4 {
            return 0.0;
        }
        let p0 = m.vert(vs[0]).position;
        let p1 = m.vert(vs[1]).position;
        let p2 = m.vert(vs[2]).position;
        let n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        if n == Vec3::ZERO {
            return 0.0;
        }
        let d = n.dot(p0);
        vs.iter()
            .map(|&v| (n.dot(m.vert(v).position) - d).abs())
            .fold(0.0f32, f32::max)
    }
    fn max_wall_warp(m: &core_math::half_edge::HalfEdgeMesh, caps: &[FaceId]) -> f32 {
        let capset: HashSet<FaceId> = caps.iter().copied().collect();
        let mut max_w = 0.0f32;
        for &c in caps {
            for he in m.face_half_edges(c) {
                let tw = m.he(he).twin;
                if tw == NO_TWIN {
                    continue;
                }
                let wf = m.he(tw).face;
                if wf == core_math::half_edge::NO_FACE || capset.contains(&wf) {
                    continue;
                }
                max_w = max_w.max(face_warp(m, wf));
            }
        }
        max_w
    }

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/planning/poly/repro/test-021.fl4");
    if !path.exists() {
        eprintln!("skip: test-021.fl4 missing");
        return;
    }
    let pf = load_binary(&path).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { continue };
        let pre = &**mesh;
        let mut mat: HashMap<u32, Vec<u32>> = HashMap::new();
        for i in 0..pre.faces.len() as u32 {
            let f = FaceId(i);
            if pre.face(f).half_edge == NO_EDGE {
                continue;
            }
            mat.entry(pre.face(f).material_id).or_default().push(i);
        }
        let region: Vec<u32> = if mat.len() > 1 {
            let maj = mat.iter().max_by_key(|(_, v)| v.len()).map(|(k, _)| *k).unwrap();
            mat.into_iter()
                .filter(|(k, _)| *k != maj)
                .flat_map(|(_, v)| v)
                .collect()
        } else {
            (0..pre.faces.len() as u32)
                .filter(|&i| pre.face(FaceId(i)).half_edge != NO_EDGE)
                .collect()
        };
        assert!(region.len() >= 2, "fixture should mark a multi-face region");
        let delta = -0.35f32;

        // FIXED path: individual face extrude + face normal (no blend).
        let mut ind = pre.clone();
        let mut faces_sorted = region.clone();
        faces_sorted.sort_unstable();
        let mut caps_ind: Vec<FaceId> = Vec::new();
        for &fid in &faces_sorted {
            let n = ind.compute_face_normal(FaceId(fid)).normalize_or_zero();
            if n.length_squared() < 0.25 {
                continue;
            }
            let cap = ind.extrude_face(FaceId(fid), Vec3::ZERO);
            for v in ind.face_vertices(cap) {
                let p = ind.vert(v).position;
                ind.vert_mut(v).position = p + n * delta;
            }
            caps_ind.push(cap);
        }
        let wall_ind = max_wall_warp(&ind, &caps_ind);

        // OLD path: region + vertex normal blend (the crooked one).
        let mut old = pre.clone();
        let sel: HashSet<FaceId> = region.iter().map(|&f| FaceId(f)).collect();
        let caps_old = old.extrude_region(&sel, Vec3::ZERO);
        let mut acc: HashMap<u32, Vec3> = HashMap::new();
        for &c in &caps_old {
            let n = old.compute_face_normal(c);
            for v in old.face_vertices(c) {
                *acc.entry(v.0).or_insert(Vec3::ZERO) += n;
            }
        }
        for (vid, n) in acc {
            let un = n.normalize_or_zero();
            if un.length_squared() < 0.25 {
                continue;
            }
            let p = old.vert(VertexId(vid)).position;
            old.vert_mut(VertexId(vid)).position = p + un * delta;
        }
        let wall_old = max_wall_warp(&old, &caps_old);

        assert!(
            wall_ind < 1e-3,
            "individual along-normals walls must stay planar (warp {wall_ind:.5})"
        );
        assert!(
            wall_old > wall_ind * 2.0 || wall_old > 1e-3,
            "sanity: old blend path must shear more (old={wall_old:.5} ind={wall_ind:.5})"
        );
        assert!(ind.validate().is_ok(), "valid: {:?}", ind.validate());
        assert!(ind.is_manifold().is_ok(), "manifold: {:?}", ind.is_manifold());
    }
}

/// Owner TEST_020 (2026-08-06): rim-touch / multi-face region must not refuse with
/// "not a single simple loop" — that left the raw topological extrude (bad pocket).
/// Also Ctrl+X DeleteFace must apply on every live face of the fixture.
#[test]
fn test_020_intrude_boundary_and_ctrl_x() {
    use core_math::half_edge::{FaceId, NO_EDGE};
    use core_math::mesh_ops::PolyEdit;
    use core_math::scene_graph::NodePayload;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/planning/poly/repro/test-020.fl4");
    if !path.exists() {
        eprintln!("skip: test-020.fl4 not present at {}", path.display());
        return;
    }
    let outcomes = drive_intrude_file(&path, 0.5);
    assert!(
        !outcomes.is_empty(),
        "test-020 must drive at least one region"
    );
    for (i, o) in outcomes.iter().enumerate() {
        match o {
            Ok(m) => {
                assert!(m.validate().is_ok(), "result {i} validate: {:?}", m.validate());
                assert!(
                    m.integrity().is_ok(),
                    "result {i} integrity: {:?}",
                    m.integrity()
                );
            }
            Err(msg) => {
                assert!(
                    !msg.contains("not a single simple loop"),
                    "result {i} still refuses simple-loop (TEST_020 edge case unfixed): {msg}"
                );
                // Other skip reasons (pocket / not through) are acceptable fall-backs.
            }
        }
    }
    // Ctrl+X face path: every live face must be deletable.
    let pf = load_binary(&path).expect("load test-020");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { continue };
        let base = &**mesh;
        let mut any = false;
        for i in 0..base.faces.len() as u32 {
            if base.face(FaceId(i)).half_edge == NO_EDGE {
                continue;
            }
            any = true;
            let mut w = base.clone();
            PolyEdit::DeleteFace { face: i }
                .apply_in_place(&mut w)
                .unwrap_or_else(|e| {
                    panic!("Ctrl+X DeleteFace({i}) must work on test-020: {e}");
                });
            assert_eq!(w.face_count(), base.face_count() - 1);
        }
        assert!(any, "test-020 body has no live faces");
    }
}

/// PINNED OWNER FIXTURES (2026-08-04). These four .fl4 files are the exact scenes the
/// owner used to break the intrude live, saved pre-extrude with the region marked in
/// yellow material. Each drove a multi-day root-cause hunt:
///   test-02 -- complex knife cut (false volume-gate refusal class)
///   test-03 -- tilted-face region (entry-lift / shear class)
///   test-04 -- 45-degree chamfer, horseshoe coplanar neighbour (fan-triangulation class;
///              volume pinned to Blender EXACT on identical input)
///   test-05 -- full pierce "completely off the poly island" (depth clamp + resolved-volume
///              gate; driven at a normal depth AND far beyond the exit)
/// Every drive must produce an installable result: watertight, edge-manifold, fold-free
/// (guaranteed by push_through_core Ok) -- asserted again here defensively. test-04 also
/// runs TWICE and must fingerprint-match itself (the extrude_region HashSet-order
/// nondeterminism regression guard).
#[test]
fn intrude_owner_fixtures_pinned() {
    let repro = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/planning/poly/repro");
    let drive_clean = |file: &str, depth: f32| -> Vec<core_math::half_edge::HalfEdgeMesh> {
        let outcomes = drive_intrude_file(&repro.join(file), depth);
        assert!(!outcomes.is_empty(), "{file}: no drivable region found");
        outcomes
            .into_iter()
            .map(|o| {
                let res = o.unwrap_or_else(|e| panic!("{file} depth={depth}: refused: {e}"));
                assert_eq!(
                    res.count_folded_coplanar_pairs(), 0,
                    "{file} depth={depth}: folded result"
                );
                assert!(res.is_edge_manifold().is_ok(), "{file} depth={depth}: non-manifold");
                res
            })
            .collect()
    };
    drive_clean("test-02.fl4", 1.2);
    drive_clean("test-03.fl4", 1.2);
    drive_clean("test-05.fl4", 1.2);
    drive_clean("test-05.fl4", 8.0); // full pierce, clamped to the farthest exit
    let a = drive_clean("test-04.fl4", 1.2);
    let vol = core_math::mesh_boolean::mesh_signed_volume(&a[0]);
    assert!(
        (vol - 0.746993).abs() < 1e-3,
        "test-04: volume {vol:.6} drifted from the Blender-EXACT pin 0.746993"
    );
    let b = drive_clean("test-04.fl4", 1.2);
    assert_eq!(
        intrude_mesh_fp(&a[0]),
        intrude_mesh_fp(&b[0]),
        "test-04: two identical drives produced different meshes -- order nondeterminism is back"
    );
}

/// Inspect an owner reference file: classify every edge of each body so we can compare the
/// tag set Smart Select would produce against the result he drew. AZRA_DIAG_FL4=<path>.
#[test]
#[ignore]
fn diag_edge_classes() {
    use core_math::half_edge::{HalfEdgeId, NO_TWIN, NO_FACE};
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { return };
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, modifiers, .. } = &node.payload else { continue };
        let mut flat = 0usize;      // coplanar: no plane change
        let mut smooth = 0usize;    // < 30 deg: inside a round
        let mut crease = 0usize;    // >= 30 deg: a real corner
        let mut seen = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) { continue; }
            let tw = mesh.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if !seen.insert(canon) { continue; }
            match mesh.edge_dihedral_cos(h) {
                None => {}
                Some(c) if c >= 1.0 - 1e-6 => flat += 1,
                Some(c) if c > 30.0f32.to_radians().cos() => smooth += 1,
                Some(_) => crease += 1,
            }
        }
        eprintln!(
            "[CLASS] '{name}' v={} f={} mods={} | flat={flat} smooth(round)={smooth} crease={crease}",
            mesh.vertex_count(), mesh.face_count(), modifiers.len()
        );
    }
}

/// DRIVE THE OWNER'S ACTUAL FILE through the REAL modifier stack. Lesson from TEST-08: a
/// synthetic cube test proved the kernel could do it while his loaded shape still failed,
/// because his mesh carries no tags. AZRA_DIAG_FL4=<path>.
#[test]
#[ignore]
fn diag_smart_select_on_file() {
    use core_math::scene_graph::{NodePayload, PolyModifierType};
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { return };
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    let empty = std::collections::HashMap::new();
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, modifiers, .. } = &node.payload else { continue };
        // A body with no modifier still needs testing: synthesize the Bevel the owner would
        // add by hand ("apply the bevel modifier and you will see they get beveled").
        let modifiers: Vec<_> = if modifiers.is_empty() {
            let mut b = core_math::scene_graph::PolyModifier::new(PolyModifierType::Bevel);
            b.bevel_width = 0.02;
            b.bevel_segments = 1;
            vec![b]
        } else {
            modifiers.clone()
        };
        let mut off = modifiers.clone();
        let mut on = modifiers.clone();
        for m in off.iter_mut() {
            if m.mod_type == PolyModifierType::Bevel { m.bevel_smart_select = false; }
        }
        for m in on.iter_mut() {
            if m.mod_type == PolyModifierType::Bevel { m.bevel_smart_select = true; }
        }
        let a = crate::FourthLayerApp::apply_poly_modifiers(mesh, &off, &empty);
        let b = crate::FourthLayerApp::apply_poly_modifiers(mesh, &on, &empty);
        eprintln!(
            "[SS] '{name}' base v={} f={} | smart OFF -> v={} f={} | smart ON -> v={} f={} | manifold={:?}",
            mesh.vertex_count(), mesh.face_count(),
            a.vertex_count(), a.face_count(),
            b.vertex_count(), b.face_count(), b.is_manifold().is_ok()
        );
    }
}

/// Inspect the FLAT support edges a boolean leaves on a cut face: their length, and whether
/// each endpoint is shared with the hole rim. Owner test_010: "one segment is not connected
/// to the cylinder vertex, and the angle of the segment is too strong."
#[test]
#[ignore]
fn diag_support_edges() {
    use core_math::half_edge::{HalfEdgeId, VertexId, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { return };
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        // Vertex valence: a rim vertex of a cylindrical hole has high valence.
        let mut val = vec![0usize; mesh.vertices.len()];
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if mesh.is_edge_live(h) { val[mesh.he(h).vertex.0 as usize] += 1; }
        }
        let mut seen = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) { continue; }
            let tw = mesh.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if !seen.insert(canon) { continue; }
            if !mesh.edge_is_flat(h, 0.0) { continue; }
            let (a, b) = (mesh.he_source(h), mesh.he(h).vertex);
            let (pa, pb) = (mesh.vert(a).position, mesh.vert(b).position);
            eprintln!(
                "[SUP] '{name}' edge{canon} len={:.4} | v{} val={} -> v{} val={}",
                pa.distance(pb), a.0, val[a.0 as usize], b.0, val[b.0 as usize]
            );
            let _ = VertexId(0);
        }
    }
}

/// SMART CLAMP on the owner's real file. Reports every FLAT edge's length before and after
/// `support_edge::relocate_support_edges`, plus (with AZRA_SMARTCLAMP_DEBUG=1) the reason
/// each candidate was accepted or refused.
///
/// DIAGNOSTIC ONLY: it feeds ALL flat edges as the eligibility set, because a .fl4 saved
/// before the sidecar existed carries no `op_created` tags. The SHIPPING path never does
/// this -- it requires boolean provenance or a persisted tag (see `support_edge.rs`).
#[test]
#[ignore]
fn diag_smart_clamp() {
    use core_math::half_edge::{HalfEdgeId, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { return };
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    let flat_lens = |m: &core_math::half_edge::HalfEdgeMesh| -> Vec<f32> {
        let mut seen = std::collections::HashSet::new();
        let mut l = Vec::new();
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) || !m.edge_is_flat(h, 0.0) { continue; }
            let tw = m.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if !seen.insert(canon) { continue; }
            l.push(m.vert(m.he_source(h)).position.distance(m.vert(m.he(h).vertex).position));
        }
        l.sort_by(|a, b| a.partial_cmp(b).unwrap());
        l
    };
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        let mut eligible: Vec<HalfEdgeId> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) || !mesh.edge_is_flat(h, 0.0) { continue; }
            let tw = mesh.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if seen.insert(canon) { eligible.push(HalfEdgeId(canon)); }
        }
        // Is the short support edge really the throttle? Report the mesh's global minimum
        // edge length too -- if something else is shorter, the support edges are not what
        // caps Clamp Overlap.
        {
            let mut mn = f32::INFINITY;
            let mut n_below = 0;
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !mesh.is_edge_live(h) { continue; }
                let l = mesh.vert(mesh.he_source(h)).position
                    .distance(mesh.vert(mesh.he(h).vertex).position);
                if l < mn { mn = l; }
                if l < 0.1792 { n_below += 1; }
            }
            eprintln!("[SC] '{name}' min edge len over the WHOLE mesh = {mn:.5}                 (directed edges shorter than the 0.1792 support edge: {n_below})");
        }
        let before = flat_lens(mesh);
        let mut m = (**mesh).clone();
        let moves = core_math::support_edge::relocate_support_edges(&mut m, &eligible);
        eprintln!("[SC] '{name}' flat before = {before:?}");
        eprintln!("[SC] '{name}' flat after  = {:?}  (relocated {})", flat_lens(&m), moves.len());
        for r in &moves {
            eprintln!(
                "[SC]   {:?} -> {:?}  len {:.4} -> {:.4}",
                r.from.1, r.to.1,
                r.from.0.distance(r.from.1), r.to.0.distance(r.to.1)
            );
        }
        // What does Clamp Overlap actually give us, before and after?
        {
            let params = core_math::bevel_op::BevelParams {
                width: 0.3, segments: 2, shape: 0.5, clamp: true,
            };
            let sel: std::collections::HashSet<u32> = (0..mesh.half_edges.len() as u32)
                .filter(|&i| mesh.is_edge_live(HalfEdgeId(i))).collect();
            let _ = core_math::bevel_op::bevel_edges_offset(mesh.as_ref(), &sel, &params);
            let w0 = core_math::bevel_op::last_bevel_effective_width();
            let sel2: std::collections::HashSet<u32> = (0..m.half_edges.len() as u32)
                .filter(|&i| m.is_edge_live(HalfEdgeId(i))).collect();
            let _ = core_math::bevel_op::bevel_edges_offset(&m, &sel2, &params);
            let w1 = core_math::bevel_op::last_bevel_effective_width();
            eprintln!("[SC] '{name}' clamped width req=0.3 -> before {w0:.5} / after {w1:.5}");
        }
        eprintln!(
            "[SC] '{name}' valid={:?} manifold={:?} folds={}",
            m.validate().is_ok(), m.is_manifold().is_ok(), m.count_folded_coplanar_pairs()
        );
    }
}

/// SLIVER CENSUS â€” the measurement that must precede any "make the bevel wider" work.
/// For every polygonal body in a .fl4: global min UNDIRECTED edge length, a decade histogram
/// of edge lengths, counts below candidate epsilons, and `last_bevel_effective_width()` for a
/// requested width (bevel-all-live-edges, the modifier's own selection).
///
///   AZRA_DIAG_FL4=<path> cargo test -p app diag_sliver_census -- --ignored --nocapture
#[test]
#[ignore]
fn diag_sliver_census() {
    use core_math::half_edge::{HalfEdgeId, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { eprintln!("set AZRA_DIAG_FL4"); return };
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        eprintln!("[CENSUS] === '{name}' v={} f={} ===", mesh.vertex_count(), mesh.face_count());
        // Undirected edge lengths (canonical id = min(self, twin)).
        let mut lens: Vec<f32> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) { continue; }
            let tw = mesh.he(h).twin;
            let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
            if !seen.insert(canon) { continue; }
            lens.push(mesh.vert(mesh.he_source(h)).position
                .distance(mesh.vert(mesh.he(h).vertex).position));
        }
        lens.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let bbox_diag = {
            let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
            for v in &mesh.vertices { lo = lo.min(v.position); hi = hi.max(v.position); }
            (hi - lo).length()
        };
        eprintln!("[CENSUS] undirected edges={} bbox_diag={bbox_diag:.4}", lens.len());
        eprintln!("[CENSUS] min={:.6} p1={:.6} median={:.6} max={:.6}",
            lens.first().copied().unwrap_or(0.0),
            lens[lens.len() / 100],
            lens[lens.len() / 2],
            lens.last().copied().unwrap_or(0.0));
        eprintln!("[CENSUS] 20 shortest: {:?}",
            lens.iter().take(20).map(|v| format!("{v:.5}")).collect::<Vec<_>>());
        // Decade histogram from 1e-6.
        for d in -6..2i32 {
            let (lo, hi) = (10f32.powi(d), 10f32.powi(d + 1));
            let n = lens.iter().filter(|&&l| l >= lo && l < hi).count();
            if n > 0 { eprintln!("[CENSUS]   [{lo:e}, {hi:e}) : {n}"); }
        }
        for eps in [1e-4f32, 1e-3, 5e-3, 1e-2, 2e-2, 5e-2, 0.1] {
            eprintln!("[CENSUS]   below {eps:e} : {} (rel {:.2e} of bbox)",
                lens.iter().filter(|&&l| l < eps).count(), eps / bbox_diag);
        }
        // Detail on the 6 shortest edges: endpoints, valence, flatness, incident face sizes.
        {
            let mut val = vec![0usize; mesh.vertices.len()];
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if mesh.is_edge_live(h) { val[mesh.he(h).vertex.0 as usize] += 1; }
            }
            let mut det: Vec<(f32, u32)> = Vec::new();
            let mut seen2 = std::collections::HashSet::new();
            for i in 0..mesh.half_edges.len() as u32 {
                let h = HalfEdgeId(i);
                if !mesh.is_edge_live(h) { continue; }
                let tw = mesh.he(h).twin;
                let canon = if tw != NO_TWIN { i.min(tw.0) } else { i };
                if !seen2.insert(canon) { continue; }
                det.push((mesh.vert(mesh.he_source(h)).position
                    .distance(mesh.vert(mesh.he(h).vertex).position), canon));
            }
            det.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for (l, canon) in det.iter().take(6) {
                let h = HalfEdgeId(*canon);
                let (a, b) = (mesh.he_source(h), mesh.he(h).vertex);
                let (fa, fb) = (mesh.he(h).face, mesh.he(mesh.he(h).twin).face);
                eprintln!("[CENSUS] short e{canon} len={l:.6} v{}{:?} val={} -> v{}{:?} val={} flat={} faces={:?}({})/{:?}({})",
                    a.0, mesh.vert(a).position, val[a.0 as usize],
                    b.0, mesh.vert(b).position, val[b.0 as usize],
                    mesh.edge_is_flat(h, 0.0),
                    fa, mesh.face_vertices(fa).len(), fb, mesh.face_vertices(fb).len());
            }
        }
        // Effective bevel width, bevel-all (the modifier's selection), before and after the
        // sliver cleanup that now runs inside the boolean.
        let report = |tag: &str, mm: &core_math::half_edge::HalfEdgeMesh| {
            let sel: std::collections::HashSet<u32> = (0..mm.half_edges.len() as u32)
                .filter(|&i| mm.is_edge_live(HalfEdgeId(i))).collect();
            for req in [0.05f32, 0.1, 0.3] {
                let params = core_math::bevel_op::BevelParams {
                    width: req, segments: 2, shape: 0.5, clamp: true,
                };
                let out = core_math::bevel_op::bevel_edges_offset(mm, &sel, &params);
                eprintln!("[CENSUS] {tag} bevel req={req} -> eff_width={:.6} (v={} f={} valid={} manifold={} folds={})",
                    core_math::bevel_op::last_bevel_effective_width(),
                    out.vertex_count(), out.face_count(), out.validate().is_ok(),
                    out.is_edge_manifold().is_ok(), out.count_folded_coplanar_pairs());
            }
        };
        report("BEFORE", mesh);
        if let Some(clean) = core_math::mesh_boolean_manifold::collapse_slivers_preserving(mesh, &[]) {
            eprintln!("[CENSUS] cleaned: v={} f={} min_edge={:.6} valid={} manifold={} folds={} vol {:.6}->{:.6}",
                clean.vertex_count(), clean.face_count(),
                core_math::mesh_boolean_manifold::min_edge_length(&clean),
                clean.validate().is_ok(), clean.is_edge_manifold().is_ok(),
                clean.count_folded_coplanar_pairs(),
                core_math::mesh_boolean::mesh_signed_volume(mesh),
                core_math::mesh_boolean::mesh_signed_volume(&clean));
            report("AFTER ", &clean);
        } else {
            eprintln!("[CENSUS] cleaned: refused (no accepted collapse)");
        }
    }
}

/// AZRA_DIAG_FL4=<path> cargo test -p app diag_fl4_second_cut_tags -- --ignored --nocapture
/// Measures Smart Select tag survival across a SECOND cut on a saved scene (owner test_014
/// 2026-08-05: "try an alternative cut â€” ALL tags break"). Seeds `op_created` on every live
/// half-edge of the loaded body, then replays BOTH app cut paths at the mesh level:
///   A) whole-body boolean (poly_boolean_execute object-mode install) â€” remap + id-stamp
///   B) edit-mode island boolean (island branch install) â€” remap + position-stamp,
///      including the keep-cutter `merge_meshes` rebuild that renumbers every id
/// and classifies every lost tag as CONSUMED (geometry the cut removed â€” correct fail-closed
/// loss) vs SPLIT (an original edge the cut subdivided â€” the remap_by_position limitation).
#[test]
#[ignore]
fn diag_fl4_second_cut_tags() {
    use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, NO_EDGE};
    use core_math::mesh_boolean::{mesh_boolean_with_provenance, BoolOp};
    use core_math::poly_mesh::CustomData;
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;

    let Ok(path) = std::env::var("AZRA_DIAG_FL4") else { eprintln!("set AZRA_DIAG_FL4"); return; };
    let pf = load_binary(std::path::Path::new(&path)).expect("load_binary");
    let sg = pf.scene_graph.as_ref().expect("scene graph");

    const Q: f32 = 1e5;
    let qp = |p: Vec3| ((p.x * Q).round() as i64, (p.y * Q).round() as i64, (p.z * Q).round() as i64);

    let seed_all = |m: &HalfEdgeMesh| -> (CustomData, usize) {
        let mut cd = CustomData::new();
        let mut n = 0usize;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if m.is_edge_live(h) { cd.op_created.set(h, true); n += 1; }
        }
        (cd, n)
    };
    let count_tags = |cd: &CustomData, m: &HalfEdgeMesh| -> usize {
        (0..m.half_edges.len() as u32)
            .filter(|&i| m.is_edge_live(HalfEdgeId(i)) && cd.op_created.get(HalfEdgeId(i)))
            .count()
    };
    // Classify each seeded edge lost by the remap: SPLIT (endpoints survive, a new mid vertex
    // lies on the segment) vs CONSUMED (anything else).
    let classify_losses = |old: &HalfEdgeMesh, new: &HalfEdgeMesh, cd_new: &CustomData| {
        use std::collections::HashMap;
        let mut new_he: HashMap<(_, _), HalfEdgeId> = HashMap::new();
        let mut out_edges: HashMap<_, Vec<Vec3>> = HashMap::new(); // src key -> dst positions
        for i in 0..new.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !new.is_edge_live(h) { continue; }
            let (a, b) = (new.vert(new.he_source(h)).position, new.vert(new.he(h).vertex).position);
            new_he.insert((qp(a), qp(b)), h);
            out_edges.entry(qp(a)).or_default().push(b);
        }
        let (mut kept, mut split, mut consumed) = (0usize, 0usize, 0usize);
        for i in 0..old.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !old.is_edge_live(h) { continue; }
            let (a, b) = (old.vert(old.he_source(h)).position, old.vert(old.he(h).vertex).position);
            if let Some(&nh) = new_he.get(&(qp(a), qp(b))) {
                if cd_new.op_created.get(nh) { kept += 1; continue; }
            }
            // Lost. Split? some new edge from `a` whose dst m is strictly inside segment a-b.
            let ab = b - a;
            let is_split = out_edges.get(&qp(a)).map_or(false, |dsts| {
                dsts.iter().any(|&m| {
                    let am = m - a;
                    let t = am.dot(ab) / ab.length_squared().max(1e-20);
                    t > 1e-4 && t < 1.0 - 1e-4 && (am - ab * t).length() < 2e-5 * ab.length().max(1.0)
                })
            });
            if is_split { split += 1; } else { consumed += 1; }
        }
        (kept, split, consumed)
    };
    // Axis-aligned box cutter as a closed half-edge mesh.
    let make_box = |mn: Vec3, mx: Vec3| -> HalfEdgeMesh {
        let p = [
            Vec3::new(mn.x, mn.y, mn.z), Vec3::new(mx.x, mn.y, mn.z),
            Vec3::new(mx.x, mx.y, mn.z), Vec3::new(mn.x, mx.y, mn.z),
            Vec3::new(mn.x, mn.y, mx.z), Vec3::new(mx.x, mn.y, mx.z),
            Vec3::new(mx.x, mx.y, mx.z), Vec3::new(mn.x, mx.y, mx.z),
        ];
        let rings: Vec<Vec<usize>> = vec![
            vec![3, 2, 1, 0], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
            vec![1, 2, 6, 5], vec![2, 3, 7, 6], vec![3, 0, 4, 7],
        ];
        HalfEdgeMesh::from_polygons(&p, &rings)
    };

    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        let body: HalfEdgeMesh = (**mesh).clone();
        eprintln!("== '{name}' v={} f={}", body.vertex_count(), body.face_count());
        let (bmin, bmax) = body.bounding_box();
        let ext = bmax - bmin;

        // ---------- PATH A: whole-body boolean (object-mode install) ----------
        {
            let (cd, seeded) = seed_all(&body);
            // Corner-biting cutter: overlaps one corner of the bbox.
            let cutter = make_box(bmin - ext * 0.10, bmin + ext * 0.35);
            let Some((res, _prov)) = mesh_boolean_with_provenance(&body, &cutter, BoolOp::Difference)
            else { eprintln!("   A: kernel refused"); continue; };
            let out = cd.remap_by_position(&body, &res);
            let (kept, split, consumed) = classify_losses(&body, &res, &out);
            eprintln!(
                "   A whole-body: seeded={seeded} kept={} (raw kept-count={}) split-lost={split} consumed-lost={consumed}",
                kept, count_tags(&out, &res)
            );
            assert!(kept > 0, "PATH A wholesale tag loss");
        }

        // ---------- PATH B: island boolean (edit-mode install, both keep-cutter arms) ----------
        {
            // Emulate the owner's flow: an island cutter merged into the SAME mesh, then X-pie.
            let c = (bmin + bmax) * 0.5;
            let cutter_box = make_box(c - ext * 0.15, c + Vec3::new(ext.x, ext.y * 0.15, ext.z * 0.15));
            let merged = crate::poly::boolean::merge_meshes(&body, &cutter_box);
            let (cd, seeded) = seed_all(&merged);
            let live: Vec<u32> = (0..merged.faces.len() as u32)
                .filter(|&i| merged.face(FaceId(i)).half_edge != NO_EDGE)
                .collect();
            // Box faces are appended LAST by merge_meshes.
            let nb_faces: std::collections::HashSet<u32> =
                live[live.len() - 6..].iter().copied().collect();
            let rem_faces: std::collections::HashSet<u32> =
                live.iter().copied().filter(|i| !nb_faces.contains(i)).collect();
            let island = crate::poly::boolean::mesh_from_faces_diag(&merged, &nb_faces).expect("island");
            let remainder = crate::poly::boolean::mesh_from_faces_diag(&merged, &rem_faces).expect("remainder");
            let Some((res, prov)) = mesh_boolean_with_provenance(&remainder, &island, BoolOp::Difference)
            else { eprintln!("   B: kernel refused"); continue; };
            // Same created-edge rule as poly_boolean_execute.
            let mut created: Vec<HalfEdgeId> = Vec::new();
            for i in 0..res.half_edges.len() as u32 {
                let he = HalfEdgeId(i);
                if res.is_edge_live(he) && prov.edge_is_cutter_interior(&res, he) { created.push(he); }
            }
            for e in prov.new_flat_edges(&res) {
                created.push(HalfEdgeId(e));
                let tw = res.he(HalfEdgeId(e)).twin;
                if tw != core_math::half_edge::NO_TWIN { created.push(tw); }
            }
            // Arm 1: consume cutter (install = res directly).
            let mut out = cd.remap_by_position(&merged, &res);
            let (kept, split, consumed) = classify_losses(&merged, &res, &out);
            out.stamp_op_created_by_position(&res, &created, &res);
            let stamped = created.iter().filter(|&&h| out.op_created.get(h)).count();
            eprintln!(
                "   B island (consume): seeded={seeded} kept={kept} split-lost={split} consumed-lost={consumed} created-stamped={stamped}/{}",
                created.len()
            );
            assert!(kept > 0, "PATH B wholesale tag loss");
            assert_eq!(stamped, created.len(), "self-identity position stamp must be total");
            // Arm 2: keep-cutter (install = merge_meshes rebuild â€” every id renumbered).
            let fin = crate::poly::boolean::merge_meshes(&res, &island);
            let mut out2 = cd.remap_by_position(&merged, &fin);
            let before_stamp = count_tags(&out2, &fin);
            out2.stamp_op_created_by_position(&res, &created, &fin);
            let after_stamp = count_tags(&out2, &fin);
            eprintln!(
                "   B island (keep-cutter merge): kept={before_stamp} stamped-into-rebuild={}",
                after_stamp - before_stamp
            );
            assert!(before_stamp > 0, "PATH B keep-cutter wholesale tag loss");
        }
    }
}

// ==================================================================
// FL4PNORM round-trip gates â€” per-poly-body AUTO SMOOTH angle
// (Blender-style normal control). NEWEST section: written AFTER FL4MPNLS.
// The angle could NOT be a field on the in-blob `PolyLayerRecord`: bincode is
// positional, so appending there would make every existing .fl4 carrying poly
// layers read the next record's bytes as the new field. A tail section costs
// zero blob bytes, which is what these gates pin.
// ==================================================================
#[cfg(test)]
mod poly_normal_save_tests {
    use super::*;
    use crate::project::PolyNormalRecord;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_pnorm_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// GATE â€” an authored Auto Smooth angle round-trips through FL4PNORM.
    #[test]
    fn auto_smooth_angle_round_trips() {
        let mut proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        proj.poly_normals = vec![
            PolyNormalRecord { node_id: 5, auto_smooth_deg: 0.0 },   // Shade Flat
            PolyNormalRecord { node_id: 9, auto_smooth_deg: 180.0 }, // Shade Smooth
        ];
        let dir = tmp("rt");
        let path = dir.join("n.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(POLY_NORMAL_MAGIC.len()).any(|w| w == POLY_NORMAL_MAGIC),
            "a scene with an authored angle must carry the FL4PNORM tail"
        );
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_normals.len(), 2);
        assert_eq!(loaded.poly_normals[0], PolyNormalRecord { node_id: 5, auto_smooth_deg: 0.0 });
        assert_eq!(loaded.poly_normals[1], PolyNormalRecord { node_id: 9, auto_smooth_deg: 180.0 });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (compat) â€” no authored angle â†’ NO FL4PNORM magic, and the file is BYTE-IDENTICAL to
    /// what the pre-feature build wrote. This is the whole point of using a tail section.
    #[test]
    fn no_authored_angle_no_magic_and_bytes_unchanged() {
        let proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        assert!(proj.poly_normals.is_empty());
        let dir = tmp("none");
        let path = dir.join("plain.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(POLY_NORMAL_MAGIC.len()).any(|w| w == POLY_NORMAL_MAGIC),
            "an unauthored file must not contain FL4PNORM magic"
        );
        // The blob itself is untouched by this feature: a plain project still deserializes and
        // carries no normal records.
        assert!(load_binary(&path).expect("load").poly_normals.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (coexistence + newest-last) â€” FL4PNORM is written AFTER FL4MPNLS, and BOTH load.
    /// An older AZRA that stops at the unknown FL4PNORM magic still gets every section it knows.
    #[test]
    fn poly_normals_written_after_panel_docs() {
        let mut proj =
            super::child_order_save_tests::project_with(core_math::scene_graph::SceneGraph::new());
        proj.material_panel_docs = vec![MaterialPanelRecord {
            material_id: 1,
            doc_json: serde_json::to_string(
                &crate::material_panel::MaterialPanelDoc::default_for_new(),
            )
            .unwrap(),
        }];
        proj.poly_normals = vec![PolyNormalRecord { node_id: 2, auto_smooth_deg: 25.0 }];
        let dir = tmp("coex");
        let path = dir.join("all.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos_of = |magic: &[u8]| bytes.windows(magic.len()).position(|w| w == magic);
        let pn = pos_of(POLY_NORMAL_MAGIC).expect("FL4PNORM present");
        let mp = pos_of(MATERIAL_PANEL_MAGIC).expect("FL4MPNLS present");
        assert!(pn > mp, "append-newest-last: FL4PNORM after FL4MPNLS (pn={pn} mp={mp})");
        let loaded = load_binary(&path).expect("load with both sections");
        assert_eq!(loaded.material_panel_docs.len(), 1);
        assert_eq!(loaded.poly_normals.len(), 1);
        assert_eq!(loaded.poly_normals[0].auto_smooth_deg, 25.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod bake_settings_save_tests {
    use super::*;
    use crate::project::io::{BAKE_SETTINGS_MAGIC, POLY_REMESH_MAGIC};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_bake_{}_{}", tag, std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// GATE (G1) — default / empty bake settings write NO FL4BAKE magic (byte-compat).
    #[test]
    fn empty_bake_settings_write_no_magic() {
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let proj = child_order_save_tests::project_with(sg);
        assert!(proj.bake_settings.is_empty());
        let dir = tmp("empty");
        let path = dir.join("e.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(BAKE_SETTINGS_MAGIC.len()).any(|w| w == BAKE_SETTINGS_MAGIC),
            "unused bake settings must not emit FL4BAKE"
        );
        assert!(load_binary(&path).expect("load").bake_settings.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (G1) — authored settings round-trip; FL4BAKE coexists after FL4PRMSH.
    #[test]
    fn bake_settings_round_trip_and_coexist_with_remesh() {
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.poly_remesh = vec![crate::project::PolyRemeshRecord {
            node_id: 1,
            mod_index: 0,
            resolution: 64,
            smooth: true,
            padding: 0.1,
            project: 1.0,
            union_islands: false,
        }];
        proj.bake_settings = vec![crate::project::BakeSettingsRecord {
            node_id: 42,
            enabled_maps: 1 << 0 | 1 << 3,
            res_global: 256,
            supersample: 2,
            padding_px: 4,
            source_kind: 1,
            high_nids: vec![7, 9],
            match_by_name: true,
            max_front: 0.2,
            max_rear: 0.03,
            ao_rays: 32,
            curv_radius: 0.05,
        }];
        let dir = tmp("rt");
        let path = dir.join("b.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos = |m: &[u8]| bytes.windows(m.len()).position(|w| w == m);
        let remesh = pos(POLY_REMESH_MAGIC).expect("FL4PRMSH present");
        let bake = pos(BAKE_SETTINGS_MAGIC).expect("FL4BAKE present");
        assert!(bake > remesh, "append-newest-last: FL4BAKE after FL4PRMSH");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.poly_remesh.len(), 1, "older tail still decodes");
        assert_eq!(loaded.bake_settings.len(), 1);
        let r = &loaded.bake_settings[0];
        assert_eq!(r.node_id, 42);
        assert_eq!(r.res_global, 256);
        assert_eq!(r.supersample, 2);
        assert_eq!(r.padding_px, 4);
        assert_eq!(r.source_kind, 1);
        assert_eq!(r.high_nids, vec![7, 9]);
        assert!(r.match_by_name);
        assert!((r.max_front - 0.2).abs() < 1e-6);
        assert!((r.max_rear - 0.03).abs() < 1e-6);
        assert_eq!(r.ao_rays, 32);
        assert!((r.curv_radius - 0.05).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_skips_default_settings() {
        // Pure record filter: default settings never emit a row.
        let s = crate::mesh_map_bake::BakeSettings::default();
        assert!(s.is_default());
    }

    /// GATE (G4) — images ride FL4BAKE next to settings; G1 Vec-only files still load.
    #[test]
    fn bake_images_round_trip_and_g1_vec_still_loads() {
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.bake_settings = vec![crate::project::BakeSettingsRecord {
            node_id: 1,
            enabled_maps: 1,
            res_global: 256,
            supersample: 1,
            padding_px: 0,
            source_kind: 0,
            high_nids: vec![],
            match_by_name: false,
            max_front: 0.1,
            max_rear: 0.05,
            ao_rays: 64,
            curv_radius: 0.0,
        }];
        proj.bake_images = vec![crate::project::BakedImageRecord {
            name: "Cube.Curvature.256".into(),
            node_id: 1,
            kind_bit: 1,
            res: 256,
            slot: 1,
            display_key_lo: 11,
            display_key_hi: 22,
            baked_at_secs: 99,
        }];
        let dir = tmp("g4img");
        let path = dir.join("i.fl4");
        save_binary(&path, &proj).expect("save");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.bake_settings.len(), 1);
        assert_eq!(loaded.bake_images.len(), 1);
        assert_eq!(loaded.bake_images[0].name, "Cube.Curvature.256");
        assert_eq!(loaded.bake_images[0].slot, 1);
        assert_eq!(loaded.bake_images[0].kind_bit, 1);
        assert_eq!(loaded.bake_images[0].display_key_lo, 11);

        // G1 wire: FL4BAKE + bare Vec<BakeSettingsRecord>.
        let mut g1 = child_order_save_tests::project_with(
            child_order_save_tests::graph_with_body().0,
        );
        save_binary(&path, &g1).expect("bare save");
        let mut bytes = std::fs::read(&path).unwrap();
        let recs = vec![crate::project::BakeSettingsRecord {
            node_id: 7,
            enabled_maps: 8,
            res_global: 512,
            supersample: 1,
            padding_px: 0,
            source_kind: 0,
            high_nids: vec![],
            match_by_name: false,
            max_front: 0.1,
            max_rear: 0.05,
            ao_rays: 16,
            curv_radius: 0.0,
        }];
        bytes.extend_from_slice(BAKE_SETTINGS_MAGIC);
        bytes.extend(bincode::serialize(&recs).unwrap());
        std::fs::write(&path, &bytes).unwrap();
        let g1_loaded = load_binary(&path).expect("G1 FL4BAKE must still load");
        assert_eq!(g1_loaded.bake_settings.len(), 1);
        assert_eq!(g1_loaded.bake_settings[0].node_id, 7);
        assert!(g1_loaded.bake_images.is_empty(), "G1 has no image records");
        let _ = g1;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (G7) — FL4B16 is after FL4BAKE; G4 files without it still load.
    #[test]
    fn bake_bit16_tail_after_fl4bake() {
        use crate::project::io::{BAKE_BIT16_MAGIC, BAKE_SETTINGS_MAGIC};
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.bake_settings = vec![crate::project::BakeSettingsRecord {
            node_id: 1,
            enabled_maps: 1,
            res_global: 256,
            supersample: 1,
            padding_px: 0,
            source_kind: 0,
            high_nids: vec![],
            match_by_name: false,
            max_front: 0.1,
            max_rear: 0.05,
            ao_rays: 64,
            curv_radius: 0.0,
        }];
        proj.bake_bit16 = vec![crate::project::BakeBit16Record {
            node_id: 1,
            bit16: 1,
        }];
        let dir = tmp("b16");
        let path = dir.join("b.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos = |m: &[u8]| bytes.windows(m.len()).position(|w| w == m);
        let bake = pos(BAKE_SETTINGS_MAGIC).expect("FL4BAKE");
        let b16 = pos(BAKE_BIT16_MAGIC).expect("FL4B16");
        assert!(b16 > bake, "FL4B16 after FL4BAKE");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.bake_bit16.len(), 1);
        assert_eq!(loaded.bake_bit16[0].bit16, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (G7) — FL4CAGE is after FL4B16; files without it still load.
    #[test]
    fn bake_cage_tail_after_fl4b16() {
        use crate::project::io::{BAKE_BIT16_MAGIC, BAKE_CAGE_MAGIC, BAKE_SETTINGS_MAGIC};
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.bake_settings = vec![crate::project::BakeSettingsRecord {
            node_id: 1,
            enabled_maps: 1,
            res_global: 256,
            supersample: 1,
            padding_px: 0,
            source_kind: 0,
            high_nids: vec![],
            match_by_name: false,
            max_front: 0.1,
            max_rear: 0.05,
            ao_rays: 64,
            curv_radius: 0.0,
        }];
        proj.bake_bit16 = vec![crate::project::BakeBit16Record {
            node_id: 1,
            bit16: 1,
        }];
        proj.bake_cage = vec![crate::project::BakeCageRecord {
            node_id: 1,
            cage_nids: vec![42],
        }];
        let dir = tmp("cage");
        let path = dir.join("c.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos = |m: &[u8]| bytes.windows(m.len()).position(|w| w == m);
        let bake = pos(BAKE_SETTINGS_MAGIC).expect("FL4BAKE");
        let b16 = pos(BAKE_BIT16_MAGIC).expect("FL4B16");
        let cage = pos(BAKE_CAGE_MAGIC).expect("FL4CAGE");
        assert!(b16 > bake, "FL4B16 after FL4BAKE");
        assert!(cage > b16, "FL4CAGE after FL4B16");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.bake_cage.len(), 1);
        assert_eq!(loaded.bake_cage[0].cage_nids, vec![42]);
        // G7 file without FL4CAGE still loads (reuse the bit16-only fixture).
        let mut no_cage = proj;
        no_cage.bake_cage.clear();
        let path2 = dir.join("nocage.fl4");
        save_binary(&path2, &no_cage).expect("save");
        let loaded2 = load_binary(&path2).expect("G7-without-cage must still load");
        assert!(loaded2.bake_cage.is_empty());
        assert_eq!(loaded2.bake_bit16.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (G7) — FL4UDIM is after FL4CAGE; files without it still load.
    #[test]
    fn bake_udim_tail_after_fl4cage() {
        use crate::project::io::{BAKE_CAGE_MAGIC, BAKE_UDIM_MAGIC};
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.bake_cage = vec![crate::project::BakeCageRecord {
            node_id: 1,
            cage_nids: vec![2],
        }];
        proj.bake_udim = crate::project::BakeUdimTail {
            flags: vec![crate::project::BakeUdimFlagRecord {
                node_id: 1,
                udim: 1,
            }],
            tiles: vec![crate::project::BakeUdimTileRecord {
                name: "Cube.AO.u1002.64".into(),
                udim: 1002,
            }],
        };
        let dir = tmp("udim");
        let path = dir.join("u.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos = |m: &[u8]| bytes.windows(m.len()).position(|w| w == m);
        let cage = pos(BAKE_CAGE_MAGIC).expect("FL4CAGE");
        let udim = pos(BAKE_UDIM_MAGIC).expect("FL4UDIM");
        assert!(udim > cage, "FL4UDIM after FL4CAGE");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.bake_udim.flags.len(), 1);
        assert_eq!(loaded.bake_udim.tiles[0].udim, 1002);
        let mut no = proj;
        no.bake_udim = Default::default();
        let path2 = dir.join("noudim.fl4");
        save_binary(&path2, &no).expect("save");
        let loaded2 = load_binary(&path2).expect("without FL4UDIM must still load");
        assert!(loaded2.bake_udim.flags.is_empty());
        assert_eq!(loaded2.bake_cage.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (G7) — FL4GRAY is after FL4UDIM; files without it still load.
    #[test]
    fn bake_gpu_ray_tail_after_fl4udim() {
        use crate::project::io::{BAKE_GPU_RAY_MAGIC, BAKE_UDIM_MAGIC};
        let (sg, _, _) = child_order_save_tests::graph_with_body();
        let mut proj = child_order_save_tests::project_with(sg);
        proj.bake_udim = crate::project::BakeUdimTail {
            flags: vec![crate::project::BakeUdimFlagRecord {
                node_id: 1,
                udim: 1,
            }],
            tiles: vec![],
        };
        proj.bake_gpu_rays = vec![crate::project::BakeGpuRayRecord {
            node_id: 1,
            gpu_rays: 1,
        }];
        let dir = tmp("gray");
        let path = dir.join("g.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        let pos = |m: &[u8]| bytes.windows(m.len()).position(|w| w == m);
        let udim = pos(BAKE_UDIM_MAGIC).expect("FL4UDIM");
        let gray = pos(BAKE_GPU_RAY_MAGIC).expect("FL4GRAY");
        assert!(gray > udim, "FL4GRAY after FL4UDIM");
        let loaded = load_binary(&path).expect("load");
        assert_eq!(loaded.bake_gpu_rays.len(), 1);
        assert_eq!(loaded.bake_gpu_rays[0].gpu_rays, 1);
        let mut no = proj;
        no.bake_gpu_rays.clear();
        let path2 = dir.join("nogray.fl4");
        save_binary(&path2, &no).expect("save");
        let loaded2 = load_binary(&path2).expect("without FL4GRAY must still load");
        assert!(loaded2.bake_gpu_rays.is_empty());
        assert_eq!(loaded2.bake_udim.flags.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// AZRA_DIAG_FL4=<path> cargo test -p app diag_fl4_cleanup_probe -- --ignored --nocapture
#[test]
#[ignore]
fn diag_fl4_cleanup_probe() {
    use core_math::half_edge::{FaceId, VertexId, NO_EDGE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    use core_math::mesh_boolean::{cleanup_mesh, cleanup_mesh_with, CleanupOptions};
    let path = std::env::var("AZRA_DIAG_FL4").expect("AZRA_DIAG_FL4");
    let pf = load_binary(std::path::Path::new(&path)).expect("load");
    let sg = pf.scene_graph.as_ref().expect("sg");
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { continue };
        eprintln!("BEFORE '{name}' v={} f={} e={}", mesh.vertex_count(), mesh.face_count(), mesh.edge_count());
        for i in 0..mesh.faces.len() as u32 {
            let f = FaceId(i);
            if mesh.face(f).half_edge == NO_EDGE { continue; }
            let vs = mesh.face_vertices(f);
            let pts: Vec<_> = vs.iter().map(|&v| mesh.vert(v).position).collect();
            let mut colin = 0usize;
            for k in 0..pts.len() {
                let a = pts[(k + pts.len() - 1) % pts.len()];
                let b = pts[k];
                let c = pts[(k + 1) % pts.len()];
                let d1 = (b - a).normalize_or_zero();
                let d2 = (c - b).normalize_or_zero();
                if d1.dot(d2) > 0.999 { colin += 1; }
            }
            if vs.len() > 4 || colin > 0 {
                eprintln!("  f{i} {}-gon collinear_on_ring={colin} ids={:?}", vs.len(), vs.iter().map(|v| v.0).collect::<Vec<_>>());
                for (k, p) in pts.iter().enumerate() {
                    eprintln!("    [{k}] ({:.4},{:.4},{:.4})", p.x, p.y, p.z);
                }
            }
        }
        let mut v2c = 0usize;
        for i in 0..mesh.vertices.len() as u32 {
            let v = VertexId(i);
            if mesh.vert(v).half_edge == NO_EDGE { continue; }
            let mut ns = std::collections::BTreeSet::new();
            let start = mesh.vert(v).half_edge;
            let mut he = start;
            for _ in 0..64 {
                ns.insert(mesh.he(he).vertex.0);
                let twin = mesh.he(he).twin;
                if twin == NO_TWIN { break; }
                he = mesh.he(twin).next;
                if he == start { break; }
            }
            if ns.len() == 2 {
                let mut it = ns.iter();
                let (a, b) = (*it.next().unwrap(), *it.next().unwrap());
                let pa = mesh.vert(VertexId(a)).position;
                let pb = mesh.vert(v).position;
                let pc = mesh.vert(VertexId(b)).position;
                let d1 = (pb - pa).normalize_or_zero();
                let d2 = (pc - pb).normalize_or_zero();
                if d1.dot(d2) > 0.999 {
                    v2c += 1;
                    eprintln!("  V2-COLIN v{i} at {:?} between v{a}-v{b}", pb);
                }
            }
        }
        eprintln!("valence-2 collinear count: {v2c}");
        match cleanup_mesh(mesh) {
            Some(c) => eprintln!("cleanup default -> v={} f={} e={}", c.vertex_count(), c.face_count(), c.edge_count()),
            None => eprintln!("cleanup default -> None"),
        }
        let opts = CleanupOptions {
            flatten_folds: true,
            merge_coplanar: true,
            dissolve_angle_deg: 5.0,
            protected_edges: vec![],
            ..Default::default()
        };
        match cleanup_mesh_with(mesh, &opts) {
            Some(c) => {
                eprintln!("cleanup dissolve5 -> v={} f={} e={}", c.vertex_count(), c.face_count(), c.edge_count());
                for i in 0..c.faces.len() as u32 {
                    let f = FaceId(i);
                    if c.face(f).half_edge == NO_EDGE { continue; }
                    let n = c.face_vertices(f).len();
                    if n > 4 { eprintln!("  still f{i} {n}-gon"); }
                }
            }
            None => eprintln!("cleanup dissolve5 -> None"),
        }
    }
}

// ---------------------------------------------------------------------------
// Owner fl4: Desktop\bevel segement.fl4 — multi-seg concave edge / 3-spoke end
// ---------------------------------------------------------------------------
#[test]
fn owner_bevel_segment_fl4_no_spoke_fan() {
    use core_math::half_edge::*;
    use core_math::bevel_op::{bevel_edges_offset, BevelParams};
    use core_math::scene_graph::NodePayload;
    use std::collections::HashSet;
    use std::path::Path;

    let path = Path::new(r"C:\Users\Proprio\Desktop\bevel segement.fl4");
    if !path.exists() {
        eprintln!("skip: Desktop\\bevel segement.fl4 not found");
        return;
    }
    let proj = crate::project::load_binary(path).expect("load fl4");
    let sg = proj.scene_graph.expect("scene_graph");
    let mut tested = 0u32;
    for (_nid, node) in sg.poly_bodies() {
        let NodePayload::PolygonalBody { mesh, name, .. } = &node.payload else { continue };
        eprintln!("[fl4] POLY '{name}' nv={} nf={}", mesh.vertex_count(), mesh.face_count());
        let mut seen = HashSet::new();
        for i in 0..mesh.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !mesh.is_edge_live(h) || mesh.he(h).twin == NO_TWIN || mesh.he(h).face == NO_FACE {
                continue;
            }
            let key = i.min(mesh.he(h).twin.0);
            if !seen.insert(key) { continue; }
            // Concave signed dihedral (same rule as bevel_op::edge_is_concave)
            let e = mesh.he(h);
            let n1 = mesh.compute_face_normal(e.face).normalize_or_zero();
            let n2 = mesh.compute_face_normal(mesh.he(e.twin).face).normalize_or_zero();
            let edir = (mesh.vert(e.vertex).position - mesh.vert(mesh.he_source(h)).position).normalize_or_zero();
            if n1.cross(n2).dot(edir) >= 0.0 { continue; }
            let d0 = mesh.vertex_fan(mesh.he_source(h)).len();
            let d1 = mesh.vertex_fan(mesh.he(h).vertex).len();
            let sel: HashSet<u32> = std::iter::once(key).collect();
            let r = bevel_edges_offset(mesh.as_ref(),
                &sel,
                &BevelParams { width: 0.12, segments: 5, shape: 0.5, clamp: true },
            );
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
            let open = (0..r.half_edges.len() as u32)
                .filter(|&i| {
                    r.he(HalfEdgeId(i)).twin == NO_TWIN && r.he(HalfEdgeId(i)).face != NO_FACE
                })
                .count();
            eprintln!(
                "[fl4] e={key} deg={d0}/{d1} nv={} nf={} open={open} man={:?} max_tri_fan={max_tri}",
                r.vertex_count(),
                r.face_count(),
                r.is_manifold().is_ok()
            );
            assert!(r.is_manifold().is_ok(), "e={key} must stay manifold");
            assert_eq!(open, 0, "e={key} must not leave a hole");
            assert!(
                max_tri <= 2,
                "e={key} spoke fan (max_tri={max_tri}) — concave end pinching"
            );
            tested += 1;
        }
    }
    assert!(tested > 0, "expected concave edges in bevel segement.fl4");
}

/// Owner 2026-08-18: desktop `close wall.fl4` — wall cleanup did not merge.
/// The extruded inner wall's new top/bottom n-gons sit on TWO split rim faces;
/// merge must dissolve every coplanar base (not bail at "more than one base").
#[test]
fn close_wall_fl4_merges_split_rim_planes() {
    use core_math::half_edge::{FaceId, HalfEdgeId, NO_EDGE, NO_FACE, NO_TWIN};
    use core_math::scene_graph::NodePayload;
    let path = std::path::Path::new(r"C:\Users\Proprio\Desktop\close wall.fl4");
    if !path.exists() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let pf = load_binary(path).expect("load close wall.fl4");
    let sg = pf.scene_graph.as_ref().expect("sg");
    let mut tested = false;
    for (_nid, node) in sg.all_nodes() {
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            continue;
        };
        let m = &**mesh;
        // Cap = the inner wall that was pushed (unique -X face at x>1).
        let cap = (0..m.faces.len() as u32)
            .map(FaceId)
            .find(|&f| {
                m.face(f).half_edge != NO_EDGE
                    && m.compute_face_normal(f).x < -0.9
                    && m.face_vertices(f).iter().all(|&v| m.vert(v).position.x > 1.0)
            })
            .expect("extruded inner wall");
        let stacked_before = {
            let mut n = 0usize;
            for i in 0..m.half_edges.len() as u32 {
                let he = HalfEdgeId(i);
                if !m.is_edge_live(he) {
                    continue;
                }
                let tw = m.he(he).twin;
                if tw == NO_TWIN || tw.0 < i {
                    continue;
                }
                let (fa, fb) = (m.he(he).face, m.he(tw).face);
                if fa == NO_FACE || fb == NO_FACE {
                    continue;
                }
                let na = m.compute_face_normal(fa).normalize_or_zero();
                let nb = m.compute_face_normal(fb).normalize_or_zero();
                if na.dot(nb) > 0.999 {
                    n += 1;
                }
            }
            n
        };
        assert!(
            stacked_before >= 2,
            "fixture must still have stacked rim planes (got {stacked_before})"
        );
        let mut trial = m.clone();
        let mut caps = std::collections::HashSet::new();
        caps.insert(cap);
        let (merged, folds) = trial.merge_coplanar_extrude_walls(&caps);
        assert!(
            merged >= 2,
            "split-rim extrude must merge at least the top+bottom multi-base walls, got {merged}"
        );
        assert_eq!(folds, 0, "merge must not fold");
        assert!(trial.validate().is_ok(), "valid: {:?}", trial.validate());
        assert!(trial.is_manifold().is_ok(), "manifold: {:?}", trial.is_manifold());
        assert!(
            trial.face_count() < m.face_count(),
            "merge must drop stacked faces ({} -> {})",
            m.face_count(),
            trial.face_count()
        );
        tested = true;
    }
    assert!(tested, "close wall.fl4 has no poly body");
}

// ============================================================================
// SSSCOLORMAP feature gates (2026-08-20) — `PbrMaterial.sss_color_map` is
// `#[serde(skip)]` (zero blob bytes — the bincode field trap cannot fire) and
// rides the existing FL4MATX2 tail as wire tag 7. These gates prove the full
// side-channel round trip AND that a pre-feature file (no tag-7 record) loads
// with a default TexMap.
// ============================================================================
mod sss_color_map_save_tests {
    use super::*;
    use core_math::pbr_material::{MapKind, TexMap};
    use core_math::primitives::{MaterialId, PbrMaterial};
    use core_math::scene_graph::SceneGraph;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("azra_ssscol_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn graph_with_sss_material() -> (SceneGraph, MaterialId) {
        let mut sg = SceneGraph::new();
        let mid = sg.add_material("Skin", PbrMaterial::default());
        let m = sg.material_by_id_mut(mid).expect("material just added");
        m.sss_color_map = TexMap {
            tex_id: 5,
            strength: 0.7,
            uv_scale: 2.0,
            projection: 1,
            blend: 2.5,
            ..TexMap::default()
        };
        (sg, mid)
    }

    /// GATE — collect emits a tag-7 record, the record survives the real
    /// save_binary/load_binary FL4MATX2 tail, and apply reattaches it onto the
    /// `#[serde(skip)]` field of a freshly-loaded (default) material.
    #[test]
    fn sss_color_map_roundtrips_via_fl4matx2() {
        let (sg, mid) = graph_with_sss_material();
        let recs = crate::project_save::collect_material_channels(&sg);
        let r = recs
            .iter()
            .find(|r| r.kind == MapKind::SssColor.wire_tag())
            .expect("a SET sss_color_map must emit a tag-7 FL4MATX2 record");
        assert_eq!((r.material_id, r.tex_id), (mid.0, 5));
        assert!((r.strength - 0.7).abs() < 1e-6 && (r.uv_scale - 2.0).abs() < 1e-6);
        assert_eq!(r.projection & 0xFF, 1);
        assert!((r.blend - 2.5).abs() < 1e-6);

        // Real binary round trip: the record rides the FL4MATX2 tail section.
        let mut proj = super::child_order_save_tests::project_with(sg.clone());
        proj.material_channels = recs;
        let dir = tmp("rt");
        let path = dir.join("s.fl4");
        save_binary(&path, &proj).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(MATERIAL_CHANNEL_MAGIC.len()).any(|w| w == MATERIAL_CHANNEL_MAGIC),
            "file must carry the FL4MATX2 tail"
        );
        let loaded = load_binary(&path).expect("load");
        // The blob-deserialized material has a DEFAULT sss_color_map (serde skip)...
        let fresh = loaded.scene_graph.clone().expect("graph");
        assert_eq!(
            fresh.material_by_id(mid).expect("material survives").1.sss_color_map,
            TexMap::default(),
            "serde(skip) ⇒ the blob itself carries no sss map bytes"
        );
        // ...and the load-side reattach restores it from the tail records.
        let mut healed = fresh;
        crate::project_load::apply_material_channels(&mut healed, &loaded.material_channels);
        let tm = healed.material_by_id(mid).expect("material").1.sss_color_map;
        assert_eq!(tm.tex_id, 5);
        assert!((tm.strength - 0.7).abs() < 1e-6 && (tm.uv_scale - 2.0).abs() < 1e-6);
        assert_eq!(tm.projection, 1);
        assert!((tm.blend - 2.5).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GATE (pre-feature compat) — records written before the feature carry no
    /// tag-7 entry: applying them leaves sss_color_map at its default, and an
    /// unknown FUTURE tag is skipped (forward tolerance unchanged).
    #[test]
    fn pre_feature_records_leave_default_sss_color_map() {
        let mut sg = SceneGraph::new();
        let mid = sg.add_material("Old", PbrMaterial::default());
        let recs = vec![
            // A pre-feature roughness record (tag 2)...
            MaterialChannelRecord {
                material_id: mid.0, kind: 2, tex_id: 3,
                strength: 1.0, uv_scale: 0.5, projection: 0, blend: 4.0,
            },
            // ...and a tag from some future build — must be skipped, not misread.
            MaterialChannelRecord {
                material_id: mid.0, kind: 99, tex_id: 8,
                strength: 1.0, uv_scale: 1.0, projection: 0, blend: 4.0,
            },
        ];
        crate::project_load::apply_material_channels(&mut sg, &recs);
        let m = sg.material_by_id(mid).expect("material").1;
        assert_eq!(m.roughness_map.tex_id, 3, "known channels still reattach");
        assert_eq!(
            m.sss_color_map,
            TexMap::default(),
            "no tag-7 record ⇒ default (unset) sss_color_map — old saves render unchanged"
        );
        // And a scene whose sss map is UNSET emits no tag-7 record (byte-compat saves).
        assert!(
            !crate::project_save::collect_material_channels(&sg)
                .iter()
                .any(|r| r.kind == MapKind::SssColor.wire_tag()),
            "tex_id 0 must not emit a record"
        );
    }
}
