// core_engine/app/src/body_library.rs
// SDF Body Library (kitbash) — save bodies to disk, browse folders, drag into viewport.
//
// Performance rules:
//  • Scan is cached (refresh on open / Save / Delete) — never every frame.
//  • Drag only carries a path payload (no load / no bake while dragging).
//  • Drop inserts graph + marks dirty; bake is async via existing body-vol pipeline.

use eframe::egui;
use core_math::scene_graph::{NodeId, NodePayload};
use crate::*;
use crate::project::{BodyLibraryEntry, library_path, load_library_entry, scan_library};

/// Drag payload for body-library → viewport (path only — cheap).
#[derive(Clone)]
pub(crate) struct BodyLibraryDragPayload {
    pub path: std::path::PathBuf,
    pub name: String,
}

impl AzraApp {
    /// Open the save-to-library dialog for a body, group, **or poly mesh** node.
    pub(crate) fn open_library_save_dialog(&mut self, nid: NodeId) {
        let node = match self.scene.scene_graph.get(nid) {
            Some(n) => n,
            None => {
                self.toast("Cannot save: node not found in scene graph");
                return;
            }
        };
        let ok = matches!(
            node.payload,
            NodePayload::SdfBody { .. }
                | NodePayload::Group { .. }
                | NodePayload::SdfObject { .. }
                | NodePayload::PolygonalBody { .. }
        );
        if !ok {
            self.toast("Cannot save: select an SDF body, Group, or Polygon mesh");
            return;
        }
        let name = node.payload.display_name().to_string();
        self.ui.library_save_nid = Some(nid);
        self.ui.library_save_name = name;
        self.ui.library_save_name_focused = false; // request focus on next draw
        if self.ui.library_save_folder.is_empty() {
            self.ui.library_save_folder = self.ui.body_library_last_folder.clone();
        }
    }

    /// Save active/selected SDF body, Group, or Polygon mesh (kitbash part).
    pub(crate) fn open_library_save_for_selection(&mut self) {
        // Prefer selected Group (kitbash root) over body.
        for id in self.sel.usel.selected.iter() {
            if let SceneItemId::Node(nid) = id {
                if matches!(
                    self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                    Some(NodePayload::Group { .. })
                ) {
                    self.open_library_save_dialog(*nid);
                    return;
                }
            }
        }
        // Polygon mesh part — save the poly node itself (not top-level SDF body).
        for id in self.sel.usel.selected.iter() {
            if let SceneItemId::Node(nid) = id {
                if matches!(
                    self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                    Some(NodePayload::PolygonalBody { .. })
                ) {
                    self.open_library_save_dialog(*nid);
                    return;
                }
            }
        }
        for id in self.sel.usel.selected.iter() {
            if let SceneItemId::Node(nid) = id {
                if matches!(
                    self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                    Some(NodePayload::SdfBody { .. }) | Some(NodePayload::SdfObject { .. })
                ) {
                    // For nested body, save the top-level body OR its Group parent if any.
                    let mut cur = *nid;
                    if let Some(parent) = self.scene.scene_graph.get(cur).and_then(|n| n.parent) {
                        if matches!(
                            self.scene.scene_graph.get(parent).map(|n| &n.payload),
                            Some(NodePayload::Group { .. })
                        ) {
                            // If multi-body under group and user selected one body, still allow
                            // saving just that body unless they selected the group.
                            let top = self.scene.scene_graph.top_level_body(cur);
                            self.open_library_save_dialog(top);
                            return;
                        }
                    }
                    let top = self.scene.scene_graph.top_level_body(cur);
                    self.open_library_save_dialog(top);
                    return;
                }
            }
        }
        if let Some(idx) = self.active_prim_index_unified() {
            if let Some(&(nid, _)) = self.scene.prim_body_map.get(idx) {
                let top = self.scene.scene_graph.top_level_body(nid);
                self.open_library_save_dialog(top);
                return;
            }
        }
        let active = self.scene.active_body_nid;
        if matches!(
            self.scene.scene_graph.get(active).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. })
        ) {
            self.open_library_save_dialog(self.scene.scene_graph.top_level_body(active));
            return;
        }
        // Active poly selection (poly work mode) — use active_node if poly.
        if let Some(nid) = self.sel.usel.active_node() {
            if matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::PolygonalBody { .. })
            ) {
                self.open_library_save_dialog(nid);
                return;
            }
        }
        self.toast("Select an SDF body, Group, or Polygon mesh to save to library");
    }

    /// Build a BodyLibraryEntry from SdfBody / Group / SdfObject.
    ///
    /// **Iterative BFS** (no recursion — stack overflow was a process-kill risk).
    /// Strips mesh caches and caps MeshVolume / sculpt / spine data so serialize cannot OOM.
    /// Returns `(entry, sanitize_notes)` — notes are user-facing warnings (empty if clean).
    fn build_library_entry(&self, root_nid: NodeId) -> Option<(BodyLibraryEntry, Vec<String>)> {
        build_library_entry_from_graph(&self.scene.scene_graph, root_nid)
    }

    /// Insert a body/group from a library file into the current scene (drop / Insert).
    /// Graph insert + dirty only — bake is async (no hitch while dragging).
    /// `world_pos`: when `Some`, places the root node there; when `None`, keeps package transform
    /// with a small diagonal nudge so stacked inserts do not perfectly overlap.
    ///
    /// Returns the inserted root `NodeId` on success (starts an interactive place session).
    pub(crate) fn insert_body_from_library(&mut self, path: &std::path::Path) -> Option<NodeId> {
        self.insert_body_from_library_at(path, None)
    }

    pub(crate) fn insert_body_from_library_at(
        &mut self,
        path: &std::path::Path,
        world_pos: Option<glam::Vec3>,
    ) -> Option<NodeId> {
        let entry = match load_library_entry(path) {
            Ok(e) => e,
            Err(e) => {
                self.toast(&format!("Library load failed:\n{}", e));
                return None;
            }
        };
        let name = entry.name.clone();
        let mut placed: Option<NodeId> = None;
        self.transaction("Insert from library", |s| {
            let root = s.scene.scene_graph.root();
            let new_nid = s.insert_library_entry(entry, root);
            if let Some(nid) = new_nid {
                if let Some(world) = world_pos {
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        node.local_position = world;
                        if let NodePayload::SdfBody { origin, .. } = &mut node.payload {
                            *origin = world;
                        }
                    }
                } else {
                    // Offset slightly so stacked kitbash drops don't perfectly overlap.
                    if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                        if let NodePayload::SdfBody { origin, .. } = &mut node.payload {
                            *origin += glam::Vec3::new(0.35, 0.0, 0.35);
                        }
                        node.local_position += glam::Vec3::new(0.35, 0.0, 0.35);
                    }
                }
                s.clear_selection_unified();
                s.select_single_unified(SceneItemId::Node(nid));
                // Prefer first SDF body under the root for active body.
                let active = s
                    .scene
                    .scene_graph
                    .walk_depth_first(nid)
                    .find_map(|(_, id, n)| {
                        if matches!(n.payload, NodePayload::SdfBody { .. }) {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(nid);
                s.scene.active_body_nid = active;
                s.sync_flat_cache();
                s.sync_duplicated_mesh_caches(nid);
                // Poly parts need Hybrid so the solid draws immediately (not SDF-only).
                let has_poly = s.scene.scene_graph.walk_depth_first(nid).any(|(_, _, n)| {
                    matches!(n.payload, NodePayload::PolygonalBody { .. })
                });
                if has_poly {
                    s.render.viewport_mode = ViewportRenderMode::Hybrid;
                }
                // Dirty all SDF bodies in the inserted subtree (async bake).
                let body_ids: Vec<NodeId> = s
                    .scene
                    .scene_graph
                    .walk_depth_first(nid)
                    .filter_map(|(_, id, n)| {
                        if matches!(n.payload, NodePayload::SdfBody { .. }) {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .collect();
                for bid in body_ids {
                    s.mark_node_body_vol_dirty(bid);
                }
                s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
                s.cache.prim_buf_stale = true;
                s.outliner.outliner_dirty = true;
                placed = Some(nid);
            }
        });
        if let Some(nid) = placed {
            self.begin_library_place_session(nid);
            self.toast(&format!(
                "Placing \"{}\" — Ctrl: surface · Scroll: resize · Click/Enter: confirm · Esc: cancel",
                name
            ));
        } else {
            self.toast(&format!("Insert failed for \"{}\"", name));
        }
        placed
    }

    /// Arm interactive place after a successful library insert (drag or button).
    pub(crate) fn begin_library_place_session(&mut self, nid: NodeId) {
        let (base_scale, base_rot) = self
            .scene
            .scene_graph
            .get(nid)
            .map(|n| (n.local_scale, n.local_rotation))
            .unwrap_or((glam::Vec3::ONE, glam::Vec3::ZERO));
        self.ui.library_place = Some(crate::types::LibraryPlaceSession {
            node_id: nid,
            scale_mul: 1.0,
            base_scale,
            base_rot,
            ignore_lmb: true,
        });
    }

    /// Confirm library place (keep node where it is).
    pub(crate) fn confirm_library_place(&mut self) {
        if let Some(sess) = self.ui.library_place.take() {
            let name = self
                .scene
                .scene_graph
                .get(sess.node_id)
                .map(|n| n.payload.display_name().to_string())
                .unwrap_or_else(|| "part".into());
            self.toast(&format!("Placed \"{}\"", name));
            self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
        }
    }

    /// Cancel library place — remove the just-inserted node.
    pub(crate) fn cancel_library_place(&mut self) {
        let Some(sess) = self.ui.library_place.take() else {
            return;
        };
        let nid = sess.node_id;
        if self.scene.scene_graph.get(nid).is_none() {
            return;
        }
        self.transaction("Cancel library place", |s| {
            s.delete_node_no_undo(nid);
            s.sync_flat_cache();
            s.sanitize_selection();
            s.sanitize_selection_unified();
            s.poly_sel.selected_faces.clear();
            s.poly_sel.selected_edges.clear();
            s.poly_sel.selected_vertices.clear();
            s.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
            s.cache.prim_buf_stale = true;
            s.outliner.outliner_dirty = true;
        });
        self.toast("Library place cancelled");
    }

    /// Per-frame interactive place: follow cursor, Ctrl surface-align, scroll resize.
    ///
    /// Returns:
    /// - `false` if no session
    /// - `true` if session is active (caller should skip LMB pick/select; MMB orbit still runs)
    ///
    /// Confirm/cancel clear the session and still return `true` for that frame so the same
    /// click is not reused for selection.
    pub(crate) fn tick_library_place_session(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
    ) -> bool {
        if self.ui.library_place.is_none() {
            return false;
        }

        // Esc / RMB → cancel (RMB is cancel here; Alt+RMB etc. still free for other tools later)
        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || response.clicked_by(egui::PointerButton::Secondary)
        {
            self.cancel_library_place();
            return true;
        }
        // Enter → confirm
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.confirm_library_place();
            return true;
        }

        let mut sess = match self.ui.library_place.clone() {
            Some(s) => s,
            None => return false,
        };
        if self.scene.scene_graph.get(sess.node_id).is_none() {
            self.ui.library_place = None;
            return false;
        }

        // Swallow the drop-release that started the session.
        if sess.ignore_lmb {
            if !ctx.input(|i| i.pointer.primary_down()) {
                sess.ignore_lmb = false;
            }
        } else if response.clicked_by(egui::PointerButton::Primary)
            || ctx.input(|i| i.pointer.primary_clicked())
        {
            self.ui.library_place = Some(sess);
            self.confirm_library_place();
            return true;
        }

        // Scroll → uniform resize (one physical notch ≈ 10%). Owns the wheel while placing
        // so the camera does not also dolly-zoom on the same notch.
        let scroll_y = ctx.input(|i| {
            let s = i.smooth_scroll_delta;
            if s.y.abs() >= s.x.abs() {
                s.y
            } else {
                s.x
            }
        });
        if scroll_y.abs() > 0.5 {
            // Positive scroll (up) = grow. Clamp so parts never invert or explode.
            let notches =
                (scroll_y / crate::input_viewport::WHEEL_UNITS_PER_NOTCH).clamp(-8.0, 8.0);
            let factor = 1.1f32.powf(notches);
            sess.scale_mul = (sess.scale_mul * factor).clamp(0.05, 50.0);
        }

        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        // While MMB orbits, freeze cursor-follow so the part does not jump under the drag.
        let orbiting = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        if !orbiting {
            let mouse = ctx
                .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
                .or_else(|| response.hover_pos())
                .or_else(|| response.interact_pointer_pos());
            if let Some(screen) = mouse {
                self.apply_library_place_transform(
                    sess.node_id,
                    screen,
                    ctrl,
                    sess.scale_mul,
                    sess.base_scale,
                    sess.base_rot,
                );
            }
        } else {
            // Still apply scale changes from scroll while orbiting.
            if let Some(node) = self.scene.scene_graph.get_mut(sess.node_id) {
                node.local_scale = sess.base_scale * sess.scale_mul;
            }
        }

        self.ui.library_place = Some(sess);
        // Keep animating under the cursor even when idle otherwise.
        ctx.request_repaint();
        true
    }

    /// Apply world pos / rot / scale for the live library place preview.
    fn apply_library_place_transform(
        &mut self,
        nid: NodeId,
        screen: egui::Pos2,
        surface_align: bool,
        scale_mul: f32,
        base_scale: glam::Vec3,
        base_rot: glam::Vec3,
    ) {
        let rect = self.viewport.viewport_rect;
        if rect.width() < 4.0 || rect.height() < 4.0 {
            return;
        }
        let scale = base_scale * scale_mul;

        let (world, rot) = if surface_align {
            let (o, d) = self.cam_ray(screen, rect);
            if let Some((hit, n)) = self.raycast_scene_surface_excluding(o, d, nid) {
                let half = self.library_place_surface_offset(nid, scale, n);
                let pos = hit + n * half;
                let rot = Self::euler_from_workplane_normal(n);
                (pos, rot)
            } else {
                // No surface under cursor — fall back to ground/workplane, keep package rot.
                (self.ui2_world_under_cursor(screen), base_rot)
            }
        } else {
            (self.ui2_world_under_cursor(screen), base_rot)
        };

        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            node.local_position = world;
            node.local_rotation = rot;
            node.local_scale = scale;
            if let NodePayload::SdfBody { origin, .. } = &mut node.payload {
                *origin = world;
            }
        }
        self.mark_dirty(DirtyFlags::SCENE | DirtyFlags::RENDER);
    }

    /// Distance along surface normal so the part sits ON the surface (half-extent of AABB).
    fn library_place_surface_offset(
        &self,
        nid: NodeId,
        scale: glam::Vec3,
        normal: glam::Vec3,
    ) -> f32 {
        let Some(node) = self.scene.scene_graph.get(nid) else {
            return 0.05;
        };
        let (mn, mx) = match &node.payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.bounding_box(),
            _ => {
                // SDF kitbash: use a modest lift so the body is not buried.
                return 0.05 * scale.y.abs().max(0.05);
            }
        };
        let ext = (mx - mn) * 0.5;
        let ext = glam::Vec3::new(
            ext.x.abs() * scale.x.abs(),
            ext.y.abs() * scale.y.abs(),
            ext.z.abs() * scale.z.abs(),
        );
        // Project local AABB half-extents onto world normal (local Y was package-up).
        // After orient-to-normal, local +Y aligns with normal — use Y half-extent.
        let n = normal.normalize_or_zero();
        if n.length_squared() < 0.25 {
            return ext.y.max(0.01);
        }
        // Conservative: max of axis-aligned extent projection.
        let lift = ext.x * n.x.abs() + ext.y * n.y.abs() + ext.z * n.z.abs();
        lift.max(0.01)
    }

    /// Scene surface hit that ignores the node being placed (and its children).
    fn raycast_scene_surface_excluding(
        &self,
        origin: glam::Vec3,
        dir: glam::Vec3,
        exclude: NodeId,
    ) -> Option<(glam::Vec3, glam::Vec3)> {
        let is_excluded = |nid: NodeId| -> bool {
            let mut cur = nid;
            loop {
                if cur == exclude {
                    return true;
                }
                match self.scene.scene_graph.get(cur).and_then(|node| node.parent) {
                    Some(p) if p != cur => cur = p,
                    _ => return false,
                }
            }
        };

        // SDF analytical hit (kitbash SDF bodies bake into mesh eventually; during place they
        // may still be field-only — fine to keep; poly parts are mesh-only).
        let sdf = self.raycast_scene(origin, dir);

        // Mesh + live poly: nearest hit whose node is not under `exclude`.
        let mut best: Option<(f32, glam::Vec3, glam::Vec3)> = None; // t, point, normal
        if let Some((p, n, nid)) = self.raycast_scene_meshes(origin, dir) {
            if !is_excluded(nid) {
                let t = (p - origin).length();
                best = Some((t, p, n));
            }
        }
        // If the nearest mesh was ourselves, walk every other poly body for a second hit.
        if best.is_none() {
            for (nid, node) in self.scene.scene_graph.poly_bodies() {
                if is_excluded(nid) || !node.visible {
                    continue;
                }
                let mesh = match &node.payload {
                    NodePayload::PolygonalBody { mesh, .. } => mesh,
                    _ => continue,
                };
                let world = self.scene.scene_graph.world_transform(nid);
                let no_hidden = std::collections::HashSet::new();
                if let Some((t, fid)) =
                    crate::poly_selection::ray_pick_mesh_face(mesh, world, origin, dir, &no_hidden)
                {
                    if best.map_or(true, |(bt, _, _)| t < bt) {
                        let ln = mesh.compute_face_normal(core_math::half_edge::FaceId(fid));
                        let mut n = world.transform_vector3(ln).normalize_or_zero();
                        if n.dot(dir) > 0.0 {
                            n = -n;
                        }
                        best = Some((t, origin + dir * t, n));
                    }
                }
            }
        }
        let mesh = best.map(|(_, p, n)| (p, n));
        match (sdf, mesh) {
            (Some(s), Some(m)) => Some(
                if (s.0 - origin).length_squared() <= (m.0 - origin).length_squared() {
                    s
                } else {
                    m
                },
            ),
            (Some(s), None) => Some(s),
            (None, Some(m)) => Some(m),
            (None, None) => None,
        }
    }

    /// Recursively insert a BodyLibraryEntry into the scene graph with fresh IDs.
    /// Restores `child_order` from package-local indices (prim index / child index).
    fn insert_library_entry(
        &mut self,
        entry: BodyLibraryEntry,
        parent: NodeId,
    ) -> Option<NodeId> {
        use core_math::scene_graph::BodyChildRef;
        let mut payload = entry.node.payload;
        // Package stores child_order as local indices (see encode_child_order_local).
        let mut local_order: Option<Vec<BodyChildRef>> = None;

        match &mut payload {
            NodePayload::SdfBody {
                primitives,
                mesh_cache_id,
                child_order,
                name,
                ..
            } => {
                if !entry.name.is_empty() {
                    *name = entry.name.clone();
                }
                local_order = Some(std::mem::take(child_order));
                for p in primitives.iter_mut() {
                    p.prim_uid = self.scene.next_prim_id;
                    self.scene.next_prim_id += 1;
                }
                *mesh_cache_id = None;
            }
            NodePayload::Group { name } | NodePayload::SdfObject { name, .. } => {
                if !entry.name.is_empty() {
                    *name = entry.name.clone();
                }
            }
            NodePayload::PolygonalBody {
                name,
                mesh_cache_id,
                ..
            } => {
                if !entry.name.is_empty() {
                    *name = entry.name.clone();
                }
                *mesh_cache_id = None; // force GPU re-upload
            }
            _ => {}
        }

        let pos = entry.node.local_position;
        let rot = entry.node.local_rotation;
        let scl = entry.node.local_scale;

        let new_nid =
            self.scene
                .scene_graph
                .add_child_with_transform(parent, payload, pos, rot, scl);

        let mut child_nids: Vec<NodeId> = Vec::with_capacity(entry.children.len());
        for child_entry in entry.children {
            if let Some(cid) = self.insert_library_entry(child_entry, new_nid) {
                child_nids.push(cid);
            }
        }

        // Decode local child_order → real prim UIDs + child NodeIds.
        if let Some(local) = local_order {
            if let Some(node) = self.scene.scene_graph.get_mut(new_nid) {
                if let NodePayload::SdfBody {
                    primitives,
                    child_order,
                    ..
                } = &mut node.payload
                {
                    let mut remapped = Vec::with_capacity(local.len());
                    for r in local {
                        match r {
                            BodyChildRef::Prim(i) => {
                                if let Some(p) = primitives.get(i as usize) {
                                    remapped.push(BodyChildRef::Prim(p.prim_uid));
                                }
                            }
                            BodyChildRef::Body(NodeId(i)) => {
                                if let Some(&cid) = child_nids.get(i as usize) {
                                    remapped.push(BodyChildRef::Body(cid));
                                }
                            }
                        }
                    }
                    *child_order = remapped;
                }
            }
        }
        // Heal any stale/missing slots without wiping a valid authored order.
        self.scene.scene_graph.ensure_body_child_order(new_nid);

        Some(new_nid)
    }

    /// Rescan library dir into UI cache (subfolders = kitbash categories).
    pub(crate) fn refresh_body_library_cache(&mut self) {
        self.ui.body_library_entries = scan_library_kitbash();
        self.ui.body_library_needs_scan = false;
    }

    /// Draw the save-to-library dialog (name + kitbash folder).
    pub(crate) fn draw_library_save_dialog(&mut self, ctx: &egui::Context) {
        let nid = match self.ui.library_save_nid {
            Some(nid) => nid,
            None => return,
        };

        let mut open = true;
        let mut do_save = false;
        let mut do_cancel = false;

        // Known kitbash folders for quick pick
        let known_folders: Vec<String> = {
            let mut set = std::collections::BTreeSet::new();
            for (label, _) in &self.ui.body_library_entries {
                if let Some((folder, _)) = label.split_once('/') {
                    if !folder.is_empty() {
                        set.insert(folder.to_string());
                    }
                }
            }
            if !self.ui.body_library_last_folder.is_empty() {
                set.insert(self.ui.body_library_last_folder.clone());
            }
            set.into_iter().collect()
        };

        let is_poly = matches!(
            self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::PolygonalBody { .. })
        );
        let win_title = if is_poly {
            "Save to Part (Library)"
        } else {
            "Save to Library (Kitbash)"
        };
        let blurb = if is_poly {
            "Save polygon mesh as a reusable library part.\nThumbnail: gray matcap, same ¾ angle as SDF parts, square crop, no background."
        } else {
            "Save SDF body for kitbash reuse.\nThumbnail: gray matcap, same ¾ angle as poly parts (display mesh), square crop, no background."
        };
        egui::Window::new(win_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(blurb).weak().small());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let name_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.ui.library_save_name)
                            .desired_width(220.0)
                            .id_source("library_save_name"),
                    );
                    // Request focus once when the dialog opens so typing is not lost to the viewport.
                    if !self.ui.library_save_name_focused {
                        name_resp.request_focus();
                        self.ui.library_save_name_focused = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Folder:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.ui.library_save_folder)
                            .hint_text("kitbash / props / …  (empty = root)")
                            .desired_width(180.0)
                            .id_source("library_save_folder"),
                    );
                    resp.on_hover_text(
                        "Subfolder under the body library (kitbash category).\n\
                         Leave empty for library root.\n\
                         Path: %LOCALAPPDATA%/Azra/library/",
                    );
                    if ui
                        .small_button("Open")
                        .on_hover_text("Open library folder in Explorer")
                        .clicked()
                    {
                        let dir = if self.ui.library_save_folder.trim().is_empty() {
                            library_path()
                        } else {
                            library_path().join(self.ui.library_save_folder.trim())
                        };
                        let _ = std::fs::create_dir_all(&dir);
                        #[cfg(target_os = "windows")]
                        {
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        }
                    }
                });
                if !known_folders.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.small("Quick:");
                        for f in &known_folders {
                            if ui.small_button(f).clicked() {
                                self.ui.library_save_folder = f.clone();
                            }
                        }
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        do_save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        if !open || do_cancel {
            self.ui.library_save_nid = None;
            self.ui.library_save_name_focused = false;
            return;
        }

        if do_save {
            // CRITICAL: Save click must do almost nothing. Clone/serialize/write run in poll + worker.
            // Prior versions cloned MeshVolume/sculpt on the UI click path → freeze/crash.
            let name = self.ui.library_save_name.clone();
            let folder = self.ui.library_save_folder.clone();
            self.ui.library_save_nid = None;
            self.ui.library_save_name_focused = false;

            if name.trim().is_empty() {
                self.toast("Name cannot be empty");
                return;
            }
            if self.pending_library_save.is_some() || self.pending_library_save_request.is_some() {
                self.toast("Library save already in progress…");
                return;
            }

            let dir = if folder.trim().is_empty() {
                library_path()
            } else {
                let mut sub = library_path();
                for part in folder.trim().replace('\\', "/").split('/') {
                    let p = part.trim();
                    if !p.is_empty() && p != "." && p != ".." {
                        sub = sub.join(p);
                    }
                }
                sub
            };

            self.ui.body_library_last_folder = folder.trim().to_string();
            self.pending_library_save_request = Some(crate::PendingLibrarySaveRequest {
                nid,
                name: name.trim().to_string(),
                dir,
            });
            self.toast("Saving to library…");
            eprintln!("[Library] queued save \"{}\" nid={:?}", name.trim(), nid);
        }
    }

    /// Poll async library save (call from poll_background_tasks — never from draw).
    ///
    /// Phase A: start a deferred request (build entry + spawn worker).
    /// Phase B: complete worker result. Never join. Never GPU thumb.
    pub(crate) fn poll_library_save(&mut self) {
        // ── Phase A: start queued request (one per poll) ──
        if self.pending_library_save.is_none() {
            if let Some(req) = self.pending_library_save_request.take() {
                self.start_library_save_worker(req);
            }
        }

        // ── Phase B: complete in-flight worker ──
        let Some(pending) = self.pending_library_save.as_ref() else {
            return;
        };
        match pending.rx.try_recv() {
            Ok(Ok((path, _thumb))) => {
                let name = pending.display_name.clone();
                let notes = pending.notes.clone();
                self.pending_library_save = None;
                // Never queue GPU screenshot here (device-loss risk under PT/bake).
                self.ui.body_library_needs_scan = true;
                // Rescan only if panel open (cheap fs list).
                if self.ui.show_body_library {
                    self.refresh_body_library_cache();
                }
                // Content library also lists Bodies — rescan NOW, not "next time the panel
                // opens". Clearing `scanned` alone only works while the library dock is open
                // (ensure_library_scanned is gated on it); saved with the dock collapsed, the
                // walk never runs, the forever index keeps its pre-save contents, and the next
                // launch reloads that stale index — the part never appears.
                self.mark_library_catalog_dirty();
                let fname = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if notes.is_empty() {
                    self.toast(&format!("Saved to library: {fname}"));
                } else {
                    let n = notes.len().min(2);
                    let head: Vec<&str> = notes.iter().take(n).map(|s| s.as_str()).collect();
                    let extra = if notes.len() > n {
                        format!(" (+{} more)", notes.len() - n)
                    } else {
                        String::new()
                    };
                    self.toast(&format!(
                        "Saved {fname} — simplified: {}{extra}",
                        head.join("; ")
                    ));
                }
                eprintln!("[Library] saved \"{name}\" → {}", path.display());
            }
            Ok(Err(e)) => {
                self.pending_library_save = None;
                self.toast(&format!("Library save failed: {e}"));
                eprintln!("[Library] save error: {e}");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_library_save = None;
                self.toast("Library save failed (worker died)");
            }
        }
    }

    /// Build entry + spawn serialize worker. Called from poll only.
    fn start_library_save_worker(&mut self, req: crate::PendingLibrarySaveRequest) {
        let crate::PendingLibrarySaveRequest { nid, name, dir } = req;

        // Build entry on main thread but iteratively + catch_unwind (poll, not draw click).
        let t0 = std::time::Instant::now();
        let built = crate::panic_safety::catch_recoverable(std::panic::AssertUnwindSafe(|| {
            self.build_library_entry(nid)
        }));
        let (entry, notes, thumb_rgb, thumb_rgba) = match built {
            Ok(Some((mut e, notes))) => {
                e.name = name.clone();
                let rgb = kitbash_thumb_color_from_entry(&e);
                // Same gray matcap + ¾ view for poly parts AND SDF bodies (owner: SDF
                // save must match poly part thumbs — not the solid kitbash brick).
                // Prefer live display mesh (mesh_cache / half-edge); fall back to entry mesh.
                let rgba = self
                    .library_matcap_thumb_for_node(nid)
                    .or_else(|| poly_matcap_thumb_from_entry(&e));
                (e, notes, rgb, rgba)
            }
            Ok(None) => {
                self.toast("Cannot save: unsupported node or empty entry");
                eprintln!("[Library] build returned None for {:?}", nid);
                return;
            }
            Err(_) => {
                self.toast("Cannot save: packaging panicked (body too heavy)");
                eprintln!("[Library] build_library_entry panicked for {:?}", nid);
                return;
            }
        };
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "[Library] built entry \"{}\" in {build_ms:.0}ms — spawning write worker",
            name
        );

        match spawn_library_save(entry, dir, thumb_rgb, thumb_rgba) {
            Ok(rx) => {
                self.pending_library_save = Some(crate::PendingLibrarySave {
                    rx,
                    display_name: name,
                    focus_nid: Some(nid),
                    notes,
                });
            }
            Err(e) => self.toast(&format!("Library save failed: {e}")),
        }
    }

    /// Draw the body library browser panel (kitbash).
    pub(crate) fn draw_body_library(&mut self, ctx: &egui::Context) {
        if !self.ui.show_body_library {
            return;
        }

        // Scan once when opened / after save-delete — never every frame (avoids stutter).
        if self.ui.body_library_needs_scan || self.ui.body_library_entries.is_empty() {
            // Only force empty→scan once; needs_scan covers refresh.
            if self.ui.body_library_needs_scan || !self.ui.body_library_scanned_once {
                self.refresh_body_library_cache();
                self.ui.body_library_scanned_once = true;
            }
        }

        let mut open = true;
        let mut pending_insert: Option<std::path::PathBuf> = None;
        let mut pending_delete: Option<std::path::PathBuf> = None;
        let mut do_refresh = false;

        egui::Window::new("Body Library (Kitbash)")
            .open(&mut open)
            .default_width(300.0)
            .default_height(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Open Folder")
                        .on_hover_text("%LOCALAPPDATA%/Azra/library/")
                        .clicked()
                    {
                        let dir = library_path();
                        #[cfg(target_os = "windows")]
                        {
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        }
                    }
                    if ui.button("Refresh").clicked() {
                        do_refresh = true;
                    }
                    ui.weak(format!("{} items", self.ui.body_library_entries.len()));
                });
                ui.label(
                    egui::RichText::new("Drag into viewport to place · no hitch while dragging")
                        .small()
                        .weak(),
                );
                ui.separator();

                let entries = self.ui.body_library_entries.clone();
                if entries.is_empty() {
                    ui.weak(
                        "No bodies saved yet.\n\
                         Right-click a body → Save to Library.",
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let thumb_size = egui::vec2(56.0, 56.0);
                            let mut last_folder = String::new();

                            for (label, path) in &entries {
                                let (folder, name) = match label.rsplit_once('/') {
                                    Some((f, n)) => (f, n),
                                    None => ("", label.as_str()),
                                };
                                if folder != last_folder {
                                    if !folder.is_empty() {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new(folder)
                                                .strong()
                                                .color(egui::Color32::from_rgb(160, 190, 220)),
                                        );
                                    }
                                    last_folder = folder.to_string();
                                }

                                ui.horizontal(|ui| {
                                    // Thumbnail / drag handle
                                    let thumb_path = path.with_extension("thumb.png");
                                    let (rect, resp) = ui.allocate_exact_size(
                                        thumb_size,
                                        egui::Sense::click_and_drag(),
                                    );
                                    if thumb_path.exists() {
                                        let uri = format!("file://{}", thumb_path.display());
                                        // Paint image in rect without blocking drag
                                        ui.put(
                                            rect,
                                            egui::Image::new(uri).fit_to_exact_size(thumb_size),
                                        );
                                    } else {
                                        ui.painter().rect_filled(
                                            rect,
                                            4.0,
                                            egui::Color32::from_gray(40),
                                        );
                                        ui.painter().text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "\u{2B21}",
                                            egui::FontId::proportional(22.0),
                                            egui::Color32::from_gray(90),
                                        );
                                    }
                                    // Drag: payload only (path) — never load .flb here.
                                    if resp.dragged() && resp.drag_delta().length() > 3.0 {
                                        egui::DragAndDrop::set_payload(
                                            ui.ctx(),
                                            BodyLibraryDragPayload {
                                                path: path.clone(),
                                                name: name.to_string(),
                                            },
                                        );
                                    }
                                    if resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                    }

                                    ui.vertical(|ui| {
                                        ui.label(name);
                                        if !folder.is_empty() {
                                            ui.weak(folder);
                                        }
                                        ui.horizontal(|ui| {
                                            if ui
                                                .small_button("Insert")
                                                .on_hover_text("Insert into scene (same as drop)")
                                                .clicked()
                                            {
                                                pending_insert = Some(path.clone());
                                            }
                                            if ui
                                                .small_button("\u{1F5D1}")
                                                .on_hover_text("Delete from library")
                                                .clicked()
                                            {
                                                pending_delete = Some(path.clone());
                                            }
                                        });
                                    });
                                });
                                // Also allow dragging the label row
                                ui.separator();
                            }
                        });
                }

                // Delete confirmation
                if let Some(del_path) = self.ui.body_library_confirm_delete.clone() {
                    let fname = del_path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 120, 80),
                        format!("Delete \"{}\" from library?", fname),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Yes, delete").clicked() {
                            let thumb = del_path.with_extension("thumb.png");
                            let _ = std::fs::remove_file(&thumb);
                            if let Err(e) = std::fs::remove_file(&del_path) {
                                self.toast(&format!("Delete failed: {}", e));
                            } else {
                                self.toast(&format!("Deleted: {}", fname));
                                self.ui.body_library_needs_scan = true;
                            }
                            self.ui.body_library_confirm_delete = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.ui.body_library_confirm_delete = None;
                        }
                    });
                }
            });

        if do_refresh {
            self.refresh_body_library_cache();
        }
        if let Some(p) = pending_insert {
            let _ = self.insert_body_from_library(&p);
        }
        if let Some(p) = pending_delete {
            self.ui.body_library_confirm_delete = Some(p);
        }
        if !open {
            self.ui.show_body_library = false;
        }
    }

    /// Ghost + drop for body library drag → viewport (load only on release).
    pub(crate) fn handle_body_library_drag_drop(&mut self, ctx: &egui::Context) {
        let payload = egui::DragAndDrop::payload::<BodyLibraryDragPayload>(ctx);
        let Some(pl) = payload else { return };

        // Ghost near cursor — no I/O.
        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
            let name = pl.name.clone();
            let ghost_pos = pos + egui::vec2(14.0, 14.0);
            egui::Area::new(egui::Id::new("body_lib_drag_ghost"))
                .fixed_pos(ghost_pos)
                .order(egui::Order::Tooltip)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(40, 55, 70, 230))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("\u{2B21} {}", name))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(200, 220, 255)),
                            );
                            ui.label(
                                egui::RichText::new("drop · Ctrl surface · scroll resize")
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(160)),
                            );
                        });
                });
        }

        // Drop only when released over the 3D viewport (not the library window).
        if ctx.input(|i| i.pointer.any_released()) {
            let pos = ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()));
            let over_vp = pos
                .map(|p| self.viewport.viewport_rect.contains(p))
                .unwrap_or(false);
            if over_vp {
                let path = pl.path.clone();
                let world = pos.map(|p| self.ui2_world_under_cursor(p));
                let _ = self.insert_body_from_library_at(&path, world);
            }
        }
    }
}

/// Iterative BFS build — never recursive (stack overflow aborts the process; catch_unwind cannot recover).
///
/// `child_order` is encoded as **local indices** (prim index into `primitives`, body index into
/// the package's `children` list) so boolean interleaving survives save/load after UID remap.
fn build_library_entry_from_graph(
    sg: &core_math::scene_graph::SceneGraph,
    root_nid: NodeId,
) -> Option<(BodyLibraryEntry, Vec<String>)> {
    use core_math::scene_graph::BodyChildRef;
    use std::collections::{HashMap, HashSet, VecDeque};

    let root = sg.get(root_nid)?;
    // Leaf poly mesh part — single node, no kitbash BFS.
    if matches!(root.payload, NodePayload::PolygonalBody { .. }) {
        let name = root.payload.display_name().to_string();
        let mut saved_node = root.clone();
        if let NodePayload::PolygonalBody {
            mesh,
            mesh_cache_id,
            name: n,
            modifiers,
            source,
            ..
        } = &mut saved_node.payload
        {
            // Bake modifiers into the mesh and clear the stack. PolyModifier wire grows
            // append-only; shipping a live stack in `.flb` breaks load after any new field
            // (owner: library parts failed with EOF at modifiers). Library parts are frozen
            // geometry — base + evaluated stack, no live modifiers.
            if !modifiers.is_empty() {
                let empty_cutters = std::collections::HashMap::new();
                let baked = crate::FourthLayerApp::apply_poly_modifiers(
                    mesh.as_ref(),
                    modifiers,
                    &empty_cutters,
                );
                *mesh = std::sync::Arc::new(baked);
                modifiers.clear();
                *source = core_math::primitive_spec::MeshSource::Authored;
            }
            *mesh_cache_id = None; // re-upload on insert
            if !name.is_empty() {
                *n = name.clone();
            }
        }
        saved_node.parent = None;
        saved_node.children = Vec::new();
        return Some((
            BodyLibraryEntry {
                version: 1,
                name,
                node: saved_node,
                children: Vec::new(),
            },
            Vec::new(),
        ));
    }
    if !matches!(
        root.payload,
        NodePayload::SdfBody { .. } | NodePayload::Group { .. } | NodePayload::SdfObject { .. }
    ) {
        return None;
    }

    let mut notes: Vec<String> = Vec::new();

    // Collect (nid, parent_in_save_tree) via BFS.
    let mut order: Vec<NodeId> = Vec::new();
    let mut parent_of: HashMap<u64, Option<u64>> = HashMap::new();
    let mut visited: HashSet<u64> = HashSet::new();
    let mut q = VecDeque::new();
    q.push_back(root_nid);
    parent_of.insert(root_nid.0, None);
    visited.insert(root_nid.0);

    const MAX_NODES: usize = 2_048;
    while let Some(nid) = q.pop_front() {
        if order.len() >= MAX_NODES {
            notes.push(format!("Tree truncated at {MAX_NODES} nodes"));
            eprintln!("[Library] truncating kitbash tree at {MAX_NODES} nodes");
            break;
        }
        order.push(nid);
        for &cid in sg.children(nid) {
            if !visited.insert(cid.0) {
                continue;
            }
            let Some(cn) = sg.get(cid) else { continue };
            if !matches!(
                cn.payload,
                NodePayload::SdfBody { .. }
                    | NodePayload::Group { .. }
                    | NodePayload::SdfObject { .. }
            ) {
                continue;
            }
            parent_of.insert(cid.0, Some(nid.0));
            q.push_back(cid);
        }
    }

    // Direct kitbash children per node (graph children order) — package `children` list order.
    let mut kitbash_kids: HashMap<u64, Vec<NodeId>> = HashMap::new();
    for &nid in &order {
        let kids: Vec<NodeId> = sg
            .children(nid)
            .iter()
            .copied()
            .filter(|&c| {
                matches!(
                    sg.get(c).map(|n| &n.payload),
                    Some(NodePayload::SdfBody { .. })
                        | Some(NodePayload::Group { .. })
                        | Some(NodePayload::SdfObject { .. })
                ) && visited.contains(&c.0)
            })
            .collect();
        kitbash_kids.insert(nid.0, kids);
    }

    let mut entries: HashMap<u64, BodyLibraryEntry> = HashMap::new();
    for &nid in &order {
        let node = match sg.get(nid) {
            Some(n) => n,
            None => continue,
        };
        let name = node.payload.display_name().to_string();
        let mut saved_node = node.clone();
        if let NodePayload::SdfBody {
            mesh_cache_id,
            child_order,
            primitives,
            ..
        } = &mut saved_node.payload
        {
            *mesh_cache_id = None;
            // Encode boolean order as local indices BEFORE sanitize mutates shapes only.
            let kids = kitbash_kids.get(&nid.0).map(|v| v.as_slice()).unwrap_or(&[]);
            // Prefer live reconciled order (authoritative) over raw stored.
            let live = sg.body_child_order(nid);
            *child_order = encode_child_order_local(&live, primitives, kids);
            let mut pnotes = sanitize_prims_for_library(primitives);
            notes.append(&mut pnotes);
        }
        saved_node.parent = None;
        saved_node.children = Vec::new();
        entries.insert(
            nid.0,
            BodyLibraryEntry {
                version: 1,
                name,
                node: saved_node,
                children: Vec::new(),
            },
        );
    }

    // Attach children to parents (preserve kitbash_kids / graph children order).
    let mut child_lists: HashMap<u64, Vec<u64>> = HashMap::new();
    for &nid in &order {
        if let Some(Some(pid)) = parent_of.get(&nid.0).copied() {
            child_lists.entry(pid).or_default().push(nid.0);
        }
    }
    // Prefer kitbash_kids order (matches encode indices).
    for (&pid, kids) in &kitbash_kids {
        if !kids.is_empty() {
            child_lists.insert(pid, kids.iter().map(|n| n.0).collect());
        }
    }
    for &nid in order.iter().rev() {
        if let Some(kids) = child_lists.remove(&nid.0) {
            let mut child_entries = Vec::with_capacity(kids.len());
            for kid in kids {
                if let Some(ce) = entries.remove(&kid) {
                    child_entries.push(ce);
                }
            }
            if let Some(e) = entries.get_mut(&nid.0) {
                e.children = child_entries;
            }
        }
    }

    let entry = entries.remove(&root_nid.0)?;
    Some((entry, notes))
}

/// Rewrite live `child_order` to package-local indices:
/// - `Prim(uid)` → `Prim(index into primitives)`
/// - `Body(nid)` → `Body(NodeId(index into kitbash children list))`
fn encode_child_order_local(
    order: &[core_math::scene_graph::BodyChildRef],
    prims: &[core_math::primitives::SdfPrimitive],
    child_nids: &[NodeId],
) -> Vec<core_math::scene_graph::BodyChildRef> {
    use core_math::scene_graph::BodyChildRef;
    use std::collections::HashMap;
    let uid_to_i: HashMap<u32, u32> = prims
        .iter()
        .enumerate()
        .map(|(i, p)| (p.prim_uid, i as u32))
        .collect();
    let mut out = Vec::with_capacity(order.len());
    for r in order {
        match *r {
            BodyChildRef::Prim(uid) => {
                if let Some(&i) = uid_to_i.get(&uid) {
                    out.push(BodyChildRef::Prim(i));
                }
            }
            BodyChildRef::Body(nid) => {
                if let Some(i) = child_nids.iter().position(|&c| c == nid) {
                    out.push(BodyChildRef::Body(NodeId(i as u64)));
                }
            }
        }
    }
    out
}

/// Cap heavy runtime data so library package stays small and safe.
/// Returns human-readable notes for any destructive change.
fn sanitize_prims_for_library(prims: &mut [core_math::primitives::SdfPrimitive]) -> Vec<String> {
    use core_math::primitives::SdfShape;
    use core_math::sdf_shapes::BoxBevel;
    const MAX_SCULPT_CELLS: usize = 50_000;
    const MAX_MESH_RES: u32 = 64; // 64³ floats ≈ 1 MB — safe for kitbash
    const MAX_SPINE_PTS: usize = 4_096;
    const MAX_CURVE_VERTS: usize = 2_048;

    let mut notes = Vec::new();
    for p in prims.iter_mut() {
        // Sculpt layers
        let mut cells = 0usize;
        for layer in &p.sculpt_layers {
            cells = cells.saturating_add(layer.delta_grid.cells.len());
        }
        if cells > MAX_SCULPT_CELLS {
            notes.push(format!(
                "Stripped sculpt layers on '{}' ({} cells > limit)",
                p.name, cells
            ));
            eprintln!(
                "[Library] stripping {} sculpt cells from prim '{}'",
                cells, p.name
            );
            p.sculpt_layers.clear();
            p.active_sculpt_layer = None;
        }

        match &mut p.shape {
            SdfShape::MeshVolume { grid, source_poly } => {
                *source_poly = None;
                if grid.res > MAX_MESH_RES || grid.dist.len() > (MAX_MESH_RES as usize).pow(3) {
                    let he = grid.corner_abs_max().max(glam::Vec3::splat(0.05));
                    notes.push(format!(
                        "MeshVolume on '{}' (res={}) → Box approx",
                        p.name, grid.res
                    ));
                    eprintln!(
                        "[Library] MeshVolume res={} too large on '{}' → Box approx",
                        grid.res, p.name
                    );
                    p.shape = SdfShape::Box {
                        half_extents: he,
                        bevel: BoxBevel::default(),
                    };
                }
            }
            SdfShape::SpinePipe { points, .. } => {
                if points.len() > MAX_SPINE_PTS {
                    notes.push(format!(
                        "Truncated spine on '{}' ({} → {} pts)",
                        p.name,
                        points.len(),
                        MAX_SPINE_PTS
                    ));
                    eprintln!(
                        "[Library] truncating spine {} → {} pts on '{}'",
                        points.len(),
                        MAX_SPINE_PTS,
                        p.name
                    );
                    points.truncate(MAX_SPINE_PTS);
                }
            }
            SdfShape::CurveExtrude { profile, .. } => {
                if profile.verts.len() > MAX_CURVE_VERTS {
                    notes.push(format!(
                        "Truncated curve on '{}' ({} → {} verts)",
                        p.name,
                        profile.verts.len(),
                        MAX_CURVE_VERTS
                    ));
                    eprintln!(
                        "[Library] truncating curve {} → {} verts on '{}'",
                        profile.verts.len(),
                        MAX_CURVE_VERTS,
                        p.name
                    );
                    profile.verts.truncate(MAX_CURVE_VERTS);
                }
            }
            _ => {}
        }
    }
    notes
}

/// Sanitize a display name into a single path component (no dots — they break
/// `Path::with_extension` uniqueness loops on names like `Body.flb`).
fn library_safe_stem(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Collapse runs of underscores and trim.
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "Body".into()
    } else {
        // Cap length so we stay under Windows MAX_PATH with suffixes.
        if s.len() > 80 {
            s.truncate(80);
        }
        s
    }
}

/// Pick a free `dir/{stem}.flb` path. Uses exclusive create (no exists() race) and
/// never hard-fails on collisions — falls back to a timestamped name.
fn unique_library_flb_path(dir: &std::path::Path, stem: &str) -> Result<std::path::PathBuf, String> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;

    // Prefer plain name, then _1 .. _999, then timestamp.
    let mut candidates: Vec<String> = Vec::with_capacity(16);
    candidates.push(format!("{stem}.flb"));
    for n in 1u32..=999 {
        candidates.push(format!("{stem}_{n}.flb"));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    candidates.push(format!("{stem}_{stamp}.flb"));
    // Last resort: stamp + process id
    candidates.push(format!("{stem}_{stamp}_{}.flb", std::process::id()));

    for name in candidates {
        let path = dir.join(&name);
        match OpenOptions::new()
            .write(true)
            .create_new(true) // fails if exists — no false "exists" loops
            .open(&path)
        {
            Ok(f) => {
                drop(f);
                // Leave empty exclusive file; atomic_write will replace via .tmp rename.
                let _ = std::fs::remove_file(&path);
                return Ok(path);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Cannot create file in {} ({}): {e}",
                    dir.display(),
                    name
                ));
            }
        }
    }
    Err(format!(
        "Could not allocate a free filename in {}",
        dir.display()
    ))
}

/// Spawn serialize+write on a background thread. Returns a channel of
/// `Ok((flb_path, thumb_path))` or Err. Caller must poll — never join on UI thread.
///
/// `thumb_rgba`: when `Some`, write that square RGBA image as the `.thumb.png` (poly matcap
/// path). When `None`, fall back to the solid-color kitbash brick from `thumb_rgb`.
fn spawn_library_save(
    entry: BodyLibraryEntry,
    dir: std::path::PathBuf,
    thumb_rgb: [u8; 3],
    thumb_rgba: Option<image::RgbaImage>,
) -> Result<
    std::sync::mpsc::Receiver<Result<(std::path::PathBuf, std::path::PathBuf), String>>,
    String,
> {
    let safe_name = library_safe_stem(&entry.name);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("lib-save".into())
        .stack_size(32 * 1024 * 1024) // large stack for bincode of deep trees
        .spawn(move || {
            let result = crate::panic_safety::catch_recoverable(std::panic::AssertUnwindSafe(|| {
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return Err(format!(
                        "Create library dir {}: {e}",
                        dir.display()
                    ));
                }
                // Guard: dir must be a directory (not a stray file path).
                if !dir.is_dir() {
                    return Err(format!(
                        "Library path is not a folder: {}",
                        dir.display()
                    ));
                }
                let path = unique_library_flb_path(&dir, &safe_name)?;
                let thumb = {
                    // thumb next to flb: Body.flb → Body.thumb.png (stem + .thumb.png)
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| safe_name.clone());
                    dir.join(format!("{stem}.thumb.png"))
                };
                let t0 = std::time::Instant::now();
                let data = bincode::serialize(&entry)
                    .map_err(|e| format!("Serialize: {e}"))?;
                // Hard size guard — refuse multi-hundred-MB kits that can OOM on load.
                const MAX_BYTES: usize = 80 * 1024 * 1024; // 80 MB
                if data.len() > MAX_BYTES {
                    return Err(format!(
                        "Package too large ({:.1} MB). Simplify MeshVolumes/sculpt before save.",
                        data.len() as f64 / (1024.0 * 1024.0)
                    ));
                }
                crate::project::atomic_write(&path, &data)?;
                if let Some(img) = thumb_rgba {
                    if let Err(e) = img.save(&thumb) {
                        eprintln!("[Library] matcap thumb save failed: {e}");
                        write_kitbash_thumb(&thumb, thumb_rgb);
                    }
                } else {
                    write_kitbash_thumb(&thumb, thumb_rgb);
                }
                let ser_ms = t0.elapsed().as_secs_f64() * 1000.0;
                eprintln!(
                    "[Library] worker wrote {} ({} bytes, {ser_ms:.0}ms) dir={}",
                    path.display(),
                    data.len(),
                    dir.display()
                );
                Ok((path, thumb))
            }));
            match result {
                Ok(Ok(v)) => {
                    let _ = tx.send(Ok(v));
                }
                Ok(Err(e)) => {
                    let _ = tx.send(Err(e));
                }
                Err(_) => {
                    let _ = tx.send(Err(
                        "Library worker panicked (body too heavy — try simpler prims)".into(),
                    ));
                }
            }
        })
        .map_err(|e| format!("Spawn library save: {e}"))?;
    Ok(rx)
}

/// First prim albedo (or cool blue default) for a readable library tile without GPU capture.
fn kitbash_thumb_color_from_entry(entry: &BodyLibraryEntry) -> [u8; 3] {
    fn walk(e: &BodyLibraryEntry) -> Option<[u8; 3]> {
        if let NodePayload::SdfBody { primitives, .. } = &e.node.payload {
            if let Some(p) = primitives.first() {
                let a = p.material.albedo;
                return Some([
                    (a.x.clamp(0.0, 1.0) * 255.0) as u8,
                    (a.y.clamp(0.0, 1.0) * 255.0) as u8,
                    (a.z.clamp(0.0, 1.0) * 255.0) as u8,
                ]);
            }
        }
        for c in &e.children {
            if let Some(rgb) = walk(c) {
                return Some(rgb);
            }
        }
        None
    }
    walk(entry).unwrap_or([90, 140, 190])
}

/// Load the gray ModoDefaultOpenGL matcap (or synthesize a soft gray sphere) for part thumbs.
fn load_part_matcap_rgba() -> (u32, u32, Vec<u8>) {
    // Prefer bundled asset (owner reference: gray clay matcap for library parts).
    let candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        if let Some(root) = crate::content_library::find_asset_root() {
            let m = root.join("Matcaps");
            for name in [
                "ModoDefaultOpenGL8.png",
                "ModoDefaultOpenGL.png",
                "ModoSculptGL12.png",
            ] {
                v.push(m.join(name));
            }
        }
        // Dev fallback: walk from executable / cwd
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                v.push(dir.join("assets").join("Matcaps").join("ModoDefaultOpenGL8.png"));
                v.push(
                    dir.join("..")
                        .join("assets")
                        .join("Matcaps")
                        .join("ModoDefaultOpenGL8.png"),
                );
            }
        }
        v.push(std::path::PathBuf::from(
            "F:/PROJECT/FourthLAyerAi/assets/Matcaps/ModoDefaultOpenGL8.png",
        ));
        v
    };
    for p in &candidates {
        if let Ok(img) = image::open(p) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            if w >= 16 && h >= 16 {
                eprintln!("[Library] part matcap loaded {}", p.display());
                return (w, h, rgba.into_raw());
            }
        }
    }
    // Synthesize soft gray clay matcap (sphere lighting) — never crash if asset missing.
    let s = 256u32;
    let mut data = vec![0u8; (s * s * 4) as usize];
    let cx = (s as f32 - 1.0) * 0.5;
    for y in 0..s {
        for x in 0..s {
            let nx = (x as f32 - cx) / cx;
            let ny = (y as f32 - cx) / cx;
            let r2 = nx * nx + ny * ny;
            let i = ((y * s + x) * 4) as usize;
            if r2 > 1.0 {
                data[i] = 0;
                data[i + 1] = 0;
                data[i + 2] = 0;
                data[i + 3] = 0;
                continue;
            }
            let nz = (1.0 - r2).max(0.0).sqrt();
            // Soft gray clay: ambient + key light
            let n = glam::Vec3::new(nx, -ny, nz).normalize_or_zero();
            let l = glam::Vec3::new(0.35, 0.55, 0.75).normalize_or_zero();
            let diff = n.dot(l).max(0.0);
            let g = (0.22 + 0.55 * diff + 0.18 * diff.powi(4)).clamp(0.0, 1.0);
            let v = (g * 255.0) as u8;
            data[i] = v;
            data[i + 1] = v;
            data[i + 2] = v;
            data[i + 3] = 255;
        }
    }
    (s, s, data)
}

fn sample_matcap(data: &[u8], mw: u32, mh: u32, nx: f32, ny: f32) -> [u8; 3] {
    // Classic matcap: map view-space normal XY from [-1,1] → UV.
    let u = ((nx * 0.5 + 0.5).clamp(0.0, 1.0) * (mw.saturating_sub(1) as f32)) as u32;
    let v = ((-ny * 0.5 + 0.5).clamp(0.0, 1.0) * (mh.saturating_sub(1) as f32)) as u32;
    let i = ((v * mw + u) * 4) as usize;
    if i + 2 < data.len() {
        [data[i], data[i + 1], data[i + 2]]
    } else {
        [160, 160, 160]
    }
}

/// Shared camera for library part thumbs (poly + SDF).
/// ¾ isometric: yaw 45°, pitch ~28.6° — owner: same angle for both paths.
fn library_thumb_camera() -> (glam::Mat3, f32, f32) {
    use glam::{Mat3, Vec3};
    let yaw: f32 = std::f32::consts::FRAC_PI_4;
    let pitch: f32 = 0.5;
    let cy = yaw.cos();
    let sy = yaw.sin();
    let cp = pitch.cos();
    let sp = pitch.sin();
    let right = Vec3::new(cy, 0.0, -sy);
    let up = Vec3::new(sy * sp, cp, cy * sp);
    let forward = right.cross(up).normalize_or_zero();
    let rot = Mat3::from_cols(right, up, forward);
    (rot, yaw, pitch)
}

/// CPU matcap thumbnail for a poly part stored in the package entry.
fn poly_matcap_thumb_from_entry(entry: &BodyLibraryEntry) -> Option<image::RgbaImage> {
    let NodePayload::PolygonalBody { mesh, .. } = &entry.node.payload else {
        return None;
    };
    render_poly_matcap_thumb(mesh.as_ref(), 256)
}

/// Rasterize any triangle soup with the shared library matcap camera (poly + SDF).
fn render_tris_matcap_thumb(
    positions: &[glam::Vec3],
    tris: &[[u32; 3]],
    size: u32,
) -> Option<image::RgbaImage> {
    use glam::{Mat3, Vec3};
    if positions.is_empty() || tris.is_empty() {
        return None;
    }
    let mut bmin = Vec3::splat(f32::INFINITY);
    let mut bmax = Vec3::splat(f32::NEG_INFINITY);
    for p in positions {
        bmin = bmin.min(*p);
        bmax = bmax.max(*p);
    }
    if !bmin.is_finite() || !bmax.is_finite() {
        return None;
    }
    let center = (bmin + bmax) * 0.5;
    let extent = (bmax - bmin).max_element().max(1e-4);
    let half = extent * 0.5 * 1.15;

    let (rot, _, _) = library_thumb_camera();
    let (mw, mh, mdata) = load_part_matcap_rgba();
    let mut img = image::RgbaImage::new(size, size);
    for p in img.pixels_mut() {
        *p = image::Rgba([0, 0, 0, 0]);
    }
    let mut zbuf = vec![f32::INFINITY; (size * size) as usize];
    let s = size as f32;
    let project = |p: Vec3| -> (f32, f32, f32) {
        let local = rot.transpose() * (p - center);
        let sx = (local.x / half * 0.5 + 0.5) * (s - 1.0);
        let sy = (-local.y / half * 0.5 + 0.5) * (s - 1.0);
        (sx, sy, local.z)
    };

    for tri in tris {
        let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if ia >= positions.len() || ib >= positions.len() || ic >= positions.len() {
            continue;
        }
        let p0 = positions[ia];
        let p1 = positions[ib];
        let p2 = positions[ic];
        let e1 = p1 - p0;
        let e2 = p2 - p0;
        let wn = e1.cross(e2);
        let area = wn.length();
        if area < 1e-12 {
            continue;
        }
        let n_world = wn / area;
        let n_view = (rot.transpose() * n_world).normalize_or_zero();
        let (x0, y0, z0) = project(p0);
        let (x1, y1, z1) = project(p1);
        let (x2, y2, z2) = project(p2);
        let min_x = x0.min(x1).min(x2).floor().max(0.0) as i32;
        let max_x = x0.max(x1).max(x2).ceil().min(s - 1.0) as i32;
        let min_y = y0.min(y1).min(y2).floor().max(0.0) as i32;
        let max_y = y0.max(y1).max(y2).ceil().min(s - 1.0) as i32;
        let denom = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
        if denom.abs() < 1e-8 {
            continue;
        }
        let rgb = sample_matcap(&mdata, mw, mh, n_view.x, n_view.y);
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let pxf = px as f32 + 0.5;
                let pyf = py as f32 + 0.5;
                let w0 = ((y1 - y2) * (pxf - x2) + (x2 - x1) * (pyf - y2)) / denom;
                let w1 = ((y2 - y0) * (pxf - x2) + (x0 - x2) * (pyf - y2)) / denom;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * z0 + w1 * z1 + w2 * z2;
                let zi = (py as u32 * size + px as u32) as usize;
                if z >= zbuf[zi] {
                    continue;
                }
                zbuf[zi] = z;
                img.put_pixel(
                    px as u32,
                    py as u32,
                    image::Rgba([rgb[0], rgb[1], rgb[2], 255]),
                );
            }
        }
    }
    Some(img)
}

fn render_poly_matcap_thumb(
    mesh: &core_math::half_edge::HalfEdgeMesh,
    size: u32,
) -> Option<image::RgbaImage> {
    use core_math::half_edge::{FaceId, NO_EDGE};
    use glam::Vec3;

    if mesh.vertices.is_empty() || mesh.faces.is_empty() {
        return None;
    }
    let positions: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for fi in 0..mesh.faces.len() as u32 {
        if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
            continue;
        }
        let fvs = mesh.face_vertices(FaceId(fi));
        if fvs.len() < 3 {
            continue;
        }
        let i0 = fvs[0].0;
        for k in 1..fvs.len() - 1 {
            tris.push([i0, fvs[k].0, fvs[k + 1].0]);
        }
    }
    render_tris_matcap_thumb(&positions, &tris, size)
}

impl AzraApp {
    /// Library thumbnail for any saveable node — same matcap + ¾ camera as poly parts.
    /// Uses live display mesh (SDF bake / poly half-edge). No GPU capture.
    pub(crate) fn library_matcap_thumb_for_node(
        &self,
        nid: NodeId,
    ) -> Option<image::RgbaImage> {
        use glam::Vec3;
        const SIZE: u32 = 256;

        // Poly leaf — half-edge mesh on the node.
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                return render_poly_matcap_thumb(mesh.as_ref(), SIZE);
            }
        }

        // SDF body / Group / SdfObject — collect display meshes from the live graph.
        let mut positions: Vec<Vec3> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let root_world = self.scene.scene_graph.world_transform(nid);

        for (_depth, walk_nid, node) in self.scene.scene_graph.walk_depth_first(nid) {
            match &node.payload {
                NodePayload::PolygonalBody { mesh, .. } => {
                    let world = self.scene.scene_graph.world_transform(walk_nid);
                    let local = root_world.inverse() * world;
                    let base = positions.len() as u32;
                    for v in &mesh.vertices {
                        positions.push(local.transform_point3(v.position));
                    }
                    use core_math::half_edge::{FaceId, NO_EDGE};
                    for fi in 0..mesh.faces.len() as u32 {
                        if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
                            continue;
                        }
                        let fvs = mesh.face_vertices(FaceId(fi));
                        if fvs.len() < 3 {
                            continue;
                        }
                        let i0 = base + fvs[0].0;
                        for k in 1..fvs.len() - 1 {
                            tris.push([i0, base + fvs[k].0, base + fvs[k + 1].0]);
                        }
                    }
                }
                NodePayload::SdfBody { mesh_cache_id, .. } => {
                    // Nested bodies are fused into the top-level bake — only top-level
                    // (or this root if it is the body) has a standalone display mesh.
                    if walk_nid != nid && self.scene.scene_graph.parent_body(walk_nid).is_some() {
                        continue;
                    }
                    let Some(cid) = *mesh_cache_id else { continue };
                    let Some(cached) = self.cache.mesh_cache.get(cid) else {
                        continue;
                    };
                    let m = &cached.cpu_mesh;
                    if m.vertices.is_empty() || m.quads.is_empty() {
                        // Imported-style path: try raster indices if available.
                        if let Some((verts, indices)) = self.cache.mesh_cache.pt_source_data(cid) {
                            let world = self.scene.scene_graph.world_transform(walk_nid);
                            let local = root_world.inverse() * world;
                            let base = positions.len() as u32;
                            for v in &verts {
                                let p = local.transform_point3(Vec3::new(v[0], v[1], v[2]));
                                positions.push(p);
                            }
                            for chunk in indices.chunks_exact(3) {
                                tris.push([
                                    base + chunk[0],
                                    base + chunk[1],
                                    base + chunk[2],
                                ]);
                            }
                        }
                        continue;
                    }
                    let world = self.scene.scene_graph.world_transform(walk_nid);
                    let local = root_world.inverse() * world;
                    let base = positions.len() as u32;
                    for v in &m.vertices {
                        positions.push(local.transform_point3(*v));
                    }
                    for q in &m.quads {
                        tris.push([base + q[0], base + q[1], base + q[2]]);
                        tris.push([base + q[0], base + q[2], base + q[3]]);
                    }
                }
                _ => {}
            }
        }

        if positions.is_empty() || tris.is_empty() {
            eprintln!(
                "[Library] matcap thumb: no display mesh for {:?} — bake/sync first or brick fallback",
                nid
            );
            return None;
        }
        let img = render_tris_matcap_thumb(&positions, &tris, SIZE);
        if img.is_some() {
            eprintln!(
                "[Library] matcap thumb OK nid={:?} verts={} tris={}",
                nid,
                positions.len(),
                tris.len()
            );
        }
        img
    }
}

/// Soft isometric-ish block in the package's accent color (no GPU).
fn write_kitbash_thumb(path: &std::path::Path, rgb: [u8; 3]) {
    let size = 128u32;
    let mut img = image::RgbaImage::new(size, size);
    let bg = image::Rgba([28, 32, 40, 255]);
    let accent = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
    let dark = image::Rgba([
        rgb[0].saturating_mul(2) / 3,
        rgb[1].saturating_mul(2) / 3,
        rgb[2].saturating_mul(2) / 3,
        255,
    ]);
    let light = image::Rgba([
        rgb[0].saturating_add(40).min(255),
        rgb[1].saturating_add(40).min(255),
        rgb[2].saturating_add(40).min(255),
        255,
    ]);
    for p in img.pixels_mut() {
        *p = bg;
    }
    // Simple “kitbash brick” — three faces of a box (reads better than a flat square).
    for y in 36..96 {
        for x in 28..100 {
            let u = (x as f32 - 28.0) / 72.0;
            let v = (y as f32 - 36.0) / 60.0;
            // top face
            if v < 0.28 && u > 0.15 && u < 0.85 {
                img.put_pixel(x, y, light);
            }
            // left face
            else if v >= 0.28 && u < 0.5 && u > 0.1 {
                img.put_pixel(x, y, dark);
            }
            // right face
            else if v >= 0.28 && u >= 0.5 && u < 0.9 {
                img.put_pixel(x, y, accent);
            }
        }
    }
    let _ = img.save(path);
}

/// Scan library root + kitbash subfolders. Labels are `folder/name` or `name`.
fn scan_library_kitbash() -> Vec<(String, std::path::PathBuf)> {
    let root = library_path();
    let mut result = Vec::new();
    scan_library_dir_rec(&root, "", &mut result);
    // Also include flat root entries from legacy scan_library for parity.
    if result.is_empty() {
        for (name, path) in scan_library() {
            result.push((name, path));
        }
    }
    result.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    result
}

fn scan_library_dir_rec(
    dir: &std::path::Path,
    rel_prefix: &str,
    out: &mut Vec<(String, std::path::PathBuf)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let folder_name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if folder_name.starts_with('.') {
                continue;
            }
            let next_rel = if rel_prefix.is_empty() {
                folder_name
            } else {
                format!("{}/{}", rel_prefix, folder_name)
            };
            // Limit depth to keep kitbash snappy (root + 2 levels).
            let depth = next_rel.matches('/').count() + 1;
            if depth <= 3 {
                scan_library_dir_rec(&p, &next_rel, out);
            }
        } else if p.extension().map(|x| x == "flb").unwrap_or(false) {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let label = if rel_prefix.is_empty() {
                stem
            } else {
                format!("{}/{}", rel_prefix, stem)
            };
            out.push((label, p));
        }
    }
}
