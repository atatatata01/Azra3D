// overlay_faces.rs — depth-tested GPU face-select fill (POLY_VIEWPORT_PERF P4).
//
// HISTORY: An earlier pass depth-tested selection tris against the solid and failed on most
// faces because the fill used a different triangulation than the solid bake ("selection only
// works on one face"). This rebuild uses the **same** `earclip_polygon` as
// `core_math::poly_mesh::raster_data_ngon` (solid display), plus a tiny outward normal pull so
// coplanar depth equality wins without silhouette leak (shader still has cliff-safe eps).
//
// Live path: GPU when edit face/group selection is non-empty; egui outline still in draw_poly.
// Fill falls back to egui only when this list is empty (no active poly selection).

use core_math::half_edge::FaceId;
use core_math::scene_graph::NodePayload;
use glam::Vec3;
use render::face_overlay::FaceVertex;
use std::hash::{Hash, Hasher};

use crate::*;

/// Soft cap on emitted face-fill vertices (Frame Budget Rule). ~33k tris.
const MAX_FACE_OVERLAY_VERTS: usize = 100_000;

impl AzraApp {
    /// Rebuild `overlay_faces` when selection / mesh / transform / look changes.
    /// Returns `true` when the draw should use the X-Ray (always-visible) fragment path
    /// (`!visible_only` — same semantics as the line overlay).
    pub(crate) fn build_overlay_faces(&mut self) -> bool {
        // X-ray when hidden-selection mode OR the active body has live modifiers (see the
        // MODIFIER CAGE note below) — computed up front so the fingerprint early-out
        // returns the same answer as a full rebuild.
        let modifier_cage = self
            .sel
            .usel
            .active_node()
            .and_then(|nid| self.scene.scene_graph.get(nid))
            .map_or(false, |node| match &node.payload {
                NodePayload::PolygonalBody { modifiers, .. } => !modifiers.is_empty(),
                _ => false,
            });
        // "Show backfaces" is an X-RAY request, not a facing filter (see the FACING note at
        // the emit loop): the only honest way to reveal a selected face whose surface is not
        // toward the camera is to stop depth-testing the fill.
        let xray = !self.sel.visible_only || modifier_cage || self.viewport.poly_sel_show_backface;
        let fp = self.overlay_faces_fingerprint();
        if fp == self.overlay_faces_fp {
            return xray;
        }

        self.overlay_faces.clear();
        self.overlay_faces_fp = fp;

        // Object mode / non-poly: nothing to fill.
        if self.modeling_context != ModelingContext::Polygonal {
            return xray;
        }
        if !matches!(
            self.poly_work_mode,
            PolyWorkMode::Face | PolyWorkMode::Group
        ) {
            return xray;
        }
        if self.poly_sel.selected_faces.is_empty() {
            return xray;
        }
        let Some(nid) = self.sel.usel.active_node() else {
            return xray;
        };
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return xray;
        };
        let NodePayload::PolygonalBody { mesh, .. } = &node.payload else {
            return xray;
        };
        // MODIFIER CAGE (owner 2026-08-03 "selection set glitches as random lines"): the fill
        // triangulates BASE faces, but the solid draws the MODIFIER-STACK mesh (bevel moves
        // verts) — depth-testing base-cage triangles against a different surface z-fights as
        // sliver artifacts. With any live modifier (`modifier_cage` above), the cage fill is
        // drawn X-RAY (no depth test), Blender edit-cage style. No modifiers → base ==
        // display, depth test stays exact.

        let world = self.scene.scene_graph.world_transform(nid);
        let sc = self.viewport.poly_sel_color;
        let fa = self.viewport.poly_sel_face_opacity.clamp(0.05, 1.0);
        let color = FaceVertex::pack_color(sc[0], sc[1], sc[2], fa);
        let hidden = &self.poly_sel.hidden_vertices;

        // FACING IS NOT OCCLUSION (owner 2026-08-07: "selected faces inside a notch lose their
        // green when I orbit"). This loop used to drop any face with
        // `n_world.dot(cam_fwd) > 0.05` — a back-face heuristic standing in for a depth test.
        // It removes the whole face the instant its normal tips past ~87 deg from the view
        // direction, which for a notch/recess wall (or ANY face near the silhouette) happens
        // while the face is still plainly visible. Blender's overlay engine never does this:
        // `overlay_edit_mesh` rasterizes the selection geometry and calls `test_occlusion()`
        // against the scene depth texture, so a face is hidden only when something is actually
        // in FRONT of it. We already have exactly that: `face_overlay.wgsl::scene_visible()`
        // samples the G-buffer depth with a cliff-safe slope epsilon. The CPU cull was pure
        // redundancy in the depth-tested path and an outright contradiction in the X-ray path.
        // Do not reintroduce a dot(normal, view) test here.

        // COPLANAR BIAS MOVED TO THE SHADER (owner 2026-08-07, second half of the "notch faces
        // lose their green" report).
        //
        // This loop used to emit `lp + face_n * 2.5e-4` — a nudge along the face's OWN normal
        // to win the coplanar depth compare. That silently assumes the normal points at the
        // camera. A notch cut by a boolean has walls whose normals point inward/away, and a
        // bevel/dissolve can flip winding; for those faces the "bias" pushed the fill *into*
        // the solid, `scene_visible()` discarded it, and the highlight dropped out at exactly
        // the angles the owner reported.
        //
        // Blender biases in DEPTH, not along geometry (`ndc_offset` in the overlay engine),
        // which has no normal dependency at all. `face_overlay.wgsl::vs_main` now applies the
        // same NDC offset. Emitting UNBIASED world positions here also keeps this vertex list
        // camera-independent, so `overlay_faces_fingerprint` (which deliberately excludes the
        // camera) still caches across orbit frames.

        for &fid_u in &self.poly_sel.selected_faces {
            if self.overlay_faces.len() + 3 > MAX_FACE_OVERLAY_VERTS {
                break;
            }
            // BOUNDS FIRST: `mesh.face()` indexes `faces[..]` directly and PANICS on a stale id.
            // `selected_faces` legitimately outlives the mesh it was built against (any topology
            // op — bevel, dissolve, boolean — renumbers faces before selection is reconciled), so
            // an out-of-range id here is a normal transient, not corruption. Skip it silently.
            if fid_u as usize >= mesh.faces.len() {
                continue;
            }
            let fid = FaceId(fid_u);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                continue;
            }
            let verts = mesh.face_vertices(fid);
            if verts.len() < 3 || verts.iter().any(|v| hidden.contains(&v.0)) {
                continue;
            }
            let face_n = mesh.compute_face_normal(fid);
            let local_pts: Vec<Vec3> = verts.iter().map(|&v| mesh.vert(v).position).collect();
            if local_pts.iter().any(|p| !p.is_finite()) || !face_n.is_finite() {
                continue;
            }
            // Face diameter — reject earclip stall-fan tris that slash across the body.
            let mut flo = local_pts[0];
            let mut fhi = local_pts[0];
            for &p in &local_pts {
                flo = flo.min(p);
                fhi = fhi.max(p);
            }
            let face_diag = (fhi - flo).length().max(1e-6);
            let max_edge = face_diag * 3.0;
            // Same ear-clip as solid raster_data_ngon (poly_mesh.rs:299).
            for tri in core_math::poly_mesh::earclip_polygon(&local_pts, face_n) {
                if self.overlay_faces.len() + 3 > MAX_FACE_OVERLAY_VERTS {
                    break;
                }
                if tri.iter().any(|&i| i as usize >= local_pts.len()) {
                    continue;
                }
                let a = local_pts[tri[0] as usize];
                let b = local_pts[tri[1] as usize];
                let c = local_pts[tri[2] as usize];
                if (b - a).length() > max_edge
                    || (c - b).length() > max_edge
                    || (a - c).length() > max_edge
                {
                    continue;
                }
                for &ci in &tri {
                    let lp = local_pts[ci as usize];
                    // Local→world exactly as the solid does. Depth bias is the shader's job.
                    let wp = world.transform_point3(lp);
                    if !wp.is_finite() {
                        continue;
                    }
                    self.overlay_faces.push(FaceVertex {
                        position: [wp.x, wp.y, wp.z],
                        color,
                    });
                }
            }
        }

        // OUTLIER DETECTOR (`AZRA_OVERLAY_TRACE=1`, off by default). A selection/polygroup fill that
        // streaks across the whole viewport is one overlay vertex sitting far outside the body — a
        // stale index resolving to foreign geometry, or a NaN. Report it with coordinates instead of
        // inferring the cause from a screenshot. Also flags non-finite values, which project to
        // garbage rather than being clipped.
        if std::env::var_os("AZRA_OVERLAY_TRACE").is_some() && !self.overlay_faces.is_empty() {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for v in &mesh.vertices {
                lo = lo.min(v.position);
                hi = hi.max(v.position);
            }
            let c = world.transform_point3((lo + hi) * 0.5);
            let radius = ((hi - lo).length() * 0.5).max(1e-4);
            let mut bad = 0usize;
            for fv in &self.overlay_faces {
                let p = Vec3::from(fv.position);
                if !p.is_finite() || (p - c).length() > radius * 50.0 {
                    if bad < 8 {
                        eprintln!(
                            "[OVERLAY] stray face vert {:?} — body centre {:?} radius {radius:.4}",
                            fv.position, c
                        );
                    }
                    bad += 1;
                }
            }
            if bad > 0 {
                eprintln!(
                    "[OVERLAY] {bad}/{} face verts outside 50x the body radius (selected_faces={})",
                    self.overlay_faces.len(),
                    self.poly_sel.selected_faces.len()
                );
            }
        }

        xray
    }

    fn overlay_faces_fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.modeling_context as u8).hash(&mut h);
        (self.poly_work_mode as u8).hash(&mut h);
        self.sel.visible_only.hash(&mut h);
        self.viewport.poly_sel_show_backface.hash(&mut h);
        self.viewport.poly_sel_face_opacity.to_bits().hash(&mut h);
        for c in &self.viewport.poly_sel_color {
            c.to_bits().hash(&mut h);
        }
        // Selection set (order-independent).
        let mut ids: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        ids.sort_unstable();
        ids.len().hash(&mut h);
        for id in ids {
            id.hash(&mut h);
        }
        self.poly_sel.hidden_vertices.len().hash(&mut h);

        if let Some(nid) = self.sel.usel.active_node() {
            nid.0.hash(&mut h);
            let world = self.scene.scene_graph.world_transform(nid);
            for col in world.to_cols_array() {
                col.to_bits().hash(&mut h);
            }
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, mesh_cache_id, .. } = &node.payload {
                    // O(1) mesh identity — NEVER content_signature() here: that is a full
                    // O(verts+edges) walk EVERY frame the body is selected (owned ~57 ms of
                    // `disp` on a 3.1M-tri body; PERF_TARGETS G1/S2/S4). Same identity rule
                    // as overlay_lines::overlay_fingerprint (C-scale, 2026-07-31): Arc ptr
                    // catches COW replacements, pt_stamp catches in-place edits via the
                    // display-sync upload that any visible mesh change must perform anyway.
                    (std::sync::Arc::as_ptr(mesh) as usize).hash(&mut h);
                    if let Some(cid) = mesh_cache_id {
                        self.cache.mesh_cache.pt_stamp(*cid).unwrap_or(0).hash(&mut h);
                    }
                }
            }
        } else {
            0u64.hash(&mut h);
        }
        h.finish()
    }
}
