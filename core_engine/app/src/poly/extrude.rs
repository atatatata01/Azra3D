use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui;
use glam::Vec3;

use core_math::half_edge::{FaceId, VertexId};
use core_math::scene_graph::NodePayload;
use core_math::mesh_ops::PolyEdit;

use crate::{
    ActiveTool, AzraApp, DirtyFlags, PolyEdgeExtrudeDrag, PolyTool, world_to_screen,
};

impl AzraApp {
    /// Record an EXTRUDE's near-tangent seams into the Bevel Smart Select sidecar (owner
    /// 2026-08-05: "extrude: we should use angle based to tag or not, maybe all 20 degree under
    /// are tagged out of bevel").
    ///
    /// ANGLE-BASED, not blanket: a normal extrude's walls meet the cap and the surrounding
    /// surface at a real crease (~90°) that the user still wants beveled, so only the seams that
    /// come out nearly FLUSH — deviation below
    /// [`EXTRUDE_TAG_TANGENT_DEG`](crate::poly::mesh_sync::EXTRUDE_TAG_TANGENT_DEG) — are
    /// excluded. That band deliberately includes EXACTLY-coplanar edges: the modifier's geometry
    /// rule leaves coplanar edges beveled (a knife cut is coplanar and must keep beveling), so
    /// provenance is the ONLY thing that can separate a flush extrude seam from a knife cut.
    /// Do not "fix" that to match the modifier's coplanar exemption.
    ///
    /// Must be called when the offset is FINAL — at zero offset every wall is degenerate and
    /// `edge_dihedral_cos` would report a meaningless angle (or none at all).
    ///
    /// Stamped by ID with NO position remap: `extrude_region` only ALLOCATES (it never calls
    /// `delete_face`), so every pre-existing id is still valid — whereas remapping by position
    /// would DROP the tags on the whole cap, which the extrude just moved.
    pub(crate) fn poly_extrude_stamp_tags(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        created_faces: &[u32],
    ) {
        if created_faces.is_empty() {
            return;
        }
        let Some(mesh) = self.poly_node_mesh(nid) else { return };
        let edges = crate::poly::op_tags::edges_of_faces(&mesh, created_faces);
        let tagged = crate::poly::op_tags::retain_near_tangent(
            &mesh,
            &edges,
            crate::poly::mesh_sync::EXTRUDE_TAG_TANGENT_DEG,
        );
        eprintln!(
            "[EXTRUDE] smart select: tagged {} of {} created edges (seams under {}°)",
            tagged.len(),
            edges.len(),
            crate::poly::mesh_sync::EXTRUDE_TAG_TANGENT_DEG,
        );
        if tagged.is_empty() {
            return;
        }
        let cd = self.poly_cd_mut(nid); // the ONE sidecar-ensure helper (poly_groups.rs)
        for he in tagged {
            cd.op_created.set(he, true);
        }
    }

    /// One-shot Extrude Along Normals (menu / command): extrude topology then slide cap verts
    /// along blended per-face normals by `offset`. Prefer the interactive Alt+E drag for UX.
    pub(crate) fn poly_extrude_faces_along_normals(&mut self, offset: f32) {
        use core_math::half_edge::VertexId;
        use std::collections::HashMap;
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }
        if offset.abs() < 1e-8 {
            self.toast("Extrude Along Normals: zero offset");
            return;
        }
        let face_n: Vec<(u32, glam::Vec3)> = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            faces.iter().map(|&f| (f, mesh.compute_face_normal(FaceId(f)))).collect()
        };
        // SMART SELECT: hold the PRE-extrude mesh so the created faces can be identified by
        // "live now, not live before" (exact — `extrude_region` only allocates).
        let pre = self.poly_node_mesh(nid);
        self.transaction("Extrude Along Normals", |s| {
            if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let m = std::sync::Arc::make_mut(mesh);
                    let set: std::collections::HashSet<FaceId> =
                        faces.iter().map(|&f| FaceId(f)).collect();
                    // Region extrude for all counts (Blender E). Caps may be new faces on open meshes.
                    let caps = m.extrude_region(&set, glam::Vec3::ZERO);
                    let cap_faces: Vec<(u32, glam::Vec3)> = if caps.len() == face_n.len() {
                        caps.iter()
                            .zip(face_n.iter())
                            .map(|(cf, (_, n))| (cf.0, *n))
                            .collect()
                    } else {
                        caps.iter()
                            .map(|cf| (cf.0, m.compute_face_normal(*cf)))
                            .collect()
                    };
                    let mut acc: HashMap<u32, glam::Vec3> = HashMap::new();
                    for &(fid, n) in &cap_faces {
                        for &v in &m.face_vertices(FaceId(fid)) {
                            *acc.entry(v.0).or_insert(glam::Vec3::ZERO) += n;
                        }
                    }
                    let mut seed: Vec<VertexId> = Vec::with_capacity(acc.len());
                    for (vid, n) in acc {
                        let un = n.normalize_or_zero();
                        if un.length_squared() > 0.25 {
                            m.vert_mut(VertexId(vid)).position += un * offset;
                        }
                        seed.push(VertexId(vid));
                    }
                    // Locality: region normals (extrude topology already region-local).
                    m.recompute_normals_region(seed);
                }
            }
            // Offset is already applied, so the seams have their real dihedral here.
            if let Some((pre, post)) = pre.as_deref().zip(s.poly_node_mesh(nid)) {
                let created = crate::poly::op_tags::faces_created(pre, &post);
                s.poly_extrude_stamp_tags(nid, &created);
            }
            s.poly_request_display_sync(nid);
            s.mark_dirty(crate::DirtyFlags::RENDER);
        });
    }

    /// Execute the Extrude Face tool on selected faces (delta undo — POLY_FOUNDATION_PLAN §4.6).
    pub(crate) fn poly_extrude_faces(&mut self, offset: f32) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Modo implicit full selection: empty selection extrudes EVERY face (SAFE — adds walls + caps,
        // never removes geometry) via the shared `poly_faces_or_all` resolver.
        let faces: Vec<u32> = self.poly_faces_or_all(nid);
        if faces.is_empty() { return; }

        // Read normals up front (before any mutation) so the offset matches what the user sees.
        let ops: Vec<PolyEdit> = {
            let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
            let mesh = match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh,
                _ => return,
            };
            // Always region extrude (Blender E) — single open faces become closed volumes;
            // solid faces lift as a shell without zero-offset coincident wall/cap loops.
            let avg = faces.iter()
                .map(|&f| mesh.compute_face_normal(FaceId(f)))
                .fold(glam::Vec3::ZERO, |a, b| a + b)
                .normalize_or_zero();
            let o = avg * offset;
            vec![PolyEdit::ExtrudeRegion { faces: faces.clone(), offset: [o.x, o.y, o.z] }]
        };
        // Selection set is unchanged (region faces keep their ids) — so `keep` is the full requested
        // set: on success the reporter restores exactly what was selected before, and on refusal the
        // same faces stay selected. This site previously DROPPED the return value entirely, so a
        // refused extrude was a total no-op with zero feedback; `#[must_use]` now makes that a
        // compile warning rather than something review has to catch.
        // SMART SELECT: same created-face rule as the interactive drag. `poly_op_batch` is the
        // generic PolyEdit applier and knows nothing about extrude provenance, so the before /
        // after meshes are compared here instead — `ExtrudeRegion` only allocates faces, so the
        // diff is exact, and this one-shot applies its offset immediately (no zero-offset stage).
        let pre = self.poly_node_mesh(nid);
        let out = self.poly_op_batch(nid, ops, "Extrude");
        if let Some((pre, post)) = pre.as_deref().zip(self.poly_node_mesh(nid)) {
            let created = crate::poly::op_tags::faces_created(pre, &post);
            self.poly_extrude_stamp_tags(nid, &created);
        }
        self.poly_report_batch(&out, "extruded", &faces);
    }

    /// Start interactive edge extrude (Edge mode E). Builds fins at zero offset, then drag slides
    /// the new verts along the average incident-face normal.
    pub(crate) fn poly_start_edge_extrude_drag(&mut self, mouse: egui::Pos2) {
        let nid = match self.sel.usel.active_node() {
            Some(n) => n,
            None => return,
        };
        let edges: HashSet<u32> = if self.poly_sel.selected_edges.is_empty() {
            self.poly_edges_or_all(nid).into_iter().collect()
        } else {
            self.poly_sel.selected_edges.clone()
        };
        if edges.is_empty() {
            self.toast("Edge Extrude: select edges first");
            return;
        }
        if !matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        ) {
            self.toast("Edge Extrude: active object is not a poly mesh");
            return;
        }

        let direction = {
            let node = match self.scene.scene_graph.get(nid) {
                Some(n) => n,
                None => return,
            };
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => mesh.edge_extrude_direction(&edges),
                _ => return,
            }
        };

        self.begin_episode("Extrude Edge");

        // poly-modifiers P0-2: `extrude_edges` returns lifted VertexIds (not edge/wall triples).
        let new_verts: Vec<u32> = {
            let node = match self.scene.scene_graph.get_mut(nid) {
                Some(n) => n,
                None => {
                    self.cancel_episode();
                    return;
                }
            };
            match &mut node.payload {
                NodePayload::PolygonalBody { mesh, .. } => {
                    let m = Arc::make_mut(mesh);
                    m.extrude_edges(&edges, Vec3::ZERO)
                        .into_iter()
                        .map(|v| v.0)
                        .collect()
                }
                _ => {
                    self.cancel_episode();
                    return;
                }
            }
        };

        if new_verts.is_empty() {
            self.cancel_episode();
            self.toast("Edge Extrude: nothing to extrude");
            return;
        }

        // Select the new EDGES, not just their vertices.
        //
        // Owner 2026-07-30: "when I duplicate edge I only see vertex." The kernel returns lifted
        // VertexIds, so this used to select those verts — leaving the user in Vertex mode staring at
        // two dots instead of the edge they just pulled out. An edge extrude's result IS an edge, so
        // that is what should be selected, in the work mode the user was already in.
        //
        // The new edges are derived here rather than by widening `extrude_edges`' return type:
        // half_edge.rs is under active edit, and every half-edge whose BOTH endpoints are lifted
        // verts is exactly the extruded rim.
        let lifted: std::collections::HashSet<u32> = new_verts.iter().copied().collect();
        let new_edge_ids: Vec<u32> = {
            let mut out = Vec::new();
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    let mut seen = std::collections::HashSet::new();
                    for i in 0..mesh.half_edges.len() as u32 {
                        let he = core_math::half_edge::HalfEdgeId(i);
                        let dst = mesh.he(he).vertex.0;
                        let src = mesh.he_source(he).0;
                        if !lifted.contains(&dst) || !lifted.contains(&src) {
                            continue;
                        }
                        // Canonical id per undirected edge so the twin does not double-add.
                        let twin = mesh.he(he).twin;
                        let canon = if twin != core_math::half_edge::NO_TWIN {
                            i.min(twin.0)
                        } else {
                            i
                        };
                        if seen.insert(canon) {
                            out.push(canon);
                        }
                    }
                }
            }
            out
        };

        let orig_sel_faces = self.poly_sel.selected_faces.clone();
        let orig_sel_edges = self.poly_sel.selected_edges.clone();
        let orig_sel_vertices = self.poly_sel.selected_vertices.clone();

        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_vertices.clear();
        if new_edge_ids.is_empty() {
            // Degenerate (a lone vertex extrude): fall back to the verts so the drag still has a
            // subject rather than silently selecting nothing.
            for &vid in &new_verts {
                self.poly_sel.selected_vertices.insert(vid);
            }
        } else {
            for &eid in &new_edge_ids {
                self.poly_sel.selected_edges.insert(eid);
            }
            // Vertices too: the shared transform gathers affected verts from the selection, and an
            // edge selection must still move its endpoints.
            for &vid in &new_verts {
                self.poly_sel.selected_vertices.insert(vid);
            }
        }
        self.poly_request_display_sync(nid);

        // HAND OFF TO GRAB — do not run a bespoke one-axis drag.
        //
        // Owner 2026-07-30: "extrude on segment only lets me do toward one direction, let's use
        // shift to change orientation and alt for a free movement; this tool should use grab as its
        // main movement option so action center works with it."
        //
        // That is exactly right, and it is also the CLAUDE.md "two code paths for one operation" rule:
        // `poly_start_transform(Grab)` already owns axis constraints (X/Y/Z + plane), free movement,
        // numeric entry, snapping, falloff, AND the Action Center pivot resolution (`:853-870`).
        // The old `PolyEdgeExtrude` drag re-implemented a single fixed direction and could not reach
        // any of it — which is why action centers appeared not to work with this tool.
        //
        // `poly_start_transform` reads the selection we just set, so the extruded rim is the subject.
        //
        // UNDO GRANULARITY: `begin_episode` AUTO-COMMITS any open episode (`undo.rs:804`), so the
        // "Extrude Edge" episode opened above closes here and the Grab opens its own. That is the
        // correct split, not an accident: the topology add is a completed fact the moment the rim
        // exists, and cancelling the move (Esc) must leave the extruded edge rather than silently
        // undoing geometry the user can already see. Two Ctrl+Z steps — add, then move — which is
        // what Blender does for E-then-cancel as well.
        self.set_poly_tool(PolyTool::ExtrudeFace);
        self.poly_start_transform(crate::TransformKind::Grab, mouse);
        let _ = direction; // orientation now comes from the transform's axis constraint
        self.ui2.active_tool = "Edge Extrude".into();
        self.ui2.active_pillar = "poly".into();
        self.toast(
            "Edge Extrude → Grab: X/Y/Z axis · Shift+axis = plane · move freely · Ctrl snap · LMB/Enter confirm · Esc cancel",
        );
    }

    pub(crate) fn poly_update_edge_extrude_drag(
        &mut self,
        mouse: egui::Pos2,
        rect: egui::Rect,
        ctrl: bool,
    ) {
        let (nid, new_verts, dir, start, prev) = {
            let d = match self.tool.current.as_poly_edge_extrude() {
                Some(d) => d,
                None => return,
            };
            (
                d.node_id,
                d.new_verts.clone(),
                d.direction,
                d.start_mouse,
                d.accumulated_offset,
            )
        };
        if dir.length_squared() < 0.25 || new_verts.is_empty() {
            return;
        }

        let cam_pos = self.viewport.camera.target
            + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho {
            self.viewport.camera.distance
        } else {
            0.0
        };

        let centroid = {
            let mut sum = Vec3::ZERO;
            let mut n = 0u32;
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    for &vid in &new_verts {
                        if (vid as usize) < mesh.vertices.len() {
                            sum += mesh.vert(VertexId(vid)).position;
                            n += 1;
                        }
                    }
                }
            }
            if n > 0 {
                sum / n as f32
            } else {
                Vec3::ZERO
            }
        };

        let s0 = world_to_screen(
            centroid,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        );
        let s1 = world_to_screen(
            centroid + dir * 0.5,
            cam_pos,
            self.viewport.camera.orbit,
            rect,
            aspect,
            ortho_hw,
            self.render.fov_deg,
        );
        let new_offset = if let (Some(p0), Some(p1)) = (s0, s1) {
            let screen_dir = egui::vec2(p1.x - p0.x, p1.y - p0.y);
            let slen = screen_dir.length();
            if slen < 1.0 {
                prev
            } else {
                let sn = screen_dir / slen;
                let delta = mouse - start;
                let projected = delta.x * sn.x + delta.y * sn.y;
                let raw = projected * self.viewport.camera.distance * self.tweak.grab_speed;
                if self.is_snap_active(ctrl) {
                    let grid = self.viewport.grid_cell_size.max(0.01);
                    (raw / grid).round() * grid
                } else {
                    raw
                }
            }
        } else {
            prev
        };

        let step = new_offset - prev;
        if step.abs() > 1e-7 {
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                    let mesh = Arc::make_mut(mesh);
                    for &vid in &new_verts {
                        if (vid as usize) < mesh.vertices.len() {
                            mesh.vert_mut(VertexId(vid)).position += dir * step;
                        }
                    }
                }
            }
            self.poly_request_display_sync(nid);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if let Some(d) = self.tool.current.as_poly_edge_extrude_mut() {
            d.accumulated_offset = new_offset;
        }
    }

    pub(crate) fn poly_confirm_edge_extrude_drag(&mut self) {
        let (nid, new_edges, offset) = match self.tool.current.as_poly_edge_extrude() {
            Some(d) => (d.node_id, d.new_edges.clone(), d.accumulated_offset),
            None => return,
        };
        if offset.abs() < 1e-6 {
            self.poly_cancel_edge_extrude_drag();
            self.toast("Edge Extrude cancelled (zero offset)");
            return;
        }
        self.commit_episode();
        self.poly_sel.selected_faces.clear();
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.selected_edges.clear();
        for e in new_edges {
            self.poly_sel.selected_edges.insert(e);
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        self.poly_request_display_sync(nid);
        self.mark_dirty(DirtyFlags::RENDER);
        self.toast("Edge Extrude applied");
    }

    pub(crate) fn poly_cancel_edge_extrude_drag(&mut self) {
        let restore = self.tool.current.as_poly_edge_extrude().map(|d| {
            (
                d.orig_sel_faces.clone(),
                d.orig_sel_edges.clone(),
                d.orig_sel_vertices.clone(),
            )
        });
        if let Some(entry) = self.pending_episode.take() {
            if let Some(sg) = entry.payload.scene_graph_snapshot() {
                self.scene.scene_graph = sg;
            }
        }
        self.tool.current = ActiveTool::Idle;
        self.poly_tool = PolyTool::Idle;
        if let Some((f, e, v)) = restore {
            self.poly_sel.selected_faces = f;
            self.poly_sel.selected_edges = e;
            self.poly_sel.selected_vertices = v;
        } else {
            self.poly_sel.selected_faces.clear();
            self.poly_sel.selected_edges.clear();
            self.poly_sel.selected_vertices.clear();
        }
        self.poly_sync_all_meshes();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Solidify / Shell — give the active poly body wall thickness (Blender Solidify, one-click,
    /// destructive with undo). A closed body becomes a hollow shell; an open surface becomes a slab.
    /// This is the "thicken the extruded surface" operation.
    pub(crate) fn poly_solidify(&mut self, thickness: f32) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        // Whole-mesh op → targets() is None → domain None → the reporter never touches selection,
        // it only reports. Refusal now says why instead of being a silent no-op.
        let ops = vec![PolyEdit::Solidify { thickness, offset: -1.0 }];
        let out = self.poly_op_batch(nid, ops, "Solidify");
        self.poly_report_batch(&out, "solidified (shell)", &[]);
        // NO auto-group — same defect as the loop-cut site (see poly/loop_cut.rs). Grouping ALL live
        // faces makes a Group-mode click select the entire mesh, because the "group" is the mesh.
        // Solidify's meaningful group would be the INNER SHELL only; tagging that needs `solidify`
        // to report the faces it created.
        let _ = &out;
    }
}
