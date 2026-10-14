//! D0 / G10 — dense unique mesh stress harness (Phase D tri-bound measure).
//! Pure-relocated from stress_test.rs.
//!
//! AZRA_STRESS_SELECT / AZRA_STRESS_EDIT flags and camera-framing comments
//! stay glued to the spawn body below.

use crate::*;
use core_math::primitives::PbrMaterial;

// ============================================================
// D0 / G10 — dense unique mesh (Phase D entry: tri-bound measure)
// ============================================================

/// Spawn `bodies` unique poly boxes, each Catmull–Clark subdivided `levels` times.
/// Default: 1 body × 7 levels ≈ 98k quads ≈ **196k tris** (unique, not instanced).
/// `AZRA_DENSE_STRESS=1` or `AZRA_DENSE_STRESS=3x8` (bodies×levels).
///
/// Phase D entry gate: if scoreboard `rast` / GPU ms dominate frame at high tri count,
/// triangle throughput is the bottleneck; if not, D remains deferred.
pub(crate) fn spawn_dense_poly_bodies(
    app: &mut AzraApp,
    bodies: usize,
    levels: u32,
) -> Vec<String> {
    use core_math::scene_graph::NodePayload;
    use std::sync::Arc;

    let mut results = vec![format!(
        "=== Dense Stress (unique meshes) bodies={} CC_levels={} ===",
        bodies, levels
    )];
    if bodies == 0 {
        results.push("bodies=0 — nothing to spawn".into());
        return results;
    }
    let levels = levels.min(10); // 6*4^10 faces is multi-million; hard cap

    app.tool.work_mode = WorkMode::Object;
    app.poly_work_mode = PolyWorkMode::Object;
    app.modeling_context = ModelingContext::Polygonal;

    let base: core_math::half_edge::HalfEdgeMesh =
        core_math::primitive_spec::PrimitiveSpec::Box(core_math::primitive_spec::BoxSpec {
            size: glam::Vec3::ONE,
            ..Default::default()
        })
        .build();

    let root = app.scene.scene_graph.root();
    let spacing = 3.0f32;
    let side = (bodies as f32).sqrt().ceil() as i32;
    let mut total_faces = 0u32;
    let mut last_nid = None;
    for i in 0..bodies {
        let mut mesh = base.clone();
        for _ in 0..levels {
            mesh.subdivide_catmull_clark();
        }
        let faces = mesh.faces.len() as u32;
        total_faces = total_faces.saturating_add(faces);
        let ix = (i as i32) % side.max(1);
        let iz = (i as i32) / side.max(1);
        // Single body (1x9): place at origin — camera frames the mesh, cut probe /
        // CUT_DUMP identity matches world. Multi-body keeps the grid (spacing 3.0).
        let pos = if bodies == 1 {
            Vec3::ZERO
        } else {
            Vec3::new(
                (ix as f32 - side as f32 * 0.5) * spacing,
                0.0,
                (iz as f32 - side as f32 * 0.5) * spacing,
            )
        };
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: format!("DenseCC.{i:03}"),
                mesh: Arc::new(mesh),
                source: Default::default(),
                mesh_cache_id: None,
                material: PbrMaterial::default(),
                modifiers: Vec::new(),
            },
        );
        if let Some(node) = app.scene.scene_graph.get_mut(nid) {
            node.local_position = pos;
        }
        // R6a: do NOT `poly_sync_mesh_cache` here — 8× dense CC raster+upload in one
        // startup frame was a measured 2.8 s stall. Mark for budgeted drain
        // (`poly_drain_display_dirty` Medium ticket, ≤12 ms/frame).
        app.poly_request_display_sync(nid);
        last_nid = Some(nid);
    }

    // Frame the cluster so the dense body fills most of the viewport (GPU fill + tri cost).
    // Prior distance≈4 left a small silhouette → scoreboard under-reported GPU work.
    let grid_r = (side as f32 * spacing * 0.5).max(0.75);
    app.viewport.camera.target = Vec3::ZERO;
    app.viewport.camera.distance = (grid_r * 1.35).max(1.4).min(12.0);

    // D0 tri-bound measure: poly LOD policy walks the dense mesh every frame (~15 ms at
    // 8×CC). Disable so the scoreboard can see GPU/rast cost, not display-LOD CPU.
    app.cache.poly_lod_auto = false;
    // Deselect so properties panel does not O(faces) scan the dense mesh for material
    // uniformity (was ~7 ms of scoreboard `lay=` at 1x9). Measure GPU, not UI.
    app.deselect_all();
    app.modeling_context = ModelingContext::Polygonal;

    // PERF_TARGETS G1/S2 + G1/S4 cells (dense SELECTED / edit-mode wire) need the
    // opposite of the deselect above: the selection/overlay cost IS the measurement.
    // AZRA_STRESS_SELECT=1 → select the last spawned body (object mode, S2).
    // AZRA_STRESS_EDIT=1   → select it AND Tab into vertex edit mode (S4).
    if let Some(nid) = last_nid {
        let want_edit = std::env::var_os("AZRA_STRESS_EDIT").is_some();
        if want_edit || std::env::var_os("AZRA_STRESS_SELECT").is_some() {
            app.select_single_unified(crate::types::SceneItemId::Node(nid));
            results.push(format!("stress-select: node {nid:?} selected"));
        }
        if want_edit {
            app.poly_switch_work_mode(PolyWorkMode::Vertex);
            results.push(format!("stress-edit: vertex mode = {:?}", app.poly_work_mode));
        }
    }

    app.draw_stream_stale = true;
    app.structural_draw_stale = true;
    // R6a: bodies already in poly_display_dirty via poly_request_display_sync.
    // Do NOT set MESH here — process_dirty(MESH) used to call poly_sync_all_meshes
    // (now budgeted, but SCENE|MESH still does stream/structural work). RENDER only
    // so the first frame draws as bodies drain in; stream rebuilds when caches land.
    app.mark_dirty(DirtyFlags::RENDER);
    app.render.viewport_mode = ViewportRenderMode::Hybrid;

    // Quads from CC ≈ faces; tris ≈ 2 * faces for display (quad→2 tris).
    let approx_tris = total_faces.saturating_mul(2);
    results.push(format!(
        "faces≈{total_faces} approx_tris≈{approx_tris} streamN={} cam_dist={:.1}",
        app.draw_stream.len(),
        app.viewport.camera.distance
    ));
    results.push(
        "Runtime: scoreboard med= gpu_ms= gpuSh= gpuGb= — if gpu_ms / gpuGb dominate frame at multi-M tris, D entry tri-bound is green."
            .into(),
    );
    // D4 coverage scene note (owner 2026-08-05): this is the preferred non-terrain
    // cluster test. Demo landscape does not exercise D4 after WP7.
    let thr = crate::cluster_bake::CLUSTER_TRI_THRESHOLD;
    if approx_tris as usize >= thr {
        results.push(format!(
            "D4: approx_tris≥threshold ({thr}) — this scene WILL cluster when AZRA_CLUSTERS=1 \
             (or Tweaks D4). Expect: display sync (1 body same frame) → [ClusterBake] → \
             [ClusterGpu] resident (3.1M bake can take tens of seconds) → cut counters."
        ));
    } else {
        results.push(format!(
            "D4: approx_tris≈{approx_tris} < threshold {thr} — raise CC levels (e.g. 1x9) to cluster."
        ));
    }
    if app.render.dbg_clusters {
        results.push(
            "D4: clusters already ON at spawn — wait for [ClusterGpu] resident before judging cut."
                .into(),
        );
        results.push(
            "D4 A/B: AZRA_CLUSTER_FORCE_L0=1 → fixed finest set (if still holes, not selection). \
             AZRA_CLUSTER_NO_VIS=1 → skip frustum/cone. AZRA_CLUSTER_CUT_PASSTHROUGH=1 → all LODs."
                .into(),
        );
    } else {
        results.push(
            "D4: clusters OFF — set AZRA_CLUSTERS=1 (same binary) for a real cluster before/after."
                .into(),
        );
    }
    results
}

