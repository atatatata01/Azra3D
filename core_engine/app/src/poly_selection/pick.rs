// pick.rs — pure relocation from poly_selection.rs
// Sub-element hover/pick, click select, grow/shrink, hide, linked selection.
// OWNER-FINAL visible-only rules travel intact with the pick path.

use std::collections::{HashSet, VecDeque};
use eframe::egui;
use glam::Vec3;
use core_math::half_edge::{VertexId, HalfEdgeId, FaceId, NO_TWIN, NO_FACE, NO_EDGE};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{
    AzraApp, ModelingContext, PolyWorkMode, PolyTool, DirtyFlags,
    ActiveTool, SelectionShape,
    world_to_screen, world_segment_to_screen, dist_point_to_segment,
};

use super::{mouse_to_ray, pick_cam_key, PICK_GRID_COLS, PICK_GRID_ROWS};

/// Screen-space hit radii (px). Edges are lines — they need a **wider** grab than verts or
/// selecting them feels finicky (owner 2026-08-06). Closest-candidate still wins, so a wider
/// radius only adds forgiveness near the intended edge.
const VERT_PICK_PX: f32 = 14.0;
const EDGE_PICK_PX: f32 = 16.0;
/// GPU pick-buffer probe radius (pixels, Chebyshev) for edge visibility — wider than the
/// default 1 px `any_near` because the edge's 1px ID line and the CPU-projected segment can
/// diverge by a pixel or two; own-id probes cannot false-positive, so wider is only safer.
const EDGE_VIS_PROBE_R: i32 = 3;
/// GPU pick-buffer probe radius (pixels, Chebyshev) for FACE hover. Faces are area primitives,
/// so the exact pixel is normally right; the radius only has to step past the 1 px edge/vert ids
/// that pass 2 paints over a face's rim. Kept small deliberately — a wide scan would let a
/// neighbouring face win when the cursor genuinely sits on the shared edge.
const FACE_VIS_PROBE_R: i32 = 2;

impl AzraApp {
    /// Per-frame reconciler (same pattern as `drop_mode_tool_on_body_switch`): poly component
    /// ids index ONE mesh. The frame the active poly body changes (create / click another body /
    /// delete) every id set is dropped — a stale id on a new mesh is at best a phantom highlight
    /// and at worst an unchecked-index CRASH (owner add-cube panic 2026-08-02: stale selected
    /// face 7 from the previous 8-face box → `half_edge.rs face()` OOB on the fresh 6-face cube).
    pub(crate) fn reconcile_poly_sel_body(&mut self) {
        let cur = self.active_poly_nid().map(|n| n.0);
        if self.poly_sel.body_stamp != cur {
            self.poly_sel.clear_components();
            self.poly_sel.body_stamp = cur;
            self.poly_invalidate_pick_cache();
            return;
        }
        self.prune_poly_sel_to_live();
    }

    /// INVARIANT: every component id in `poly_sel` (+ the active-vertex history) names a LIVE
    /// element of the active mesh. Same body is not enough: whole-mesh rebuilds (knife cut,
    /// booleans, dice — anything through `HalfEdgeMesh::from_polygons`) renumber every id and
    /// drop dead slots, so a pre-rebuild id can point past the shrunken arrays. Owner crash
    /// 2026-10-13: delete a vertex (leaves dead slots), then K-connect two vertices → the knife
    /// rebuild shrank the mesh to 228 verts while `hovered_vertex` still held 301 →
    /// `vert(301)` OOB in the same frame's overlay draw. Enforced HERE, once, for every op —
    /// run per frame and again right before the overlay draw — so no op has to remember it.
    /// Ids that are in range but were renumbered stay pointing at a live element (wrong
    /// highlight, not a crash); id-stable rebuilds are the real cure (CLAUDE.md poly kernel).
    pub(crate) fn prune_poly_sel_to_live(&mut self) {
        let Some(nid) = self.active_poly_nid() else { return };
        let Some(NodePayload::PolygonalBody { mesh, .. }) = self.scene.scene_graph.get(nid).map(|n| &n.payload) else {
            return;
        };
        let live_v = |v: &u32| mesh.is_vert_live(VertexId(*v));
        let live_e = |e: &u32| mesh.is_edge_live(HalfEdgeId(*e));
        let live_f = |f: &u32| mesh.is_face_live(FaceId(*f));
        let sel = &mut self.poly_sel;
        sel.selected_vertices.retain(live_v);
        sel.selected_edges.retain(live_e);
        sel.selected_faces.retain(live_f);
        sel.hidden_vertices.retain(live_v);
        sel.hovered_vertex = sel.hovered_vertex.filter(live_v);
        sel.hovered_edge = sel.hovered_edge.filter(live_e);
        sel.hovered_face = sel.hovered_face.filter(live_f);
        self.poly_active_vertex = self.poly_active_vertex.filter(live_v);
        self.poly_vert_history.retain(live_v);
    }

    /// Rebuild the screen-space pick cache for vertex or edge mode.
    /// The sub-element mode the hover/pick cache should use. Loop Cut (Blender Ctrl+R) needs EDGE hover
    /// regardless of the active select mode, so it works from Vertex/Face mode too. Without this the hovered
    /// edge stayed None outside Edge mode and the cut silently no-op'd ("loop cut doesn't create an edge").
    fn effective_pick_mode(&self) -> PolyWorkMode {
        if self.poly_tool == PolyTool::LoopCut { PolyWorkMode::Edge } else { self.poly_work_mode }
    }

    fn rebuild_pick_cache(&mut self, rect: egui::Rect, cam_key: u64, node_key: u64) {
        // CONSOLE_AUDIT A1: count + time every rebuild (extrude drag: watch pick_rebuilds in log).
        let _t0 = std::time::Instant::now();
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_orbit = self.viewport.camera.orbit;
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let hidden = &self.poly_sel.hidden_vertices;
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let xform_pos = |local: Vec3| -> Vec3 {
            let w = world_xform * glam::Vec4::new(local.x, local.y, local.z, 1.0);
            Vec3::new(w.x, w.y, w.z)
        };

        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let pick_mode = self.effective_pick_mode();
        let mode_u8 = match pick_mode {
            PolyWorkMode::Vertex => 0u8,
            PolyWorkMode::Edge => 1,
            _ => 2,
        };

        let gc = PICK_GRID_COLS;
        let gr = PICK_GRID_ROWS;
        let cell_w = rect.width() / gc as f32;
        let cell_h = rect.height() / gr as f32;
        let total_cells = (gc * gr) as usize;

        let cache = &mut self.poly_pick_cache;
        cache.cam_key = cam_key;
        cache.mode = mode_u8;
        cache.node_key = node_key;
        cache.grid_cols = gc;
        cache.grid_rows = gr;
        cache.cell_w = cell_w;
        cache.cell_h = cell_h;
        cache.rect_min_x = rect.min.x;
        cache.rect_min_y = rect.min.y;
        cache.grid.clear();
        cache.grid.resize(total_cells, Vec::new());

        match pick_mode {
            PolyWorkMode::Vertex => {
                cache.vert_screen.clear();
                cache.edge_screen.clear();
                for i in 0..mesh.vertices.len() as u32 {
                    if mesh.vert(VertexId(i)).half_edge == NO_EDGE || hidden.contains(&i) { continue; }
                    let pos = xform_pos(mesh.vert(VertexId(i)).position);
                    if let Some(sp) = world_to_screen(pos, cam_pos, cam_orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        let idx = cache.vert_screen.len();
                        cache.vert_screen.push((i, sp.x, sp.y));
                        let col = ((sp.x - rect.min.x) / cell_w).floor() as i32;
                        let row = ((sp.y - rect.min.y) / cell_h).floor() as i32;
                        if col >= 0 && col < gc as i32 && row >= 0 && row < gr as i32 {
                            let ci = (row as u32 * gc + col as u32) as usize;
                            // NO capacity gate — see `PolyPickCache::grid`. The `u16` cell type
                            // this used to guard silently made every element past 65 536
                            // unhoverable.
                            cache.grid[ci].push(idx as u32);
                        }
                    }
                }
            }
            PolyWorkMode::Edge => {
                cache.vert_screen.clear();
                cache.edge_screen.clear();
                // `canonical_edges` KEEPS live faceless wire/boundary edges. A partial knife cut
                // that stops inside a face, or a Delete-Only-Faces wire edge, is a real selectable
                // SEGMENT even with no incident face — see the rule's doc comment for the class of
                // bug a `face == NO_FACE` filter here reopens.
                for he_id in mesh.canonical_edges() {
                    let i = he_id.0;
                    let hei = mesh.he(he_id);
                    let src = mesh.he_source(he_id);
                    if hidden.contains(&src.0) || hidden.contains(&hei.vertex.0) { continue; }
                    let p0 = xform_pos(mesh.vert(src).position);
                    let p1 = xform_pos(mesh.vert(hei.vertex).position);
                    // Near-plane CLIP the segment (not a both-endpoints-Some gate): an edge with one
                    // endpoint behind the camera near plane used to be DROPPED entirely, so at close zoom
                    // / grazing angles edges near the camera became unpickable — a core "edge selection
                    // feels finicky" cause. Clipping keeps the visible part pickable.
                    if let Some((a, b)) = world_segment_to_screen(p0, p1, cam_pos, cam_orbit, rect, aspect, ortho_hw, self.render.fov_deg) {
                        let idx = cache.edge_screen.len();
                        cache.edge_screen.push((i, a.x, a.y, b.x, b.y));
                        // Insert into all cells the edge AABB covers
                        let min_x = a.x.min(b.x);
                        let max_x = a.x.max(b.x);
                        let min_y = a.y.min(b.y);
                        let max_y = a.y.max(b.y);
                        let c0 = ((min_x - rect.min.x) / cell_w).floor() as i32;
                        let c1 = ((max_x - rect.min.x) / cell_w).floor() as i32;
                        let r0 = ((min_y - rect.min.y) / cell_h).floor() as i32;
                        let r1 = ((max_y - rect.min.y) / cell_h).floor() as i32;
                        let c0 = c0.max(0).min(gc as i32 - 1) as u32;
                        let c1 = c1.max(0).min(gc as i32 - 1) as u32;
                        let r0 = r0.max(0).min(gr as i32 - 1) as u32;
                        let r1 = r1.max(0).min(gr as i32 - 1) as u32;
                        // NO capacity gate — see `PolyPickCache::grid`. The `u16` cell type this
                        // used to guard is what made "edge select degrades after a while" real:
                        // past 65 536 projected edges the rest were never registered in any cell.
                        for r in r0..=r1 {
                            for c in c0..=c1 {
                                cache.grid[(r * gc + c) as usize].push(idx as u32);
                            }
                        }
                    }
                }
            }
            _ => {} // Face mode doesn't use the grid cache
        }
        self.poly_pick_rebuilds_this_frame =
            self.poly_pick_rebuilds_this_frame.saturating_add(1);
        self.poly_pick_rebuild_ms_this_frame +=
            _t0.elapsed().as_secs_f32() * 1000.0;
    }

    /// Occlusion depth of the mesh's own front surface under `mouse`, measured as the
    /// signed distance ALONG the camera forward axis (view-space depth) of the nearest
    /// ray-triangle hit. Returns `None` when the ray misses every face (nothing occludes).
    ///
    /// Used by "visible only" selection: a candidate vertex/edge is occluded when its own
    /// view-space depth is meaningfully greater than this front-surface depth. Computed on
    /// the mesh's own triangles only — this is a self-occlusion test (matches Blender's
    /// per-object visible-only pick), not a whole-scene depth test. Reuses `mouse_to_ray`
    /// and `ray_triangle` (no new intersection math).
    pub(crate) fn poly_front_surface_depth(
        &self,
        nid: core_math::scene_graph::NodeId,
        mouse: egui::Pos2,
        rect: egui::Rect,
        cam_pos: Vec3,
        cam_orbit: glam::Quat,
        aspect: f32,
        ortho_hw: f32,
    ) -> Option<f32> {
        let node = self.scene.scene_graph.get(nid)?;
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return None,
        };
        let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
        // Hidden faces don't occlude — they aren't drawn (the helper skips them).
        let hidden = &self.poly_sel.hidden_vertices;
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let (ray_o, ray_d) = mouse_to_ray(mouse, rect, cam_pos, cam_orbit, aspect, ortho_hw, self.render.fov_deg);
        // Ear-clipped (concave-safe) — a fan used to report a front surface where the face
        // has no coverage, so verts/edges in a concave pocket were wrongly culled.
        super::ray_pick_mesh_face(mesh, world_xform, ray_o, ray_d, hidden)
            .map(|(t, _)| ((ray_o + ray_d * t) - cam_pos).dot(fwd))
    }

    /// View-space depth (along camera forward) of a world point, for occlusion comparison.
    pub(crate) fn view_depth(p: Vec3, cam_pos: Vec3, cam_fwd: Vec3) -> f32 {
        (p - cam_pos).dot(cam_fwd)
    }

    /// True when face `fid` is the nearest mesh surface under viewport point `sp`
    /// (self-occlusion for polygroup paint when the GPU pick buffer is not ready).
    pub(crate) fn poly_face_is_frontmost_at(
        &self,
        nid: core_math::scene_graph::NodeId,
        fid: u32,
        sp: egui::Pos2,
        rect: egui::Rect,
        cam_pos: Vec3,
        cam_orbit: glam::Quat,
        aspect: f32,
        ortho_hw: f32,
    ) -> bool {
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return false;
        };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return false,
        };
        let hidden = &self.poly_sel.hidden_vertices;
        let world_xform = self.scene.scene_graph.world_transform(nid);
        let (ray_o, ray_d) =
            mouse_to_ray(sp, rect, cam_pos, cam_orbit, aspect, ortho_hw, self.render.fov_deg);
        super::ray_pick_mesh_face(mesh, world_xform, ray_o, ray_d, hidden).map(|(_, f)| f) == Some(fid)
    }

    /// Find hovered sub-element (vertex, edge, or face) for the selected poly mesh.
    /// Uses screen-space grid cache for O(k) vertex/edge picking.
    pub(crate) fn poly_hover_pick(&mut self, mouse: egui::Pos2, rect: egui::Rect) {
        self.poly_sel.hovered_vertex = None;
        self.poly_sel.hovered_edge = None;
        self.poly_sel.hovered_face = None;
        self.poly_sel.hovered_face_other = None;

        if self.modeling_context != ModelingContext::Polygonal { return; }
        // NOT WHILE A DRAG EPISODE IS OPEN (VIEWPORT_PERF_2026-08-12, owner: "when I edit the mesh
        // I go back to 7 fps"). Mid-edit the mesh key changes every frame, so the GPU select
        // buffer is never valid and the Face arm falls back to its O(faces) CPU scan — the exact
        // 77–133 ms this function was fixed to stop paying, back every frame of a drag.
        //
        // Hover is meaningless while a gesture owns the pointer: the tool captured its target when
        // it armed, `poly_click_select` does not run mid-drag, and the hover fields are recomputed
        // on the first frame after the commit. Answering "nothing hovered" is honest and free;
        // answering it with a full mesh scan is what made modeling feel broken.
        //
        // Bounded like the select-buffer gate: `auto_commit_episodes` closes a leaked episode at
        // the end of every frame, so a stuck episode cannot freeze hover for more than one frame.
        if self.pending_episode.is_some() { return; }
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };

        let cam_pos = self.viewport.camera.target + self.viewport.camera.orbit * Vec3::new(0.0, 0.0, self.viewport.camera.distance);
        let cam_orbit = self.viewport.camera.orbit;
        let aspect = rect.width() / rect.height().max(1.0);
        let ortho_hw = if self.render.ortho { self.viewport.camera.distance } else { 0.0 };
        let mode = self.effective_pick_mode();

        match mode {
            PolyWorkMode::Vertex => {
                let cam_key = pick_cam_key(cam_pos, cam_orbit, rect, ortho_hw);
                // Cache key must cover mesh CONTENT + transform + hidden set (poly_pick_mesh_key),
                // not just the node id: with `nid.0` alone, any edit that kept the same camera
                // (grab, extrude, bevel…) left STALE projected positions/ids in the cache — clicks
                // then hit where elements USED to be (the "sometimes registers" bug, POLY-SEL).
                let node_key = self.poly_pick_mesh_key().unwrap_or(nid.0 as u64);
                let mode_u8 = 0u8;
                if self.poly_pick_cache.cam_key != cam_key
                    || self.poly_pick_cache.mode != mode_u8
                    || self.poly_pick_cache.node_key != node_key
                {
                    self.rebuild_pick_cache(rect, cam_key, node_key);
                }

                // Grid-accelerated vertex pick: check mouse cell + 8 neighbors.
                // Gather candidates first (releases the cache borrow), then apply the
                // occlusion test — a nearer-in-screen but occluded vertex must NOT beat a
                // visible one, so filtering happens on the candidate set, not greedily.
                let (mc, mr) = self.poly_pick_cache.cell_of(mouse.x, mouse.y);
                let gc = self.poly_pick_cache.grid_cols;
                let gr = self.poly_pick_cache.grid_rows;
                let r_lo = if mr > 0 { mr - 1 } else { 0 };
                let r_hi = (mr + 1).min(gr - 1);
                let c_lo = if mc > 0 { mc - 1 } else { 0 };
                let c_hi = (mc + 1).min(gc - 1);
                let mut candidates: Vec<(u32, f32, f32, f32)> = Vec::new(); // (vertex_id, screen_dist, sx, sy)
                for r in r_lo..=r_hi {
                    for c in c_lo..=c_hi {
                        let ci = (r * gc + c) as usize;
                        for &idx in &self.poly_pick_cache.grid[ci] {
                            let (vid, sx, sy) = self.poly_pick_cache.vert_screen[idx as usize];
                            let dx = sx - mouse.x;
                            let dy = sy - mouse.y;
                            let d = (dx * dx + dy * dy).sqrt();
                            if d < VERT_PICK_PX {
                                candidates.push((vid, d, sx, sy));
                            }
                        }
                    }
                }
                candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                // Visibility: ONE system with paint (PAINT_SELECT_AUDIT P3) — the GPU select
                // buffer when valid (O(1) depth-resolved lookup on the vert's OWN id, no
                // inference, no bias heuristics). The CPU depth ray + `distance*0.02` bias
                // remains ONLY as the fallback while the buffer is pending (rare now — a
                // pointer press refreshes the buffer synchronously).
                let img = if self.sel.visible_only { self.poly_pick_image_valid() } else { None };
                let occl_depth = if self.sel.visible_only && img.is_none() {
                    self.poly_front_surface_depth(nid, mouse, rect, cam_pos, cam_orbit, aspect, ortho_hw)
                } else { None };
                let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
                let world_xform = self.scene.scene_graph.world_transform(nid);
                let mesh = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh),
                    _ => None,
                });
                // Fallback depth bias: a vertex sitting ON the front face is at ~occl_depth;
                // allow a small forward tolerance so silhouette/front verts aren't wrongly culled.
                let bias = (self.viewport.camera.distance * 0.02).max(1e-3);
                let mut hv: Option<u32> = None;
                for (vid, _d, sx, sy) in &candidates {
                    let visible = if let Some(img) = img {
                        // Per-element buffer: the vert wrote its OWN id, depth-resolved with a
                        // priority bias over its faces. A hit within r=2 means the vert IS
                        // visible there — own-id probes cannot false-positive, and the old
                        // incident-face inference (silhouette false-culls / behind-surface
                        // false-passes) is gone.
                        img.any_near_r(
                            egui::pos2(*sx, *sy),
                            2,
                            |id| id == crate::poly_select_gpu::pick_id_vert(*vid),
                        )
                    } else if let (Some(front), Some(mesh)) = (occl_depth, mesh) {
                        let w = world_xform * glam::Vec4::new(
                            mesh.vert(VertexId(*vid)).position.x,
                            mesh.vert(VertexId(*vid)).position.y,
                            mesh.vert(VertexId(*vid)).position.z, 1.0);
                        let vdepth = Self::view_depth(Vec3::new(w.x, w.y, w.z), cam_pos, fwd);
                        vdepth <= front + bias
                    } else {
                        true
                    };
                    if visible {
                        hv = Some(*vid);
                        break;
                    }
                }
                self.poly_sel.hovered_vertex = hv;
            }
            PolyWorkMode::Edge => {
                let cam_key = pick_cam_key(cam_pos, cam_orbit, rect, ortho_hw);
                // Content-aware key — see Vertex arm comment (stale-cache click bug).
                let node_key = self.poly_pick_mesh_key().unwrap_or(nid.0 as u64);
                let mode_u8 = 1u8;
                if self.poly_pick_cache.cam_key != cam_key
                    || self.poly_pick_cache.mode != mode_u8
                    || self.poly_pick_cache.node_key != node_key
                {
                    self.rebuild_pick_cache(rect, cam_key, node_key);
                }

                let (mc, mr) = self.poly_pick_cache.cell_of(mouse.x, mouse.y);
                let gc = self.poly_pick_cache.grid_cols;
                let gr = self.poly_pick_cache.grid_rows;
                // Halo scales with pick radius / cell size so a 16px grab still finds edges
                // whose AABB only lands in a neighboring cell.
                let cell_w = self.poly_pick_cache.cell_w.max(1.0);
                let cell_h = self.poly_pick_cache.cell_h.max(1.0);
                let halo = ((EDGE_PICK_PX / cell_w.min(cell_h)).ceil() as i32).clamp(1, 3);
                let r_lo = mr.saturating_sub(halo as u32);
                let r_hi = (mr + halo as u32).min(gr.saturating_sub(1));
                let c_lo = mc.saturating_sub(halo as u32);
                let c_hi = (mc + halo as u32).min(gc.saturating_sub(1));
                let mut candidates: Vec<(u32, f32, f32, f32, f32)> = Vec::new(); // (half_edge_id, screen_dist, t, closest sx, sy)
                let mut seen: HashSet<u32> = HashSet::new();
                for r in r_lo..=r_hi {
                    for c in c_lo..=c_hi {
                        let ci = (r * gc + c) as usize;
                        for &idx in &self.poly_pick_cache.grid[ci] {
                            let (eid, ax, ay, bx, by) = self.poly_pick_cache.edge_screen[idx as usize];
                            if !seen.insert(eid) {
                                continue;
                            }
                            let a = egui::pos2(ax, ay);
                            let b = egui::pos2(bx, by);
                            let d = dist_point_to_segment(mouse, a, b);
                            if d < EDGE_PICK_PX {
                                // Parameter of the closest point ON the segment (screen space) —
                                // occlusion tests this point (not the midpoint).
                                let (ex, ey) = (bx - ax, by - ay);
                                let len2 = ex * ex + ey * ey;
                                let t = if len2 > 1e-6 {
                                    (((mouse.x - ax) * ex + (mouse.y - ay) * ey) / len2).clamp(0.0, 1.0)
                                } else {
                                    0.5
                                };
                                candidates.push((eid, d, t, ax + ex * t, ay + ey * t));
                            }
                        }
                    }
                }
                candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                // Visibility gate. ONE system with paint (PAINT_SELECT_AUDIT P3): GPU select
                // buffer when valid — the edge wrote its OWN id (depth-biased over its own
                // faces), so visibility is one exact lookup. The 2026-08-06 T1-T3 inference
                // tiers (incident faces / endpoint fans / cursor-ray face) existed only
                // because the old buffer had no edge channel — deleted with their cause.
                // CPU depth ray + bias survives as the pending-buffer fallback only.
                let img = if self.sel.visible_only {
                    self.poly_pick_image_valid()
                } else {
                    None
                };
                let occl_depth = if self.sel.visible_only && img.is_none() {
                    self.poly_front_surface_depth(
                        nid, mouse, rect, cam_pos, cam_orbit, aspect, ortho_hw,
                    )
                } else {
                    None
                };
                let fwd = (cam_orbit * Vec3::NEG_Z).normalize();
                let world_xform = self.scene.scene_graph.world_transform(nid);
                let mesh = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                    NodePayload::PolygonalBody { mesh, .. } => Some(mesh),
                    _ => None,
                });
                let bias = (self.viewport.camera.distance * 0.02).max(1e-3);
                // Nearest face under the cursor — FALLBACK path only (buffer pending): rescues
                // the "clicked the face right next to the edge" case the depth ray misjudges.
                // Never computed when the image is valid (own-id lookup needs no rescue).
                let ray_face: Option<FaceId> = if self.sel.visible_only && img.is_none() {
                    if let Some(mesh) = mesh {
                        let (ray_o, ray_d) = mouse_to_ray(
                            mouse, rect, cam_pos, cam_orbit, aspect, ortho_hw, self.render.fov_deg,
                        );
                        let hidden = &self.poly_sel.hidden_vertices;
                        super::ray_pick_mesh_face(mesh, world_xform, ray_o, ray_d, hidden)
                            .map(|(_, f)| FaceId(f))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let edge_incident = |mesh: &core_math::half_edge::HalfEdgeMesh, eid: u32, f: FaceId| -> bool {
                    let hedge = mesh.he(HalfEdgeId(eid));
                    f == hedge.face
                        || (hedge.twin != NO_TWIN && mesh.he(hedge.twin).face == f)
                };

                let mut he: Option<u32> = None;
                for (eid, _d, t, spx, spy) in &candidates {
                    let Some(mesh) = mesh else {
                        he = Some(*eid);
                        break;
                    };
                    let visible = if let Some(img) = img {
                        let sp = egui::pos2(*spx, *spy);
                        // The edge's OWN id at its closest-on-screen point (wide probe absorbs
                        // line-raster offset). Endpoint vert ids also accepted: a vert drawn
                        // with higher priority can own the pixel where a short edge's closest
                        // point lands — a visible endpoint proves the edge visible there.
                        let canon = crate::poly_select_gpu::canonical_he(mesh, *eid);
                        let src = mesh.he_source(HalfEdgeId(*eid)).0;
                        let dst = mesh.he(HalfEdgeId(*eid)).vertex.0;
                        img.any_near_r(sp, EDGE_VIS_PROBE_R, |id| {
                            id == crate::poly_select_gpu::pick_id_edge(canon)
                                || id == crate::poly_select_gpu::pick_id_vert(src)
                                || id == crate::poly_select_gpu::pick_id_vert(dst)
                        })
                    } else if let Some(front) = occl_depth {
                        let src = mesh.he_source(HalfEdgeId(*eid));
                        let dst = mesh.he(HalfEdgeId(*eid)).vertex;
                        let ps = mesh.vert(src).position;
                        let pd = mesh.vert(dst).position;
                        let near_l = ps + (pd - ps) * *t;
                        let w = world_xform
                            * glam::Vec4::new(near_l.x, near_l.y, near_l.z, 1.0);
                        let vdepth =
                            Self::view_depth(Vec3::new(w.x, w.y, w.z), cam_pos, fwd);
                        // T4 — depth at edge point, or T3 ray-face incident
                        vdepth <= front + bias
                            || ray_face.map_or(false, |rf| edge_incident(mesh, *eid, rf))
                    } else {
                        true // x-ray / visible_only off
                    };
                    if visible {
                        he = Some(*eid);
                        break;
                    }
                }
                self.poly_sel.hovered_edge = he;
            }
            PolyWorkMode::Face | PolyWorkMode::Group | PolyWorkMode::Object => {
                // Face / Group mode: ray-triangle pick (Group expands to whole polygroup on click).
                //
                // SCENE-WIDE, not active-body-only. Restricted to the active body, the ray flew
                // THROUGH a hole in that body and hit its own far WALL, while a separate body
                // sitting nearer in the hole was never even tested (owner repro: shell + disc).
                // The GLOBAL nearest hit is "what you see", which is also why no extra
                // `visible_only` gate is applied here — nearest-along-the-ray IS the front
                // surface. The owner-final paint / RMB-region / Alt+RMB rules are untouched.
                //
                // VISIBILITY IS THE GPU BUFFER FIRST (VIEWPORT_PERF C, 2026-08-12). The scan below
                // is O(faces) with two allocations and an ear-clip per face, and it ran EVERY
                // frame the cursor sat over the viewport: 77–133 ms of the frame on a 323k-tri
                // body, with `tool=Idle`. Verts and edges already answer "what is in front here?"
                // with one depth-resolved lookup in the select buffer; Face mode was the only arm
                // still asking a CPU ray. It now asks the same buffer:
                //   • buffer valid + a face id under the cursor → that face IS the frontmost
                //     surface of the active body there (depth-resolved on the GPU). O(1).
                //   • buffer valid + no face id → the active body is provably NOT the frontmost
                //     visible surface at that pixel, so it cannot win the nearest-hit contest.
                //     Skip it and ray only the OTHER bodies (`hovered_face_other`), which the
                //     id-0 occluder channel cannot name.
                //   • buffer stale / pending → NO hover this frame (see below).
                let gpu_face = self.poly_pick_image_valid()
                    .map(|img| img.nearest_face_at(mouse, FACE_VIS_PROBE_R));
                if let Some(Some(fid)) = gpu_face {
                    self.poly_sel.hovered_face = Some(fid);
                    self.poly_sel.hovered_face_other = None;
                    return;
                }
                // THE ACTIVE BODY IS NEVER CPU-SCANNED. The original fix kept the O(faces) scan as
                // a "rare, buffer-pending" fallback. It is not rare: the buffer's key includes the
                // CAMERA, so it is invalid on EVERY frame of an orbit — owner 2026-08-12,
                // `input=78ms, tool=Poly Transform, repaint_next=pointer_down`, i.e. the whole
                // 143 ms frame was this fallback while the camera moved. A fallback that runs
                // whenever the user is moving is the hot path, not a fallback.
                //
                // Dropping it costs nothing a click can notice: a pointer PRESS refreshes the
                // select buffer SYNCHRONOUSLY in `poll_poly_select_buffer` (`want_sync`), and poll
                // runs before input in the same frame — so the frame a click is interpreted on
                // always has a valid buffer and resolves hover exactly. What is lost is the hover
                // HIGHLIGHT during camera motion, which returns the frame the camera settles.
                //
                // Other bodies still get the ray: they render as id-0 occluders, so the buffer
                // cannot name them, and they are not the 323k-tri body this is protecting.
                if gpu_face.is_none() {
                    return; // buffer not valid for this camera/mesh — no answer this frame
                }
                // Reaching here means the buffer WAS valid and reported no active-body face at the
                // cursor, so the active body is provably not frontmost there and is skipped below.
                let hidden = self.poly_sel.hidden_vertices.clone();
                let no_hidden: HashSet<u32> = HashSet::new();
                let (ray_o, ray_d) = mouse_to_ray(mouse, rect, cam_pos, cam_orbit, aspect, ortho_hw, self.render.fov_deg);
                let mut best: Option<(f32, NodeId, u32)> = None;
                for (bnid, node) in self.scene.scene_graph.poly_bodies() {
                    if !node.visible { continue; }
                    if bnid == nid { continue; } // active body: answered by the buffer above
                    let mesh = match &node.payload {
                        NodePayload::PolygonalBody { mesh, .. } => mesh,
                        _ => continue,
                    };
                    // Hidden state is per-ACTIVE-body; other bodies hide nothing.
                    let h = if bnid == nid { &hidden } else { &no_hidden };
                    let world = self.scene.scene_graph.world_transform(bnid);
                    if let Some((t, fid)) = super::ray_pick_mesh_face(mesh, world, ray_o, ray_d, h) {
                        if best.map_or(true, |(bt, _, _)| t < bt) { best = Some((t, bnid, fid)); }
                    }
                }
                match best {
                    // Winner is the active body — normal hover.
                    Some((_, bnid, fid)) if bnid == nid => {
                        self.poly_sel.hovered_face = Some(fid);
                        self.poly_sel.hovered_face_other = None;
                    }
                    // Winner is ANOTHER body. `fid` indexes THAT mesh, so it must NOT go into
                    // `hovered_face` (unchecked `face(FaceId)` on the active mesh = OOB crash,
                    // 2026-08-02). Park it; `poly_click_select` switches body + reconciles first.
                    Some((_, bnid, fid)) => {
                        self.poly_sel.hovered_face = None;
                        self.poly_sel.hovered_face_other = Some((bnid.0, fid));
                    }
                    None => {}
                }
            }
        }
    }

    /// Handle click to select/deselect sub-elements.
    /// True when nothing is under the cursor for the active poly work mode — i.e. this is an "empty"
    /// click on the background. Drives the empty-click tool-drop escalation in `poly_release_click_select`.
    pub(crate) fn poly_hover_is_empty(&self) -> bool {
        match self.poly_work_mode {
            PolyWorkMode::Vertex => self.poly_sel.hovered_vertex.is_none(),
            PolyWorkMode::Edge => self.poly_sel.hovered_edge.is_none(),
            // A face on ANOTHER poly body is still a hit — not an empty background click.
            PolyWorkMode::Face | PolyWorkMode::Group =>
                self.poly_sel.hovered_face.is_none() && self.poly_sel.hovered_face_other.is_none(),
            PolyWorkMode::Object => true,
        }
    }

    pub(crate) fn poly_click_select(&mut self, shift: bool) {
        // Cross-body face pick: hover found a NEARER face on a DIFFERENT poly body. Promote it
        // HERE (not in hover) for two reasons: (a) hover runs every frame outside the selection
        // -undo wrapper, so switching bodies on mere hover would desync Ctrl+Z — this fn is
        // called from `handle_click_select_inner`, INSIDE the wrapper; (b) the face id belongs to
        // the other mesh, so `reconcile_poly_sel_body()` must drop the old per-mesh id sets
        // BEFORE it is stored, or `face(FaceId)` indexes out of bounds (crash 2026-08-02).
        if let Some((raw_nid, fid)) = self.poly_sel.hovered_face_other.take() {
            if matches!(self.poly_work_mode, PolyWorkMode::Face | PolyWorkMode::Group) {
                let bnid = NodeId(raw_nid);
                if self.scene.scene_graph.get(bnid).is_some() {
                    self.select_scene_node(bnid);
                    self.reconcile_poly_sel_body();
                    self.poly_sel.hovered_face = Some(fid);
                    self.poly_invalidate_pick_cache();
                }
            }
        }
        match self.poly_work_mode {
            PolyWorkMode::Group => {
                self.poly_click_select_group(shift);
            }
            PolyWorkMode::Vertex => {
                if let Some(v) = self.poly_sel.hovered_vertex {
                    if shift {
                        if self.poly_sel.selected_vertices.remove(&v) {
                            // Deselected v — if it was the active target, hand active to any remaining vert.
                            if self.poly_active_vertex == Some(v) {
                                self.poly_active_vertex = self.poly_sel.selected_vertices.iter().copied().next();
                            }
                            self.poly_vert_history.retain(|&x| x != v);
                        } else {
                            self.poly_sel.selected_vertices.insert(v);
                            self.poly_active_vertex = Some(v); // last shift-clicked = "merge at last" target
                            self.poly_push_vert_history(v);
                        }
                    } else {
                        self.poly_sel.selected_vertices.clear();
                        self.poly_sel.selected_vertices.insert(v);
                        self.poly_active_vertex = Some(v);
                        self.poly_vert_history.clear();
                        self.poly_push_vert_history(v);
                    }
                } else if !shift {
                    self.poly_sel.selected_vertices.clear();
                    self.poly_active_vertex = None;
                    self.poly_vert_history.clear();
                }
            }
            PolyWorkMode::Edge => {
                if let Some(e) = self.poly_sel.hovered_edge {
                    // An edge is TWO half-edges (e, twin) and a pick can land on either. Selection stores
                    // one; if a later shift-click hovers the OTHER half, remove(&e) misses and it re-inserts
                    // instead of deselecting (the owner's "deselect edge doesn't work"). Match BOTH halves.
                    let twin = self.sel.usel.active_node()
                        .and_then(|nid| self.scene.scene_graph.get(nid))
                        .and_then(|n| match &n.payload {
                            NodePayload::PolygonalBody { mesh, .. } if (e as usize) < mesh.half_edges.len() => {
                                let t = mesh.he(HalfEdgeId(e)).twin;
                                if t != NO_TWIN { Some(t.0) } else { None }
                            }
                            _ => None,
                        });
                    if shift {
                        let removed = self.poly_sel.selected_edges.remove(&e)
                            | twin.map_or(false, |t| self.poly_sel.selected_edges.remove(&t));
                        if !removed {
                            self.poly_sel.selected_edges.insert(e);
                            self.poly_push_edge_history(e);
                        } else {
                            self.poly_edge_history.retain(|&x| x != e && twin.map_or(true, |t| x != t));
                        }
                    } else {
                        self.poly_sel.selected_edges.clear();
                        self.poly_sel.selected_edges.insert(e);
                        self.poly_edge_history.clear();
                        self.poly_push_edge_history(e);
                    }
                } else if !shift {
                    self.poly_sel.selected_edges.clear();
                    self.poly_edge_history.clear();
                }
            }
            PolyWorkMode::Face => {
                if let Some(f) = self.poly_sel.hovered_face {
                    if shift {
                        if !self.poly_sel.selected_faces.remove(&f) {
                            self.poly_sel.selected_faces.insert(f);
                            self.poly_push_face_history(f);
                        } else {
                            self.poly_face_history.retain(|&x| x != f);
                        }
                    } else {
                        self.poly_sel.selected_faces.clear();
                        self.poly_sel.selected_faces.insert(f);
                        self.poly_face_history.clear();
                        self.poly_push_face_history(f);
                    }
                } else if !shift {
                    self.poly_sel.selected_faces.clear();
                    self.poly_face_history.clear();
                }
            }
            PolyWorkMode::Object => {}
        }
        // Selection highlight is a GPU overlay. A click that only mutates the set
        // without RENDER leaves the last uploaded lines on screen until the next
        // camera-driven dispatch (owner: highlight appears only after orbit).
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Clear all sub-element selections.
    pub(crate) fn poly_clear_selection(&mut self) {
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_faces.clear();
    }

    /// Force select-all in the current poly edit mode (never deselects).
    /// Used after Mesh Cleanup so the user sees every remaining component.
    pub(crate) fn poly_select_all(&mut self) {
        self.poly_sel.selected_vertices.clear();
        self.poly_sel.selected_edges.clear();
        self.poly_sel.selected_faces.clear();
        // Toggle with empty selection → select all.
        self.poly_select_all_toggle();
    }

    /// Toggle select all / deselect all in current poly sub-mode.
    pub(crate) fn poly_select_all_toggle(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };
        let hidden = &self.poly_sel.hidden_vertices;
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                if self.poly_sel.selected_vertices.is_empty() {
                    for i in 0..mesh.vertices.len() as u32 {
                        if mesh.vert(VertexId(i)).half_edge != NO_EDGE && !hidden.contains(&i) {
                            self.poly_sel.selected_vertices.insert(i);
                        }
                    }
                } else {
                    self.poly_sel.selected_vertices.clear();
                }
            }
            PolyWorkMode::Edge => {
                if self.poly_sel.selected_edges.is_empty() {
                    // Canonical walk — "select all" must reach the SAME edge set the user can
                    // hover and click, boundary / wire edges of an open mesh included.
                    for he_id in mesh.canonical_edges() {
                        if hidden.contains(&mesh.he_source(he_id).0) { continue; }
                        if hidden.contains(&mesh.he(he_id).vertex.0) { continue; }
                        self.poly_sel.selected_edges.insert(he_id.0);
                    }
                } else {
                    self.poly_sel.selected_edges.clear();
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                if self.poly_sel.selected_faces.is_empty() {
                    for i in 0..mesh.faces.len() as u32 {
                        if mesh.face(FaceId(i)).half_edge != NO_EDGE
                            && mesh.face_vertices(FaceId(i)).iter().all(|v| !hidden.contains(&v.0))
                        {
                            self.poly_sel.selected_faces.insert(i);
                        }
                    }
                } else {
                    self.poly_sel.selected_faces.clear();
                }
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Invert selection in current poly sub-mode. Hidden elements are excluded.
    pub(crate) fn poly_invert_selection(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };
        let hidden = &self.poly_sel.hidden_vertices;
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                let mut new_sel = HashSet::new();
                for i in 0..mesh.vertices.len() as u32 {
                    if mesh.vert(VertexId(i)).half_edge != NO_EDGE
                        && !hidden.contains(&i)
                        && !self.poly_sel.selected_vertices.contains(&i)
                    { new_sel.insert(i); }
                }
                self.poly_sel.selected_vertices = new_sel;
            }
            PolyWorkMode::Edge => {
                let mut new_sel = HashSet::new();
                for he_id in mesh.canonical_edges() {
                    if hidden.contains(&mesh.he_source(he_id).0) { continue; }
                    if hidden.contains(&mesh.he(he_id).vertex.0) { continue; }
                    if !self.poly_sel.selected_edges.contains(&he_id.0) { new_sel.insert(he_id.0); }
                }
                self.poly_sel.selected_edges = new_sel;
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let mut new_sel = HashSet::new();
                for i in 0..mesh.faces.len() as u32 {
                    if mesh.face(FaceId(i)).half_edge != NO_EDGE
                        && mesh.face_vertices(FaceId(i)).iter().all(|v| !hidden.contains(&v.0))
                        && !self.poly_sel.selected_faces.contains(&i)
                    { new_sel.insert(i); }
                }
                self.poly_sel.selected_faces = new_sel;
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Grow selection by one BFS step outward.
    pub(crate) fn poly_grow_selection(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                let mut to_add = HashSet::new();
                for &vi in &self.poly_sel.selected_vertices {
                    for he in mesh.vertex_fan(VertexId(vi)) {
                        to_add.insert(mesh.he(he).vertex.0);
                    }
                }
                self.poly_sel.selected_vertices.extend(to_add);
            }
            PolyWorkMode::Edge => {
                let mut to_add = HashSet::new();
                for &ei in &self.poly_sel.selected_edges {
                    let src = mesh.he_source(HalfEdgeId(ei)).0;
                    let dst = mesh.he(HalfEdgeId(ei)).vertex.0;
                    for &v in &[src, dst] {
                        for he in mesh.vertex_fan(VertexId(v)) {
                            let twin = mesh.he(he).twin;
                            let canonical = if twin == NO_TWIN || he.0 < twin.0 { he.0 } else { twin.0 };
                            if mesh.he(HalfEdgeId(canonical)).face != NO_FACE {
                                to_add.insert(canonical);
                            }
                        }
                    }
                }
                self.poly_sel.selected_edges.extend(to_add);
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let mut to_add = HashSet::new();
                for &fi in &self.poly_sel.selected_faces {
                    for he in mesh.face_half_edges(FaceId(fi)) {
                        let twin = mesh.he(he).twin;
                        if twin != NO_TWIN {
                            let adj_face = mesh.he(twin).face;
                            if adj_face != NO_FACE { to_add.insert(adj_face.0); }
                        }
                    }
                }
                self.poly_sel.selected_faces.extend(to_add);
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Shrink selection: remove elements that have any unselected neighbor.
    pub(crate) fn poly_shrink_selection(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                let mut to_remove = Vec::new();
                for &vi in &self.poly_sel.selected_vertices {
                    let has_unsel = mesh.vertex_fan(VertexId(vi)).iter().any(|&he| {
                        !self.poly_sel.selected_vertices.contains(&mesh.he(he).vertex.0)
                    });
                    if has_unsel { to_remove.push(vi); }
                }
                for v in to_remove { self.poly_sel.selected_vertices.remove(&v); }
            }
            PolyWorkMode::Edge => {
                let mut to_remove = Vec::new();
                for &ei in &self.poly_sel.selected_edges {
                    let src = mesh.he_source(HalfEdgeId(ei)).0;
                    let dst = mesh.he(HalfEdgeId(ei)).vertex.0;
                    let has_unsel = [src, dst].iter().any(|&v| {
                        mesh.vertex_fan(VertexId(v)).iter().any(|&he| {
                            let twin = mesh.he(he).twin;
                            let c = if twin == NO_TWIN || he.0 < twin.0 { he.0 } else { twin.0 };
                            mesh.he(HalfEdgeId(c)).face != NO_FACE && !self.poly_sel.selected_edges.contains(&c)
                        })
                    });
                    if has_unsel { to_remove.push(ei); }
                }
                for e in to_remove { self.poly_sel.selected_edges.remove(&e); }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let mut to_remove = Vec::new();
                for &fi in &self.poly_sel.selected_faces {
                    let has_unsel = mesh.face_half_edges(FaceId(fi)).iter().any(|&he| {
                        let twin = mesh.he(he).twin;
                        if twin == NO_TWIN { return true; }
                        let adj = mesh.he(twin).face;
                        adj == NO_FACE || !self.poly_sel.selected_faces.contains(&adj.0)
                    });
                    if has_unsel { to_remove.push(fi); }
                }
                for f in to_remove { self.poly_sel.selected_faces.remove(&f); }
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    // ============================================================
    // Hide / Unhide (vertex-centric)
    // ============================================================

    /// H — Hide selected elements.
    pub(crate) fn poly_hide_selected(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let mut to_hide = HashSet::new();
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                to_hide = self.poly_sel.selected_vertices.clone();
            }
            PolyWorkMode::Edge => {
                for &ei in &self.poly_sel.selected_edges {
                    to_hide.insert(mesh.he_source(HalfEdgeId(ei)).0);
                    to_hide.insert(mesh.he(HalfEdgeId(ei)).vertex.0);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &fi in &self.poly_sel.selected_faces {
                    for &v in &mesh.face_vertices(FaceId(fi)) {
                        to_hide.insert(v.0);
                    }
                }
            }
            PolyWorkMode::Object => return,
        }
        if to_hide.is_empty() { return; }

        let edge_verts: Vec<(u32, u32, u32)> = self.poly_sel.selected_edges.iter()
            .map(|&ei| (ei, mesh.he_source(HalfEdgeId(ei)).0, mesh.he(HalfEdgeId(ei)).vertex.0))
            .collect();
        let face_verts: Vec<(u32, Vec<u32>)> = self.poly_sel.selected_faces.iter()
            .map(|&fi| (fi, mesh.face_vertices(FaceId(fi)).iter().map(|v| v.0).collect()))
            .collect();

        self.poly_sel.hidden_vertices.extend(&to_hide);
        self.poly_sel.selected_vertices.retain(|v| !to_hide.contains(v));
        let hidden = &self.poly_sel.hidden_vertices;
        self.poly_sel.selected_edges.retain(|ei| {
            edge_verts.iter().find(|(e, _, _)| e == ei)
                .map(|(_, s, d)| !hidden.contains(s) && !hidden.contains(d))
                .unwrap_or(true)
        });
        self.poly_sel.selected_faces.retain(|fi| {
            face_verts.iter().find(|(f, _)| f == fi)
                .map(|(_, vs)| vs.iter().all(|v| !hidden.contains(v)))
                .unwrap_or(true)
        });
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Shift+H — Hide unselected (isolate).
    ///
    /// Empty selection must NOT hide everything (safe_verts empty ⇒ every vert hidden).
    /// Require an explicit selection — same rule as other isolate tools.
    pub(crate) fn poly_hide_unselected(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        let mut safe_verts = HashSet::new();
        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                safe_verts = self.poly_sel.selected_vertices.clone();
            }
            PolyWorkMode::Edge => {
                for &ei in &self.poly_sel.selected_edges {
                    safe_verts.insert(mesh.he_source(HalfEdgeId(ei)).0);
                    safe_verts.insert(mesh.he(HalfEdgeId(ei)).vertex.0);
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                for &fi in &self.poly_sel.selected_faces {
                    for &v in &mesh.face_vertices(FaceId(fi)) {
                        safe_verts.insert(v.0);
                    }
                }
            }
            PolyWorkMode::Object => return,
        }
        if safe_verts.is_empty() {
            self.toast("Hide Unselected: select something to keep visible first");
            return;
        }

        for i in 0..mesh.vertices.len() as u32 {
            if mesh.vert(VertexId(i)).half_edge != NO_EDGE && !safe_verts.contains(&i) {
                self.poly_sel.hidden_vertices.insert(i);
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Alt+H — Unhide all hidden geometry.
    pub(crate) fn poly_unhide_all(&mut self) {
        if self.poly_sel.hidden_vertices.is_empty() { return; }
        self.poly_sel.hidden_vertices.clear();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    // ============================================================
    // Select Linked (BFS flood-fill)
    // ============================================================

    /// L — Select all elements linked to the hovered element via BFS.
    pub(crate) fn poly_select_linked_hover(&mut self, shift: bool) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                let start = match self.poly_sel.hovered_vertex { Some(v) => v, None => return };
                if !shift { self.poly_sel.selected_vertices.clear(); }
                let linked = Self::bfs_linked_vertices(mesh, &[start], &self.poly_sel.hidden_vertices);
                self.poly_sel.selected_vertices.extend(linked);
            }
            PolyWorkMode::Edge => {
                let start = match self.poly_sel.hovered_edge { Some(e) => e, None => return };
                if !shift {
                    self.poly_sel.selected_edges.clear();
                    self.poly_edge_history.clear();
                }
                let src = mesh.he_source(HalfEdgeId(start)).0;
                let linked_verts = Self::bfs_linked_vertices(mesh, &[src], &self.poly_sel.hidden_vertices);
                for he_id in mesh.canonical_edges() {
                    let s = mesh.he_source(he_id).0;
                    let d = mesh.he(he_id).vertex.0;
                    if linked_verts.contains(&s) && linked_verts.contains(&d) {
                        self.poly_sel.selected_edges.insert(he_id.0);
                    }
                }
                self.poly_push_edge_history(start);
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let start = match self.poly_sel.hovered_face { Some(f) => f, None => return };
                if !shift {
                    self.poly_sel.selected_faces.clear();
                    self.poly_face_history.clear();
                }
                let linked = Self::bfs_linked_faces(mesh, &[start], &self.poly_sel.hidden_vertices);
                self.poly_sel.selected_faces.extend(linked);
                // History: seed becomes active (island select does not invent a loop direction).
                self.poly_push_face_history(start);
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Ctrl+L — Select all elements linked to the current selection via BFS.
    pub(crate) fn poly_select_linked_from_sel(&mut self) {
        let nid = match self.sel.usel.active_node() { Some(n) => n, None => return };
        let node = match self.scene.scene_graph.get(nid) { Some(n) => n, None => return };
        let mesh = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh,
            _ => return,
        };

        match self.poly_work_mode {
            PolyWorkMode::Vertex => {
                let seeds: Vec<u32> = self.poly_sel.selected_vertices.iter().copied().collect();
                if seeds.is_empty() { return; }
                let linked = Self::bfs_linked_vertices(mesh, &seeds, &self.poly_sel.hidden_vertices);
                self.poly_sel.selected_vertices.extend(linked);
            }
            PolyWorkMode::Edge => {
                if self.poly_sel.selected_edges.is_empty() { return; }
                let mut seed_verts = HashSet::new();
                for &ei in &self.poly_sel.selected_edges {
                    seed_verts.insert(mesh.he_source(HalfEdgeId(ei)).0);
                    seed_verts.insert(mesh.he(HalfEdgeId(ei)).vertex.0);
                }
                let seeds: Vec<u32> = seed_verts.into_iter().collect();
                let linked_verts = Self::bfs_linked_vertices(mesh, &seeds, &self.poly_sel.hidden_vertices);
                for he_id in mesh.canonical_edges() {
                    let s = mesh.he_source(he_id).0;
                    let d = mesh.he(he_id).vertex.0;
                    if linked_verts.contains(&s) && linked_verts.contains(&d) {
                        self.poly_sel.selected_edges.insert(he_id.0);
                    }
                }
            }
            PolyWorkMode::Face | PolyWorkMode::Group => {
                let seeds: Vec<u32> = self.poly_sel.selected_faces.iter().copied().collect();
                if seeds.is_empty() { return; }
                let linked = Self::bfs_linked_faces(mesh, &seeds, &self.poly_sel.hidden_vertices);
                self.poly_sel.selected_faces.extend(linked);
            }
            PolyWorkMode::Object => {}
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// BFS flood-fill through vertex adjacency. Returns all connected vertices.
    /// pub(crate): also reused by `poly/boolean.rs` island extraction (mesh-island cutter).
    pub(crate) fn bfs_linked_vertices(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        seeds: &[u32],
        hidden: &HashSet<u32>,
    ) -> HashSet<u32> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        for &s in seeds {
            if !hidden.contains(&s) {
                visited.insert(s);
                queue.push_back(s);
            }
        }
        while let Some(v) = queue.pop_front() {
            for he in mesh.vertex_fan(VertexId(v)) {
                let nb = mesh.he(he).vertex.0;
                if !visited.contains(&nb) && !hidden.contains(&nb) {
                    visited.insert(nb);
                    queue.push_back(nb);
                }
            }
        }
        visited
    }

    /// BFS flood-fill through face adjacency (via half-edge twins). Returns all connected faces.
    fn bfs_linked_faces(
        mesh: &core_math::half_edge::HalfEdgeMesh,
        seeds: &[u32],
        hidden: &HashSet<u32>,
    ) -> HashSet<u32> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        for &s in seeds {
            let dominated = mesh.face_vertices(FaceId(s)).iter().any(|v| hidden.contains(&v.0));
            if !dominated {
                visited.insert(s);
                queue.push_back(s);
            }
        }
        while let Some(fi) = queue.pop_front() {
            for he in mesh.face_half_edges(FaceId(fi)) {
                let twin = mesh.he(he).twin;
                if twin == NO_TWIN { continue; }
                let adj = mesh.he(twin).face;
                if adj == NO_FACE { continue; }
                let adj_id = adj.0;
                if visited.contains(&adj_id) { continue; }
                let adj_hidden = mesh.face_vertices(FaceId(adj_id)).iter().any(|v| hidden.contains(&v.0));
                if !adj_hidden {
                    visited.insert(adj_id);
                    queue.push_back(adj_id);
                }
            }
        }
        visited
    }
}

// Owner crash 2026-10-13: delete a vertex (dead slots stay), then K-connect two vertices →
// the knife rebuilt through `from_polygons`, compacting the mesh below the hovered id →
// `vert(301)` on a 228-vertex mesh in the same frame's overlay draw. The knife, dice,
// subdivide, poke, weld and boolean commits are now id-stable (`from_polygons_keeping_ids`),
// but join / add-to-body, Mesh Cleanup's merge and the bevel still compact — so this keeps the
// CRASH invariant for any compacting rebuild.
#[cfg(test)]
mod stale_component_id_tests {
    use crate::{FourthLayerApp, ModelingContext, PolyWorkMode};
    use core_math::half_edge::{HalfEdgeMesh, VertexId};
    use core_math::scene_graph::NodePayload;
    use glam::Vec3;
    use std::sync::Arc;

    /// n×n quad grid: (n+1)² vertices, highest ids at the end.
    fn grid(n: u32) -> HalfEdgeMesh {
        let mut p = Vec::new();
        for y in 0..=n { for x in 0..=n { p.push(Vec3::new(x as f32, y as f32, 0.0)); } }
        let mut faces = Vec::new();
        for y in 0..n { for x in 0..n {
            let i = y * (n + 1) + x;
            faces.push(vec![i as usize, (i + 1) as usize, (i + n + 2) as usize, (i + n + 1) as usize]);
        } }
        HalfEdgeMesh::from_polygons(&p, &faces)
    }

    /// A COMPACTING rebuild (what the knife / dice / boolean / weld commits did before 2026-10-13,
    /// and join / cleanup-merge / bevel still do): live faces only, vertices renumbered in
    /// traversal order, dead slots gone.
    fn rebuild_compacted(mesh: &HalfEdgeMesh) -> HalfEdgeMesh {
        let mut positions = Vec::new();
        let mut index = std::collections::HashMap::new();
        let mut polys = Vec::new();
        for f in 0..mesh.faces.len() as u32 {
            let f = core_math::half_edge::FaceId(f);
            if !mesh.is_face_live(f) { continue; }
            polys.push(mesh.face_vertices(f).into_iter().map(|v| *index.entry(v.0).or_insert_with(|| {
                positions.push(mesh.vert(v).position);
                positions.len() - 1
            })).collect::<Vec<usize>>());
        }
        HalfEdgeMesh::from_polygons(&positions, &polys)
    }

    #[test]
    fn rebuild_after_delete_drops_stale_hover_and_selection() {
        let mut mesh = grid(4); // 25 vertices
        // Delete interior vertices: their slots stay allocated (dead), ids do not move.
        for v in [6u32, 7, 8, 12] { mesh.delete_vertex(VertexId(v)); }
        let pre_len = mesh.vertices.len();
        let mut app = FourthLayerApp::new_headless();
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(root, NodePayload::PolygonalBody {
            name: "Grid".into(),
            mesh: Arc::new(mesh.clone()),
            mesh_cache_id: None,
            material: core_math::primitives::PbrMaterial::default(),
            modifiers: Vec::new(),
            source: core_math::primitive_spec::MeshSource::Authored,
        });
        app.select_single_unified(crate::SceneItemId::Node(nid));
        app.modeling_context = ModelingContext::Polygonal;
        app.poly_work_mode = PolyWorkMode::Vertex;
        app.reconcile_poly_sel_body(); // stamp the body (first-frame behaviour)

        // Hover + select the HIGHEST live ids of the pre-cut mesh, as the knife click leaves them.
        let high = (pre_len - 1) as u32;
        assert!(mesh.is_vert_live(VertexId(high)));
        app.poly_sel.hovered_vertex = Some(high);
        app.poly_sel.selected_vertices.insert(high);
        app.poly_active_vertex = Some(high);
        app.poly_vert_history.push(high);

        // A compacting rebuild, installed in place on the scene mesh.
        let rebuilt = rebuild_compacted(&mesh);
        assert!((high as usize) >= rebuilt.vertices.len(), "repro needs the rebuild to shrink below the hovered id");
        if let Some(NodePayload::PolygonalBody { mesh, .. }) = app.scene.scene_graph.get_mut(nid).map(|n| &mut n.payload) {
            *mesh = Arc::new(rebuilt);
        }

        // What the frame runs between the tool commit and the overlay draw.
        app.prune_poly_sel_to_live();

        let Some(NodePayload::PolygonalBody { mesh, .. }) = app.scene.scene_graph.get(nid).map(|n| &n.payload) else { unreachable!() };
        assert_eq!(app.poly_sel.hovered_vertex, None, "stale hover id must not survive the rebuild");
        assert!(app.poly_sel.selected_vertices.iter().all(|&v| mesh.is_vert_live(VertexId(v))));
        assert!(app.poly_active_vertex.map_or(true, |v| mesh.is_vert_live(VertexId(v))));
        assert!(app.poly_vert_history.iter().all(|&v| mesh.is_vert_live(VertexId(v))));
        // Exactly the overlay's read (`draw_poly.rs`): must be in bounds for every remaining id.
        for &v in &app.poly_sel.selected_vertices { let _ = mesh.vert(VertexId(v)).position; }
    }
}
