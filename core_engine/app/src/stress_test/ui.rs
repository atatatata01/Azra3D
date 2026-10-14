//! Stress Test UI panel + cache test scene builders.
//! Pure-relocated from stress_test.rs.

use crate::*;
use core_math::primitives::{BlendProfile, PbrMaterial};
use super::{
    Lcg, run_scene_stress, run_operation_stress, run_gpu_stress, run_memory_stress,
    run_production_pipeline_stress, spawn_shared_poly_instances,
    spawn_multi_mesh_poly_instances,
};

// ============================================================
// UI Panel
// ============================================================
impl AzraApp {
    pub(crate) fn draw_stress_test_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_stress_test { return; }

        let mut open = self.ui.show_stress_test;
        egui::Window::new("Stress Test Suite")
            .open(&mut open)
            .default_size([400.0, 500.0])
            .vscroll(true)
            .show(ctx, |ui| {
                ui.heading("Stress Test Suite");
                ui.separator();

                if ui.button("Run Scene Stress").clicked() {
                    let r = run_scene_stress(self);
                    self.ui.stress_test_results.extend(r);
                }
                if ui.button("Run Operation Stress").clicked() {
                    let r = run_operation_stress(self);
                    self.ui.stress_test_results.extend(r);
                }
                if ui.button("Run GPU Stress").clicked() {
                    let r = run_gpu_stress(self);
                    self.ui.stress_test_results.extend(r);
                }
                if ui.button("Run Memory Stress").clicked() {
                    let r = run_memory_stress(self);
                    self.ui.stress_test_results.extend(r);
                }

                ui.separator();
                ui.label("C.1 / B.4 instance scale (shared poly mesh):");
                ui.horizontal(|ui| {
                    for n in [100usize, 500, 1000, 2500, 10000] {
                        if ui.button(format!("{n}")).clicked() {
                            let r = spawn_shared_poly_instances(self, n);
                            self.ui.stress_test_results.extend(r);
                        }
                    }
                });

                ui.separator();
                ui.label("C.6 multi-mesh gate (N meshes × M instances, interleaved):");
                ui.horizontal(|ui| {
                    for (n, m) in [(5usize, 40usize), (8, 50), (5, 200)] {
                        if ui.button(format!("{n}×{m}")).clicked() {
                            let r = spawn_multi_mesh_poly_instances(self, n, m);
                            self.ui.stress_test_results.extend(r);
                        }
                    }
                });

                ui.separator();
                ui.label("Advanced:");
                if ui.button("Run Production Pipeline").clicked() {
                    let r = run_production_pipeline_stress(self);
                    self.ui.stress_test_results.extend(r);
                }

                ui.separator();
                if ui.button("Run ALL").clicked() {
                    self.ui.stress_test_results.clear();
                    let r1 = run_scene_stress(self);
                    self.ui.stress_test_results.extend(r1);
                    self.ui.stress_test_results.push(String::new());
                    let r2 = run_operation_stress(self);
                    self.ui.stress_test_results.extend(r2);
                    self.ui.stress_test_results.push(String::new());
                    let r3 = run_gpu_stress(self);
                    self.ui.stress_test_results.extend(r3);
                    self.ui.stress_test_results.push(String::new());
                    let r4 = run_memory_stress(self);
                    self.ui.stress_test_results.extend(r4);
                    self.ui.stress_test_results.push(String::new());
                    let r5 = run_production_pipeline_stress(self);
                    self.ui.stress_test_results.extend(r5);
                }

                ui.separator();
                ui.label("--- Results ---");

                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for line in &self.ui.stress_test_results {
                            ui.monospace(line);
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Copy Results").clicked() {
                        let text = self.ui.stress_test_results.join("\n");
                        ui.output_mut(|o| o.copied_text = text);
                    }
                    if ui.button("Clear").clicked() {
                        self.ui.stress_test_results.clear();
                    }
                });
            });
        self.ui.show_stress_test = open;
    }

    /// Create a standardized 50-prim, 3-body test scene for cache performance testing.
    /// Fixed seed for reproducibility. Auto-enables Per-Body Cache + Hybrid mode.
    pub(crate) fn create_cache_test_scene(&mut self) {
        use core_math::primitives::{SdfShape, BooleanOp, PbrMaterial};
        use core_math::scene_graph::NodePayload;

        self.new_scene();

        // Remove default sphere (new_scene creates one)
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(self.scene.sdf_body_nid) {
            prims.clear();
        }
        self.sync_flat_cache();

        // Suppress per-prim dirty marking during batch creation
        self.cache.suppress_body_vol_dirty = true;

        // Deterministic RNG
        let mut rng = Lcg::new(0xCAFE_BABE);

        // --- Materials ---
        let chrome = PbrMaterial {
            albedo: Vec3::new(1.0, 1.0, 1.0),
            metallic: 1.0,
            roughness: 0.05,
            ..PbrMaterial::default()
        };
        let gray_matte = PbrMaterial {
            albedo: Vec3::splat(0.5),
            metallic: 0.0,
            roughness: 0.8,
            ..PbrMaterial::default()
        };
        let default_mat = PbrMaterial::default();

        // --- Create 3 bodies ---
        let body_configs: &[(Vec3, usize, &str, &PbrMaterial)] = &[
            (Vec3::new(-3.0, 0.0, 0.0), 17, "Chrome", &chrome),
            (Vec3::new( 0.0, 0.0, 0.0), 17, "Matte",  &gray_matte),
            (Vec3::new( 3.0, 0.0, 0.0), 16, "Default", &default_mat),
        ];

        let root = self.scene.scene_graph.root();
        for (center, count, label, mat) in body_configs {
            let body_name = format!("Test_{}", label);
            let body_nid = self.scene.scene_graph.add_child(
                root,
                NodePayload::SdfBody {
                    name: body_name,
                    primitives: Vec::new(),
                    mesh_cache_id: None,
                    body_op: BooleanOp::Union { k: 0.15 },
                    origin: *center,
                   
                    body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
                    mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
                    blend_profile: BlendProfile::default(),
                    material: PbrMaterial::default(),
                    child_order: Vec::new(),
                mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),},
            );

            // Add spheres to this body
            if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_nid) {
                for i in 0..*count {
                    let uid = self.scene.next_prim_id;
                    self.scene.next_prim_id += 1;
                    let offset = Vec3::new(
                        rng.range_f32(-2.0, 2.0),
                        rng.range_f32(-2.0, 2.0),
                        rng.range_f32(-2.0, 2.0),
                    );
                    let radius = rng.range_f32(0.3, 0.7);
                    let mut prim = SdfPrimitive::with_uid(
                        format!("{}.{:03}", label, i + 1),
                        SdfShape::Sphere { radius },
                        *center + offset,
                        uid,
                    );
                    prim.material = (*mat).clone();
                    // First prim in each body is Union, rest smooth-union
                    prim.operation = if i == 0 {
                        BooleanOp::Union { k: 0.0 }
                    } else {
                        BooleanOp::Union { k: 0.15 }
                    };
                    prims.push(prim);
                }
            }
        }

        // Remove the default body (from new_scene) if it has no prims
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(self.scene.sdf_body_nid) {
            if prims.is_empty() {
                self.scene.scene_graph.remove(self.scene.sdf_body_nid);
            }
        }

        // Set active body to first test body
        if let Some((nid, _)) = self.scene.scene_graph.sdf_bodies().next() {
            self.scene.sdf_body_nid = nid;
            self.scene.active_body_nid = nid;
        }

        self.sync_flat_cache();

        // --- Add 2 point lights ---
        let light_warm = LightData {
            id: self.scene.next_light_id,
            name: format!("Warm.{:03}", self.scene.next_light_id),
            light_type: LightType::Point,
            position: Vec3::new(2.0, 3.0, -1.0),
            intensity: 2.0,
            color: [1.0, 0.92, 0.8],
            range: 15.0,
            ..LightData::default()
        };
        self.scene.next_light_id += 1;
        self.scene.scene_graph.add_child(root, NodePayload::Light(light_warm));

        let light_cool = LightData {
            id: self.scene.next_light_id,
            name: format!("Cool.{:03}", self.scene.next_light_id),
            light_type: LightType::Point,
            position: Vec3::new(-2.0, 1.0, 2.0),
            intensity: 1.0,
            color: [0.7, 0.85, 1.0],
            range: 15.0,
            ..LightData::default()
        };
        self.scene.next_light_id += 1;
        self.scene.scene_graph.add_child(root, NodePayload::Light(light_cool));
        // Lights are graph-native — no cache to sync.

        // --- Try to load Factory_Pumps HDR ---
        if let Some(asset_root) = crate::content_library::find_asset_root() {
            // Search common HDR paths
            let candidates = [
                asset_root.join("HDR_Labs/Factory_Pumps.hdr"),
                asset_root.join("HDR/Factory_Pumps.hdr"),
                asset_root.join("Factory_Pumps.hdr"),
            ];
            if let Some(path) = candidates.iter().find(|p| p.exists()) {
                // env_ibl worker path (decode + install in poll over the next frames)
                eprintln!("[TestScene] HDR queued: {}", path.display());
                self.request_env_load(path.clone());
            }
        }

        // --- Render settings for cache testing ---
        self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        self.render.ssr_enabled = false;
        self.render.fog_enabled = false;
        self.render.bloom_enabled = false;
        self.render.interaction_half_res = true;
        self.render.interaction_shadows = true;
        self.cache.use_body_vol_cache = true;

        // --- Camera: frame all 3 bodies ---
        self.viewport.camera.target = Vec3::ZERO;
        self.viewport.camera.distance = 12.0;
        self.viewport.camera.orbit = glam::Quat::from_euler(glam::EulerRot::YXZ, -0.4, 0.25, 0.0);

        // End batch mode — trigger ONE full rebake
        self.cache.suppress_body_vol_dirty = false;
        self.cache.body_vol_cache.mark_all_bodies_dirty(&self.scene.scene_graph);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT | DirtyFlags::SCENE);

        self.toast("Cache Test Scene: 3 bodies, 50 prims, Hybrid + Per-Body Cache ON");
    }

    // ================================================================
    // Advanced Cache Test Scene — 5 bodies, ~60 prims
    // Stresses every cache edge case: booleans, blends, modifiers,
    // scale extremes, nested bodies, mixed sharp/smooth.
    // ================================================================
    pub(crate) fn create_advanced_cache_test_scene(&mut self) {
        use core_math::primitives::{
            SdfShape, BooleanOp, PbrMaterial, Modifier, ModifierType,
            BoxBevel, BevelMode,
        };
        use core_math::scene_graph::NodePayload;

        // Deterministic seed — identical scene every launch
        let mut rng = Lcg::new(0xDEAD_BEEF_CAFE);
        let _ = &mut rng; // suppress unused-mut if no rng calls

        self.new_scene();

        // Remove default sphere from new_scene
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(self.scene.sdf_body_nid) {
            prims.clear();
        }
        self.sync_flat_cache();

        self.transaction_snapshot_only("Create Advanced Cache Test Scene");

        // Suppress per-prim dirty marking during batch creation
        self.cache.suppress_body_vol_dirty = true;

        let root = self.scene.scene_graph.root();

        // ============================================================
        // Materials
        // ============================================================
        let mat_chrome = PbrMaterial {
            albedo: Vec3::new(1.0, 1.0, 1.0),
            metallic: 1.0,
            roughness: 0.05,
            ..PbrMaterial::default()
        };
        let mat_gray_matte = PbrMaterial {
            albedo: Vec3::splat(0.5),
            metallic: 0.0,
            roughness: 0.8,
            ..PbrMaterial::default()
        };
        let mat_dark_metal = PbrMaterial {
            albedo: Vec3::splat(0.15),
            metallic: 0.9,
            roughness: 0.3,
            ..PbrMaterial::default()
        };
        let mat_gold = PbrMaterial {
            albedo: Vec3::new(1.0, 0.8, 0.3),
            metallic: 1.0,
            roughness: 0.15,
            ..PbrMaterial::default()
        };
        let mat_default = PbrMaterial::default();

        // Helper: allocate a prim UID
        macro_rules! uid {
            () => {{ let u = self.scene.next_prim_id; self.scene.next_prim_id += 1; u }}
        }

        // ============================================================
        // Body A — "Boolean Stress" at (-4, 0, 0)
        // 1 box base, 3 sphere subtracts, 2 cylinder drills,
        // 1 torus smooth-union, nested child body with 3 spheres
        // ============================================================
        let body_a_center = Vec3::new(-4.0, 0.0, 0.0);
        let body_a_nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: "A_BooleanStress".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: body_a_center,
           
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_a_nid) {
            // Base box (union, first prim)
            let mut p = SdfPrimitive::with_uid(
                "A_Base.001".into(),
                SdfShape::Box { half_extents: Vec3::splat(0.75), bevel: BoxBevel::default() },
                body_a_center,
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_chrome.clone();
            prims.push(p);

            // 3 spheres subtracted (sharp boolean)
            let sphere_offsets = [
                Vec3::new(0.3, 0.3, 0.5),
                Vec3::new(-0.4, -0.2, 0.4),
                Vec3::new(0.0, 0.4, -0.3),
            ];
            for (i, off) in sphere_offsets.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("A_SubSphere.{:03}", i + 1),
                    SdfShape::Sphere { radius: 0.35 },
                    body_a_center + *off,
                    uid!(),
                );
                p.operation = BooleanOp::Subtract { k: 0.0 }; // sharp subtract
                p.material = mat_chrome.clone();
                prims.push(p);
            }

            // 2 cylinders subtracted (drill holes — sharp)
            let mut cyl1 = SdfPrimitive::with_uid(
                "A_DrillX.001".into(),
                SdfShape::Cylinder { radius: 0.15, half_height: 1.2 },
                body_a_center + Vec3::new(0.0, 0.0, 0.0),
                uid!(),
            );
            cyl1.rotation_deg = Vec3::new(0.0, 0.0, 90.0); // drill along X
            cyl1.operation = BooleanOp::Subtract { k: 0.0 };
            cyl1.material = mat_chrome.clone();
            prims.push(cyl1);

            let mut cyl2 = SdfPrimitive::with_uid(
                "A_DrillZ.001".into(),
                SdfShape::Cylinder { radius: 0.12, half_height: 1.2 },
                body_a_center + Vec3::new(0.2, 0.2, 0.0),
                uid!(),
            );
            cyl2.rotation_deg = Vec3::new(90.0, 0.0, 0.0); // drill along Z
            cyl2.operation = BooleanOp::Subtract { k: 0.0 };
            cyl2.material = mat_chrome.clone();
            prims.push(cyl2);

            // 1 torus smooth-unioned on top
            let mut torus = SdfPrimitive::with_uid(
                "A_TopTorus.001".into(),
                SdfShape::Torus { major_radius: 0.4, minor_radius: 0.1 },
                body_a_center + Vec3::new(0.0, 0.85, 0.0),
                uid!(),
            );
            torus.operation = BooleanOp::Union { k: 0.3 };
            torus.material = mat_chrome.clone();
            prims.push(torus);
        }

        // Nested child body inside Body A: 3 small spheres, smooth union, subtract from parent
        let child_a_nid = self.scene.scene_graph.add_child(body_a_nid, NodePayload::SdfBody {
            name: "A_NestedCluster".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Subtract { k: 0.0 },
            origin: body_a_center + Vec3::new(0.0, -0.3, 0.0),
           
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(child_a_nid) {
            let child_offsets = [
                Vec3::new(-0.2, 0.0, 0.0),
                Vec3::new(0.15, 0.15, 0.1),
                Vec3::new(0.0, -0.15, -0.15),
            ];
            for (i, off) in child_offsets.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("A_Nested.{:03}", i + 1),
                    SdfShape::Sphere { radius: 0.18 },
                    body_a_center + Vec3::new(0.0, -0.3, 0.0) + *off,
                    uid!(),
                );
                p.operation = if i == 0 {
                    BooleanOp::Union { k: 0.0 }
                } else {
                    BooleanOp::Union { k: 0.5 } // smooth union within child
                };
                p.material = mat_chrome.clone();
                prims.push(p);
            }
        }
        // Body A total: 7 + 3 nested = 10 prims

        // ============================================================
        // Body B — "Blend Cluster" at (0, 0, 0)
        // 8 spheres, varying blend_k and radii
        // ============================================================
        let body_b_center = Vec3::ZERO;
        let body_b_nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: "B_BlendCluster".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: body_b_center,
           
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_b_nid) {
            // Deterministic sphere placements — tight cluster
            let sphere_data: &[(Vec3, f32, f32)] = &[
                // (offset, radius, blend_k)
                (Vec3::new( 0.0,  0.0,  0.0), 0.45, 0.0),  // center, base (hard union)
                (Vec3::new( 0.5,  0.1,  0.0), 0.35, 0.4),
                (Vec3::new(-0.4,  0.2,  0.3), 0.30, 0.5),
                (Vec3::new( 0.1, -0.4,  0.4), 0.40, 0.6),
                (Vec3::new(-0.3,  0.5, -0.2), 0.25, 0.7),
                (Vec3::new( 0.4, -0.1, -0.5), 0.50, 0.4),
                (Vec3::new(-0.2, -0.3, -0.3), 0.20, 0.8),
                (Vec3::new( 0.3,  0.3,  0.3), 0.60, 0.5),
            ];
            for (i, &(off, radius, blend_k)) in sphere_data.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("B_Blend.{:03}", i + 1),
                    SdfShape::Sphere { radius },
                    body_b_center + off,
                    uid!(),
                );
                p.operation = BooleanOp::Union { k: blend_k };
                p.material = mat_gray_matte.clone();
                prims.push(p);
            }
        }
        // Body B total: 8 prims

        // ============================================================
        // Body C — "Shape Zoo" at (4, 0, 0)
        // 1 box w/bevel, 1 cylinder, 1 torus, 1 capsule, 1 cone, 2 spheres
        // All union, no blending
        // ============================================================
        let body_c_center = Vec3::new(4.0, 0.0, 0.0);
        let body_c_nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: "C_ShapeZoo".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: body_c_center,
           
            body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_c_nid) {
            // Box with bevel
            let mut bevel = BoxBevel::default();
            for r in &mut bevel.radii { *r = 0.06; }
            for m in &mut bevel.modes { *m = BevelMode::Round; }
            let mut p = SdfPrimitive::with_uid(
                "C_BevelBox.001".into(),
                SdfShape::Box { half_extents: Vec3::new(0.4, 0.5, 0.35), bevel },
                body_c_center + Vec3::new(-1.0, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Cylinder
            let mut p = SdfPrimitive::with_uid(
                "C_Cylinder.001".into(),
                SdfShape::Cylinder { radius: 0.25, half_height: 0.5 },
                body_c_center + Vec3::new(-0.3, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Torus
            let mut p = SdfPrimitive::with_uid(
                "C_Torus.001".into(),
                SdfShape::Torus { major_radius: 0.35, minor_radius: 0.1 },
                body_c_center + Vec3::new(0.4, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Capsule
            let mut p = SdfPrimitive::with_uid(
                "C_Capsule.001".into(),
                SdfShape::Capsule { radius: 0.15, half_height: 0.4 },
                body_c_center + Vec3::new(1.0, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Cone
            let mut p = SdfPrimitive::with_uid(
                "C_Cone.001".into(),
                SdfShape::Cone { radius: 0.3, half_height: 0.5 },
                body_c_center + Vec3::new(-0.3, 0.0, 0.8),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // 2 spheres of different sizes
            let mut p = SdfPrimitive::with_uid(
                "C_BigSphere.001".into(),
                SdfShape::Sphere { radius: 0.5 },
                body_c_center + Vec3::new(0.4, 0.0, 0.8),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            let mut p = SdfPrimitive::with_uid(
                "C_SmallSphere.001".into(),
                SdfShape::Sphere { radius: 0.2 },
                body_c_center + Vec3::new(1.0, 0.0, 0.8),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);
        }
        // Body C total: 7 prims

        // ============================================================
        // Body D — "Modifier Stress" at (0, 3, 0)
        // 1 box twist, 1 cylinder taper, 1 box shear, 3 plain spheres
        // ============================================================
        let body_d_center = Vec3::new(0.0, 3.0, 0.0);
        let body_d_nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: "D_ModifierStress".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: body_d_center,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_d_nid) {
            // Twisted box
            let mut p = SdfPrimitive::with_uid(
                "D_TwistedBox.001".into(),
                SdfShape::Box { half_extents: Vec3::new(0.3, 0.7, 0.3), bevel: BoxBevel::default() },
                body_d_center + Vec3::new(-1.5, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.modifier = Modifier {
                mod_type: ModifierType::Twist,
                strength: 1.5, // radians per unit along Y
                axis: 0,       // Y axis
                param2: 0.5,
                mask: Default::default(),
                height_map: None,
            };
            p.material = mat_dark_metal.clone();
            prims.push(p);

            // Tapered cylinder
            let mut p = SdfPrimitive::with_uid(
                "D_TaperCyl.001".into(),
                SdfShape::Cylinder { radius: 0.3, half_height: 0.6 },
                body_d_center + Vec3::new(-0.5, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.modifier = Modifier {
                mod_type: ModifierType::Taper,
                strength: 0.8,  // scale factor
                axis: 0,        // Y axis
                param2: 0.3,    // end_scale
                mask: Default::default(),
                height_map: None,
            };
            p.material = mat_dark_metal.clone();
            prims.push(p);

            // Sheared box
            let mut p = SdfPrimitive::with_uid(
                "D_ShearBox.001".into(),
                SdfShape::Box { half_extents: Vec3::new(0.4, 0.5, 0.4), bevel: BoxBevel::default() },
                body_d_center + Vec3::new(0.7, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.modifier = Modifier {
                mod_type: ModifierType::Shear,
                strength: 0.6,
                axis: 1,   // X axis affected
                param2: 0.0, // drive axis Y
                mask: Default::default(),
                height_map: None,
            };
            p.material = mat_dark_metal.clone();
            prims.push(p);

            // 3 plain spheres (comparison)
            let plain_offsets = [
                Vec3::new(-1.5, 0.0, 1.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 1.0),
            ];
            for (i, off) in plain_offsets.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("D_Plain.{:03}", i + 1),
                    SdfShape::Sphere { radius: 0.3 },
                    body_d_center + *off,
                    uid!(),
                );
                p.operation = BooleanOp::Union { k: 0.0 };
                p.material = mat_dark_metal.clone();
                prims.push(p);
            }
        }
        // Body D total: 6 prims

        // ============================================================
        // Body E — "Scale Extremes" at (-2, 3, 0)
        // 1 large sphere (1.5), 5 tiny spheres (0.1-0.15) on surface,
        // 2 medium prims with chamfer union
        // ============================================================
        let body_e_center = Vec3::new(-2.0, 3.0, 0.0);
        let body_e_nid = self.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: "E_ScaleExtremes".into(),
            primitives: Vec::new(),
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: body_e_center,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_e_nid) {
            // Large sphere
            let mut p = SdfPrimitive::with_uid(
                "E_Giant.001".into(),
                SdfShape::Sphere { radius: 1.5 },
                body_e_center,
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_gold.clone();
            prims.push(p);

            // 5 tiny spheres on the surface (deterministic positions on unit sphere * 1.5)
            let tiny_dirs: &[(f32, f32, f32, f32)] = &[
                // (nx, ny, nz, radius)
                ( 0.577,  0.577,  0.577, 0.10),
                (-0.707,  0.707,  0.0,   0.12),
                ( 0.0,    0.447,  0.894, 0.11),
                ( 0.894,  0.0,   -0.447, 0.15),
                (-0.333,  0.667, -0.667, 0.13),
            ];
            for (i, &(nx, ny, nz, r)) in tiny_dirs.iter().enumerate() {
                let dir = Vec3::new(nx, ny, nz).normalize();
                let surface_pos = body_e_center + dir * 1.5;
                let mut p = SdfPrimitive::with_uid(
                    format!("E_Tiny.{:03}", i + 1),
                    SdfShape::Sphere { radius: r },
                    surface_pos,
                    uid!(),
                );
                p.operation = BooleanOp::Union { k: 0.1 }; // slight blend so they merge with big sphere
                p.material = mat_gold.clone();
                prims.push(p);
            }

            // 2 medium prims with chamfer union
            let mut p = SdfPrimitive::with_uid(
                "E_ChamferBox.001".into(),
                SdfShape::Box { half_extents: Vec3::splat(0.3), bevel: BoxBevel::default() },
                body_e_center + Vec3::new(0.8, -1.0, 0.5),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.15 };
            p.blend_profile = BlendProfile::Chamfer;
            p.material = mat_gold.clone();
            prims.push(p);

            let mut p = SdfPrimitive::with_uid(
                "E_ChamferCyl.001".into(),
                SdfShape::Cylinder { radius: 0.2, half_height: 0.4 },
                body_e_center + Vec3::new(-0.6, -1.2, -0.4),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.2 };
            p.blend_profile = BlendProfile::Chamfer;
            p.material = mat_gold.clone();
            prims.push(p);
        }
        // Body E total: 8 prims
        // Grand total: 10 + 8 + 7 + 6 + 8 = 39 direct + nested
        // With some extras, this is ~39-42 prims. Let me add more to hit ~60.

        // ---- Bonus prims to reach ~60 total ----

        // Body B: add 8 more blended prims (varying shapes, not just spheres)
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_b_nid) {
            let extra_data: &[(Vec3, SdfShape, f32)] = &[
                (Vec3::new( 0.6, -0.5,  0.2), SdfShape::Capsule { radius: 0.15, half_height: 0.25 }, 0.5),
                (Vec3::new(-0.5,  0.0, -0.6), SdfShape::Cylinder { radius: 0.2, half_height: 0.3 }, 0.6),
                (Vec3::new( 0.0,  0.6,  0.5), SdfShape::Sphere { radius: 0.22 }, 0.7),
                (Vec3::new(-0.6, -0.4,  0.5), SdfShape::Sphere { radius: 0.28 }, 0.4),
                (Vec3::new( 0.3, -0.6, -0.4), SdfShape::Capsule { radius: 0.12, half_height: 0.2 }, 0.55),
                (Vec3::new(-0.1,  0.7, -0.3), SdfShape::Sphere { radius: 0.35 }, 0.45),
                (Vec3::new( 0.7,  0.4,  0.1), SdfShape::Torus { major_radius: 0.2, minor_radius: 0.06 }, 0.5),
                (Vec3::new(-0.4,  0.3,  0.7), SdfShape::Cone { radius: 0.15, half_height: 0.25 }, 0.6),
            ];
            for (i, (off, shape, blend_k)) in extra_data.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("B_Extra.{:03}", i + 1),
                    shape.clone(),
                    body_b_center + *off,
                    uid!(),
                );
                p.operation = BooleanOp::Union { k: *blend_k };
                p.material = mat_gray_matte.clone();
                prims.push(p);
            }
        }
        // Body B now: 16 prims

        // Body C: add more shapes to the zoo
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_c_nid) {
            // Stair
            let mut p = SdfPrimitive::with_uid(
                "C_Stair.001".into(),
                SdfShape::Stair { half_extents: Vec3::new(0.3, 0.3, 0.4), num_steps: 4 },
                body_c_center + Vec3::new(-1.0, 0.0, 0.8),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Spiral stair
            let mut p = SdfPrimitive::with_uid(
                "C_Spiral.001".into(),
                SdfShape::SpiralStair { radius: 0.5, half_height: 0.5, num_steps: 8, step_width: 0.2 },
                body_c_center + Vec3::new(1.5, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.material = mat_default.clone();
            prims.push(p);

            // Chamfer-subtracted box
            let mut p = SdfPrimitive::with_uid(
                "C_ChamferSub.001".into(),
                SdfShape::Sphere { radius: 0.35 },
                body_c_center + Vec3::new(0.0, 0.0, -0.8),
                uid!(),
            );
            p.operation = BooleanOp::Subtract { k: 0.1 };
            p.blend_profile = BlendProfile::Chamfer;
            p.material = mat_default.clone();
            prims.push(p);
        }
        // Body C now: 10 prims

        // Body D: add 2 more modifier combos
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_d_nid) {
            // Displaced sphere
            let mut p = SdfPrimitive::with_uid(
                "D_Displaced.001".into(),
                SdfShape::Sphere { radius: 0.4 },
                body_d_center + Vec3::new(1.8, 0.0, 0.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.modifier = Modifier {
                mod_type: ModifierType::Displace,
                strength: 0.08, // amplitude
                axis: 0,
                param2: 4.0,    // frequency
                mask: Default::default(),
                height_map: None,
            };
            p.material = mat_dark_metal.clone();
            prims.push(p);

            // Smooth (erosion) capsule
            let mut p = SdfPrimitive::with_uid(
                "D_Smooth.001".into(),
                SdfShape::Capsule { radius: 0.25, half_height: 0.4 },
                body_d_center + Vec3::new(1.8, 0.0, 1.0),
                uid!(),
            );
            p.operation = BooleanOp::Union { k: 0.0 };
            p.modifier = Modifier {
                mod_type: ModifierType::Smooth,
                strength: -0.03, // negative = erosion
                axis: 0,
                param2: 0.5,
                mask: Default::default(),
                height_map: None,
            };
            p.material = mat_dark_metal.clone();
            prims.push(p);
        }
        // Body D now: 8 prims

        // Body E: add 3 more extreme-scale prims
        if let Some(prims) = self.scene.scene_graph.body_primitives_mut(body_e_nid) {
            // Microsphere cluster (3 very tiny spheres near bottom)
            let micro_offsets = [
                Vec3::new( 0.3, -1.5, 0.0),
                Vec3::new( 0.35, -1.45, 0.1),
                Vec3::new( 0.25, -1.48, -0.05),
            ];
            for (i, off) in micro_offsets.iter().enumerate() {
                let mut p = SdfPrimitive::with_uid(
                    format!("E_Micro.{:03}", i + 1),
                    SdfShape::Sphere { radius: 0.05 + i as f32 * 0.02 },
                    body_e_center + *off,
                    uid!(),
                );
                p.operation = BooleanOp::Union { k: 0.05 };
                p.material = mat_gold.clone();
                prims.push(p);
            }
        }
        // Body E now: 11 prims
        // Grand total: 10 + 16 + 10 + 8 + 11 = 55 prims (with nested child body)

        // ---- Remove the default body if empty ----
        if let Some(prims) = self.scene.scene_graph.body_primitives(self.scene.sdf_body_nid) {
            if prims.is_empty() {
                self.scene.scene_graph.remove(self.scene.sdf_body_nid);
            }
        }

        // Set active body to first test body
        if let Some((nid, _)) = self.scene.scene_graph.sdf_bodies().next() {
            self.scene.sdf_body_nid = nid;
            self.scene.active_body_nid = nid;
        }

        self.sync_flat_cache();

        // ============================================================
        // Lights — 2 point + 1 spot
        // ============================================================

        // Warm point light
        let light_warm = LightData {
            id: self.scene.next_light_id,
            name: format!("WarmPoint.{:03}", self.scene.next_light_id),
            light_type: LightType::Point,
            position: Vec3::new(2.0, 4.0, -2.0),
            intensity: 2.5,
            color: [1.0, 0.95, 0.85],
            range: 20.0,
            ..LightData::default()
        };
        self.scene.next_light_id += 1;
        self.scene.scene_graph.add_child(root, NodePayload::Light(light_warm));

        // Cool blue point light
        let light_cool = LightData {
            id: self.scene.next_light_id,
            name: format!("CoolPoint.{:03}", self.scene.next_light_id),
            light_type: LightType::Point,
            position: Vec3::new(-3.0, 2.0, 3.0),
            intensity: 1.2,
            color: [0.7, 0.8, 1.0],
            range: 18.0,
            ..LightData::default()
        };
        self.scene.next_light_id += 1;
        self.scene.scene_graph.add_child(root, NodePayload::Light(light_cool));

        // Spot light pointing down — tests shadow quality on cached bodies
        let light_spot = LightData {
            id: self.scene.next_light_id,
            name: format!("TopSpot.{:03}", self.scene.next_light_id),
            light_type: LightType::Spot,
            position: Vec3::new(0.0, 5.0, 0.0),
            intensity: 3.0,
            color: [1.0, 1.0, 1.0],
            range: 12.0,
            azimuth: 0.0,
            elevation: -90.0, // pointing straight down
            spot_inner_deg: 15.0,
            spot_outer_deg: 25.0,
            ..LightData::default()
        };
        self.scene.next_light_id += 1;
        self.scene.scene_graph.add_child(root, NodePayload::Light(light_spot));

        // Lights are graph-native — no cache to sync.

        // ============================================================
        // HDR environment — try Factory_Pumps
        // ============================================================
        if let Some(asset_root) = crate::content_library::find_asset_root() {
            let candidates = [
                asset_root.join("HDR_Labs/Factory_Pumps.hdr"),
                asset_root.join("HDR/Factory_Pumps.hdr"),
                asset_root.join("Factory_Pumps.hdr"),
            ];
            if let Some(path) = candidates.iter().find(|p| p.exists()) {
                // env_ibl worker path (decode + install in poll over the next frames)
                self.request_env_load(path.clone());
            }
        }

        // ============================================================
        // Render settings — optimized for cache testing
        // ============================================================
        self.render.viewport_mode = crate::ViewportRenderMode::Hybrid;
        self.render.ssr_enabled = false;
        self.render.fog_enabled = false;
        self.render.bloom_enabled = false;
        self.render.interaction_half_res = true;
        self.render.interaction_shadows = true;
        self.cache.use_body_vol_cache = true;

        // ============================================================
        // Camera — frame all 5 bodies, slightly elevated
        // ============================================================
        self.viewport.camera.target = Vec3::new(0.0, 1.2, 0.0);
        self.viewport.camera.distance = 14.0;
        self.viewport.camera.orbit = glam::Quat::from_euler(glam::EulerRot::YXZ, -0.35, 0.30, 0.0);

        // End batch mode — trigger ONE full rebake
        self.cache.suppress_body_vol_dirty = false;
        self.cache.body_vol_cache.mark_all_bodies_dirty(&self.scene.scene_graph);
        self.mark_dirty(DirtyFlags::RENDER | DirtyFlags::LIGHT | DirtyFlags::SCENE);

        self.toast("Advanced Cache Test: 5 bodies, ~55 prims, booleans+blends+modifiers+scale");
    }
}
