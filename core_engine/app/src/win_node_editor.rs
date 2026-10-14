// win_node_editor.rs — Shader node graph editor window
// Extracted from ui_windows.rs
use eframe::egui;
use crate::*;
use crate::shader_nodes;
use core_math::primitives::MaterialId;

impl AzraApp {
    // ================================================================
    // 6. Node Editor Window
    // ================================================================
    pub(crate) fn draw_node_editor_window(&mut self, ctx: &egui::Context) {
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2")
            && self.ui2_panel_visible(&[crate::ui2::PanelKind::NodeEditor])
        {
            self.ui.show_node_editor = false;
            return;
        }
        if !self.ui.show_node_editor { return; }
        let mut open = self.ui.show_node_editor;
        egui::Window::new("\u{1f517} Shader Node Editor")
            .id(egui::Id::new("shader_node_editor"))
            .collapsible(true)
            .resizable([true, true])
            .default_size([820.0, 500.0])
            .min_width(360.0)
            .min_height(260.0)
            .max_size(ctx.screen_rect().size() * 0.95)
            .drag_to_scroll(false)
            .open(&mut open)
            .show(ctx, |ui| self.draw_node_editor_panel(ui));
        self.ui.show_node_editor = open;
    }

    pub(crate) fn reveal_shader_node_editor(&mut self) {
        if std::env::var("AZRA_UI").ok().as_deref() == Some("v2") {
            let kind = crate::ui2::PanelKind::NodeEditor;
            self.ui2_set_panel_visible(&[kind], crate::ui2::DockSide::Bottom, true);
            if let Some(group) = self.ui2.dock.iter_mut().find(|group| group.tabs.contains(&kind)) {
                group.active = group.tabs.iter().position(|tab| *tab == kind).unwrap_or(0);
                group.collapsed = false;
                group.head_coll = false;
                if matches!(group.side, crate::ui2::DockSide::Top | crate::ui2::DockSide::Bottom)
                    && group.height <= 0.0
                {
                    group.height = 320.0;
                }
            }
            self.ui.show_node_editor = false;
        } else {
            self.ui.show_node_editor = true;
        }
    }

    /// Shared Shader Node Editor body used by floating and docked panels.
    pub(crate) fn draw_node_editor_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut node_changed = false;
        let mut node_atomic_change = false;
        let mut node_toast: Option<String> = None;
        let mut node_show_wgsl: Option<String> = None;
        let mut node_graph_before_edit: Option<shader_nodes::NodeGraph> = None;
        let mut add_material_to_selection = false;

        let target_label = match self.ui.node_graph_target {
            Some(NodeGraphTarget::Prim(pi)) => self
                .scene
                .primitives
                .get(pi)
                .map(|p| format!("Prim: {}", p.name)),
            Some(NodeGraphTarget::Material(mid)) => {
                let name = self
                    .scene
                    .scene_graph
                    .materials()
                    .find(|(_, id, _, _)| *id == mid)
                    .map(|(_, _, n, _)| {
                        if n.is_empty() {
                            format!("Material {}", mid.0)
                        } else {
                            n.to_string()
                        }
                    });
                Some(format!(
                    "Material: {}",
                    name.unwrap_or_else(|| format!("id {}", mid.0))
                ))
            }
            None => None,
        };

                if std::env::var("AZRA_UI_PROBE").is_ok() {
                    let h = ui.max_rect().height();
                    let last = ui.ctx().data_mut(|d| {
                        let prev = d.get_temp::<f32>(egui::Id::new("azra_ui_probe_ne_h"));
                        d.insert_temp(egui::Id::new("azra_ui_probe_ne_h"), h);
                        prev
                    });
                    if let Some(last) = last {
                        if (h - last).abs() > 1.0 {
                            eprintln!(
                                "[UIProbe] node editor content h {last:.1} -> {h:.1} (d {:+.1})",
                                h - last
                            );
                        }
                    }
                }
                // Full-content drag sink (same class as outliner_tree_dnd_guard): register FIRST
                // so presses over empty chrome / gaps resolve drag here instead of the Window
                // Area's movable Sense::drag (which moves the whole window + feels like "all tabs move").
                let content_guard = ui.available_rect_before_wrap();
                let _ = ui.interact(
                    content_guard,
                    ui.id().with("shader_node_editor_content_drag_guard"),
                    egui::Sense::drag(),
                );
                if let Some(ref name) = target_label {
                    ui.label(egui::RichText::new(format!("Editing: {name}")).weak());
                    if matches!(self.ui.node_graph_target, Some(NodeGraphTarget::Material(_))) {
                        ui.label(
                            egui::RichText::new(
                                "Master Material node = Properties panel (one material). Assign it to prims/meshes to use it.",
                            )
                            .small()
                            .italics()
                            .weak(),
                        );
                    }
                } else {
                    let has_selection = self.has_material_assign_target();
                    if ui
                        .add_enabled(
                            has_selection,
                            egui::Button::new("+ Add Material to Selected Object"),
                        )
                        .on_disabled_hover_text("Select an SDF or polygon object first")
                        .clicked()
                    {
                        add_material_to_selection = true;
                    }
                }
                ui.separator();
                // Scene prim list for Prim SDF Mask / SDF Curvature pickers.
                let prim_choices: Vec<(u32, String)> = self
                    .scene
                    .scene_graph
                    .all_primitives_ordered()
                    .into_iter()
                    .map(|(_nid, _i, p)| {
                        (
                            p.prim_uid,
                            if p.name.is_empty() {
                                format!("Prim {}", p.prim_uid)
                            } else {
                                p.name.clone()
                            },
                        )
                    })
                    .collect();

                // Consume the remaining window body exactly once, then draw the canvas and footer
                // in bounded child UIs. Child contents cannot feed their measured height back into
                // the floating window, so continuous repaint cannot ratchet its bottom edge down.
                let content_rect = ui.available_rect_before_wrap();
                let _ = ui.allocate_rect(content_rect, egui::Sense::hover());
                const FOOTER_H: f32 = 40.0;
                let footer_top = (content_rect.bottom() - FOOTER_H).max(content_rect.top());
                let footer_rect = egui::Rect::from_min_max(
                    egui::pos2(content_rect.left(), footer_top),
                    content_rect.max,
                );
                let canvas_rect = egui::Rect::from_min_max(
                    content_rect.min,
                    egui::pos2(
                        content_rect.right(),
                        (footer_rect.top() - ui.spacing().item_spacing.y).max(content_rect.top()),
                    ),
                );
                let mut ed = shader_nodes::NodeEditorOutcome::default();
                let mut canvas_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(canvas_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                canvas_ui.set_min_size(canvas_rect.size());
                canvas_ui.set_max_size(canvas_rect.size());
                ed = shader_nodes::draw_node_editor(
                    &mut canvas_ui,
                    &mut self.ui.node_graph,
                    &mut self.ui.node_drag_state,
                    &prim_choices,
                );
                node_changed = ed.changed;
                node_atomic_change = ed.atomic_change;
                node_graph_before_edit = ed.graph_before_edit;
                // Image Texture "Load Image" → same deferred dialog as Properties map UI.
                if let Some(kind_tag) = ed.load_map_kind {
                    if let Some(kind) =
                        core_math::pbr_material::MapKind::from_wire_tag(kind_tag)
                    {
                        match self.ui.node_graph_target {
                            Some(NodeGraphTarget::Material(mid)) => {
                                self.request_map_file_dialog(
                                    kind,
                                    crate::material_textures::MapTarget::Graph(mid),
                                );
                            }
                            Some(NodeGraphTarget::Prim(pi)) => {
                                if let Some(prim) = self.scene.primitives.get(pi) {
                                    let uid = prim.prim_uid;
                                    self.request_map_file_dialog(
                                        kind,
                                        crate::material_textures::MapTarget::Prim(
                                            core_math::scene_graph::NodeId(0),
                                            uid,
                                        ),
                                    );
                                }
                            }
                            None => {
                                self.toast("Open graph from a Material (or prim) to load maps");
                            }
                        }
                    }
                }

                let mut footer_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(footer_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                footer_ui.separator();
                footer_ui.horizontal(|ui| {
                    if self.cache.graph_live_dirty || self.cache.graph_live_in_flight {
                        ui.spinner();
                        ui.label(egui::RichText::new("Updating maps…").small());
                    }
                    if ui.button("Show WGSL").clicked() {
                        let code = self.ui.node_graph.generate_wgsl();
                        node_show_wgsl = Some(code);
                    }
                    let has_target = self.ui.node_graph_target.is_some();
                    let apply_label = match self.ui.node_graph_target {
                        Some(NodeGraphTarget::Material(_)) => "Apply to Material",
                        Some(NodeGraphTarget::Prim(_)) => "Apply to Primitive",
                        None => "Apply",
                    };
                    if ui
                        .add_enabled(has_target, egui::Button::new(apply_label))
                        .clicked()
                    {
                        match self.ui.node_graph_target {
                            Some(NodeGraphTarget::Material(mid)) => {
                                let label = target_label.clone().unwrap_or_else(|| "Material".into());
                                // Graph + PbrMaterial are one material — push Master sockets, then bake maps.
                                // Clear GPU live noise layer so the baked map is the single source of truth.
                                self.transaction("Apply material shader graph", |s| {
                                    let g = s.ui.node_graph.clone();
                                    if let Some(m) = s.scene.scene_graph.material_by_id_mut(mid) {
                                        g.push_to_pbr(m);
                                        m.noise_pattern.enabled = false;
                                        m.noise_pattern.amount = 0.0;
                                        m.albedo_map.strength = 1.0;
                                    }
                                    s.ui.material_node_graphs.insert(mid.0, g);
                                });
                                self.enqueue_material_node_graph_bake(mid);
                                node_toast = Some(format!(
                                    "Applied graph on {label} (maps + base properties)"
                                ));
                            }
                            Some(NodeGraphTarget::Prim(pi)) => {
                                let code = self.ui.node_graph.generate_wgsl();
                                if let Some(prim) = self.scene.primitives.get(pi) {
                                    let uid = prim.prim_uid;
                                    let name = prim.name.clone();
                                    let graph_clone = self.ui.node_graph.clone();
                                    self.transaction("Apply shader graph", |s| {
                                        s.ui.node_graphs.insert(uid, graph_clone);
                                        s.write_prim(pi, |p| {
                                            p.node_graph = Some(code.clone());
                                        });
                                    });
                                    self.enqueue_node_graph_bake(uid);
                                    // Prim graphs still inject into deferred uber WGSL.
                                    self.shader_dirty = true;
                                    node_toast = Some(format!(
                                        "Applied + baked shader graph to '{}' (prim override)",
                                        name
                                    ));
                                }
                            }
                            None => {}
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui
                        .add_enabled(has_target, egui::Button::new("Clear Graph"))
                        .clicked()
                    {
                        match self.ui.node_graph_target {
                            Some(NodeGraphTarget::Material(mid)) => {
                                let label = target_label.clone().unwrap_or_else(|| "Material".into());
                                self.transaction("Clear material shader graph", |s| {
                                    s.ui.material_node_graphs.remove(&mid.0);
                                });
                                self.clear_material_node_graph_maps(mid);
                                node_toast = Some(format!("Cleared shader graph from {label}"));
                            }
                            Some(NodeGraphTarget::Prim(pi)) => {
                                if let Some(prim) = self.scene.primitives.get(pi) {
                                    let uid = prim.prim_uid;
                                    let name = prim.name.clone();
                                    self.transaction("Clear shader graph", |s| {
                                        s.ui.node_graphs.remove(&uid);
                                        s.write_prim(pi, |p| {
                                            p.node_graph = None;
                                        });
                                    });
                                    self.clear_node_graph_maps(uid);
                                    node_toast =
                                        Some(format!("Cleared shader graph from '{}'", name));
                                }
                            }
                            None => {}
                        }
                        self.shader_dirty = true;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                });
        if add_material_to_selection {
            if let Some(mid) = self.promote_embedded_material_for_selection(Some(self.ui.node_graph.clone())) {
                self.open_material_shader_graph(mid);
                if crate::node_graph_bake::channels_from_graph(&self.ui.node_graph).any() {
                    self.enqueue_material_node_graph_bake(mid);
                }
            }
        }
        if node_changed {
            if !self.has_active_episode() {
                let graph_before_edit = node_graph_before_edit
                    .take()
                    .expect("node editor changes must retain their pre-edit graph");
                let edited_graph = std::mem::replace(&mut self.ui.node_graph, graph_before_edit);
                if node_atomic_change {
                    self.transaction_snapshot_only("Edit shader graph");
                } else if ctx.input(|i| i.pointer.any_down()) {
                    self.begin_episode("Edit shader graph");
                } else {
                    self.transaction_snapshot_only("Edit shader graph");
                }
                self.ui.node_graph = edited_graph;
            }
            self.ui.node_graph.wgsl_cache = None;
            match self.ui.node_graph_target {
                Some(NodeGraphTarget::Material(mid)) => {
                    // Push Master scalars first.
                    self.commit_material_node_graph_edit(mid, false);
                    let needs_bake = crate::node_graph_bake::bake_channels_from_graph(
                        &self.ui.node_graph,
                        self.ui.material_panel_docs.get(&mid.0),
                    ).any();
                    if needs_bake {
                        self.schedule_live_material_graph_bake(mid);
                    }
                }
                Some(NodeGraphTarget::Prim(pi)) => {
                    if let Some(prim) = self.scene.primitives.get(pi) {
                        let uid = prim.prim_uid;
                        self.ui.node_graphs.insert(uid, self.ui.node_graph.clone());
                        if self.apply_live_gpu_noise_prim(uid) {
                            self.cache.graph_live_owner = None;
                            self.cache.graph_live_dirty = false;
                        } else {
                            self.schedule_live_prim_graph_bake(uid);
                        }
                    }
                }
                None => {}
            }
        }
        if let Some(msg) = node_toast {
            self.toast(&msg);
        }
        if let Some(code) = node_show_wgsl {
            self.ai.code_review_result = code;
            self.ai.show_code_review = true;
        }
    }

    /// Open the node editor bound to an outliner Material.
    /// Master Material node defaults are seeded from the live `PbrMaterial` so the graph
    /// and the Properties panel show the **same** material (not two copies).
    pub(crate) fn open_material_shader_graph(&mut self, mid: MaterialId) {
        self.ui.node_drag_state.clear_selection();
        let mut g = self
            .ui
            .material_node_graphs
            .get(&mid.0)
            .cloned()
            .unwrap_or_default();
        if let Some((_nid, mat)) = self.scene.scene_graph.material_by_id(mid) {
            g.pull_from_pbr(mat);
        }
        // Persist so the graph always exists once opened (UI + node share one material).
        self.ui.material_node_graphs.insert(mid.0, g.clone());
        self.ui.node_graph = g;
        self.ui.node_graph_target = Some(NodeGraphTarget::Material(mid));
        self.reveal_shader_node_editor();
    }

    /// Open the node editor bound to a prim (legacy / override).
    pub(crate) fn open_prim_shader_graph(&mut self, prim_index: usize) {
        if let Some(prim) = self.scene.primitives.get(prim_index) {
            self.ui.node_drag_state.clear_selection();
            let uid = prim.prim_uid;
            if let Some(g) = self.ui.node_graphs.get(&uid) {
                self.ui.node_graph = g.clone();
            } else {
                self.ui.node_graph = shader_nodes::NodeGraph::default();
            }
            self.ui.node_graph_target = Some(NodeGraphTarget::Prim(prim_index));
            self.reveal_shader_node_editor();
        }
    }

    /// True if this outliner material has a stored procedural graph.
    pub(crate) fn material_has_shader_graph(&self, mid: MaterialId) -> bool {
        self.ui.material_node_graphs.contains_key(&mid.0)
    }

    /// Properties panel edited `PbrMaterial` → mirror into Master Material node (same material).
    pub(crate) fn sync_pbr_into_material_graph(&mut self, mid: MaterialId) {
        let Some((_nid, mat)) = self.scene.scene_graph.material_by_id(mid) else {
            return;
        };
        let mat = mat.clone();
        if let Some(g) = self.ui.material_node_graphs.get_mut(&mid.0) {
            g.pull_from_pbr(&mat);
        }
        if matches!(self.ui.node_graph_target, Some(NodeGraphTarget::Material(m)) if m == mid) {
            self.ui.node_graph.pull_from_pbr(&mat);
        }
    }

    /// After node-editor edits: save graph, push Master → PbrMaterial, optional rebake.
    ///
    /// Master Material unconnected sockets **are** the Properties base values (one material).
    /// Wired sockets bake to maps; unconnected channels clear stale maps so albedo tint
    /// is not double-multiplied by an old solid bake.
    pub(crate) fn commit_material_node_graph_edit(&mut self, mid: MaterialId, rebake: bool) {
        use crate::shader_nodes::NodeGraph;
        use core_math::pbr_material::MapKind;
        let g = self.ui.node_graph.clone();
        let direct_albedo = g.master_direct_constant_color(NodeGraph::MO_ALBEDO);
        // P2 ownership gate (spec C1): a map slot the PANEL owns (hand-loaded
        // image layer) is never cleared by graph-side commits — only sockets the
        // graph stopped driving AND the panel does not own fall back to scalar.
        let owned: Vec<MapKind> = MapKind::ALL
            .iter()
            .copied()
            .filter(|&k| self.mp_panel_owns_map(mid.0, k))
            .collect();
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            g.push_to_pbr(m);
            if g.master_socket_driven(NodeGraph::MO_ALBEDO)
                || g.master_socket_driven(NodeGraph::MO_ROUGHNESS)
            {
                m.noise_pattern.enabled = false;
                m.noise_pattern.amount = 0.0;
            }
            if let Some(color) = direct_albedo {
                m.albedo = glam::Vec3::from_array(color);
                m.albedo_map.tex_id = 0;
                m.albedo_map.absolute = false;
                m.noise_pattern.enabled = false;
                m.noise_pattern.amount = 0.0;
            }
            // Unconnected Master sockets own the scalar — drop map so UI/node/viewport match.
            let clear = |m: &mut core_math::pbr_material::PbrMaterial, k: MapKind| {
                if !owned.contains(&k) {
                    m.map_mut(k).tex_id = 0;
                }
            };
            // G2: the socket→map-slot OR-folds (AO⊂albedo, height⊂normal/bump…)
            // live ONCE, in `channels_from_graph` — this block was re-deriving
            // them inverted. `!wired.x` = the graph stopped driving that slot.
            let wired = crate::node_graph_bake::channels_from_graph(&g);
            if !wired.albedo {
                clear(m, MapKind::Albedo);
            }
            if !wired.roughness {
                clear(m, MapKind::Roughness);
            }
            if !wired.metallic {
                clear(m, MapKind::Metallic);
            }
            if !wired.emission {
                clear(m, MapKind::Emission);
            }
            if !wired.normal {
                clear(m, MapKind::Normal);
            }
            if !wired.bump {
                clear(m, MapKind::Bump);
            }
            if !wired.displacement {
                clear(m, MapKind::Displacement);
            }
            if !wired.sss_color {
                clear(m, MapKind::SssColor);
            }
        }
        self.ui.material_node_graphs.insert(mid.0, g);
        // P2 PROMOTION CHOKE POINT (spec C1): every hand edit in the node editor
        // routes through here (win_node_editor.rs node_changed → this fn) —
        // reconcile the panel doc's per-channel GraphDriven ownership.
        self.mp_reconcile_graph_ownership(mid);
        if rebake {
            self.enqueue_material_node_graph_bake(mid);
        }
        self.cache.prim_buf_stale = true;
        self.outliner.outliner_dirty = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Push simple Noise graph params into `PbrMaterial.noise_pattern` for **same-frame GPU**
    /// evaluation in `gbuffer.wgsl`. Returns true if handled (caller must NOT CPU-bake).
    pub(crate) fn apply_live_gpu_noise_material(&mut self, mid: MaterialId) -> bool {
        use glam::Vec3;
        let live = match self.ui.node_graph.extract_live_gpu_noise() {
            Some(l) => l,
            None => {
                // Complex graph / not noise — turn off GPU noise layer so baked maps win.
                if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                    m.noise_pattern.enabled = false;
                    m.noise_pattern.amount = 0.0;
                }
                return false;
            }
        };
        let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) else {
            return false;
        };
        // Fragment shader: color = mix(albedo, noise_color, noise). amount=1 → full replace.
        if live.drives_albedo {
            m.albedo = Vec3::from_array(live.color0);
            m.albedo_map.tex_id = 0; // must not multiply a stale bake over live noise
            m.albedo_map.strength = 0.0;
        }
        m.roughness = live.roughness0.clamp(0.0, 1.0);
        if live.drives_roughness {
            m.roughness_map.tex_id = 0;
            m.roughness_map.strength = 0.0;
        }
        m.noise_pattern.enabled = true;
        m.noise_pattern.amount = 1.0;
        m.noise_pattern.scale = live.scale;
        m.noise_pattern.octaves = live.octaves;
        m.noise_pattern.color = Vec3::from_array(live.color1);
        m.noise_pattern.roughness = live.roughness1.clamp(0.0, 1.0);
        m.noise_pattern.metallic = m.metallic; // no metallic modulation from live noise
        // Seed via edge_wear.w when edge wear is off (see gbuffer.wgsl).
        m.edge_wear.enabled = false;
        m.edge_wear.amount = 0.0;
        m.edge_wear.sharpness = live.seed;
        // Blend curve via cavity_dirt pack (amount stays 0 → cavity layer off).
        // .w = 100+mode, .y=white, .z=k, color.rgb=black  (see gbuffer.wgsl).
        m.cavity_dirt.enabled = false;
        m.cavity_dirt.amount = 0.0;
        m.cavity_dirt.roughness = live.white;
        m.cavity_dirt.metallic = live.curve_k.max(0.001);
        m.cavity_dirt.sharpness = 100.0 + live.curve.as_u32() as f32;
        m.cavity_dirt.color = Vec3::splat(live.black);
        self.cache.prim_buf_stale = true;
        // Live noise → material_by_id_mut above bumps material_generation.
        self.outliner.outliner_dirty = true;
        self.mark_dirty(DirtyFlags::RENDER);
        true
    }

    /// Same GPU live path for prim override graphs.
    pub(crate) fn apply_live_gpu_noise_prim(&mut self, prim_uid: u32) -> bool {
        use glam::Vec3;
        let live = match self.ui.node_graph.extract_live_gpu_noise() {
            Some(l) => l,
            None => return false,
        };
        let Some(idx) = self.prim_index_by_uid(prim_uid) else {
            return false;
        };
        self.write_prim(idx, |p| {
            if live.drives_albedo {
                p.material.albedo = Vec3::from_array(live.color0);
                p.material.albedo_map.tex_id = 0;
                p.material.albedo_map.strength = 0.0;
            }
            p.material.roughness = live.roughness0.clamp(0.0, 1.0);
            if live.drives_roughness {
                p.material.roughness_map.tex_id = 0;
                p.material.roughness_map.strength = 0.0;
            }
            p.material.noise_pattern.enabled = true;
            p.material.noise_pattern.amount = 1.0;
            p.material.noise_pattern.scale = live.scale;
            p.material.noise_pattern.octaves = live.octaves;
            p.material.noise_pattern.color = Vec3::from_array(live.color1);
            p.material.noise_pattern.roughness = live.roughness1.clamp(0.0, 1.0);
            p.material.noise_pattern.metallic = p.material.metallic;
            p.material.edge_wear.enabled = false;
            p.material.edge_wear.amount = 0.0;
            p.material.edge_wear.sharpness = live.seed;
            p.material.cavity_dirt.enabled = false;
            p.material.cavity_dirt.amount = 0.0;
            p.material.cavity_dirt.roughness = live.white;
            p.material.cavity_dirt.metallic = live.curve_k.max(0.001);
            p.material.cavity_dirt.sharpness = 100.0 + live.curve.as_u32() as f32;
            p.material.cavity_dirt.color = Vec3::splat(live.black);
        });
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        true
    }
}
