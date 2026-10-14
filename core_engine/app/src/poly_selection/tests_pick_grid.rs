//! The screen-space pick grid must index EVERY projected element (owner 2026-08-11: "edge select
//! works well when I start using the program but after a while it starts to bug / degrade").
//!
//! `rebuild_pick_cache` pushes each projected vertex/edge into `vert_screen` / `edge_screen`
//! (unbounded `Vec`) and then registers its index in the 24×16 screen grid — the ONLY structure
//! `poly_hover_pick` ever iterates. The grid held `u16`, so registration was gated on
//! `if idx <= u16::MAX`. Past 65 536 projected elements the surplus lived in `*_screen` but in no
//! cell at all: **unhoverable, therefore unclickable, silently**.
//!
//! Why it reads as "degrades over time" rather than "broken":
//!   * the ceiling is crossed by MODELLING — ~2 edges per quad, so a plane past ~32 800 quads on
//!     screen is already over it, and every loop cut / subdivide / bevel pushes further;
//!   * `canonical_edges` walks half-edge index order ≈ creation order, so the elements that fall
//!     off the end are the NEWEST geometry — exactly the edges the user just made;
//!   * only *projected* elements count, so zooming or orbiting changes which ones make the cut,
//!     making it feel intermittent rather than like a hard limit.
//!
//! These gates assert the invariant directly — every `*_screen` entry is reachable from at least
//! one grid cell — so no future capacity choice can silently reintroduce the cliff.

#[cfg(test)]
mod tests {
    use crate::{AzraApp, DirtyFlags, ModelingContext, PolyWorkMode, SceneItemId};
    use core_math::half_edge::HalfEdgeMesh;
    use core_math::scene_graph::NodePayload;
    use eframe::egui;
    use glam::{Quat, Vec3};

    /// Headless app with one poly plane of `n`×`n` quads, framed so the whole sheet projects
    /// inside the viewport rect. `new_plane` builds in XZ, so the camera looks straight down −Y.
    fn app_with_plane(n: u32, mode: PolyWorkMode) -> (AzraApp, egui::Rect) {
        let mesh = HalfEdgeMesh::new_plane(2.0, 2.0, n, n);
        let mut app = AzraApp::new_headless();
        app.modeling_context = ModelingContext::Polygonal;
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "sheet".into(),
                mesh: std::sync::Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(SceneItemId::Node(nid));
        app.poly_work_mode = mode;
        app.poly_sel.body_stamp = Some(nid.0);
        // Occlusion is not what is under test, and skipping it avoids a 40k-face ray cast.
        app.sel.visible_only = false;
        // Look straight down at the XZ sheet from +Y, far enough that all of it is on screen.
        app.viewport.camera.target = Vec3::ZERO;
        app.viewport.camera.distance = 6.0;
        app.viewport.camera.orbit = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
        app.viewport.viewport_rect = rect;
        (app, rect)
    }

    /// Indices present in no grid cell — i.e. projected but unreachable by any hover query.
    fn unreachable(cache: &crate::PolyPickCache, n_screen: usize) -> usize {
        let mut seen = vec![false; n_screen];
        for cell in &cache.grid {
            for &i in cell {
                if (i as usize) < n_screen {
                    seen[i as usize] = true;
                }
            }
        }
        seen.iter().filter(|s| !**s).count()
    }

    /// 200×200 quads = 40 000 faces / 80 400 undirected edges — over the old 65 536 ceiling and
    /// squarely in the size range the owner models at. Before the fix: 14 864 edges unreachable.
    #[test]
    fn every_projected_edge_is_reachable_from_the_pick_grid() {
        let (mut app, rect) = app_with_plane(200, PolyWorkMode::Edge);
        app.poly_hover_pick(rect.center(), rect);

        let n = app.poly_pick_cache.edge_screen.len();
        assert!(
            n > 65_536,
            "PRECONDITION: the fixture must project MORE than the old u16 ceiling, else this \
             gate proves nothing (projected {n} edges — check the camera framing)"
        );
        let missing = unreachable(&app.poly_pick_cache, n);
        assert_eq!(
            missing, 0,
            "{missing} of {n} projected edges are in `edge_screen` but in NO grid cell — \
             `poly_hover_pick` only ever walks grid cells, so those edges cannot be hovered \
             and cannot be clicked"
        );
    }

    /// Same ceiling on the vertex path. 260×260 quads = 68 121 verts, just over 65 536.
    #[test]
    fn every_projected_vertex_is_reachable_from_the_pick_grid() {
        let (mut app, rect) = app_with_plane(260, PolyWorkMode::Vertex);
        app.poly_hover_pick(rect.center(), rect);

        let n = app.poly_pick_cache.vert_screen.len();
        assert!(
            n > 65_536,
            "PRECONDITION: the fixture must project MORE than the old u16 ceiling \
             (projected {n} verts — check the camera framing)"
        );
        let missing = unreachable(&app.poly_pick_cache, n);
        assert_eq!(
            missing, 0,
            "{missing} of {n} projected vertices are unreachable from the pick grid"
        );
    }

    /// The elements that used to fall off the end are the HIGH half-edge ids — newest geometry.
    /// Pinning that makes the user-visible shape of the bug explicit: what stopped being
    /// selectable was whatever you had just modelled.
    #[test]
    fn the_last_created_edges_are_the_ones_the_ceiling_used_to_drop() {
        let (mut app, rect) = app_with_plane(200, PolyWorkMode::Edge);
        app.poly_hover_pick(rect.center(), rect);
        let cache = &app.poly_pick_cache;
        let n = cache.edge_screen.len();
        assert!(n > 65_536);

        // What the u16 gate would have kept vs dropped, replayed on the real cache order.
        let dropped: Vec<u32> = cache.edge_screen[65_536..].iter().map(|e| e.0).collect();
        let kept_max = cache.edge_screen[..65_536].iter().map(|e| e.0).max().unwrap();
        let dropped_min = dropped.iter().copied().min().unwrap();
        assert!(
            dropped_min > kept_max / 2,
            "the dropped set must sit in the HIGH half-edge ids (newest geometry): \
             kept_max={kept_max} dropped_min={dropped_min}"
        );
        // …and they are reachable now.
        assert_eq!(unreachable(cache, n), 0);
    }

    /// Owner: "I need to rotate the camera after an edge selection to see the highlight."
    /// The live LMB path is `poly_release_click_select` (paint-click / tiny box). It used
    /// to mutate `selected_edges` and leave `is_dirty` false, so `dispatch_gpu_render`
    /// early-returned and the GPU overlay kept last frame's lines until an orbit dirtied.
    #[test]
    fn edge_click_select_dirties_the_viewport_so_the_highlight_rebuilds() {
        let (mut app, _rect) = app_with_plane(2, PolyWorkMode::Edge);
        app.poly_sel.hovered_edge = Some(0);
        app.dirty.take();
        app.is_dirty = false;
        app.overlay_lines_fp = 0xDEAD_BEEF;
        app.poly_release_click_select(egui::pos2(100.0, 100.0), false, false, false, 1.0);
        assert!(
            app.poly_sel.selected_edges.contains(&0),
            "the hovered edge must be selected"
        );
        assert!(
            app.is_dirty,
            "edge click must mark RENDER so the GPU highlight uploads this frame, \
             not on the next camera move"
        );
        assert!(
            app.dirty.has(DirtyFlags::RENDER),
            "the RENDER bit must be set (is_dirty alone can be cleared without a rebuild)"
        );
    }
}
