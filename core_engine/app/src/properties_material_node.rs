//! Properties editor for a selected MATERIAL graph node (`NodePayload::Material`).
//!
//! Selecting a material in the outliner's Materials section shows this panel: name, the shared
//! base-PBR editor (albedo tint / roughness / metalness + the albedo MAP row — the
//! `material_textures` array + `material_map_loads` deferred upload path), emission, and LIVE
//! map slots (normal / roughness / metallic) with image thumbnails (click = replace), per-map
//! projection combos (E4) and per-map strength — plus "All maps" projection/tiling conveniences.
//!
//! Undo discipline: the widget mutates a LOCAL snapshot; on change we open an episode (slider
//! drags) or take `transaction_snapshot_only` (one-shots) BEFORE writing the snapshot back to the
//! graph — so the captured pre-state is genuinely pre-mutation. Every edit is undoable.

use crate::ui_menus::AzraMenus;
use eframe::egui;
use core_math::primitives::MaterialId;
use core_math::scene_graph::NodePayload;
use crate::*;

impl AzraApp {
    /// Draw the material-node editor if the active selection is a Material node.
    /// Returns `true` when it handled the selection (caller skips the body panel).
    pub(crate) fn draw_props_material_node(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> bool {
        let Some(nid) = self.sel.usel.active_node() else { return false };
        let Some((mid, mut name, mut mat)) = self.scene.scene_graph.get(nid).and_then(|n| {
            match &n.payload {
                NodePayload::Material { id, name, data } => Some((*id, name.clone(), data.clone())),
                _ => None,
            }
        }) else { return false };

        // Clear dangling map tex_ids that no longer have backing pixels (after array reset /
        // rejected upload). Prevents thumb/GPU sampling of ghost slots when selecting a material.
        if let Some(gpu) = self.gpu.as_ref() {
            let used = gpu.material_textures.used_layers();
            for kind in core_math::pbr_material::MapKind::ALL {
                let m = mat.map_mut(kind);
                if m.tex_id != 0 && m.tex_id >= used {
                    eprintln!(
                        "[Material] clearing dangling {} tex_id={} (used_layers={})",
                        kind.label(),
                        m.tex_id,
                        used
                    );
                    m.tex_id = 0;
                }
            }
        }

        let mut changed = false;
        let mut drag_started = false;
        let mut desc: &'static str = "Edit material";

        ui.colored_label(egui::Color32::from_rgb(200, 140, 220),
            egui::RichText::new(format!("\u{1F3A8} {}",
                if name.is_empty() { format!("Material {}", mid.0) } else { name.clone() })).strong());
        ui.separator();

        // Preview swatch + usage summary.
        let (prim_refs, face_refs) = self.scene.scene_graph.material_use_counts(mid);
        let preview_id = self.cache.mat_preview_cache.get_or_create(ctx, &mat);
        ui.horizontal(|ui| {
            ui.image(egui::load::SizedTexture::new(preview_id, egui::vec2(36.0, 36.0)));
            ui.vertical(|ui| {
                if prim_refs + face_refs > 0 {
                    ui.label(egui::RichText::new(format!(
                        "Used by {} prim(s), {} face(s)", prim_refs, face_refs)).small().weak());
                } else {
                    ui.label(egui::RichText::new("Not assigned yet").small().weak());
                }
                ui.label(egui::RichText::new(format!(
                    "R:{:.2} M:{:.2}", mat.roughness, mat.metallic)).small().weak());
            });
        });

        // Name (rename with undo — same commit-per-change pattern as the prim name field).
        ui.horizontal(|ui| {
            ui.label("Name:");
            let te = ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0));
            if te.changed() { changed = true; desc = "Rename material"; }
        });
        ui.add_space(2.0);

        // ---- Procedural shader graph (PRIMARY owner = this outliner Material) ----
        {
            let has_graph = self.material_has_shader_graph(mid);
            ui.horizontal(|ui| {
                let btn = if has_graph {
                    "Edit Shader Graph"
                } else {
                    "Add Shader Graph"
                };
                if ui
                    .button(btn)
                    .on_hover_text(
                        "Node graph lives on this Material — assign the material to prims/meshes \
                         to share the baked look.",
                    )
                    .clicked()
                {
                    self.open_material_shader_graph(mid);
                }
                if has_graph {
                    ui.label(
                        egui::RichText::new("\u{1F517} graph")
                            .small()
                            .color(egui::Color32::from_rgb(120, 200, 255)),
                    );
                    if ui
                        .small_button("X")
                        .on_hover_text("Remove procedural graph from this material")
                        .clicked()
                    {
                        self.transaction_snapshot_only("Remove material shader graph");
                        self.ui.material_node_graphs.remove(&mid.0);
                        self.clear_material_node_graph_maps(mid);
                        // Material graphs bake to Hybrid maps only — no deferred uber recompile.
                        self.mark_dirty(DirtyFlags::RENDER);
                        changed = true;
                        desc = "Remove material shader graph";
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Presets:").small());
                ui.azra_menu_button("Load Preset…", |ui| {
                    for (name, desc_p, builder) in crate::shader_nodes::MaterialPresets::list() {
                        if ui.button(format!("{name} — {desc_p}")).clicked() {
                            self.transaction_snapshot_only("Apply material shader preset");
                            let graph = builder();
                            self.ui.material_node_graphs.insert(mid.0, graph);
                            // Async bake — UI must not stall (was 1024² on the click frame).
                            // Do NOT set shader_dirty: material graphs feed Hybrid via baked maps.
                            // Setting it recompiled DF-full on the UI thread → AppHang / crash.
                            self.enqueue_material_node_graph_bake(mid);
                            self.mark_dirty(DirtyFlags::RENDER);
                            self.toast(&format!(
                                "{name} applied — baking maps in background…"
                            ));
                            ui.close_menu();
                        }
                    }
                });
            });
            ui.label(
                egui::RichText::new(
                    "Graph → Apply bakes maps onto this Material. Assign it from the Material combo on prims.",
                )
                .small()
                .weak()
                .italics(),
            );
            ui.add_space(4.0);
        }

        // Focus request from an outliner map-channel CHILD row (Modo-shader-tree children):
        // highlight that channel's slot row and scroll to it for ~2.5s, then auto-clear. Also
        // cleared when a different material's editor draws (stale target).
        let now = ctx.input(|i| i.time);
        let mut focus_kind: Option<core_math::pbr_material::MapKind> = None;
        let mut focus_fresh = false; // scroll only in the first beat, not for the whole fade
        if let Some((fnid, k, t0)) = self.outliner.focus_map_channel {
            if fnid == nid && now - t0 < 2.5 {
                focus_kind = Some(k);
                focus_fresh = now - t0 < 0.2;
                ctx.request_repaint(); // keep the fade timer ticking
            } else {
                self.outliner.focus_map_channel = None;
            }
        }
        // The albedo row lives inside the SHARED draw_pbr_material widget — highlight via a
        // banner directly above it instead of restyling the shared widget.
        if focus_kind == Some(core_math::pbr_material::MapKind::Albedo) {
            let r = ui.colored_label(egui::Color32::from_rgb(230, 180, 255),
                "\u{25B6} Diffuse Color map (below)");
            if focus_fresh { r.scroll_to_me(Some(egui::Align::Center)); }
        }

        // Base Properties = Master Material node (same material, not a second copy).
        // When a shader graph exists, these scalars are the unconnected Master sockets.
        let has_graph = self.material_has_shader_graph(mid);
        if has_graph {
            ui.label(
                egui::RichText::new(
                    "Base Properties = Master Material node (same material — edit here or on the node).",
                )
                .small()
                .weak()
                .italics(),
            );
            // If a Master socket is wired, the graph drives that channel — don't fight it.
            if let Some(g) = self.ui.material_node_graphs.get(&mid.0) {
                use crate::shader_nodes::NodeGraph;
                let driven: Vec<&str> = [
                    (NodeGraph::MO_ALBEDO, "Albedo"),
                    (NodeGraph::MO_ROUGHNESS, "Roughness"),
                    (NodeGraph::MO_METALLIC, "Metallic"),
                    (NodeGraph::MO_OPACITY, "Opacity"),
                    (NodeGraph::MO_EMISSION, "Emission"),
                    (NodeGraph::MO_AO, "AO"),
                    (NodeGraph::MO_HEIGHT, "Height"),
                    (NodeGraph::MO_NORMAL, "Normal"),
                    (NodeGraph::MO_CLEARCOAT, "Clearcoat"),
                    (NodeGraph::MO_IOR, "IOR"),
                    (NodeGraph::MO_SHEEN, "Sheen"),
                    (NodeGraph::MO_EMISSION_INTENSITY, "Em. Intensity"),
                    (NodeGraph::MO_SSS_RADIUS, "SSS"),
                    (NodeGraph::MO_DISPLACEMENT, "Displacement"),
                ]
                .into_iter()
                .filter(|(idx, _)| g.master_socket_driven(*idx))
                .map(|(_, name)| name)
                .collect();
                if !driven.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "Graph-driven: {} (wired into Master Material)",
                            driven.join(", ")
                        ))
                        .small()
                        .color(egui::Color32::from_rgb(255, 180, 80)),
                    );
                }
            }
        }

        // Shared base-PBR + albedo-map editor (the ONE widget — no duplication).
        // UV projection is selectable only when the material has polygon users (faces carry UVs;
        // SDF prims don't) — honest gating, documented in MATERIALS_UX §E4.
        let uv_allowed = face_refs > 0;
        let ai_albedo = !self.ai.ai_tex_maps_png[0].is_empty();
        let albedo_thumb = if mat.albedo_map.is_set() {
            self.map_slot_thumb(ctx, mat.albedo_map.tex_id, false)
        } else { None };
        let outcome = crate::material_editor::draw_pbr_material(ui, &mut mat, ai_albedo, albedo_thumb, uv_allowed);
        if outcome.changed { changed = true; }
        if outcome.drag_started { drag_started = true; }
        if !outcome.desc.is_empty() && (outcome.changed || outcome.drag_started) { desc = outcome.desc; }
        if outcome.clear_albedo_map { mat.albedo_map = Default::default(); }

        // Emission (PbrMaterial has it — expose it; don't extend the struct).
        {
            let mut em = crate::material_editor::linear_vec3_to_color32(mat.emission_color);
            ui.horizontal(|ui| {
                ui.label("Emission:");
                if ui.color_edit_button_srgba(&mut em).changed() {
                    mat.emission_color = crate::material_editor::color32_to_linear_vec3(em);
                    drag_started = true; changed = true; desc = "Emission color";
                }
            });
            let r = ui.add(egui::Slider::new(&mut mat.emission_intensity, 0.0..=50.0)
                .text("Emission Int").logarithmic(true));
            if r.drag_started() { drag_started = true; }
            if r.changed() { changed = true; desc = "Emission intensity"; }
        }

        // Shader mode + SSS/Glass/Clearcoat (one material; realtime vs PT evaluators).
        {
            let adv = crate::material_editor::draw_shader_mode_and_advanced(ui, &mut mat, mid.0 as u64);
            if adv.changed { changed = true; }
            if adv.drag_started { drag_started = true; }
            if !adv.desc.is_empty() && (adv.changed || adv.drag_started) {
                desc = adv.desc;
            }
        }

        // ---- Map slots: Normal / Roughness / Metalness (shared widget) ----
        // Runtime TexMaps on PbrMaterial; pixels ride the shared texture array (FL4MATTX),
        // channel params ride FL4MATX2. Loads queue through material_map_loads (Frame Ordering).
        use core_math::pbr_material::MapKind;
        let data_thumbs = {
            let mut t = [None; 7];
            for (i, kind) in MapKind::DATA_SLOTS.iter().enumerate() {
                let m = mat.map(*kind);
                if m.is_set() {
                    t[i] = self.map_slot_thumb(ctx, m.tex_id, kind.is_data());
                }
            }
            t
        };
        // Outliner focus: scroll to the map-slot section when a child row was clicked.
        if let Some(k) = focus_kind {
            if MapKind::DATA_SLOTS.contains(&k) {
                let r = ui.colored_label(
                    egui::Color32::from_rgb(230, 180, 255),
                    format!("\u{25B6} {} map (below)", k.label()),
                );
                if focus_fresh { r.scroll_to_me(Some(egui::Align::Center)); }
            }
        }
        let slots_out = crate::material_editor::draw_data_map_slots(ui, &mut mat, data_thumbs, uv_allowed);
        if slots_out.changed { changed = true; }
        if slots_out.drag_started { drag_started = true; }
        if !slots_out.desc.is_empty() && (slots_out.changed || slots_out.drag_started) {
            desc = slots_out.desc;
        }
        let load_kind = slots_out.load_data_map;

        // ---- "All maps" conveniences (projection is PER-MAP above; this writes all channels) ----
        // Tiling stays per-material for now (owner didn't ask for per-map tiling — open question,
        // noted in MATERIALS_UX §E4).
        {
            use core_math::pbr_material::MapProjection;
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("All maps:").small());
                // Set-all convenience: shows "Mixed" when the set channels disagree.
                let set_projs: Vec<u32> = MapKind::ALL.iter()
                    .filter(|k| mat.map(**k).is_set())
                    .map(|k| mat.map(*k).projection)
                    .collect();
                let uniform = set_projs.windows(2).all(|w| w[0] == w[1]);
                let text = if set_projs.is_empty() {
                    MapProjection::from_u32(mat.albedo_map.projection).label().to_string()
                } else if uniform {
                    MapProjection::from_u32(set_projs[0]).label().to_string()
                } else {
                    "Mixed".to_string()
                };
                let mut pick: Option<MapProjection> = None;
                egui::ComboBox::from_id_salt(("mat_projection_all", mid.0))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        // Driven by MapProjection::ALL, not a hand-written list — adding a
                        // variant without exposing it here is the failure mode this avoids.
                        for proj in MapProjection::ALL {
                            match proj {
                                // UV is the one projection with a hard requirement: it needs real
                                // polygon UVs, which SDF/voxel geometry does not have.
                                MapProjection::Uv => {
                                    ui.add_enabled_ui(face_refs > 0, |ui| {
                                        if ui.selectable_label(false, proj.label())
                                            .on_hover_text(
                                                "Needs polygon UVs — SDF/voxel geometry has none")
                                            .clicked()
                                        {
                                            pick = Some(proj);
                                        }
                                    });
                                }
                                _ => {
                                    let hint = match proj {
                                        MapProjection::Box =>
                                            "Blends 3 axis-aligned samples by surface normal — \
                                             the general-purpose choice, no UVs needed",
                                        MapProjection::Sphere =>
                                            "Longitude/latitude around the body origin — best on \
                                             ball-like shapes; pinches at the poles",
                                        MapProjection::Cylinder =>
                                            "Wraps around the Y axis — best on tubes and columns; \
                                             seams at the top and bottom caps",
                                        MapProjection::Planar =>
                                            "One flat projection down the body's Z axis — decal \
                                             style; stretches on faces parallel to it",
                                        MapProjection::Uv => "",
                                    };
                                    if ui.selectable_label(false, proj.label())
                                        .on_hover_text(hint)
                                        .clicked()
                                    {
                                        pick = Some(proj);
                                    }
                                }
                            }
                        }
                    });
                if let Some(sel) = pick {
                    let code = sel.as_u32();
                    // Already uniform on this projection → no empty undo.
                    let already = MapKind::ALL.iter().all(|k| {
                        let m = mat.map(*k);
                        !m.is_set() || m.projection == code
                    }) && mat.albedo_map.projection == code;
                    if !already {
                        for k in MapKind::ALL { mat.map_mut(k).projection = code; }
                        changed = true; desc = "Map projection";
                    }
                }
            });
            let mut tiling = mat.albedo_map.uv_scale;
            let r = ui.add(egui::Slider::new(&mut tiling, 0.05..=10.0)
                .text("Tiling (all maps)").logarithmic(true));
            if r.drag_started() { drag_started = true; }
            if r.changed() {
                for k in MapKind::ALL { mat.map_mut(k).uv_scale = tiling; }
                changed = true; desc = "Map tiling";
            }
        }

        // ---- Write-back with undo (snapshot BEFORE the graph mutation) ----
        if changed {
            if drag_started && !self.has_active_episode() {
                self.begin_episode(desc);
            } else if !drag_started && !self.has_active_episode() {
                self.transaction_snapshot_only(desc);
            }
            if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                if let NodePayload::Material { name: n, data, .. } = &mut node.payload {
                    *n = name;
                    *data = mat;
                }
            }
            // Properties UI is the Master Material exposed — mirror scalars into the node graph
            // so Base Color / Master Albedo stay one material (not two copies).
            self.sync_pbr_into_material_graph(mid);
            // Graph materials feed the raster material upload, the SDF prim buffer (resolved at
            // serialization) and PT — re-upload + re-render, no bake (materials never move geometry).
            self.cache.prim_buf_stale = true;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.temporal_frame_valid = false;
            }
            self.mark_dirty(DirtyFlags::RENDER);
            self.outliner.outliner_dirty = true; // name/swatch may have changed
        }

        // Deferred albedo-map intents (file picker / AI maps) → poll-phase upload targeting the
        // GRAPH material (Frame Ordering Contract — no GPU work here).
        self.handle_graph_albedo_map_outcome(&outcome, mid);

        // Deferred data-map load (Normal / Roughness / Metallic): the picker AND the decode
        // run on a worker thread (file_dialog.rs — rfd modal-in-frame crash-class fix; the
        // worker picks `decode_file_to_data_map` from kind.is_data()). Upload in poll;
        // `apply_map_slot` writes the channel's tex_id/strength next frame.
        if let Some(kind) = load_kind {
            self.request_map_file_dialog(kind, crate::material_textures::MapTarget::Graph(mid));
        }

        ui.add_space(4.0);
        true
    }
}

impl AzraApp {
    /// Graph-material variant of the albedo-map outcome handlers (see material_editor.rs — the
    /// prim + body-material variants). Queues the deferred decode/upload targeting
    /// `MapTarget::Graph(mid)`; `apply_map_slot` writes the slot back in poll.
    pub(crate) fn handle_graph_albedo_map_outcome(
        &mut self,
        outcome: &crate::material_editor::MaterialEditOutcome,
        mid: MaterialId,
    ) {
        use crate::material_textures::{MapTarget, PendingMapLoad};
        if outcome.load_albedo_map {
            // Deferred dialog (file_dialog.rs — rfd modal-in-frame crash-class fix).
            self.request_map_file_dialog(
                core_math::pbr_material::MapKind::Albedo,
                MapTarget::Graph(mid),
            );
        }
        if outcome.apply_ai_maps {
            let albedo_png = self.ai.ai_tex_maps_png[0].clone();
            if !albedo_png.is_empty() {
                if let Some(pixels) = crate::material_textures::decode_to_map(&albedo_png) {
                    self.cache.material_map_loads.push(PendingMapLoad {
                        target: MapTarget::Graph(mid),
                        kind: core_math::pbr_material::MapKind::Albedo,
                        pixels,
                        strength: 1.0,
                        name: "AI Albedo".into(),
                        high_precision: false,
                        pixels_lo: Vec::new(),
                        green_dx: false,
                    });
                }
            }
        }
    }
}
