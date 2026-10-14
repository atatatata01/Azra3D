// draw_poly.rs — Polygon editing overlays: face feedback, selected faces,
//   poly sub-element overlays, loop cut preview, bevel/extrude/inset HUD, knife overlay
// Extracted from drawing.rs
use eframe::egui;
use glam::Vec3;
use core_math::primitives::SdfShape;
use core_math::half_edge::{VertexId, HalfEdgeId, FaceId};
use core_math::scene_graph::NodePayload;
use crate::*;

impl AzraApp {
    pub(crate) fn draw_face_feedback(&self, painter: &egui::Painter, rect: egui::Rect) {
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        // Hovered face highlight — semi-transparent quad overlay (like gizmo arrow hover)
        if !self.tool.current.is_face_push_pull() {
            if let Some((idx, local_n)) = self.tool.hovered_face {
                // Don't draw hover on already-selected faces
                let already_selected = self.sel.selected_faces.iter().any(|n| (*n - local_n).length() < 0.01);
                if !already_selected {
                    if let Some(prim) = self.scene.primitives.get(idx) {
                        if let SdfShape::Box { half_extents, .. } = &prim.shape {
                            let he = *half_extents * prim.scale;
                            let rot = prim.rotation_quat();
                            let face_dist = he.x * local_n.x.abs() + he.y * local_n.y.abs() + he.z * local_n.z.abs();
                            let face_center_local = local_n * face_dist;
                            let (t1, t2) = if local_n.x.abs() > 0.5 {
                                (Vec3::new(0.0, he.y, 0.0), Vec3::new(0.0, 0.0, he.z))
                            } else if local_n.y.abs() > 0.5 {
                                (Vec3::new(he.x, 0.0, 0.0), Vec3::new(0.0, 0.0, he.z))
                            } else {
                                (Vec3::new(he.x, 0.0, 0.0), Vec3::new(0.0, he.y, 0.0))
                            };
                            let corners = [
                                face_center_local - t1 - t2, face_center_local + t1 - t2,
                                face_center_local + t1 + t2, face_center_local - t1 + t2,
                            ];
                            let screen_pts: Vec<egui::Pos2> = corners.iter().filter_map(|&c| {
                                let world = prim.position + rot * c;
                                world_to_screen(world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)
                            }).collect();
                            if screen_pts.len() == 4 {
                                let fill = egui::Color32::from_rgba_unmultiplied(255, 200, 50, 35);
                                painter.add(egui::Shape::convex_polygon(screen_pts.clone(), fill, egui::Stroke::NONE));
                                let outline = egui::Color32::from_rgba_unmultiplied(255, 200, 0, 160);
                                painter.add(egui::Shape::closed_line(screen_pts, egui::Stroke::new(self.tweak.face_outline_width, outline)));
                            }
                            // Small direction arrow from face center
                            let world_n = (rot * local_n).normalize();
                            let face_center = prim.position + rot * face_center_local;
                            if let Some(sp) = world_to_screen(face_center, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                                let arrow_end = face_center + world_n * 0.4;
                                if let Some(ep) = world_to_screen(arrow_end, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                                    let av = ep - sp;
                                    let an = av.normalized();
                                    let tip = sp + an * self.tweak.arrow_tip_px;
                                    painter.arrow(sp, tip - sp, egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(255,200,0,180)));
                                }
                            }
                        }
                    }
                }
            }
        }
        // Drag feedback
        if let Some(fd) = self.tool.current.as_face_push_pull() {
            if let Some(prim) = self.scene.primitives.get(fd.prim_idx) {
                let rot = prim.rotation_quat();
                let world_n = (rot * fd.local_normal).normalize();
                let sc = fd.original_scale;
                // Original face position (visual = stored * scale)
                let he_orig = fd.original_extents * sc;
                let face_dist_orig = he_orig.x * fd.local_normal.x.abs() + he_orig.y * fd.local_normal.y.abs() + he_orig.z * fd.local_normal.z.abs();
                let orig_face = fd.original_pos + world_n * face_dist_orig;
                if let Some(os) = world_to_screen(orig_face, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    painter.circle_filled(os, 4.0, egui::Color32::from_gray(120));
                }
                // Current face position (visual = stored * scale)
                if let SdfShape::Box { half_extents, .. } = &prim.shape {
                    let he_cur = *half_extents * prim.scale;
                    let face_dist_cur = he_cur.x * fd.local_normal.x.abs() + he_cur.y * fd.local_normal.y.abs() + he_cur.z * fd.local_normal.z.abs();
                    let cur_face = prim.position + world_n * face_dist_cur;
                    if let Some(cs) = world_to_screen(cur_face, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        painter.circle_filled(cs, 6.0, egui::Color32::from_rgb(0,200,100));
                        // Distance label
                        let text = format!("{:.3}", fd.accumulated_delta);
                        painter.text(cs + egui::vec2(0.0, -18.0), egui::Align2::CENTER_BOTTOM, text,
                            egui::FontId::monospace(12.0), egui::Color32::WHITE);
                    }
                }
            }
        }
    }

    /// Draw blue/cyan overlay on selected faces in edit mode.
    pub(crate) fn draw_selected_faces(&self, painter: &egui::Painter, rect: egui::Rect) {
        let target = match self.sel.edit_target { Some(t) => t, None => return };
        if self.sel.selected_faces.is_empty() { return; }
        let prim = match self.scene.primitives.get(target) { Some(p) => p, None => return };
        let he = match &prim.shape {
            SdfShape::Box { half_extents, .. } => *half_extents * prim.scale,
            _ => return,
        };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let rot = prim.rotation_quat();

        for local_n in &self.sel.selected_faces {
            // Compute the 4 corners of this box face
            let face_dist = he.x * local_n.x.abs() + he.y * local_n.y.abs() + he.z * local_n.z.abs();
            let face_center_local = *local_n * face_dist;

            // Two tangent axes on this face
            let (t1, t2) = if local_n.x.abs() > 0.5 {
                (Vec3::new(0.0, he.y, 0.0), Vec3::new(0.0, 0.0, he.z))
            } else if local_n.y.abs() > 0.5 {
                (Vec3::new(he.x, 0.0, 0.0), Vec3::new(0.0, 0.0, he.z))
            } else {
                (Vec3::new(he.x, 0.0, 0.0), Vec3::new(0.0, he.y, 0.0))
            };

            let corners_local = [
                face_center_local - t1 - t2,
                face_center_local + t1 - t2,
                face_center_local + t1 + t2,
                face_center_local - t1 + t2,
            ];

            // Transform to world space and project to screen
            let screen_pts: Vec<egui::Pos2> = corners_local.iter().filter_map(|&c| {
                let world = prim.position + rot * c;
                world_to_screen(world, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)
            }).collect();

            if screen_pts.len() == 4 {
                // Filled semi-transparent blue quad
                let fill = egui::Color32::from_rgba_unmultiplied(50, 140, 255, 60);
                painter.add(egui::Shape::convex_polygon(
                    screen_pts.clone(), fill, egui::Stroke::NONE,
                ));
                // Cyan outline
                let outline = egui::Color32::from_rgba_unmultiplied(0, 200, 255, 200);
                painter.add(egui::Shape::closed_line(
                    screen_pts, egui::Stroke::new(2.0, outline),
                ));
            }
        }
    }

    // ================================================================
    // Polygon editing overlays
    // ================================================================

    /// Draw sub-element overlays for polygon editing (selected/hovered vertices, edges, faces).
    ///
    /// Called **ungated** from both viewport overlay paths: selection/hover is feedback, not
    /// decoration. Unselected vert cage still respects Poly Overlays; polygroups use their
    /// own toggle; edit wire is the separate GPU line pass.
    pub(crate) fn draw_poly_overlays(&self, painter: &egui::Painter, rect: egui::Rect) {
        // Hinge tool owns the viewport chrome — skip normal vert/edge/face cages so the
        // pick-edge pre-highlight and rotate gizmo stay readable (no vertex overlay).
        if self.tool.current.is_poly_face_hinge() {
            self.draw_face_hinge_overlay(painter, rect);
            return;
        }
        if self.modeling_context != ModelingContext::Polygonal { return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        // World transform for the selected node — converts mesh local coords to world coords
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform_pos = |local: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };

        // Selection look — driven by Viewport Overlays → Poly Selection settings.
        let sc = self.viewport.poly_sel_color;
        let sa = (self.viewport.poly_sel_opacity.clamp(0.0, 1.0) * 255.0) as u8;
        let fa = (self.viewport.poly_sel_face_opacity.clamp(0.0, 1.0) * 255.0) as u8;
        let edge_w = self.viewport.poly_sel_edge_width.max(0.5);
        let vert_r = self.viewport.poly_sel_vert_size.max(1.0);
        let show_back = self.viewport.poly_sel_show_backface;
        let cam_fwd = (self.viewport.camera.orbit * Vec3::NEG_Z).normalize_or_zero();
        let to_rgb = |c: [f32; 3], a: u8| {
            egui::Color32::from_rgba_unmultiplied(
                (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                a,
            )
        };
        let sel_vert_color = to_rgb(sc, sa);
        let hover_vert_color = egui::Color32::from_rgb(255, 220, 100); // yellow hover (fixed)
        // (Edge highlight colors live in `overlay_lines.rs::append_poly_selected_edges` now —
        // that batch is depth-tested on the GPU. See the edge-mode note further down.)
        let sel_face_fill = to_rgb(sc, fa);
        let sel_face_outline = to_rgb(sc, sa.min(200).max(80));
        let faces_away = |n_world: Vec3| -> bool {
            // True when face normal points away from camera (back-facing).
            n_world.dot(cam_fwd) > 0.05
        };

        let hidden = &self.poly_sel.hidden_vertices;

        // -- The EDIT WIREFRAME is no longer drawn here. --
        // It is now a depth-tested GPU line pass: segments gathered in overlay_lines.rs,
        // drawn in viewport_render.rs against the raster pass's depth buffer.
        //
        // What used to live here: a per-face back-face cull (Newell normal) plus an
        // O(edges x tris) CPU ray-triangle hidden-line clip capped at 250 faces. Both were
        // heuristics standing in for a depth test, and neither could hide the edges of a far
        // island sitting BEHIND a near one — they only ever asked "does this edge's own face
        // point away?", never "is something else in front of it?". That is the "I can see
        // through the mesh" report. The hardware depth test answers the real question, for
        // any mesh, at any face count, with no cap. Deleted rather than kept as a fallback:
        // two code paths for one operation is exactly what the no-bandaids rule forbids.
        //
        // Selection/hover/vertex-debug overlays stay as flat egui (always-on-top).
        // FACE FILL is egui again: a depth-tested GPU pass against the solid mesh's
        // different triangulation discarded most coplanar faces (owner: "selection only
        // works on one face"). Bleed at grazing angles is preferable to invisible selection.

        // -- DEBUG: every vertex with its index + LOCAL coords, for cross-referencing against Blender
        //    (Shift+I toggles). Magenta dot + yellow label. Works in any edit sub-mode. --
        if self.poly_vertex_debug && self.poly_work_mode != PolyWorkMode::Object {
            for i in 0..mesh.vertices.len() {
                let lp = mesh.vert(VertexId(i as u32)).position;
                let pos = xform_pos(lp);
                if let Some(sp) = world_to_screen(pos, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                    painter.circle_filled(sp, 3.0, egui::Color32::from_rgb(255, 40, 255));
                    // Just the ID on the dot (the coords live in the "Vertex Debug" table window — no clutter).
                    painter.text(sp + egui::vec2(5.0, -4.0), egui::Align2::LEFT_BOTTOM,
                        format!("{i}"), egui::FontId::monospace(11.0), egui::Color32::from_rgb(255, 255, 120));
                }
            }
        }

        // Selection/hover (vertex + edge) is drawn AFTER polygroup fills below — same painter
        // has no depth buffer; later shapes cover earlier ones. Drawing selection first made
        // opaque group colour bury edge highlights (owner: "edge selection under polygroup").

        // -- Polygroup visualisation (solid, matcap-friendly, depth-occluded) --
        // Opaque fills only on faces that are FRONTMOST under the camera (no see-through).
        // Occlusion: GPU face-ID pick buffer when valid; else CPU raycast to the mesh front surface.
        // Always backface-cull — never use selection "show backfaces" for group paint.
        //
        // Wire: DO NOT invent a second full-mesh wire (a previous restroke used crude backface
        // cull and drew back edges across painted faces — diagonal "see-through" lines). Opaque
        // fills cover the GPU line pass underneath, so after fills we only restroke edges of
        // faces we actually painted, reusing the same undirected half-edge walk as
        // `append_mesh_edges` (overlay_lines.rs) and the same poly/object wire look settings.
        // Midpoint occlusion uses the pick buffer so only front topology shows — same contract
        // as the depth-tested GPU wire.
        if self.ui.poly_show_polygroups {
            if let Some(cd) = self.cache.poly_custom_data.get(&nid) {
                if cd.has_polygroups() {
                    let color_of = |gid: u16| -> egui::Color32 {
                        let rgb = core_math::poly_mesh::PolyGroupMeta::color_for_id(gid);
                        egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
                    };
                    // Prefer GPU pick image; fall back to per-centroid raycast against the mesh.
                    let pick = self.poly_pick_image_valid();
                    let cam_orbit = self.viewport.camera.orbit;
                    // Collect (depth, fill, screen tri) for visible tris only, then far→near.
                    // Also track which face ids painted so wire restroke is restricted to them.
                    let mut tris: Vec<(f32, egui::Color32, [egui::Pos2; 3])> = Vec::new();
                    let mut painted_faces: std::collections::HashSet<u32> =
                        std::collections::HashSet::new();
                    for (i, f) in mesh.faces.iter().enumerate() {
                        if f.half_edge == core_math::half_edge::NO_EDGE {
                            continue;
                        }
                        let fid_u = i as u32;
                        let Some(&gid) = cd.groups.get(FaceId(fid_u)) else {
                            continue;
                        };
                        if gid == 0 {
                            continue;
                        }
                        let fid_t = FaceId(fid_u);
                        let verts = mesh.face_vertices(fid_t);
                        if verts.len() < 3 || verts.iter().any(|v| hidden.contains(&v.0)) {
                            continue;
                        }
                        let local_pts: Vec<Vec3> =
                            verts.iter().map(|&v| mesh.vert(v).position).collect();
                        let n = mesh.compute_face_normal(fid_t);
                        let n_world = world_xform.transform_vector3(n).normalize_or_zero();
                        // Always cull backs for polygroup paint (camera-facing only).
                        if n_world.length_squared() > 0.25 && faces_away(n_world) {
                            continue;
                        }
                        let fill = color_of(gid);
                        let mut face_painted = false;
                        for tri in core_math::poly_mesh::earclip_polygon(&local_pts, n) {
                            let mut world_pts = [Vec3::ZERO; 3];
                            let mut tp = [egui::Pos2::ZERO; 3];
                            let mut ok = true;
                            for (k, &li) in tri.iter().enumerate() {
                                let w = xform_pos(local_pts[li as usize]);
                                world_pts[k] = w;
                                match world_to_screen(
                                    w,
                                    cam_pos,
                                    cam_orbit,
                                    rect,
                                    aspect,
                                    ortho_hw,
                                    self.render.fov_deg,
                                ) {
                                    Some(p) => tp[k] = p,
                                    None => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            if !ok {
                                continue;
                            }
                            let world_c = (world_pts[0] + world_pts[1] + world_pts[2]) / 3.0;
                            let sp_c = egui::pos2(
                                (tp[0].x + tp[1].x + tp[2].x) / 3.0,
                                (tp[0].y + tp[1].y + tp[2].y) / 3.0,
                            );
                            // Depth occlusion: only paint if THIS face is the front surface here.
                            let visible = if let Some(img) = pick {
                                img.any_near(sp_c, |id| {
                                    id == crate::poly_select_gpu::pick_id_face(fid_u)
                                })
                            } else {
                                // CPU: ray through centroid; nearest hit must be this face.
                                self.poly_face_is_frontmost_at(
                                    nid,
                                    fid_u,
                                    sp_c,
                                    rect,
                                    cam_pos,
                                    cam_orbit,
                                    aspect,
                                    ortho_hw,
                                )
                            };
                            if !visible {
                                continue;
                            }
                            face_painted = true;
                            let depth = (world_c - cam_pos).dot(cam_fwd);
                            tris.push((depth, fill, tp));
                        }
                        if face_painted {
                            painted_faces.insert(fid_u);
                        }
                    }
                    tris.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    // Fills first (under wire) — ZBrush PolyFrame: solid group colour, then edges.
                    for (_d, fill, tp) in tris {
                        painter.add(egui::Shape::convex_polygon(
                            tp.to_vec(),
                            fill,
                            egui::Stroke::NONE,
                        ));
                    }

                    // Wire ON TOP of painted group fills only.
                    // Same undirected edge set as GPU `append_mesh_edges`; same look as poly
                    // edit wire (edit modes) or object wire overlay (object mode). Midpoint must
                    // hit an adjacent painted face in the pick buffer — no crude backface cull.
                    if !painted_faces.is_empty() {
                        let edit_wire = self.poly_work_mode != PolyWorkMode::Object
                            && self.viewport.show_overlays
                            && self.viewport.show_poly_overlays;
                        let (wire_w, wire_col) = if edit_wire {
                            let [r, g, b] = self.render.poly_wire_color;
                            let a = (self.render.poly_wire_opacity.clamp(0.0, 1.0) * 255.0) as u8;
                            (
                                self.render.poly_wire_thickness.max(0.8),
                                egui::Color32::from_rgba_unmultiplied(
                                    (r.clamp(0.0, 1.0) * 255.0) as u8,
                                    (g.clamp(0.0, 1.0) * 255.0) as u8,
                                    (b.clamp(0.0, 1.0) * 255.0) as u8,
                                    a.max(40),
                                ),
                            )
                        } else {
                            let [r, g, b] = self.render.wire_overlay_color;
                            let a = (self.render.wire_overlay_opacity.clamp(0.0, 1.0) * 255.0) as u8;
                            (
                                self.render.wire_overlay_thickness.max(0.8),
                                egui::Color32::from_rgba_unmultiplied(
                                    (r.clamp(0.0, 1.0) * 255.0) as u8,
                                    (g.clamp(0.0, 1.0) * 255.0) as u8,
                                    (b.clamp(0.0, 1.0) * 255.0) as u8,
                                    a.max(40),
                                ),
                            )
                        };
                        for i in 0..mesh.half_edges.len() as u32 {
                            let he = HalfEdgeId(i);
                            if !mesh.is_edge_live(he) {
                                continue;
                            }
                            let tw = mesh.he(he).twin;
                            // Undirected once — identical gate to append_mesh_edges.
                            if tw != core_math::half_edge::NO_TWIN && (tw.0 as usize) < i as usize {
                                continue;
                            }
                            let f_a = mesh.he(he).face;
                            let f_b = if tw != core_math::half_edge::NO_TWIN {
                                mesh.he(tw).face
                            } else {
                                core_math::half_edge::NO_FACE
                            };
                            let fa = if f_a != core_math::half_edge::NO_FACE {
                                Some(f_a.0)
                            } else {
                                None
                            };
                            let fb = if f_b != core_math::half_edge::NO_FACE {
                                Some(f_b.0)
                            } else {
                                None
                            };
                            // Only edges that bound a face we painted (covers the fill).
                            let touches_paint = fa.map(|f| painted_faces.contains(&f)).unwrap_or(false)
                                || fb.map(|f| painted_faces.contains(&f)).unwrap_or(false);
                            if !touches_paint {
                                continue;
                            }
                            let src = mesh.he_source(he);
                            let dst = mesh.he(he).vertex;
                            if hidden.contains(&src.0) || hidden.contains(&dst.0) {
                                continue;
                            }
                            let p0 = xform_pos(mesh.vert(src).position);
                            let p1 = xform_pos(mesh.vert(dst).position);
                            let mid = (p0 + p1) * 0.5;
                            let Some(sp_mid) = world_to_screen(
                                mid,
                                cam_pos,
                                cam_orbit,
                                rect,
                                aspect,
                                ortho_hw,
                                self.render.fov_deg,
                            ) else {
                                continue;
                            };
                            // Depth: midpoint must resolve to this edge's OWN id (drawn into the
                            // buffer, depth-biased) or one of its two adjacent faces.
                            let front_ok = if let Some(img) = pick {
                                img.any_near(sp_mid, |id| {
                                    id == crate::poly_select_gpu::pick_id_edge(i)
                                        || crate::poly_select_gpu::pick_id_to_face(id)
                                            .map_or(false, |f| fa == Some(f) || fb == Some(f))
                                })
                            } else {
                                fa.map(|f| {
                                    self.poly_face_is_frontmost_at(
                                        nid, f, sp_mid, rect, cam_pos, cam_orbit, aspect, ortho_hw,
                                    )
                                })
                                .unwrap_or(false)
                                    || fb
                                        .map(|f| {
                                            self.poly_face_is_frontmost_at(
                                                nid, f, sp_mid, rect, cam_pos, cam_orbit, aspect,
                                                ortho_hw,
                                            )
                                        })
                                        .unwrap_or(false)
                            };
                            if !front_ok {
                                continue;
                            }
                            if let Some((a, b)) = world_segment_to_screen(
                                p0,
                                p1,
                                cam_pos,
                                cam_orbit,
                                rect,
                                aspect,
                                ortho_hw,
                                self.render.fov_deg,
                            ) {
                                painter.line_segment([a, b], egui::Stroke::new(wire_w, wire_col));
                            }
                        }
                    }
                }
            }
        }

        // -- Face mode overlays (after polygroup fills so selection stays visible) --
        // Selected faces. A boolean/dissolve leaves NON-CONVEX faces (L-shaped n-gons); egui's
        // convex_polygon fan-triangulates from vertex 0, so a concave face's fill spills across the
        // notch. Ear-clip the face and fill per-triangle; outline is per-edge near-plane-clipped.
        //
        // BOUNDS FIRST (owner 2026-08-06: selection highlight panic): `mesh.face()` /
        // `face_vertices` index `faces[]` and panic on stale ids. Topology ops (knife, inset,
        // bevel, boolean, align) renumber faces before selection is reconciled — same guard as
        // overlay_faces.rs.
        if matches!(self.poly_work_mode, PolyWorkMode::Face | PolyWorkMode::Group) {
            // The FILL is the GPU pass (overlay_faces.rs -> face_overlay.wgsl), which is
            // depth-tested against the G-buffer the solid wrote. This egui fill is the
            // fallback for when that list is empty (documented at overlay_faces.rs:10) and
            // must not double-blend on top of it — the two used to draw together, so the
            // translucent green was applied twice.
            let gpu_fill_live = !self.overlay_faces.is_empty();
            // Outline occlusion: same rule as edge mode. X-Ray when "Show backfaces" or
            // Visible-Only is off; otherwise a real depth test (pick buffer), never facing.
            let face_xray = show_back || !self.sel.visible_only;
            let pick = self.poly_pick_image_valid();
            let cam_orbit = self.viewport.camera.orbit;
            for &fid in &self.poly_sel.selected_faces {
                if fid as usize >= mesh.faces.len() {
                    continue;
                }
                let fid_t = FaceId(fid);
                if mesh.face(fid_t).half_edge == core_math::half_edge::NO_EDGE {
                    continue;
                }
                let verts = mesh.face_vertices(fid_t);
                if verts.len() < 3 || verts.iter().any(|v| hidden.contains(&v.0)) { continue; }
                // Skip non-finite / runaway verts (stray selection "slash" across the viewport).
                if verts.iter().any(|&v| {
                    let p = mesh.vert(v).position;
                    !p.is_finite()
                }) {
                    continue;
                }
                let local_pts: Vec<Vec3> = verts.iter().map(|&v| mesh.vert(v).position).collect();
                let n = mesh.compute_face_normal(fid_t);
                if !n.is_finite() {
                    continue;
                }
                // FACING IS NOT OCCLUSION. The cull that used to sit here
                // (`!show_back && faces_away(n_world)`) dropped the ENTIRE selected face the
                // moment its normal tipped ~87 degrees off the view direction — which for a
                // boolean notch wall, or any face near the silhouette, happens while the face
                // is plainly visible. That is the owner's "selected faces in the notch lose
                // their green when I orbit" (2026-08-07). Occlusion is the depth test below /
                // in face_overlay.wgsl. Do not reintroduce a dot(normal, view) test.
                // Face diameter for rejecting earclip fan-stall tris that span the whole mesh.
                let mut flo = local_pts[0];
                let mut fhi = local_pts[0];
                for &p in &local_pts {
                    flo = flo.min(p);
                    fhi = fhi.max(p);
                }
                let face_diag = (fhi - flo).length().max(1e-6);
                let max_edge = face_diag * 3.0;
                for tri in core_math::poly_mesh::earclip_polygon(&local_pts, n) {
                    if gpu_fill_live {
                        break; // GPU depth-tested fill owns this face
                    }
                    // Guard earclip indices (should always be valid; skip if corrupt).
                    if tri.iter().any(|&i| i as usize >= local_pts.len()) {
                        continue;
                    }
                    let a = local_pts[tri[0] as usize];
                    let b = local_pts[tri[1] as usize];
                    let c = local_pts[tri[2] as usize];
                    let e0 = (b - a).length();
                    let e1 = (c - b).length();
                    let e2 = (a - c).length();
                    // Fan fallback on non-simple faces can emit a chord across the mesh —
                    // that reads as a bright selection "slash" (owner screenshot).
                    if e0 > max_edge || e1 > max_edge || e2 > max_edge {
                        continue;
                    }
                    if !a.is_finite() || !b.is_finite() || !c.is_finite() {
                        continue;
                    }
                    let tp: Vec<egui::Pos2> = tri.iter().filter_map(|&i| {
                        world_to_screen(xform_pos(local_pts[i as usize]), cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg)
                    }).collect();
                    if tp.len() == 3 {
                        painter.add(egui::Shape::convex_polygon(tp, sel_face_fill, egui::Stroke::NONE));
                    }
                }
                // Per-edge near-plane-clipped outline (not closed_line): filter_map would drop a
                // behind-camera vertex and draw a spurious chord / selection flare.
                let outline_w = (edge_w * 0.5).max(1.0);
                // Outline samples lie ON the face's rim, whose pixels carry the rim EDGE/VERT
                // ids (drawn over the face with depth priority) — accept the whole rim family.
                let rim_accept: std::collections::HashSet<u32> = {
                    let mut s = std::collections::HashSet::new();
                    s.insert(crate::poly_select_gpu::pick_id_face(fid));
                    for he in mesh.face_half_edges(fid_t) {
                        s.insert(crate::poly_select_gpu::pick_id_edge(
                            crate::poly_select_gpu::canonical_he(mesh, he.0),
                        ));
                    }
                    for &v in &verts {
                        s.insert(crate::poly_select_gpu::pick_id_vert(v.0));
                    }
                    s
                };
                for i in 0..verts.len() {
                    let a = xform_pos(mesh.vert(verts[i]).position);
                    let b = xform_pos(mesh.vert(verts[(i + 1) % verts.len()]).position);
                    // Depth (not facing): the boundary segment draws unless something else is
                    // actually in front of it — the face-ID pick buffer at the segment midpoint,
                    // the same test edge mode uses. Per-segment, so a face that is half behind a
                    // neighbour loses only the hidden half, Blender-style.
                    // Same two rules as the edge path: probe ALONG the segment (never a lone
                    // midpoint sitting on the shared rim of both faces), and when the face-ID
                    // buffer is absent draw rather than guess — see the long note there.
                    if !face_xray {
                        if let Some(img) = pick {
                            let mut front_ok = false;
                            for t in [0.2f32, 0.5, 0.8] {
                                let sample = a + (b - a) * t;
                                let Some(sp) = world_to_screen(
                                    sample, cam_pos, cam_orbit, rect, aspect, ortho_hw,
                                    self.render.fov_deg,
                                ) else {
                                    continue;
                                };
                                if img.any_near_r(sp, 3, |id| rim_accept.contains(&id)) {
                                    front_ok = true;
                                    break;
                                }
                            }
                            if !front_ok {
                                continue;
                            }
                        }
                    }
                    if let Some((sa, sb)) = world_segment_to_screen(a, b, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        painter.line_segment([sa, sb], egui::Stroke::new(outline_w, sel_face_outline));
                    }
                }
            }
        }

        // -- Edge mode overlays --
        //
        // MOVED TO THE GPU (owner 2026-08-07: "when I zoom out and all edges are selected, during
        // the movement I see the back edges"). Selected/hovered edge highlights are emitted by
        // `overlay_lines.rs::append_poly_selected_edges` into the depth-tested GPU line batch, so
        // they are occluded by the solid EVERY frame — including while the camera moves, which is
        // exactly when this egui path failed. It could not depth-test itself, so it leaned on the
        // face-ID pick buffer; that buffer only refreshes once the camera settles, and the
        // documented "no depth info => draw" fallback then showed every back edge mid-orbit.
        // X-Ray is honored by the batch itself (`poly_edit_wire_xray` picks the always-on-top
        // pass), which is where "show the hidden ones too" belongs.
        //
        // Kept here: nothing for edge mode. Do NOT reintroduce an egui edge highlight — without a
        // depth buffer it can only guess, and every guess is one of the two bugs above.

        // -- Vertex mode overlays (topmost — above group paint + edge highlights) --
        // Small tight beveled squares (SDF green). Occlusion via GPU face-ID pick buffer so
        // verts behind solid faces do not draw (owner 2026-08-06). `show_back` restores x-ray.
        if self.poly_work_mode == PolyWorkMode::Vertex {
            // Tight cage + select: half-size from poly_sel_vert_size (default 3 → ~5–6 px).
            let cage_half = (vert_r * 0.72).clamp(1.75, 3.25);
            let bevel = 1.2_f32; // soft corner — “tight bevel square”
            let cage_fill = egui::Color32::from_rgb(18, 18, 18);
            let cage_outline = egui::Color32::from_rgb(55, 70, 60);
            // Selected: app SDF green fill + darker green rim (no white halo).
            let sel_fill = sel_vert_color;
            let sel_rim = egui::Color32::from_rgba_unmultiplied(
                (sc[0] * 0.45 * 255.0) as u8,
                (sc[1] * 0.55 * 255.0) as u8,
                (sc[2] * 0.40 * 255.0) as u8,
                255,
            );
            // Unselected cage dots are decoration (Poly Overlays). Selection/hover always draw.
            let show_vert_cage = self.viewport.show_overlays && self.viewport.show_poly_overlays;
            let pick_img = if !show_back {
                self.poly_pick_image_valid()
            } else {
                None
            };
            // Front-face table only when pick is settling — do NOT use it as a pick-miss
            // fallback (that reintroduces see-through verts behind the solid).
            let face_n_world: Option<Vec<Vec3>> = if pick_img.is_none() && !show_back {
                Some(
                    (0..mesh.faces.len() as u32)
                        .map(|fi| {
                            if mesh.face(FaceId(fi)).half_edge == core_math::half_edge::NO_EDGE {
                                return Vec3::ZERO;
                            }
                            let n = mesh.compute_face_normal(FaceId(fi));
                            let w = world_xform * glam::Vec4::new(n.x, n.y, n.z, 0.0);
                            Vec3::new(w.x, w.y, w.z).normalize_or_zero()
                        })
                        .collect(),
                )
            } else {
                None
            };
            let vert_visible = |vid: u32, sp: egui::Pos2| -> bool {
                if show_back {
                    return true;
                }
                if let Some(img) = pick_img {
                    // Depth occlusion: the vert's OWN depth-resolved id (drawn with priority
                    // bias over its faces) — no incident-face inference. Wider probe (r=3) so
                    // silhouette corners still resolve under raster slop.
                    return img.any_near_r(sp, 3, |id| {
                        id == crate::poly_select_gpu::pick_id_vert(vid)
                    });
                }
                // Pick buffer settling (orbit / first frame / mid-edit): front-face only so
                // selection does not vanish for a whole drag — never pure-hide (old default).
                if let Some(ref ns) = face_n_world {
                    for &h in &mesh.vertex_fan(VertexId(vid)) {
                        let f = mesh.he(h).face;
                        if f == core_math::half_edge::NO_FACE {
                            continue;
                        }
                        let n = ns.get(f.0 as usize).copied().unwrap_or(Vec3::ZERO);
                        if n != Vec3::ZERO && !faces_away(n) {
                            return true;
                        }
                    }
                    return false;
                }
                true
            };
            let draw_vert_sq = |painter: &egui::Painter, sp: egui::Pos2, half: f32, fill: egui::Color32, rim: egui::Color32, rim_w: f32| {
                let r = egui::Rect::from_center_size(sp, egui::vec2(half * 2.0, half * 2.0));
                painter.rect_filled(r, bevel, fill);
                painter.rect_stroke(r, bevel, egui::Stroke::new(rim_w, rim), egui::StrokeKind::Middle);
            };
            if show_vert_cage {
                for i in 0..mesh.vertices.len() as u32 {
                    if mesh.vert(VertexId(i)).half_edge == core_math::half_edge::NO_EDGE {
                        continue;
                    }
                    if hidden.contains(&i) {
                        continue;
                    }
                    if self.poly_sel.selected_vertices.contains(&i) {
                        continue;
                    }
                    if self.poly_sel.hovered_vertex == Some(i) {
                        continue;
                    }
                    let pos = xform_pos(mesh.vert(VertexId(i)).position);
                    if let Some(sp) = world_to_screen(
                        pos,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    ) {
                        if !vert_visible(i, sp) {
                            continue;
                        }
                        draw_vert_sq(painter, sp, cage_half, cage_fill, cage_outline, 0.75);
                    }
                }
            }
            for &vid in &self.poly_sel.selected_vertices {
                if hidden.contains(&vid) {
                    continue;
                }
                let pos = xform_pos(mesh.vert(VertexId(vid)).position);
                if let Some(sp) = world_to_screen(
                    pos,
                    cam_pos,
                    self.viewport.camera.orbit,
                    rect,
                    aspect,
                    ortho_hw,
                    self.render.fov_deg,
                ) {
                    if !vert_visible(vid, sp) {
                        continue;
                    }
                    let half = cage_half.max(2.0);
                    draw_vert_sq(painter, sp, half, sel_fill, sel_rim, 1.0);
                }
            }
            if let Some(vid) = self.poly_sel.hovered_vertex {
                if !self.poly_sel.selected_vertices.contains(&vid) {
                    let pos = xform_pos(mesh.vert(VertexId(vid)).position);
                    if let Some(sp) = world_to_screen(
                        pos,
                        cam_pos,
                        self.viewport.camera.orbit,
                        rect,
                        aspect,
                        ortho_hw,
                        self.render.fov_deg,
                    ) {
                        if vert_visible(vid, sp) {
                            let half = (cage_half * 1.05).max(2.0);
                            draw_vert_sq(
                                painter,
                                sp,
                                half,
                                hover_vert_color,
                                egui::Color32::from_rgb(40, 40, 30),
                                0.9,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Draw loop cut preview (yellow highlight on the edge loop that will be cut).
    pub(crate) fn draw_loop_cut_preview(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.poly_tool != PolyTool::LoopCut { return; }
        if self.modeling_context != ModelingContext::Polygonal { return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };

        // Read the live session (start_edge/count/factor/paired/limit) if one is active, else fall back
        // to the hovered edge at centre. In LIVE mode the real cut geometry is shown, so skip the line
        // preview entirely.
        // `rings` is the whole session: one entry for a hover cut, several when a multi-edge
        // selection spans independent rings. Previewing only the primary would show one yellow
        // line while confirm inserts N loops.
        let (rings, count, factor, paired, live, orig_mesh, curvature) =
            if let Some(d) = self.tool.current.as_poly_loop_cut_slide() {
                (
                    Self::loop_cut_session_rings(d),
                    d.count,
                    d.factor,
                    d.paired,
                    d.live,
                    Some(d.original_mesh.clone()),
                    d.curvature,
                )
            } else {
                let he = match self.poly_sel.hovered_edge {
                    Some(e) => e,
                    None => return,
                };
                (
                    vec![PolyLoopCutRing { start_edge: he, limit: None, segment_mode: false }],
                    self.loop_cut_count.max(1),
                    0.0f32,
                    false,
                    false,
                    None,
                    self.loop_cut_effective_curvature(),
                )
            };
        if live {
            return;
        }

        let node = match self.scene.scene_graph.get(nid) {
            Some(n) => n,
            None => return,
        };
        let node_mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };
        // Preview rails come from the pre-cut snapshot (session) or the unmodified node mesh.
        let mesh: &core_math::half_edge::HalfEdgeMesh = orig_mesh.as_deref().unwrap_or(node_mesh);

        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform_pos = |local: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };
        let fractions = Self::loop_cut_fractions(count, factor, paired);
        let preview_color = egui::Color32::from_rgb(255, 230, 50); // bright yellow
        let project = |p: Vec3| {
            world_to_screen(
                xform_pos(p),
                cam_pos,
                self.viewport.camera.orbit,
                rect,
                aspect,
                ortho_hw,
                self.render.fov_deg,
            )
        };

        for ring in &rings {
            if ring.segment_mode {
                // No loop: cut points on each selected edge + connect lines across shared faces.
                let edges = Self::loop_cut_segment_edges(ring);
                // (src, dst) VertexId pairs, twin-deduped.
                let mut pairs: Vec<(core_math::half_edge::VertexId, core_math::half_edge::VertexId)> =
                    Vec::new();
                let mut seen = std::collections::HashSet::new();
                for &eid in &edges {
                    let he = HalfEdgeId(eid);
                    if !mesh.is_edge_live(he) {
                        continue;
                    }
                    if !seen.insert(mesh.canonical_edge(he).0) {
                        continue;
                    }
                    pairs.push((mesh.he_source(he), mesh.he(he).vertex));
                }
                for &t in &fractions {
                    let pts: Vec<Vec3> = pairs
                        .iter()
                        .map(|&(s, d)| mesh.vert(s).position.lerp(mesh.vert(d).position, t))
                        .collect();
                    for &p in &pts {
                        if let Some(sp) = project(p) {
                            painter.circle_filled(sp, 4.0, preview_color);
                        }
                    }
                    // Connect cut points that share a face → preview of the real cut segment.
                    for i in 0..pairs.len() {
                        for j in (i + 1)..pairs.len() {
                            let (s0, d0) = pairs[i];
                            let (s1, d1) = pairs[j];
                            let mut share = false;
                            for fi in 0..mesh.faces.len() as u32 {
                                let fid = core_math::half_edge::FaceId(fi);
                                if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE {
                                    continue;
                                }
                                let ring = mesh.face_vertices(fid);
                                let e0 = ring.contains(&s0) && ring.contains(&d0);
                                let e1 = ring.contains(&s1) && ring.contains(&d1);
                                if e0 && e1 {
                                    share = true;
                                    break;
                                }
                            }
                            if share {
                                if let (Some(sa), Some(sb)) = (project(pts[i]), project(pts[j])) {
                                    painter.line_segment(
                                        [sa, sb],
                                        egui::Stroke::new(2.5, preview_color),
                                    );
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // Loop mode: per crossed quad, join entry point to exit point at each cut fraction.
            // Both come from `rail_curve_point`, the SAME function the cut uses, so a
            // follow-curvature preview bulges exactly where the real loop will land — a straight
            // preview over a curved cut would be a visual lie.
            let rails =
                mesh.loop_cut_preview_rail_verts(HalfEdgeId(ring.start_edge), ring.limit.as_ref());
            for &t in &fractions {
                for &((eb, et), (xb, xt)) in &rails {
                    let a = mesh.rail_curve_point(eb, et, t, curvature);
                    let b = mesh.rail_curve_point(xb, xt, t, curvature);
                    if let (Some(sa), Some(sb)) = (project(a), project(b)) {
                        painter.line_segment([sa, sb], egui::Stroke::new(2.5, preview_color));
                        painter.circle_filled(sa, 3.0, preview_color);
                        painter.circle_filled(sb, 3.0, preview_color);
                    }
                }
            }
        }
    }

    /// Live bevel feedback near the cursor only (width / profile / clamp).
    /// Editable fields live in Tool Parameters (`draw_tool_params_poly_bevel`) — no second
    /// floating "Bevel" window (same dock-only rule as create adjust / extrude / inset).
    /// NOTE: the Consume Geometry toggle master added to the old floating panel moved to
    /// Tool Parameters with the rest — it is not lost, just re-homed.
    pub(crate) fn draw_bevel_hud(&mut self, ctx: &egui::Context, _rect: egui::Rect) {
        let (width, segments, shape, clamp) = match self.tool.current.as_poly_bevel() {
            Some(d) => (d.width, d.segments, d.shape, d.clamp),
            None => return,
        };

        // Live width/profile label near the cursor (visual feedback only).
        // Shape mapping matches BevelParams / Blender profile=0.5 circle:
        //   0 = flat chamfer, 0.5 = circular round (DEFAULT), 1 = convex bulge.
        // Segments=1 is always a flat strip regardless of shape; call that out.
        let orange = egui::Color32::from_rgb(255, 165, 0);
        // Prefer the last rebuild's effective segment count (may be forced to 1).
        let eff_seg = core_math::bevel_op::last_bevel_effective_segments().max(1);
        let forced_flat = core_math::bevel_op::last_bevel_forced_flat();
        let profile_label = if eff_seg <= 1 || forced_flat {
            "Chamfer"
        } else if shape < 0.2 {
            "Flat"
        } else if shape < 0.8 {
            "Round"
        } else {
            "Convex"
        };
        let mouse = self.viewport.viewport_mouse;
        let width_clamped = clamp && core_math::bevel_op::last_bevel_width_was_clamped();
        let label = if forced_flat && segments > 1 {
            format!("{:.3}  {}  (round blocked)", width, profile_label)
        } else if width_clamped {
            let eff = core_math::bevel_op::last_bevel_effective_width();
            format!("{eff:.3}  {}  ×{}  (clamped — C=wider)", profile_label, segments)
        } else {
            format!("{:.3}  {}  ×{}", width, profile_label, segments)
        };
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("bevel_hud_label")))
            .text(
                egui::pos2(mouse.x + 20.0, mouse.y - 20.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::proportional(12.0),
                orange,
            );
    }

    /// Draw extrude HUD: offset value near cursor.
    /// Settings are in Tool Properties panel (workspace.rs).
    pub(crate) fn draw_extrude_hud(&self, painter: &egui::Painter, _rect: egui::Rect) {
        let d = match self.tool.current.as_poly_extrude() { Some(d) => d, None => return };
        let orange = egui::Color32::from_rgb(255, 165, 0);
        let mouse = self.viewport.viewport_mouse;
        let mode = if d.along_normals {
            let gi = if d.along_normals_grouped { "G" } else { "I" };
            if d.force_planar {
                format!("{gi}·□") // □ = force planar
            } else {
                gi.to_string()
            }
        } else {
            "R".into()
        };
        painter.text(
            egui::pos2(mouse.x + 20.0, mouse.y - 20.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{mode}  {:.3}", d.accumulated_offset),
            egui::FontId::proportional(12.0),
            orange,
        );
    }

    /// Draw inset HUD: amount value near cursor.
    /// Settings are in Tool Properties panel (workspace.rs).
    pub(crate) fn draw_inset_hud(&self, painter: &egui::Painter, _rect: egui::Rect) {
        let d = match self.tool.current.as_poly_inset() { Some(d) => d, None => return };
        let orange = egui::Color32::from_rgb(255, 165, 0);
        let mouse = self.viewport.viewport_mouse;
        painter.text(
            egui::pos2(mouse.x + 20.0, mouse.y - 20.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.3}", d.accumulated_amount),
            egui::FontId::proportional(12.0),
            orange,
        );
    }

    /// Draw knife tool visual feedback: placed points, connecting lines, snap preview.
    pub(crate) fn draw_knife_overlay(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.poly_tool != PolyTool::Knife { return; }
        // While R/C shape is sizing: HUD only. Polyline points + outline are drawn ungated in
        // `draw_knife_shape_select_overlay` so scroll-beveled corners stay visible even when
        // poly overlays are off (owner: scroll bevels rect corners AND polyline points).
        if self.knife_shape_kind != 0 && self.knife_shape_anchor.is_some() {
            let kind = if self.knife_shape_kind == 1 { "RECT" } else { "CIRCLE" };
            // Depth phase owns the HUD: the sizing hints (Space/R/array/scroll) are all dead
            // once the shape is locked, and listing dead keys is worse than listing none.
            if self.knife_shape_depth_mode {
                // The depth is SIGNED (in = pocket, out = boss), so the number alone is ambiguous
                // at a glance — name the direction and the op that will consume it.
                let d = self.knife_shape_depth;
                let dir = if d < 0.0 { "OUT" } else { "IN" };
                let op = match self.knife_bool_op {
                    crate::poly_knife::KnifeBoolOp::Subtract => "SUBTRACT",
                    crate::poly_knife::KnifeBoolOp::Union => "UNION",
                    crate::poly_knife::KnifeBoolOp::Intersect => "INTERSECT",
                };
                painter.text(
                    egui::pos2(rect.center().x, rect.max.y - 30.0),
                    egui::Align2::CENTER_BOTTOM,
                    format!(
                        "Knife {kind} {op} · DEPTH {dir} {:.3} · MMB-orbit to look · LMB-drag = depth · release = cut · Esc = cancel",
                        d.abs()
                    ),
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_rgb(255, 160, 40),
                );
                return;
            }
            let center = if self.knife_shape_from_center {
                " · from CENTER (Alt)"
            } else {
                ""
            };
            let face_sel = !self.poly_sel.selected_faces.is_empty();
            let commit = match self.knife_bool_op {
                crate::poly_knife::KnifeBoolOp::Union => " · release = ADD (U) · E = height",
                crate::poly_knife::KnifeBoolOp::Intersect => " · release = KEEP inside (I)",
                _ if face_sel => " · release = slice selected",
                _ => " · release = hole · E = depth · S = slice · U/I = add/keep",
            };
            let extra = if self.knife_shape_kind == 2 {
                format!(" · {} segs (scroll) · Space=move", self.knife_shape_segments)
            } else if self.knife_shape_kind == 1 {
                let pct = (self.knife_shape_corner_bevel * 100.0).round() as i32;
                let deg = self.knife_shape_rotation_rad.to_degrees();
                let arr = if self.knife_shape_array_count >= 2 {
                    format!(" · array ×{} (scroll)", self.knife_shape_array_count)
                } else {
                    " · Ctrl+D=array".to_string()
                };
                format!(
                    " · bevel {pct}% · {} segs · rot {deg:.0}°{arr} · Space=move · R · Ctrl=snap {}",
                    self.knife_shape_segments,
                    self.viewport.snap_rotation_deg
                )
            } else {
                String::new()
            };
            painter.text(
                egui::pos2(rect.center().x, rect.max.y - 30.0),
                egui::Align2::CENTER_BOTTOM,
                format!("Knife {kind}{center}{commit}{extra}"),
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(80, 255, 120),
            );
            return;
        }
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };

        let yellow = egui::Color32::from_rgb(255, 220, 50);
        let red = egui::Color32::from_rgb(255, 80, 80);
        let white = egui::Color32::WHITE;

        // Draw placed points as circles and connecting lines
        let mut prev_sp: Option<egui::Pos2> = None;
        for &pt in &self.knife_points {
            if let Some(sp) = world_to_screen(pt, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                painter.circle_filled(sp, 4.0, yellow);
                painter.circle_stroke(sp, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
                if let Some(prev) = prev_sp {
                    painter.line_segment([prev, sp], egui::Stroke::new(2.0, yellow));
                }
                prev_sp = Some(sp);
            }
        }

        // (The cut-depth extrusion preview lives in `draw_knife_shape_select_overlay` — this
        // function early-returns whenever a shape is anchored, which is exactly when the depth
        // phase is live, so a preview drawn here would never reach the screen.)

        // Draw preview line + the snap-state marker. THE SNAP KIND MUST BE UNMISTAKABLE (Blender's
        // knife draws distinct markers + the target edge; ours drew one red circle for everything —
        // the owner placed a mid-face point while certain it was edge-snapped):
        //   VERTEX  → green filled SQUARE
        //   EDGE    → cyan circle + the snapped edge itself highlighted cyan
        //   ANGLE   → orange diamond (straight + angle snap from snap menu / Ctrl)
        //   FREE    → red circle (= this point will land MID-FACE, on no edge)
        let green = egui::Color32::from_rgb(80, 230, 90);
        let cyan = egui::Color32::from_rgb(60, 200, 255);
        let orange = egui::Color32::from_rgb(255, 160, 40);
        if let Some(snap) = self.knife_snap_pos {
            if let Some(snap_sp) = world_to_screen(snap, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                // Highlight the snapped edge (kind==2) so "which edge" is visible, not implied.
                if let Some((e0, e1)) = self.knife_snap_edge {
                    let s0 = world_to_screen(e0, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
                    let s1 = world_to_screen(e1, cam_pos, self.viewport.camera.orbit, rect, aspect, ortho_hw, self.render.fov_deg);
                    if let (Some(a), Some(b)) = (s0, s1) {
                        painter.line_segment([a, b], egui::Stroke::new(2.5, cyan));
                    }
                }
                match self.knife_snap_kind {
                    1 => { // vertex: green square
                        let r = egui::Rect::from_center_size(snap_sp, egui::vec2(11.0, 11.0));
                        painter.rect_filled(r, 1.0, green);
                        painter.rect_stroke(r, 1.0, egui::Stroke::new(1.5, egui::Color32::BLACK), egui::StrokeKind::Middle);
                    }
                    2 => { // edge: cyan circle
                        painter.circle_filled(snap_sp, 5.5, cyan);
                        painter.circle_stroke(snap_sp, 5.5, egui::Stroke::new(1.5, egui::Color32::BLACK));
                    }
                    3 => { // angle-constrained straight: orange diamond
                        let r = 6.0;
                        let pts = [
                            snap_sp + egui::vec2(0.0, -r),
                            snap_sp + egui::vec2(r, 0.0),
                            snap_sp + egui::vec2(0.0, r),
                            snap_sp + egui::vec2(-r, 0.0),
                        ];
                        painter.add(egui::Shape::convex_polygon(pts.to_vec(), orange, egui::Stroke::new(1.5, egui::Color32::BLACK)));
                    }
                    4 => { // close-loop lock: ringed target on the FIRST point
                        painter.circle_filled(snap_sp, 6.5, egui::Color32::from_rgb(255, 220, 50));
                        painter.circle_stroke(snap_sp, 6.5, egui::Stroke::new(2.0, egui::Color32::BLACK));
                        painter.circle_stroke(snap_sp, 11.0, egui::Stroke::new(2.0, white));
                    }
                    _ => { // free on a face: red = "this will NOT land on an edge"
                        painter.circle_filled(snap_sp, 4.5, red);
                        painter.circle_stroke(snap_sp, 4.5, egui::Stroke::new(1.5, white));
                    }
                }
                // Preview line (solid when angle-constrained — "straight line" feedback)
                if let Some(prev) = prev_sp {
                    let stroke = if self.knife_snap_kind == 3 {
                        egui::Stroke::new(2.0, orange)
                    } else {
                        egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(255, 220, 50, 150))
                    };
                    painter.line_segment([prev, snap_sp], stroke);
                }
            }
        }

        // HUD text — states the snap kind in words as well as color.
        let pts = self.knife_points.len();
        let ang = self.viewport.snap_rotation_deg;
        let snap_lbl = match (self.knife_snap_pos.is_some(), self.knife_snap_kind) {
            (false, _) => String::new(),
            (true, 1) => "  [SNAP: vertex]".into(),
            (true, 2) => "  [SNAP: edge]".into(),
            (true, 4) => "  [CLOSE LOOP: double-click]".into(),
            (true, 3) => {
                // Ctrl = H/V axis; magnet = angle increments — same marker, different label.
                if ang > 0.05 {
                    format!("  [STRAIGHT / {ang:.0}°]")
                } else {
                    "  [STRAIGHT H/V]".into()
                }
            }
            (true, _) => "  [free: mid-face]".into(),
        };
        let hint = if pts == 0 {
            "Knife: Click to place first point"
        } else if pts >= 3 {
            "Knife: Click | dbl-click start = close | Ctrl=H/V | Shift/magnet=angle ° | Enter: cut | Esc: cancel"
        } else {
            "Knife: Click | Ctrl=H/V | Shift/magnet=angle ° | Enter: cut | Esc: cancel"
        };
        painter.text(
            egui::pos2(rect.center().x, rect.max.y - 30.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{} ({} points){}", hint, pts, snap_lbl),
            egui::FontId::proportional(14.0),
            match self.knife_snap_kind {
                1 => green,
                2 => cyan,
                3 => orange,
                _ => yellow,
            },
        );
    }
}
