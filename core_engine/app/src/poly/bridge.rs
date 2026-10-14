use std::collections::HashSet;
use std::sync::Arc;
use glam::Vec3;
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_TWIN, NO_FACE, NO_EDGE};
use core_math::scene_graph::NodePayload;
use crate::{AzraApp, DirtyFlags, PolyWorkMode};

impl AzraApp {
    /// Bridge: prefer Blender **Bridge Edge Loops** when Edge mode has two closed equal-length loops;
    /// **two selected faces** open both holes and strip a tunnel between their rings; otherwise keep
    /// the legacy 1-vert / 2-vert / 2-edge "make a quad" fabrications.
    ///
    /// Refuse-before-transaction: no undo entry on empty/wrong selection or failed geometry
    /// (same honesty as fill / inset / circle).
    pub(crate) fn poly_bridge(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };

        let sel_verts: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
        let sel_edges: Vec<u32> = self.poly_sel.selected_edges.iter().copied().collect();
        let mut sel_faces: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
        sel_faces.sort_unstable();

        // Face path: exactly two faces → delete them and bridge their boundary rings into a tunnel
        // (Blender Bridge Edge Loops on a two-face selection). Priority over edge/legacy when the
        // user has two faces picked (Face mode or mixed). Closed solids whose side walls already
        // connect the rings fall back to delete-only (open the through-tunnel).
        if sel_faces.len() >= 2 {
            let fids: Vec<FaceId> = sel_faces.iter().map(|&f| FaceId(f)).collect();
            let preview = {
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
                let mut scratch = (**mesh).clone();
                scratch.bridge_faces_multi(&fids)
            };
            if let Some(new_faces) = preview {
                let nf = new_faces.len();
                self.transaction("Bridge Faces", |s| {
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                            let mesh = Arc::make_mut(mesh);
                            let faces = mesh.bridge_faces_multi(&fids).unwrap_or_default();
                            s.poly_sel.selected_faces.clear();
                            s.poly_sel.selected_edges.clear();
                            s.poly_sel.selected_vertices.clear();
                            for f in &faces { s.poly_sel.selected_faces.insert(f.0); }
                            s.poly_request_display_sync(nid);
                            s.mark_dirty(DirtyFlags::RENDER);
                            if nf == 0 {
                                s.toast("Bridge: opened tunnel (caps removed)");
                            } else {
                                s.toast(&format!("Bridged {} faces", nf));
                            }
                        }
                    }
                });
                return;
            }
            self.toast("Bridge Faces: need two or more non-adjacent faces with openable loops");
            return;
        }

        // Fast path: two edge loops → real bridge_edge_selection (kernel).
        if self.poly_work_mode == PolyWorkMode::Edge && sel_edges.len() >= 6 {
            let edge_set: HashSet<HalfEdgeId> = sel_edges.iter().map(|&e| HalfEdgeId(e)).collect();
            // `Ok(n)` = n faces will be created; `Err(msg)` = refuse WITH A REASON (never a silent
            // no-op — a silent fall-through to the legacy path is what made the multi-loop failure
            // invisible to the owner in the first place).
            let nfaces: Result<usize, &'static str> = {
                let Some(node) = self.scene.scene_graph.get(nid) else { return };
                let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
                let rings = mesh.edge_loop_rings(&edge_set);
                // N >= 2 loops in ONE invocation: `bridge_edge_selection` chains them into N-1
                // bridges (Blender parity, oracle `bridge_multi_ref.py`). This used to demand
                // EXACTLY two, which is what made the tool dead on a multi-selection.
                if rings.len() < 2 || rings.iter().any(|r| r.len() < 3) {
                    Err("not_loops")
                } else {
                    let mut scratch = (**mesh).clone();
                    let faces = scratch.bridge_edge_selection(&edge_set);
                    // audit #3 (half_edge red line): never install a NON-MANIFOLD bridge — bridging two
                    // interior (already 2-faced) loops duplicates directed edges / orphans twins that
                    // validate() misses, and it would persist to the .fl4. Gate on is_manifold (like boolean.rs).
                    //
                    // This gate is a DELIBERATE divergence from Blender for N > 2 (BRIDGE.md §3).
                    // Chaining N rings that each already bound a face gives every INTERIOR ring 3
                    // faces per edge; Blender ships that (`bridge_multi_ref.py` records nonman=8 for
                    // 3 rings, 16 for 4), AZRA refuses it. The manifold-clean N-loop case is the
                    // FACE selection, which deletes the caps first — see `bridge_faces_multi`.
                    if faces.is_empty() {
                        Err("no_faces")
                    } else if scratch.is_manifold().is_err() {
                        Err("nonmanifold")
                    } else {
                        Ok(faces.len())
                    }
                }
            };
            if let Ok(nf) = nfaces {
                self.transaction("Bridge Edge Loops", |s| {
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                            let mesh = Arc::make_mut(mesh);
                            let faces = mesh.bridge_edge_selection(&edge_set);
                            s.poly_sel.selected_faces.clear();
                            s.poly_sel.selected_edges.clear();
                            for f in &faces { s.poly_sel.selected_faces.insert(f.0); }
                            s.poly_request_display_sync(nid);
                            s.mark_dirty(DirtyFlags::RENDER);
                            s.toast(&format!("Bridged {} faces", nf));
                        }
                    }
                });
                return;
            }
            // A refusal that the user can ACT on. Only "not_loops" may fall through to the legacy
            // 1-vert / 2-vert / 2-edge fabrications — a real ring selection that we declined for
            // geometric reasons must say so, not silently try to make an unrelated quad.
            match nfaces {
                Err("nonmanifold") => {
                    self.toast("Bridge: that chain would be non-manifold — in Face mode, \
                                select the caps instead so they are removed first");
                    return;
                }
                Err("no_faces") => {
                    self.toast("Bridge Edge Loops: loops overlap or share vertices");
                    return;
                }
                _ => {} // "not_loops" → legacy path below
            }
        }

        // Legacy path: dry-run on a clone first.
        let cam_forward = self.viewport.camera.orbit * Vec3::new(0.0, 0.0, -1.0);
        let can_legacy = {
            let Some(node) = self.scene.scene_graph.get(nid) else { return };
            let NodePayload::PolygonalBody { mesh, .. } = &node.payload else { return };
            let mut scratch = (**mesh).clone();
            try_legacy_bridge(&mut scratch, &sel_verts, &sel_edges, cam_forward)
        };
        if !can_legacy {
            if self.poly_work_mode == PolyWorkMode::Face && sel_faces.len() < 2 {
                self.toast("Bridge Faces: select two or more faces");
            } else if self.poly_work_mode == PolyWorkMode::Edge && sel_edges.len() >= 3 {
                // Neither "equal-length" nor "exactly two" is a constraint any more: unequal rings
                // fan (`bridge_unequal_loops`) and N rings chain (`bridge_ring_chain`). What still
                // refuses is a selection that is not a set of closed rings, or one whose bridge
                // would be non-manifold — so say that instead of a constraint that no longer exists.
                self.toast("Bridge Edge Loops: need two or more closed loops with a manifold bridge");
            } else if sel_verts.len() < 1 && sel_edges.len() != 2 {
                self.toast("Bridge: select 2+ faces, 1–2 verts, 2 edges, or 2+ edge loops");
            } else {
                self.toast("Bridge: could not create face");
            }
            return;
        }

        self.transaction("Bridge", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    let ok = try_legacy_bridge(mesh, &sel_verts, &sel_edges, cam_forward);
                    if ok {
                        s.poly_request_display_sync(nid);
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.toast("Bridge face created");
                    }
                }
            }
        });
    }
}

/// Legacy 1-vert / 2-vert / 2-edge bridge. Mutates `mesh`. Returns true if a face was created.
fn try_legacy_bridge(
    mesh: &mut HalfEdgeMesh,
    sel_verts: &[u32],
    sel_edges: &[u32],
    cam_forward: Vec3,
) -> bool {
    if sel_verts.len() == 1 {
        let vid = VertexId(sel_verts[0]);
        let fan = mesh.vertex_fan(vid);
        if fan.is_empty() { return false; }
        let he_id = fan.iter().find(|&&h| mesh.he(h).twin == NO_TWIN)
            .copied().unwrap_or(fan[0]);
        let dst = mesh.he(he_id).vertex;
        let src_pos = mesh.vert(vid).position;
        let dst_pos = mesh.vert(dst).position;
        let edge_dir = dst_pos - src_pos;
        let edge_len = edge_dir.length();
        if edge_len < 1e-6 { return false; }
        let dir = edge_dir / edge_len;
        let outward = camera_oriented_outward(dir, cam_forward, mesh.compute_vertex_normal(vid));

        let v0 = vid;
        let v3 = dst;
        let v1 = mesh.alloc_vertex(core_math::half_edge::HVertex {
            position: src_pos + outward * edge_len, normal: Vec3::Y, half_edge: NO_EDGE,
        });
        let v2 = mesh.alloc_vertex(core_math::half_edge::HVertex {
            position: dst_pos + outward * edge_len, normal: Vec3::Y, half_edge: NO_EDGE,
        });

        mesh.create_face(&[v0, v1, v2, v3]);
        mesh.recompute_normals();
        true
    } else if sel_verts.len() == 2 {
        let va = VertexId(sel_verts[0]);
        let vb = VertexId(sel_verts[1]);
        let pa = mesh.vert(va).position;
        let pb = mesh.vert(vb).position;
        let edge_dir = pb - pa;
        let edge_len = edge_dir.length();
        if edge_len < 1e-6 { return false; }
        let dir = edge_dir / edge_len;
        let na = mesh.compute_vertex_normal(va);
        let nb = mesh.compute_vertex_normal(vb);
        let avg_normal = ((na + nb) * 0.5).normalize_or_zero();
        let outward = camera_oriented_outward(dir, cam_forward, avg_normal);

        let vc = mesh.alloc_vertex(core_math::half_edge::HVertex {
            position: pa + outward * edge_len, normal: Vec3::Y, half_edge: NO_EDGE,
        });
        let vd = mesh.alloc_vertex(core_math::half_edge::HVertex {
            position: pb + outward * edge_len, normal: Vec3::Y, half_edge: NO_EDGE,
        });

        mesh.create_face(&[va, vc, vd, vb]);
        mesh.recompute_normals();
        true
    } else if sel_edges.len() == 2 {
        let he_a = HalfEdgeId(sel_edges[0]);
        let he_b = HalfEdgeId(sel_edges[1]);
        if !mesh.is_edge_live(he_a) || !mesh.is_edge_live(he_b) { return false; }
        let src_a = mesh.he_source(he_a);
        let dst_a = mesh.he(he_a).vertex;
        let src_b = mesh.he_source(he_b);
        let dst_b = mesh.he(he_b).vertex;
        let pa = mesh.vert(src_a).position;
        let pb = mesh.vert(src_b).position;
        let pc = mesh.vert(dst_b).position;

        // Anti-bowtie: nearest-endpoint compare picks the endpoint of
        // edge B that continues the loop without self-intersection.
        let ring = if (pa - pb).length_squared() < (pa - pc).length_squared() {
            [src_a, dst_a, dst_b, src_b]
        } else {
            [src_a, dst_a, src_b, dst_b]
        };

        if let Some(fid) = mesh.create_face(&ring) {
            mesh.recompute_normals();

            // K73: winding must match adjacent geometry, not selection order.
            let mut ref_n = Vec3::ZERO;
            for &he in &[he_a, he_b] {
                let owner = mesh.he(he).face;
                let twin = mesh.he(he).twin;
                let f = if owner != NO_FACE && owner != fid {
                    Some(owner)
                } else if twin != NO_TWIN {
                    let tf = mesh.he(twin).face;
                    if tf != NO_FACE && tf != fid { Some(tf) } else { None }
                } else { None };
                if let Some(f) = f { ref_n += mesh.compute_face_normal(f); }
            }
            ref_n = ref_n.normalize_or_zero();

            let new_n = mesh.compute_face_normal(fid);
            let flip = if ref_n.length_squared() > 1e-6 {
                new_n.dot(ref_n) < 0.0
            } else {
                new_n.dot(-cam_forward) < 0.0
            };
            if flip {
                mesh.flip_face(fid);
                mesh.recompute_normals();
            }
            true
        } else {
            false
        }
    } else {
        false
    }
}

/// Compute an outward direction that faces the camera.
fn camera_oriented_outward(edge_dir: Vec3, cam_forward: Vec3, normal: Vec3) -> Vec3 {
    let cam_right = cam_forward.cross(Vec3::Y).normalize_or_zero();
    let projected = (cam_right - edge_dir * cam_right.dot(edge_dir)).normalize_or_zero();

    if projected.length_squared() < 0.01 {
        let fallback = normal.cross(edge_dir).normalize_or_zero();
        if fallback.length_squared() < 0.01 {
            let up = if edge_dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
            edge_dir.cross(up).normalize()
        } else {
            fallback
        }
    } else {
        if projected.dot(-cam_forward) > 0.0 { projected } else { -projected }
    }
}
