// core_engine/app/src/properties_panel.rs
//
// Gate 5 — Properties panel (mockup `.prop-rail` + pages).
// Object page: live transform for selection (poly mesh free TRS; SDF body free
// instance TRS; SDF prim free pos/rot/scale).
// Modifiers page: real body field/mesh stacks + prim modifier stack (no longer a stub).

use eframe::egui;
use egui_phosphor::regular as ph;
use glam::Vec3;
use crate::ui_fields as f;
use crate::ui_theme as t;
use crate::{DirtyFlags, FourthLayerApp};

/// Which page of the Properties rail is active (egui temp memory).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PropsPage { Object, Modifiers, Sdf, Material }

const PAGE_ID: &str = "ui2_props_page";

fn load_page(ui: &egui::Ui) -> PropsPage {
    match ui.data(|d| d.get_temp::<u8>(egui::Id::new(PAGE_ID))).unwrap_or(0) {
        1 => PropsPage::Modifiers,
        2 => PropsPage::Sdf,
        3 => PropsPage::Material,
        _ => PropsPage::Object,
    }
}
fn store_page(ui: &mut egui::Ui, p: PropsPage) {
    let v = match p {
        PropsPage::Object => 0u8,
        PropsPage::Modifiers => 1,
        PropsPage::Sdf => 2,
        PropsPage::Material => 3,
    };
    ui.data_mut(|d| d.insert_temp(egui::Id::new(PAGE_ID), v));
}

/// Draw the full Properties body (rail + page). Cap-only — never grow the dock.
pub fn draw(ui: &mut egui::Ui) {
    // Stub entry (no app) — Object page shows empty/hint only.
    draw_with_app(ui, None);
}

impl FourthLayerApp {
    // ── shared undo helpers (Object + SDF pages) ────────────────────────────
    //
    // Change detection here is value-diff. (`vec3_row` DOES return RowSignals
    // with drag lifecycle since the D7 fix — Tool Params uses them; this page's
    // first-changed-frame episode logic predates that and stays valid as-is.)
    // Undo follows the v1 epilogue pattern (properties.rs:84) adapted to that:
    //  · transforms → an episode begun on the FIRST changed frame (the graph
    //    still holds pre-drag state at that point) and committed by
    //    `auto_commit_episodes` on pointer release — ONE undo entry per drag;
    //    a typed edit commits the same frame (pointer already up).
    //  · one-shot meta (visible / locked / name) → `transaction()`.

    /// Begin (once) the props-panel transform episode. MUST run BEFORE the
    /// frame's graph/flat-cache write — the snapshot is the pre-drag state.
    fn props_begin_xform_undo(&mut self, desc: &str) {
        if !self.has_active_episode() {
            self.begin_episode(desc);
        }
    }

    /// Commit node-level meta edits (visible / locked / name) for a poly or SDF
    /// body node as ONE transaction, with the canonical flag set of the outliner
    /// eye toggle (outliner_panel.rs:60): visibility is VIEW state → RENDER +
    /// prim_buf_stale + owning-body vol dirty — never SCENE. Lock/name need no
    /// render flags (`transaction()` itself marks the outliner).
    fn props_node_meta_commit(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        vis: bool,
        vis0: bool,
        lock: bool,
        lock0: bool,
        new_name: Option<String>,
    ) {
        let vis_changed = vis != vis0;
        let desc = if vis_changed {
            "Toggle visibility"
        } else if lock != lock0 {
            "Toggle lock"
        } else if new_name.is_some() {
            "Rename"
        } else {
            return;
        };
        self.transaction(desc, |s| {
            if let Some(n) = s.scene.scene_graph.get_mut(nid) {
                n.visible = vis;
                n.locked = lock;
                if let Some(nm) = new_name {
                    match &mut n.payload {
                        core_math::scene_graph::NodePayload::PolygonalBody {
                            name, ..
                        }
                        | core_math::scene_graph::NodePayload::SdfBody { name, .. }
                        | core_math::scene_graph::NodePayload::VoxelBody { name, .. } => {
                            *name = nm;
                        }
                        _ => {}
                    }
                }
            }
            if vis_changed {
                s.mark_dirty(DirtyFlags::RENDER);
                s.mark_node_body_vol_dirty(nid);
                s.cache.prim_buf_stale = true;
            }
        });
    }

    /// Commit prim meta edits (visible / locked / name) as ONE transaction.
    /// `write_prim`'s mirror already handles the data side of a visibility flip —
    /// vis participates in the boolean chain, so the mirror flags it structural
    /// and sets prim_buf_stale + a FULL body bake itself (prim_actions.rs:520).
    /// Only RENDER is marked here; NEVER SCENE (DirtyFlags discipline).
    fn props_prim_meta_commit(
        &mut self,
        idx: usize,
        vis: bool,
        vis0: bool,
        lock: bool,
        lock0: bool,
        new_name: Option<String>,
    ) {
        let vis_changed = vis != vis0;
        let desc = if vis_changed {
            "Toggle visibility"
        } else if lock != lock0 {
            "Toggle lock"
        } else if new_name.is_some() {
            "Rename"
        } else {
            return;
        };
        self.transaction(desc, |s| {
            s.write_prim(idx, |p| {
                p.visible = vis;
                p.locked = lock;
                if let Some(nm) = new_name {
                    p.name = nm;
                }
            });
            if vis_changed {
                s.mark_dirty(DirtyFlags::RENDER);
            }
        });
    }

    pub(crate) fn ui2_draw_properties(&mut self, ui: &mut egui::Ui) {
        // Safety: reborrow as &mut through a short-lived scope in draw_with_app.
        // We pass self via raw pointer pattern avoided — use internal helper that takes &mut Self.
        let cap = ui.available_width();
        ui.set_max_width(cap);
        ui.set_min_width(cap);
        ui.set_min_height(ui.available_height());

        let cur = load_page(ui);
        let mut clicked: Option<PropsPage> = None;

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            let rail_w = 36.0;
            egui::Frame::none().fill(t::SURFACE_TOOLCOL).show(ui, |ui| {
                ui.set_width(rail_w);
                ui.set_min_height(ui.available_height());
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    if rail_btn(ui, cur, PropsPage::Object, ph::CUBE, t::ACCENT_LIGHT, "Object") {
                        clicked = Some(PropsPage::Object);
                    }
                    if rail_btn(
                        ui,
                        cur,
                        PropsPage::Modifiers,
                        ph::WRENCH,
                        t::SCULPT_ORANGE,
                        "Modifiers",
                    ) {
                        clicked = Some(PropsPage::Modifiers);
                    }
                    if rail_btn(ui, cur, PropsPage::Sdf, ph::CIRCLE_HALF, t::SDF, "SDF") {
                        clicked = Some(PropsPage::Sdf);
                    }
                    if rail_btn(
                        ui,
                        cur,
                        PropsPage::Material,
                        ph::PAINT_BRUSH,
                        t::MESH,
                        "Material",
                    ) {
                        clicked = Some(PropsPage::Material);
                    }
                });
            });

            let body = ui.available_rect_before_wrap();
            ui.painter()
                .vline(body.left(), body.y_range(), egui::Stroke::new(1.0, t::SEAM));
            ui.add_space(1.0);

            ui.vertical(|ui| {
                let body_w = ui.available_width();
                let body_h = ui.available_height();
                ui.set_min_width(body_w);
                ui.set_max_width(body_w);
                ui.set_min_height(body_h);
                ui.set_max_height(body_h);
                if cur == PropsPage::Material {
                    // P1 material panel manages its own scroll — the header
                    // stays pinned (spec §2.1), so no shared ScrollArea here.
                    self.props_material_page_v2(ui);
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("v2_props_scroll")
                    .auto_shrink([false, false])
                    .max_height(body_h)
                    .show(ui, |ui| {
                        ui.set_min_width(body_w);
                        ui.set_max_width(body_w);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add_space(4.0);
                        match cur {
                            PropsPage::Object => self.props_object_page(ui),
                            PropsPage::Modifiers => self.props_modifiers_page(ui),
                            PropsPage::Sdf => self.props_sdf_page(ui),
                            PropsPage::Material => unreachable!("handled above"),
                        }
                    });
            });
        });

        if let Some(p) = clicked {
            store_page(ui, p);
        }
    }

    /// Object page: free per-axis Position / Rotation / Scale for the active selection.
    fn props_object_page(&mut self, ui: &mut egui::Ui) {
        // Resolve what we're editing.
        enum Target {
            Poly(core_math::scene_graph::NodeId),
            SdfBody(core_math::scene_graph::NodeId),
            /// Voxel sculpt body — was falling into None (empty Properties).
            Voxel(core_math::scene_graph::NodeId),
            Prim(usize),
            None,
        }
        // LIGHTS (fix 2026-07-28: "when I select a light I do not see any properties").
        // The Object page's target match handled PolygonalBody / SdfBody and let a Light fall into
        // `_ => Target::None`, so selecting the Sun showed the "Select a mesh, SDF body, or
        // primitive" empty state — even though a full light panel exists and is reachable from the
        // OLD properties path (`properties.rs:77`). The v2 shell simply never dispatched to it.
        //
        // `draw_props_light` is self-gating (it resolves the active light itself and returns when
        // there is none), so calling it unconditionally here is safe and adds nothing for a
        // non-light selection. Returning early keeps a light from also rendering the transform
        // editor below, whose Target resolution has no meaning for a light.
        // A LIGHT IS A GRAPH NODE, not a `SceneItemId::Light` (fix 2026-07-28 — the earlier
        // attempt at 0a68b44 gated on `active_light()` and never fired).
        //
        // Instrumenting settled what three rounds of code reading could not: clicking Sun.001
        // logs `active=Some(Node(NodeId(1)))`, never `Light(..)`. Lights live in the scene graph
        // as `NodePayload::Light`, so the outliner selects them the same way it selects any node.
        // `SceneItemId::Light` exists and `outliner_draw_light.rs` does use it, but that is a
        // different selection route than the one the owner's click takes.
        //
        // Detect by PAYLOAD instead — that cannot disagree with how the node was selected.
        // `draw_props_light` is still self-gating internally, so this only decides which panel to
        // show, and returns early so a light does not also render the transform editor.
        let is_light_node = self
            .sel
            .usel
            .active_node()
            .and_then(|nid| self.scene.scene_graph.get(nid))
            .is_some_and(|n| matches!(n.payload, core_math::scene_graph::NodePayload::Light(_)));
        if is_light_node || self.sel.usel.active_light().is_some() {
            self.draw_props_light(ui);
            return;
        }

        let target = if let Some(nid) = self.sel.usel.active_node() {
            match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
                Some(core_math::scene_graph::NodePayload::PolygonalBody { .. }) => {
                    Target::Poly(nid)
                }
                Some(core_math::scene_graph::NodePayload::SdfBody { .. }) => {
                    Target::SdfBody(nid)
                }
                Some(core_math::scene_graph::NodePayload::VoxelBody { .. }) => {
                    Target::Voxel(nid)
                }
                _ => Target::None,
            }
        } else if let Some(idx) = self.active_prim_index_unified() {
            if idx < self.scene.primitives.len() {
                Target::Prim(idx)
            } else {
                Target::None
            }
        } else {
            Target::None
        };

        match target {
            Target::None => {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Select a mesh, SDF body, voxel, or primitive")
                            .size(11.0)
                            .color(t::TEXT_FAINT),
                    );
                });
            }
            Target::Poly(nid) => self.props_object_poly(ui, nid),
            Target::SdfBody(nid) => self.props_object_sdf_body(ui, nid),
            Target::Voxel(nid) => self.props_object_voxel(ui, nid),
            Target::Prim(idx) => self.props_object_prim(ui, idx),
        }
    }

    /// Object page for a VoxelBody node: name, transform, visibility, resolution / voxel count.
    /// Res buttons reuse the same `voxel_resample_to` / remesh popup path as the sculpt pie.
    fn props_object_voxel(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        let snap = self.scene.scene_graph.get(nid).map(|n| {
            let name = match &n.payload {
                core_math::scene_graph::NodePayload::VoxelBody { name, .. } => name.clone(),
                _ => "Voxel".into(),
            };
            (n.local_position, n.local_rotation, n.local_scale, n.visible, n.locked, name)
        });
        let Some((pos0, rot0, scl0, vis0, lock0, name0)) = snap else {
            return;
        };

        // Live grid stats from sculpt session when this body is active; else graph payload grid.
        let (res, bounds, tris, solid_hint) = if let Some(v) = self.sculpt.voxel.as_ref() {
            if v.node == nid {
                let res = v.grid.res;
                let solid = (res as u64).saturating_pow(3);
                (res, v.grid.bounds, v.tris, solid)
            } else {
                self.scene
                    .scene_graph
                    .get(nid)
                    .and_then(|n| match &n.payload {
                        core_math::scene_graph::NodePayload::VoxelBody { grid, .. } => {
                            let res = grid.res;
                            Some((res, grid.bounds, 0u32, (res as u64).saturating_pow(3)))
                        }
                        _ => None,
                    })
                    .unwrap_or((0, 0.0, 0, 0))
            }
        } else {
            self.scene
                .scene_graph
                .get(nid)
                .and_then(|n| match &n.payload {
                    core_math::scene_graph::NodePayload::VoxelBody { grid, .. } => {
                        let res = grid.res;
                        Some((res, grid.bounds, 0u32, (res as u64).saturating_pow(3)))
                    }
                    _ => None,
                })
                .unwrap_or((0, 0.0, 0, 0))
        };

        let mut pos = [pos0.x, pos0.y, pos0.z];
        let mut rot = [rot0.x, rot0.y, rot0.z];
        let mut scl = [scl0.x, scl0.y, scl0.z];
        let mut vis = vis0;
        let mut lock = lock0;
        let mut name = name0.clone();
        let mut xform = false;
        let mut meta = false;
        let mut do_reset = false;
        let mut remesh_to: Option<u32> = None;
        let mut open_remesh = false;

        f::section(ui, "props/obj/transform", "Transform", Some("world"), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Free per-axis  ·  voxel body")
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Reset")
                        .on_hover_text("Identity transform (keeps voxel grid)")
                        .clicked()
                    {
                        do_reset = true;
                    }
                });
            });
            f::vec3_row(ui, "Position", &mut pos);
            f::vec3_row(ui, "Rotation", &mut rot);
            f::vec3_row(ui, "Scale", &mut scl);
            xform = (pos[0] - pos0.x).abs()
                + (pos[1] - pos0.y).abs()
                + (pos[2] - pos0.z).abs()
                + (rot[0] - rot0.x).abs()
                + (rot[1] - rot0.y).abs()
                + (rot[2] - rot0.z).abs()
                + (scl[0] - scl0.x).abs()
                + (scl[1] - scl0.y).abs()
                + (scl[2] - scl0.z).abs()
                > 1e-6;
        });

        f::section(ui, "props/obj/voxel", "Voxel", None, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("Resolution  {res}³"))
                        .size(11.5)
                        .color(t::TEXT),
                );
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Grid cells  {solid_hint}  ·  bounds ±{bounds:.2}"
                    ))
                    .size(10.5)
                    .color(t::TEXT_DIM),
                );
            });
            if tris > 0 {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("Surface triangles  {tris}"))
                            .size(10.5)
                            .color(t::TEXT_DIM),
                    );
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Res")
                        .size(11.0)
                        .color(t::TEXT_DIM),
                );
                ui.add_space(4.0);
                // Same presets as remesh popup / sculpt res pie.
                for &r in &[64u32, 128, 256, 512, 1024] {
                    let selected = res == r;
                    let resp = ui.selectable_label(selected, format!("{r}"));
                    if resp.clicked() && r != res {
                        remesh_to = Some(r);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui
                    .button("Remesh…")
                    .on_hover_text("Open remesh dialog (Ctrl+R) — same as sculpt res pie")
                    .clicked()
                {
                    open_remesh = true;
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Higher res = smoother detail · Ctrl+Z restores previous")
                        .size(10.0)
                        .color(t::TEXT_FAINT)
                        .italics(),
                );
            });
        });

        f::section(ui, "props/obj/display", "Display", None, |ui| {
            if f::bool_row(ui, "Visible", &mut vis).changed() {
                meta = true;
            }
            if f::bool_row(ui, "Locked", &mut lock).changed() {
                meta = true;
            }
        });

        f::section(ui, "props/obj/name", "Identity", None, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Name").size(11.0).color(t::TEXT_DIM));
                ui.add_space(6.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .desired_width((ui.available_width() - 8.0).max(40.0))
                        .font(egui::TextStyle::Body),
                );
                if r.changed() {
                    meta = true;
                }
            });
        });

        if do_reset {
            pos = [0.0, 0.0, 0.0];
            rot = [0.0, 0.0, 0.0];
            scl = [1.0, 1.0, 1.0];
            xform = true;
        }
        if xform {
            self.props_begin_xform_undo("Transform");
            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                n.local_position = Vec3::new(pos[0], pos[1], pos[2]);
                n.local_rotation = Vec3::new(rot[0], rot[1], rot[2]);
                n.local_scale = Vec3::new(
                    scl[0].max(0.0001),
                    scl[1].max(0.0001),
                    scl[2].max(0.0001),
                );
            }
            self.mark_transform_dirty();
        }
        if meta {
            let renamed = name != name0;
            // Extend rename arm to VoxelBody (props_node_meta_commit historically only
            // handled Poly/SdfBody names).
            let vis_changed = vis != vis0;
            let lock_changed = lock != lock0;
            if vis_changed || lock_changed || renamed {
                let desc = if vis_changed {
                    "Toggle visibility"
                } else if lock_changed {
                    "Toggle lock"
                } else {
                    "Rename"
                };
                let nm = if renamed { Some(name.clone()) } else { None };
                self.transaction(desc, |s| {
                    if let Some(n) = s.scene.scene_graph.get_mut(nid) {
                        n.visible = vis;
                        n.locked = lock;
                        if let Some(new_name) = nm {
                            if let core_math::scene_graph::NodePayload::VoxelBody {
                                name, ..
                            } = &mut n.payload
                            {
                                *name = new_name;
                            }
                        }
                    }
                    if vis_changed {
                        s.mark_dirty(DirtyFlags::RENDER);
                        s.mark_node_body_vol_dirty(nid);
                        s.cache.prim_buf_stale = true;
                    }
                });
            }
        }
        // Resample / remesh after UI borrow ends — must be active voxel for live path.
        if open_remesh {
            // Activate this voxel body if needed so remesh targets it.
            if self.sculpt.voxel.as_ref().map(|v| v.node) != Some(nid) {
                self.toast("Enter Sculpt mode on this body to remesh");
            } else {
                self.voxel_open_remesh();
            }
        }
        if let Some(r) = remesh_to {
            if self.sculpt.voxel.as_ref().map(|v| v.node) != Some(nid) {
                self.toast("Enter Sculpt mode on this body to change resolution");
            } else {
                self.voxel_resample_to(r);
            }
        }
    }

    fn props_object_poly(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        let snap = self.scene.scene_graph.get(nid).map(|n| {
            (
                n.local_position,
                n.local_rotation,
                n.local_scale,
                n.visible,
                n.locked,
                match &n.payload {
                    core_math::scene_graph::NodePayload::PolygonalBody { name, .. } => {
                        name.clone()
                    }
                    _ => "Mesh".into(),
                },
            )
        });
        let Some((pos0, rot0, scl0, vis0, lock0, name0)) = snap else {
            return;
        };

        let mut pos = [pos0.x, pos0.y, pos0.z];
        let mut rot = [rot0.x, rot0.y, rot0.z];
        let mut scl = [scl0.x, scl0.y, scl0.z];
        let mut vis = vis0;
        let mut lock = lock0;
        let mut name = name0.clone();
        let mut xform = false;
        let mut meta = false;
        let mut do_reset = false;

        f::section(ui, "props/obj/transform", "Transform", Some("world"), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Free per-axis  ·  poly mesh")
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Reset")
                        .on_hover_text("Identity transform (keeps mesh geometry)")
                        .clicked()
                    {
                        do_reset = true;
                    }
                });
            });
            f::vec3_row(ui, "Position", &mut pos);
            f::vec3_row(ui, "Rotation", &mut rot);
            f::vec3_row(ui, "Scale", &mut scl);
            xform = (pos[0] - pos0.x).abs()
                + (pos[1] - pos0.y).abs()
                + (pos[2] - pos0.z).abs()
                + (rot[0] - rot0.x).abs()
                + (rot[1] - rot0.y).abs()
                + (rot[2] - rot0.z).abs()
                + (scl[0] - scl0.x).abs()
                + (scl[1] - scl0.y).abs()
                + (scl[2] - scl0.z).abs()
                > 1e-6;
        });

        let mut apply_poly = None;
        f::section(ui, "props/obj/display", "Display", None, |ui| {
            if f::bool_row(ui, "Visible", &mut vis).changed() {
                meta = true;
            }
            if f::bool_row(ui, "Locked", &mut lock).changed() {
                meta = true;
            }
            apply_poly = crate::apply_transform::draw_apply_menu(
                ui,
                crate::apply_transform::ApplyTargetKind::Poly,
            );
        });

        f::section(ui, "props/obj/name", "Identity", None, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Name").size(11.0).color(t::TEXT_DIM));
                ui.add_space(6.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .desired_width((ui.available_width() - 8.0).max(40.0))
                        .font(egui::TextStyle::Body),
                );
                if r.changed() {
                    meta = true;
                }
            });
        });

        if do_reset {
            pos = [0.0, 0.0, 0.0];
            rot = [0.0, 0.0, 0.0];
            scl = [1.0, 1.0, 1.0];
            xform = true;
        }
        if xform {
            // Undo: episode starts BEFORE the graph write (pre-drag snapshot);
            // auto_commit_episodes commits on release — one entry per drag.
            self.props_begin_xform_undo("Transform");
            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                n.local_position = Vec3::new(pos[0], pos[1], pos[2]);
                n.local_rotation = Vec3::new(rot[0], rot[1], rot[2]);
                n.local_scale = Vec3::new(
                    scl[0].max(0.0001),
                    scl[1].max(0.0001),
                    scl[2].max(0.0001),
                );
            }
            self.mark_transform_dirty();
        }
        if meta {
            let renamed = name != name0;
            self.props_node_meta_commit(nid, vis, vis0, lock, lock0, renamed.then_some(name));
        }
        if let Some(action) = apply_poly {
            self.apply_menu_poly(nid, action);
        }
    }

    fn props_object_sdf_body(&mut self, ui: &mut egui::Ui, nid: core_math::scene_graph::NodeId) {
        // Free body-level instance transform (origin / rotation / scale) — does not
        // rebake the SDF field when BODY_LOCAL_PRIMS (field is body-local).
        let snap = self.scene.scene_graph.get(nid).and_then(|n| {
            if let core_math::scene_graph::NodePayload::SdfBody {
                origin,
                body_rotation,
                body_scale,
                name,
                ..
            } = &n.payload
            {
                let (rx, ry, rz) = body_rotation.to_euler(glam::EulerRot::YXZ);
                Some((
                    *origin,
                    [
                        rx.to_degrees(),
                        ry.to_degrees(),
                        rz.to_degrees(),
                    ],
                    *body_scale,
                    n.visible,
                    n.locked,
                    name.clone(),
                ))
            } else {
                None
            }
        });
        let Some((origin0, rot_deg0, scale0, vis0, lock0, name0)) = snap else {
            return;
        };
        // to_euler YXZ → (yaw, pitch, roll). Display X=pitch, Y=yaw, Z=roll (prim convention).
        let rot0 = [rot_deg0[1], rot_deg0[0], rot_deg0[2]];
        let mut pos = [origin0.x, origin0.y, origin0.z];
        let mut rot = rot0;
        let mut scl = [scale0.x, scale0.y, scale0.z];
        let mut vis = vis0;
        let mut lock = lock0;
        let mut name = name0.clone();
        let mut xform = false;
        let mut meta = false;
        let mut do_reset = false;

        f::section(ui, "props/obj/transform", "Transform", Some("world"), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Free body transform  ·  SDF instance (no field rebake)")
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reset").clicked() {
                        do_reset = true;
                    }
                });
            });
            f::vec3_row(ui, "Position", &mut pos);
            f::vec3_row(ui, "Rotation", &mut rot);
            f::vec3_row(ui, "Scale", &mut scl);
        });
        xform = (pos[0] - origin0.x).abs()
            + (pos[1] - origin0.y).abs()
            + (pos[2] - origin0.z).abs()
            + (rot[0] - rot0[0]).abs()
            + (rot[1] - rot0[1]).abs()
            + (rot[2] - rot0[2]).abs()
            + (scl[0] - scale0.x).abs()
            + (scl[1] - scale0.y).abs()
            + (scl[2] - scale0.z).abs()
            > 1e-5;

        let mut apply_body = None;
        f::section(ui, "props/obj/display", "Display", None, |ui| {
            if f::bool_row(ui, "Visible", &mut vis).changed() {
                meta = true;
            }
            if f::bool_row(ui, "Locked", &mut lock).changed() {
                meta = true;
            }
            apply_body = crate::apply_transform::draw_apply_menu(
                ui,
                crate::apply_transform::ApplyTargetKind::SdfBody,
            );
        });
        f::section(ui, "props/obj/name", "Identity", None, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Name").size(11.0).color(t::TEXT_DIM));
                ui.add_space(6.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut name)
                            .desired_width((ui.available_width() - 8.0).max(40.0)),
                    )
                    .changed()
                {
                    meta = true;
                }
            });
        });

        if do_reset {
            pos = [0.0, 0.0, 0.0];
            rot = [0.0, 0.0, 0.0];
            scl = [1.0, 1.0, 1.0];
            xform = true;
        }
        if xform {
            // Undo: episode starts BEFORE the graph write (pre-drag snapshot);
            // auto_commit_episodes commits on release — one entry per drag.
            self.props_begin_xform_undo("Transform");
            if let Some(n) = self.scene.scene_graph.get_mut(nid) {
                n.local_position = Vec3::new(pos[0], pos[1], pos[2]);
                n.local_rotation = Vec3::new(rot[0], rot[1], rot[2]);
                n.local_scale = Vec3::new(
                    scl[0].max(0.0001),
                    scl[1].max(0.0001),
                    scl[2].max(0.0001),
                );
                if let core_math::scene_graph::NodePayload::SdfBody {
                    origin,
                    body_rotation,
                    body_scale,
                    ..
                } = &mut n.payload
                {
                    *origin = Vec3::new(pos[0], pos[1], pos[2]);
                    // rot is X=pitch, Y=yaw, Z=roll → YXZ euler
                    *body_rotation = glam::Quat::from_euler(
                        glam::EulerRot::YXZ,
                        rot[1].to_radians(),
                        rot[0].to_radians(),
                        rot[2].to_radians(),
                    );
                    *body_scale = Vec3::new(
                        scl[0].max(0.0001),
                        scl[1].max(0.0001),
                        scl[2].max(0.0001),
                    );
                }
            }
            // Body-local field: instance transform only — no volume rebake.
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if meta {
            let renamed = name != name0;
            self.props_node_meta_commit(nid, vis, vis0, lock, lock0, renamed.then_some(name));
        }
        if let Some(action) = apply_body {
            self.apply_menu_sdf_body(nid, action);
        }

        // Nested sub-body: Boolean + Blend K (how it folds into the parent body).
        // Top-level bodies stay free-instance transform only (always Union into the scene).
        if self.is_nested_sdf_body(nid) {
            ui.add_space(6.0);
            f::section(ui, "props/obj/subbody_bool", "Sub-body Boolean", Some("blend"), |ui| {
                let _ = self.draw_subbody_boolean_params(ui, nid);
            });
        }
    }

    fn props_object_prim(&mut self, ui: &mut egui::Ui, idx: usize) {
        if idx >= self.scene.primitives.len() {
            return;
        }
        let p0 = &self.scene.primitives[idx];
        let mut pos = [p0.position.x, p0.position.y, p0.position.z];
        let mut rot = [p0.rotation_deg.x, p0.rotation_deg.y, p0.rotation_deg.z];
        let mut scl = [p0.scale.x, p0.scale.y, p0.scale.z];
        let (vis0, lock0) = (p0.visible, p0.locked);
        let mut vis = vis0;
        let mut lock = lock0;
        let mut name = p0.name.clone();
        let mut xform = false;
        let mut meta = false;
        let mut do_reset = false;

        f::section(ui, "props/obj/transform", "Transform", Some("local"), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Free per-axis  ·  SDF prim (non-uniform scale ok; prefer shape size for Box)",
                    )
                    .size(10.0)
                    .color(t::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reset").clicked() {
                        do_reset = true;
                    }
                });
            });
            f::vec3_row(ui, "Position", &mut pos);
            f::vec3_row(ui, "Rotation", &mut rot);
            f::vec3_row(ui, "Scale", &mut scl);
        });
        {
            let p = &self.scene.primitives[idx];
            xform = (pos[0] - p.position.x).abs()
                + (pos[1] - p.position.y).abs()
                + (pos[2] - p.position.z).abs()
                + (rot[0] - p.rotation_deg.x).abs()
                + (rot[1] - p.rotation_deg.y).abs()
                + (rot[2] - p.rotation_deg.z).abs()
                + (scl[0] - p.scale.x).abs()
                + (scl[1] - p.scale.y).abs()
                + (scl[2] - p.scale.z).abs()
                > 1e-6;
        }

        let mut apply_prim = None;
        f::section(ui, "props/obj/display", "Display", None, |ui| {
            if f::bool_row(ui, "Visible", &mut vis).changed() {
                meta = true;
            }
            if f::bool_row(ui, "Locked", &mut lock).changed() {
                meta = true;
            }
            apply_prim = crate::apply_transform::draw_apply_menu(
                ui,
                crate::apply_transform::ApplyTargetKind::SdfPrim,
            );
        });
        f::section(ui, "props/obj/name", "Identity", None, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Name").size(11.0).color(t::TEXT_DIM));
                ui.add_space(6.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut name)
                            .desired_width((ui.available_width() - 8.0).max(40.0)),
                    )
                    .changed()
                {
                    meta = true;
                }
            });
        });

        if do_reset {
            // Keep position; identity rot/scale.
            rot = [0.0, 0.0, 0.0];
            scl = [1.0, 1.0, 1.0];
            xform = true;
        }
        if xform {
            let force_field = (scl[0] - self.scene.primitives[idx].scale.x).abs()
                + (scl[1] - self.scene.primitives[idx].scale.y).abs()
                + (scl[2] - self.scene.primitives[idx].scale.z).abs()
                + (rot[0] - self.scene.primitives[idx].rotation_deg.x).abs()
                + (rot[1] - self.scene.primitives[idx].rotation_deg.y).abs()
                + (rot[2] - self.scene.primitives[idx].rotation_deg.z).abs()
                > 1e-6;
            // Undo: episode starts BEFORE write_prim_transform_ex (pre-drag
            // snapshot); auto_commit_episodes commits on release — one entry
            // per drag. write_prim_transform_ex itself creates no undo entry.
            self.props_begin_xform_undo("Transform");
            self.write_prim_transform_ex(
                idx,
                Vec3::new(pos[0], pos[1], pos[2]),
                Vec3::new(rot[0], rot[1], rot[2]),
                Vec3::new(
                    scl[0].max(0.0001),
                    scl[1].max(0.0001),
                    scl[2].max(0.0001),
                ),
                force_field,
            );
            if force_field {
                self.rebake_no_undo();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if meta {
            // vis/lock/name are meta, never structural: SCENE is forbidden here
            // (the mirror inside write_prim handles the bake side of a vis flip).
            self.props_prim_meta_commit(idx, vis, vis0, lock, lock0, Some(name));
        }
        if let Some(action) = apply_prim {
            self.apply_menu_sdf_prim(idx, action);
        }
    }
}

fn draw_with_app(ui: &mut egui::Ui, _app: Option<&mut FourthLayerApp>) {
    // Fallback layout when no app (token board / demos).
    let cap = ui.available_width();
    ui.set_max_width(cap);
    ui.set_min_width(cap);
    ui.label(
        egui::RichText::new("Properties need live app context")
            .size(11.0)
            .color(t::TEXT_FAINT),
    );
}

fn rail_btn(
    ui: &mut egui::Ui,
    cur: PropsPage,
    target: PropsPage,
    icon: &str,
    col: egui::Color32,
    tip: &str,
) -> bool {
    let on = cur == target;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
    if on {
        ui.painter().rect_filled(rect.shrink(2.0), 5.0, t::accent_tint(46));
        ui.painter().rect_stroke(rect.shrink(2.0), 5.0, egui::Stroke::new(1.0, t::accent_tint(120)), egui::StrokeKind::Middle);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect.shrink(2.0), 5.0, t::SURFACE_HEADER);
    }
    let ic = if on { col } else if resp.hovered() { t::TEXT_SECONDARY } else { t::TEXT_DIM };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        ic,
    );
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        resp.clone().on_hover_text(tip);
    }
    resp.clicked()
}

// ── pages (stub data, field kit) ─────────────────────────────────────────────

fn page_object(ui: &mut egui::Ui) {
    f::section(ui, "props/obj/transform", "Transform", None, |ui| {
        let mut pos = load_v3(ui, "props.obj.pos", [0.0, 0.0, 0.0]);
        let mut rot = load_v3(ui, "props.obj.rot", [0.0, 0.0, 0.0]);
        let mut scl = load_v3(ui, "props.obj.scl", [1.0, 1.0, 1.0]);
        f::vec3_row(ui, "Position", &mut pos);
        f::vec3_row(ui, "Rotation", &mut rot);
        f::vec3_row(ui, "Scale", &mut scl);
        store_v3(ui, "props.obj.pos", pos);
        store_v3(ui, "props.obj.rot", rot);
        store_v3(ui, "props.obj.scl", scl);
    });
    f::section(ui, "props/obj/display", "Display", None, |ui| {
        let mut vis = load_bool(ui, "props.obj.vis", true);
        let mut lock = load_bool(ui, "props.obj.lock", false);
        f::bool_row(ui, "Visible", &mut vis);
        f::bool_row(ui, "Locked", &mut lock);
        store_bool(ui, "props.obj.vis", vis);
        store_bool(ui, "props.obj.lock", lock);
    });
    f::section(ui, "props/obj/name", "Identity", None, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Name").size(11.0).color(t::TEXT_DIM));
            ui.add_space(8.0);
            let mut name = load_str(ui, "props.obj.name", "Body");
            let r = ui.add(egui::TextEdit::singleline(&mut name)
                .desired_width(ui.available_width().max(40.0))
                .font(egui::TextStyle::Body));
            if r.changed() { store_str(ui, "props.obj.name", name); }
        });
    });
}

// page_modifiers mockup replaced by FourthLayerApp::props_modifiers_page (real stacks).

impl FourthLayerApp {
    /// Properties → Modifiers: **one panel for all body types**.
    ///
    /// * **Poly body** — display-mesh stack (Subdivide / Mirror / Bevel / Array / …).
    /// * **SDF body** — field modifiers (volume bake) + post-DC mesh modifiers.
    /// * **SDF primitive** — elongation + prim stack (Mirror / Array / Twist / Shell / …).
    fn props_modifiers_page(&mut self, ui: &mut egui::Ui) {
        use core_math::scene_graph::NodePayload;

        // ── Resolve selection ───────────────────────────────────────────────
        let active = self.sel.usel.active_node();
        let poly_nid = active.filter(|nid| {
            matches!(
                self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                Some(NodePayload::PolygonalBody { .. })
            )
        });
        let sel_sdf_body = active.filter(|nid| {
            matches!(
                self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                Some(NodePayload::SdfBody { .. })
            )
        });
        let body_from_prim = self.active_prim_index_unified().and_then(|idx| {
            if idx >= self.scene.primitives.len() {
                return None;
            }
            let uid = self.scene.primitives[idx].prim_uid;
            self.scene.scene_graph.find_body_containing_prim(uid)
        });
        let sdf_body_nid = sel_sdf_body.or(body_from_prim);
        let prim_idx = self
            .active_prim_index_unified()
            .filter(|i| *i < self.scene.primitives.len());

        let has_anything =
            poly_nid.is_some() || sdf_body_nid.is_some() || prim_idx.is_some();
        if !has_anything {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("Select a mesh, SDF body, or primitive")
                        .size(11.0)
                        .color(t::TEXT_FAINT),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Poly: Subdivide / Mirror / Bevel / Array\n\
                         SDF body: field + display mesh modifiers\n\
                         Primitive: Mirror, Array, Twist, Shell, …",
                    )
                    .size(10.0)
                    .color(t::TEXT_FAINT),
                );
            });
            return;
        }

        // Banner: what kind of selection is driving this panel.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let kind = if poly_nid.is_some() {
                "POLY MESH"
            } else if sdf_body_nid.is_some() || prim_idx.is_some() {
                "SDF"
            } else {
                "MODIFIERS"
            };
            ui.label(
                egui::RichText::new(kind)
                    .size(9.5)
                    .color(t::TEXT_SECTION)
                    .strong(),
            );
        });
        ui.add_space(4.0);

        // ── POLY ────────────────────────────────────────────────────────────
        if let Some(nid) = poly_nid {
            self.draw_poly_modifier_stack(ui, nid, true);
            return; // poly-only panel when a poly body is active
        }

        // ── SDF OBJECT LEVEL ────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("OBJECT LEVEL")
                    .size(9.5)
                    .color(t::TEXT_SECTION)
                    .strong(),
            );
        });
        ui.add_space(2.0);
        if let Some(nid) = sdf_body_nid {
            self.seed_and_sync_body_field_stack(nid);
            self.draw_props_body_field_modifiers(ui, nid);
            self.draw_props_body_mesh_modifiers(ui, nid);
        } else {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("No SDF body in selection")
                        .size(11.0)
                        .color(t::TEXT_FAINT),
                );
            });
        }

        ui.add_space(10.0);
        // ── SDF PRIMITIVE LEVEL ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("PRIMITIVE LEVEL")
                    .size(9.5)
                    .color(t::TEXT_SECTION)
                    .strong(),
            );
        });
        ui.add_space(2.0);

        if let Some(idx) = prim_idx {
            f::section(ui, "props/mod/elongate", "Elongation", None, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Domain stretch along each axis (no cross-section squash)",
                        )
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                    );
                });
                let e = &mut self.scene.primitives[idx].elongation;
                let mut changed = false;
                let mut drag = false;
                for (v, lab) in [(&mut e.x, "X"), (&mut e.y, "Y"), (&mut e.z, "Z")] {
                    let r = ui.add(egui::Slider::new(v, 0.0..=5.0).text(lab));
                    if r.drag_started() {
                        drag = true;
                    }
                    if r.changed() {
                        changed = true;
                    }
                }
                if changed {
                    if drag && !self.has_active_episode() {
                        self.begin_episode("Elongation");
                    } else if !drag && !self.has_active_episode() {
                        self.transaction_snapshot_only("Elongation");
                    }
                    self.mirror_prim_to_graph(idx);
                    if self.is_master(idx) {
                        let uid = self.scene.primitives[idx].prim_uid;
                        self.sync_instances_of(uid);
                    }
                    self.rebake_no_undo();
                }
            });

            let mut pctx = crate::properties_sdf::PropsContext {
                changed: false,
                desc: String::new(),
                drag_started: false,
                new_bool_op: None,
                new_mat: None,
                idx: Some(idx),
                add_slice: false,
                slice_duplicate: false,
                new_shared_mat_from_prim: false,
                jump_to_material: None,
            };
            self.draw_props_prim_modifiers(ui, &mut pctx, idx, true);
            if pctx.changed {
                if pctx.drag_started && !self.has_active_episode() {
                    self.begin_episode(&pctx.desc);
                } else if !pctx.drag_started && !self.has_active_episode() {
                    self.transaction_snapshot_only(&pctx.desc);
                }
                self.mirror_prim_to_graph(idx);
                if self.is_master(idx) {
                    let uid = self.scene.primitives[idx].prim_uid;
                    self.sync_instances_of(uid);
                }
                self.rebake_no_undo();
            }
        } else {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Select a primitive for Mirror / Array / Twist / …")
                        .size(11.0)
                        .color(t::TEXT_FAINT),
                );
            });
        }
    }

    /// SDF page: Boolean (mockup) + real Resolution / Visibility / Performance
    /// for the selected SDF body or primitive.
    fn props_sdf_page(&mut self, ui: &mut egui::Ui) {
        // (Shape Radius / Blend K / Boolean removed — they live in the tool-parameters UI.)

        // Resolve the SDF context: selected SDF body node, else the active prim
        // (whose bake resolution lives on its top-level owning body).
        let sel_body = self.sel.usel.active_node().filter(|nid| {
            matches!(
                self.scene.scene_graph.get(*nid).map(|n| &n.payload),
                Some(core_math::scene_graph::NodePayload::SdfBody { .. })
            )
        });
        let prim_idx = if sel_body.is_none() {
            self.active_prim_index_unified()
                .filter(|i| *i < self.scene.primitives.len())
        } else {
            None
        };
        if sel_body.is_none() && prim_idx.is_none() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("Select an SDF body or primitive")
                        .size(11.0)
                        .color(t::TEXT_FAINT),
                );
            });
            return;
        }

        // ── BOOLEAN — selected prim's CSG op (was only in Tool Params; easy to miss).
        // First prim seeds the field (op ignored); later prims Union/Subtract/Intersect.
        // Collect mutations OUTSIDE the section closure so we never re-enter
        // set_prims_boolean_op / write_prim mid-draw (undo transaction + rebake).
        let mut pending_bool: Option<(u32, core_math::primitives::BooleanOp)> = None;
        let mut pending_blend_k: Option<(usize, f32)> = None;
        if let Some(idx) = prim_idx {
            let uid = self.scene.primitives[idx].prim_uid;
            let mut lab = {
                use core_math::primitives::BooleanOp;
                match &self.scene.primitives[idx].operation {
                    BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. } => "Union",
                    BooleanOp::Subtract { .. } | BooleanOp::ChamferSubtract { .. } => "Subtract",
                    BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. } => "Intersect",
                    BooleanOp::Seam { .. } | BooleanOp::Extrude { .. } => "Slice",
                }
                .to_owned()
            };
            let cur_k = self.scene.primitives[idx].operation.blend_k();
            f::section(ui, "props/sdf/boolean", "Boolean", Some("prim"), |ui| {
                let opts = ["Union", "Subtract", "Intersect", "Slice"];
                if f::enum_combo_id(ui, ("props_sdf_op", uid), "Operation", &mut lab, &opts) {
                    use core_math::primitives::BooleanOp;
                    let op = match lab.as_str() {
                        "Subtract" => BooleanOp::Subtract { k: cur_k },
                        "Intersect" => {
                            if cur_k > 0.0 {
                                BooleanOp::SmoothIntersect { k: cur_k }
                            } else {
                                BooleanOp::Intersect
                            }
                        }
                        "Slice" => BooleanOp::Seam {
                            width: cur_k.max(0.05),
                            depth: 0.02,
                            profile: 0,
                            raised: false,
                        },
                        _ => BooleanOp::Union { k: cur_k },
                    };
                    pending_bool = Some((uid, op));
                }
                let mut k = cur_k;
                let k_max = self.tweak.blend_k_max.max(0.1);
                let resp = f::num(ui, "Blend K", &mut k, 0.01, 0.0..=k_max, None);
                if resp.changed() {
                    pending_blend_k = Some((idx, k.clamp(0.0, k_max)));
                }
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "First prim seeds the solid (op ignored). Subtract needs a 2nd+ prim.",
                        )
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                    );
                });
            });
        }
        if let Some((uid, op)) = pending_bool {
            let _ = self.set_prims_boolean_op(&[(uid, op)]);
        }
        if let Some((idx, k)) = pending_blend_k {
            self.write_prim(idx, |p| p.operation.set_blend_k(k));
            self.rebake_no_undo();
        }

        // ── RESOLUTION — per-top-level-body volume bake resolution (same choices +
        // apply path as the legacy Grid picker in properties_render.rs).
        let bake_body = self
            .scene
            .scene_graph
            .top_level_body(sel_body.unwrap_or(self.scene.active_body_nid));
        let cur = if self.cache.use_body_vol_cache {
            self.cache
                .body_vol_cache
                .body_resolution(bake_body)
                .unwrap_or(self.render.voxel_cache_res)
        } else {
            self.render.voxel_cache_res
        };
        // Selectable ceiling mirrors the Grid picker / body_grid_res_state (S2.6: sparse ≥512
        // is structural on the per-body route; legacy global stays ≤512).
        let (_, sel_max) = self.body_grid_res_state(bake_body);
        let mut pick_res: Option<u32> = None;
        f::section(ui, "props/sdf/resolution", "Resolution", Some("body"), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                for res in [64u32, 128, 256, 512, 1024, 2048] {
                    let blocked = res > sel_max;
                    let resp = ui.add_enabled(
                        !blocked,
                        egui::SelectableLabel::new(cur == res, format!("{res}³")),
                    );
                    if blocked {
                        resp.on_hover_text(
                            "1024³/2048³ need the sparse brick pipeline (Developer Tweaks → Sparse bake).",
                        );
                    } else if res >= 2048 {
                        resp.clone().on_hover_text(
                            "2048³ sparse bricks (opt-in, 2048-virtual). ~4× the 1024³ pool VRAM; self-heals down a notch at the GPU buffer ceiling. 16-24GB GPU recommended.",
                        );
                        // Allow re-click same res to force rebake (stale lock / failed bake).
                        if resp.clicked() {
                            pick_res = Some(res);
                        }
                    } else if resp.clicked() {
                        pick_res = Some(res);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let hint = if self.cache.body_vol_cache.use_adaptive_resolution {
                    "Manual pick locks this body (Adaptive will not overwrite until Adaptive is toggled)."
                } else {
                    "Bake grid of the top-level body — change rebakes it"
                };
                ui.label(
                    egui::RichText::new(hint)
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
            });
        });
        if let Some(res) = pick_res {
            self.apply_body_grid_resolution(bake_body, res);
            self.toast(&format!("Bake resolution: {res}³"));
        }

        // ── VISIBILITY — real visible/locked flags of the selected body or prim.
        if let Some(nid) = sel_body {
            let snap = self.scene.scene_graph.get(nid).map(|n| (n.visible, n.locked));
            if let Some((vis0, lock0)) = snap {
                let mut vis = vis0;
                let mut lock = lock0;
                let mut meta = false;
                f::section(ui, "props/sdf/visibility", "Visibility", None, |ui| {
                    if f::bool_row(ui, "Visible", &mut vis).changed() {
                        meta = true;
                    }
                    if f::bool_row(ui, "Locked", &mut lock).changed() {
                        meta = true;
                    }
                });
                if meta {
                    self.props_node_meta_commit(nid, vis, vis0, lock, lock0, None);
                }
            }
        } else if let Some(idx) = prim_idx {
            let p0 = &self.scene.primitives[idx];
            let (vis0, lock0) = (p0.visible, p0.locked);
            let mut vis = vis0;
            let mut lock = lock0;
            let mut meta = false;
            f::section(ui, "props/sdf/visibility", "Visibility", None, |ui| {
                if f::bool_row(ui, "Visible", &mut vis).changed() {
                    meta = true;
                }
                if f::bool_row(ui, "Locked", &mut lock).changed() {
                    meta = true;
                }
            });
            if meta {
                // vis/lock are meta, never structural: RENDER only — the mirror
                // inside write_prim handles prim_buf_stale + the full body bake
                // of a vis flip (prim_actions.rs:520). SCENE is forbidden here.
                self.props_prim_meta_commit(idx, vis, vis0, lock, lock0, None);
            }
        }

        // ── PERFORMANCE — real SDF-bake perf settings (app-global).
        let mut adaptive = self.cache.body_vol_cache.use_adaptive_resolution;
        let mut adaptive_changed = false;
        f::section(ui, "props/sdf/perf", "Performance", Some("global"), |ui| {
            if f::bool_row(ui, "Adaptive Resolution", &mut adaptive).changed() {
                adaptive_changed = true;
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Bake resolution scales with body size (global)")
                        .size(10.0)
                        .color(t::TEXT_FAINT),
                );
            });
        });
        if adaptive_changed {
            self.cache
                .body_vol_cache
                .set_adaptive_resolution(adaptive, &self.scene.scene_graph);
            if self.cache.use_body_vol_cache {
                self.trigger_body_vol_bake();
            }
            self.mark_dirty(DirtyFlags::RENDER);
        }
    }
}

// page_material stub REPLACED by the P1 read-only panel:
// material_panel/view.rs — `props_material_page_v2` (MATERIAL_PANEL_SPEC.md §4-P1).

// ── egui temp helpers ────────────────────────────────────────────────────────

fn load_bool(ui: &egui::Ui, key: &str, def: bool) -> bool {
    ui.data(|d| d.get_temp::<bool>(egui::Id::new(key))).unwrap_or(def)
}
fn store_bool(ui: &mut egui::Ui, key: &str, v: bool) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new(key), v));
}
fn load_str(ui: &egui::Ui, key: &str, def: &str) -> String {
    ui.data(|d| d.get_temp::<String>(egui::Id::new(key))).unwrap_or_else(|| def.to_owned())
}
fn store_str(ui: &mut egui::Ui, key: &str, v: String) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new(key), v));
}
fn load_v3(ui: &egui::Ui, key: &str, def: [f32; 3]) -> [f32; 3] {
    ui.data(|d| d.get_temp::<[f32; 3]>(egui::Id::new(key))).unwrap_or(def)
}
fn store_v3(ui: &mut egui::Ui, key: &str, v: [f32; 3]) {
    ui.data_mut(|d| d.insert_temp(egui::Id::new(key), v));
}
