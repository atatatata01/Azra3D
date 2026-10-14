//! Voxel sculpt brush panel (pure-relocated from ui_panels.rs).

use eframe::egui;
use crate::{DirtyFlags, AzraApp, WorkMode};

impl AzraApp {
    // ============================================================
    // Material DNA helpers
    // ============================================================

    // dna helpers moved to properties.rs
    // status_line_hint moved to viewport_hints.rs — it is now a thin join over `viewport_hints()`,
    // THE single source of truth for the contextual hint row (active modal tool first, work-mode
    // fallback second).

    /// Floating "Sculpt Brush" panel (Phase 0.1 + 0.3). The 11 voxel brushes are wired (input_sculpt
    /// / voxel_sculpt) but had no proper picker — this is it: a brush GRID (over the `VoxelBrush` enum),
    /// radius / strength / lazy-mouse / symmetry sliders, the material editor that feeds the voxel
    /// display mesh's raster G-buffer (P0.3 — albedo / roughness / metallic), and a Remesh (Ctrl+R)
    /// button. Shown only in Sculpt mode with an active voxel object. Mutations to brush/tool state are
    /// transient (the sculpt subsystem owns its own GPU-authority + snapshot undo — CLAUDE.md), and the
    /// material edit is a one-shot scene mutation routed through `transaction()`.
    pub(crate) fn draw_voxel_sculpt_panel(&mut self, ctx: &egui::Context) {
        use core_math::voxel_sculpt::VoxelBrush;
        use core_math::scene_graph::NodePayload;
        if self.tool.work_mode != WorkMode::Sculpt || self.sculpt.voxel.is_none() { return; }

        // Deferred actions collected inside the &mut borrow, run after it drops (matches the strip in
        // workspace.rs). Material edits are deferred so the &mut v borrow doesn't alias the scene graph.
        let mut act_open_remesh = false;
        let mut act_remesh_to: Option<u32> = None;
        let mut mat_edit: Option<(glam::Vec3, f32, f32)> = None; // (albedo, roughness, metallic)
        let mut mat_reset_clay = false;
        let mut clay_shading_set: Option<bool> = None; // toggle cavity_dirt.enabled (the clay darkening)
        let mut fill_all = false; // "Fill All": paint every solid voxel with the brush paint colour (deferred — runs after the &mut v borrow ends)
        let mut paint_full_remesh = false; // Paint Color toggled ON → force a full CPU re-mesh so resident-GPU-only chunks get a colour-carrying CPU mesh
        let mut smooth_all = false; // "Smooth All": global Laplacian relax over the whole sculpt (deferred — one sculpt-undo step)
        let mut symmetrize_axis: Option<usize> = None; // "Symmetrize": deferred one-shot mirror across a chosen local axis (0=X/1=Y/2=Z)
        let mut voxelize_now = false; // "Voxelize Now": flush pending surface-mode Move grabs into the grid (deferred — runs after the &mut v borrow ends)
        let mut surface_mode_switch: Option<bool> = None; // Voxel|Surface segmented toggle (deferred — mode sync needs &mut self)
        let mut mask_remesh_region: Option<[u32; 6]> = None; // Clear/Invert/Show mask → remesh after &mut v ends
        let mut load_brush_tip = false; // deferred rfd multi-file tip import
        let mut load_brush_tip_folder = false;
        let mut load_brush_tip_blender = false;
        let mut install_proc_tips = false; // deferred procedural essentials install
        let mut tip_toast: Option<String> = None; // deferred toast (avoid borrow with &mut v)
        let mut tip_cache_invalidate = false;
        let mut save_brush_as: Option<String> = None;
        let mut load_brush_name: Option<String> = None;
        let mut delete_brush_name: Option<String> = None;
        // egui TextEdit for save name (tool-local; re-seeded empty when window opens)
        // Use a static-ish buffer on the voxel path via deferred only when button clicked with name.

        // Seed the material widgets from the active VoxelBody node (the live source the raster path
        // reads each frame — raster_render.rs `NodePayload::VoxelBody { material, .. }`).
        let node = self.sculpt.voxel.as_ref().map(|v| v.node);
        let (mut alb, mut rough, mut metal, clay_on) = node
            .and_then(|n| self.scene.scene_graph.get(n))
            .and_then(|n| if let NodePayload::VoxelBody { material, .. } = &n.payload {
                Some((material.albedo, material.roughness, material.metallic, material.cavity_dirt.enabled))
            } else { None })
            .unwrap_or((glam::Vec3::new(0.82, 0.45, 0.38), 0.70, 0.0, true));

        egui::Window::new("\u{1F58C} Sculpt Brush")
            .resizable(true)
            .default_size([300.0, 640.0])
            .min_size([240.0, 180.0])
            .default_pos([12.0, 90.0])
            .show(ctx, |ui| {
                // Scroll so a short resized window still reaches Direction / Material / Remesh.
                egui::ScrollArea::vertical()
                    .id_salt("sculpt_brush_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                // Stretch sliders with the window (resizable panel).
                ui.set_min_width(220.0);
                let slide_w = (ui.available_width() - 120.0).clamp(90.0, 320.0);
                ui.spacing_mut().slider_width = slide_w;
                if let Some(v) = self.sculpt.voxel.as_mut() {
                    // ---- Sculpting mode: Voxel | Surface (3D-Coat style) ----
                    // Voxel = brushes write the density field (free topology). Surface = brushes
                    // displace the existing mesh vertices (fixed topology, crisp fine detail);
                    // flipping back to Voxel revoxelizes the edited mesh into the field.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Mode").strong().small());
                        if ui.selectable_label(!v.surface_mode, "Voxel")
                            .on_hover_text("Voxel mode — brushes edit the density field; topology is free (default).")
                            .clicked() && v.surface_mode
                        {
                            surface_mode_switch = Some(false);
                        }
                        // Surface mode needs the mesh to BE the live display (≤512³; the >512³
                        // sparse stroke path raymarches the atlas — Display Invariant).
                        let surface_ok = v.grid.res <= 512;
                        let resp = ui.add_enabled(surface_ok, egui::SelectableLabel::new(v.surface_mode, "Surface"))
                            .on_hover_text(if surface_ok {
                                "Surface mode — brushes displace the mesh directly (fixed topology, \
                                 faster + crisper fine detail). Toggling back to Voxel re-voxelizes."
                            } else {
                                "Surface mode requires ≤512³ (mesh display); lower the resolution (Ctrl+R)."
                            });
                        if resp.clicked() && !v.surface_mode {
                            surface_mode_switch = Some(true);
                        }
                        if v.surface_mode && v.surface_dirty_region.is_some() {
                            ui.weak("mesh-edited").on_hover_text(
                                "Un-voxelized surface edits — voxelized automatically on mode switch / save / leave.");
                        }
                    });
                    ui.separator();
                    // ---- Brush menu: factory ops + saved user brushes (flat grid, no Load dropdown) ----
                    ui.label(egui::RichText::new("Brush").strong().small());
                    let brushes: [(VoxelBrush, &str, &str); 14] = [
                        (VoxelBrush::Draw,        "Draw",    "Deposit material along the surface normal"),
                        (VoxelBrush::Erase,       "Erase",   "Carve material away (or hold Ctrl with any brush)"),
                        (VoxelBrush::Smooth,      "Smooth",  "Relax toward the local average (also: hold Shift)"),
                        (VoxelBrush::Flatten,     "Flatten", "Plane polish — push surface to the contact plane (HPolish-style)"),
                        (VoxelBrush::Clay,        "Clay",    "Even buildup — fills cavities faster than ridges"),
                        (VoxelBrush::Scrape,      "Scrape",  "One-directional flatten — shave above the plane"),
                        (VoxelBrush::Crease,      "Crease",  "Dam Standard — sharp thin groove / panel line (Ctrl raises a ridge)"),
                        (VoxelBrush::Fill,        "Fill",    "Deposit only into cavities below the plane"),
                        (VoxelBrush::ClayBuildup, "Build",   "ZBrush Clay Buildup — layers volume along surface normal (flat strip tops)"),
                        (VoxelBrush::Inflate,     "Inflate", "Puff the surface outward along its normal (Ctrl deflates)"),
                        (VoxelBrush::Pinch,       "Pinch",   "Pull material toward the stroke to sharpen edges (Ctrl bulges)"),
                        (VoxelBrush::Move,        "Move",    "Grab/pull clay (mesh grab). Elasticity + Pull Strength. Ctrl = along normal. Voxelize on release when Dynamic Topology is ON."),
                        (VoxelBrush::Smudge,      "Smudge",  "Soft density taffy warp (same engine, soft falloff)."),
                        (VoxelBrush::SnakeHook,   "Snake",   "Snake Hook — pull the surface out into a tube/spike (SURFACE mode only). crease_pinch (Pinch Amount) narrows the tip."),
                    ];
                    let user_brush_id = egui::Id::new("active_user_brush_name");
                    let mut active_user = ui.ctx().data_mut(|d| {
                        d.get_temp::<String>(user_brush_id).unwrap_or_default()
                    });
                    let presets = crate::brush_library::list_brush_presets();
                    // Factory + saved share one grid so saved brushes appear in the brush menu.
                    let cols = 3usize;
                    let cell_w = 62.0;
                    let cell_h = 22.0;
                    egui::Grid::new("voxel_brush_grid").num_columns(cols).spacing([4.0, 4.0]).show(ui, |ui| {
                        let mut i = 0usize;
                        for (b, name, tip) in &brushes {
                            // Factory pick clears user-preset highlight.
                            let selected = active_user.is_empty() && v.brush == *b;
                            // Mode gating (documented fail-closed guard):
                            //  • Surface mode: density-native brushes (Fill/Smudge) have no honest
                            //    vertex analog → greyed.
                            //  • Voxel mode: Snake Hook is surface-only (no density version) → greyed.
                            let snake_voxel_only =
                                !v.surface_mode && matches!(*b, VoxelBrush::SnakeHook);
                            let usable = (v.surface_mode
                                && crate::voxel_surface_sculpt::surface_brush_supported(*b))
                                || (!v.surface_mode && !snake_voxel_only);
                            let resp = ui.add_enabled(usable,
                                egui::SelectableLabel::new(selected, *name));
                            let resp = if usable { resp.on_hover_text(*tip) }
                                else if snake_voxel_only {
                                    resp.on_disabled_hover_text(
                                        "Snake Hook is surface-mode only — switch to Surface mode to use it.")
                                } else {
                                    resp.on_disabled_hover_text(
                                        "Density-only brush — switch to Voxel mode to use it.")
                                };
                            if resp.clicked() {
                                v.set_active_brush(*b);
                                active_user.clear();
                            }
                            i += 1;
                            if i % cols == 0 { ui.end_row(); }
                        }
                        // Saved user brushes — same menu cells, not a ComboBox.
                        for n in &presets {
                            let selected = active_user == *n;
                            let short = if n.len() > 9 { format!("{}…", &n[..8]) } else { n.clone() };
                            if ui.add_sized([cell_w, cell_h], egui::SelectableLabel::new(selected, short))
                                .on_hover_text(format!(
                                    "Saved brush “{n}” — click to load full settings (appears in this menu)."
                                ))
                                .clicked()
                            {
                                load_brush_name = Some(n.clone());
                                active_user = n.clone();
                                // Seed Save/Del field with the slot name (once on pick).
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(egui::Id::new("brush_save_as_name"), n.clone());
                                });
                            }
                            i += 1;
                            if i % cols == 0 { ui.end_row(); }
                        }
                        if i % cols != 0 {
                            ui.end_row();
                        }
                    });
                    ui.ctx().data_mut(|d| d.insert_temp(user_brush_id, active_user.clone()));

                    // Save / delete under the menu (no Load dropdown).
                    ui.horizontal(|ui| {
                        let id = egui::Id::new("brush_save_as_name");
                        let mut name = ui.ctx().data_mut(|d| {
                            d.get_temp::<String>(id).unwrap_or_else(|| "My Brush".into())
                        });
                        ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .desired_width(90.0)
                                .hint_text("save name"),
                        );
                        if ui.button("Save")
                            .on_hover_text(
                                "Save current settings into the brush menu (AppData/Azra/brushes). \
                                 Appears as a button next to Draw/Clay/… after save."
                            )
                            .clicked()
                        {
                            let n = name.trim().to_string();
                            if !n.is_empty() {
                                save_brush_as = Some(n.clone());
                                active_user = n.clone();
                                ui.ctx().data_mut(|d| d.insert_temp(user_brush_id, n));
                            }
                        }
                        if ui.small_button("Del")
                            .on_hover_text("Delete the saved brush named in the field (or the selected menu slot).")
                            .clicked()
                        {
                            let n = if !active_user.is_empty() {
                                active_user.clone()
                            } else {
                                name.trim().to_string()
                            };
                            if !n.is_empty() {
                                delete_brush_name = Some(n);
                                active_user.clear();
                                ui.ctx().data_mut(|d| d.insert_temp(user_brush_id, String::new()));
                            }
                        }
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(id, name);
                            d.insert_temp(user_brush_id, active_user.clone());
                        });
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button("Install ZB/Coat starters")
                            .on_hover_text(
                                "Write industry-feel starter brushes into the brush menu: \
                                 ZB Clay BuildUp, ZB Clay, ZB DamStandard, ZB hPolish, Coat Clay, ZB Standard. \
                                 Safe to re-run (overwrites those names only)."
                            )
                            .clicked()
                        {
                            match crate::brush_library::install_industry_starters() {
                                Ok(n) => tip_toast = Some(format!("Installed {n} industry starter brushes")),
                                Err(e) => tip_toast = Some(format!("Starters failed: {e}")),
                            }
                        }
                    });
                    if presets.is_empty() {
                        ui.label(
                            egui::RichText::new("No saved brushes — Install ZB/Coat starters, or name + Save.")
                                .small()
                                .color(egui::Color32::from_rgb(130, 130, 140)),
                        );
                    }

                    ui.separator();
                    // ---- Brush parameters ----
                    // Per-brush settings toggle: ON = each brush remembers its own Radius/Strength/Falloff/
                    // Direction (saved on switch, restored on return); OFF = shared across all brushes.
                    ui.checkbox(&mut v.per_brush_settings, "Per-brush settings")
                        .on_hover_text(
                            "ON (default): each base op has its own factory + tweaks in the brush system \
                             (radius, strength, falloff, spacing, plane/build/pinch, dynamics, \u{2026}). \
                             Switching ops saves/restores knobs. First pick of each op loads its factory. \
                             OFF: one shared settings set for all base ops."
                        );

                    // ---- Brush system knobs: ALWAYS exposed for every base op (not per-op UI gates) ----
                    ui.label(egui::RichText::new("Settings (all brushes)").strong().small())
                        .on_hover_text(
                            "These knobs belong to the brush system — available for every base op. \
                             Base op (Draw/Clay/…) only chooses the density operator; it does not hide settings."
                        );
                    ui.spacing_mut().slider_width = 130.0;
                    // Sculpt on Surface — ZBrush / Blender style stick-to-mesh (default ON).
                    {
                        let on = v.sculpt_on_surface;
                        let label = if on {
                            "● Sculpt on Surface"
                        } else {
                            "○ Free (off surface)"
                        };
                        if ui
                            .add_sized(
                                [ui.available_width().min(280.0), 26.0],
                                egui::SelectableLabel::new(on, label),
                            )
                            .on_hover_text(
                                "ON (ZBrush / Blender): brush sticks to the density surface (raycast), \
                                 front faces only — no painting in empty air. Stamp sits ON the iso \
                                 (not offset toward the camera).\n\
                                 OFF: freer placement when the pick misses."
                            )
                            .clicked()
                        {
                            v.sculpt_on_surface = !v.sculpt_on_surface;
                        }
                    }
                    // Add / Remove (ZBrush Zadd/Zsub). Maps to density build vs carve for every base op.
                    // Crease/Erase/Scrape: Remove = groove/cut (default), Add = ridge/raise.
                    // Draw/Clay/Build: Add = deposit (default), Remove = carve.
                    ui.horizontal(|ui| {
                        let is_carve = matches!(
                            v.brush,
                            VoxelBrush::Erase | VoxelBrush::Crease | VoxelBrush::Scrape
                        );
                        ui.label(egui::RichText::new("Mode").small()).on_hover_text(
                            "Zadd/Zsub density polarity. Ctrl flips for this stroke.\n\
                             Crease: Remove = cut groove (default), Add = raise ridge.\n\
                             Draw/Clay: Add = deposit, Remove = carve."
                        );
                        if ui
                            .selectable_label(v.brush_add, "Add")
                            .on_hover_text(if is_carve {
                                "Add (Zadd) — raise ridge / reverse of groove. Ctrl → cut."
                            } else {
                                "Add (Zadd) — deposit material. Ctrl → carve."
                            })
                            .clicked()
                        {
                            v.brush_add = true;
                        }
                        if ui
                            .selectable_label(!v.brush_add, "Remove")
                            .on_hover_text(if is_carve {
                                "Remove (Zsub) — cut groove (Dam Standard default). Ctrl → ridge."
                            } else {
                                "Remove (Zsub) — carve material. Ctrl → deposit."
                            })
                            .clicked()
                        {
                            v.brush_add = false;
                        }
                        let hint = if is_carve {
                            if v.brush_add { "→ ridge · Ctrl=groove" } else { "→ groove · Ctrl=ridge" }
                        } else if v.brush_add {
                            "→ deposit · Ctrl=carve"
                        } else {
                            "→ carve · Ctrl=deposit"
                        };
                        ui.label(
                            egui::RichText::new(hint)
                                .small()
                                .color(egui::Color32::from_rgb(140, 140, 150)),
                        );
                    });
                    ui.add(egui::Slider::new(&mut v.radius, 0.05..=1.0).text("Radius").step_by(0.005))
                        .on_hover_text("Brush size. Hotkeys: Z / X (smaller / larger). Scroll also adjusts size.");
                    ui.add(egui::Slider::new(&mut v.strength, 0.05..=1.0).text("Strength").step_by(0.01))
                        .on_hover_text("Deposit / effect strength. Hotkeys: Shift+Z / Shift+X.");
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Falloff").small());
                        for p in core_math::voxel_sculpt::FalloffPreset::ALL {
                            if ui.selectable_label(v.falloff_preset == p, p.label())
                                .on_hover_text("Radial falloff family. Blend uses Hardness below.")
                                .clicked()
                            {
                                v.falloff_preset = p;
                            }
                        }
                    });
                    // Hardness: Blender-style plateau on EVERY falloff preset (not only Blend).
                    ui.add(egui::Slider::new(&mut v.falloff, 0.0..=1.0).text("Hardness").step_by(0.01))
                        .on_hover_text(
                            "Blender-style hardness: 0 = soft (curve over full radius), 1 = hard (full-strength \
                             core, sharp rim). Applies to ALL falloff presets (Smooth/Sharp/Sphere/\u{2026})."
                        );
                    ui.add(egui::Slider::new(&mut v.stroke_spacing, 0.03..=0.40).text("Spacing").step_by(0.01))
                        .on_hover_text(
                            "Dab spacing as a fraction of radius. Lower = denser continuous clay \
                             (ZBrush/Coat clay often 4–6%). Clay BuildUp factory = 5%."
                        );
                    // ---- Factory recipe (brush system) — every base op has one ----
                    ui.horizontal(|ui| {
                        let label = crate::VoxelObject::factory_label(v.brush);
                        if ui.button(format!("Reset {label} factory"))
                            .on_hover_text(
                                "Reload this base op's factory recipe into the brush system knobs. \
                                 Radius is kept. Does not install tip alphas (GPU path stays live)."
                            )
                            .clicked()
                        {
                            v.apply_brush_factory(v.brush);
                        }
                        if matches!(
                            v.brush,
                            VoxelBrush::ClayBuildup | VoxelBrush::Crease | VoxelBrush::Flatten
                        ) {
                            if ui.small_button("+tip")
                                .on_hover_text(
                                    "Also install the recipe tip alpha (Square / Thin Line). \
                                     Tips force the CPU stamp path until cleared."
                                )
                                .clicked()
                            {
                                v.apply_brush_factory_with_tip(v.brush);
                            }
                        }
                    });
                    // ═══════════════════════════════════════════════════════════
                    // ACTIVATORS — toggle ON to layer behaviors onto ANY base op
                    // (e.g. Draw + Gather = deposit then pinch). Params below
                    // enable only when the feature is active.
                    // ═══════════════════════════════════════════════════════════
                    ui.label(egui::RichText::new("Activators (toggle ON to use)").strong().small())
                        .on_hover_text(
                            "Turn features ON for the current base op. Example: Draw + Gather = Draw then Pinch. \
                             Up to 4 operators total (base + stacks). Greyed sliders below = inactive."
                        );
                    ui.horizontal_wrapped(|ui| {
                        let is_pinch = matches!(v.brush, VoxelBrush::Pinch);
                        ui.add_enabled(!is_pinch, egui::Checkbox::new(&mut v.stack_gather, "+Gather (Pinch)"))
                            .on_hover_text(if is_pinch {
                                "Base is already Pinch — gather is the main op."
                            } else {
                                "ON: after each dab, pull density toward the stroke axis (Pinch). Works on Draw/Clay/…"
                            });
                        let is_flat = matches!(v.brush, VoxelBrush::Flatten);
                        ui.add_enabled(!is_flat, egui::Checkbox::new(&mut v.stack_flatten, "+Flatten"))
                            .on_hover_text("ON: push surface to the contact plane after the base op.");
                        // Fill family already is PlaneFill/PlaneClip — stacking +Fill is a no-op in fold.
                        let is_fill_family = matches!(
                            v.brush,
                            VoxelBrush::Fill
                                | VoxelBrush::Clay
                                | VoxelBrush::ClayBuildup
                                | VoxelBrush::Scrape
                        );
                        ui.add_enabled(!is_fill_family, egui::Checkbox::new(&mut v.stack_fill, "+Fill"))
                            .on_hover_text(if is_fill_family {
                                "Base is already a plane-fill family op (Clay/Fill/Build/Scrape)."
                            } else {
                                "ON: deposit only below the plane after the base op."
                            });
                        let is_crease = matches!(v.brush, VoxelBrush::Crease);
                        ui.add_enabled(!is_crease, egui::Checkbox::new(&mut v.stack_crease, "+Crease"))
                            .on_hover_text("ON: cut a groove after the base op (uses Crease width/depth).");
                        let is_inf = matches!(v.brush, VoxelBrush::Inflate);
                        ui.add_enabled(!is_inf, egui::Checkbox::new(&mut v.stack_inflate, "+Inflate"))
                            .on_hover_text("ON: puff shell after the base op.");
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut v.auto_smooth, "Auto Smooth")
                            .on_hover_text("ON: mild relax after each dab (any base except Smooth/Move).");
                        ui.checkbox(&mut v.surface_extrude, "Surface extrude")
                            .on_hover_text("ON: live surface normal each dab (skin-relative deposit direction).");
                        ui.add_enabled_ui(!v.sculpt_on_surface, |ui| {
                            ui.checkbox(&mut v.front_faces_only, "Front faces only")
                                .on_hover_text(
                                    "ON: skip dabs facing away from the camera. \
                                     Forced ON while Sculpt on Surface is active."
                                );
                        });
                        // Always editable — including Draw. Draw uses soft dual approach for BOTH
                        // Add and Remove (so polarity feels the same); Accu only scales the rate.
                        ui.checkbox(&mut v.accumulate, "Accumulate")
                            .on_hover_text(
                                "Draw: OFF = soft approach solid/empty (Add≈Remove feel); ON = stronger \
                                 dual rate. Clay/Fill/Build: OFF = Max plane fill, ON = additive. \
                                 Crease groove always cuts (ignores this)."
                            );
                    });
                    // Active stack summary (+ MAX_OPS=4 cap warning)
                    {
                        let mut parts = Vec::new();
                        if v.pinch_active() { parts.push("Gather"); }
                        if v.flatten_active() { parts.push("Flatten"); }
                        if v.fill_active() { parts.push("Fill"); }
                        if v.crease_active() { parts.push("Crease"); }
                        if v.inflate_active() { parts.push("Inflate"); }
                        if v.auto_smooth { parts.push("AutoSmooth"); }
                        let stack_n = (v.stack_gather as u8
                            + v.stack_flatten as u8
                            + v.stack_fill as u8
                            + v.stack_crease as u8
                            + v.stack_inflate as u8) as usize;
                        // Base + stacks ≤ 4 ops total in fold/GPU.
                        if stack_n >= 3 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Stack heavy ({stack_n} activators) — max 4 ops total; later toggles may be dropped"
                                ))
                                .small()
                                .color(egui::Color32::from_rgb(200, 160, 90)),
                            );
                        }
                        if !parts.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Active: {}", parts.join(" + ")))
                                    .small()
                                    .color(egui::Color32::from_rgb(120, 180, 140)),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("Active: base only — toggle activators above to layer effects")
                                    .small()
                                    .color(egui::Color32::from_rgb(140, 140, 140)),
                            );
                        }
                    }

                    // ---- Pinch (enabled when Pinch base or +Gather) ----
                    let pinch_on = v.pinch_active();
                    ui.label(egui::RichText::new(if pinch_on { "Pinch / Gather" } else { "Pinch / Gather (off — enable +Gather)" }).strong().small());
                    if !pinch_on {
                        ui.label(
                            egui::RichText::new("Turn on +Gather above, or select the Pinch brush.")
                                .small()
                                .color(egui::Color32::from_rgb(200, 160, 90)),
                        );
                    }
                    ui.add_enabled_ui(pinch_on, |ui| {
                        ui.add(egui::Slider::new(&mut v.pinch_intensity, 0.15..=4.0).text("Pinch strength").step_by(0.05))
                            .on_hover_text(
                                "REACH — how far along the stroke axis density is sampled. \
                                 Independent of Brush Strength. 1 = normal, 2–4 = aggressive."
                            );
                        ui.add(egui::Slider::new(&mut v.pinch_width, 0.2..=3.0).text("Pinch width").step_by(0.05))
                            .on_hover_text(
                                "FOOTPRINT — how wide the pinch affects the brush disc. \
                                 <1 = tight core only, 1 = normal, >1 = broad shoulders. \
                                 Not the same as Strength (reach)."
                            );
                        ui.add(egui::Slider::new(&mut v.pinch_amount, 0.1..=2.0).text("Pinch amount").step_by(0.05))
                            .on_hover_text(
                                "Blend weight (with Brush Strength) — how hard density values move toward the sample."
                            );
                    });

                    // ---- Crease (enabled when Crease base or +Crease) ----
                    let crease_on = v.crease_active();
                    ui.label(egui::RichText::new(if crease_on { "Crease / Dam Standard" } else { "Crease (off)" }).strong().small());
                    if crease_on {
                        ui.label(
                            egui::RichText::new(
                                if v.brush_add {
                                    "Mode Add = raise ridge · switch to Remove to cut a groove"
                                } else {
                                    "Mode Remove = cut groove (default) · Accumulate does not affect Crease"
                                },
                            )
                            .small()
                            .color(egui::Color32::from_rgb(180, 160, 100)),
                        );
                    }
                    ui.add_enabled_ui(crease_on, |ui| {
                        ui.add(egui::Slider::new(&mut v.crease_width, 0.25..=3.0).text("Crease width").step_by(0.05))
                            .on_hover_text("Groove width (1 = legacy power curve). Narrow line tip + Follow orientation.");
                        ui.add(egui::Slider::new(&mut v.crease_depth, 0.1..=3.0).text("Crease depth").step_by(0.05))
                            .on_hover_text("How hard the groove carves (or ridge builds if Mode=Add).");
                    });

                    // ---- Trim (Scrape base only) ----
                    let scrape_on = v.scrape_active();
                    ui.label(egui::RichText::new(if scrape_on { "Trim / Scrape" } else { "Trim / Scrape (off — pick Scrape)" }).strong().small());
                    ui.add_enabled_ui(scrape_on, |ui| {
                        ui.add(egui::Slider::new(&mut v.plane_angle, -0.5..=0.5).text("Plane angle").step_by(0.01));
                        ui.add(egui::Slider::new(&mut v.cut_depth, 0.1..=3.0).text("Cut depth").step_by(0.05));
                        ui.checkbox(&mut v.fill_holes, "Fill holes");
                    });

                    // ---- Plane / Build / Flatten ----
                    let plane_on = v.flatten_active() || v.fill_active() || v.build_active()
                        || matches!(v.brush, VoxelBrush::Clay);
                    ui.label(egui::RichText::new(if plane_on {
                        "Plane / Build / Flatten"
                    } else {
                        "Plane / Build / Flatten (off — use Clay/Build/Fill/Flatten or +stack)"
                    }).strong().small());
                    ui.add_enabled_ui(plane_on, |ui| {
                        ui.add_enabled_ui(v.build_active(), |ui| {
                            ui.add(egui::Slider::new(&mut v.layer_height, 0.0..=0.45).text("Layer height").step_by(0.01))
                                .on_hover_text("Build proud plane as ×radius.");
                            ui.add(egui::Slider::new(&mut v.layer_gain, 0.0..=0.50).text("Layer×Str").step_by(0.01))
                                .on_hover_text("How much Brush Strength thickens the Build layer.");
                            ui.add(egui::Slider::new(&mut v.build_rate, 0.25..=4.0).text("Build rate").step_by(0.05))
                                .on_hover_text("How fast Build packs toward the layer. Higher always affects the core.");
                        });
                        ui.add(egui::Slider::new(&mut v.height_limit, 0.05..=1.0).text("Height limit").step_by(0.01))
                            .on_hover_text(
                                "Max density Clay / Fill / Build / Flatten may reach (1 = off). \
                                 Floored just above the surface iso so mid values never collapse clay. \
                                 Only limits growth — never digs existing solid down."
                            );
                        ui.add(egui::Slider::new(&mut v.plane_offset, 0.0..=0.45).text("Plane offset").step_by(0.01))
                            .on_hover_text(
                                "Shifts Clay / Fill / Flatten plane along the normal as ×radius. 0 = through contact."
                            );
                        ui.add(egui::Slider::new(&mut v.plane_bias, -0.25..=0.25).text("Plane bias").step_by(0.01))
                            .on_hover_text("Extra plane shift (+ proud / − buried). Added to Plane Offset.");
                        ui.add_enabled_ui(v.flatten_active(), |ui| {
                            ui.add(egui::Slider::new(&mut v.flatten_amount, 0.05..=1.5).text("Flatten amt").step_by(0.05))
                                .on_hover_text("How hard Flatten pushes to the plane (scales with Brush Strength).");
                        });
                        ui.add_enabled_ui(v.flatten_active() || v.inflate_active(), |ui| {
                            ui.add(egui::Slider::new(&mut v.volume_preservation, 0.0..=1.0).text("Vol preserve").step_by(0.05))
                                .on_hover_text(
                                    "Softens Flatten/Inflate deep in solid cores (0 = full effect). \
                                     Enable Flatten or Inflate (base or +stack) to use."
                                );
                        });
                    });

                    // ---- Inflate ----
                    let inflate_on = v.inflate_active();
                    ui.label(egui::RichText::new(if inflate_on { "Inflate" } else { "Inflate (off)" }).strong().small());
                    ui.add_enabled_ui(inflate_on, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Dir").small());
                            for d in core_math::brush::InflateDirection::ALL {
                                if ui.selectable_label(v.inflate_direction == d.as_u32(), d.label())
                                    .on_hover_text(d.tip())
                                    .clicked()
                                {
                                    v.inflate_direction = d.as_u32();
                                }
                            }
                        });
                        ui.add(egui::Slider::new(&mut v.inflate_speed, 0.1..=3.0).text("Inflate speed").step_by(0.05));
                        ui.add(egui::Slider::new(&mut v.thickness_clamp, 0.1..=1.0).text("Thickness clamp").step_by(0.05));
                    });

                    // Auto smooth intensity (only when toggled)
                    ui.add_enabled_ui(v.auto_smooth, |ui| {
                        ui.add(egui::Slider::new(&mut v.auto_smooth_intensity, 0.05..=1.0).text("Auto smooth str").step_by(0.05));
                    });
                    // Auto Mask — feature protect (system knob, all base ops).
                    // Shares the protect buffer with Mask Paint. Off / mode change CLEARS that buffer
                    // so protection does not stick forever after the first Peak/Cavity pass.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Auto Mask").small())
                            .on_hover_text(
                                "Writes protect-mask from surface features before each stamp. \
                                 Peak = ridges, Cavity = dents, Curv = both, Flat = flats. \
                                 Same buffer as Mask Paint. \
                                 Off or switching mode CLEARS the protect mask so locks do not stick. \
                                 Re-stroking soft-overwrites (can release protection)."
                            );
                        let prev_am = v.auto_mask;
                        for m in core_math::voxel_sculpt::AutoMaskMode::ALL {
                            if ui.selectable_label(v.auto_mask == m, m.label())
                                .on_hover_text(match m {
                                    core_math::voxel_sculpt::AutoMaskMode::Off =>
                                        "No automatic mask. Clears existing protect mask (same as Clear).",
                                    core_math::voxel_sculpt::AutoMaskMode::Curvature =>
                                        "Protect high curvature (peaks + cavities). Clears prior auto mask.",
                                    core_math::voxel_sculpt::AutoMaskMode::Peak =>
                                        "Protect convex ridges / peaks. Clears prior auto mask.",
                                    core_math::voxel_sculpt::AutoMaskMode::Cavity =>
                                        "Protect concave cavities. Clears prior auto mask.",
                                    core_math::voxel_sculpt::AutoMaskMode::Flat =>
                                        "Protect flat regions (sculpt edges only). Clears prior auto mask.",
                                })
                                .clicked()
                            {
                                v.auto_mask = m;
                            }
                        }
                        // Changing Off or mode: drop stuck protection so the setting feels live.
                        if v.auto_mask != prev_am {
                            if v.grid.has_mask() {
                                v.grid.clear_mask();
                                if let Some(d) = v.grid.take_dirty() {
                                    mask_remesh_region = Some([
                                        d.min[0], d.min[1], d.min[2],
                                        d.max[0], d.max[1], d.max[2],
                                    ]);
                                }
                            }
                            if v.auto_mask.is_on() {
                                // Ensure Show is on so the user sees fresh auto-mask write.
                                if !v.grid.show_mask {
                                    v.grid.set_show_mask(true);
                                }
                            }
                        }
                    });
                    ui.add_enabled_ui(v.auto_mask.is_on(), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Slider::new(&mut v.auto_mask_intensity, 0.05..=1.0).text("AM str").step_by(0.05))
                                .on_hover_text(
                                    "How hard auto-mask locks features. Soft-overwrite: re-stroke after lowering \
                                     can release protection (no longer permanent max-lock)."
                                );
                            ui.checkbox(&mut v.auto_mask_invert, "Invert")
                                .on_hover_text("Invert feature response (e.g. Peak mode protects flats instead).");
                        });
                    });
                    if v.auto_mask.is_on() && v.grid.has_mask() {
                        ui.label(
                            egui::RichText::new("Auto Mask active — Off clears protection")
                                .small()
                                .color(egui::Color32::from_rgb(180, 140, 220)),
                        );
                    }
                    // ---- Brush Dynamics (each slider: 0 = OFF, >0 = ON) ----
                    ui.label(egui::RichText::new("Brush Dynamics (0 = off)").strong().small())
                        .on_hover_text(
                            "Per-dab randomization. Each slider is its own toggle: 0 = off, >0 = active. \
                             Works on every base op."
                        );
                    ui.add(egui::Slider::new(&mut v.dyn_jitter, 0.0..=1.0).text("Jitter").step_by(0.01))
                        .on_hover_text("Small high-frequency position noise in the stamp plane (×radius).");
                    ui.add(egui::Slider::new(&mut v.dyn_scatter, 0.0..=2.0).text("Scatter").step_by(0.02))
                        .on_hover_text("Larger position scatter in the stamp plane (×radius). Spray / spray-gun feel.");
                    ui.add(egui::Slider::new(&mut v.dyn_random_rotation, 0.0..=1.0).text("Random Rotation").step_by(0.01))
                        .on_hover_text("Random tip-alpha rotation range (0 = none, 1 = ±180°). Needs a tip image.");
                    ui.add(egui::Slider::new(&mut v.dyn_random_strength, 0.0..=1.0).text("Random Strength").step_by(0.01))
                        .on_hover_text("± strength variation per dab (0 = fixed, 1 = 0..2×).");
                    ui.add(egui::Slider::new(&mut v.dyn_random_radius, 0.0..=1.0).text("Random Radius").step_by(0.01))
                        .on_hover_text("± radius variation per dab (0 = fixed, 1 ≈ 0.15..2×).");
                    ui.add(egui::Slider::new(&mut v.dyn_stroke_noise, 0.0..=1.0).text("Stroke Noise").step_by(0.01))
                        .on_hover_text("Low-frequency path meander along the stroke (×radius). Organic / hand-drawn feel.");
                    // Tip alpha — visual library (form preview, not just names)
                    ui.label(egui::RichText::new("Brush Tip (alpha)").strong().small())
                        .on_hover_text(
                            "Alpha multiplies stamp falloff (white=full, black=none). \
                             Procedural SDF/noise tips ship by default; import PNG/ABR packs anytime. \
                             Library: AppData/Azra/brush_tips."
                        );
                    ui.horizontal(|ui| {
                        if ui.button("Import\u{2026}")
                            .on_hover_text(
                                "Multi-select: PNG / JPG / TGA / WebP / EXR, Photoshop ABR, \
                                 or Blender .blend brush packs. Saved to tip library."
                            )
                            .clicked()
                        {
                            load_brush_tip = true;
                        }
                        if ui.button("Folder\u{2026}")
                            .on_hover_text(
                                "Import every image/ABR/.blend under a folder (Blender alpha packs, \
                                 3D-Coat Alphas, Substance alphas)."
                            )
                            .clicked()
                        {
                            load_brush_tip_folder = true;
                        }
                        if ui.small_button("Defaults")
                            .on_hover_text(
                                "Install / reinstall procedural essentials (SDF shapes + FBM noise + \
                                 classic discs). Stock Blender brushes are procedural — no PNG in .blend."
                            )
                            .clicked()
                        {
                            install_proc_tips = true;
                        }
                        if ui.small_button("Blender")
                            .on_hover_text(
                                "Scan Blender installs for essentials .blend packs. Stock brushes have \
                                 no PNG — falls back to procedural SDF essentials."
                            )
                            .clicked()
                        {
                            load_brush_tip_blender = true;
                        }
                        if v.brush_tip.is_some() {
                            if ui.small_button("Clear")
                                .on_hover_text("Remove active tip (file stays in library). GPU identity disc.")
                                .clicked()
                            {
                                v.brush_tip = None;
                                v.brush_tip_name.clear();
                                v.brush_tip_path = None;
                            }
                            ui.label(
                                egui::RichText::new("tip → GPU")
                                    .small()
                                    .color(egui::Color32::from_rgb(120, 180, 140)),
                            )
                            .on_hover_text(
                                "Brush tip alpha samples on the GPU (same path as radial falloff). \
                                 No CPU arm — sculpt stays fluid with alphas always on."
                            );
                        }
                    });

                    // ── Procedural Alpha (SDF shapes + shader FBM noise) ──
                    // LIVE: any kind/slider change re-bakes and installs the tip on the brush
                    // (was Apply-only → "changing alpha does nothing" / tips never update).
                    ui.collapsing("Procedural Alpha (SDF / Noise)", |ui| {
                        use crate::brush_tip_lib::{generate_proc_alpha, save_proc_alpha_to_library, ProcAlphaKind, ProcAlphaParams};
                        let id = egui::Id::new("proc_alpha_params");
                        let live_id = egui::Id::new("proc_alpha_live_apply");
                        let mut params = ui.ctx().data_mut(|d| {
                            d.get_temp::<ProcAlphaParams>(id).unwrap_or_default()
                        });
                        // Live → brush ON by default: tips sample on the GPU (no CPU force).
                        let mut live_apply = ui.ctx().data_mut(|d| {
                            d.get_temp::<bool>(live_id).unwrap_or(true)
                        });
                        let prev_params = params.clone();
                        ui.horizontal_wrapped(|ui| {
                            for k in ProcAlphaKind::ALL {
                                if ui.selectable_label(params.kind == k, k.label()).clicked() {
                                    params.kind = k;
                                }
                            }
                        });
                        ui.add(egui::Slider::new(&mut params.soft, 0.02..=0.45).text("Soft edge").step_by(0.01));
                        if params.kind.is_sdf() {
                            ui.add(egui::Slider::new(&mut params.fill, 0.4..=0.95).text("SDF fill").step_by(0.01))
                                .on_hover_text("How large the SDF solid is inside the tip disc.");
                            ui.checkbox(&mut params.shell, "Shell / outline")
                                .on_hover_text("Bake a hollow band around the SDF surface (ring tips).");
                            if params.shell {
                                ui.add(egui::Slider::new(&mut params.shell_w, 0.04..=0.35).text("Shell width").step_by(0.01));
                            }
                        }
                        if params.kind.is_noise() {
                            ui.add(egui::Slider::new(&mut params.scale, 1.0..=24.0).text("Noise scale").step_by(0.5))
                                .on_hover_text("Same idea as shader Noise node scale.");
                            ui.add(egui::Slider::new(&mut params.octaves, 1..=6).text("Octaves"))
                                .on_hover_text("FBM octaves — matches shader fbm3.");
                            ui.add(egui::Slider::new(&mut params.seed, 0..=9999).text("Seed"));
                        }
                        ui.add(egui::Slider::new(&mut params.contrast, 0.25..=3.0).text("Contrast").step_by(0.05));
                        ui.checkbox(&mut params.invert, "Invert");
                        ui.checkbox(&mut live_apply, "Live → brush")
                            .on_hover_text(
                                "ON (default): install tip on the brush as you edit. \
                                 Tips sample on the GPU — no performance arm for alpha."
                            );
                        let tip = generate_proc_alpha(&params);
                        let params_changed = params.kind != prev_params.kind
                            || (params.soft - prev_params.soft).abs() > 1e-6
                            || (params.fill - prev_params.fill).abs() > 1e-6
                            || params.shell != prev_params.shell
                            || (params.shell_w - prev_params.shell_w).abs() > 1e-6
                            || (params.scale - prev_params.scale).abs() > 1e-6
                            || params.octaves != prev_params.octaves
                            || params.seed != prev_params.seed
                            || (params.contrast - prev_params.contrast).abs() > 1e-6
                            || params.invert != prev_params.invert;
                        // Live install only when the user opted in AND params actually changed
                        // (never auto-install just because tip is empty — that forced CPU by surprise).
                        if live_apply && params_changed {
                            v.brush_tip = Some(tip.clone());
                            v.brush_tip_name = params.kind.file_stem().to_string();
                            v.brush_tip_path = None;
                        }
                        ui.horizontal(|ui| {
                            let name = format!("proc_{}", params.kind.file_stem());
                            let tex = self.tip_preview_cache.active_preview(ui.ctx(), &name, &tip, 64);
                            let sized = egui::load::SizedTexture::new(tex, egui::vec2(64.0, 64.0));
                            ui.add(egui::Image::new(sized));
                            ui.vertical(|ui| {
                                if ui.button("Apply to brush")
                                    .on_hover_text("Install this procedural alpha on the live brush (CPU stamps while tip is set).")
                                    .clicked()
                                {
                                    v.brush_tip = Some(tip.clone());
                                    v.brush_tip_name = params.kind.file_stem().to_string();
                                    v.brush_tip_path = None;
                                    tip_toast = Some(format!("Tip: {}", params.kind.label()));
                                }
                                if ui.button("Save to library")
                                    .on_hover_text("Write PNG into AppData tip library for later picks.")
                                    .clicked()
                                {
                                    match save_proc_alpha_to_library(params.kind.file_stem(), &tip) {
                                        Ok((stem, path)) => {
                                            v.brush_tip = Some(tip.clone());
                                            v.brush_tip_name = stem.clone();
                                            v.brush_tip_path = Some(path.display().to_string());
                                            tip_cache_invalidate = true;
                                            tip_toast = Some(format!("Saved tip {stem}"));
                                        }
                                        Err(e) => tip_toast = Some(format!("Save failed: {e}")),
                                    }
                                }
                                if live_apply {
                                    ui.label(
                                        egui::RichText::new("Live on brush (GPU alpha)")
                                            .small()
                                            .color(egui::Color32::from_rgb(120, 180, 140)),
                                    );
                                }
                            });
                        });
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(id, params);
                            d.insert_temp(live_id, live_apply);
                        });
                    });
                    // Active tip square (shared widget: thumbnail + dnd drop target).
                    ui.horizontal(|ui| {
                        let (_clicked, dropped) = crate::brush_tip_lib::active_tip_square(
                            ui,
                            &mut self.tip_preview_cache,
                            v.brush_tip.as_ref(),
                            &v.brush_tip_name,
                            72.0,
                        );
                        if let Some((name, path, tip)) = dropped {
                            v.brush_tip = Some(tip);
                            v.brush_tip_name = name.clone();
                            v.brush_tip_path = Some(path.to_string_lossy().to_string());
                            tip_toast = Some(format!("Tip: {name}"));
                        }
                        ui.vertical(|ui| {
                            if let Some(ref tip) = v.brush_tip {
                                let name = if v.brush_tip_name.is_empty() {
                                    "active"
                                } else {
                                    v.brush_tip_name.as_str()
                                };
                                ui.label(egui::RichText::new(name).strong().small());
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}×{} · ON brush",
                                        tip.width, tip.height
                                    ))
                                    .small()
                                    .color(egui::Color32::from_rgb(120, 180, 140)),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(
                                        "No tip on brush — pick a library thumb, drag one onto \
                                         the square, or use Procedural Alpha (Live).",
                                    )
                                    .small()
                                    .color(egui::Color32::from_rgb(180, 150, 100)),
                                );
                            }
                        });
                    });
                    // Scrollable visual grid of library tips (shared widget; thumbs also drag).
                    if let Some((name, path, tip)) = crate::brush_tip_lib::tip_library_grid(
                        ui,
                        &mut self.tip_preview_cache,
                        &v.brush_tip_name,
                        "sculpt_panel",
                    ) {
                        v.brush_tip = Some(tip);
                        v.brush_tip_name = name.clone();
                        v.brush_tip_path = Some(path.to_string_lossy().to_string());
                        tip_toast = Some(format!("Tip: {name}"));
                    }
                    // Move note only (not gated settings)
                    if matches!(v.brush, core_math::voxel_sculpt::VoxelBrush::Move | core_math::voxel_sculpt::VoxelBrush::Smudge) {
                        ui.label(egui::RichText::new("Move/Smudge: density warp · Ctrl = normal · redistance on release")
                            .small().color(egui::Color32::from_rgb(140, 190, 140)));
                    }

                    // ---- Mask (dense Vec or sparse HashMap) ----
                    // Clear / Invert / Show tint mark the grid dirty; remesh runs after this &mut v borrow ends.
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut v.mask_paint, "Mask paint")
                            .on_hover_text("Paint protect-mask instead of density. Ctrl = clear mask under brush. Masked voxels resist sculpt. Sparse uses a compact non-zero map.")
                            .changed() {}
                        let mut show = v.grid.show_mask;
                        if ui.checkbox(&mut show, "Show")
                            .on_hover_text("Viewport mask tint (darken protected surface). Off hides tint without clearing protection.")
                            .changed()
                        {
                            v.grid.set_show_mask(show);
                            if let Some(d) = v.grid.take_dirty() {
                                mask_remesh_region = Some([d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                            }
                        }
                        if ui.small_button("Invert")
                            .on_hover_text("Invert mask protection (Ctrl+I in Sculpt). Sparse: inverts painted cells only.")
                            .clicked()
                        {
                            v.grid.invert_mask();
                            if let Some(d) = v.grid.take_dirty() {
                                mask_remesh_region = Some([d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                            }
                        }
                        if ui.small_button("Clear")
                            .on_hover_text("Remove all mask protection and tint.")
                            .clicked()
                        {
                            v.grid.clear_mask();
                            if let Some(d) = v.grid.take_dirty() {
                                mask_remesh_region = Some([d.min[0], d.min[1], d.min[2], d.max[0], d.max[1], d.max[2]]);
                            }
                        }
                        if v.grid.has_mask() {
                            let tag = if v.grid.density.is_sparse() { "mask on (sparse)" } else { "mask on" };
                            ui.label(egui::RichText::new(tag).small().color(egui::Color32::from_rgb(180, 140, 220)));
                        }
                    });

                    // ---- MOVE knobs (Grab) ----
                    if matches!(v.brush, VoxelBrush::Move | VoxelBrush::Smudge) {
                        ui.label(egui::RichText::new("Move / Grab").strong().small())
                            .on_hover_text("Strength \u{D7} Elasticity \u{D7} Pull Strength scale screen-space drag (Blender grab scales by strength). Dynamic Topology = voxelize each release.");
                        ui.add(egui::Slider::new(&mut v.move_elasticity, 0.0..=2.0).text("Elasticity").step_by(0.05))
                            .on_hover_text("How far the grab follows the cursor (1 = full). Move factory = 1.00.");
                        ui.add(egui::Slider::new(&mut v.move_pull_strength, 0.0..=2.0).text("Pull strength").step_by(0.05))
                            .on_hover_text("Second drag multiplier (drag \u{D7} Elasticity \u{D7} Pull). Move factory = 1.00.");
                        ui.checkbox(&mut v.move_connected_only, "Connected only")
                            .on_hover_text(
                                "Prefer island-local grab under the hit (connectivity flood reserved; \
                                 sphere falloff still applies). Move factory = ON."
                            );
                    }
                    // ---- SNAKE HOOK knobs (surface-mode tube) ----
                    if matches!(v.brush, VoxelBrush::SnakeHook) {
                        ui.label(egui::RichText::new("Snake Hook").strong().small())
                            .on_hover_text("Pull the surface out into a tube. Strength \u{D7} Elasticity \u{D7} Pull Strength scale the drag (shared with Move). Surface mode only.");
                        ui.add(egui::Slider::new(&mut v.move_elasticity, 0.0..=2.0).text("Elasticity").step_by(0.05));
                        ui.add(egui::Slider::new(&mut v.move_pull_strength, 0.0..=2.0).text("Pull strength").step_by(0.05));
                        // crease_pinch stored in pinch_amount: 0.5 = OFF (Blender default); >0.5 narrows
                        // the tip into a spike, <0.5 inflates it (volume-preserving).
                        ui.add(egui::Slider::new(&mut v.pinch_amount, 0.0..=1.0).text("Tip pinch (crease)").step_by(0.02))
                            .on_hover_text("Blender crease_pinch. 0.5 = off. >0.5 narrows the tip into a horn/spike; <0.5 inflates it.");
                    }
                    // ---- MOVE: 3D-Coat "Voxelize immediately" / Dynamic Topology ----
                    // Coat (since 4.9.37): live Move always deforms SURFACE mesh. ON = re-voxelize when
                    // the stroke ends (brief bake). OFF = stay on surface for multi-move, voxelize when
                    // you switch tools / press Voxelize. Bake is always async here (no multi-second freeze).
                    // Factory Dynamic Topology ON ≡ Voxelize immediately ON.
                    let mut voxelize_immediately = !v.move_deferred;
                    if ui.checkbox(&mut voxelize_immediately, "Dynamic Topology / Voxelize immediately")
                        .on_hover_text(
                            "ON = re-voxelize after each Move release (async) — factory Dynamic Topology. \
                             OFF = free multi-move surface mode, voxelize on demand (button / Enter / ~2s idle)."
                        )
                        .changed()
                    {
                        v.move_deferred = !voxelize_immediately;
                    }
                    let pending_n = v.pending_move_grabs.len();
                    if pending_n > 0 {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("\u{25C6} surface mode: {} pending move{}",
                                pending_n, if pending_n == 1 { "" } else { "s" })).small().color(egui::Color32::from_rgb(230, 170, 90)));
                            if ui.button("Voxelize")
                                .on_hover_text("Bake pending surface Moves into the density grid now (also: Enter, or ~2s idle). One undoable step.")
                                .clicked() { voxelize_now = true; }
                        });
                    }

                    // ---- PEN / TABLET PRESSURE (self-contained block) ----
                    // Master "P" toggle + two compact target checkboxes. Default OFF. When on, the stylus
                    // force read each frame in handle_voxel_sculpt_input modulates the brush. On a device
                    // with no pressure (mouse, or a Windows pen seen as plain mouse events) force falls back
                    // to 1.0, so the brush behaves exactly as with pressure off.
                    ui.horizontal(|ui| {
                        if ui.add(egui::SelectableLabel::new(v.pressure_enabled, "\u{270E} P"))
                            .on_hover_text("Pen pressure: stylus force modulates the brush (needs a tablet that reports pressure; mouse = no effect)")
                            .clicked() { v.pressure_enabled = !v.pressure_enabled; }
                        ui.add_enabled_ui(v.pressure_enabled, |ui| {
                            ui.checkbox(&mut v.pressure_to_strength, "\u{2192}Strength")
                                .on_hover_text("Scale brush Strength by pressure (strength \u{D7} pressure). Clay BuildUp = 100%.");
                            ui.checkbox(&mut v.pressure_to_radius, "\u{2192}Radius")
                                .on_hover_text("Scale brush Radius by pressure. Amount below (Clay BuildUp = 30%).");
                        });
                    });
                    ui.add_enabled_ui(v.pressure_enabled && v.pressure_to_radius, |ui| {
                        ui.add(egui::Slider::new(&mut v.pressure_radius_amount, 0.0..=1.0).text("P\u{2192}R amt").step_by(0.01))
                            .on_hover_text(
                                "Pressure->Radius amount: radius *= (1-amt + amt*pressure). \
                                 0.30 = 30% (Clay BuildUp). 0.60 ~ old 40%..100% mix."
                            );
                    });
                    // Alpha Orientation (ZBrush / 3D-Coat full option set)
                    ui.label(egui::RichText::new("Alpha Orientation").strong().small())
                        .on_hover_text(
                            "How the tip image rotates on each dab. Fixed / Follow Stroke / View / Random \
                             + Angle offset. Same family as ZBrush Alpha→Orientation and 3D-Coat Stamp."
                        );
                    ui.horizontal_wrapped(|ui| {
                        use crate::TipOrientation;
                        for o in TipOrientation::ALL {
                            let sel = TipOrientation::from_u32(v.tip_orient) == o
                                || (v.tip_follow_stroke && o == TipOrientation::FollowStroke
                                    && v.tip_orient == TipOrientation::Fixed.as_u32());
                            if ui.selectable_label(sel, o.label()).on_hover_text(o.tip()).clicked() {
                                v.tip_orient = o.as_u32();
                                v.tip_follow_stroke = o == TipOrientation::FollowStroke;
                            }
                        }
                    });
                    sculpt_slider_drag(ui, &mut v.tip_angle, 0.0..=360.0, "Alpha angle", 1.0, 0.5)
                        .on_hover_text(
                            "Fixed rotation offset in degrees (0–360). Added after Fixed/Follow/View/Random. \
                             Slider or type the number."
                        );
                    ui.add(egui::Slider::new(&mut v.tip_contrast, 0.25..=2.0).text("Alpha contrast").step_by(0.01))
                        .on_hover_text("Tip mid-point contrast (1 = identity).");
                    ui.add(egui::Slider::new(&mut v.lazy, 0.0..=1.0).text("Lazy mouse").step_by(0.01))
                        .on_hover_text("Stroke stabilizer (LazyMouse) — higher = smoother & laggier, 0 = off. Default 0.10 (was 0.3 — felt laggy).");

                    // ---- Live stroke cost (P0 feel-perf) — always on so thin-stamps are visible ----
                    // stamps_raw = interpolator+symmetry before budget; stamps = after. If raw >> stamps,
                    // the budget is thinning the stroke (beads / weak clay). brush_ms = density phase.
                    {
                        let p = v.profile;
                        let thin = p.stamps_raw > p.stamps.saturating_mul(2).max(2);
                        let col = if thin {
                            egui::Color32::from_rgb(230, 140, 80)
                        } else if p.brush_ms > 12.0 {
                            egui::Color32::from_rgb(220, 100, 100)
                        } else {
                            egui::Color32::from_rgb(120, 160, 130)
                        };
                        let msg = if thin {
                            format!(
                                "stroke THIN {}/{} stamps · brush {:.1}ms · mesh {:.1}ms",
                                p.stamps, p.stamps_raw, p.brush_ms, p.mesh_ms
                            )
                        } else {
                            format!(
                                "stroke {}/{} stamps · brush {:.1}ms · mesh {:.1}ms",
                                p.stamps, p.stamps_raw, p.brush_ms, p.mesh_ms
                            )
                        };
                        ui.label(egui::RichText::new(msg).small().color(col))
                            .on_hover_text(
                                "Live stroke budget. stamps = applied this frame / stamps_raw = before cap. \
                                 THIN = budget dropped stamps (reduce radius or res, or turn off multi-axis symmetry). \
                                 brush_ms = density write; mesh_ms = remesh drain."
                            );
                    }

                    // ---- Smooth All: one global Laplacian relax over the WHOLE sculpt (not a stroke) ----
                    if ui.button("\u{25CB} Smooth All")
                        .on_hover_text("Relax the ENTIRE sculpt toward its local average for a few iterations (a global density smooth). One undoable step (Ctrl+Z reverts the whole smooth).")
                        .clicked() { smooth_all = true; }

                    ui.separator();
                    // ---- Symmetry (mirror the stamp across local X/Y/Z planes) ----
                    ui.label(egui::RichText::new("Symmetry").strong().small());
                    ui.horizontal(|ui| {
                        for (axis, label) in [(0usize, "X"), (1, "Y"), (2, "Z")] {
                            if ui.selectable_label(v.symmetry[axis], label)
                                .on_hover_text("Mirror sculpting across this local axis plane").clicked() {
                                v.symmetry[axis] = !v.symmetry[axis];
                            }
                        }
                    });

                    // ---- Symmetrize: ONE-SHOT mirror of one half onto the other (NOT a live toggle) ----
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Symmetrize").small())
                            .on_hover_text("Mirror the +axis half onto the -axis half to make the sculpt symmetric NOW. One undoable step (Ctrl+Z reverts). Shape only - paint is not mirrored yet.");
                        for (axis, label) in [(0usize, "X"), (1, "Y"), (2, "Z")] {
                            if ui.button(label)
                                .on_hover_text("Copy the +half of this axis onto the -half now (undoable).")
                                .clicked() { symmetrize_axis = Some(axis); }
                        }
                    });

                    // ---- Direction (sculpt plane + oriented brush ring) ----
                    // Surface / Normal / View / Brush / Average / Original / Tangent / Fixed Axis.
                    ui.label(egui::RichText::new("Direction").strong().small())
                        .on_hover_text(
                            "Deposit plane + brush RING (watch the green ring tilt). \
                             Original = first-click lock. Surface = live each dab. Average = area under brush. \
                             Avg region + Smooth normal are SPATIAL (under the tip) — they change the plane hard."
                        );
                    ui.horizontal_wrapped(|ui| {
                        use core_math::voxel_sculpt::BrushDirection;
                        for d in BrushDirection::ALL {
                            if ui.selectable_label(v.brush_dir == d, d.label())
                                .on_hover_text(d.tip())
                                .clicked()
                            {
                                v.brush_dir = d;
                            }
                        }
                    });
                    // Spatial Area Plane knobs (Blender Area Plane / Coat Average under brush).
                    sculpt_slider_drag(
                        ui,
                        &mut v.dir_average_region,
                        0.25..=4.0,
                        "Avg region",
                        0.05,
                        0.02,
                    )
                    .on_hover_text(
                        "SculptGL-style area normal under the tip (× brush radius). \
                         Draw/Clay/Fill sample this every dab for a stable clay plane. \
                         0.25 = tight detail · 1.0 = SculptGL default · 4.0 = wide flat. \
                         Watch the brush RING tilt."
                    );
                    sculpt_slider_drag(
                        ui,
                        &mut v.dir_smooth_normal,
                        0.0..=1.0,
                        "Smooth normal",
                        0.01,
                        0.01,
                    )
                    .on_hover_text(
                        "Blend Direction toward the spatial area normal under the brush. \
                         0 = pure Direction (e.g. locked Original). 1 = full area plane (flattens curves). \
                         On Original: 0 = hard locked plane, 1 = soft area-follow (RADICAL difference on spheres). \
                         Watch the brush RING — it must tilt when you change this."
                    );
                    // Live read-out so users know which plane is active.
                    {
                        use core_math::voxel_sculpt::BrushDirection;
                        let mode = match v.brush_dir {
                            BrushDirection::FirstPoint => "Original lock",
                            BrushDirection::Average => "Area average",
                            BrushDirection::Surface => "Live surface",
                            BrushDirection::Normal => "Cursor normal",
                            BrushDirection::View => "View",
                            BrushDirection::Brush => "Stroke path",
                            BrushDirection::Tangent => "Tangent",
                            BrushDirection::AxisX => "World X",
                            BrushDirection::AxisY => "World Y",
                            BrushDirection::AxisZ => "World Z",
                        };
                        let sm = v.dir_smooth_normal.clamp(0.0, 1.0);
                        let hint = if sm > 0.95 {
                            format!("{mode} → full AREA (region {:.1}×r)", v.dir_average_region)
                        } else if sm > 0.05 {
                            format!("{mode} + {:.0}% area blend", sm * 100.0)
                        } else {
                            mode.to_string()
                        };
                        ui.label(
                            egui::RichText::new(format!("Plane: {hint}"))
                                .small()
                                .color(egui::Color32::from_rgb(120, 180, 140)),
                        );
                    }

                    ui.separator();
                    // ---- Material (P0.3): feeds the voxel display mesh's raster G-buffer. ----
                    ui.label(egui::RichText::new("Material").strong().small());
                    let mut srgb = [
                        linear_to_srgb_u(alb.x), linear_to_srgb_u(alb.y), linear_to_srgb_u(alb.z),
                    ];
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label("Albedo");
                        if ui.color_edit_button_rgb(&mut srgb).changed() {
                            alb = glam::Vec3::new(srgb_to_linear_u(srgb[0]), srgb_to_linear_u(srgb[1]), srgb_to_linear_u(srgb[2]));
                            changed = true;
                        }
                    });
                    if ui.add(egui::Slider::new(&mut rough, 0.0..=1.0).text("Roughness").step_by(0.01)).changed() { changed = true; }
                    if ui.add(egui::Slider::new(&mut metal, 0.0..=1.0).text("Metallic").step_by(0.01)).changed() { changed = true; }
                    if changed { mat_edit = Some((alb, rough, metal)); }

                    // Clay shading toggle — the cavity-dirt layer mixes the surface toward dark brown in
                    // crevices (the "clay" look). It also overrides a freshly-set Albedo across the bumpy
                    // sculpt surface, so a pink albedo reads dark. Turn it off to see the flat albedo.
                    let mut clay = clay_on;
                    if ui.checkbox(&mut clay, "Clay shading")
                        .on_hover_text("Cavity-dirt darkening in crevices. Off = the Albedo shows flat (turn off if your colour looks dark/muddy).")
                        .changed() { clay_shading_set = Some(clay); }

                    // Paint Color (sculpt-vertex-paint): deposit the BRUSH paint colour onto voxels WHILE
                    // sculpting. This is a SEPARATE colour from Material > Albedo above (the body's base
                    // material) — painting recolours individual voxels with the brush colour so the result
                    // reads against the untouched base.
                    // Dense-only (WORK_ORDER #25): sparse (≥256³) brush path ignores colour — do not lie.
                    let sparse = v.grid.density.is_sparse();
                    if sparse && v.paint_color_on {
                        v.paint_color_on = false; // keep state honest if res crossed into sparse
                    }
                    let mut paint_on = v.paint_color_on;
                    let paint_tip = if sparse {
                        "Paint Color is dense-only — sparse grids (≥256³) write density only (no colour channel)."
                    } else {
                        "When ON, any brush also paints the BRUSH paint colour (below) onto the voxels it touches (live). This is separate from Material > Albedo (the object's base material). OFF = shape only."
                    };
                    if ui.add_enabled(!sparse, egui::Checkbox::new(&mut paint_on, "Paint Color"))
                        .on_hover_text(paint_tip)
                        .changed() && !sparse
                    {
                        v.paint_color_on = paint_on;
                        // Turning paint ON switches this body to the colour-aware CPU re-mesh/draw path
                        // (`cpu_paint_path`). Chunks meshed earlier via the resident GPU path have no
                        // current CPU mesh (their `chunk_tris` is 0), so without a full CPU re-mesh the
                        // un-painted parts of the surface would vanish. Defer a full surface re-mesh
                        // (Frame Ordering Contract — set a flag, act after the &mut v borrow drops).
                        if paint_on { paint_full_remesh = true; }
                    }
                    if sparse {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 170, 90),
                            "\u{26A0} Paint Color unavailable on sparse grids",
                        );
                    }
                    // Dedicated BRUSH paint colour picker (distinct from Material > Albedo). Stored linear,
                    // edited in sRGB (matches the Albedo swatch above). TOOL state — carries across bodies.
                    ui.horizontal(|ui| {
                        ui.label("Brush color").on_hover_text("The colour Paint Color / Fill All deposit per-voxel. Separate from the object's Material > Albedo.");
                        let mut psrgb = [
                            linear_to_srgb_u(v.paint_color.x), linear_to_srgb_u(v.paint_color.y), linear_to_srgb_u(v.paint_color.z),
                        ];
                        if ui.color_edit_button_rgb(&mut psrgb).changed() {
                            v.paint_color = glam::Vec3::new(srgb_to_linear_u(psrgb[0]), srgb_to_linear_u(psrgb[1]), srgb_to_linear_u(psrgb[2]));
                        }
                    });
                    // Fill All: colourise EVERY solid voxel with the brush paint colour, one undoable step.
                    // Dense-only (same gate as Paint Color).
                    if ui.add_enabled(!sparse, egui::Button::new("Fill All"))
                        .on_hover_text(if sparse {
                            "Fill All is dense-only — unavailable on sparse grids (≥256³)."
                        } else {
                            "Paint EVERY solid voxel with the brush colour above, in one undoable step (Ctrl+Z reverts the whole fill). Dense (\u{2264}256\u{B3}) only."
                        })
                        .clicked() && !sparse
                    { fill_all = true; }

                    ui.separator();
                    if ui.small_button("Reset clay").on_hover_text("Restore the signature terracotta clay look").clicked() {
                        mat_reset_clay = true;
                    }

                    ui.separator();
                    // ---- Resolution / Remesh ----
                    ui.label(egui::RichText::new(format!("Res {}\u{B3} \u{2022} {} tris", v.grid.res, v.tris)).small());
                    ui.horizontal(|ui| {
                        for r in [32u32, 64, 128, 192, 256] {
                            if ui.selectable_label(v.grid.res == r, format!("{}\u{B3}", r))
                                .on_hover_text("Remesh to this resolution").clicked() { act_remesh_to = Some(r); }
                        }
                    });
                    if ui.button("\u{27F3} Remesh\u{2026}  (Ctrl+R)").on_hover_text("Choose polygon count / resolution").clicked() {
                        act_open_remesh = true;
                    }
                }
                    }); // ScrollArea
            });

        // ---- Apply deferred actions (borrow of `self.sculpt.voxel` has ended) ----
        // Fill All paints the colour buffer (not the scene graph), so it uses the sculpt colour-undo path
        // (its OWN path, NOT transaction()) — see voxel_fill_all_color.
        if fill_all { self.voxel_fill_all_color(); }
        // Paint Color just turned ON: re-mesh the whole surface on the CPU so every surface chunk (incl.
        // ones previously meshed only as resident GPU buffers) has a current colour-carrying CPU mesh.
        if paint_full_remesh { self.voxel_build_all_chunks(); }
        // Smooth All: global Laplacian relax over the whole grid (its OWN sculpt-undo path, NOT transaction()).
        if smooth_all { self.voxel_smooth_all(crate::input_sculpt::SMOOTH_ALL_ITERS); }
        if let Some(ax) = symmetrize_axis { self.voxel_symmetrize(ax, true); }
        // Voxelize Now: bake the pending surface-mode Move grabs into the density grid (one sculpt-undo step).
        if voxelize_now { self.voxel_flush_pending_move(); }
        if let Some(want_surface) = surface_mode_switch { self.voxel_surface_set_mode(want_surface); }
        // Brush tip image picker (deferred rfd — never modal mid-frame).
        if load_brush_tip { self.request_brush_tip_file_dialog(); }
        if load_brush_tip_folder { self.request_brush_tip_folder_dialog(); }
        if load_brush_tip_blender { self.request_brush_tip_blender_essentials(); }
        if install_proc_tips {
            match crate::brush_tip_lib::install_procedural_essentials_force() {
                Ok(n) => {
                    self.tip_preview_cache.invalidate();
                    self.toast(&format!("Installed {n} procedural tip alphas"));
                }
                Err(e) => self.toast(&format!("Tip install failed: {e}")),
            }
        }
        if tip_cache_invalidate {
            self.tip_preview_cache.invalidate();
        }
        if let Some(msg) = tip_toast {
            self.toast(&msg);
        }
        if let Some(n) = save_brush_as { self.brush_save_as(&n); }
        if let Some(n) = load_brush_name { self.brush_load_preset(&n); }
        if let Some(n) = delete_brush_name {
            match crate::brush_library::delete_brush_preset(&n) {
                Ok(()) => self.toast(&format!("Brush deleted: {n}")),
                Err(e) => self.toast(&format!("Delete failed: {e}")),
            }
        }
        // Mask Clear / Invert / Show toggle: remesh so viewport tint updates.
        if let Some(region) = mask_remesh_region {
            self.voxel_mark_chunks_in_region(region);
            self.mark_dirty(DirtyFlags::RENDER);
        }
        if let Some((albedo, roughness, metallic)) = mat_edit {
            if let Some(n) = node {
                // Slider drag: ONE episode (not a transaction per frame — that stacked empty undos).
                // Write live; auto_commit_episodes settles on pointer-up.
                let already = self.scene.scene_graph.get(n).map(|node| {
                    match &node.payload {
                        NodePayload::VoxelBody { material, .. } => {
                            (material.albedo - albedo).length_squared() < 1e-12
                                && (material.roughness - roughness).abs() < 1e-6
                                && (material.metallic - metallic).abs() < 1e-6
                        }
                        _ => true,
                    }
                }).unwrap_or(true);
                if !already {
                    if !self.has_active_episode() {
                        self.begin_episode("Voxel material");
                    }
                    if let Some(node) = self.scene.scene_graph.get_mut(n) {
                        if let NodePayload::VoxelBody { material, .. } = &mut node.payload {
                            material.albedo = albedo;
                            material.roughness = roughness;
                            material.metallic = metallic;
                        }
                    }
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        }
        if let Some(enabled) = clay_shading_set {
            if let Some(n) = node {
                let already = self.scene.scene_graph.get(n).map(|node| {
                    matches!(&node.payload, NodePayload::VoxelBody { material, .. }
                        if material.cavity_dirt.enabled == enabled)
                }).unwrap_or(true);
                if !already {
                    self.transaction("Voxel clay shading", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(n) {
                            if let NodePayload::VoxelBody { material, .. } = &mut node.payload {
                                material.cavity_dirt.enabled = enabled;
                            }
                        }
                        s.mark_dirty(DirtyFlags::RENDER);
                    });
                }
            }
        }
        if mat_reset_clay {
            if let Some(n) = node {
                // Already the clay defaults → no empty undo.
                let clay_alb = glam::Vec3::new(0.82, 0.45, 0.38);
                let already = self.scene.scene_graph.get(n).map(|node| {
                    match &node.payload {
                        NodePayload::VoxelBody { material, .. } => {
                            (material.albedo - clay_alb).length_squared() < 1e-12
                                && (material.roughness - 0.70).abs() < 1e-6
                                && material.metallic.abs() < 1e-6
                                && material.cavity_dirt.enabled
                                && (material.cavity_dirt.amount - 0.55).abs() < 1e-6
                        }
                        _ => true,
                    }
                }).unwrap_or(true);
                if !already {
                    self.transaction("Voxel material reset", |s| {
                        if let Some(node) = s.scene.scene_graph.get_mut(n) {
                            if let NodePayload::VoxelBody { material, .. } = &mut node.payload {
                                material.albedo = clay_alb;
                                material.roughness = 0.70;
                                material.metallic = 0.0;
                                material.cavity_dirt.enabled = true;
                                material.cavity_dirt.amount = 0.55;
                            }
                        }
                        s.mark_dirty(DirtyFlags::RENDER);
                    });
                }
            }
        }
        if let Some(r) = act_remesh_to { self.voxel_resample_to(r); }
        if act_open_remesh { self.voxel_open_remesh(); }
    }

    /// Brush picker for a PolySculpt Edit session (no voxel object).
    pub(crate) fn draw_poly_sculpt_panel(&mut self, ctx: &egui::Context) {
        use core_math::voxel_sculpt::VoxelBrush;
        if self.tool.work_mode != WorkMode::Sculpt || self.sculpt.poly_edit.is_none() {
            return;
        }
        if self.sculpt.poly_radius <= 1e-4 {
            self.sculpt.poly_radius = 0.15;
        }
        if self.sculpt.poly_strength <= 1e-4 {
            self.sculpt.poly_strength = 1.0;
        }
        let cur = self
            .sculpt
            .poly_brush
            .unwrap_or(VoxelBrush::Draw);
        egui::Window::new("\u{1F58C} PolySculpt")
            .resizable(true)
            .default_size([280.0, 360.0])
            .min_size([220.0, 160.0])
            .default_pos([12.0, 90.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("Same brushes as the sculpt room — they displace this mesh.")
                        .small()
                        .weak(),
                );
                // PR-0 (ZB plan §11.2): live stroke costs. pick is the O(F) walk PR-4
                // kills; upload_bytes ≈ VB size until PR-3 scatter lands. Amber = the
                // frame is O(mesh) somewhere.
                {
                    let p = self.sculpt.poly_profile;
                    if p.seed_count > 0 {
                        let mode = match p.normals_mode {
                            2 => "full",
                            1 => "region",
                            _ => "-",
                        };
                        let hot = p.pick_ms > 8.0
                            || p.upload_ms > 8.0
                            || p.normals_ms > 8.0
                            || p.normals_mode == 2;
                        let col = if hot {
                            egui::Color32::from_rgb(230, 160, 80)
                        } else {
                            egui::Color32::from_rgb(120, 160, 130)
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "pick {:.1} · query {:.2} · kernel {:.2} · normals {:.1} ({mode}) · upload {:.1}ms {:.1}MB · {} seeds",
                                p.pick_ms, p.query_ms, p.kernel_ms, p.normals_ms,
                                p.upload_ms, p.upload_bytes as f32 / 1e6, p.seed_count,
                            ))
                            .small()
                            .color(col),
                        )
                        .on_hover_text(
                            "PolySculpt stroke costs (ZB_RESPONSIVE_SCULPT_PLAN). Amber: an \
                             O(mesh) stage is hot — pick/upload/normals should be O(brush).",
                        );
                    }
                }
                ui.separator();
                ui.label(egui::RichText::new("Brush").strong().small());
                let brushes: [(VoxelBrush, &str, &str); 12] = [
                    (VoxelBrush::Draw, "Draw", "Deposit along the surface normal"),
                    (VoxelBrush::Erase, "Erase", "Carve (or hold Ctrl)"),
                    (VoxelBrush::Smooth, "Smooth", "Relax (or hold Shift)"),
                    (VoxelBrush::Flatten, "Flatten", "Push toward the contact plane"),
                    (VoxelBrush::Clay, "Clay", "Even buildup"),
                    (VoxelBrush::Scrape, "Scrape", "Shave above the plane"),
                    (VoxelBrush::Crease, "Crease", "Sharp groove (Ctrl = ridge)"),
                    (VoxelBrush::ClayBuildup, "Build", "Clay buildup strips"),
                    (VoxelBrush::Inflate, "Inflate", "Puff along the normal"),
                    (VoxelBrush::Pinch, "Pinch", "Sharpen toward the stroke"),
                    (VoxelBrush::Move, "Move", "Grab / pull"),
                    (VoxelBrush::SnakeHook, "Snake", "Pull a tube"),
                ];
                egui::Grid::new("poly_sculpt_brush_grid")
                    .num_columns(3)
                    .spacing([4.0, 4.0])
                    .show(ui, |ui| {
                        for (i, (b, name, tip)) in brushes.iter().enumerate() {
                            // Snake Hook has no poly implementation (its tube is voxel-chunk
                            // machinery; the shared dab kernel has no arm) — greyed fail-closed,
                            // same pattern as the voxel grid's mode gating above. The stamp
                            // path gates it too (`handle_poly_sculpt_input`).
                            let unsupported =
                                matches!(*b, VoxelBrush::SnakeHook);
                            let resp = ui.add_enabled(
                                !unsupported,
                                egui::SelectableLabel::new(cur == *b, *name),
                            );
                            let resp = if unsupported {
                                resp.on_disabled_hover_text(
                                    "Snake Hook is not in poly sculpt yet — Voxel Surface mode only.",
                                )
                            } else {
                                resp.on_hover_text(*tip)
                            };
                            if resp.clicked() {
                                self.sculpt.poly_brush = Some(*b);
                            }
                            if (i + 1) % 3 == 0 {
                                ui.end_row();
                            }
                        }
                    });
                ui.add_enabled(false, egui::Label::new(
                    egui::RichText::new("Fill / Smudge — density-only").small().weak(),
                ));
                ui.separator();
                self.draw_tool_params_sculpt(ui);
                ui.separator();
                if ui.button("Done").clicked() {
                    self.poly_sculpt_leave_edit();
                }
            });
    }
}

/// Slider + compact DragValue for precise type/scroll entry (Direction knobs, etc.).
/// Left: range slider (label). Right: numeric drag (same value).
fn sculpt_slider_drag(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    step: f64,
    drag_speed: f64,
) -> egui::Response {
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::Slider::new(value, range.clone())
                .text(text)
                .step_by(step)
                .show_value(false),
        );
        let dr = ui.add(
            egui::DragValue::new(value)
                .speed(drag_speed)
                .range(range)
                .fixed_decimals(2),
        );
        r.union(dr)
    })
    .inner
}

/// sRGB(0..1) -> linear(0..1). Local helpers so the sculpt panel converts colours at the UI<->data
/// boundary without depending on the private converters in properties_sdf_material.rs.
fn srgb_to_linear_u(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn linear_to_srgb_u(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}
