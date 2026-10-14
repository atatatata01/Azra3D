//! P1 — READ-ONLY material panel page (MATERIAL_PANEL_SPEC.md §2 / §4-P1).
//!
//! Replaces the ui2 Properties "Material" page stub (properties_panel.rs). This is
//! the channel-major panel: pinned header, shader-model label (create-time type,
//! not a tier), Texture space group, channel list with per-material collapse +
//! `map`/`graph` badges + per-channel 8/16-bit toggle,
//! connection dots, and the "show only driven" filter. NOTHING here mutates the
//! material, the doc store, or the graph (P1 rule) — the ONLY app-state writes are
//! navigation (`open_material_shader_graph`, spec-allowed) and toasts.
//!
//! Choices (reported per task):
//! - **Doc source:** a stored `ui.material_panel_docs` doc wins (authored intent —
//!   the model IS the truth once one exists, e.g. loaded from FL4MPNLS); otherwise
//!   the doc is DERIVED FRESH EVERY FRAME into a local via
//!   `derive_doc_from_material` (18 fixed channels, no allocation beyond the Vec —
//!   trivially cheap) and **never inserted into the store** (read-only panel: no
//!   app-state mutation, no FL4MPNLS pollution, and edits made through the v1
//!   material editor are visible immediately with zero invalidation machinery).
//! - **Collapse state:** egui temp memory, one `u32` bitmask of EXPANDED channel
//!   indices keyed `("mp_expand", material_id)` — per-material, survives selection
//!   changes for the session (spec P1 acceptance), default 0 = all collapsed.
//! - **Filter toggle:** egui temp memory bool `"mp_filter_driven"`, session-only.

use crate::ui_menus::AzraMenus;
use eframe::egui;
use egui_phosphor::regular as ph;
use core_math::pbr_material::{MapProjection, PbrMaterial};
use core_math::primitives::MaterialId;
use core_math::scene_graph::NodePayload;
use crate::ui_fields as f;
use crate::ui_theme as t;
use crate::AzraApp;

use super::layer_ui::{self, ChannelRowOut, GenCtx};
use super::lower::cost_summary;
use super::model::*;

pub(super) const ROW_H: f32 = 26.0; // collapsed channel row (spec: ~26-30px, 18 rows ≤ 900px budget)
pub(super) const LAYER_ROW_H: f32 = 22.0; // depth-2 layer summary (read-only stubs)

/// Display name per channel — ONE term per concept (spec §2 rules).
pub(super) fn kind_label(k: ChannelKind) -> &'static str {
    match k {
        ChannelKind::BaseColor => "Base color",
        ChannelKind::Roughness => "Roughness",
        ChannelKind::Metallic => "Metalness",
        ChannelKind::Opacity => "Opacity",
        ChannelKind::Emission => "Emission",
        ChannelKind::Normal => "Normal",
        ChannelKind::Bump => "Bump",
        ChannelKind::Displacement => "Displacement",
        ChannelKind::Clearcoat => "Clearcoat",
        ChannelKind::ClearcoatRoughness => "Clearcoat roughness",
        ChannelKind::Sheen => "Sheen",
        ChannelKind::SheenTint => "Sheen tint",
        ChannelKind::Anisotropy => "Anisotropy",
        ChannelKind::AnisotropyRotation => "Anisotropy rotation",
        ChannelKind::Ior => "IOR",
        ChannelKind::SssRadius => "SSS radius",
        ChannelKind::SssColor => "SSS color",
        ChannelKind::GlassTint => "Glass tint",
    }
}

pub(super) fn generator_label(g: GeneratorSource) -> &'static str {
    match g {
        GeneratorSource::Curvature => "Curvature",
        GeneratorSource::AmbientOcclusion => "Ambient occlusion",
        GeneratorSource::Thickness => "Thickness",
        GeneratorSource::Position => "Position",
        GeneratorSource::NormalDirection => "Normal direction",
        GeneratorSource::Cavity => "Cavity",
        GeneratorSource::Direction => "Direction",
    }
}

// GenCtx: see `layer_ui::GenCtx` (generator availability for the active bake body).

/// Human name for a material SHADER MODEL (not a quality tier).
/// Chosen when the material is created (Octane-style Diffuse / Specular / Glass…).
pub(super) fn shader_model_name(code: u32) -> &'static str {
    match code {
        1 => "Subsurface",
        2 => "Glass",
        3 => "VFX",
        4 => "Advanced",
        _ => "Standard PBR",
    }
}

/// Compact float for captions: `2` not `2.00`, `2.5` stays `2.5`.
pub(super) fn trim_f(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

pub(super) fn lin_to_c32(c: [f32; 3]) -> egui::Color32 {
    // Same gamma-ish display convention as material_editor's swatches: cheap srgb-ish.
    let g = |v: f32| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
    egui::Color32::from_rgb(g(c[0]), g(c[1]), g(c[2]))
}

/// Channel identity → stable bit index for the per-material expand bitmask.
pub(super) fn kind_bit(k: ChannelKind) -> u32 {
    // ChannelKind is append-only, so discriminant order is stable.
    k as u32
}

mod channel_menu;
mod channel_row;
mod dialogs;

pub(super) use channel_menu::*;
pub(super) use channel_row::*;

impl AzraApp {
    /// Resolve the material the panel should show (task order):
    /// 1. selected outliner Material node; 2. active prim's `material_id`;
    /// 3. active body's prevailing / face material. `None` or `MaterialId::NONE`
    /// → the quiet no-material hint.
    fn props_material_resolve(&self) -> Option<MaterialId> {
        // 1 — selected Material node.
        if let Some(nid) = self.sel.usel.active_node() {
            if let Some(NodePayload::Material { id, .. }) =
                self.scene.scene_graph.get(nid).map(|n| &n.payload)
            {
                return Some(*id);
            }
        }
        // 2 — active prim's material_id (graph-referenced).
        if let Some(idx) = self.active_prim_index_unified() {
            if let Some(p) = self.scene.primitives.get(idx) {
                return Some(p.material_id);
            }
        }
        // 3 — active body's material.
        if let Some(nid) = self.sel.usel.active_node() {
            match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(NodePayload::SdfBody { .. }) => {
                    return Some(self.scene.scene_graph.body_prevailing_material(nid));
                }
                Some(NodePayload::PolygonalBody { mesh, .. }) => {
                    let raw = mesh.faces.first().map(|f| f.material_id).unwrap_or(0);
                    return Some(MaterialId(raw));
                }
                _ => {}
            }
        }
        None
    }

    /// Body-scoped material resolve (GPU_CURVATURE_KERNEL_PLAN.md P2): the material driving a
    /// SPECIFIC SdfBody, not "whatever the panel/outliner selection currently is". Reuses the
    /// same graph-node → prim → body-prevailing precedence as `props_material_resolve`, but
    /// anchored on `body` instead of the active selection, so the vertex-curvature poll can ask
    /// "what material does THIS body use" independent of what the user has selected/opened in
    /// the panel this frame.
    pub(crate) fn props_material_resolve_for_body(
        &self,
        body: core_math::scene_graph::NodeId,
    ) -> Option<MaterialId> {
        match self.scene.scene_graph.get(body).map(|n| &n.payload) {
            Some(NodePayload::SdfBody { .. }) => {
                let mid = self.scene.scene_graph.body_prevailing_material(body);
                if mid == MaterialId::NONE { None } else { Some(mid) }
            }
            _ => None,
        }
    }

    /// The P1 read-only Material page. Draws its OWN scroll area so the header
    /// stays pinned — the caller must NOT wrap this in a ScrollArea.
    pub(crate) fn props_material_page_v2(&mut self, ui: &mut egui::Ui) {
        let mid = self.props_material_resolve().unwrap_or(MaterialId::NONE);
        let resolved = self
            .scene
            .scene_graph
            .material_by_id(mid)
            .map(|(nid, m)| (nid, m.clone()));
        let Some((mat_nid, mat)) = resolved else {
            // NEW-1: `mid` is NONE (or dangling), so there is no Material NODE —
            // but that does NOT mean nothing is rendering. `resolve_prim_material`
            // falls back to the object's EMBEDDED material, and a fresh scene
            // ships zero material nodes, so this is the DEFAULT state of every new
            // scene: the look you see is the one the panel used to refuse to edit.
            // Offer the promotion instead of a dead end.
            self.mp_draw_no_material_state(ui);
            return;
        };
        let name = match self.scene.scene_graph.get(mat_nid).map(|n| &n.payload) {
            Some(NodePayload::Material { name, .. }) if !name.is_empty() => name.clone(),
            _ => format!("Material {}", mid.0),
        };
        // Doc: stored authored doc wins (refreshed from the live material so
        // undo/redo track); else derive fresh this frame. Widgets edit this
        // LOCAL copy; any change commits it through mp_commit_doc_edit (P2).
        //
        // Auto-migrate no-UV bodies onto the Vertex curvature path and drop any
        // stale planar gift-wrap maps (owner rule) before the panel draws.
        let _ = self.mp_ensure_vertex_curvature_path(mid);
        let mat = self
            .scene
            .scene_graph
            .material_by_id(mid)
            .map(|(_, m)| m.clone())
            .unwrap_or(mat);
        let mut doc = self.mp_current_doc(mid, &mat);
        let conflicts = self.ui.mp_bake_conflicts.get(&mid.0).copied().unwrap_or(0);
        // Edit signals collected across the frame → ONE commit after the UI.
        let mut edit_changed = false;
        let mut edit_drag = false;
        let mut edit_desc: &'static str = "Material edit";
        // ADJUST_SLIDER_LAG F2: which channel was scrubbed (None = full multi-channel bake).
        let mut edit_channel: Option<ChannelKind> = None;
        let mut add_kind: Option<ChannelKind> = None;
        let mut remove_kind: Option<(ChannelKind, bool)> = None; // (kind, needs_confirm)
        let mut duplicate_req: Option<(ChannelKind, ChannelKind)> = None; // (from, to)
        let mut retype_req: Option<(ChannelKind, ChannelKind)> = None;    // (from, to)
        let mut copy_layer_req: Option<(ChannelKind, usize, ChannelKind)> = None; // (from, li, to)
        let mut revert_kind: Option<ChannelKind> = None;
        let mut bit_depth_toggle_kind: Option<ChannelKind> = None;
        // P0-4: conflict-badge resolutions chosen this frame.
        let mut conflict_yield: Option<ChannelKind> = None;
        let mut conflict_keep: Option<ChannelKind> = None;

        // ── P5 preset state (precomputed: mp_user_presets needs &mut self) ──
        let mp_builtins = super::preset::builtin_presets();
        let mp_user: Vec<super::preset::MaterialPreset> = self.mp_user_presets().to_vec();
        let cur_preset: String = super::preset::matching_preset_name(&doc, &mp_builtins, &mp_user)
            .unwrap_or("—")
            .to_string();
        let mut apply_preset: Option<super::preset::MaterialPreset> = None;
        let mut preset_save_as = false;
        let mut preset_delete: Option<usize> = None;

        // ── 1 · Pinned header (outside the scroll) ──────────────────────────
        let mut open_tree = false;
        // P8: header/label reflects whether the shader tree is ALREADY open on
        // THIS material (single-window: "Open" when closed, "Show" when already up).
        let tree_open_here = self.ui.show_node_editor
            && matches!(
                self.ui.node_graph_target,
                Some(crate::NodeGraphTarget::Material(m)) if m == mid
            );
        {
            let ctx = ui.ctx().clone();
            let preview = self.cache.mat_preview_cache.get_or_create(&ctx, &mat);
            let driven = doc
                .channels
                .iter()
                .filter(|c| !matches!(c.state, ChannelState::Value(_)))
                .count();
            egui::Frame::none()
                .fill(t::SURFACE_HEADER)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.image(egui::load::SizedTexture::new(preview, egui::vec2(30.0, 30.0)));
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(
                                egui::RichText::new(&name).size(12.5).color(t::TEXT).strong(),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "Material · {driven} channel{} driven  ·  {}",
                                    if driven == 1 { "" } else { "s" },
                                    cost_summary(&mat)
                                ))
                                .size(9.5)
                                .color(t::TEXT_FAINT),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let r = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(ph::TREE_STRUCTURE).size(14.0),
                                    )
                                    .min_size(egui::vec2(t::IBTN_SIZE, t::IBTN_SIZE)),
                                )
                                .on_hover_text(if tree_open_here {
                                    "Show in shader tree"
                                } else {
                                    "Open shader tree"
                                });
                            if r.clicked() {
                                open_tree = true;
                            }
                            // P5 preset combo (data presets — spec §9 / owner D1).
                            ui.azra_menu_button(
                                egui::RichText::new(format!("{cur_preset} {}", ph::CARET_DOWN))
                                    .size(10.5)
                                    .color(t::TEXT_SECONDARY),
                                |ui| {
                                    ui.set_min_width(160.0);
                                    for p in &mp_builtins {
                                        if ui.button(&p.name).clicked() {
                                            apply_preset = Some(p.clone());
                                            ui.close_menu();
                                        }
                                    }
                                    if !mp_user.is_empty() {
                                        ui.separator();
                                        for (i, p) in mp_user.iter().enumerate() {
                                            ui.horizontal(|ui| {
                                                if ui.button(&p.name).clicked() {
                                                    apply_preset = Some(p.clone());
                                                    ui.close_menu();
                                                }
                                                if ui
                                                    .small_button(
                                                        egui::RichText::new(ph::TRASH).size(11.0),
                                                    )
                                                    .on_hover_text("Delete preset")
                                                    .clicked()
                                                {
                                                    preset_delete = Some(i);
                                                    ui.close_menu();
                                                }
                                            });
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("Save as preset…").clicked() {
                                        preset_save_as = true;
                                        ui.close_menu();
                                    }
                                },
                            )
                            .response
                            .on_hover_text("Material preset (applies channel set + values)");
                        });
                    });
                });
            let hr = ui.max_rect();
            ui.painter().hline(
                hr.x_range(),
                ui.cursor().top(),
                egui::Stroke::new(1.0, t::SEAM),
            );
        }
        if open_tree {
            // P8: header opens the tree FRAMED on the material's OUTPUT (Master)
            // node — never an unframed window. Routes through the ONE entry point.
            self.mp_frame_shader_node(mid, None);
        }

        // ── P3: layer thumbnails (needs &mut self → precomputed here) ───────
        let mut thumbs: std::collections::HashMap<u32, egui::TextureId> =
            std::collections::HashMap::new();
        {
            let ctx = ui.ctx().clone();
            for ch in &doc.channels {
                let is_data = ch.kind.map_kind().map(|mk| mk.is_data()).unwrap_or(true);
                for l in &ch.layers {
                    if let LayerContent::Image { tex_id, .. } = l.content {
                        if tex_id != 0 && !thumbs.contains_key(&tex_id) {
                            if let Some(tid) = self.map_slot_thumb(&ctx, tex_id, is_data) {
                                thumbs.insert(tex_id, tid);
                            }
                        }
                    }
                }
            }
        }
        // Deferred: the panel must not mutate the doc store mid-draw (it is borrowed for reading).
        let mut gen_mode_set: Option<u32> = None;
        let mut bake_maps = false;
        let mut load_layer: Option<(ChannelKind, usize, bool)> = None; // (kind, layer, is_mask)
        // Owner #7: (channel kind, dropped library item_idx) — resolved after the channel loop.
        let mut dropped_map: Option<(ChannelKind, usize)> = None;
        let mut dropped_os: Option<(ChannelKind, std::path::PathBuf)> = None;

        // ── P6: generator availability + C4 radius floor. Derived from the
        // SAME source the bake snapshot uses (`snapshot_sdf_field_for_material`
        // reads the ACTIVE body's prims), so the gate/caption and the bake can
        // never disagree. No SDF prims → Curvature/Cavity sources disabled.
        let gen_ctx = {
            let nid = self.scene.active_body_nid;
            // GPU_CURVATURE_KERNEL_PLAN.md P3: the HONEST UV-presence check — the active body's
            // authored per-corner UV layer, NOT "any poly body exists in the scene" (the
            // confirmed-bad heuristic at this file's texture-space projection gate). Empty/absent
            // sidecar = no authored UVs = byte-identical to every mesh's default state.
            let has_uvs = self
                .cache
                .poly_custom_data
                .get(&nid)
                .map_or(false, |cd| !cd.uv.is_empty());
            let prims = self.scene.scene_graph.body_primitives(nid);
            match prims {
                Some(pr) if !pr.is_empty() => {
                    let mut src = crate::node_graph_bake::SdfFieldSources::default();
                    src.body_prims = pr.to_vec();
                    let (bmin, bmax) = src.body_aabb(0);
                    GenCtx {
                        has_sdf: true,
                        has_poly_mesh: false,
                        radius_floor: Some(super::lower::curvature_radius_floor_aabb(bmin, bmax)),
                        auto_radius: Some(super::lower::curvature_auto_radius_aabb(bmin, bmax)),
                        has_uvs,
                    }
                }
                _ => {
                    // POLY CURVATURE: a polygonal body has no prim array and therefore no field,
                    // but its half-edge mesh IS measurable.
                    //
                    // Resolved through `poly_curv_body()` — the SAME resolver the bake snapshot
                    // uses, and NOT `active_body_nid` (see that function on why the edit-scope
                    // pointer walks off a poly body). If these two ever disagree the menu enables
                    // for one body while the bake measures another.
                    let poly_nid = self.poly_curv_body();
                    let has_poly_mesh = poly_nid.is_some();
                    // UV presence must be read from the SAME body, for the same reason.
                    let has_uvs = poly_nid.map_or(has_uvs, |p| {
                        self.cache
                            .poly_custom_data
                            .get(&p)
                            .map_or(false, |cd| !cd.uv.is_empty())
                    });
                    // Radius bounds come from the DISPLAY mesh when it is already cached (a
                    // read-only lookup — building it here would be a mutation during a panel
                    // draw, which the Frame Ordering Contract forbids). Absent, the Auto button
                    // simply hides rather than offering a number derived from the base mesh,
                    // which would ignore the modifier stack.
                    let bounds = poly_nid
                        .and_then(|p| self.poly_display_cache.get(&p))
                        .map(|e| e.mesh.bounding_box());
                    GenCtx {
                        has_sdf: false,
                        has_poly_mesh,
                        radius_floor: bounds
                            .map(|(lo, hi)| super::lower::curvature_radius_floor_aabb(lo, hi)),
                        auto_radius: bounds
                            .map(|(lo, hi)| super::lower::curvature_auto_radius_aabb(lo, hi)),
                        has_uvs,
                    }
                }
            }
        };

        // ── Session view state (egui temp) ──────────────────────────────────
        let expand_id = egui::Id::new(("mp_expand", mid.0));
        let mut expanded: u32 = ui.data(|d| d.get_temp(expand_id)).unwrap_or(0);
        let filter_id = egui::Id::new("mp_filter_driven");
        let mut only_driven: bool = ui.data(|d| d.get_temp(filter_id)).unwrap_or(false);
        // P8: which channel (if any) to frame in the shader tree this frame.
        let mut frame_channel: Option<ChannelKind> = None;

        // ── Scrolling body ──────────────────────────────────────────────────
        let body_h = ui.available_height();
        egui::ScrollArea::vertical()
            .id_salt("v2_mat_panel_scroll")
            .auto_shrink([false, false])
            .max_height(body_h)
            .show(ui, |ui| {
                let w = ui.available_width();
                ui.set_min_width(w);
                ui.set_max_width(w);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(4.0);

                // ── 2 · Shader MODEL (not a quality tier) ──────────────────
                // Octane-style: Diffuse / PBR / Glass / SSS are different
                // materials, chosen at CREATE (Ctrl+M → Shader type). The panel
                // only shows which model this material is — not a tier switcher.
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Model")
                            .size(10.5)
                            .color(t::TEXT_DIM),
                    );
                    ui.label(
                        egui::RichText::new(shader_model_name(doc.shader_type))
                            .size(12.0)
                            .color(t::TEXT)
                            .strong(),
                    )
                    .on_hover_text(
                        "Shader model for this material (Standard PBR, Subsurface, Glass, VFX, Advanced).\n\
                         Not a quality tier — pick the type when you create the material \
                         (Ctrl+M → Shader type, or Add → Material).",
                    );
                    // ── MESH MAPS: bake right here ─────────────────────────
                    // Owner 2026-08-11: *"i should have a button right in the material setting
                    // after choosing PBR"*. Mesh maps were reachable only from the Poly menu /
                    // Tool Parameters, which is the wrong place: you bake them BECAUSE you are
                    // about to texture, and this is where texturing happens. The maps still
                    // belong to the MESH (a material only references them), so the button acts on
                    // the selected poly body and says so when there is not one.
                    if let Some(body) = self.poly_curv_body() {
                        let baked = self.ui.baked_images.for_body(body).count();
                        let busy = self.cache.mesh_map_rx.is_some()
                            || self.cache.pending_mesh_map_bake.is_some();
                        let has_uv = self
                            .cache
                            .poly_custom_data
                            .get(&body)
                            .is_some_and(|cd| !cd.uv.is_empty());
                        ui.add_space(10.0);
                        let label = if busy {
                            "Baking mesh maps…".to_string()
                        } else if baked > 0 {
                            format!("Rebake mesh maps ({baked})")
                        } else {
                            "Bake mesh maps".to_string()
                        };
                        let btn = ui.add_enabled(!busy && has_uv, egui::Button::new(
                            egui::RichText::new(label).size(10.5),
                        ));
                        if btn
                            .on_hover_text(
                                "Bake Curvature, AO, Thickness, Position, World Normal and ID from                                  this body's post-modifier mesh into its UV layout — the Substance                                  mesh-map set. Runs on a worker; settings in Tool Parameters.",
                            )
                            .on_disabled_hover_text(if has_uv {
                                "A bake is already running."
                            } else {
                                "This body has no UVs yet — run Poly ▸ UV Unwrap first."
                            })
                            .clicked()
                        {
                            bake_maps = true;
                        }
                        if !has_uv {
                            ui.label(
                                egui::RichText::new("needs UVs")
                                    .size(9.5)
                                    .color(t::WARNING),
                            );
                        }
                    }

                    // ── Generator MODE (Live vs Baked) ─────────────────────
                    // Owner-directed: keep both paths, user-selectable, and take the
                    // performance-oriented decision. Baked is a texture fetch; Live rebuilds a
                    // res^2 curvature grid per knob set per bake. Auto picks Baked whenever
                    // there is a baked map to sample, which is the right default for both
                    // pillars without the user having to know which one they are in.
                    {
                        use crate::material_panel::model::GeneratorMode;
                        let cur = GeneratorMode::from_u32(doc.generator_mode);
                        let resolved = self.resolved_generator_mode(mid);
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("Gen").size(10.5).color(t::TEXT_DIM));
                        egui::ComboBox::from_id_salt(("mp_gen_mode", mid.0))
                            .selected_text(egui::RichText::new(cur.label()).size(10.5))
                            .width(132.0)
                            .show_ui(ui, |ui| {
                                for m in [
                                    GeneratorMode::Auto,
                                    GeneratorMode::Live,
                                    GeneratorMode::Baked,
                                ] {
                                    if ui
                                        .selectable_label(cur == m, m.label())
                                        .on_hover_text(m.hint())
                                        .clicked()
                                        && cur != m
                                    {
                                        gen_mode_set = Some(m.as_u32());
                                    }
                                }
                            });
                        if cur == GeneratorMode::Auto {
                            ui.label(
                                egui::RichText::new(format!("-> {}", resolved.label()))
                                    .size(9.5)
                                    .color(t::TEXT_DIM),
                            );
                        } else if cur == GeneratorMode::Baked
                            && self
                                .poly_curv_body()
                                .and_then(|b| {
                                    self.baked_mesh_map(
                                        b,
                                        crate::mesh_map_bake::MeshMapKind::Curvature,
                                    )
                                })
                                .is_none()
                        {
                            // Say it rather than bake black — the failure mode this whole
                            // feature kept hitting.
                            ui.label(
                                egui::RichText::new("no baked map")
                                    .size(9.5)
                                    .color(t::WARNING),
                            )
                            .on_hover_text("Run Poly > Bake Mesh Maps, or switch to Live.");
                        }
                    }

                    // Eval / bake status so the owner knows which pipeline is running.
                    let live_gpu = mat.noise_pattern.enabled && mat.noise_pattern.amount > 0.001;
                    let mat_key = crate::node_graph_bake::GraphBakeOwner::Material(mid.0).key();
                    let baking = self.cache.graph_live_in_flight
                        || self.cache.graph_live_dirty
                        || self.cache.graph_bake_pending.contains_key(&mat_key);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(8.0);
                        if baking {
                            ui.spinner();
                            ui.label(
                                egui::RichText::new("baking maps…")
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(160, 200, 140)),
                            )
                            .on_hover_text(
                                "Maps baking at Bake N² target. 8/16 precision applies on install.",
                            );
                        } else if live_gpu {
                            ui.label(
                                egui::RichText::new("LIVE GPU")
                                    .size(10.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(120, 200, 140)),
                            )
                            .on_hover_text(
                                "Channel set to Live — GPU evaluates noise when possible. \
                                 Click Live/Bake on a channel row to switch. 8/16 is still available.",
                            );
                        }
                    });
                });

                // ── 3 · Texture space (LIVE — material-level; inheriting maps
                // follow; per-layer override is P3). Offset: NO engine binding
                // (TexMap has no offset field) → read-only, reported spec gap. ─
                f::section(ui, "props/mat2/texspace", "Texture space", None, |ui| {
                    let ts = doc.texture_space;
                    // Dropdown with all announced projection types (not a 2-option segment).
                    // UV gated off for pure SDF selections (no mesh UVs).
                    // UV needs mesh UVs — enable when any poly body exists in the scene.
                    let uv_allowed = self.scene.scene_graph.all_nodes().any(|(_, n)| {
                        matches!(n.payload, NodePayload::PolygonalBody { .. })
                    });
                    let mut proj_u = ts.projection;
                    if crate::material_editor::draw_projection_combo(
                        ui,
                        ("mp_texspace_proj", mid.0),
                        &mut proj_u,
                        uv_allowed,
                    ) {
                        mp_set_texture_space(&mut doc, |s| s.projection = proj_u);
                        edit_changed = true;
                        edit_desc = "Map projection";
                    }
                    let mut tiling = ts.tiling[0];
                    let r = f::num(ui, "Tiling", &mut tiling, 0.02, 0.05..=10.0, None);
                    if r.changed() {
                        mp_set_texture_space(&mut doc, |s| s.tiling = [tiling, tiling]);
                        edit_changed = true;
                        edit_desc = "Map tiling";
                    }
                    if r.drag_started() {
                        edit_drag = true;
                    }
                    // Box (triplanar) only: blend softness + per-plane 90° rotation.
                    if ts.projection == 0 {
                        let mut blend = ts.blend;
                        let rb = f::num(ui, "Blend", &mut blend, 0.05, 0.25..=7.75, None)
                            .on_hover_text(
                                "Triplanar plane blend — lower = smoother seams between projections, higher = harder edges",
                            );
                        if rb.changed() {
                            mp_set_texture_space(&mut doc, |s| s.blend = blend);
                            edit_changed = true;
                            edit_desc = "Triplanar blend";
                        }
                        if rb.drag_started() {
                            edit_drag = true;
                        }
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Rotate 90°")
                                    .size(10.0)
                                    .color(t::TEXT_DIM),
                            );
                            ui.add_space(4.0);
                            let mut cycle = |ui: &mut egui::Ui,
                                             label: &str,
                                             tip: &str,
                                             get: fn(&TextureSpace) -> u8,
                                             set: fn(&mut TextureSpace, u8)| {
                                let q = get(&ts) & 3;
                                let deg = q as u32 * 90;
                                let text = format!("{label} {deg}°");
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new(text).size(10.0),
                                    ))
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    mp_set_texture_space(&mut doc, |s| set(s, (get(s) + 1) & 3));
                                    edit_changed = true;
                                    edit_desc = "Triplanar plane rotation";
                                }
                            };
                            cycle(
                                ui,
                                "X",
                                "YZ projection plane (X-facing) — click cycles 0° → 90° → 180° → 270°",
                                |s| s.rot_x,
                                |s, v| s.rot_x = v,
                            );
                            cycle(
                                ui,
                                "Y",
                                "XZ projection plane (Y-facing) — click cycles 0° → 90° → 180° → 270°",
                                |s| s.rot_y,
                                |s, v| s.rot_y = v,
                            );
                            cycle(
                                ui,
                                "Z",
                                "XY projection plane (Z-facing) — click cycles 0° → 90° → 180° → 270°",
                                |s| s.rot_z,
                                |s, v| s.rot_z = v,
                            );
                        });
                    }
                    ro_row(ui, "Offset", &format!("{:.2} , {:.2}", ts.offset[0], ts.offset[1]))
                        ;
                    caption(
                        ui,
                        if ts.projection == 0 {
                            "Box (triplanar): Blend softens plane seams · Rotate 90° per axis · Offset: no TexMap field (per-layer override via subgraph)"
                        } else {
                            "inherited by all maps · material offset: no TexMap field (bincode-frozen) — per-layer override offset works via the layer subgraph"
                        },
                    );
                });

                // ── 4 · Channels ────────────────────────────────────────────
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("CHANNELS")
                            .size(9.5)
                            .color(t::TEXT_SECTION)
                            .strong(),
                    );
                    // Bake target resolution (owner: choose quality when maps bake).
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(8.0);
                        let mut res = snap_panel_bake_res(doc.bake_res);
                        let before = res;
                        egui::ComboBox::from_id_salt(("mp_bake_target", mid.0))
                            .selected_text(format!("Bake {res}²"))
                            .width(96.0)
                            .show_ui(ui, |ui| {
                                let steps: &[u32] =
                                    if crate::material_textures::product_map_ceiling()
                                        >= crate::material_textures::MAX_MAP_RES
                                    {
                                        &BAKE_TARGET_STEPS_4K
                                    } else {
                                        &BAKE_TARGET_STEPS
                                    };
                                for &s in steps {
                                    ui.selectable_value(&mut res, s, format!("{s}²"));
                                }
                            })
                            .response
                            .on_hover_text(
                                "Map bake TARGET resolution (working res before GPU install).\n\
                                 128 / 256 = fast scrub · 512 = balanced · 1024 = full quality.\n\
                                 2048 = product ceiling (2K, D3/M4) — grows the material array.\n\
                                 4K deferred (set AZRA_ALLOW_4K_MAPS=1 to unlock).\n\
                                 Image Mult stacks default toward 1024 for sharpness;\n\
                                 lower targets bake faster but soft Mult over photos.\n\
                                 Same control as Master Material → Map res in the shader tree.",
                            );
                        if res != before {
                            doc.bake_res = res;
                            edit_changed = true;
                            edit_desc = "Bake target resolution";
                        }
                    });
                });
                let texture_space = doc.texture_space;
                // Layer selection lives on UiState (egui's temp store never round-tripped it —
                // 3621 writes, 0 reads ever saw a value). Copy out, thread through the channel
                // loop + inspector, write back once at the end.
                let mut layer_sel: Option<layer_ui::LayerSel> =
                    self.ui.mp_layer_sel.get(&mid.0).copied();
                // Snapshot the kinds present BEFORE the &mut iteration (the duplicate/retype
                // submenus must not offer a kind that already exists — see `present_kinds`).
                let present_kinds: Vec<ChannelKind> =
                    doc.channels.iter().map(|c| c.kind).collect();
                for ch in &mut doc.channels {
                    if only_driven && matches!(ch.state, ChannelState::Value(_)) {
                        continue;
                    }
                    let bit = 1u32 << kind_bit(ch.kind);
                    let is_open = expanded & bit != 0;
                    let conflict = conflicts & bit != 0;
                    let out = channel_row(
                        ui,
                        mid.0,
                        ch,
                        is_open,
                        conflict,
                        texture_space,
                        gen_ctx,
                        self.gpu.as_ref(),
                        &thumbs,
                        &mut layer_sel,
                        &present_kinds,
                        &mat,
                    );
                    if let Some(to) = out.duplicate_to {
                        duplicate_req = Some((ch.kind, to));
                    }
                    if let Some(to) = out.retype_to {
                        retype_req = Some((ch.kind, to));
                    }
                    if let Some((li, to)) = out.copy_layer_to {
                        copy_layer_req = Some((ch.kind, li, to));
                    }
                    if let Some(li) = out.load_layer {
                        load_layer = Some((ch.kind, li, false));
                    }
                    if let Some(li) = out.load_mask {
                        load_layer = Some((ch.kind, li, true));
                    }
                    if let Some(item_idx) = out.dropped_lib_item {
                        dropped_map = Some((ch.kind, item_idx));
                    }
                    if let Some(path) = out.dropped_os_file.clone() {
                        dropped_os = Some((ch.kind, path));
                    }
                    if out.toggled {
                        expanded ^= bit;
                    }
                    if out.edit_in_tree {
                        frame_channel = Some(ch.kind);
                    }
                    if out.changed {
                        edit_changed = true;
                        edit_desc = kind_label(ch.kind);
                        edit_channel = Some(ch.kind);
                    }
                    if out.drag_started {
                        edit_drag = true;
                    }
                    if out.remove {
                        let needs_confirm =
                            !matches!(ch.state, ChannelState::Value(_));
                        remove_kind = Some((ch.kind, needs_confirm));
                    }
                    if out.revert {
                        revert_kind = Some(ch.kind);
                    }
                    // P0-4: conflict badge resolutions (each routes to an
                    // existing mechanism — no new ownership semantics).
                    if out.conflict_yield_graph {
                        conflict_yield = Some(ch.kind);
                    }
                    if out.conflict_keep_panel {
                        conflict_keep = Some(ch.kind);
                    }
                    if out.bit_depth_toggled {
                        bit_depth_toggle_kind = Some(ch.kind);
                    }
                    if out.bake_res_changed {
                        edit_changed = true;
                        edit_desc = "Bake target resolution";
                    }
                    if out.eval_mode_changed {
                        edit_changed = true;
                        edit_desc = "Live / Bake mode";
                    }
                }

                // ── 5 · Add channel (LIVE) + filter toggle ──────────────────
                // Grouped by category with renderer support (Hybrid / Path Trace).
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.azra_menu_button(
                        egui::RichText::new(format!("{} Add channel", ph::PLUS)).size(11.0),
                        |ui| {
                            let missing: Vec<ChannelKind> = ALL_KINDS
                                .iter()
                                .copied()
                                .filter(|k| doc.channel(*k).is_none())
                                .collect();
                            if missing.is_empty() {
                                ui.label(
                                    egui::RichText::new("All channels present")
                                        .size(10.5)
                                        .color(t::TEXT_FAINT),
                                );
                            } else if let Some(k) = draw_channel_kind_menu(ui, &missing) {
                                add_kind = Some(k);
                                ui.close_menu();
                            }
                        },
                    )
                    .response
                    .on_hover_text(
                        "Channels are grouped by what they drive.\n\
                         Hybrid = realtime viewport · Path Trace = ground truth.",
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui
                        .checkbox(&mut only_driven, "")
                        .on_hover_text("Hide plain-value channels")
                        .changed()
                    {
                        // view state only — written back below
                    }
                    ui.label(
                        egui::RichText::new("show only driven channels")
                            .size(11.0)
                            .color(t::TEXT_SECONDARY),
                    );
                });

                // Mesh-map baker (not the graph "Bake 256²"). Lives on the poly body.
                if self.poly_curv_body().is_some() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("MESH MAPS")
                            .size(10.0)
                            .color(t::TEXT_DIM),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Curvature / AO / Normal / Position from the mesh unwrap.\n\
                             Not the channel Bake 256² above (that is the material graph).",
                        )
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                    );
                    self.draw_tool_params_mesh_maps(ui);
                    ui.add_space(8.0);
                }

                // ── LAYER PROPERTIES — docked OUTSIDE the channel tree ───────
                // (Substance layout: tree above, selected-layer inspector below)
                let mut insp = ChannelRowOut::default();
                // Drive picker: float sockets from this material's live graph (ADR Decision 2).
                let drive_sources: Vec<layer_ui::DriveCandidate> = self
                    .ui
                    .material_node_graphs
                    .get(&mid.0)
                    .map(layer_ui::collect_drive_candidates)
                    .unwrap_or_default();
                layer_ui::draw_layer_inspector(
                    ui,
                    mid.0,
                    &mut doc,
                    gen_ctx,
                    self.gpu.as_ref(),
                    &thumbs,
                    &drive_sources,
                    layer_sel,
                    &mut insp,
                );
                // Persist for the next frame (and for the load_layer/load_mask lookups below).
                match layer_sel {
                    Some(v) => { self.ui.mp_layer_sel.insert(mid.0, v); }
                    None => { self.ui.mp_layer_sel.remove(&mid.0); }
                }
                if let Some(li) = insp.load_layer {
                    // Channel kind from material selection
                    if let Some((k, _)) = layer_sel {
                        if let Some(kind) = ALL_KINDS.iter().copied().find(|c| *c as u32 == k) {
                            load_layer = Some((kind, li, false));
                        }
                    }
                }
                if let Some(li) = insp.load_mask {
                    if let Some((k, _)) = layer_sel {
                        if let Some(kind) = ALL_KINDS.iter().copied().find(|c| *c as u32 == k) {
                            load_layer = Some((kind, li, true));
                        }
                    }
                }
                if insp.changed {
                    edit_changed = true;
                    edit_desc = "Layer properties";
                    // Inspector is material-scoped; map selection → channel for F2 filter.
                    if let Some((k, _)) = layer_sel {
                        edit_channel = ALL_KINDS.iter().copied().find(|c| *c as u32 == k);
                    }
                }
                if insp.drag_started {
                    edit_drag = true;
                }

                ui.add_space(6.0);
            });

        ui.data_mut(|d| {
            d.insert_temp(expand_id, expanded);
            d.insert_temp(filter_id, only_driven);
        });

        // ── Commit (P2): one write-back per frame, v1 undo pattern ──────────
        let doc_destructive = super::preset::apply_is_destructive(&doc);
        if edit_changed {
            // F2: accumulate single-channel scrub filter before lower schedules Live bake.
            if let Some(k) = edit_channel {
                let mask = crate::node_graph_bake::BakeChannelMask::from_channel_kind(k);
                if mask.any() {
                    self.cache.graph_live_channel_filter = Some(
                        self.cache
                            .graph_live_channel_filter
                            .map(|p| p.or(mask))
                            .unwrap_or(mask),
                    );
                }
            }
            self.mp_commit_doc_edit(mid, doc, edit_desc, edit_drag);
        }
        // Live 8/16 height flag — apply high_precision to the installed TexMap immediately
        // so Displacement/Bump update without waiting for a full re-bake (pixels already
        // 16-bit packed when high_precision was last installed).
        if let Some(k) = bit_depth_toggle_kind {
            self.mp_apply_channel_bit_depth(mid, k);
        }
        if let Some(k) = add_kind {
            self.mp_add_channel(mid, k);
        }
        match remove_kind {
            Some((k, false)) => self.mp_remove_channel_confirmed(mid, k),
            Some((k, true)) => {
                self.ui.mp_confirm =
                    Some(super::edit::MpConfirm::RemoveChannel { mid: mid.0, kind: k });
            }
            None => {}
        }
        if let Some((from, to)) = duplicate_req {
            self.mp_duplicate_channel_to(mid, from, to);
        }
        if let Some((from, to)) = retype_req {
            self.mp_retype_channel(mid, from, to);
        }
        if let Some((from, li, to)) = copy_layer_req {
            self.mp_copy_layer_to_channel(mid, from, li, to);
        }
        if let Some(k) = revert_kind {
            self.ui.mp_confirm =
                Some(super::edit::MpConfirm::RevertChannel { mid: mid.0, kind: k });
        }
        // P0-4: "Let the shader tree own it" DESTROYS the panel's layer stack →
        // same confirmation class as Remove/Revert. "Keep my map" changes no
        // scene state (it only clears the advisory bit) → applies immediately.
        if let Some(k) = conflict_yield {
            self.ui.mp_confirm =
                Some(super::edit::MpConfirm::YieldChannelToGraph { mid: mid.0, kind: k });
        }
        if let Some(k) = conflict_keep {
            self.mp_conflict_keep_panel(mid, k);
        }
        // P3 per-layer Load…: remember which layer claims the decoded slot, then
        // run the SHARED deferred map picker (file_dialog.rs worker path).
        // Deferred to after the draw: queueing a bake mutates app state the panel is reading.
        if bake_maps {
            self.poly_request_mesh_map_bake();
        }
        // Apply the generator-mode switch after the draw (the doc was borrowed above).
        if let Some(code) = gen_mode_set {
            if let Some(d) = self.ui.material_panel_docs.get_mut(&mid.0) {
                d.generator_mode = code;
            }
            // Re-bake so the change is visible immediately: the mode decides where curvature
            // COMES FROM, so nothing on screen reflects it until the maps are rebuilt.
            self.enqueue_material_node_graph_bake(mid);
        }
        if let Some((kind, li, is_mask)) = load_layer {
            if let Some(mk) = kind.map_kind() {
                self.ui.mp_pending_layer_load = Some((mid.0, kind, li, is_mask));
                self.request_map_file_dialog(
                    mk,
                    crate::material_textures::MapTarget::Graph(mid),
                );
            }
        }
        // Owner #7: a library texture was dropped on a channel row — same effect as picking a
        // file with "Load…", but the path is already known (no picker). Pick the file from the
        // dropped library item that best matches this channel's role (single-file texture items
        // use their only file; PBR material sets pick the map matching the channel kind).
        if let Some((kind, item_idx)) = dropped_map {
            if let Some(mk) = kind.map_kind() {
                if let Some(path) = self.mp_resolve_drop_texture_path(item_idx, mk) {
                    // CREATE the destination Image layer if the channel has none (owner
                    // 2026-07-28: "if I click drag image from library inside a channel it should
                    // automatically create an image layer with that image").
                    //
                    // This used to hardcode layer index 0 and assume it already existed AND was
                    // an Image. `mp_apply_pending_layer_load` does `layers.get_mut(li)` and then
                    // matches `LayerContent::Image`, so on an empty channel — or one whose first
                    // layer is a Noise/Generator — the decoded texture was silently dropped: the
                    // file loaded, nothing appeared, no error.
                    //
                    // Now the drop targets a real Image layer every time: reuse the topmost
                    // existing one (so re-dropping onto a channel REPLACES the image rather than
                    // stacking duplicates), else append a fresh one.
                    let li = self.mp_ensure_image_layer_for_drop(mid, kind);
                    self.ui.mp_pending_layer_load = Some((mid.0, kind, li, false));
                    self.mp_mark_hand_loaded_map(mid, mk, 1.0);
                    self.file_dialogs.load_map_from_path(
                        path,
                        crate::material_textures::MapTarget::Graph(mid),
                        mk,
                    );
                } else {
                    self.toast("Dropped item has no matching texture image");
                }
            }
        }
        // Same gesture, OS file explorer instead of the library. The path is already
        // known and already extension-checked by the row, so this skips
        // `mp_resolve_drop_texture_path` and otherwise walks the IDENTICAL path:
        // ensure a real Image layer exists (reusing the topmost one, so re-dropping
        // REPLACES rather than stacking), then hand the file to the shared loader.
        if let Some((kind, path)) = dropped_os {
            if let Some(mk) = kind.map_kind() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("image")
                    .to_string();
                let li = self.mp_ensure_image_layer_for_drop(mid, kind);
                self.ui.mp_pending_layer_load = Some((mid.0, kind, li, false));
                self.file_dialogs.load_map_from_path(
                    path,
                    crate::material_textures::MapTarget::Graph(mid),
                    mk,
                );
                self.toast(&format!("{name} → {}", kind_label(kind)));
            }
        }
        // ── P5 preset actions (destructive-apply gate on the CURRENT doc) ───
        if let Some(p) = apply_preset {
            if doc_destructive {
                self.ui.mp_pending_preset = Some((mid.0, p));
            } else {
                self.mp_apply_preset(mid, &p);
            }
        }
        if preset_save_as {
            self.ui.mp_preset_naming = Some((mid.0, name.clone()));
        }
        if let Some(i) = preset_delete {
            self.ui.mp_preset_delete = Some(i);
        }

        self.mp_draw_confirm_dialog(ui.ctx());
        self.mp_draw_preset_dialogs(ui.ctx());

        if let Some(kind) = frame_channel {
            // P8: dot / "Edit in shader tree" → frame THIS channel's node, selected.
            self.mp_frame_shader_node(mid, Some(kind));
        }
    }

}
