//! Stress test suite — dev menu diagnostic tool.
//! Accessible via Window → Stress Test or Debug menu.
//! Runs automated scene/operation/GPU/memory tests and prints a performance report.

use crate::*;
use core_math::primitives::{BlendProfile, PbrMaterial};
use std::time::Instant;

mod instance;
mod multi_mesh;
mod dense;
mod demo_scene;
mod ui;
// re-export harness entry points used by app_new / external callers
pub(crate) use instance::spawn_shared_poly_instances;
pub(crate) use multi_mesh::spawn_multi_mesh_poly_instances;
pub(crate) use dense::spawn_dense_poly_bodies;
pub(crate) use demo_scene::spawn_demo_terrain_scene;
pub(crate) use demo_scene::spawn_demo_interior_scene;
pub(crate) use demo_scene::poll_demo_tex_propagate;

// ---- Simple deterministic PRNG (LCG) ----
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self { Self(seed) }
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        ((self.0 >> 16) & 0x7fff) as u32
    }
    /// Random f32 in [lo, hi]
    fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next_u32() % 10000) as f32 / 10000.0;
        lo + t * (hi - lo)
    }
    fn random_pos(&mut self, extent: f32) -> Vec3 {
        Vec3::new(
            self.range_f32(-extent, extent),
            self.range_f32(-extent, extent),
            self.range_f32(-extent, extent),
        )
    }
}

// ============================================================
// TEST 1: Scene Stress
// ============================================================
fn run_scene_stress(app: &mut AzraApp) -> Vec<String> {
    let mut results = vec!["=== Scene Stress Test ===".to_string()];
    let tiers: &[usize] = &[10, 25, 50, 100, 200, 500];
    let mut rng = Lcg::new(42);

    app.transaction_snapshot_only("stress: scene stress");

    for &count in tiers {
        // Create prims
        for _ in 0..count {
            let uid = app.scene.next_prim_id;
            let name = app.next_name("StressS");
            let pos = rng.random_pos(5.0);
            let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, pos, uid);
            let nid = app.scene.active_body_nid;
            if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
                prims.push(prim);
            }
        }

        // Measure sync_flat_cache
        let t0 = Instant::now();
        app.sync_flat_cache();
        let sync_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // Measure serialize_from_graph
        let t1 = Instant::now();
        let _ordered = app.scene.scene_graph.all_primitives_ordered();
        let serialize_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let total = app.scene.primitives.len();
        results.push(format!(
            "{:>4} prims (total {}): sync={:.1}ms  serialize={:.1}ms",
            count, total, sync_ms, serialize_ms
        ));

        // Undo back for next tier
        app.undo();
        app.transaction_snapshot_only("stress: scene stress");
    }

    // Final undo to restore original state
    app.undo();
    results
}

// ============================================================
// TEST 2: Operation Stress
// ============================================================
fn run_operation_stress(app: &mut AzraApp) -> Vec<String> {
    let mut results = vec!["=== Operation Stress Test ===".to_string()];
    let mut rng = Lcg::new(123);

    app.transaction_snapshot_only("stress: op stress");

    // Create 20 prims
    let mut uids = Vec::new();
    for _ in 0..20 {
        let uid = app.scene.next_prim_id;
        let name = app.next_name("StressO");
        let pos = rng.random_pos(5.0);
        let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, pos, uid);
        let nid = app.scene.active_body_nid;
        if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
            prims.push(prim);
        }
        uids.push(uid);
    }
    app.sync_flat_cache();

    // -- Transform stress: 100 ops --
    let t0 = Instant::now();
    let mut max_op = 0.0f64;
    let mut min_op = f64::MAX;
    for _ in 0..100 {
        let uid = uids[rng.next_u32() as usize % uids.len()];
        if let Some(&idx) = app.scene.prim_uid_to_index.get(&uid) {
            let pos = rng.random_pos(5.0);
            let op_start = Instant::now();
            app.write_prim_position(idx, pos);
            let op_ms = op_start.elapsed().as_secs_f64() * 1000.0;
            max_op = max_op.max(op_ms);
            min_op = min_op.min(op_ms);
        }
    }
    let transform_total = t0.elapsed().as_secs_f64() * 1000.0;
    let transform_avg = transform_total / 100.0;
    if min_op == f64::MAX { min_op = 0.0; }
    results.push(format!(
        "Transform (100 ops): total={:.1}ms  avg={:.2}ms  max={:.2}ms  min={:.2}ms",
        transform_total, transform_avg, max_op, min_op
    ));

    // -- Create+Undo stress: 50 cycles --
    let t_create_total = Instant::now();
    let mut create_total = 0.0f64;
    let mut undo_total = 0.0f64;
    for _ in 0..50 {
        app.transaction_snapshot_only("stress create");
        let tc = Instant::now();
        let uid = app.scene.next_prim_id;
        let name = app.next_name("StressC");
        let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO, uid);
        let nid = app.scene.active_body_nid;
        if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
            prims.push(prim);
        }
        app.sync_flat_cache();
        create_total += tc.elapsed().as_secs_f64() * 1000.0;

        let tu = Instant::now();
        app.undo();
        undo_total += tu.elapsed().as_secs_f64() * 1000.0;
    }
    let _ = t_create_total; // total already tracked per-op
    results.push(format!(
        "Create+Undo (50 cycles): create_avg={:.2}ms  undo_avg={:.2}ms",
        create_total / 50.0, undo_total / 50.0
    ));

    // -- Duplicate/Delete stress: 20 cycles --
    // Select a prim first
    if let Some(&first_uid) = uids.first() {
        app.select_single_unified(SceneItemId::Prim(first_uid));
    }
    let mut dup_total = 0.0f64;
    let mut del_total = 0.0f64;
    let mut undo_dd_total = 0.0f64;
    for _ in 0..20 {
        app.transaction_snapshot_only("stress dup");

        let td = Instant::now();
        app.duplicate_selected();
        dup_total += td.elapsed().as_secs_f64() * 1000.0;

        let tdel = Instant::now();
        app.delete_selected();
        del_total += tdel.elapsed().as_secs_f64() * 1000.0;

        let tu = Instant::now();
        app.undo(); // restore delete
        app.undo(); // restore duplicate
        undo_dd_total += tu.elapsed().as_secs_f64() * 1000.0;
    }
    results.push(format!(
        "Duplicate/Delete (20 cycles): dup_avg={:.2}ms  del_avg={:.2}ms  undo_avg={:.2}ms",
        dup_total / 20.0, del_total / 20.0, undo_dd_total / 20.0
    ));

    // Undo back to original state
    app.undo();
    results
}

// ============================================================
// TEST 3: GPU Stress
// ============================================================
fn run_gpu_stress(app: &mut AzraApp) -> Vec<String> {
    let mut results = vec!["=== GPU Stress Test ===".to_string()];
    let tiers: &[usize] = &[10, 50, 100, 200, 500, 1000];
    let mut rng = Lcg::new(777);

    app.transaction_snapshot_only("stress: gpu stress");

    for &count in tiers {
        for _ in 0..count {
            let uid = app.scene.next_prim_id;
            let name = app.next_name("StressG");
            let pos = rng.random_pos(5.0);
            let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, pos, uid);
            let nid = app.scene.active_body_nid;
            if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
                prims.push(prim);
            }
        }
        app.sync_flat_cache();

        let total_prims = app.scene.primitives.len();
        let floats_per_prim = core_math::FLOATS_PER_PRIM;
        let buffer_bytes = total_prims * floats_per_prim * 4;
        let buffer_kb = buffer_bytes as f64 / 1024.0;

        results.push(format!(
            "{:>4} prims (total {}): buffer={:.0}KB",
            count, total_prims, buffer_kb
        ));

        app.undo();
        app.transaction_snapshot_only("stress: gpu stress");
    }

    app.undo();
    results.push(format!(
        "FLOATS_PER_PRIM = {}, bytes_per_prim = {}",
        core_math::FLOATS_PER_PRIM,
        core_math::FLOATS_PER_PRIM * 4
    ));
    results.push("Note: actual GPU frame time requires interactive testing.".to_string());
    results.push("Use the Perf HUD (F3) during manual scene building for GPU timing.".to_string());
    results
}

// ============================================================
// TEST 4: Memory Stress
// ============================================================
fn run_memory_stress(app: &mut AzraApp) -> Vec<String> {
    let mut results = vec!["=== Memory Stress Test ===".to_string()];

    // Baseline
    let base_prims = app.scene.primitives.len();
    let base_nodes = app.scene.scene_graph.node_count();
    let base_uid_map = app.scene.prim_uid_to_index.len();
    let base_body_map = app.scene.prim_body_map.len();
    let base_undo = app.history.undo_stack.len();
    let base_lights = app.scene.light_count();
    results.push(format!(
        "Baseline: prims={}, nodes={}, uid_map={}, body_map={}, undo={}, lights={}",
        base_prims, base_nodes, base_uid_map, base_body_map, base_undo, base_lights
    ));

    app.transaction_snapshot_only("stress: memory stress");

    // -- 100 create/delete cycles (5 prims each) --
    for _ in 0..100 {
        for _ in 0..5 {
            let uid = app.scene.next_prim_id;
            let name = app.next_name("StressM");
            let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO, uid);
            let nid = app.scene.active_body_nid;
            if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
                prims.push(prim);
            }
        }
        app.sync_flat_cache();
        // Delete the 5 we just added (use remove_prims_from_graph for batch removal)
        let len = app.scene.primitives.len();
        if len >= 5 {
            let indices: Vec<usize> = ((len - 5)..len).collect();
            app.remove_prims_from_graph(&indices);
        }
    }

    let after_cd_prims = app.scene.primitives.len();
    let after_cd_nodes = app.scene.scene_graph.node_count();
    let after_cd_uid = app.scene.prim_uid_to_index.len();
    let after_cd_body = app.scene.prim_body_map.len();
    results.push(format!(
        "After 100 create/delete cycles: prims={}, nodes={}, uid_map={}, body_map={}",
        after_cd_prims, after_cd_nodes, after_cd_uid, after_cd_body
    ));

    // Undo back, re-save for next phase
    app.undo();
    app.transaction_snapshot_only("stress: memory body");

    // -- 50 body create/delete cycles --
    // Actually create a new SdfBody node, add prims, then delete the body.
    // This exercises the body cascade-delete path (scene_graph.remove + sync_flat_cache).
    for cycle in 0..50 {
        use core_math::scene_graph::NodePayload;
        use core_math::primitives::BooleanOp;
        let root = app.scene.scene_graph.root();
        let body_name = format!("StressBody.{:03}", cycle);
        let mut body_prims = Vec::new();
        for j in 0..3 {
            let uid = app.scene.next_prim_id;
            let name = app.next_name("StressB");
            body_prims.push(SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO, uid));
            let _ = j;
        }
        let body_nid = app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
            name: body_name,
            primitives: body_prims,
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: Vec3::ZERO,
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
        app.sync_flat_cache();

        // Delete the body — cascade removes child prims from graph
        app.scene.scene_graph.remove(body_nid);
        app.sync_flat_cache();
    }

    let after_body_prims = app.scene.primitives.len();
    let after_body_nodes = app.scene.scene_graph.node_count();
    let after_body_uid = app.scene.prim_uid_to_index.len();
    let after_body_body = app.scene.prim_body_map.len();
    results.push(format!(
        "After 50 body create/delete: prims={}, nodes={}, uid_map={}, body_map={}",
        after_body_prims, after_body_nodes, after_body_uid, after_body_body
    ));

    // Undo back, re-save for undo cycles
    app.undo();
    app.transaction_snapshot_only("stress: memory undo");

    // -- 50 undo/redo cycles --
    for _ in 0..50 {
        app.transaction_snapshot_only("stress undo cycle");
        let uid = app.scene.next_prim_id;
        let name = app.next_name("StressU");
        let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO, uid);
        let nid = app.scene.active_body_nid;
        if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
            prims.push(prim);
        }
        app.sync_flat_cache();
        app.undo();
    }

    let after_undo_prims = app.scene.primitives.len();
    let after_undo_nodes = app.scene.scene_graph.node_count();
    let after_undo = app.history.undo_stack.len();
    results.push(format!(
        "After 50 undo/redo cycles: prims={}, nodes={}, undo={}",
        after_undo_prims, after_undo_nodes, after_undo
    ));

    // Final undo to restore original
    app.undo();

    // -- Leak detection --
    let final_prims = app.scene.primitives.len();
    let final_nodes = app.scene.scene_graph.node_count();
    let final_uid_map = app.scene.prim_uid_to_index.len();
    let final_body_map = app.scene.prim_body_map.len();

    let mut leaks = Vec::new();
    if final_prims != base_prims { leaks.push(format!("prims grew by {}", final_prims as i64 - base_prims as i64)); }
    if final_nodes != base_nodes { leaks.push(format!("nodes grew by {}", final_nodes as i64 - base_nodes as i64)); }
    if final_uid_map != base_uid_map { leaks.push(format!("uid_map grew by {}", final_uid_map as i64 - base_uid_map as i64)); }
    if final_body_map != base_body_map { leaks.push(format!("body_map grew by {}", final_body_map as i64 - base_body_map as i64)); }

    if leaks.is_empty() {
        results.push("Leaks detected: NONE".to_string());
    } else {
        for leak in &leaks {
            results.push(format!("LEAK DETECTED: {}", leak));
        }
    }

    results
}

// ============================================================
// TEST 5: Production Pipeline — "Can a real artist work in this?"
// ============================================================
fn run_production_pipeline_stress(app: &mut AzraApp) -> Vec<String> {
    use core_math::primitives::{
        BooleanOp, Modifier, ModifierType,
        DnaRule, DnaCondition, DnaSurface, DnaInput, DnaBlendMode,
    };
    use core_math::scene_graph::NodePayload;

    let mut results = vec!["=== PRODUCTION PIPELINE STRESS TEST ===".to_string()];
    let mut rng = Lcg::new(31415);

    // ── Baseline ──
    let base_prims = app.scene.primitives.len();
    let base_nodes = app.scene.scene_graph.node_count();
    let base_lights = app.scene.light_count();

    app.transaction_snapshot_only("stress: production pipeline");

    // ── PHASE 1: Save state + enable render features ──
    // Save individual render settings we'll modify
    let saved_fog = app.render.fog_enabled;
    let saved_fog_density = app.render.fog_density;
    let saved_bloom = app.render.bloom_enabled;
    let saved_bloom_int = app.render.bloom_intensity;
    let saved_ssr = app.render.ssr_enabled;
    let saved_denoise = app.render.pt_denoise;
    let saved_quality = app.render.render_quality;
    let saved_res_scale = app.render.resolution_scale;
    let saved_upsample = app.render.upsample_enabled;
    let saved_comp_enabled = app.compositor.enabled;
    let saved_comp_effects_len = app.compositor.effects.len();

    app.render.fog_enabled = true;
    app.render.fog_density = 0.05;
    app.render.bloom_enabled = true;
    app.render.bloom_intensity = 0.8;
    app.render.ssr_enabled = true;
    app.render.pt_denoise = true;
    app.render.render_quality = 2; // high
    app.render.resolution_scale = 1.0;
    app.render.upsample_enabled = false;

    // Enable compositor with bloom + vignette + color grading
    app.compositor.enabled = true;
    if app.compositor.effects.is_empty() {
        use crate::types::{CompositorEffect, CompositorEffectType};
        app.compositor.effects.push(CompositorEffect::new(CompositorEffectType::Bloom));
        app.compositor.effects.push(CompositorEffect::new(CompositorEffectType::Vignette));
        app.compositor.effects.push(CompositorEffect::new(CompositorEffectType::ColorGrading));
    } else {
        for e in &mut app.compositor.effects {
            e.enabled = true;
        }
    }

    app.mark_dirty(DirtyFlags::RENDER);
    results.push(String::new());
    results.push("Render: fog=ON bloom=ON SSR=ON denoise=ON compositor=ON".to_string());

    // ── PHASE 2: Build the scene ──
    results.push(String::new());
    results.push("Scene build (cumulative timing):".to_string());

    // ═══ STAGE A: SDF Geometry (100 prims, 5 bodies) ═══
    let root = app.scene.scene_graph.root();
    let shapes_for_body = |rng: &mut Lcg, count: usize, shape_fn: &dyn Fn(&mut Lcg, usize) -> SdfShape,
                           op_fn: &dyn Fn(&mut Lcg, usize) -> BooleanOp,
                           extent: f32| -> Vec<SdfPrimitive> {
        let mut prims = Vec::new();
        for i in 0..count {
            let uid = rng.next_u32();
            let name = format!("SP.{:04}", uid % 10000);
            let pos = rng.random_pos(extent);
            let rot = Vec3::new(
                rng.range_f32(0.0, 360.0),
                rng.range_f32(0.0, 360.0),
                rng.range_f32(0.0, 360.0),
            );
            let scale = Vec3::new(
                rng.range_f32(0.3, 2.0),
                rng.range_f32(0.3, 2.0),
                rng.range_f32(0.3, 2.0),
            );
            let mut p = SdfPrimitive::with_uid(name, shape_fn(rng, i), pos, uid);
            p.rotation_deg = rot;
            p.scale = scale;
            p.operation = op_fn(rng, i);
            prims.push(p);
        }
        prims
    };

    let ta = Instant::now();

    // Body 1: Hero hard-surface (30 prims)
    let body1_prims = shapes_for_body(&mut rng, 30,
        &|rng, i| match i % 4 {
            0 => SdfShape::Box { half_extents: Vec3::new(
                    rng.range_f32(0.2, 0.8), rng.range_f32(0.2, 0.8), rng.range_f32(0.2, 0.8)),
                    bevel: BoxBevel::default() },
            1 => SdfShape::Cylinder { radius: rng.range_f32(0.2, 0.5), half_height: rng.range_f32(0.3, 0.8) },
            2 => SdfShape::Sphere { radius: rng.range_f32(0.2, 0.6) },
            _ => SdfShape::Capsule { radius: rng.range_f32(0.1, 0.3), half_height: rng.range_f32(0.3, 0.6) },
        },
        &|rng, i| match i % 4 {
            0 => BooleanOp::Subtract { k: 0.0 },
            1 => BooleanOp::Union { k: 0.2 },
            2 => BooleanOp::Intersect,
            _ => BooleanOp::Union { k: rng.range_f32(0.3, 0.5) },
        },
        3.0,
    );
    app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
        name: "Hero Hard-Surface".into(),
        primitives: body1_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 },
        origin: Vec3::ZERO,
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

    // Body 2: Organic cluster (25 spheres, smooth union, tight cluster)
    let body2_prims = shapes_for_body(&mut rng, 25,
        &|rng, _| SdfShape::Sphere { radius: rng.range_f32(0.3, 0.7) },
        &|rng, _| BooleanOp::Union { k: rng.range_f32(0.3, 0.7) },
        2.0,
    );
    app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
        name: "Organic Cluster".into(),
        primitives: body2_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 },
        origin: Vec3::new(5.0, 0.0, 0.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // Body 3: Environment (20 prims: 15 boxes + 5 sphere subtracts)
    let body3_prims = shapes_for_body(&mut rng, 20,
        &|rng, i| if i < 15 {
            SdfShape::Box { half_extents: Vec3::new(
                rng.range_f32(0.5, 2.0), rng.range_f32(0.5, 2.0), rng.range_f32(0.5, 2.0)),
                bevel: BoxBevel::default() }
        } else {
            SdfShape::Sphere { radius: rng.range_f32(0.3, 0.8) }
        },
        &|_, i| if i < 15 { BooleanOp::Union { k: 0.0 } } else { BooleanOp::Subtract { k: 0.0 } },
        5.0,
    );
    app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
        name: "Environment".into(),
        primitives: body3_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 },
        origin: Vec3::new(-5.0, 0.0, 0.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // Body 4: Details (15 mixed prims, small scale, sharp booleans)
    let body4_prims = shapes_for_body(&mut rng, 15,
        &|rng, i| match i % 5 {
            0 => SdfShape::Cone { radius: rng.range_f32(0.1, 0.3), half_height: rng.range_f32(0.2, 0.5) },
            1 => SdfShape::Torus { major_radius: rng.range_f32(0.2, 0.4), minor_radius: rng.range_f32(0.05, 0.1) },
            2 => SdfShape::Capsule { radius: rng.range_f32(0.05, 0.15), half_height: rng.range_f32(0.1, 0.3) },
            3 => SdfShape::Box { half_extents: Vec3::splat(rng.range_f32(0.1, 0.3)), bevel: BoxBevel::default() },
            _ => SdfShape::Sphere { radius: rng.range_f32(0.1, 0.25) },
        },
        &|_, i| if i % 3 == 0 { BooleanOp::Subtract { k: 0.0 } } else { BooleanOp::Union { k: 0.0 } },
        2.0,
    );
    app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
        name: "Details".into(),
        primitives: body4_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 },
        origin: Vec3::new(0.0, 0.0, 5.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // Body 5: Nested assembly — Body5A (5 smooth union) + Body5B (5 sharp subtract)
    let body5a_prims = shapes_for_body(&mut rng, 5,
        &|rng, _| SdfShape::Sphere { radius: rng.range_f32(0.2, 0.5) },
        &|_, _| BooleanOp::Union { k: 0.3 },
        1.5,
    );
    let body5b_prims = shapes_for_body(&mut rng, 5,
        &|rng, _| SdfShape::Box { half_extents: Vec3::splat(rng.range_f32(0.15, 0.35)), bevel: BoxBevel::default() },
        &|_, _| BooleanOp::Subtract { k: 0.0 },
        1.5,
    );
    let body5_nid = app.scene.scene_graph.add_child(root, NodePayload::SdfBody {
        name: "Nested Assembly".into(),
        primitives: Vec::new(),
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 },
        origin: Vec3::new(0.0, 0.0, -5.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
    app.scene.scene_graph.add_child(body5_nid, NodePayload::SdfBody {
        name: "Nested-A".into(),
        primitives: body5a_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.2 },
        origin: Vec3::new(0.5, 0.0, 0.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});
    app.scene.scene_graph.add_child(body5_nid, NodePayload::SdfBody {
        name: "Nested-B".into(),
        primitives: body5b_prims,
        mesh_cache_id: None,
        body_op: BooleanOp::Subtract { k: 0.0 },
        origin: Vec3::new(-0.5, 0.0, 0.0),
       
        body_rotation: glam::Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(),
        material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    // Sync flat cache + measure
    let ta_sync = Instant::now();
    app.sync_flat_cache();
    let sync_a_ms = ta_sync.elapsed().as_secs_f64() * 1000.0;
    let ser_a = Instant::now();
    let _ = app.scene.scene_graph.all_primitives_ordered();
    let ser_a_ms = ser_a.elapsed().as_secs_f64() * 1000.0;
    let prim_count = app.scene.primitives.len() - base_prims;
    let build_a_ms = ta.elapsed().as_secs_f64() * 1000.0;
    results.push(format!(
        "  A. SDF geometry ({} prims, 7 bodies):     build={:.0}ms sync={:.1}ms ser={:.1}ms",
        prim_count, build_a_ms, sync_a_ms, ser_a_ms
    ));

    // ═══ STAGE B: Polygon Meshes (10 objects) ═══
    let tb = Instant::now();
    let poly_types = ["box", "sphere", "cylinder", "plane", "torus", "cone"];
    for i in 0..10 {
        let gen = poly_types[i % poly_types.len()];
        app.create_poly_mesh(gen);
    }
    let sync_b = Instant::now();
    app.sync_flat_cache();
    let sync_b_ms = sync_b.elapsed().as_secs_f64() * 1000.0;
    let build_b_ms = tb.elapsed().as_secs_f64() * 1000.0;
    results.push(format!(
        "  B. + Poly meshes (10 objects):              build={:.0}ms sync={:.1}ms",
        build_b_ms, sync_b_ms
    ));

    // ═══ STAGE C: Modifiers on 20 SDF prims ═══
    let tc = Instant::now();
    let modifier_types = [
        (ModifierType::Twist,    0.8_f32, 0.5_f32),
        (ModifierType::Taper,    0.6,     0.7),
        (ModifierType::Smooth,   0.05,    0.0),
        (ModifierType::Displace, 0.15,    3.0),
        (ModifierType::Shear,    0.4,     1.0),
    ];
    let total_prims_now = app.scene.primitives.len();
    let mut modified_count = 0u32;
    for i in 0..20 {
        let idx = i % total_prims_now;
        if idx >= app.scene.primitives.len() { break; }
        let (mt, str_base, p2_base) = modifier_types[i % modifier_types.len()];
        let strength = str_base + rng.range_f32(-0.2, 0.2);
        let param2 = p2_base + rng.range_f32(-0.1, 0.1);
        let axis = (i % 3) as u8;
        app.write_prim(idx, |p| {
            p.modifiers.push(Modifier {
                mod_type: mt,
                strength: strength.max(0.01),
                axis,
                param2: param2.max(0.01),
                mask: core_math::primitives::VolumeMask::default(),
                height_map: None,
            });
            p.sync_modifiers_to_gpu_fields();
        });
        modified_count += 1;
    }
    let sync_c = Instant::now();
    app.sync_flat_cache();
    let sync_c_ms = sync_c.elapsed().as_secs_f64() * 1000.0;
    let build_c_ms = tc.elapsed().as_secs_f64() * 1000.0;
    results.push(format!(
        "  C. + Modifiers ({} prims):                  build={:.0}ms sync={:.1}ms",
        modified_count, build_c_ms, sync_c_ms
    ));

    // ═══ STAGE D: Displacement on 20 prims (via Displace modifier) ═══
    let td = Instant::now();
    let mut displace_count = 0u32;
    for i in 0..20 {
        let idx = (i + 5) % total_prims_now; // offset from modifier prims
        if idx >= app.scene.primitives.len() { break; }
        let freq = rng.range_f32(1.0, 5.0);
        let amount = rng.range_f32(0.02, 0.15);
        app.write_prim(idx, |p| {
            p.modifiers.push(Modifier {
                mod_type: ModifierType::Displace,
                strength: amount,
                axis: 0,
                param2: freq,
                mask: core_math::primitives::VolumeMask::default(),
                height_map: None,
            });
            p.sync_modifiers_to_gpu_fields();
        });
        displace_count += 1;
    }
    let sync_d = Instant::now();
    app.sync_flat_cache();
    let sync_d_ms = sync_d.elapsed().as_secs_f64() * 1000.0;
    let build_d_ms = td.elapsed().as_secs_f64() * 1000.0;
    let floats_per_prim = core_math::FLOATS_PER_PRIM;
    let gpu_buf_kb = (app.scene.primitives.len() * floats_per_prim * 4) as f64 / 1024.0;
    results.push(format!(
        "  D. + Displacement ({} prims):                build={:.0}ms sync={:.1}ms buf={:.0}KB",
        displace_count, build_d_ms, sync_d_ms, gpu_buf_kb
    ));

    // ═══ STAGE E: Materials + DNA rules ═══
    let te = Instant::now();
    // 20 material presets applied round-robin
    let mat_presets: Vec<(&str, Vec3, f32, f32)> = vec![
        // Metals
        ("steel",    Vec3::new(0.78, 0.78, 0.78), 0.15, 1.0),
        ("copper",   Vec3::new(0.95, 0.64, 0.54), 0.20, 1.0),
        ("gold",     Vec3::new(1.0,  0.84, 0.0),  0.10, 1.0),
        ("titanium", Vec3::new(0.54, 0.50, 0.50), 0.35, 1.0),
        // Plastics
        ("red",      Vec3::new(0.9,  0.1,  0.1),  0.40, 0.0),
        ("blue",     Vec3::new(0.1,  0.2,  0.9),  0.35, 0.0),
        ("white",    Vec3::new(0.95, 0.95, 0.95), 0.50, 0.0),
        ("black",    Vec3::new(0.05, 0.05, 0.05), 0.55, 0.0),
        // Rough
        ("concrete", Vec3::new(0.6,  0.58, 0.55), 0.85, 0.0),
        ("wood",     Vec3::new(0.55, 0.35, 0.15), 0.75, 0.0),
        ("stone",    Vec3::new(0.5,  0.48, 0.45), 0.90, 0.0),
        ("clay",     Vec3::new(0.66, 0.58, 0.53), 0.65, 0.0),
        // Glass-like
        ("tint_r",   Vec3::new(0.9,  0.5,  0.5),  0.05, 0.0),
        ("tint_g",   Vec3::new(0.5,  0.9,  0.5),  0.08, 0.0),
        ("tint_b",   Vec3::new(0.5,  0.5,  0.9),  0.10, 0.0),
        ("tint_w",   Vec3::new(0.9,  0.9,  0.9),  0.12, 0.0),
        // Mixed
        ("mix1",     Vec3::new(0.7,  0.3,  0.2),  0.45, 0.4),
        ("mix2",     Vec3::new(0.3,  0.6,  0.7),  0.50, 0.5),
        ("mix3",     Vec3::new(0.8,  0.7,  0.2),  0.40, 0.3),
        ("mix4",     Vec3::new(0.4,  0.4,  0.6),  0.55, 0.6),
    ];

    for idx in 0..total_prims_now.min(app.scene.primitives.len()) {
        let (_, albedo, rough, metal) = mat_presets[idx % mat_presets.len()];
        app.write_prim(idx, |p| {
            p.material.albedo = albedo;
            p.material.roughness = rough;
            p.material.metallic = metal;
        });
    }

    // DNA rules on 50 prims
    let mut dna_total_rules = 0u32;
    // Edge Wear on 20 prims
    for i in 0..20.min(total_prims_now) {
        app.write_prim(i, |p| {
            p.material_dna.rules.push(DnaRule {
                name: "Edge Wear".into(),
                condition: DnaCondition::GreaterThan {
                    input: DnaInput::Curvature,
                    threshold: 0.3,
                    falloff: 0.2,
                },
                surface: DnaSurface {
                    albedo: Vec3::new(0.85, 0.75, 0.65),
                    roughness: 0.3,
                    metalness: 0.8,
                    en_albedo: true,
                    en_roughness: true,
                    en_metalness: true,
                    ..DnaSurface::default()
                },
                blend_mode: DnaBlendMode::Normal,
                intensity: 0.5 + (i as f32) * 0.02,
                visible: true,
                ..DnaRule::default()
            });
        });
        dna_total_rules += 1;
    }
    // Cavity Dirt on 15 prims
    for i in 20..35.min(total_prims_now) {
        app.write_prim(i, |p| {
            p.material_dna.rules.push(DnaRule {
                name: "Cavity Dirt".into(),
                condition: DnaCondition::LessThan {
                    input: DnaInput::CavityDepth,
                    threshold: 0.4,
                    falloff: 0.15,
                },
                surface: DnaSurface {
                    albedo: Vec3::new(0.15, 0.12, 0.08),
                    roughness: 0.9,
                    en_albedo: true,
                    en_roughness: true,
                    ..DnaSurface::default()
                },
                blend_mode: DnaBlendMode::Multiply,
                intensity: 0.5,
                visible: true,
                ..DnaRule::default()
            });
        });
        dna_total_rules += 1;
    }
    // Noise Pattern on 10 prims
    for i in 35..45.min(total_prims_now) {
        app.write_prim(i, |p| {
            p.material_dna.rules.push(DnaRule {
                name: "Noise Pattern".into(),
                condition: DnaCondition::NoiseThreshold {
                    scale: 3.0 + (i as f32) * 0.5,
                    threshold: 0.5,
                    falloff: 0.2,
                },
                surface: DnaSurface {
                    albedo: Vec3::new(0.3, 0.25, 0.2),
                    roughness: 0.7,
                    en_albedo: true,
                    en_roughness: true,
                    ..DnaSurface::default()
                },
                blend_mode: DnaBlendMode::Normal,
                intensity: 0.6,
                visible: true,
                ..DnaRule::default()
            });
        });
        dna_total_rules += 1;
    }
    // 5 prims with 2 rules each (Edge Wear + Cavity Dirt)
    for i in 45..50.min(total_prims_now) {
        app.write_prim(i, |p| {
            p.material_dna.rules.push(DnaRule {
                name: "Edge Wear".into(),
                condition: DnaCondition::GreaterThan {
                    input: DnaInput::Curvature,
                    threshold: 0.25,
                    falloff: 0.15,
                },
                surface: DnaSurface {
                    albedo: Vec3::new(0.9, 0.85, 0.75),
                    roughness: 0.25,
                    metalness: 0.9,
                    en_albedo: true, en_roughness: true, en_metalness: true,
                    ..DnaSurface::default()
                },
                intensity: 0.7,
                visible: true,
                ..DnaRule::default()
            });
            p.material_dna.rules.push(DnaRule {
                name: "Cavity Dirt".into(),
                condition: DnaCondition::LessThan {
                    input: DnaInput::CavityDepth,
                    threshold: 0.5,
                    falloff: 0.2,
                },
                surface: DnaSurface {
                    albedo: Vec3::new(0.1, 0.08, 0.05),
                    roughness: 0.95,
                    en_albedo: true, en_roughness: true,
                    ..DnaSurface::default()
                },
                blend_mode: DnaBlendMode::Multiply,
                intensity: 0.6,
                visible: true,
                ..DnaRule::default()
            });
        });
        dna_total_rules += 2;
    }

    let sync_e = Instant::now();
    app.sync_flat_cache();
    let sync_e_ms = sync_e.elapsed().as_secs_f64() * 1000.0;
    let gpu_buf_kb_e = (app.scene.primitives.len() * floats_per_prim * 4) as f64 / 1024.0;
    let build_e_ms = te.elapsed().as_secs_f64() * 1000.0;
    results.push(format!(
        "  E. + Materials (20) + DNA ({} rules on 50 prims): build={:.0}ms sync={:.1}ms buf={:.0}KB",
        dna_total_rules, build_e_ms, sync_e_ms, gpu_buf_kb_e
    ));

    // ═══ STAGE F: 30 Lights ═══
    let tf = Instant::now();
    // Add lights directly to graph (avoid 30x save_undo from add_light)
    let mut light_count_added = 0u32;
    let mut shadow_count = 0u32;

    // 1 directional (key)
    {
        let id = app.scene.next_light_id; app.scene.next_light_id += 1;
        let mut l = LightData::default();
        l.id = id;
        l.name = format!("Sun.{:03}", id);
        l.azimuth = 45.0;
        l.elevation = -35.0;
        l.intensity = 1.5;
        l.color = [1.0, 0.95, 0.85];
        l.shadow_softness = 8.0;
        app.scene.scene_graph.add_child(root, NodePayload::Light(l));
        light_count_added += 1;
        shadow_count += 1;
    }

    // 10 point lights (5 shadow, 5 no-shadow)
    let point_colors: &[[f32;3]] = &[
        [1.0, 0.8, 0.6], [0.6, 0.8, 1.0], [1.0, 0.5, 0.3],
        [0.5, 1.0, 0.7], [0.9, 0.7, 1.0],
        [1.0, 0.95, 0.9], [0.8, 0.9, 1.0], [1.0, 0.85, 0.7],
        [0.7, 1.0, 0.9], [0.95, 0.9, 1.0],
    ];
    for i in 0..10 {
        let id = app.scene.next_light_id; app.scene.next_light_id += 1;
        let pos = Vec3::new(
            rng.range_f32(-8.0, 8.0),
            rng.range_f32(1.0, 6.0),
            rng.range_f32(-8.0, 8.0),
        );
        let mut l = LightData::new_point(id, format!("Point.{:03}", id), pos);
        l.color = point_colors[i];
        if i < 5 {
            l.intensity = rng.range_f32(1.0, 3.0);
            l.shadow_softness = rng.range_f32(5.0, 15.0);
            shadow_count += 1;
        } else {
            l.intensity = rng.range_f32(0.1, 0.5);
        }
        app.scene.scene_graph.add_child(root, NodePayload::Light(l));
        light_count_added += 1;
    }

    // 10 spot lights (3 shadow, 7 no-shadow)
    for i in 0..10 {
        let id = app.scene.next_light_id; app.scene.next_light_id += 1;
        let pos = Vec3::new(
            rng.range_f32(-6.0, 6.0),
            rng.range_f32(3.0, 8.0),
            rng.range_f32(-6.0, 6.0),
        );
        let mut l = LightData::new_spot(id, format!("Spot.{:03}", id), pos);
        l.elevation = rng.range_f32(-60.0, -20.0);
        l.azimuth = rng.range_f32(0.0, 360.0);
        if i < 3 {
            l.intensity = rng.range_f32(2.0, 4.0);
            l.spot_inner_deg = rng.range_f32(10.0, 20.0);
            l.spot_outer_deg = rng.range_f32(25.0, 45.0);
            l.shadow_softness = rng.range_f32(5.0, 12.0);
            shadow_count += 1;
        } else {
            l.intensity = rng.range_f32(0.3, 1.0);
            l.spot_inner_deg = rng.range_f32(20.0, 40.0);
            l.spot_outer_deg = rng.range_f32(45.0, 60.0);
        }
        app.scene.scene_graph.add_child(root, NodePayload::Light(l));
        light_count_added += 1;
    }

    // 9 point lights (far fill, no shadow, very low intensity)
    for _ in 0..9 {
        let id = app.scene.next_light_id; app.scene.next_light_id += 1;
        let pos = Vec3::new(
            rng.range_f32(-15.0, 15.0),
            rng.range_f32(2.0, 10.0),
            rng.range_f32(-15.0, 15.0),
        );
        let mut l = LightData::new_point(id, format!("Fill.{:03}", id), pos);
        l.intensity = rng.range_f32(0.05, 0.2);
        l.range = 20.0;
        app.scene.scene_graph.add_child(root, NodePayload::Light(l));
        light_count_added += 1;
    }

    // Lights are graph-native — no cache to sync.
    app.mark_dirty(DirtyFlags::LIGHT);
    let sync_f_ms = tf.elapsed().as_secs_f64() * 1000.0;
    results.push(format!(
        "  F. + Lights ({}, {} shadow):                build={:.0}ms",
        light_count_added, shadow_count, sync_f_ms
    ));

    // ── PHASE 3: Final scene stats ──
    let final_prims = app.scene.primitives.len() - base_prims;
    let final_lights = app.scene.light_count() - base_lights;
    let gpu_buf_final = (app.scene.primitives.len() * floats_per_prim * 4) as f64 / 1024.0;

    // Estimated GPU ops
    let sdf_ops = final_prims * 12 + 25 * 5; // smooth-blend prims from organic cluster
    let modifier_ops = modified_count as usize * 15;
    let displacement_ops = displace_count as usize * 8;
    let material_ops = dna_total_rules as usize * 8;
    let lighting_ops = final_lights * 15 + shadow_count as usize * 30;
    let total_ops = sdf_ops + modifier_ops + displacement_ops + material_ops + lighting_ops;

    results.push(String::new());
    results.push("Final scene:".to_string());
    results.push(format!(
        "  SDF prims: {} | Poly meshes: 10 | Bodies: 7 (2 nested)",
        final_prims
    ));
    results.push(format!(
        "  Modifiers: {} prims | Displacement: {} prims",
        modified_count, displace_count
    ));
    results.push(format!(
        "  Materials: 20 unique | DNA rules: {} total on 50 prims",
        dna_total_rules
    ));
    results.push(format!(
        "  Lights: {} ({} shadow-casting)",
        final_lights, shadow_count
    ));
    results.push(format!("  GPU buffer: {:.0}KB", gpu_buf_final));
    results.push(String::new());
    results.push("Estimated GPU cost per pixel:".to_string());
    results.push(format!(
        "  SDF eval: ~{} ops ({} prims + blends + modifiers + displacement)",
        sdf_ops + modifier_ops + displacement_ops, final_prims
    ));
    results.push(format!(
        "  Material/DNA: ~{} ops ({} rules x 8)",
        material_ops, dna_total_rules
    ));
    results.push(format!(
        "  Lighting: ~{} ops ({} lights + {} shadows)",
        lighting_ops, final_lights, shadow_count
    ));
    results.push("  Post-process: fog + denoise + bloom + vignette + color_grade + SSR".to_string());
    results.push(format!("  TOTAL: ~{} ops/pixel", total_ops));

    // ── PHASE 4: Interactive performance under full load ──
    results.push(String::new());
    results.push("Interactive under full load:".to_string());

    let current_prims = app.scene.primitives.len();

    // 4.1 — 20 SDF prim transforms
    {
        let mut t_total = 0.0f64;
        let mut t_max = 0.0f64;
        let mut t_min = f64::MAX;
        for i in 0..20 {
            let idx = i % current_prims;
            if idx >= app.scene.primitives.len() { break; }
            let pos = rng.random_pos(5.0);
            let rot = Vec3::new(rng.range_f32(0.0, 360.0), rng.range_f32(0.0, 360.0), rng.range_f32(0.0, 360.0));
            let scale = Vec3::new(rng.range_f32(0.5, 1.5), rng.range_f32(0.5, 1.5), rng.range_f32(0.5, 1.5));
            let t0 = Instant::now();
            app.write_prim_transform(idx, pos, rot, scale);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            t_total += ms;
            t_max = t_max.max(ms);
            t_min = t_min.min(ms);
        }
        if t_min == f64::MAX { t_min = 0.0; }
        results.push(format!(
            "  SDF transform (20):  avg={:.2}ms  max={:.2}ms  min={:.2}ms",
            t_total / 20.0, t_max, t_min
        ));
    }

    // 4.2 — 10 material changes
    {
        let mut t_total = 0.0f64;
        let mut t_max = 0.0f64;
        for i in 0..10 {
            let idx = (i * 7) % current_prims;
            if idx >= app.scene.primitives.len() { break; }
            let r = rng.range_f32(0.1, 0.9);
            let t0 = Instant::now();
            app.write_prim(idx, |p| {
                p.material.roughness = r;
                p.material.albedo = Vec3::new(r, 1.0 - r, 0.5);
            });
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            t_total += ms;
            t_max = t_max.max(ms);
        }
        results.push(format!(
            "  Material edit (10):  avg={:.2}ms  max={:.2}ms",
            t_total / 10.0, t_max
        ));
    }

    // 4.3 — 5 modifier param changes
    {
        let mut t_total = 0.0f64;
        let mut t_max = 0.0f64;
        for i in 0..5 {
            let idx = i % current_prims;
            if idx >= app.scene.primitives.len() { break; }
            let s = rng.range_f32(0.1, 1.5);
            let t0 = Instant::now();
            app.write_prim(idx, |p| {
                if let Some(m) = p.modifiers.first_mut() {
                    m.strength = s;
                }
            });
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            t_total += ms;
            t_max = t_max.max(ms);
        }
        results.push(format!(
            "  Modifier edit (5):   avg={:.2}ms  max={:.2}ms",
            t_total / 5.0, t_max
        ));
    }

    // 4.4 — 5 structural ops (2 creates, 2 deletes, 1 undo)
    {
        let mut t_total = 0.0f64;
        app.transaction_snapshot_only("stress: structural");

        // 2 creates
        for _ in 0..2 {
            let t0 = Instant::now();
            let uid = app.scene.next_prim_id;
            let name = app.next_name("StressP");
            let prim = SdfPrimitive::with_uid(name, SdfShape::Sphere { radius: 0.5 }, rng.random_pos(3.0), uid);
            let nid = app.scene.active_body_nid;
            if let Some(prims) = app.scene.scene_graph.body_primitives_mut(nid) {
                prims.push(prim);
            }
            app.sync_flat_cache();
            t_total += t0.elapsed().as_secs_f64() * 1000.0;
        }

        // 2 deletes
        for _ in 0..2 {
            let len = app.scene.primitives.len();
            if len > 1 {
                let t0 = Instant::now();
                app.remove_prim_from_graph(len - 1);
                t_total += t0.elapsed().as_secs_f64() * 1000.0;
            }
        }

        // 1 undo
        {
            let t0 = Instant::now();
            app.undo();
            t_total += t0.elapsed().as_secs_f64() * 1000.0;
        }

        results.push(format!(
            "  Structural ops (5):  avg={:.2}ms",
            t_total / 5.0
        ));
    }

    // 4.5 — 5 undo operations
    {
        // Push 5 undo states, then undo them
        for _ in 0..5 {
            app.transaction_snapshot_only("stress undo perf");
            let idx = 0.min(app.scene.primitives.len().saturating_sub(1));
            if idx < app.scene.primitives.len() {
                app.write_prim(idx, |p| p.material.roughness += 0.01);
            }
        }
        let mut t_total = 0.0f64;
        let mut t_max = 0.0f64;
        for _ in 0..5 {
            let t0 = Instant::now();
            app.undo();
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            t_total += ms;
            t_max = t_max.max(ms);
        }
        results.push(format!(
            "  Undo (5):            avg={:.2}ms  max={:.2}ms",
            t_total / 5.0, t_max
        ));
    }

    // 4.6 — 5 light property changes
    {
        let mut t_total = 0.0f64;
        let mut t_max = 0.0f64;
        let light_ids: Vec<u32> = app.scene.light_ids();
        if light_ids.is_empty() { /* skip */ } else {
        for i in 0..5 {
            let lid = light_ids[i % light_ids.len()];
            let pos = rng.random_pos(8.0);
            let intensity = rng.range_f32(0.5, 3.0);
            let t0 = Instant::now();
            app.write_light(lid, |l| {
                l.position = pos;
                l.intensity = intensity;
            });
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            t_total += ms;
            t_max = t_max.max(ms);
        }
        results.push(format!(
            "  Light edit (5):      avg={:.2}ms  max={:.2}ms",
            t_total / 5.0, t_max
        ));
    }} // close if light_ids.is_empty() else + outer block

    // ── PHASE 5: Cleanup ──
    results.push(String::new());

    // Restore render settings
    app.render.fog_enabled = saved_fog;
    app.render.fog_density = saved_fog_density;
    app.render.bloom_enabled = saved_bloom;
    app.render.bloom_intensity = saved_bloom_int;
    app.render.ssr_enabled = saved_ssr;
    app.render.pt_denoise = saved_denoise;
    app.render.render_quality = saved_quality;
    app.render.resolution_scale = saved_res_scale;
    app.render.upsample_enabled = saved_upsample;
    app.compositor.enabled = saved_comp_enabled;
    // Remove compositor effects we added (if we added them)
    if saved_comp_effects_len == 0 {
        app.compositor.effects.clear();
    }
    app.mark_dirty(DirtyFlags::RENDER);

    // Undo ALL to original state
    app.undo();

    let final_p = app.scene.primitives.len();
    let final_n = app.scene.scene_graph.node_count();
    let final_l = app.scene.light_count();

    let p_ok = final_p == base_prims;
    let n_ok = final_n == base_nodes;
    let l_ok = final_l == base_lights;

    results.push(format!(
        "Cleanup: prims={}  nodes={}  lights={}",
        if p_ok { "OK" } else { "LEAK" },
        if n_ok { "OK" } else { "LEAK" },
        if l_ok { "OK" } else { "LEAK" },
    ));
    if !p_ok { results.push(format!("  prims: {} -> {} (delta {})", base_prims, final_p, final_p as i64 - base_prims as i64)); }
    if !n_ok { results.push(format!("  nodes: {} -> {} (delta {})", base_nodes, final_n, final_n as i64 - base_nodes as i64)); }
    if !l_ok { results.push(format!("  lights: {} -> {} (delta {})", base_lights, final_l, final_l as i64 - base_lights as i64)); }

    // Verdict
    results.push(String::new());
    // Re-compute average transform time for verdict
    let mut verdict_transform_avg = 0.0f64;
    {
        let cp = app.scene.primitives.len();
        if cp > 0 {
            let mut rng2 = Lcg::new(9999);
            let t0 = Instant::now();
            for i in 0..10 {
                let idx = i % cp;
                if idx < app.scene.primitives.len() {
                    app.write_prim_position(idx, rng2.random_pos(3.0));
                }
            }
            verdict_transform_avg = t0.elapsed().as_secs_f64() * 1000.0 / 10.0;
        }
    }

    let verdict = if verdict_transform_avg < 1.0 {
        "Transform < 1ms:  Production-ready"
    } else if verdict_transform_avg < 5.0 {
        "Transform 1-5ms:  Usable with minor lag"
    } else if verdict_transform_avg < 16.0 {
        "Transform 5-16ms: Sub-60fps during interaction"
    } else {
        "Transform > 16ms: Not interactive"
    };
    results.push(format!("=== VERDICT: {} ===", verdict));

    results
}

