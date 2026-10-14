//! C.1 / B.4 — shared-mesh poly instance scale harness.
//! Pure-relocated from stress_test.rs.

use crate::*;
use core_math::primitives::PbrMaterial;

// ============================================================
// TEST: C.1 / B.4 — shared-mesh poly instance scale
// ============================================================
/// Spawn `count` poly boxes that share **one** MeshCacheId (B.4), placed on a grid.
/// Used by Stress Test UI and `AZRA_INSTANCE_STRESS=N` for C.1 multi-instance measure.
pub(crate) fn spawn_shared_poly_instances(app: &mut AzraApp, count: usize) -> Vec<String> {
    use core_math::scene_graph::NodePayload;
    use std::sync::Arc;

    let mut results = vec![format!(
        "=== Instance Scale (shared mesh) count={} ===",
        count
    )];
    if count == 0 {
        results.push("count=0 — nothing to spawn".into());
        return results;
    }

    app.tool.work_mode = WorkMode::Object;
    app.poly_work_mode = PolyWorkMode::Object;
    app.modeling_context = ModelingContext::Polygonal;

    // Build the test geometry straight from the spec — this used to route through a fully
    // populated `PolyCreateAdjust` (a UI struct) just to reach the mesh builder.
    let mesh: Arc<core_math::half_edge::HalfEdgeMesh> = Arc::new(
        core_math::primitive_spec::PrimitiveSpec::Box(core_math::primitive_spec::BoxSpec {
            size: glam::Vec3::splat(0.8),
            ..Default::default()
        })
        .build(),
    );
    let root = app.scene.scene_graph.root();

    // First body: allocate GPU mesh via poly_sync
    let first = app.scene.scene_graph.add_child(
        root,
        NodePayload::PolygonalBody {
            name: "InstBox.000".into(),
            mesh: mesh.clone(),
            source: Default::default(),
            mesh_cache_id: None,
            material: PbrMaterial::default(),
            modifiers: Vec::new(),
        },
    );
    if let Some(node) = app.scene.scene_graph.get_mut(first) {
        node.local_position = Vec3::ZERO;
    }
    app.poly_sync_mesh_cache(first);
    let shared_cid = match app.scene.scene_graph.get(first).map(|n| &n.payload) {
        Some(NodePayload::PolygonalBody {
            mesh_cache_id: Some(c),
            ..
        }) => *c,
        _ => {
            results.push("FAIL: first body has no mesh_cache_id after sync".into());
            return results;
        }
    };

    let side = (count as f32).sqrt().ceil() as i32;
    let spacing = 1.2f32;
    for i in 1..count {
        let ix = (i as i32) % side;
        let iz = (i as i32) / side;
        let pos = Vec3::new(
            (ix as f32 - side as f32 * 0.5) * spacing,
            0.0,
            (iz as f32 - side as f32 * 0.5) * spacing,
        );
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: format!("InstBox.{:03}", i),
                mesh: mesh.clone(),
                source: Default::default(),
                mesh_cache_id: Some(shared_cid), // B.4 share
                material: PbrMaterial::default(),
                modifiers: Vec::new(),
            },
        );
        if let Some(node) = app.scene.scene_graph.get_mut(nid) {
            node.local_position = pos;
        }
        // B.4 path: must not COW when poly_mesh_gpu_key matches
        app.poly_sync_mesh_cache(nid);
    }

    let share = app.shared_mesh_cache_id_count();
    let users = app.mesh_cache_user_count(shared_cid);
    let unique_cids = {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        for (_, node) in app.scene.scene_graph.all_nodes() {
            if let NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            } = &node.payload
            {
                s.insert(*c);
            }
        }
        s.len()
    };

    // D1 / T1 measure: poly LOD + selection chrome must not mask instance GPU/CPU cost.
    app.cache.poly_lod_auto = false;
    app.deselect_all();
    app.modeling_context = ModelingContext::Polygonal;
    // Frame the grid so most instances stay in frustum (cull sabotage uses AZRA_NO_DRAW_SORT).
    let grid_r = (side as f32 * spacing * 0.5).max(2.0);
    app.viewport.camera.target = Vec3::ZERO;
    app.viewport.camera.distance = (grid_r * 1.6).max(4.0).min(200.0);

    app.draw_stream_stale = true;
    app.structural_draw_stale = true;
    app.ensure_structural_draw_stream();
    app.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH | DirtyFlags::RENDER);
    app.render.viewport_mode = ViewportRenderMode::Hybrid;

    results.push(format!(
        "shared_cid={shared_cid:?} users={users} unique_poly_cids={unique_cids} share_metric={share} streamN={} cam_dist={:.1}",
        app.draw_stream.len(),
        app.viewport.camera.distance
    ));
    if unique_cids == 1 && users == count {
        results.push(format!(
            "PASS B.4: {count} bodies → 1 MeshCacheId (one mesh upload)"
        ));
    } else {
        results.push(format!(
            "FAIL B.4: expected 1 cid / {count} users, got unique={unique_cids} users={users}"
        ));
    }
    results.push(format!(
        "D1/T1: scoreboard med= gpu_ms= draws= instUp= — target 100k ≤16 ms, camera-only instUp=0"
    ));
    results
}

