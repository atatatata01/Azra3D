// LAB poly→SDF (iteration 2) — DESTRUCTIVE convert: a selected polygon mesh becomes a real
// `SdfShape::MeshVolume` primitive inside the active SDF body's tree, with full boolean+blend
// (it uses the prim's own `operation`, like any native shape). Log:
// docs/planning/lab/poly-mesh-sdf-operand.md
//
// Flow:
//   convert_selected_poly_to_sdf()  (UI action)
//     → gather the selected (else first visible) CLOSED PolygonalBody's world triangles
//     → spawn a background thread: region_mesh_to_sdf on a tight, mesh-centered cubic grid
//   poll_lab_mesh_sdf()  (poll phase, drains the one-shot channel)
//     → transaction: add a MeshVolume prim to the active SDF body (add_prim_to_graph does the
//       full structural checklist), then remove the source poly node (destructive) → rebake.
//
// The grid is authored in SHAPE-LOCAL space (centered on the prim), so the standard per-prim
// transform (position/rotation/scale) in the bake shader + CPU FieldEvaluator places it — no
// per-body matrix. The prim's world position = the poly's AABB centre.

use crate::{AzraApp, DirtyFlags, SceneItemId};
use eframe::egui;
use core_math::scene_graph::{NodeId, NodePayload};
use core_math::sdf_shapes::{MeshSdfData, SdfShape};
use core_math::primitives::{SdfPrimitive, BooleanOp};
use glam::Vec3;
use std::sync::Arc;

/// Max grid resolution for a converted mesh. Cap: per-prim grid is (re)serialized and uploaded
/// each body bake, so 128³ ≈ 8 MB is the safe ceiling. Prefer [`lab_res_for_tris`] over this
/// constant for Link/Refresh — fixed 128³ on a 12-tri box was ~1s of nearest-tri work
/// (owner 2026-08-06: LAB-LINK 974ms / refresh 1084ms for Poly.box).
pub(crate) const LAB_CONVERT_RES: u32 = 128;

/// Pick MeshVolume sample resolution from triangle count.
///
/// `region_mesh_to_sdf` is O(res³ × nearest work); cost is almost entirely the lattice size.
/// A primitive box (12 tris) does not need 128³ — 40³ is solid for boolean imprints and
/// ~33× fewer cells. Live **draft** (link first paint / bevel settle) is cheaper still;
/// a final pass re-samples after idle so quality is not permanently low-res.
pub(crate) fn lab_res_for_tris(tri_count: usize, draft: bool) -> u32 {
    if draft {
        return match tri_count {
            0..=80 => 24,
            81..=800 => 32,
            801..=8_000 => 48,
            _ => 64,
        };
    }
    match tri_count {
        0..=80 => 40,        // box / simple prims after bevel
        81..=800 => 56,
        801..=5_000 => 80,
        5_001..=40_000 => 112,
        _ => LAB_CONVERT_RES, // dense imports
    }
}

/// What to do with a finished conversion.
pub(crate) enum ConvertMode {
    /// Destructive: create a static prim (centered world-space grid), consume the poly.
    Destructive,
    /// Live modifier: create a LINKED prim (poly-local grid), hide the poly (kept editable).
    Link,
    /// Live modifier refresh: swap the grid on these already-linked prims (poly-local grid).
    /// `draft` = low-res interactive pass; when false, full-quality res for that mesh.
    Refresh { prim_uids: Vec<u32>, draft: bool },
}

pub(crate) struct LabConvertResult {
    /// Destructive: centered grid (bounds_min = -half). Link/Refresh: poly-LOCAL grid
    /// (bounds_min = poly-local AABB min) so the prim transform equals the poly transform.
    grid: MeshSdfData,
    /// Destructive only: world-space AABB centre → the new prim's position. Else ZERO.
    center: Vec3,
    source_name: String,
    poly_nid: NodeId,
    target_body: NodeId, // Destructive/Link: the SDF body to add the prim to
    mode: ConvertMode,
    tri_count: usize,
    ms: f32,
}

/// Debounce for live-modifier **draft** refreshes: fires this long after the last
/// poly_sync_mesh_cache signal (per-frame during drags → "on release").
const LAB_REFRESH_DEBOUNCE_MS: u64 = 280;
/// After a draft lands, schedule a full-quality re-sample this long after the last edit
/// (if still idle). Keeps interactive imprint snappy without permanent low-res.
const LAB_FINAL_REFRESH_MS: u64 = 700;

#[derive(Default)]
pub(crate) struct LabMeshSdfState {
    /// In-flight conversion (one-shot). `None` = idle. One conversion at a time.
    rx: Option<std::sync::mpsc::Receiver<Result<LabConvertResult, String>>>,
    /// Live-modifier refresh queue: poly nid → last geometry-change signal time
    /// (stamped by lab_notify_poly_mesh_changed via poly_sync_mesh_cache).
    pending_refresh: std::collections::HashMap<NodeId, std::time::Instant>,
    /// Polys that got a draft refresh and still need a final full-res sample when idle.
    pending_final: std::collections::HashMap<NodeId, std::time::Instant>,
    /// One-shot warning latch per poly (open-mesh refresh skip) so we don't toast every edit.
    warned_open: std::collections::HashSet<NodeId>,
}

impl AzraApp {
    /// True when a poly body is available to convert (drives the menu item's enabled state).
    pub(crate) fn lab_has_convertible_poly(&self) -> bool {
        self.lab_pick_poly_to_convert().is_some()
    }

    /// UI action: convert the selected (else first visible) closed poly mesh into a MeshVolume
    /// primitive in the active SDF body. Non-blocking — the conversion runs on a worker thread.
    pub(crate) fn convert_selected_poly_to_sdf(&mut self) {
        let Some(poly_nid) = self.lab_pick_poly_to_convert() else {
            self.toast("Select a closed polygon mesh first");
            return;
        };
        self.convert_poly_to_sdf(poly_nid);
    }

    /// Convert a SPECIFIC poly body (by node id) into a MeshVolume primitive in the active SDF
    /// body. Used by the outliner right-click (targets the clicked node) and by
    /// `convert_selected_poly_to_sdf`. Non-blocking — conversion runs on a worker thread.
    pub(crate) fn convert_poly_to_sdf(&mut self, poly_nid: NodeId) {
        // Owner 2026-08-03: expose the transfer settings BEFORE accepting the conversion.
        // This entry now opens the pre-convert dialog; the actual work happens in
        // convert_poly_to_sdf_now (the dialog's Convert button).
        if self.lab_mesh_sdf.rx.is_some() {
            self.toast("Convert already running…");
            return;
        }
        // First MeshVolume / convert can land in an empty scene — create the body.
        let target_body = self.ensure_sdf_body();
        let Some(node) = self.scene.scene_graph.get(poly_nid) else { return };
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { return };
        let mut tris = 0usize;
        for i in 0..mesh.faces.len() as u32 {
            let fid = core_math::half_edge::FaceId(i);
            if mesh.face(fid).half_edge == core_math::half_edge::NO_EDGE { continue; }
            tris += mesh.face_vertices(fid).len().saturating_sub(2);
        }
        let closed = !mesh
            .half_edges
            .iter()
            .any(|h| h.twin == core_math::half_edge::NO_TWIN);
        self.ui.convert_sdf_dialog = Some((poly_nid, name.clone(), tris, closed));
    }

    /// The dialog's Convert button: gather (ear-clip, sliver-culled) and spawn at `res`.
    pub(crate) fn convert_poly_to_sdf_now(&mut self, poly_nid: NodeId, res: u32) {
        if self.lab_mesh_sdf.rx.is_some() {
            self.toast("Convert already running…");
            return;
        }
        let target_body = self.ensure_sdf_body();
        let (verts, indices, name) = match self.lab_gather_poly_world_tris(poly_nid) {
            Some(v) => v,
            None => { self.toast("Poly mesh has no geometry"); return; }
        };
        self.toast(&format!("Converting '{name}' to SDF ({res}³)…"));
        self.lab_spawn_convert(verts, indices, name, poly_nid, target_body, ConvertMode::Destructive, res);
    }

    /// Pre-convert settings window (owner 2026-08-03). Drawn every frame from app_update's
    /// window list; instant no-op while `convert_sdf_dialog` is None.
    pub(crate) fn draw_convert_sdf_dialog(&mut self, ctx: &egui::Context) {
        let Some((nid, name, tris, closed)) = self.ui.convert_sdf_dialog.clone() else { return };
        let mut open = true;
        let mut do_convert = false;
        let mut cancel = false;
        egui::Window::new("Convert to SDF Volume")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.label(format!("{name} — {tris} triangles"));
                if !closed {
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 160, 60),
                        "⚠ Mesh has open edges — inside/outside can be ambiguous",
                    );
                }
                ui.separator();
                ui.add(
                    egui::Slider::new(&mut self.ui.convert_sdf_res, 16..=128)
                        .text("grid cells³"),
                );
                ui.weak("Higher = closer to the original shape (128³ ≈ 8 MB per bake).");
                ui.weak("Concave n-gons are ear-clip triangulated and zero-area slivers");
                ui.weak("removed automatically before sampling.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Convert").clicked() { do_convert = true; }
                    if ui.button("Cancel").clicked() { cancel = true; }
                });
            });
        if !open || cancel {
            self.ui.convert_sdf_dialog = None;
        }
        if do_convert {
            self.ui.convert_sdf_dialog = None;
            let res = self.ui.convert_sdf_res.clamp(16, 128);
            self.convert_poly_to_sdf_now(nid, res);
        }
    }

    /// UI action: LINK the selected (else first visible) closed poly as a LIVE SDF modifier —
    /// a MeshVolume prim in the active body that follows the poly's transform every frame and
    /// re-converts (debounced) when its geometry is edited. The poly is hidden, not consumed.
    pub(crate) fn link_selected_poly_as_sdf(&mut self) {
        let Some(poly_nid) = self.lab_pick_poly_to_convert() else {
            self.toast("Select a closed polygon mesh first");
            return;
        };
        self.link_poly_as_sdf(poly_nid);
    }

    /// Link a SPECIFIC poly body as a live SDF modifier (outliner right-click path).
    ///
    /// Adaptive final res (not draft): low-tri meshes finish in a few ms after the
    /// brute-force path; draft→final would force a second body bake (~1s cascade) for
    /// no gain. Live bevel/edit still uses draft refresh + idle final.
    pub(crate) fn link_poly_as_sdf(&mut self, poly_nid: NodeId) {
        if self.lab_mesh_sdf.rx.is_some() {
            self.toast("Convert already running…");
            return;
        }
        let target_body = self.ensure_sdf_body();
        let (verts, indices, name) = match self.lab_gather_poly_local_tris(poly_nid) {
            Some(v) => v,
            None => { self.toast("Poly mesh has no geometry (or is open)"); return; }
        };
        let tri_count = indices.len() / 3;
        let res = lab_res_for_tris(tri_count, false);
        self.toast(&format!("Linking '{name}' as SDF modifier ({res}³)…"));
        self.lab_spawn_convert(verts, indices, name, poly_nid, target_body, ConvertMode::Link, res);
    }

    /// Shared conversion-thread spawner. Link/Refresh grids are rebased to the poly's LOCAL
    /// frame on the worker (bounds_min += local centre) so the prim transform can equal the
    /// poly transform exactly.
    fn lab_spawn_convert(
        &mut self, verts: Vec<Vec3>, indices: Vec<u32>, name: String,
        poly_nid: NodeId, target_body: NodeId, mode: ConvertMode, grid_res: u32,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.lab_mesh_sdf.rx = Some(rx);
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let res = convert_tris_to_mesh_grid(&verts, &indices, grid_res)
                .map(|(mut grid, center)| {
                    let (grid_center, rebased) = match mode {
                        ConvertMode::Destructive => (center, false),
                        _ => (Vec3::ZERO, true),
                    };
                    if rebased {
                        // Poly-local grid: centered (-half) → local frame (centre - half).
                        grid.bounds_min += center;
                    }
                    LabConvertResult {
                        grid, center: grid_center, source_name: name, poly_nid, target_body,
                        mode, tri_count: indices.len() / 3,
                        ms: t0.elapsed().as_secs_f32() * 1000.0,
                    }
                });
            let _ = tx.send(res);
        });
    }

    /// Poll-phase driver: (1) drive linked prims from their polys' transforms (every frame,
    /// no-op guarded), (2) drain the conversion channel, (3) fire debounced geometry refreshes.
    pub(crate) fn poll_lab_mesh_sdf(&mut self) {
        self.lab_follow_linked_prims();

        let recv = self.lab_mesh_sdf.rx.as_ref().map(|rx| rx.try_recv());
        match recv {
            Some(Ok(Ok(result))) => {
                self.lab_mesh_sdf.rx = None;
                match &result.mode {
                    ConvertMode::Destructive => self.lab_commit_conversion(result),
                    ConvertMode::Link => self.lab_commit_link(result),
                    ConvertMode::Refresh { .. } => self.lab_commit_refresh(result),
                }
            }
            Some(Ok(Err(e))) => {
                self.lab_mesh_sdf.rx = None;
                self.toast(&format!("Convert failed: {e}"));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.lab_mesh_sdf.rx = None;
                self.toast("Convert thread died");
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }

        self.lab_schedule_pending_refreshes();
    }

    /// Every frame: linked prims copy their source poly's world transform (translation into
    /// the body-local frame; body rotation/scale not compensated — spike limitation, bodies
    /// are usually untransformed). write_prim_transform is no-op-guarded, so an unmoved poly
    /// costs nothing; a moving poly drives partial rebakes exactly like a gizmo drag. This is
    /// DERIVED state — no undo entries; undoing the POLY's move re-syncs the prim naturally.
    fn lab_follow_linked_prims(&mut self) {
        let links: Vec<(usize, NodeId)> = self.scene.primitives.iter().enumerate()
            .filter_map(|(i, p)| match &p.shape {
                SdfShape::MeshVolume { source_poly: Some(nid), .. } => Some((i, *nid)),
                _ => None,
            })
            .collect();
        for (idx, poly_nid) in links {
            if self.scene.scene_graph.get(poly_nid).is_none() { continue; } // source deleted → frozen
            let m = self.scene.scene_graph.world_transform(poly_nid);
            let (s, r, t) = m.to_scale_rotation_translation();
            let body_origin = self.scene.prim_body_map.get(idx)
                .and_then(|&(body_nid, _)| self.scene.scene_graph.get(body_nid))
                .and_then(|n| match &n.payload {
                    NodePayload::SdfBody { origin, .. } => Some(*origin),
                    _ => None,
                })
                .unwrap_or(Vec3::ZERO);
            // rotation_quat() builds YXZ from rotation_deg — invert with the same order.
            let (ry, rx, rz) = r.to_euler(glam::EulerRot::YXZ);
            let rot_deg = Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
            self.write_prim_transform(idx, t - body_origin, rot_deg, s);
        }
    }

    /// Called by poly_sync_mesh_cache after EVERY poly retessellation — stamps the debounce
    /// clock for polys that drive a linked MeshVolume prim.
    pub(crate) fn lab_notify_poly_mesh_changed(&mut self, nid: NodeId) {
        let is_linked = self.scene.primitives.iter().any(|p| matches!(
            &p.shape, SdfShape::MeshVolume { source_poly: Some(l), .. } if *l == nid));
        if is_linked {
            let now = std::time::Instant::now();
            self.lab_mesh_sdf.pending_refresh.insert(nid, now);
            // New geometry invalidates any pending final — draft will re-queue final after.
            self.lab_mesh_sdf.pending_final.remove(&nid);
        }
    }

    /// Fire a re-convert for polys whose last geometry signal is older than the debounce
    /// (one conversion in flight at a time; remaining entries wait their turn).
    /// Draft first (cheap), then a final full-res pass after `LAB_FINAL_REFRESH_MS` idle.
    fn lab_schedule_pending_refreshes(&mut self) {
        if self.lab_mesh_sdf.rx.is_some() {
            return;
        }
        // Prefer draft work (interactive), then final upgrades.
        let draft_due = self.lab_mesh_sdf.pending_refresh.iter()
            .find(|(_, t)| t.elapsed().as_millis() as u64 >= LAB_REFRESH_DEBOUNCE_MS)
            .map(|(nid, _)| *nid);
        if let Some(poly_nid) = draft_due {
            self.lab_mesh_sdf.pending_refresh.remove(&poly_nid);
            self.lab_spawn_refresh_for_poly(poly_nid, true);
            // After draft, queue a final if no more edits arrive.
            self.lab_mesh_sdf.pending_final.insert(poly_nid, std::time::Instant::now());
            return;
        }
        let final_due = self.lab_mesh_sdf.pending_final.iter()
            .find(|(nid, t)| {
                // Skip if still receiving geometry edits.
                !self.lab_mesh_sdf.pending_refresh.contains_key(nid)
                    && t.elapsed().as_millis() as u64 >= LAB_FINAL_REFRESH_MS
            })
            .map(|(nid, _)| *nid);
        if let Some(poly_nid) = final_due {
            self.lab_mesh_sdf.pending_final.remove(&poly_nid);
            self.lab_spawn_refresh_for_poly(poly_nid, false);
        }
    }

    fn lab_spawn_refresh_for_poly(&mut self, poly_nid: NodeId, draft: bool) {
        let prim_uids: Vec<u32> = self.scene.primitives.iter()
            .filter_map(|p| match &p.shape {
                SdfShape::MeshVolume { source_poly: Some(l), .. } if *l == poly_nid => Some(p.prim_uid),
                _ => None,
            })
            .collect();
        if prim_uids.is_empty() {
            return; // link removed while waiting
        }

        let Some((verts, indices, name)) = self.lab_gather_poly_local_tris(poly_nid) else {
            // Open/degenerate mesh mid-edit: keep the imprint frozen, warn once per poly.
            if self.lab_mesh_sdf.warned_open.insert(poly_nid) {
                self.toast("SDF link paused: mesh is open — imprint frozen until it is closed again");
            }
            return;
        };
        self.lab_mesh_sdf.warned_open.remove(&poly_nid);
        let tri_count = indices.len() / 3;
        let res = lab_res_for_tris(tri_count, draft);
        let target_body = self.scene.active_body_nid; // unused by Refresh
        self.lab_spawn_convert(
            verts,
            indices,
            name,
            poly_nid,
            target_body,
            ConvertMode::Refresh { prim_uids, draft },
            res,
        );
    }

    /// Refresh complete: swap the grid on every linked prim (derived state — no undo; the
    /// poly edit that caused this owns the undo, and undoing it re-fires a refresh back).
    fn lab_commit_refresh(&mut self, r: LabConvertResult) {
        let ConvertMode::Refresh { prim_uids, draft } = &r.mode else { return };
        let new_grid = Arc::new(r.grid);
        let mut touched_bodies: Vec<NodeId> = Vec::new();
        for &uid in prim_uids {
            let Some(idx) = self.prim_index_by_uid(uid) else { continue };
            let g = new_grid.clone();
            self.write_prim(idx, |p| {
                if let SdfShape::MeshVolume { grid, .. } = &mut p.shape { *grid = g; }
            });
            if let Some(&(body_nid, _)) = self.scene.prim_body_map.get(idx) {
                if !touched_bodies.contains(&body_nid) { touched_bodies.push(body_nid); }
            }
        }
        for body in touched_bodies {
            self.mark_node_body_vol_dirty(body);
        }
        self.cache.body_vol_cache.clear_bake_debounce();
        self.trigger_body_vol_bake();
        self.mark_dirty(DirtyFlags::RENDER);
        let kind = if *draft { "draft" } else { "final" };
        eprintln!(
            "[LAB-LINK] '{}' refreshed ({kind} {}³) → {} prim(s) in {:.0}ms",
            r.source_name,
            new_grid.res,
            prim_uids.len(),
            r.ms
        );
    }

    /// Link complete: create the linked prim (transform = poly transform) and hide the poly.
    fn lab_commit_link(&mut self, r: LabConvertResult) {
        let target = if self.scene.scene_graph.body_primitives(r.target_body).is_some() {
            r.target_body
        } else {
            self.scene.active_body_nid
        };
        if self.scene.scene_graph.get(r.poly_nid).is_none() {
            self.toast("Link cancelled: poly was deleted during conversion");
            return;
        }
        let body_origin = match self.scene.scene_graph.get(target).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => *origin,
            _ => Vec3::ZERO,
        };
        let m = self.scene.scene_graph.world_transform(r.poly_nid);
        let (s, rot, t) = m.to_scale_rotation_translation();
        let (ry, rx, rz) = rot.to_euler(glam::EulerRot::YXZ);

        let uid = self.scene.next_prim_id;
        self.scene.next_prim_id += 1;
        let grid_res = r.grid.res;
        let mut prim = SdfPrimitive::new(
            self.next_name("Mesh"),
            SdfShape::MeshVolume { grid: Arc::new(r.grid), source_poly: Some(r.poly_nid) },
            t - body_origin,
        );
        prim.prim_uid = uid;
        prim.rotation_deg = Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
        prim.scale = s;
        prim.operation = BooleanOp::Union { k: 0.0 };

        let name = r.source_name.clone();
        let tris = r.tri_count;
        let ms = r.ms;
        let poly_nid = r.poly_nid;
        let saved_active = self.scene.active_body_nid;
        self.transaction("Link poly as SDF modifier", |st| {
            st.scene.active_body_nid = target;
            st.add_prim_to_graph(prim);
            st.scene.active_body_nid = saved_active;
            // Auto-hide the source poly (owner decision): only its SDF imprint shows. The poly
            // stays in the outliner — unhide (eye) to edit it; edits refresh the imprint.
            if let Some(node) = st.scene.scene_graph.get_mut(poly_nid) {
                node.visible = false;
            }
            st.mark_dirty(DirtyFlags::RENDER);
            st.mark_outliner_dirty();
        });
        eprintln!("[LAB-LINK] '{name}' ({tris} tris, {grid_res}³) linked as SDF modifier in {ms:.0}ms");
        self.toast(&format!("'{name}' linked as SDF modifier ({tris} tris, {grid_res}³, {ms:.0}ms)"));
    }

    /// Properties-panel action: break the live link — the prim keeps its last grid (static).
    pub(crate) fn lab_make_prim_static(&mut self, idx: usize) {
        if idx >= self.scene.primitives.len() { return; }
        // Already static (no live poly source) → no empty undo.
        let linked = matches!(
            &self.scene.primitives[idx].shape,
            SdfShape::MeshVolume { source_poly: Some(_), .. }
        );
        if !linked { return; }
        self.transaction("Unlink SDF modifier", |s| {
            s.write_prim(idx, |p| {
                if let SdfShape::MeshVolume { source_poly, .. } = &mut p.shape {
                    *source_poly = None;
                }
            });
            s.mark_dirty(DirtyFlags::RENDER);
        });
        self.toast("SDF modifier unlinked (imprint is now static)");
    }

    /// Properties-panel action: force a re-convert of a linked prim now.
    pub(crate) fn lab_request_refresh_for_prim(&mut self, idx: usize) {
        let Some(p) = self.scene.primitives.get(idx) else { return };
        if let SdfShape::MeshVolume { source_poly: Some(nid), .. } = &p.shape {
            let nid = *nid;
            // Backdate the stamp so the debounce fires on the next poll.
            let past = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(LAB_REFRESH_DEBOUNCE_MS + 1))
                .unwrap_or_else(std::time::Instant::now);
            self.lab_mesh_sdf.pending_refresh.insert(nid, past);
        }
    }

    /// POLY-LOCAL triangles of a poly body with its modifier stack applied (the DISPLAY mesh,
    /// same as raster). Returns None for empty or OPEN meshes (winding sign needs closed).
    fn lab_gather_poly_local_tris(&self, nid: NodeId) -> Option<(Vec<Vec3>, Vec<u32>, String)> {
        let node = self.scene.scene_graph.get(nid)?;
        let NodePayload::PolygonalBody { name, mesh, modifiers, .. } = &node.payload else { return None };
        let display = Self::apply_poly_modifiers(
            mesh,
            modifiers,
            &std::collections::HashMap::new(),
        );
        if display.half_edges.iter().any(|h| h.twin == core_math::half_edge::NO_TWIN) {
            return None; // open mesh
        }
        let (raster, indices) = display.to_raster_data();
        if raster.is_empty() || indices.is_empty() { return None; }
        let verts = raster.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
        Some((verts, indices, name.clone()))
    }

    /// Create the MeshVolume prim in the target body and consume the poly — one undo entry.
    fn lab_commit_conversion(&mut self, r: LabConvertResult) {
        // Target body may have been deleted while converting — fall back to active.
        let target = if self.scene.scene_graph.body_primitives(r.target_body).is_some() {
            r.target_body
        } else {
            self.scene.active_body_nid
        };
        // Prim world position = mesh centre, expressed in the target body's local frame
        // (translation only — body rotation/scale is rare and out of spike scope).
        let body_origin = match self.scene.scene_graph.get(target).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { origin, .. }) => *origin,
            _ => Vec3::ZERO,
        };
        let local_pos = r.center - body_origin;

        let uid = self.scene.next_prim_id;
        self.scene.next_prim_id += 1;
        let grid_res = r.grid.res;
        let mut prim = SdfPrimitive::new(
            self.next_name("Mesh"),
            SdfShape::MeshVolume { grid: Arc::new(r.grid), source_poly: None },
            local_pos,
        );
        prim.prim_uid = uid;
        prim.operation = BooleanOp::Union { k: 0.0 }; // default — user can switch to subtract/blend

        let name = r.source_name.clone();
        let tris = r.tri_count;
        let ms = r.ms;
        let poly_nid = r.poly_nid;
        let saved_active = self.scene.active_body_nid;
        self.transaction("Convert poly to SDF primitive", |s| {
            s.scene.active_body_nid = target;      // add_prim_to_graph targets the active body
            s.add_prim_to_graph(prim);
            s.scene.active_body_nid = saved_active;
            s.lab_remove_poly_node(poly_nid);
            s.rebake_no_undo();
            s.mark_dirty(DirtyFlags::RENDER);
        });
        eprintln!("[LAB-CONVERT] '{name}' ({tris} tris) → MeshVolume {grid_res}³ prim in {ms:.0}ms");
        self.toast(&format!("'{name}' → SDF primitive ({tris} tris, {grid_res}³, {ms:.0}ms)"));
    }

    /// Remove a PolygonalBody node (mesh-cache cleanup + graph removal + deselect). Called inside
    /// the convert transaction so it is part of the same undo entry.
    fn lab_remove_poly_node(&mut self, nid: NodeId) {
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let NodePayload::PolygonalBody { mesh_cache_id, .. } = &node.payload {
                if let Some(cid) = *mesh_cache_id { self.cache.mesh_cache.remove(cid); }
            }
        }
        self.scene.scene_graph.remove(nid);
        self.deselect_items(&[SceneItemId::Node(nid)]);
        self.sync_flat_cache();
        self.sanitize_selection();
        self.mark_outliner_dirty();
    }

    /// Pick the poly body to convert: the first SELECTED closed PolygonalBody, else the first
    /// visible closed one. Open meshes (boundary edges) are skipped — SignMethod::Normal needs a
    /// closed mesh for a reliable inside/outside sign.
    fn lab_pick_poly_to_convert(&self) -> Option<NodeId> {
        let selected: std::collections::HashSet<NodeId> =
            self.sel.usel.selected_nodes().collect();
        let is_closed = |mesh: &core_math::half_edge::HalfEdgeMesh| {
            !mesh.half_edges.iter().any(|h| h.twin == core_math::half_edge::NO_TWIN)
        };
        // Prefer a selected closed poly.
        for (nid, node) in self.scene.scene_graph.poly_bodies() {
            if !node.visible || !selected.contains(&nid) { continue; }
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                if is_closed(mesh) { return Some(nid); }
            }
        }
        // Else the first visible closed poly.
        for (nid, node) in self.scene.scene_graph.poly_bodies() {
            if !node.visible { continue; }
            if let NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                if is_closed(mesh) { return Some(nid); }
            }
        }
        None
    }

    /// World-space triangles of a poly body (positions transformed by its world matrix).
    fn lab_gather_poly_world_tris(&self, nid: NodeId) -> Option<(Vec<Vec3>, Vec<u32>, String)> {
        let node = self.scene.scene_graph.get(nid)?;
        let NodePayload::PolygonalBody { name, mesh, .. } = &node.payload else { return None };
        let (raster, indices) = mesh.to_raster_data();
        if raster.is_empty() || indices.is_empty() { return None; }
        let m = self.scene.scene_graph.world_transform(nid);
        let verts = raster.iter()
            .map(|v| m.transform_point3(Vec3::new(v[0], v[1], v[2])))
            .collect();
        Some((verts, indices, name.clone()))
    }
}

/// Convert world-space triangles → a shape-local, centered MeshSdfData grid + its world centre.
/// Fits a tight cubic grid around the mesh AABB (10% margin so boundary samples stay positive),
/// samples via region_mesh_to_sdf (SignMethod::Normal), and centers the grid on the prim origin.
fn convert_tris_to_mesh_grid(
    verts: &[Vec3], indices: &[u32], res: u32,
) -> Result<(MeshSdfData, Vec3), String> {
    if verts.is_empty() || indices.len() < 3 {
        return Err("mesh has no triangles".into());
    }
    let mut mn = Vec3::splat(f32::INFINITY);
    let mut mx = Vec3::splat(f32::NEG_INFINITY);
    for v in verts { mn = mn.min(*v); mx = mx.max(*v); }
    if !mn.is_finite() || !mx.is_finite() {
        return Err("mesh has non-finite vertices".into());
    }
    let center = (mn + mx) * 0.5;
    let half = ((mx - mn) * 0.5).max_element().max(1e-3) * 1.1; // tight cubic fit + margin

    // region_mesh_to_sdf samples the lattice p_i = -half + i*cell in its LOCAL frame — shift the
    // mesh so that frame is centered on the AABB centre (→ grid is shape-local, centered on prim).
    let local: Vec<Vec3> = verts.iter().map(|v| *v - center).collect();
    let dist = io_bridge::mesh_to_sdf::region_mesh_to_sdf(
        &local, indices, half, res, [0, 0, 0, res - 1, res - 1, res - 1],
    ).ok_or("region_mesh_to_sdf returned None (degenerate mesh)")?;

    let cell = half * 2.0 / (res as f32 - 1.0);
    let grid = MeshSdfData {
        res,
        bounds_min: Vec3::splat(-half),
        cell,
        dist: Arc::new(dist),
    };
    Ok((grid, center))
}
