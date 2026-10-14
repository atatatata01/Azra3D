// core_engine/app/src/material_import.rs
// ============================================================
// Library PBR material import (drag-drop payoff, 2026-07-17)
// ============================================================
//
// Drag an ambientCG-layout folder material (assets/PBR/<Name>/ with Color / Normal / Roughness
// [/ Metallic] images) from the content library onto an SDF body or polygon mesh — in the
// viewport or onto an outliner row — and get a fully wired **node-shader** material:
//
//   ONE transaction: create the `NodePayload::Material` node (named after the folder, Box
//   projection + 1.0 world-unit tiling = `TexMap` defaults), assign it to the target
//   (SDF body → every prim's `material_id`; poly body → every face's `HFace.material_id`),
//   AND install a `NodeGraph` with Image Texture nodes wired into Master Material for every
//   map channel — the **same** shader system as hand-authored procedural materials.
//   Ctrl+Z removes the material AND the assignment in one step.
//
//   Map decode runs on a WORKER thread (a 4K PNG decode is 100+ ms — never on the frame
//   thread); decoded maps stream back through `material_import_rx`, are forwarded (budgeted)
//   into `cache.material_map_loads`, and land on the GPU in `poll_background_tasks` via the
//   existing `MaterialTextureArray` upload + `apply_map_slot` write-back (Frame Ordering
//   Contract — this module never touches the GPU). Like every map load today, the tex_id
//   write-back itself is not a separate undo step; undoing the drop transaction removes the
//   material node, so the late-landing slots write into nothing (apply_map_slot no-ops).
//
// Viewport targeting REUSES the existing pickers (no second pick path):
//   - an SDF prim under the cursor (`tool.hovered_prim`, the per-frame CPU raycast) resolves to
//     its body via the canonical `find_body_containing_prim`;
//   - otherwise the GPU colour-ID pick (`pick_node_at`, the same one click-select uses) is
//     dispatched at the drop position and `handle_pending_pick` routes the async result here
//     through `pending_material_drop` instead of selecting.

use std::path::PathBuf;
use eframe::egui;
use core_math::pbr_material::{MapKind, MaterialId};
use core_math::scene_graph::{NodeId, NodePayload};
use crate::{AzraApp, ContentCategory, ContentItem, DirtyFlags, LibraryDragPayload};

/// Frame Budget rule: max decoded maps forwarded to the GPU upload queue per frame
/// (each is a MAP_RES² RGBA8 buffer → one 4 MB texture write in poll).
const MAX_IMPORT_MAPS_PER_FRAME: usize = 4;

/// Which channel of the source image feeds the map. `Rgb` = the whole image (normal path);
/// `G`/`B` = one channel of a packed OcclusionRoughnessMetallic map (UE export layout),
/// replicated to grayscale at decode. The R (occlusion) channel has no PbrMaterial slot today.
#[derive(Clone, Copy, PartialEq)]
enum SrcChannel {
    Rgb,
    G,
    B,
}

/// Map files of a library PBR folder item, in channel order (role tags come from
/// `content_library::detect_texture_role` at scan time). First file per channel wins.
/// A packed `orm` file splits into Roughness (G) + Metallic (B) — those channels lose to a
/// dedicated Roughness/Metallic file when the folder ships both.
fn library_item_map_files(item: &ContentItem) -> Vec<(MapKind, PathBuf, SrcChannel)> {
    let mut out: Vec<(MapKind, PathBuf, SrcChannel)> = Vec::new();
    for (kind, role) in [
        (MapKind::Albedo, "basecolor"),
        (MapKind::Normal, "normal"),
        (MapKind::Roughness, "roughness"),
        (MapKind::Metallic, "metallic"),
        (MapKind::Emission, "emission"),
        (MapKind::Emission, "emissive"),
        (MapKind::Bump, "bump"),
        (MapKind::Bump, "height"),
        (MapKind::Displacement, "displacement"),
        (MapKind::Displacement, "disp"),
    ] {
        if let Some((_, p)) = item.files.iter().find(|(r, _)| r == role) {
            out.push((kind, p.clone(), SrcChannel::Rgb));
        }
    }
    if let Some((_, p)) = item.files.iter().find(|(r, _)| r == "orm") {
        for (kind, ch) in [(MapKind::Roughness, SrcChannel::G), (MapKind::Metallic, SrcChannel::B)] {
            if !out.iter().any(|(k, _, _)| *k == kind) {
                out.push((kind, p.clone(), ch));
            }
        }
    }
    out
}

/// True when this drag payload is a library FOLDER material (the new graph-material import
/// path). Individual texture files keep the legacy embedded-prim apply.
/// A folder PBR material (ambientCG layout) — the graph-material import target.
/// Individual texture files stay on the legacy embedded apply.
fn item_is_folder_material(item: &ContentItem) -> bool {
    item.category == ContentCategory::Materials && !item.is_individual_file
}

fn is_folder_material(app: &AzraApp, payload: &LibraryDragPayload) -> bool {
    payload.category == ContentCategory::Materials
        && app.library.items.get(payload.item_idx).map_or(false, |i| !i.is_individual_file)
}

impl AzraApp {
    /// Import a library folder material onto a single body (SDF or poly). See module docs.
    pub(crate) fn import_library_material_to_node(&mut self, item_idx: usize, nid: NodeId) {
        self.import_library_material_to_nodes(item_idx, &[nid]);
    }

    /// Import ONE library folder material and assign it to every body in `nids`
    /// (multi-select outliner / multi-object drag). One material node, one map decode
    /// worker, one undo entry — all targets share the same `MaterialId`.
    pub(crate) fn import_library_material_to_nodes(&mut self, item_idx: usize, nids: &[NodeId]) {
        if nids.is_empty() {
            return;
        }
        let Some(item) = self.library.items.get(item_idx) else { return };
        if item.is_individual_file {
            return; // folder materials only (legacy path handles files)
        }
        let name = item.name.clone();
        let files = library_item_map_files(item);
        if files.is_empty() {
            self.toast(&format!("'{}': no usable map files found", name));
            return;
        }

        // Keep only assignable SDF / poly bodies (skip lights, empties, nested junk).
        let targets: Vec<(NodeId, bool)> = nids
            .iter()
            .filter_map(|&nid| {
                match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::SdfBody { .. }) => Some((nid, true)),
                    Some(NodePayload::PolygonalBody { .. }) => Some((nid, false)),
                    _ => None,
                }
            })
            .collect();
        if targets.is_empty() {
            self.toast("Drop on an SDF body or polygon mesh (other types keep embedded materials)");
            return;
        }

        // Deduplicate kinds (height/disp aliases may both appear).
        let mut kinds: Vec<MapKind> = Vec::new();
        for (k, _, _) in &files {
            if !kinds.contains(k) {
                kinds.push(*k);
            }
        }

        let n_targets = targets.len();
        let desc = if n_targets == 1 {
            "Apply Library Material".to_string()
        } else {
            format!("Apply Library Material to {n_targets} objects")
        };

        // ONE transaction: material node + assignment to ALL targets (undo removes all).
        let mut mid = MaterialId::NONE;
        self.transaction(&desc, |s| {
            let mut mat = core_math::primitives::PbrMaterial::default();
            mat.name = name.clone();
            // TexMap::default() already encodes the drop contract: Box projection (0) +
            // 1.0 world-unit tiling. tex_ids land asynchronously via apply_map_slot.
            mid = s.scene.scene_graph.add_material(&name, mat);
            // NO node graph is installed here any more — see the block after this
            // transaction. Installing `from_library_pbr_maps` made every channel
            // GraphDriven, which SHADOWS the panel's map-slot branch and left the
            // maps permanently unreachable (owner 2026-08-06: "I need access to the
            // channel parameter"). The panel doc is authored instead.
            let mut any_sdf = false;
            for &(nid, is_sdf) in &targets {
                if is_sdf {
                    if let Some(prims) = s.scene.scene_graph.body_primitives_mut(nid) {
                        for p in prims.iter_mut() {
                            p.material_id = mid;
                        }
                    }
                    any_sdf = true;
                } else {
                    // Face mode + selected faces on this body → only those faces; else whole mesh.
                    // Only the *active* poly body respects face-mode subset (multi-object
                    // apply always paints whole meshes so all selected objects get the look).
                    let face_mode = n_targets == 1
                        && matches!(
                            s.poly_work_mode,
                            crate::PolyWorkMode::Face | crate::PolyWorkMode::Group
                        )
                        && !s.poly_sel.selected_faces.is_empty()
                        && s.sel.usel.active_node() == Some(nid);
                    if face_mode {
                        if let Some(node) = s.scene.scene_graph.get_mut(nid) {
                            if let NodePayload::PolygonalBody { mesh, .. } = &mut node.payload {
                                let mesh = std::sync::Arc::make_mut(mesh);
                                for &fid in &s.poly_sel.selected_faces {
                                    if (fid as usize) < mesh.faces.len() {
                                        mesh.faces[fid as usize].material_id = mid.0;
                                    }
                                }
                            }
                        }
                    } else {
                        s.apply_material_all_faces_no_undo(nid, mid.0);
                    }
                    // Multi-material GPU ranges need a display rebuild (same as Ctrl+M assign).
                    s.poly_force_display_resync(nid);
                }
            }
            if any_sdf {
                s.sync_flat_cache(); // flat prim cache mirrors material_id
                s.cache.prim_buf_stale = true;
            }
            s.mark_dirty(DirtyFlags::RENDER);
            s.outliner.outliner_dirty = true;
        });

        // Worker decode (never on the frame thread — ambientCG maps are 1-4K PNGs/JPGs).
        // Data maps (normal/rough/metal) are pre-encoded for the sRGB array on the worker too.
        // Maps land on PbrMaterial slots; ImageTexture nodes sample those same slots on bake.
        let n_maps = files.len();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawn = std::thread::Builder::new().name("material-import".into()).spawn(move || {
            for (kind, path, ch) in files {
                let px = match ch {
                    SrcChannel::Rgb if kind.is_data() => {
                        crate::material_textures::decode_file_to_data_map(&path)
                    }
                    SrcChannel::Rgb => crate::material_textures::decode_file_to_map(&path),
                    // Packed ORM split: G = roughness, B = metallic.
                    SrcChannel::G => crate::material_textures::decode_file_to_data_map_channel(&path, 1),
                    SrcChannel::B => crate::material_textures::decode_file_to_data_map_channel(&path, 2),
                };
                // Green-channel convention (GL/DX) — filename + folder layout; worker thread
                // because the layout probe reads the directory.
                let green_dx = kind == MapKind::Normal
                    && crate::material_textures::normal_green_is_dx(&path);
                match px {
                    Some(px) => {
                        let name = crate::material_textures::map_display_name(&path);
                        let _ = tx.send((mid, kind, px, name, green_dx));
                    }
                    None => eprintln!("[MaterialImport] failed to decode {}", path.display()),
                }
            }
        });
        match spawn {
            Ok(_) => {
                self.material_import_rx.push(rx);
                // ── Author the PANEL doc, not a node graph ──────────────────────────
                //
                // A library material is nothing but one plain image per channel, and the
                // panel's Layered "Fast path" is the canonical representation of exactly
                // that (`lower_to_material` writes the same TexMap slot the renderer
                // reads). Wiring an equivalent ImageTexture graph made BOTH systems claim
                // the channel; the graph won the precedence in `derive_doc_from_material`,
                // so every parameter — strength, tiling, projection, blend, adjust — became
                // unreachable and the only exit the UI offered was the destructive
                // "Revert to panel control".
                //
                // `mp_mark_hand_loaded_map` is the EXISTING function for "a map is on its
                // way into this channel": it installs a single Image layer with tex_id 0 and
                // stores the doc WITHOUT lowering. `heal_doc_fast_texids` then fills the
                // tex_id the frame the async upload lands, so opening the panel before the
                // decode finishes self-heals with no extra plumbing (and it scales past
                // `mp_pending_layer_load`, which is one Option and could not track 4 or 5
                // concurrent decodes).
                //
                // Its GraphDriven early-return is inert here precisely BECAUSE no graph is
                // installed — the two changes are one change.
                for &k in &kinds {
                    self.mp_mark_hand_loaded_map(mid, k, 1.0);
                }
                // Single-target: select the material node so the panel shows it.
                // Multi-target: keep the multi-object selection (owner: apply to many
                // selected outliner objects must not collapse selection to Materials).
                if n_targets == 1 {
                    if let Some((mnid, _)) = self.scene.scene_graph.material_by_id(mid) {
                        self.select_single_unified(crate::SceneItemId::Node(mnid));
                    }
                }
                let toast = if n_targets == 1 {
                    format!("Applied '{name}' — loading {n_maps} map(s)…")
                } else {
                    format!("Applied '{name}' to {n_targets} objects — loading {n_maps} map(s)…")
                };
                self.toast(&toast);
            }
            Err(e) => self.toast(&format!("Material import thread failed: {e}")),
        }
    }

    /// Drain finished import decodes into the deferred GPU upload queue (budgeted — Frame
    /// Budget rule). Called from `poll_background_tasks` BEFORE the `material_map_loads`
    /// drain so a finished decode uploads the same frame.
    pub(crate) fn poll_material_imports(&mut self, ctx: &egui::Context) {
        if self.material_import_rx.is_empty() { return; }
        use std::sync::mpsc::TryRecvError;
        use crate::material_textures::{MapTarget, PendingMapLoad};
        let mut rxs = std::mem::take(&mut self.material_import_rx);
        let mut budget = MAX_IMPORT_MAPS_PER_FRAME;
        let mut keep = Vec::new();
        for rx in rxs.drain(..) {
            let mut disconnected = false;
            while budget > 0 {
                match rx.try_recv() {
                    Ok((mid, kind, pixels, name, green_dx)) => {
                        budget -= 1;
                        self.cache.material_map_loads.push(PendingMapLoad {
                            target: MapTarget::Graph(mid),
                            kind,
                            pixels,
                            // Per-kind default, applied at EVERY install site together
                            // (bake_dispatch, texture_cache, here) — which is the condition the
                            // previous revert set: a per-kind strength here without the bake
                            // path is half a fix. Only Bump differs (0.1); Displacement stays
                            // 1.0, so the reverted Displacement split is NOT reintroduced —
                            // that one still needs content_library::detect_texture_role.
                            strength: kind.default_strength(),
                            name,
                            high_precision: false,
                            pixels_lo: Vec::new(),
                            green_dx,
                        });
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => { disconnected = true; break; }
                }
            }
            // Keep the receiver while its worker lives OR buffered maps remain past the budget.
            if !disconnected { keep.push(rx); }
        }
        self.material_import_rx = keep;
        if !self.material_import_rx.is_empty() {
            ctx.request_repaint(); // keep frames flowing until every map has landed
        }
    }

    /// Viewport drop of a library folder material at the current pointer (legacy drag path).
    pub(crate) fn drop_library_material_on_viewport(&mut self, item_idx: usize, ctx: &egui::Context) {
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos().or(i.pointer.hover_pos())) else {
            return;
        };
        self.drop_library_material_at(item_idx, pos, ctx);
    }

    /// Apply a folder PBR material from a viewport drag-drop.
    ///
    /// **Multi-select:** if 2+ SDF/poly bodies are selected, apply to **all** of them
    /// (owner: drag material with many objects selected in the outliner). No pick needed.
    /// **Single / empty selection:** apply to the body under `screen_pos` (raycast / GPU pick).
    ///
    /// Important: do **not** rely on `tool.hovered_prim` here. That field is only updated when
    /// the viewport widget is hovered (`response.hovered()`). During a library drag the pointer
    /// is not owned by the viewport, so hover is always cleared and selection-only drops felt
    /// broken. Always raycast / GPU-pick at the drop screen position instead.
    pub(crate) fn drop_library_material_at(
        &mut self,
        item_idx: usize,
        screen_pos: egui::Pos2,
        ctx: &egui::Context,
    ) {
        // Multi-object selection wins: one material → every selected body.
        let sel = self.collect_library_material_import_targets();
        if sel.len() > 1 {
            self.import_library_material_to_nodes(item_idx, &sel);
            return;
        }

        let rect = self.viewport.viewport_rect;
        if rect.width() < 4.0 || rect.height() < 4.0 {
            return;
        }
        // Scene rect used by UI2 library may be smaller than full viewport_rect (library strip).
        // Still accept any pos inside viewport_rect; callers already filtered to "over scene".
        if !rect.contains(screen_pos) {
            // Soft miss: outliner row drop may own the release instead.
            // Still allow multi already handled above; single selection fallback:
            if sel.len() == 1 {
                self.import_library_material_to_nodes(item_idx, &sel);
            }
            return;
        }

        // 1) CPU raycast for SDF prim under the drop point → body.
        if let Some(idx) = self.raycast_select(screen_pos, rect, false) {
            if let Some(uid) = self.scene.primitives.get(idx).map(|p| p.prim_uid) {
                if let Some(bnid) = self.scene.scene_graph.find_body_containing_prim(uid) {
                    // Prefer full multi-select when the hit is part of a multi selection;
                    // otherwise paint the hit body (and merge with single selection if any).
                    let mut targets = sel;
                    if !targets.contains(&bnid) {
                        targets.push(bnid);
                    }
                    self.import_library_material_to_nodes(item_idx, &targets);
                    return;
                }
            }
        }

        // 2) Async colour-ID pick (poly / imported mesh / etc.) — same as click-select.
        // Result is consumed by handle_pending_pick via pending_material_drop.
        // When a single body is already selected, still queue the pick so drop-on-other
        // can retarget; multi was handled above.
        let px = (screen_pos.x - rect.min.x).max(0.0) as u32;
        let py = (screen_pos.y - rect.min.y).max(0.0) as u32;
        let (w, h) = (rect.width() as u32, rect.height() as u32);
        if w == 0 || h == 0 {
            return;
        }
        let _ = self.pick_node_at(px, py, w, h, false, false);
        if self.pending_pick.is_some() {
            self.pending_material_drop = Some(item_idx);
            ctx.request_repaint(); // pick resolves next frame
        } else if sel.len() == 1 {
            // No pick pipeline — fall back to the selected body.
            self.import_library_material_to_nodes(item_idx, &sel);
        } else {
            self.toast("Nothing under the cursor to texture");
        }
    }

    /// Prim under screen position for legacy embedded-material apply (individual textures).
    /// Does not use hovered_prim (cleared while library is dragging).
    pub(crate) fn prim_under_screen(&self, screen_pos: egui::Pos2) -> Option<usize> {
        let rect = self.viewport.viewport_rect;
        if rect.width() < 4.0 || rect.height() < 4.0 || !rect.contains(screen_pos) {
            return None;
        }
        self.raycast_select(screen_pos, rect, false)
    }

    /// Outliner row drop-target hook: while a library folder material is being dragged over
    /// `rect`, highlight it; on release, import and CONSUME the payload (so the viewport drop
    /// handler doesn't also fire). Cheap no-op when nothing is dragged.
    ///
    /// **Multi-select:** if several SDF/poly bodies are selected, dropping on any selected
    /// (or any valid) row applies to **all** selected bodies — not only the hovered row.
    pub(crate) fn material_row_drop_check(&mut self, ctx: &egui::Context, rect: egui::Rect, nid: NodeId) {
        let Some(payload) = egui::DragAndDrop::payload::<LibraryDragPayload>(ctx) else { return };
        if !is_folder_material(self, &payload) { return; }
        // Only SDF / poly rows are valid targets — don't highlight rows that would refuse.
        let valid = matches!(self.scene.scene_graph.get(nid).map(|n| &n.payload),
            Some(NodePayload::SdfBody { .. }) | Some(NodePayload::PolygonalBody { .. }));
        if !valid { return; }
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
        if !rect.contains(pos) { return; }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground, egui::Id::new("mat_drop_row_hl")));
        painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(200, 140, 220)), egui::StrokeKind::Middle);
        if ctx.input(|i| i.pointer.any_released()) {
            let item_idx = payload.item_idx;
            egui::DragAndDrop::clear_payload(ctx);
            let mut targets = self.collect_library_material_import_targets();
            if targets.len() > 1 {
                // Multi-select: paint every selected body.
                if !targets.contains(&nid) {
                    targets.push(nid);
                }
                self.import_library_material_to_nodes(item_idx, &targets);
            } else {
                self.import_library_material_to_node(item_idx, nid);
            }
        }
    }

    /// Whether the current drag payload should take the graph-material import path
    /// (used by content_library.rs to route the drop with a minimal diff).
    pub(crate) fn drag_is_folder_material(&self, payload: &LibraryDragPayload) -> bool {
        is_folder_material(self, payload)
    }

    /// Right-click library **Apply to selection** / click-apply for Materials & Textures.
    ///
    /// - **Folder PBR material** → ONE graph material + maps assigned to **every** selected
    ///   poly body and/or SDF body (outliner multi-select). Shared `MaterialId`.
    /// - **Single texture file** → legacy embedded apply on every selected SDF prim.
    pub(crate) fn apply_library_material_to_selection(&mut self, item_idx: usize) {
        let Some(item) = self.library.items.get(item_idx) else {
            self.toast("Library item missing");
            return;
        };
        if !matches!(
            item.category,
            ContentCategory::Materials | ContentCategory::Textures
        ) {
            return;
        }

        // Individual maps keep the legacy prim-embedded path (not a full PBR graph).
        if item.is_individual_file {
            let mut prims = self.selected_prim_indices_unified();
            if prims.is_empty() {
                if let Some(idx) = self.active_prim_index_unified() {
                    prims.push(idx);
                }
            }
            if prims.is_empty() {
                self.toast(
                    "Select an SDF primitive for a single texture \
                     (folder materials apply to poly meshes too)",
                );
                return;
            }
            for prim_idx in prims {
                self.apply_library_material(item_idx, prim_idx);
            }
            return;
        }

        let targets = self.collect_library_material_import_targets();
        if targets.is_empty() {
            self.toast("Select a poly mesh or SDF object first");
            return;
        }
        // ONE material for the whole multi-selection (shared maps, one undo).
        self.import_library_material_to_nodes(item_idx, &targets);
    }

    /// Unique SDF/poly body NodeIds for library material apply from the current selection.
    pub(crate) fn collect_library_material_import_targets(&self) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::new();
        let mut push = |nid: NodeId| {
            if out.contains(&nid) {
                return;
            }
            let ok = matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::SdfBody { .. } | NodePayload::PolygonalBody { .. })
            );
            if ok {
                out.push(nid);
            }
        };

        // Selected SDF prims → their owning bodies
        for idx in self.selected_prim_indices_unified() {
            if let Some(p) = self.scene.primitives.get(idx) {
                if let Some(bnid) = self.scene.scene_graph.find_body_containing_prim(p.prim_uid) {
                    push(bnid);
                }
            }
        }

        // Selected / active scene nodes (poly object mode, body selection)
        for nid in self.sel.usel.selected_nodes() {
            push(nid);
        }
        if let Some(nid) = self.sel.usel.active_node() {
            push(nid);
        }

        out
    }

    /// Click-apply target: the active node if it is an SDF/poly body, else the body
    /// containing the active prim. Same target classes as the viewport drop path.
    pub(crate) fn active_material_body_nid(&self) -> Option<NodeId> {
        if let Some(nid) = self.sel.usel.active_node() {
            if matches!(
                self.scene.scene_graph.get(nid).map(|n| &n.payload),
                Some(NodePayload::SdfBody { .. }) | Some(NodePayload::PolygonalBody { .. })
            ) {
                return Some(nid);
            }
        }
        let idx = self.active_prim_index_unified()?;
        let uid = self.scene.primitives.get(idx)?.prim_uid;
        self.scene.scene_graph.find_body_containing_prim(uid)
    }

    /// ONE click-apply route for library materials (panel click-apply AND right-click
    /// "Apply to Selection"). Always routes through multi-selection apply so outliner
    /// multi-select paints every selected body.
    pub(crate) fn apply_library_material_to_active(&mut self, item_idx: usize) {
        self.apply_library_material_to_selection(item_idx);
    }
}

#[cfg(test)]
mod import_ownership_tests {
    /// GATE — a library drop must NOT install a node graph.
    ///
    /// Deliberately a TEXT check, and that is defensible HERE in a way it is not for a
    /// behavioural fix: the property being pinned is literally "this call does not appear
    /// in this file". Installing `from_library_pbr_maps` makes every channel GraphDriven,
    /// which shadows the panel's map-slot branch and makes every channel parameter
    /// unreachable (owner report 2026-08-06). The panel doc authored by
    /// `mp_mark_hand_loaded_map` is the single owner of a library material's channels.
    #[test]
    fn library_drop_does_not_install_a_node_graph() {
        // Scan only the code ABOVE this test module — `include_str!` pulls in the test's
        // own source, and the assert message below names the function.
        let src = include_str!("material_import.rs");
        let src = src.split("#[cfg(test)]").next().unwrap_or(src);
        // Ignore the explanatory prose; only a real call matters.
        let code_hits = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .filter(|l| l.contains("from_library_pbr_maps"))
            .count();
        assert_eq!(
            code_hits, 0,
            "material_import must not build a NodeGraph for a library drop - see the doc              comment on NodeGraph::from_library_pbr_maps"
        );
        assert!(
            src.contains("mp_mark_hand_loaded_map"),
            "the library drop must author the panel doc via mp_mark_hand_loaded_map"
        );
    }
}
