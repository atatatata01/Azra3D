//! C.6 — multi-mesh instance gate harness (N meshes × M instances).
//! Pure-relocated from stress_test.rs.

use crate::*;
use core_math::primitives::PbrMaterial;

// ============================================================
// C.6 — multi-mesh instance gate (N meshes × M instances, interleaved)
// ============================================================

/// Spawn **N distinct** poly meshes with **M instances each**, interleaved so that
/// without draw-list sort consecutive batching would issue N×M draws.
///
/// C.6 gate (pure, also in `raster_common::count_mesh_instance_draws` tests):
/// after sort, submitted mesh draws ≈ N. Runtime: check scoreboard `draws=` ≈ N after a frame
/// (all instances in view, no frustum cull of whole groups).
///
/// `AZRA_MULTI_MESH_STRESS=5x200` or Stress Test UI.
pub(crate) fn spawn_multi_mesh_poly_instances(
    app: &mut AzraApp,
    n_meshes: usize,
    m_each: usize,
) -> Vec<String> {
    use core_math::scene_graph::NodePayload;
    use std::sync::Arc;

    let mut results = vec![format!(
        "=== C.6 Multi-Mesh Instance Gate N={} meshes × M={} each ===",
        n_meshes, m_each
    )];
    if n_meshes == 0 || m_each == 0 {
        results.push("FAIL: N and M must be > 0".into());
        return results;
    }

    app.tool.work_mode = WorkMode::Object;
    app.poly_work_mode = PolyWorkMode::Object;
    app.modeling_context = ModelingContext::Polygonal;

    let kinds = ["box", "sphere", "cylinder", "cone", "torus"];
    let root = app.scene.scene_graph.root();
    let mut master_cids: Vec<core_math::scene_graph::MeshCacheId> = Vec::with_capacity(n_meshes);
    let mut master_meshes: Vec<Arc<core_math::half_edge::HalfEdgeMesh>> =
        Vec::with_capacity(n_meshes);
    let mut master_mats: Vec<PbrMaterial> = Vec::with_capacity(n_meshes);
    let mut master_nids: Vec<core_math::scene_graph::NodeId> = Vec::with_capacity(n_meshes);

    // Create N masters with distinct geometry (different kind/radius → distinct MeshCacheId).
    for k in 0..n_meshes {
        let kind = kinds[k % kinds.len()];
        // Vary a size parameter per master so each mesh gets a distinct `content_signature`
        // even when two masters share a kind — that is what this gate is actually testing.
        let bump = 0.05 * (k as f32);
        let Some(mut spec) = core_math::primitive_spec::PrimitiveSpec::from_kind_id(kind) else {
            results.push(format!("FAIL: unknown primitive kind '{kind}'"));
            return results;
        };
        {
            use core_math::primitive_spec::PrimitiveSpec as P;
            match &mut spec {
                P::Box(s) => s.size = glam::Vec3::splat(0.7 + bump),
                P::UvSphere(s) => s.radius = 0.35 + bump,
                P::Cylinder(s) => s.radius = 0.35 + bump,
                P::Cone(s) => s.radius_bottom = 0.35 + bump,
                P::Torus(s) => s.major_r = 0.35 + bump,
                _ => {}
            }
        }
        let mesh: Arc<core_math::half_edge::HalfEdgeMesh> = Arc::new(spec.build());
        let mut mat = PbrMaterial::default();
        // Distinct albedo so content-keyed mat_share keeps N material slots.
        mat.albedo = glam::Vec3::new(
            0.2 + 0.15 * (k as f32 % 5.0),
            0.3 + 0.1 * ((k as f32 + 1.0) % 4.0),
            0.5 + 0.1 * ((k as f32 + 2.0) % 3.0),
        );
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: format!("C6Mesh{k}.000"),
                mesh: mesh.clone(),
                source: Default::default(),
                mesh_cache_id: None,
                material: mat.clone(),
                modifiers: Vec::new(),
            },
        );
        if let Some(node) = app.scene.scene_graph.get_mut(nid) {
            node.local_position = Vec3::new(k as f32 * 3.0, 0.0, 0.0);
        }
        app.poly_sync_mesh_cache(nid);
        let cid = match app.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody {
                mesh_cache_id: Some(c),
                ..
            }) => *c,
            _ => {
                results.push(format!("FAIL: master {k} has no mesh_cache_id"));
                return results;
            }
        };
        master_cids.push(cid);
        master_meshes.push(mesh);
        master_mats.push(mat);
        master_nids.push(nid);
    }

    // Interleave instances: for each slot i, place instance of every mesh.
    // Insertion order interleaves cids — consecutive batch without sort = N×M draws.
    let spacing = 1.5f32;
    let side = ((n_meshes * m_each) as f32).sqrt().ceil() as i32;
    let mut placed = 0usize;
    for i in 1..m_each {
        for k in 0..n_meshes {
            let ix = (placed as i32) % side;
            let iz = (placed as i32) / side;
            placed += 1;
            let pos = Vec3::new(
                (ix as f32 - side as f32 * 0.5) * spacing,
                0.0,
                (iz as f32 - side as f32 * 0.5) * spacing,
            );
            let nid = app.scene.scene_graph.add_child(
                root,
                NodePayload::PolygonalBody {
                    name: format!("C6Mesh{k}.{i:03}"),
                    mesh: master_meshes[k].clone(),
                    source: Default::default(),
                    mesh_cache_id: Some(master_cids[k]),
                    material: master_mats[k].clone(),
                    modifiers: Vec::new(),
                },
            );
            if let Some(node) = app.scene.scene_graph.get_mut(nid) {
                node.local_position = pos;
            }
            app.poly_sync_mesh_cache(nid);
        }
    }
    // Reposition masters into the grid (instance 0 of each).
    for k in 0..n_meshes {
        let ix = (k as i32) % side.max(1);
        let iz = (k as i32) / side.max(1);
        let pos = Vec3::new(
            (ix as f32 - side as f32 * 0.5) * spacing,
            0.0,
            (iz as f32 - side as f32 * 0.5) * spacing + 0.01 * k as f32,
        );
        if let Some(node) = app.scene.scene_graph.get_mut(master_nids[k]) {
            node.local_position = pos;
        }
    }

    let unique_cids = master_cids.len();
    let total = n_meshes * m_each;
    // Pure C.6 gate on the *insertion* interleave pattern (same as stream before sort).
    let mut gate_entries: Vec<(u64, bool)> = Vec::with_capacity(total);
    for i in 0..m_each {
        for k in 0..n_meshes {
            let _ = i;
            gate_entries.push((master_cids[k].0, false));
        }
    }
    // Masters were inserted first then interleaved clones — stream order ≈ masters then interleave.
    // Gate uses the canonical interleaved pattern the plan describes.
    let draws_sorted = crate::raster_common::count_mesh_instance_draws(&gate_entries, true);
    let draws_unsorted = crate::raster_common::count_mesh_instance_draws(&gate_entries, false);

    app.draw_stream_stale = true;
    app.ensure_structural_draw_stream();
    app.mark_dirty(DirtyFlags::SCENE | DirtyFlags::MESH | DirtyFlags::RENDER);
    app.render.viewport_mode = ViewportRenderMode::Hybrid;

    results.push(format!(
        "unique_cids={unique_cids} total_bodies≈{total} streamN={} share={}",
        app.draw_stream.len(),
        app.shared_mesh_cache_id_count()
    ));
    results.push(format!(
        "C.6 pure gate (interleaved pattern): sorted_draws={draws_sorted} unsorted_draws={draws_unsorted} expect_N={n_meshes}"
    ));
    if draws_sorted == n_meshes as u32 && draws_unsorted == total as u32 {
        results.push(format!(
            "PASS C.6: sort → {n_meshes} draws; without sort → {total} draws (sabotage holds)"
        ));
    } else {
        results.push(format!(
            "FAIL C.6: expected sorted={n_meshes} unsorted={total}, got sorted={draws_sorted} unsorted={draws_unsorted}"
        ));
    }
    results.push(
        "Runtime: after a camera frame, scoreboard draws≈N when all instances visible (frustum may reduce).".into(),
    );
    results
}

