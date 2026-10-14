// core_engine/app/src/app_update.rs
//
// DE-GOD ROUND 1 — slice 32 (master): the eframe::App trait impl (raw_input_hook + the now-slim
// update()) and the slice-31 `init_gpu_ctx` inherent method, moved verbatim out of main.rs.
// Trait impls are crate-global, so relocating `impl eframe::App for FourthLayerApp` here changes
// nothing at the call site; `init_gpu_ctx` is an inherent method that resolves crate-wide.
// Plan: docs/planning/APP_CRATE_DEGOD_PLAN.md.

use eframe::egui;
use eframe::wgpu;
use crate::*;

// ==================================================================
// Main update loop
// ==================================================================
impl eframe::App for FourthLayerApp {
    /// Strip the **Mode Pie** key (default Tab = ACT_MODE_PIE) from raw input before egui
    /// processes focus cycling. Rebindable: whichever chord is bound to Mode Pie is stripped.
    /// Stashed into `raw_mode_pie_events` so `update()` can open/hold the pie.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.input_system.raw_mode_pie_events.clear();
        if crate::visdiff::harness_owns_input() {
            // The harness window is a normal (maximized) desktop window: stray user clicks and
            // keys would switch shading modes / move the camera mid-capture and invalidate the
            // pixels. While AZRA_VISDIFF drives the app, human input never reaches it.
            raw_input.events.retain(|ev| !matches!(ev,
                egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Copy | egui::Event::Cut
                | egui::Event::Paste(_) | egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_)
                | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } | egui::Event::Zoom(_)
                | egui::Event::Touch { .. } | egui::Event::PointerGone));
            raw_input.modifiers = egui::Modifiers::NONE;
            return;
        }
        if self.input_system.any_text_focused {
            return;
        }
        let bind = self.keybindings.get(ACT_MODE_PIE).clone();
        raw_input.events.retain(|ev| {
            if let egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } = ev
            {
                if *key != bind.key {
                    return true;
                }
                // Press: only intercept when modifiers match the bind (Ctrl+Tab ≠ plain Tab).
                // Release: always intercept this key so hold-state can clear.
                if *pressed {
                    if !bind.matches_raw(*key, modifiers) {
                        return true;
                    }
                }
                self.input_system.raw_mode_pie_events.push(ev.clone());
                return false;
            }
            true
        });
    }

    /// App-exit flush for the debounced ui2 layout save (a change made less than
    /// the debounce window before closing would otherwise be lost).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.ui2_layout_flush_on_exit();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.diag_frame += 1;
        // Flight recorder (vis_recorder): mirror the existing frame counter into the static so
        // BodyVolumeCache-internal visrec sites stamp events with the same frame numbers.
        crate::vis_recorder::set_vis_frame(self.diag_frame);
        let wall_start = std::time::Instant::now();
        profile_scope!("frame_update");
        let _perf_frame_t0 = std::time::Instant::now();
        self.perf.begin_frame();
        // Starvation sentinel, sampled BEFORE any frame work. Re-sampled in the [STALL]
        // branch; the pair tells whether a slow frame was starved (sentinel inflates with
        // it) or genuinely spent in the measured buckets (sentinel flat). ~60µs cost.
        let sentinel_start_ms = crate::profiling::sentinel_ms();
        // AZRA_VISDIFF harness (visdiff.rs): self-driving visual verification. Env-gated.
        if std::env::var("AZRA_VISDIFF").is_ok() {
            self.visdiff_tick();
            ctx.request_repaint(); // keep frames flowing even when idle
        }
        // AZRA_PARITY gate (parity_gate.rs): raster-vs-PT brightness parity. Env-gated.
        if crate::parity_gate::parity_active() {
            self.parity_gate_tick();
            ctx.request_repaint();
        }
        // AZRA_SELDIAG harness (seldiag.rs): selection-highlight orbit sweep. Env-gated.
        if std::env::var("AZRA_SELDIAG").is_ok() {
            self.seldiag_tick(ctx);
        }
        // Flicker-capture harness: its OWN env gate (was nested under AZRA_VISDIFF —
        // never fired without it).
        if std::env::var("AZRA_FLICKER_SHOTS").is_ok() {
            self.flicker_shots_tick();
            ctx.request_repaint();
        }
        // AZRA_NURBSDIFF harness (nurbsdiff.rs): NURBS tool-path shots. Env-gated.
        // Owner 2026-08-01: no NURBS tool milestone without a harness shot.
        if std::env::var("AZRA_NURBSDIFF").is_ok() {
            self.nurbsdiff_tick();
            ctx.request_repaint();
        }
        // AZRA_MATCAP_DIAG (visdiff.rs): owner "PBR progressive still renders in MatCap"
        // repro. Deliberately NO unconditional request_repaint here — the tick measures
        // the app's OWN idle cadence after the mode switch; a forced pump would mask it.
        if std::env::var("AZRA_MATCAP_DIAG").is_ok() {
            self.matcap_diag_tick(ctx);
        }
        // AZRA_KNIFEDIAG (visdiff.rs): owner "knife slice always fails" — drives the real
        // knife input path on a created box and screenshots the cut.
        if std::env::var("AZRA_KNIFEDIAG").is_ok() {
            self.knifediag_tick(ctx);
        }
        // AZRA_BRIDGEDIAG (bridgediag.rs): owner "bridge does not work on multi select" —
        // drives the REAL poly_bridge() tool entry on an N-loop / N-face selection.
        if std::env::var("AZRA_BRIDGEDIAG").is_ok() {
            self.bridgediag_tick(ctx);
        }
        // AZRA_SOAK harness (soak.rs): scripted production-API session + pixel verdicts.
        // Zero cost when unset — same single env check pattern as VISDIFF.
        if std::env::var("AZRA_SOAK").is_ok() {
            self.soak_tick();
            ctx.request_repaint();
        }
        // D0 scoreboard / stress: AZRA_QUIT_AFTER_SECS=N → clean exit after N seconds
        // (lets unattended AZRA_SCOREBOARD + AZRA_DENSE_STRESS runs finish without kill -9).
        {
            static QUIT_DEADLINE: std::sync::OnceLock<Option<std::time::Instant>> =
                std::sync::OnceLock::new();
            let deadline = QUIT_DEADLINE.get_or_init(|| {
                std::env::var("AZRA_QUIT_AFTER_SECS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|&n| n > 0)
                    .map(|n| {
                        eprintln!("[QUIT] AZRA_QUIT_AFTER_SECS={n} — will exit cleanly");
                        std::time::Instant::now() + std::time::Duration::from_secs(n)
                    })
            });
            if let Some(t) = *deadline {
                if std::time::Instant::now() >= t {
                    eprintln!("[QUIT] deadline reached — exit 0");
                    std::process::exit(0);
                }
                ctx.request_repaint();
            }
        }
        self.scheduler.begin_frame();
        // Clear per-frame warming hover (set by outliner draw, read by poll_warming)
        self.cache.warming.hovered_body = None;

        // Progressive GPU pipeline compilation — one pipeline per frame
        if self.gpu_init.is_some() {
            self.init_gpu_ctx();
        }
        // NOTE: poll_background_tasks was here but moved to before egui panels.
        // All GPU resource mutations (atlas grow, texture replace) must complete
        // before viewport_render creates bind groups. See commit message.
        if !self.ui.toasts.is_empty(){ctx.request_repaint_after(std::time::Duration::from_millis(16));}
        // Update window title with filename + modified indicator — ONLY when it changed.
        // send_viewport_cmd requests an immediate repaint (egui context.rs), so the old
        // unconditional per-frame send made every frame schedule the next: a permanent
        // full-rate busy loop in every mode (the "idle floor" / owner "MatCap slow"
        // 2026-08-07, pinned by ctx.repaint_causes() in the AZRA_MATCAP_DIAG harness).
        let title = self.window_title();
        if title != self.project.last_window_title {
            self.project.last_window_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        // UI v2 flag (clean ship-ready shell, gated behind AZRA_UI=v2). Old UI stays default.
        let ui_v2 = std::env::var("AZRA_UI").ok().as_deref() == Some("v2");

        // Apply theme settings — only when the style key changes.
        // Per-frame clone + set_style was 0.2–0.8 ms of the full-UI idle floor (ui-floor review).
        // v2 tokens are static; v1 re-applies when dark_mode / font_size change.
        let style_key: u64 = if ui_v2 {
            1 // AZRA token style (fonts installed in AzraApp::new)
        } else {
            let theme = &self.layout.theme;
            2u64
                | (if theme.dark_mode { 1 << 8 } else { 0 })
                | ((theme.font_size.to_bits() as u64) << 16)
        };
        if self.ui.egui_style_key != style_key {
            if ui_v2 {
                ctx.set_visuals(egui::Visuals::dark());
                let mut style = (*ctx.style()).clone();
                ui_theme::apply_style(&mut style);
                ctx.set_style(style);
            } else {
                let theme = &self.layout.theme;
                if theme.dark_mode {
                    ctx.set_visuals(egui::Visuals::dark());
                } else {
                    ctx.set_visuals(egui::Visuals::light());
                }
                let mut style = (*ctx.style()).clone();
                style.text_styles.insert(
                    egui::TextStyle::Body,
                    egui::FontId::proportional(theme.font_size),
                );
                style.text_styles.insert(
                    egui::TextStyle::Button,
                    egui::FontId::proportional(theme.font_size),
                );
                ctx.set_style(style);
            }
            self.ui.egui_style_key = style_key;
        }

        // Rebuild shader pipelines if node graph changed
        if self.shader_dirty { self.rebuild_shader_pipelines(); }

        // Auto-save check
        self.check_auto_save();

        // Evict stale material preview textures (only when cache is large)
        if self.cache.mat_preview_cache.textures.len() > 64 {
            let active: Vec<u64> = self.scene.primitives.iter()
                .map(|p| mat_preview::material_hash(&p.material))
                .collect();
            self.cache.mat_preview_cache.retain(&active);
        }

        // Mode Pie key was stripped in raw_input_hook; reconstruct pressed/held from stash.
        let mode_pie_bind = self.keybindings.get(ACT_MODE_PIE).clone();
        let mode_pie_pressed = self.input_system.raw_mode_pie_events.iter().any(|ev| {
            matches!(
                ev,
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if mode_pie_bind.matches_raw(*key, modifiers)
            )
        });
        for ev in &self.input_system.raw_mode_pie_events {
            if let egui::Event::Key { key, pressed, .. } = ev {
                if *key == mode_pie_bind.key {
                    self.input_system.mode_pie_held = *pressed;
                }
            }
        }
        // Remember whether a text field has focus for the next raw_input_hook call
        self.input_system.any_text_focused = ctx.wants_keyboard_input();

        // Drain startup toast (set during new() before toast system was available)
        if let Some(msg) = self.ui.startup_toast.take() { self.toast(&msg); }

        // Derive modeling context from selection (before any tool/UI reads it)
        self.derive_modeling_context();
        // Resolve work plane for this frame (before any tool reads it)
        self.resolve_workplane();

        // Drop handles that were moved to dropping last frame — now safe (GPU submitted previous frame)
        for h in &mut self.ai.ai_tex_dropping { *h = None; }

        // POLY_VIEWPORT_PERF P0/P1a: latch last-frame modifier eval count for the Perf HUD,
        // then reset. Orbit with a selected stacked poly body must leave this at 0.
        self.poly_modifier_evals_last_frame = self.poly_modifier_evals_this_frame;
        self.poly_modifier_evals_this_frame = 0;
        // PLAN_FOR_GROK_VIEWPORT_PERF: sync call/mark counters for coalescing proof.
        self.poly_sync_calls_last_frame = self.poly_sync_calls_this_frame;
        self.poly_sync_calls_this_frame = 0;
        self.poly_sync_marks_last_frame = self.poly_sync_marks_this_frame;
        self.poly_sync_marks_this_frame = 0;
        // CONSOLE_AUDIT A1: reset extrude / pick-cache diagnostics for this frame.
        self.poly_pick_rebuilds_this_frame = 0;
        self.poly_pick_rebuild_ms_this_frame = 0.0;
        self.extrude_perf_calls = 0;
        self.extrude_perf_delta_ms = 0.0;
        self.extrude_perf_sync_ms = 0.0;
        self.extrude_perf_total_ms = 0.0;

        // Pre-panel setup so far (derive_modeling, workplane, counters, …) — Gate 0 STALL split.
        let _pre_panel_ms = wall_start.elapsed().as_secs_f32() * 1000.0;
        self.ui.cpu_section_ms[9] = _pre_panel_ms;

        // Poll all background tasks BEFORE any egui panel draws.
        // Critical ordering: dispatch_pending_primary_bakes() may trigger atlas
        // grow_depth() which replaces the 3D texture. This MUST complete before
        // viewport_render() creates bind groups referencing atlas.texture.
        // Previously at end of update() — caused stale bind group crashes.
        let _t_poll = std::time::Instant::now();
        time_block!("poll_background_tasks", self.poll_background_tasks(ctx));
        self.ui.cpu_section_ms[8] = _t_poll.elapsed().as_secs_f32() * 1000.0;

        // ---- MENU BAR ----
        // v2: categorized menus live in the app bar (drawn in ui2_draw).
        // v1: classic draw_menu_bar.
        // D0 measure: AZRA_MINIMAL_UI=1 (or dense stress) skips shell chrome so scoreboard
        // `lay=` is not polluted by panel work — GPU/tri cost must be visible on its own.
        // AZRA_FULL_UI=1 overrides the stress-harness minimal-UI default: the PERF_TARGETS
        // matrix cells vs Blender are full-UI numbers, so full-UI must be measurable
        // under stress spawns too (Blender ran its full shell).
        let minimal_ui = (std::env::var_os("AZRA_MINIMAL_UI").is_some()
            || std::env::var_os("AZRA_DENSE_STRESS").is_some()
            || std::env::var_os("AZRA_INSTANCE_STRESS").is_some()
            || std::env::var_os("AZRA_MULTI_MESH_STRESS").is_some())
            && std::env::var_os("AZRA_FULL_UI").is_none();
        let _t_layout = std::time::Instant::now();
        let (do_import_obj, do_import_gltf) = if minimal_ui || ui_v2 {
            (false, false)
        } else {
            self.draw_menu_bar(ctx)
        };

        // ---- Import glTF as mesh ----
        if do_import_gltf {
            self.import_gltf_dialog();
        }

        // ---- Import OBJ as SDF ----
        if do_import_obj {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Wavefront OBJ", &["obj"])
                .set_title("Import OBJ as SDF Volume")
                .pick_file()
            {
                let path_str = path.to_string_lossy().to_string();
                let res    = self.cache.cpu_volume.resolution[0];
                let bounds = self.cache.cpu_volume.grid_bounds;
                match io_bridge::mesh_to_sdf::obj_to_sdf(&path_str, res, bounds) {
                    Ok(result) => {
                        let mesh_name = result.mesh_name.clone();
                        let vertex_count = result.vertex_count;
                        let face_count = result.face_count;
                        self.transaction("Import OBJ to SDF", |s| {
                            s.cache.cpu_volume.data = result.data;
                            s.cache.cpu_volume.mark_dirty([0, 0, 0], [res - 1, res - 1, res - 1]);
                            // Clear SDF prims from graph (source of truth)
                            if let Some(prims) = s.scene.scene_graph.body_primitives_mut(s.scene.sdf_body_nid) {
                                prims.clear();
                            }
                            // Rebuild flat cache from graph (canonical path)
                            s.sync_flat_cache();
                            s.deselect_all();
                            s.voxel_undo_reset();
                            s.sculpt.current_stroke.clear();
                            s.sculpt.stroke_active = false;
                            // Sculpt mode entered
                            s.tool.work_mode = WorkMode::Sculpt;
                            // Structural change: prims cleared → SCENE (not just RENDER)
                            s.mark_dirty(DirtyFlags::SCENE);
                        });
                        self.toast(&format!(
                            "\u{2705} {} \u{2014} {}v / {}f \u{2192} {}\u{00B3} SDF \u{2014} pr\u{00EA}t \u{00E0} sculpter",
                            mesh_name, vertex_count, face_count, res
                        ));
                    }
                    Err(e) => { self.toast(&format!("\u{274C} Import OBJ: {}", e)); }
                }
            }
        }

        // ---- Windows (extracted to ui_windows.rs) ----
        let _t_lay2 = std::time::Instant::now();
        // [LAY3] drill-down: name the single most expensive window fn this frame.
        let mut _lay3_worst: (&str, f32) = ("", 0.0);
        macro_rules! wtime {
            ($name:literal, $call:expr) => {{
                let _t = std::time::Instant::now();
                $call;
                let _ms = _t.elapsed().as_secs_f32() * 1000.0;
                if _ms > _lay3_worst.1 { _lay3_worst = ($name, _ms); }
            }};
        }
        if !minimal_ui {
            // Skip the whole aux-window fan-out when every window is closed (common case).
            // Each draw_* still early-returns, but the call + Instant + flag reads add up.
            let any_aux = self.ui.show_keybind_settings
                || self.ui.show_experimental
                || self.ui.show_about
                || self.ui.show_shortcut_editor
                || self.ui.show_export_window
                || self.ui.show_gltf_export
                // show_tweak_panel drawn AFTER ui2_draw (same class as node editor) so docks
                // never paint over Developer Tweaks (owner 2026-08-06).
                || self.ui.show_stress_test
                || self.ui.show_layout_settings
                || self.ui.show_macro_manager
                || self.ui.show_panel_list
                || self.ui.show_node_editor
                || self.ui.show_pt_settings
                || self.ui.show_texgen_window
                || self.ui.convert_sdf_dialog.is_some()
                || self.ui.mesh_cleanup_dialog.is_some()
                || self.ui.mesh_cleanup_result.is_some()
                || self.ui.pending_action_after_save.is_some()
                || self.ui.command_palette.open
                || self.ai.show_ai_tex_window
                || self.ai.show_beauty_window;
            if any_aux {
                wtime!("draw_keybind_settings", self.draw_keybind_settings(ctx));
                wtime!("draw_experimental_settings", self.draw_experimental_settings(ctx));
                wtime!("draw_about_window", self.draw_about_window(ctx));
                wtime!("draw_shortcut_editor", self.draw_shortcut_editor(ctx));
                wtime!("draw_agent_settings", self.draw_agent_settings(ctx));
                wtime!("draw_code_review", self.draw_code_review(ctx));
                // NODE EDITOR + DEVELOPER TWEAKS draw order: after `ui2_draw` (see below).
                // egui paints Areas in creation order within a layer; a SidePanel created LATER
                // covers a floating Window created earlier — drawing them here put the dock on
                // top. They are drawn after the shell so they float above modeless tool windows.
                wtime!("draw_export_window", self.draw_export_window(ctx));
                wtime!("draw_gltf_export_window", self.draw_gltf_export_window(ctx));
                wtime!("draw_texture_generator", self.draw_texture_generator(ctx));
                wtime!("draw_ai_texture_window", self.draw_ai_texture_window(ctx));
                wtime!("draw_ai_beauty_window", self.draw_ai_beauty_window(ctx));
                wtime!("draw_grid_settings", self.draw_grid_settings(ctx));
                wtime!("draw_workplane_settings", self.draw_workplane_settings(ctx));
                wtime!("draw_unsaved_dialog", self.draw_unsaved_dialog(ctx));
                wtime!("draw_convert_sdf_dialog", self.draw_convert_sdf_dialog(ctx));
                // Drop live cleanup session if the user left the tool without Apply/Cancel.
                self.mesh_cleanup_poll_session();
                wtime!("draw_mesh_cleanup_dialog", self.draw_mesh_cleanup_dialog(ctx));
                wtime!(
                    "draw_mesh_cleanup_result_dialog",
                    self.draw_mesh_cleanup_result_dialog(ctx)
                );
                wtime!("draw_pt_settings_window", self.draw_pt_settings_window(ctx));
                wtime!("draw_stress_test_window", self.draw_stress_test_window(ctx));
                wtime!("draw_layout_settings", self.draw_layout_settings(ctx));
                wtime!("draw_macro_manager", self.draw_macro_manager(ctx));
                wtime!("draw_command_palette", self.draw_command_palette(ctx));
                wtime!("draw_panel_list", self.draw_panel_list(ctx));
            }
            self.drain_deferred_dialog_action(ctx); // CR-70b: execute deferred post-dialog actions
            self.poll_pending_file_open(); // CR-70b: poll background file-open dialog
            // poly create adjust: docked in Tool Parameters only (no floating duplicate).
            // Self-gating — cheap when closed.
            self.draw_poly_inset_adjust(ctx);  // Blender-style "adjust last operation" panel (inset)
            self.draw_poly_bevel_adjust(ctx);  // Blender-style "adjust last operation" panel (bevel resize)
            self.draw_poly_circle_adjust(ctx); // Blender-style "adjust last operation" panel (circle)
            self.draw_poly_tosphere_adjust(ctx); // Blender-style "adjust last operation" panel (to sphere)
            self.draw_poly_boolean_adjust(ctx); // Blender-style "adjust last operation" panel (boolean material)
            self.draw_nurbs_fillet_adjust(ctx); // NURBS fillet radius re-solve (FaceOrigin path)
            self.draw_array_dup_toolbar(ctx);  // Shift+D array session (scroll / C / I / M)
        }
        if _lay3_worst.1 > 2.0 && std::env::var_os("AZRA_SCOREBOARD").is_some() {
            eprintln!("[LAY3] worst window fn: {} = {:.2} ms", _lay3_worst.0, _lay3_worst.1);
        }
        let _lay2_windows = _t_lay2.elapsed().as_secs_f32() * 1000.0;

        // ---- KEYBOARD ----
        let _t_lay2 = std::time::Instant::now();
        if !minimal_ui {
            self.handle_keyboard(ctx, mode_pie_pressed);
        }
        let _lay2_kb = _t_lay2.elapsed().as_secs_f32() * 1000.0;
        let _t_lay2 = std::time::Instant::now();

        if !minimal_ui {
            if ui_v2 {
                // ---- NEW UI (clean shell) ----
                self.ui2_draw(ctx);
                // Floating tool windows go AFTER the shell so they are not painted over by the dock.
                // Note: Developer Tweaks draws even later (after dock re-paint + with context
                // menus) so ui2_repaint_dock_over_library cannot cover it.
                self.draw_node_editor_window(ctx);
                // After ui2_draw so left_rects / right_rects / viewport_hole match this frame.
                if self.ui.request_ui_layout_dump {
                    self.ui.request_ui_layout_dump = false;
                    self.dump_ui2_layout(ctx);
                }
            } else {
                // ---- VIEWPORT HEADER BAR ----
                if self.panel_layout.is_visible(panel::PanelId::ViewportHeader) {
                    self.draw_viewport_header(ctx);
                }

                // ---- LEFT TOOL STRIP + SETTINGS ----
                if self.panel_layout.is_visible(panel::PanelId::LeftToolStrip) {
                    self.draw_left_tool_strip(ctx);
                }
                if self.panel_layout.is_visible(panel::PanelId::ToolProperties) {
                    self.draw_create_tool_settings(ctx);
                    self.draw_linear_array_tool_settings(ctx);
                    self.draw_extract_intersection_settings(ctx);
                    self.draw_poly_tool_properties(ctx);
                }
                // LAY4 probe: name the panel inside the `shell+libs` bucket that
                // scales with scene size (20k-node demo scene: lay ≈ 4-5 ms while
                // the whole GPU frame is 1.2 ms — the UI, not the renderer).
                let _t_right = std::time::Instant::now();
                self.draw_right_panel(ctx);
                let _lay4_right = _t_right.elapsed().as_secs_f32() * 1000.0;

                // ---- AGENT COMMAND BAR (floating) ----
                if self.panel_layout.is_visible(panel::PanelId::AgentBar) {
                    self.draw_agent_bar(ctx);
                }

                // ---- CONTENT LIBRARY (bottom panel) ----
                let _t_lib = std::time::Instant::now();
                self.draw_content_library(ctx);
                self.handle_library_drag_drop(ctx);
                let _lay4_lib = _t_lib.elapsed().as_secs_f32() * 1000.0;
                if (_lay4_right > 1.0 || _lay4_lib > 1.0)
                    && std::env::var_os("AZRA_SCOREBOARD").is_some()
                {
                    use std::cell::Cell;
                    thread_local! { static L4: Cell<Option<std::time::Instant>> = Cell::new(None); }
                    let now = std::time::Instant::now();
                    let due = L4.with(|c| {
                        let d = c.get().map_or(true, |t| now.duration_since(t).as_secs_f32() >= 2.0);
                        if d { c.set(Some(now)); }
                        d
                    });
                    if due {
                        eprintln!("[LAY4] right_panel={_lay4_right:.2} content_library={_lay4_lib:.2}");
                    }
                }
            }

            // ---- BODY LIBRARY (kitbash) — save dialog + browser + drag-drop ----
            self.draw_library_save_dialog(ctx);
            self.draw_body_library(ctx);
            self.handle_body_library_drag_drop(ctx);
        }

        let _layout_ms = _t_layout.elapsed().as_secs_f32() * 1000.0;
        // Scoreboard drill-down for the `lay` bucket (PERF_TARGETS residual: ~6-8 ms with a
        // dense body in scene, deselected): windows list vs keyboard vs shell draw.
        if _layout_ms > 4.0 && std::env::var_os("AZRA_SCOREBOARD").is_some() {
            let shell = _t_lay2.elapsed().as_secs_f32() * 1000.0;
            use std::cell::Cell;
            thread_local! { static LAST: Cell<Option<std::time::Instant>> = Cell::new(None); }
            let now = std::time::Instant::now();
            let fire = LAST.with(|c| {
                let due = c.get().map(|t| now.duration_since(t).as_secs_f32() >= 2.0).unwrap_or(true);
                if due { c.set(Some(now)); }
                due
            });
            if fire {
                // Note: interactive v2 dock paint is timed later (post-CentralPanel) and
                // folded into `cpu_section_ms[5]`; this total is shell+windows only.
                eprintln!(
                    "[LAY2] shell_total={_layout_ms:.1} windows={_lay2_windows:.2} keyboard={_lay2_kb:.2} shell+libs={shell:.2} (dock paint timed post-Central as [LAY-DOCK])"
                );
            }
        }
        let mut was_dirty = self.is_dirty;
        // ui2: frameless CentralPanel so the viewport fills right up to the dock edges (the default
        // central-panel frame's inner margin was the "dark space" gap between panel and viewport).
        let central = egui::CentralPanel::default();
        let central = if ui_v2 { central.frame(egui::Frame::none()) } else { central };
        central.show(ctx,|ui|{
            // ui2: allocate the cell with HOVER only, then put camera/gizmo drag on an
            // inset rect so the 6px dock seams (and library hsplit) are never owned by
            // the viewport widget. After add_primitive, gizmo_armed=true turns ANY
            // primary press on the old full-area click_and_drag into a prim drag —
            // which ate the resize seams (owner: "can't resize after adding primitive").
            let (full, _cell) = if ui_v2 {
                ui.allocate_exact_size(ui.available_size(), egui::Sense::hover())
            } else {
                ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag())
            };
            // Library + Model: use CentralPanel cell (`full`) as the horizontal
            // ground truth (already between SidePanels). Still intersect with
            // viewport_hole so we never expand past measured dock edges.
            let hole = if ui_v2 {
                let h = self.ui2.viewport_hole;
                if full.width() > 8.0 && h.width() > 8.0 {
                    full.intersect(h)
                } else if full.width() > 8.0 {
                    full
                } else {
                    h
                }
            } else {
                full
            };
            let library_in_dock = ui_v2
                && self
                    .ui2
                    .dock
                    .iter()
                    .any(|group| group.tabs.contains(&crate::ui2::PanelKind::Library));
            // ui2: the attached Library claims the bottom of the hole. A detached
            // Library is already accounted for by the shared dock layout.
            let rect = if ui_v2 {
                if library_in_dock {
                    hole
                } else {
                    crate::library_panel::scene_rect(hole, &self.ui2.library)
                }
            } else {
                full
            };
            // Dual-pane split: resolve pane rects + which camera is active for input.
            // Invariant: `viewport.camera` is the active pane; `ui2.camera_b` is the other.
            let (pane_a, pane_b, input_rect) = if ui_v2 {
                if let Some(split) = self.ui2.vp_split {
                    if self.ui2.camera_b.is_none() {
                        self.ui2.camera_b = Some(self.viewport.camera);
                    }
                    let (ra, rb) = split.pane_rects(rect);
                    // Hover switches active pane (Blender-style); freeze switch while MMB orbits.
                    let mmb = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
                    if !mmb {
                        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                            let want = if rb.contains(pos) {
                                1u8
                            } else if ra.contains(pos) {
                                0u8
                            } else {
                                self.ui2.split_active_pane
                            };
                            if want != self.ui2.split_active_pane {
                                if let Some(other) = self.ui2.camera_b.as_mut() {
                                    std::mem::swap(&mut self.viewport.camera, other);
                                }
                                self.ui2.split_active_pane = want;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        }
                    }
                    let irec = if self.ui2.split_active_pane == 0 { ra } else { rb };
                    (Some(ra), Some(rb), irec)
                } else {
                    // Joined: drop secondary camera if it lingered.
                    if self.ui2.camera_b.is_some() {
                        self.ui2.camera_b = None;
                        self.ui2.split_active_pane = 0;
                    }
                    (None, None, rect)
                }
            } else {
                (None, None, rect)
            };
            // Drag hit = scene minus dock-edge strips. While a dock edge drag is
            // active, give the viewport NO drag sense so gizmo/camera cannot steal.
            let response = if ui_v2 {
                let hit = crate::ui2::DOCK_EDGE_HIT;
                // When split, interact on the full scene so either pane can receive
                // clicks; camera/gizmo math uses `input_rect` (active pane).
                let drag_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + hit, rect.top()),
                    egui::pos2((rect.right() - hit).max(rect.left() + hit + 1.0), rect.bottom()),
                );
                let sense = if self.ui2.edge_drag.is_some() {
                    egui::Sense::hover()
                } else {
                    egui::Sense::click_and_drag()
                };
                ui.interact(drag_rect, ui.id().with("v2_vp_drag"), sense)
            } else {
                _cell
            };
            // Library is painted AFTER scene overlays (below) so workplane/grid never
            // sit on top of the library band. Opaque belt still fills the zone first.
            if ui_v2 && hole.bottom() > rect.bottom() {
                let band = egui::Rect::from_min_max(
                    egui::pos2(hole.left(), rect.bottom()),
                    hole.right_bottom(),
                );
                ui.painter().rect_filled(band, 0.0, crate::ui_theme::SURFACE_PANEL);
            }
            // Any size change (dock hide/reopen, library collapse, window resize)
            // must rebuild GPU textures — compare integer px to ignore float noise.
            // B3d: require ≥2 px delta in either axis. 1 px A/B thrash (library seam /
            // dock layout fighting) was self-sustaining mark_dirty(RENDER) → was_dirty
            // spin + multi-ms library passes every other frame.
            let new_px = (
                input_rect.width().round() as i32,
                input_rect.height().round() as i32,
            );
            let old_px = (
                self.viewport.last_rect_size.x.round() as i32,
                self.viewport.last_rect_size.y.round() as i32,
            );
            let dw = (new_px.0 - old_px.0).unsigned_abs();
            let dh = (new_px.1 - old_px.1).unsigned_abs();
            if dw >= 2 || dh >= 2 {
                self.viewport.last_rect_size = input_rect.size();
                self.mark_dirty(DirtyFlags::RENDER);
            }
            self.viewport.viewport_rect = input_rect;
            self.ui.uv_host_rect = rect;

            let _diag_t0 = std::time::Instant::now();
            self.handle_viewport_input(ctx, ui, input_rect, &response);
            let _input_ms = _diag_t0.elapsed().as_secs_f32() * 1000.0;

            // ---- MESH CACHE (hybrid pipeline) ----
            let _diag_t0 = std::time::Instant::now();
            time_block!("poll_mesh_cache", self.poll_mesh_cache());
            time_block!("upgrade_mesh_lod", self.upgrade_mesh_lod());
            // PLAN_FOR_GROK_VIEWPORT_PERF P3: poly object-mode display LOD (rate-limited).
            {
                let t0 = std::time::Instant::now();
                time_block!("poll_poly_lod_policy", self.poll_poly_lod_policy());
                self.perf.poly_lod_ms = t0.elapsed().as_secs_f32() * 1000.0;
            }
            // SURFACE MODE auto-voxelize: if deferred Move grabs are pending and the ~2s idle timer has
            // elapsed (re-armed on each Move release), bake the accumulated stack into the density grid now
            // (3D-Coat's auto-voxelize after the last move). One-shot per arming; the flush clears the timer.
            if self.sculpt.voxel.as_ref().map_or(false, |v| v.move_voxelize_due.map_or(false, |t| std::time::Instant::now() >= t)) {
                time_block!("voxel_flush_pending_move", self.voxel_flush_pending_move());
            }
            // Drain a bounded batch of queued voxel chunk re-meshes (Frame Budget Rule). Marked by
            // the brush/undo (only the chunks the edit touched); freshly-meshed chunks upload in
            // the same frame via upload_pending below. No-op when nothing is queued.
            time_block!("voxel_flush_dirty_chunks", self.voxel_flush_dirty_chunks());
            // Ensure SDF body meshes exist when body vol cache is enabled and not dragging.
            // Gated: no mesh allocation when cache is off; no rebake during interaction.
            // Still remesh during FacePushPull / Elongate gizmo so half_extents /
            // elongation field edits show live (not only after a later gizmo move).
            // Pure translate/rotate gizmo keeps mesh frozen (instance move).
            let mesh_dragging = (self.tool.current.is_gizmo()
                && self.tool.gizmo_mode != GizmoMode::Elongate)
                || self.tool.current.is_transform()
                || self.tool.current.is_bevel()
                || self.tool.current.is_bool_edge_bevel()
                || self.tool.current.is_falloff_adjust();
            if self.cache.use_body_vol_cache && !mesh_dragging {
                time_block!("ensure_body_meshes", self.ensure_body_meshes());
                // P1b: post-DC mesh stack → folded mesh in mesh_cache (draw as CachedCpu).
                time_block!("ensure_body_mesh_mod_displays", self.ensure_body_mesh_mod_displays());
            }
            if self.render.viewport_mode == ViewportRenderMode::Hybrid {
                // PLAN_FOR_GROK_VIEWPORT_PERF P1: coalesce poly display marks from this
                // frame's tools into one sync per body before Hybrid mesh ensure/upload.
                // R6a order: ensure only *enqueues* recoveries into poly_display_dirty;
                // drain runs after and does at most one body this frame. Never reverse
                // (ensure must not see empty dirty and force-sync the whole backlog).
                time_block!("ensure_poly_meshes", self.ensure_poly_meshes());
                time_block!("poly_drain_display_dirty", self.poly_drain_display_dirty());
                time_block!("ensure_nurbs_meshes", self.ensure_nurbs_meshes());
            }
            // Always drain pending poly/NURBS/SDF mesh GPU uploads — not only Hybrid.
            // Poly bevel/extrude call update_editable_mesh every drag frame; if upload waited
            // for Hybrid-only, solid mesh lagged until orbit/move forced another path.
            // R6a: if display dirty remains (budgeted multi-body load), keep frames flowing
            // until the backlog drains — without spinning when empty.
            if !self.poly_display_dirty.is_empty() {
                ctx.request_repaint();
            }
            if let Some(gpu) = &self.gpu {
                time_block!("upload_pending", self.cache.mesh_cache.upload_pending(&gpu.device, &gpu.queue));
            }

            let _mesh_ms = _diag_t0.elapsed().as_secs_f32() * 1000.0;
            // ---- GPU RENDER ----
            was_dirty = was_dirty || self.is_dirty; // capture dirty from input/mesh before dispatch clears it
            let _t_ma = std::time::Instant::now();
            self.update_viewport_mode_auto();
            let mode_auto_ms = _t_ma.elapsed().as_secs_f32() * 1000.0;
            self.ui.cpu_section_ms[7] = mode_auto_ms;
            let _diag_t0 = std::time::Instant::now();
            let uv_full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            // Dual-pane: render View A → cache texture, render View B → main texture, paint both.
            // Single view: one dispatch into full scene rect (existing path).
            if let (Some(ra), Some(rb)) = (pane_a, pane_b) {
                // Only re-dispatch when dirty (orbit / scene / seam). Idle frames re-paint last textures.
                if self.is_dirty {
                    // Resolve absolute A/B cameras from active + other.
                    let cam_active = self.viewport.camera;
                    let cam_other = self.ui2.camera_b.unwrap_or(cam_active);
                    let (cam_a, cam_b) = if self.ui2.split_active_pane == 0 {
                        (cam_active, cam_other)
                    } else {
                        (cam_other, cam_active)
                    };

                    // --- View A ---
                    self.viewport.camera = cam_a;
                    self.viewport.viewport_rect = ra;
                    // Force a pass even if only B moved so A cache stays scene-coherent.
                    self.mark_dirty(DirtyFlags::RENDER);
                    self.dispatch_gpu_render(ctx, ra);
                    self.capture_split_pane_a();

                    // --- View B ---
                    self.viewport.camera = cam_b;
                    self.viewport.viewport_rect = rb;
                    self.mark_dirty(DirtyFlags::RENDER);
                    self.dispatch_gpu_render(ctx, rb);

                    // Restore active camera for tools / overlays / next-frame input.
                    self.viewport.camera = cam_active;
                    self.viewport.viewport_rect = input_rect;
                }

                // Paint both panes (live or last-frame cache).
                if let Some(gpu) = &self.gpu {
                    if gpu.pipelines.wgsl_pipelines_ready {
                        if let Some(tid_a) = gpu.split_pane_texture_id {
                            ui.painter().image(tid_a, ra, uv_full, egui::Color32::WHITE);
                        }
                        if let Some(tid_b) = gpu.texture_id {
                            ui.painter().image(tid_b, rb, uv_full, egui::Color32::WHITE);
                        }
                    }
                }
            } else {
                let _t_dg = std::time::Instant::now();
                self.dispatch_gpu_render(ctx, rect);
                let _dg_ms = _t_dg.elapsed().as_secs_f32() * 1000.0;
                // Phase C (depth-raycast): gbuf_normal is now written this frame — copy the cursor pixel for
                // the voxel sculpt brush position (collected next frame in poll, off the UI thread).
                let _t_vp = std::time::Instant::now();
                self.voxel_surface_pick(ctx, rect);
                // Scoreboard drill-down: `disp=` spikes need attribution between the render
                // dispatch and the surface pick (the two non-paint costs in this bucket).
                if std::env::var_os("AZRA_SCOREBOARD").is_some() {
                    let vp_ms = _t_vp.elapsed().as_secs_f32() * 1000.0;
                    if _dg_ms > 10.0 || vp_ms > 10.0 {
                        eprintln!("[DISP] dispatch_gpu_render={_dg_ms:.1} voxel_surface_pick={vp_ms:.1}");
                    }
                }
                // Draw viewport image (or fallback gradient while SDF compiles).
                // Tier 1: gate on the PRODUCTION readiness (lite + volume-preview), NOT the analytical
                // raymarch `pipeline` — which is now compiled lazily and stays None in the default workflow.
                if let Some(gpu)=&self.gpu{
                    if gpu.pipelines.wgsl_pipelines_ready && gpu.texture_id.is_some() {
                        if let Some(tid)=gpu.texture_id{
                            ui.painter().image(tid, rect, uv_full, egui::Color32::WHITE);
                        }
                    } else {
                        // Production SDF pipelines still compiling in background — show gradient fallback
                        let c = |f: &[f32;3]| egui::Color32::from_rgb((f[0]*255.0) as u8,(f[1]*255.0) as u8,(f[2]*255.0) as u8);
                        let top = c(&self.render.bg_color_top);
                        let bot = c(&self.render.bg_color_bottom);
                        let mut mesh = egui::Mesh::default();
                        mesh.colored_vertex(rect.left_top(), top);
                        mesh.colored_vertex(rect.right_top(), top);
                        mesh.colored_vertex(rect.right_bottom(), bot);
                        mesh.colored_vertex(rect.left_bottom(), bot);
                        mesh.add_triangle(0, 1, 2);
                        mesh.add_triangle(0, 2, 3);
                        ui.painter().add(egui::Shape::mesh(mesh));
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "⏳ Compiling SDF shaders...", egui::FontId::proportional(18.0), egui::Color32::from_white_alpha(140));
                    }
                }
            }
            let _dispatch_ms = _diag_t0.elapsed().as_secs_f32() * 1000.0;

            // ---- VIEWPORT OVERLAYS ----
            // v2: mockup scene chrome (label + tool-options) + split handles, then the
            // real scene overlays (gizmo, falloff, create HUD, selection…) so G/R/S and
            // create still work. Legacy floating tool bar / shading strip stay off —
            // those live in ui2 chrome instead.
            let _diag_t0 = std::time::Instant::now();
            // OVERLAY BUCKET SPLIT (owner perf 2026-07-29). This span was reported as
            // "overlays=14.0ms" and read as viewport-overlay cost — but it contains the library
            // panel, thumbnail ticking and dock construction too, and only ends with the two
            // actual overlay draws. Optimising `draw_viewport_overlays` on that number could
            // easily have yielded nothing. These sub-timers name the real spender.
            let mut _ov_chrome = 0.0f32;
            let mut _ov_scene = 0.0f32;
            let mut _ov_library = 0.0f32;
            let mut _ov_thumbs = 0.0f32;
            let mut _ov_viewport = 0.0f32;
            if ui_v2 {
                let _t = std::time::Instant::now();
                self.ui2_viewport_scene_chrome(ctx, rect);
                self.ui2_viewport_split_chrome(ctx, rect);
                _ov_chrome = _t.elapsed().as_secs_f32() * 1000.0;
                // Overlays on the active pane only (camera + projection match input_rect).
                let _t = std::time::Instant::now();
                self.ui2_draw_scene_overlays(ctx, ui, input_rect);
                _ov_scene = _t.elapsed().as_secs_f32() * 1000.0;
                // Library: real assets/ scan + thumbs, last so it stays above scene paint.
                // O2: scan only when open (closed = 6px strip; no folder/shown rebuild).
                let library_visible = self.ui2.library.open || library_in_dock;
                if library_visible {
                    self.ensure_library_scanned();
                }
                // Enable material apply when a poly body and/or SDF prim/body is selected.
                // Supersedes the older `active_material_body_nid() || active_prim_index_unified()`
                // pair — the helper also covers poly bodies, which that pair missed.
                self.ui2.library.can_apply = self.has_material_assign_target();
                // Multi-select count for right-click "Add to N selected objects" label.
                self.ui2.library.apply_target_count =
                    self.collect_library_material_import_targets().len();
                let _t_lib = std::time::Instant::now();
                let (_lib_over, lib_drop) = if library_in_dock {
                    (false, None)
                } else {
                    crate::library_panel::draw_in_central(
                        ui,
                        &mut self.ui2.library,
                        &mut self.library,
                        hole,
                    )
                };
                // Thumbnail decode/upload is UI-only idle work. Texture uploads run on the
                // frame thread, so never let them compete with active viewport interaction.
                if library_visible {
                    let want: Vec<usize> = crate::library_panel::visible_thumb_indices(
                        &self.ui2.library,
                    )
                    .to_vec();
                    let thumb_ticket = crate::frame_scheduler::WorkTicket {
                        class: crate::frame_scheduler::WorkClass::IdleOnly,
                        cost_hint_ms: 1.0,
                        name: "library_thumbnails",
                    };
                    if self.scheduler.can_run(&thumb_ticket) {
                        let _t_th = std::time::Instant::now();
                        self.library_tick_thumbs(ctx, &want);
                        _ov_thumbs = _t_th.elapsed().as_secs_f32() * 1000.0;
                        self.scheduler.record(&thumb_ticket, _ov_thumbs);
                    } else {
                        self.scheduler.record_skipped(&thumb_ticket);
                        ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    }
                }
                _ov_library = _t_lib.elapsed().as_secs_f32() * 1000.0;
                if let Some(drop) = lib_drop {
                    self.ui2_library_place(&drop, ctx);
                }
                // Right-click menu actions (find / fav / rename / delete / apply…).
                if let Some(action) = self.ui2.library.pending.take() {
                    self.handle_lib_ctx_action(action);
                }
                // Grip ⣿ menu: open another panel type as a floating window.
                if let Some((kind, anchor)) = self.ui2.library.pending_open_float.take() {
                    if kind == crate::ui2::PanelKind::Library {
                        self.ui2.library.open = false;
                    }
                    let anchor = anchor.unwrap_or_else(|| {
                        egui::pos2(ctx.screen_rect().center().x, ctx.screen_rect().bottom() - 80.0)
                    });
                    self.ui2_reveal_panel(ctx, kind, anchor);
                    let name = kind.label();
                    self.toast(&format!("Opened {} (floating)", name));
                }
            } else {
                let _t = std::time::Instant::now();
                // Input/tools above may have rebuilt the active poly mesh THIS frame (knife,
                // boolean): drop component ids that no longer name live elements before drawing.
                self.prune_poly_sel_to_live();
                self.draw_viewport_overlays(ctx, ui, rect);
                _ov_viewport = _t.elapsed().as_secs_f32() * 1000.0;
            }
            // 2D UV layout (Blender UV Editor style) — SPLITS the viewport, UV on the LEFT.
            //
            // Owner 2026-08-11: *"ALT U SHOULD BRING UV VIEW BY SPLITTING THE VIEWPORT, AND
            // SHOWING THE UV TO THE LEFT"*. It used to paint over the WHOLE viewport, which hides
            // the model you are unwrapping — the one thing you need to see while marking seams.
            // A split keeps both, and matches every other DCC's UV workflow.
            self.draw_uv_layout_split(ui, rect);
            // Neural Beauty look layer (on top of live viewport when enabled)
            self.draw_ai_beauty_viewport_overlay(ui, rect);
            let _overlays_ms = _diag_t0.elapsed().as_secs_f32() * 1000.0;
            // Report the split whenever this bucket is the frame's problem. Threshold, not
            // PERF_SPAM: the owner's report showed ~14ms EVERY idle frame, so spam-gating it
            // would hide the steady state — which is exactly the complaint.
            // Same env gate as everywhere else. This site used `== Ok("1")` while
            // `perf_spam_on()` (main.rs) uses `var_os(..).is_some()`, so with the variable
            // set to anything but "1" the two disagreed — which is how one log carried
            // 1,778 `[LIB]` lines against 44 `[Overlays]` lines and looked like the library
            // was spiking constantly. One spelling, one meaning.
            if _overlays_ms > 8.0 || crate::perf_spam_on() {
                eprintln!(
                    "[Overlays] {:.1}ms = chrome {:.1} + scene {:.1} + library {:.1} \
                     (thumbs {:.1}) + viewport {:.1} | poly_outline {:.1} | unattributed {:.1} | {}",
                    _overlays_ms, _ov_chrome, _ov_scene, _ov_library, _ov_thumbs, _ov_viewport,
                    self.poly_sel_outline_ms,
                    _overlays_ms - _ov_chrome - _ov_scene - _ov_library - _ov_viewport,
                    crate::profiling::perf_stamp());
            }
            // Store viewport section timings
            self.ui.cpu_section_ms[0] = _input_ms;
            self.ui.cpu_section_ms[1] = _mesh_ms;
            self.ui.cpu_section_ms[2] = _dispatch_ms;
            // [3] = submit_ms — set inside dispatch_gpu_render
            self.ui.cpu_section_ms[4] = _overlays_ms;
            self.ui.cpu_section_ms[5] = _layout_ms;
        });
        // v2: re-paint L/R docks ABOVE library/CentralPanel so Properties never
        // sits under the library band (multi-agent + owner screenshots).
        // SOLE interactive paint of Properties/Material — material slider commits land HERE,
        // AFTER the CentralPanel GPU dispatch. Without a same-frame re-dispatch, roughness/
        // metalness only appear after something that forces continuous dirty (e.g. PathTrace
        // progressive frames) — the owner's "works only after PathTrace then back to PBR".
        //
        // SidePanel earlier only reserved width; this is the real dock tree and was outside
        // `_layout_ms` → scoreboard `lay=` under-reported UI (unaccounted ≈ dock paint).
        // Fold dock wall time into `cpu_section_ms[5]` so full-UI floor is honest.
        if ui_v2 {
            let dirty_before_dock = self.is_dirty;
            let _t_dock = std::time::Instant::now();
            self.ui2_repaint_dock_over_library(ctx, crate::ui2::DockSide::Left);
            self.ui2_repaint_dock_over_library(ctx, crate::ui2::DockSide::Right);
            let dock_ms = _t_dock.elapsed().as_secs_f32() * 1000.0;
            self.ui.ui2_dock_paint_ms = dock_ms;
            self.ui.cpu_section_ms[5] += dock_ms;
            if dock_ms > 2.0 && std::env::var_os("AZRA_SCOREBOARD").is_some() {
                use std::cell::Cell;
                thread_local! { static LAST_DOCK: Cell<Option<std::time::Instant>> = Cell::new(None); }
                let now = std::time::Instant::now();
                let fire = LAST_DOCK.with(|c| {
                    let due = c.get().map(|t| now.duration_since(t).as_secs_f32() >= 2.0).unwrap_or(true);
                    if due { c.set(Some(now)); }
                    due
                });
                if fire {
                    eprintln!(
                        "[LAY-DOCK] post-Central dock paint = {dock_ms:.2} ms \
                         (per-card idle: only the card under the pointer is live; \
                         Material does not rebuild when hovering Outliner)"
                    );
                }
            }
            if self.is_dirty && !dirty_before_dock {
                // Material (or other dock) edit this frame — re-shade with the new GPU materials.
                let rect = self.viewport.viewport_rect;
                if rect.width() > 8.0 && rect.height() > 8.0 {
                    self.dispatch_gpu_render(ctx, rect);
                }
            }
        } else {
            self.ui.ui2_dock_paint_ms = 0.0;
        }
        // Toasts + menus + command queue (former unaccounted black box — Gate 0).
        let _t_toast_menu = std::time::Instant::now();
        self.draw_toasts(ctx);
        // Quick light color popup (C on a lights-only selection) — same layering
        // class as toasts: floats above the viewport, closes on outside click.
        self.draw_light_color_popup(ctx);

        // Developer Tweaks AFTER dock re-paint (ui2_repaint_dock_over_library) so Poly /
        // Properties never cover it. Same class as node-editor layering; order=Foreground
        // alone is not enough when docks paint later in the frame (owner 2026-08-06).
        if !minimal_ui {
            self.draw_tweak_panel(ctx);
        }

        // ---- CONTEXT MENU / Q MENU ----
        // v2: Shift+A Add menu is live. Viewport RMB context menu MUST still draw in v2 —
        // input sets `ctx_menu_open` but the old gate only called `draw_context_menu` on
        // v1, so RMB did nothing (owner: menu gone + Selection Shape / region-select
        // settings unreachable). Engine path is the same menus.rs popup; not v1 shell chrome.
        // Context menu uses Order::Tooltip so it sits above tool-options (Box Tool bar).
        if ui_v2 {
            self.draw_context_menu(ctx);
            self.draw_shape_swap_pie(ctx);
            self.draw_add_popup(ctx);
            // Self-contained popup (not v1 shell chrome) — safe in v2. Revives the Ctrl+M
            // material-assign dead chord (draw_mat_assign_popup reads ui.show_mat_assign,
            // set by the Ctrl+M handler; it only ever drew in the v1 branch before).
            self.draw_mat_assign_popup(ctx);
            // Remesh dialog (Ctrl+R / ctxbar Remesh / Object res presets) — not v1 chrome.
            self.draw_voxel_remesh_popup(ctx);
            self.draw_poly_remesh_popup(ctx);
            self.draw_poly_sculpt_panel(ctx);
            self.object_link_draw_menu(ctx);
        } else {
            self.draw_context_menu(ctx);
            self.draw_shape_swap_pie(ctx);
            self.draw_q_menu(ctx);
            self.draw_add_popup(ctx);
            self.draw_mat_assign_popup(ctx);
            self.draw_voxel_sculpt_panel(ctx);
            self.draw_poly_sculpt_panel(ctx);
            self.object_link_draw_menu(ctx);
        }

        // ---- COMMAND QUEUE ----
        self.process_commands();
        self.ui.cpu_section_ms[10] = _t_toast_menu.elapsed().as_secs_f32() * 1000.0;

        // ---- DIRTY FLAGS: end-of-frame processing ----
        // Handles SCENE (rebake + graph sync), LIGHT, MESH in one place.
        let _t_dirty = std::time::Instant::now();
        self.process_dirty();
        self.ui.cpu_section_ms[6] = _t_dirty.elapsed().as_secs_f32() * 1000.0;

        // ---- PERF COUNTERS: end-of-frame snapshot ----
        // Always record frame time (AZRA_MASTER_ROADMAP exit gate). HUD-only fields stay gated.
        let frame_ms = _perf_frame_t0.elapsed().as_secs_f64() * 1000.0;
        {
            let avg: f64 = {
                let h = &self.perf.frame_history;
                let sum: f64 = h.iter().sum();
                sum / h.len() as f64
            };
            if avg > 0.1 && frame_ms > 2.0 * avg {
                self.perf.consecutive_spike_count += 1;
            } else {
                self.perf.consecutive_spike_count = 0;
            }
            self.perf.record_frame(frame_ms);
        }
        if self.ui.show_perf_hud {
            self.perf.prim_count = self.scene.primitives.len();
            self.perf.light_count = self.scene.light_count();
            self.perf.node_count = self.scene.scene_graph.all_nodes().count();
            // Pipeline warmup count: count optional pipeline slots that are Some
            if let Some(gpu) = &self.gpu {
                let pl = &gpu.pipelines;
                let warm = [
                    pl.pipeline_lite.is_some(),
                    pl.deferred_pipeline.is_some(),
                    pl.deferred_lite_pipeline.is_some(),
                    pl.fog_pipeline.is_some(),
                    pl.pt_tri_pipeline.is_some(),
                    pl.fxaa_pipeline.is_some(),
                    pl.primary_bake_pipeline.is_some(),
                    pl.brick_fill_pipeline.is_some(),
                    pl.temporal_reproject_pipeline.is_some(),
                    pl.pt_denoise_pipeline.is_some(),
                    pl.ssao.is_some(),
                ];
                self.perf.pipelines_warm = warm.iter().filter(|&&v| v).count() as u32;
                self.perf.pipelines_total = warm.len() as u32;
            }
        }

        // ---- REPAINT THROTTLE ----
        // B3/B3b/B3c: true idle must settle (no continuous request_repaint).
        // Gate: release + empty scene + AZRA_SCOREBOARD=1 + no input → stderr stops growing
        // within ~10s of settle.
        //
        // B3c sticky-latch fix: `is_dirty` is a boolean latch that can outlive the dirty
        // bitset when `dispatch_gpu_render` early-returns without clearing it (compile
        // skip path) or when scoreboard samples dirty flags only in poll (early frame).
        // After process_dirty, if the bitset is empty, drop the latch — pending async work
        // is tracked separately via has_pending_work.
        if !self.dirty.any() {
            self.is_dirty = false;
        }
        // B3c: stuck AI "thinking" when sidecar never responds used to pin has_pending_work.
        self.watchdog_ai_thinking();

        let pointer_down = ctx.input(|i| i.pointer.any_down());
        let scrolling = ctx.input(|i| {
            i.smooth_scroll_delta != egui::Vec2::ZERO || i.raw_scroll_delta != egui::Vec2::ZERO
        });
        let (has_pending_work, pending_reason) = self.idle_pending_work();
        let tool_busy = self.tool.current.is_busy();
        // B3d: do NOT treat mid-frame `was_dirty` as continuous-spin fuel.
        // That latch was true whenever something called mark_dirty mid-frame (often a
        // 1px rect thrash or a one-shot RENDER), process_dirty cleared bits, is_dirty
        // false — yet was_dirty forced request_repaint forever (few-fps ping-pong with
        // [Overlays] library spikes). This frame already rendered; next frame only if
        // dirty remains, is_dirty still set, pending async, or real input.
        let dirty_still = self.dirty.any() || self.is_dirty;
        let is_active = pointer_down
            || scrolling
            || dirty_still
            || tool_busy
            || self.sdf_sculpt_active
            || has_pending_work;
        // What is fueling the NEXT repaint. Shared by [IDLE_SPIN] and [STALL] — the stall
        // line needs it because stalled frames arrive while tool=Idle, and WHY a frame was
        // produced at all is half the diagnosis. "none" = event-driven idle after this frame.
        let repaint_fuel: &str = if pointer_down {
            "pointer_down"
        } else if scrolling {
            "scrolling"
        } else if dirty_still {
            "dirty_still"
        } else if tool_busy {
            "tool_busy"
        } else if self.sdf_sculpt_active {
            "sdf_sculpt"
        } else if has_pending_work {
            pending_reason.unwrap_or("pending_unknown")
        } else {
            "none"
        };
        // B3c: persisted tweak.repaint_ms==0 from a stress session must NOT spin forever.
        // Uncapped idle only with explicit AZRA_UNCAPPED_IDLE=1 (or AZRA_PERF_SPAM stress).
        let uncapped_idle = self.tweak.repaint_ms == 0
            && (std::env::var_os("AZRA_UNCAPPED_IDLE").is_some()
                || std::env::var_os("AZRA_PERF_SPAM").is_some());
        if is_active {
            ctx.request_repaint();
            // Name the continuous-spin source when scoreboard is on (owner gate).
            if std::env::var_os("AZRA_SCOREBOARD").is_some() {
                use std::cell::Cell;
                use std::time::Instant;
                thread_local! {
                    static LAST_SPIN_LOG: Cell<Option<Instant>> = Cell::new(None);
                }
                let now = Instant::now();
                let fire = LAST_SPIN_LOG.with(|c| {
                    let due = match c.get() {
                        None => true,
                        Some(t) => now.duration_since(t).as_secs_f32() >= 2.0,
                    };
                    if due {
                        c.set(Some(now));
                    }
                    due
                });
                if fire {
                    // Same precedence as the old inline chain — computed once above so
                    // [STALL] and [IDLE_SPIN] can never disagree about the spin source.
                    let reason = repaint_fuel;
                    eprintln!(
                        "[IDLE_SPIN] reason={reason} dirty_bits={:#x} is_dirty={} tool={} pending={} \
                         first_mark={} was_dirty_midframe={}",
                        self.dirty.bits(),
                        self.is_dirty,
                        self.tool.current.variant_name(),
                        pending_reason.unwrap_or("-"),
                        self.perf
                            .first_dirty_caller
                            .as_deref()
                            .unwrap_or("-"),
                        was_dirty,
                    );
                }
            }
        } else if uncapped_idle {
            ctx.request_repaint();
        } else if let Some(wake_ms) = self.next_idle_timer_wake_ms() {
            ctx.request_repaint_after(std::time::Duration::from_millis(wake_ms));
        }
        // else: true idle — event-driven only.
        // Drop a body-scoped mode-tool (Bevel/Extrude) if the user switched to a
        // different body this frame (Tool Exit Convention — object changed).
        self.drop_mode_tool_on_body_switch();
        // Stale poly component ids on a new active body = phantom highlight / OOB crash class.
        self.reconcile_poly_sel_body();
        // Auto-commit any active episode when pointer released (see TRANSACTION_DESIGN.md)
        self.auto_commit_episodes(ctx);
        // CPU frame-time ring — always fill so status-bar FPS / CPU profiler share one source.
        // (Previously gated on `show_cpu_frame_profiler`, which left ui2 status stuck at 0 fps.)
        {
            let frame_ms = _perf_frame_t0.elapsed().as_secs_f32() * 1000.0;
            if self.ui.cpu_frame_times.len() >= 120 {
                self.ui.cpu_frame_times.pop_front();
            }
            self.ui.cpu_frame_times.push_back(frame_ms);
        }
        let wall_ms = wall_start.elapsed().as_secs_f32() * 1000.0;
        self.prev_frame_wall_ms = wall_ms; // CR-70 Fix C: tracked for TDR guard
        // Slow-frame attribution (FRAME_STALL_PLAN Gate 0): former `unaccounted` is split into
        // poll / setup / toast_menu so Idle multi-second stalls identify a real owner.
        // 120 ms catches a STALL. It does not catch a SUSTAINED SLOW FRAME, which is what a user
        // actually reports: owner 2026-08-12, "still 17 fps when I edit" — ~59 ms/frame, so every
        // bucket breakdown stayed silent and the only thing in the log was a sparse
        // `[SPIKE] poll_background_tasks` that was NOT the cost. A frame that misses 60 fps by 3x
        // while the user is mid-gesture is exactly the frame worth attributing, so during an
        // interaction the bar drops to 40 ms. Idle keeps the 120 ms bar: a slow idle frame is
        // usually a one-off (a bake landing, a panel opening) and printing those would bury the
        // interactive ones.
        let interacting = self.pending_episode.is_some()
            || self.interaction_episode.active
            || !self.tool.current.is_idle();
        if wall_ms > 120.0 || (interacting && wall_ms > 40.0) {
            let s = &self.ui.cpu_section_ms;
            // Known buckets: input mesh dispatch overlays layout dirty mode_auto poll setup toast_menu.
            // submit (s[3]) is nested under dispatch wall time — do not double-count.
            let acc = s[0] + s[1] + s[2] + s[4] + s[5] + s[6] + s[7] + s[8] + s[9] + s[10];
            // Second sentinel sample, taken WHILE the stall is known: start/end pair flat
            // (~0.0x ms) = the buckets above are real; inflated = UI thread starved and
            // the buckets are victims of whoever holds the cores (see profiling.rs).
            let sentinel_end_ms = crate::profiling::sentinel_ms();
            eprintln!(
                "[STALL] frame={:.0}ms | layout={:.1} input={:.1} mesh={:.1} dispatch={:.1}(submit={:.1}) overlays={:.1} dirty={:.1} mode_auto={:.1} | poll={:.1} setup={:.1} toast_menu={:.1} | unaccounted={:.1} | tool={} | sentinel={:.2}/{:.2}ms repaint_next={} | {}",
                wall_ms,
                s[5],
                s[0],
                s[1],
                s[2],
                s[3],
                s[4],
                s[6],
                s[7],
                s[8],
                s[9],
                s[10],
                (wall_ms - acc).max(0.0),
                self.tool.current.variant_name(),
                sentinel_start_ms,
                sentinel_end_ms,
                repaint_fuel,
                crate::profiling::perf_stamp()
            );
            // CONSOLE_AUDIT A1: attribute Poly Extrude drag stalls (diagnostic only).
            if self.tool.current.is_poly_extrude()
                || self.tool.current.is_poly_edge_extrude()
                || self.extrude_perf_calls > 0
            {
                eprintln!(
                    "[EXTRUDE_PERF] calls={} total={:.1}ms delta={:.1}ms sync={:.1}ms | pick_rebuilds={} pick_ms={:.1} | input={:.1} layout={:.1} mesh={:.1} overlays={:.1}",
                    self.extrude_perf_calls,
                    self.extrude_perf_total_ms,
                    self.extrude_perf_delta_ms,
                    self.extrude_perf_sync_ms,
                    self.poly_pick_rebuilds_this_frame,
                    self.poly_pick_rebuild_ms_this_frame,
                    s[0],
                    s[5],
                    s[1],
                    s[4],
                );
            }
        } else if self.extrude_perf_total_ms > 8.0 || self.poly_pick_rebuild_ms_this_frame > 8.0 {
            // Sub-stall but hot extrude path — still emit so A1 can attribute without waiting for 120ms.
            eprintln!(
                "[EXTRUDE_PERF] calls={} total={:.1}ms delta={:.1}ms sync={:.1}ms | pick_rebuilds={} pick_ms={:.1} | input={:.1} layout={:.1}",
                self.extrude_perf_calls,
                self.extrude_perf_total_ms,
                self.extrude_perf_delta_ms,
                self.extrude_perf_sync_ms,
                self.poly_pick_rebuilds_this_frame,
                self.poly_pick_rebuild_ms_this_frame,
                self.ui.cpu_section_ms[0],
                self.ui.cpu_section_ms[5],
            );
        }
        if crate::perf_spam_on() && wall_ms > 10.0 {
            eprintln!("[SPIKE] update(): {:.1}ms (layout={:.1} input={:.1} mesh={:.1} dispatch={:.1} overlays={:.1} dirty={:.1})",
                wall_ms,
                self.ui.cpu_section_ms[5], self.ui.cpu_section_ms[0], self.ui.cpu_section_ms[1],
                self.ui.cpu_section_ms[2], self.ui.cpu_section_ms[4], self.ui.cpu_section_ms[6]);
        }
    }
}

impl AzraApp {
    /// B3c: first active continuous-work reason, if any. Used by the idle repaint gate and
    /// `[IDLE_SPIN]` diagnostics under AZRA_SCOREBOARD.
    pub(crate) fn idle_pending_work(&self) -> (bool, Option<&'static str>) {
        if self.pending_screenshot.is_some() {
            return (true, Some("screenshot"));
        }
        if self.ai.agent.is_thinking() {
            return (true, Some("agent_thinking"));
        }
        if self.ai.claude.is_thinking {
            return (true, Some("claude_thinking"));
        }
        if self.ai.ai_tex_rx.is_some() {
            return (true, Some("ai_tex_rx"));
        }
        if self.ai.beauty_rx.is_some() {
            return (true, Some("beauty_rx"));
        }
        if self.ai.pending_beauty_capture.is_some() {
            return (true, Some("beauty_capture"));
        }
        if self.ai.beauty_capture_requested {
            return (true, Some("beauty_requested"));
        }
        if self.ui.export_rx.is_some() {
            return (true, Some("export_rx"));
        }
        if self.project.pending_file_open.is_some() {
            return (true, Some("file_open"));
        }
        if !self.env_prefilter_queue.is_empty() {
            return (true, Some("env_prefilter"));
        }
        if self.ui.deferred_action_countdown > 0 {
            return (true, Some("deferred_action"));
        }
        if self.poly_pick_pending.is_some() {
            return (true, Some("poly_pick"));
        }
        if !self.cache.graph_bake_pending.is_empty() {
            return (true, Some("graph_bake"));
        }
        if self.cache.graph_live_dirty {
            return (true, Some("graph_live_dirty"));
        }
        if self.cache.graph_live_in_flight {
            return (true, Some("graph_live_inflight"));
        }
        if !self.cache.graph_curv_pending.is_empty() || !self.cache.graph_curv_inflight.is_empty() {
            return (true, Some("graph_curv"));
        }
        if self.cache.use_body_vol_cache && self.cache.body_vol_cache.needs_idle_poll() {
            return (true, Some("volume_bake"));
        }
        if self.gpu.as_ref().map_or(false, |g| !g.bake_pending.is_empty()) {
            return (true, Some("bake_pending"));
        }
        if !self.cache.pending_dc_jobs.is_empty() {
            return (true, Some("dc_jobs"));
        }
        if !self.terrain_remesh_jobs.is_empty()
            || !self.terrain_world.pending_display_remesh.is_empty()
        {
            return (true, Some("terrain_remesh"));
        }
        if self
            .sculpt
            .voxel
            .as_ref()
            .map_or(false, |v| v.pending_move_bake.is_some())
        {
            return (true, Some("move_bake"));
        }
        if self.sculpt.voxel.as_ref().map_or(false, |v| {
            v.move_voxelize_due
                .map_or(false, |t| std::time::Instant::now() >= t)
        }) {
            return (true, Some("voxelize_due"));
        }
        // WGSL compile / library scan: already schedule request_repaint_after(50–100ms)
        // in their poll paths — do NOT promote to continuous request_repaint here.
        if self.cache.post_load_warmup > 0 {
            return (true, Some("post_load_warmup"));
        }
        // R6a budgeted multi-body poly display drain still in flight.
        if !self.poly_display_dirty.is_empty() {
            return (true, Some("poly_display_dirty"));
        }
        (false, None)
    }

    /// B3c: clear stuck agent/claude thinking when the worker never returns (no AI server).
    /// Without this, `is_thinking` pins continuous `request_repaint` forever.
    pub(crate) fn watchdog_ai_thinking(&mut self) {
        const MAX_THINK_SECS: f32 = 90.0;
        if self.ai.agent.is_thinking() && self.ai.agent.thinking_seconds() > MAX_THINK_SECS {
            eprintln!(
                "[AI] agent thinking watchdog — {:.0}s with no result; clearing (sidecar absent?)",
                self.ai.agent.thinking_seconds()
            );
            self.ai.agent.status = crate::agent::AgentStatus::Error(
                "Timed out waiting for Ollama (is the server running?)".into(),
            );
        }
        if self.ai.claude.is_thinking && self.ai.claude.thinking_seconds() > MAX_THINK_SECS {
            eprintln!(
                "[AI] claude thinking watchdog — {:.0}s with no result; clearing",
                self.ai.claude.thinking_seconds()
            );
            // Drain a stuck flag; next poll will also see Empty and stay cleared.
            self.ai.claude.is_thinking = false;
            self.ai.code_review_err = Some(
                "Timed out waiting for Claude (network / API key / no response)".into(),
            );
        }
    }

    /// B3b: soonest delay until an idle timer that only runs inside `update` must fire.
    /// Returns `None` when no timer is armed. Callers schedule `request_repaint_after`.
    ///
    /// Covers: sculpt `move_voxelize_due` (~2s), auto-save (120s when `scene_modified`),
    /// body-vol retighten/draft settle debounces. Does **not** cover in-flight bake work
    /// (that goes through `has_pending_work` / continuous repaint).
    pub(crate) fn next_idle_timer_wake_ms(&self) -> Option<u64> {
        let mut soonest: Option<u64> = None;
        let consider = |soonest: &mut Option<u64>, rem_ms: u64| {
            let rem = rem_ms.max(1);
            *soonest = Some(soonest.map_or(rem, |a| a.min(rem)));
        };

        // (2) Surface-mode Move auto-voxelize — armed on release, flushed when now >= due.
        if let Some(due) = self.sculpt.voxel.as_ref().and_then(|v| v.move_voxelize_due) {
            let now = std::time::Instant::now();
            if now >= due {
                consider(&mut soonest, 1);
            } else {
                consider(
                    &mut soonest,
                    due.saturating_duration_since(now).as_millis() as u64,
                );
            }
        }

        // (3) Auto-save — only when the scene is dirty; same interval as `check_auto_save`.
        if self.project.scene_modified {
            let interval = Self::AUTO_SAVE_INTERVAL_SECS;
            let elapsed = self.project.last_auto_save.elapsed().as_secs();
            if elapsed >= interval {
                consider(&mut soonest, 1);
            } else {
                // Coarse second rem; wake near the deadline so check_auto_save can run.
                let rem_ms = (interval - elapsed).saturating_mul(1000).max(1);
                consider(&mut soonest, rem_ms);
            }
        }

        // (1) Volume settle timers (retighten 150ms / draft promote 200ms) not yet due.
        if self.cache.use_body_vol_cache {
            if let Some(ms) = self.cache.body_vol_cache.next_settle_wake_ms() {
                consider(&mut soonest, ms);
            }
        }

        soonest
    }
}

impl FourthLayerApp {
    /// First-frame GPU-context bring-up: recreates the background-thread BGLs, spawns the
    /// WGSL pipeline-compile thread, and assembles the final `GpuCtx` into `self.gpu`.
    /// Extracted verbatim from `update()` (slice 31). Runs once, guarded by `self.gpu_init.is_some()`.
    pub(crate) fn init_gpu_ctx(&mut self) {
            // === FAST STARTUP: compile all PP + bevel in ONE frame, then kick off SDF in background ===
            let init = self.gpu_init.take().unwrap();
            let device = std::sync::Arc::new(init.render_state.device.clone());
            let queue = std::sync::Arc::new(init.render_state.queue.clone());

            // DEVICE-LOSS ATTRIBUTION (P0-R3a, from the 512 review): the owner's hard crash left
            // no artifact — the process died before the visdump auto-dumper could run, so TDR vs
            // VRAM-OOM vs validation could not be distinguished. Capture BOTH channels the instant
            // they fire and flush to disk immediately (eprintln alone can be lost with the console):
            // uncaptured validation/OOM errors and the device-lost reason string.
            device.on_uncaptured_error(Box::new(|e| {
                let msg = format!("[GPU-ERROR] uncaptured: {e}");
                eprintln!("{msg}");
                crate::vis_recorder::visrec(0, "gpu.error", || msg.clone());
                let _ = crate::vis_recorder::dump_to_file(&msg);
            }));
            device.set_device_lost_callback(Box::new(|reason, msg| {
                let line = format!("[GPU-LOST] reason={reason:?} msg={msg}");
                eprintln!("{line}");
                let _ = crate::vis_recorder::dump_to_file(&line);
            }));
            // Wrap pipeline cache in Arc early so both main thread and background thread can reference it
            let shared_cache = std::sync::Arc::new(init.pipeline_cache);
            let cache_ref = (*shared_cache).as_ref();
            let t_all = std::time::Instant::now();

            // 1. Compile all 6 post-process pipelines synchronously (~50ms total with cache)
            let ssr_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("ssr"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.ssr_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("pass_ssr"),compilation_options:Default::default(),cache:cache_ref});
            let tonemap_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("tonemap"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.tonemap_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("pass_tonemap"),compilation_options:Default::default(),cache:cache_ref});
            let bloom_extract_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("bloom_ext"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.bloom_extract_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("bloom_extract"),compilation_options:Default::default(),cache:cache_ref});
            let bloom_blur_h_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("bloom_h"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.bloom_blur_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("bloom_blur_h"),compilation_options:Default::default(),cache:cache_ref});
            let bloom_blur_v_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("bloom_v"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.bloom_blur_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("bloom_blur_v"),compilation_options:Default::default(),cache:cache_ref});
            let upsample_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("upsample"),layout:Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.upsample_bgl],push_constant_ranges:&[]})),module:&init.pp_shader,entry_point:Some("pass_upsample"),compilation_options:Default::default(),cache:cache_ref});
            eprintln!("[TIMING] All 6 PP pipelines compiled: {:?}", t_all.elapsed());

            // 2. Bevel preview (tiny shader)
            let bevel_wgsl = include_str!("../../graphics/src/bevel_preview.wgsl");
            let bevel_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor{label:Some("BevelPreview"),source:wgpu::ShaderSource::Wgsl(bevel_wgsl.into())});
            let bevel_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&init.bevel_preview_bgl],push_constant_ranges:&[]});
            let bevel_preview_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{label:Some("bevel_preview"),layout:Some(&bevel_pl),module:&bevel_shader,entry_point:Some("bevel_preview"),compilation_options:Default::default(),cache:cache_ref});
            eprintln!("[TIMING] PP + bevel compiled: {:?}", t_all.elapsed());

            // 3. Save PP cache immediately (in case SDF compilation crashes)
            if let Some(ref cache) = *shared_cache { gpu_init::save_pipeline_cache(cache); }

            // 4. (GLSL BVH / GLSL PT pipelines REMOVED 2026-07-14 — dead analytical display path)

            // 5a. Compile DF-lite synchronously (fast — needed for deferred shading on first frame)
            let df_lite = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                label:Some("deferred_lite"),layout:Some(&init.deferred_pl),module:&init.df_shader,
                entry_point:Some("deferred_shade"),compilation_options:Default::default(),cache:cache_ref});
            eprintln!("[TIMING] DF-lite pipeline compiled synchronously (deferred shading ready)");

            // 5b. DF-full compiled in background thread (with blend noise neutralized, compiles in ~20-30ms)

            // 5c. Spawn background thread for remaining WGSL SDF pipelines
            // Order: RM (heaviest) → RM-lite → DF-full (longest)
            let bg_device = device.clone();
            let bg_cache = shared_cache.clone();
            // Recreate BGL + PipelineLayout for background thread (neither is Clone in wgpu 22).
            // Uses shared descriptors from graphics::bgl — identical to startup BGLs.
            let bg_rm_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("raymarch_bgl_bg"),entries:&graphics::bgl::raymarch_bgl_entries()});
            let bg_rm_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bg_rm_bgl],push_constant_ranges:&[]});
            let bg_sculpt_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("sculpt_raymarch_bgl_bg"),entries:&graphics::bgl::sculpt_raymarch_bgl_entries()});
            let bg_sculpt_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bg_sculpt_bgl],push_constant_ranges:&[]});
            let bg_df_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("deferred_bgl_bg"),entries:&graphics::bgl::deferred_bgl_entries()});
            let bg_df_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:None,bind_group_layouts:&[&bg_df_bgl],push_constant_ranges:&[]});
            // BGLs + layouts for lazy pipelines (compiled in bg thread after the 6 core pipelines)
            // Layout lives in graphics::bgl (single source) — see fog_bgl_entries().
            // This is the SAME layout as app_new.rs's fog_bgl; it used to be a hand-kept copy.
            let bg_fog_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("fog_bgl_bg"),entries:&graphics::bgl::fog_bgl_entries()});
            let bg_fog_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("fog_bg"),bind_group_layouts:&[&bg_fog_bgl],push_constant_ranges:&[]});
            let bg_fxaa_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("fxaa_bgl_bg"),entries:&[
                wgpu::BindGroupLayoutEntry{binding:0,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:false},view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None},
                wgpu::BindGroupLayoutEntry{binding:1,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::StorageTexture{access:wgpu::StorageTextureAccess::WriteOnly,format:wgpu::TextureFormat::Rgba16Float,view_dimension:wgpu::TextureViewDimension::D2},count:None},
            ]});
            let bg_fxaa_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("fxaa_bg"),bind_group_layouts:&[&bg_fxaa_bgl],push_constant_ranges:&[]});
            // BGL entries — SINGLE SOURCE: graphics::bgl::bake_bgl_entries()
            let bg_bake_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("primary_bake_bgl_bg"),entries:&graphics::bgl::bake_bgl_entries()});
            let bg_bake_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("primary_bake_bg"),bind_group_layouts:&[&bg_bake_bgl],push_constant_ranges:&[]});
            let bg_brick_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("brick_fill_bgl_bg"),entries:&[
                wgpu::BindGroupLayoutEntry{binding:0,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None},
                wgpu::BindGroupLayoutEntry{binding:1,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Uniform,has_dynamic_offset:false,min_binding_size:None},count:None},
                // binding(2): atlas texture (brick fill writes directly to atlas)
                wgpu::BindGroupLayoutEntry{binding:2,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::StorageTexture{access:wgpu::StorageTextureAccess::WriteOnly,format:wgpu::TextureFormat::R32Float,view_dimension:wgpu::TextureViewDimension::D3},count:None},
                wgpu::BindGroupLayoutEntry{binding:3,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None},
                // binding(4): clip_curves storage buffer
                wgpu::BindGroupLayoutEntry{binding:4,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None},
                // binding(5): T1 height-map atlas (dummy when unused)
                wgpu::BindGroupLayoutEntry{binding:5,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Storage{read_only:true},has_dynamic_offset:false,min_binding_size:None},count:None},
            ]});
            let bg_brick_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("brick_fill_bg"),bind_group_layouts:&[&bg_brick_bgl],push_constant_ranges:&[]});
            let bg_temporal_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor{label:Some("temporal_bgl_bg"),entries:&[
                wgpu::BindGroupLayoutEntry{binding:0,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Texture{sample_type:wgpu::TextureSampleType::Float{filterable:false},view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None},
                wgpu::BindGroupLayoutEntry{binding:1,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::Buffer{ty:wgpu::BufferBindingType::Uniform,has_dynamic_offset:false,min_binding_size:None},count:None},
                wgpu::BindGroupLayoutEntry{binding:2,visibility:wgpu::ShaderStages::COMPUTE,ty:wgpu::BindingType::StorageTexture{access:wgpu::StorageTextureAccess::WriteOnly,format:wgpu::TextureFormat::R32Uint,view_dimension:wgpu::TextureViewDimension::D2},count:None},
            ]});
            let bg_temporal_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor{label:Some("temporal_bg"),bind_group_layouts:&[&bg_temporal_bgl],push_constant_ranges:&[]});
            let compiling_heavy_shader = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let bg_compiling_flag = compiling_heavy_shader.clone();
            let (tx, rx) = std::sync::mpsc::channel::<PipelineReady>();
            // Tier 1: compile the heavy ANALYTICAL pipelines (raymarch, raymarch-march, deferred-FULL,
            // fog, brick, pathtrace) eagerly ONLY if analytical is actually enabled. Default volume/raster
            // never uses them — they compile lazily via ensure_*_pipeline on first analytical/Pbr-Lit/PT
            // frame instead, so startup + every per-primitive recompile drop from minutes to seconds.
            let compile_analytical_eager = !crate::gpu_init::LAZY_ANALYTICAL_COMPILE;
            eprintln!("[Azra] Spawning background WGSL pipeline compilation thread (analytical_eager={})...", compile_analytical_eager);
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                let cache_ref = (*bg_cache).as_ref();
                // 1. (Full analytical raymarch compile REMOVED 2026-07-14 — dead march display path.)
                // 2. Raymarch-lite (stripped, faster compile) — the live floor+background pass.
                let lite_wgsl = graphics::build_raymarch_lite_wgsl();
                let lite_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                    label:Some("RaymarchLite"),source:wgpu::ShaderSource::Wgsl(lite_wgsl.into())});
                let rm_lite = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                    label:Some("raymarch_lite"),layout:Some(&bg_rm_pl),module:&lite_shader,
                    entry_point:Some("raymarch_gbuf"),compilation_options:Default::default(),cache:cache_ref});
                eprintln!("[TIMING] Raymarch-lite pipeline compiled (background): {:?}", t.elapsed());
                let _ = tx.send(PipelineReady::RaymarchLite(rm_lite));
                if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }
                // (2b. Volume preview REMOVED 2026-07-13 — SDF volume-raymarch display stripped.)
                // 2c. Sculpt voxel raymarch (Plan B1, self-contained, instant compile)
                let sculpt_wgsl = graphics::build_sculpt_voxel_raymarch_wgsl();
                let sculpt_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                    label:Some("SculptVoxelRaymarch"),source:wgpu::ShaderSource::Wgsl(sculpt_wgsl.into())});
                let sculpt_pipe = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                    label:Some("sculpt_voxel_raymarch"),layout:Some(&bg_sculpt_pl),module:&sculpt_shader,
                    entry_point:Some("sculpt_voxel_raymarch"),compilation_options:Default::default(),cache:cache_ref});
                eprintln!("[TIMING] Sculpt-voxel-raymarch pipeline compiled (background): {:?}", t.elapsed());
                let _ = tx.send(PipelineReady::SculptRaymarch(sculpt_pipe));
                // 3. (Raymarch-march compile REMOVED 2026-07-14 — dead analytical interaction kernel.)
                // 5. DF-full (full material eval — heaviest WGSL) — Tier 1: eager only for Pbr/Lit/analytical.
                // Default SolidHdr shades with DF-LITE (compiled synchronously at startup); DF-full is the
                // "Compiling PBR shaders" long pole and is NOT used by the default workflow. Lazy via
                // ensure_deferred_full_pipeline on the first Pbr/Lit frame instead.
                if compile_analytical_eager {
                    bg_compiling_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    let df_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let df_wgsl = graphics::build_deferred_wgsl(None);
                        let df_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                            label:Some("DeferredFull (background)"),
                            source:wgpu::ShaderSource::Wgsl(df_wgsl.into())});
                        bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                            label:Some("deferred_full"),layout:Some(&bg_df_pl),module:&df_shader,
                            entry_point:Some("deferred_shade"),compilation_options:Default::default(),cache:cache_ref})
                    }));
                    // ALWAYS clear flag — even on panic
                    bg_compiling_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                    match df_result {
                        Ok(df_full) => {
                            eprintln!("[TIMING] DF-full pipeline compiled (background): {:?}", t.elapsed());
                            let _ = tx.send(PipelineReady::DeferredFull(df_full));
                            if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }
                        }
                        Err(e) => {
                            let msg = e.downcast_ref::<String>().map(|s| s.as_str())
                                .or_else(|| e.downcast_ref::<&str>().copied())
                                .unwrap_or("unknown");
                            eprintln!("[SHADER ERROR] DF-full background compilation panicked: {}", msg);
                        }
                    }
                } else { let _ = &bg_df_pl; let _ = &bg_compiling_flag; }
                eprintln!("[TIMING] ====== Core production WGSL pipelines ready: {:?} ======", t.elapsed());
                // Tier 1: persist the production cache (lite/VP/sculpt) NOW. The old intermediate saves
                // lived inside the gated RM/DF-full blocks, so without this an interrupted background
                // compile would never persist the production pipelines → re-cold-compile every launch.
                if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }

                // 6. Lazy pipelines — compiled eagerly on bg thread to avoid first-use hitches
                // Fog pipeline — Tier 1: lazy (ensure_fog_pipeline fires when fog is enabled). Eager warmup
                // only when analytical is on (it embeds scene_sdf → grows with primitives).
                if compile_analytical_eager {
                    let fog_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let fog_wgsl = graphics::build_fog_wgsl(None);
                        let fog_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                            label: Some("Fog (bg)"), source: wgpu::ShaderSource::Wgsl(fog_wgsl.into())});
                        let full = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                            label: Some("fog"), layout: Some(&bg_fog_pl), module: &fog_shader,
                            entry_point: Some("pass_fog"), compilation_options: Default::default(), cache: cache_ref});
                        let lite = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                            label: Some("fog_lite"), layout: Some(&bg_fog_pl), module: &fog_shader,
                            entry_point: Some("pass_fog_lite"), compilation_options: Default::default(), cache: cache_ref});
                        (full, lite)
                    }));
                    match fog_result {
                        Ok((full, lite)) => {
                            eprintln!("[TIMING] Fog pipelines (full+lite) compiled (background): {:?}", t.elapsed());
                            let _ = tx.send(PipelineReady::Fog { full, lite });
                        }
                        Err(e) => {
                            let msg = e.downcast_ref::<String>().map(|s| s.as_str())
                                .or_else(|| e.downcast_ref::<&str>().copied())
                                .unwrap_or("unknown");
                            eprintln!("[SHADER ERROR] Fog bg compilation panicked: {}", msg);
                        }
                    }
                } else { let _ = &bg_fog_pl; }
                // FXAA pipeline
                let fxaa_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let fxaa_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                        label: Some("FXAA (bg)"), source: wgpu::ShaderSource::Wgsl(graphics::FXAA_WGSL.into())});
                    bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                        label: Some("fxaa"), layout: Some(&bg_fxaa_pl), module: &fxaa_shader,
                        entry_point: Some("pass_fxaa"), compilation_options: Default::default(), cache: cache_ref})
                }));
                match fxaa_result {
                    Ok(p) => { eprintln!("[TIMING] FXAA pipeline compiled (background): {:?}", t.elapsed()); let _ = tx.send(PipelineReady::Fxaa(p)); }
                    Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] FXAA bg compilation panicked: {}", msg); }
                }
                // Temporal reproject pipeline
                let temporal_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let temporal_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                        label: Some("temporal_reproject (bg)"), source: wgpu::ShaderSource::Wgsl(graphics::TEMPORAL_REPROJECT_WGSL.into())});
                    bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                        label: Some("temporal_reproject"), layout: Some(&bg_temporal_pl), module: &temporal_shader,
                        entry_point: Some("temporal_reproject"), compilation_options: Default::default(), cache: cache_ref})
                }));
                match temporal_result {
                    Ok(p) => { eprintln!("[TIMING] Temporal reproject pipeline compiled (background): {:?}", t.elapsed()); let _ = tx.send(PipelineReady::Temporal(p)); }
                    Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] Temporal bg compilation panicked: {}", msg); }
                }
                // Primary bake pipeline
                let bake_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let bake_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                        label: Some("volume_bake.wgsl (bg)"),
                        source: wgpu::ShaderSource::Wgsl(graphics::stub_out_blend_noise_wgsl(graphics::VOLUME_BAKE_WGSL).into())});
                    let bake = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                        label: Some("primary_bake_pipeline"), layout: Some(&bg_bake_pl), module: &bake_shader,
                        entry_point: Some("bake_volume"), compilation_options: Default::default(), cache: cache_ref});
                    // STAGE-2 GPU CLASSIFY: the classify entry lives in the SAME module + layout —
                    // compiling it here reuses the just-built module (cheap) and keeps the two
                    // volume_bake pipelines hot-swapping together.
                    let classify = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                        label: Some("classify_bricks_pipeline"), layout: Some(&bg_bake_pl), module: &bake_shader,
                        entry_point: Some("classify_bricks"), compilation_options: Default::default(), cache: cache_ref});
                    // STAGE-3 GPU FINALIZE: the four finalize entries, same module + layout —
                    // ONE shared compile helper (gpu_init) for background, lazy-ensure and tests.
                    let finalize = gpu_init::compile_classify_finalize_pipelines(
                        &bg_device, &bg_bake_pl, &bake_shader, cache_ref);
                    (bake, classify, finalize)
                }));
                match bake_result {
                    Ok((p, c, f)) => {
                        eprintln!("[TIMING] Primary bake + classify(+finalize) pipelines compiled (background): {:?}", t.elapsed());
                        let _ = tx.send(PipelineReady::PrimaryBake(p));
                        let _ = tx.send(PipelineReady::ClassifyBricks(c, f));
                    }
                    Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] Primary bake bg compilation panicked: {}", msg); }
                }
                // Tier 1: persist again after the bake (the other production pipeline) so a kill here
                // still leaves lite/VP/sculpt/bake cached → next launch is warm (~60ms).
                if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }
                // SDF DC mesher (sdf_mesh.wgsl — 2 modules, 5 pipelines). PRODUCTION: the first
                // select's bake-accept path needs it in sdf_populate_resident; compiling it lazily
                // there was the 562ms first-select stall (owner log 5, 2026-07-16 — the [SLOWPOLL]
                // primary=562 column). Compiled right AFTER the bake set: the bake pipeline is
                // needed one-plus frames EARLIER in the same chain (dispatch before accept), so
                // this ordering keeps the bake warm-up unregressed. Race window (select lands
                // before this finishes) is covered by the ensure_sdf_mesher lazy fallback, which
                // now names its cost in the log.
                let mesher_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::sdf_gpu_mesh::SdfMeshGpu::new_with_cache(&bg_device, cache_ref)
                }));
                match mesher_result {
                    Ok(m) => {
                        eprintln!("[TIMING] SDF DC mesher compiled (background): {:?}", t.elapsed());
                        let _ = tx.send(PipelineReady::SdfMesher(m));
                        if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }
                    }
                    Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] SDF DC mesher bg compilation panicked: {}", msg); }
                }
                // Brick fill pipeline — Tier 1: PARKED (use_brick_pool off); lazy via ensure_brick_fill.
                if compile_analytical_eager {
                    let brick_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let brick_shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor{
                            label: Some("brick_fill.wgsl (bg)"),
                            source: wgpu::ShaderSource::Wgsl(graphics::build_brick_fill_wgsl().into())});
                        bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor{
                            label: Some("brick_fill_pipeline"), layout: Some(&bg_brick_pl), module: &brick_shader,
                            entry_point: Some("brick_fill"), compilation_options: Default::default(), cache: cache_ref})
                    }));
                    match brick_result {
                        Ok(p) => { eprintln!("[TIMING] Brick fill pipeline compiled (background): {:?}", t.elapsed()); let _ = tx.send(PipelineReady::BrickFill(p)); }
                        Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] Brick fill bg compilation panicked: {}", msg); }
                    }
                } else { let _ = &bg_brick_pl; }
                // SSAO pipelines (render/src/ssao.rs — tiny standalone module, pt_denoise class).
                let ssao_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render::ssao::SsaoPipelines::new(&bg_device, cache_ref)
                }));
                match ssao_result {
                    Ok(p) => { eprintln!("[TIMING] SSAO pipelines compiled (background): {:?}", t.elapsed()); let _ = tx.send(PipelineReady::Ssao(p)); }
                    Err(e) => { let msg = e.downcast_ref::<String>().map(|s| s.as_str()).or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown"); eprintln!("[SHADER ERROR] SSAO bg compilation panicked: {}", msg); }
                }
                // CONSOLE_AUDIT B3: RT-shadow visibility pass — same BGL entries as
                // `ensure_rt_shadow_pipeline` (graphics::bgl::rt_shadow_bgl_entries).
                let rt_shadow_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let bgl = bg_device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("rt_shadow_bgl (background)"),
                        entries: &graphics::bgl::rt_shadow_bgl_entries(),
                    });
                    let pl = bg_device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("rt_shadow (background)"),
                        bind_group_layouts: &[&bgl],
                        push_constant_ranges: &[],
                    });
                    let shader = bg_device.create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("RtShadow (background)"),
                        source: wgpu::ShaderSource::Wgsl(graphics::build_rt_shadow_wgsl().into()),
                    });
                    let p = bg_device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("rt_shadow (background)"),
                        layout: Some(&pl),
                        module: &shader,
                        entry_point: Some("rt_shadow"),
                        compilation_options: Default::default(),
                        cache: cache_ref,
                    });
                    (bgl, p)
                }));
                match rt_shadow_result {
                    Ok((bgl, p)) => {
                        eprintln!("[TIMING] RT-shadow pipeline compiled (background): {:?}", t.elapsed());
                        let _ = tx.send(PipelineReady::RtShadow { bgl, pipeline: p });
                    }
                    Err(e) => {
                        let msg = e.downcast_ref::<String>().map(|s| s.as_str())
                            .or_else(|| e.downcast_ref::<&str>().copied()).unwrap_or("unknown");
                        eprintln!("[SHADER ERROR] RT-shadow bg compilation panicked: {}", msg);
                    }
                }
                if let Some(ref cache) = *bg_cache { gpu_init::save_pipeline_cache(cache); }
                eprintln!("[TIMING] ====== ALL background WGSL PIPELINES READY: {:?} total ======", t.elapsed());
            });

            // 6. Assemble GpuCtx NOW — GLSL BVH ready, WGSL compiling in background
            self.gpu = Some(GpuCtx {
                native_rt: device.features().contains(app_gpu::native_rt::REQUIRED_FEATURES)
                    .then(app_gpu::native_rt::NativeRtRenderer::default),
                native_rt_geometry_epoch: 0,
                native_rt_active: false,
                pt_interactive_active: false,
                native_rt_status: app_gpu::native_rt::NativeRtStatus::Empty,
                device: device.clone(), queue: queue.clone(),
                render_state: init.render_state,
                pipelines: PipelineRegistry {
                    pipeline_lite: None,
                    prim_outline_pipeline: None,
                    volume_preview_sculpt_pipeline: None,
                    tonemap_pipeline,
                    bloom_extract_pipeline,
                    bloom_blur_h_pipeline, bloom_blur_v_pipeline,
                    deferred_pipeline: None, deferred_lite_pipeline: Some(df_lite),
                    ssr_pipeline,
                    env_prefilter_pipeline: None,
                    fog_pipeline: None,
                    fog_lite_pipeline: None,
                    upsample_pipeline,
                    bevel_preview_pipeline,
                    pt_tri_pipeline: None,
                    native_rt_pipelines: None,
                    rt_shadow_pipeline: None,
                    rt_denoise_pipeline: None,
                    rt_temporal_pipeline: None,
                    fxaa_pipeline: None,
                    primary_bake_pipeline: None,
                    classify_bricks_pipeline: None,
                    classify_finalize_pipelines: None,
                    // GPU CURVATURE: lazy-compiled on the first curvature bake only — most
                    // sessions never wire an SdfCurvature node, so it stays off the startup path.
                    curvature_eval_pipeline: None,
                    curv_dispatch_args_pipeline: None,
                    curv_sum_tile_counters_pipeline: None,
                    brick_fill_pipeline: None,
                    temporal_reproject_pipeline: None,
                    pt_denoise_pipeline: None,
                    pt_reproject_pipeline: None,
                    pt_restir_reproject_pipeline: None,
                    pt_upscale_pipeline: None,
                    pt_warmup: crate::app_poll::PtPipelineWarmup::default(),
                    pt_tri_interactive_pipeline: None,
                    ssao: None,
                    ssgi: None,
                    blend_pipeline: None,
                    raymarch_pipeline_layout: init.pl,
                    pipeline_cache: shared_cache,
                    sdf_compile_rx: Some(rx),
                    df_full_compile_rx: None,
                    df_full_compile_started: false,
                    wgsl_pipelines_ready: false,
                    compiling_heavy_shader,
                    shader_compile_error: None,
                },
                bind_groups: BindGroupCache {
                    bind_group_layout: init.bgl,
                    bind_group: None,
                    sculpt_raymarch_bgl: init.sculpt_raymarch_bgl, sculpt_raymarch_bg: None,
                    tonemap_bind_group_layout: init.tonemap_bgl, tonemap_bind_group: None,
                    bloom_extract_bgl: init.bloom_extract_bgl, bloom_extract_bg: None,
                    bloom_blur_bgl: init.bloom_blur_bgl, bloom_blur_h_bg: None, bloom_blur_v_bg: None,
                    deferred_bgl: init.deferred_bgl, deferred_bg: None,
                    ssr_bgl: init.ssr_bgl, ssr_bg: None,
                    fog_bgl: init.fog_bgl, fog_bg: None,
                    upsample_bgl: init.upsample_bgl, upsample_bg: None,
                    pt_bgl: init.pt_bgl, pt_bg: None, pt_bg_generation: 0, pt_bg_mat_tex_gen: 0,
                    rt_shadow_bgl: None, rt_shadow_bg: None, rt_shadow_bg_generation: 0,
                    rt_denoise_bgl: None, rt_denoise_bg: None, rt_denoise_bg_hi: None,
                    rt_temporal_bgl: None, rt_temporal_bg: None, rt_temporal_bg_hi: None,
                    bevel_preview_bgl: init.bevel_preview_bgl, bevel_preview_bg: None,
                    fxaa_bgl: None, fxaa_bg: None,
                    primary_bake_bgl: None,
                    brick_fill_bgl: None,
                    temporal_reproject_bgl: None, temporal_reproject_bg: None,
                    pt_denoise_bgl: None, pt_denoise_bg: None, pt_denoise_bg_p0_temp: None,
                    pt_denoise_bg_ping: None, pt_denoise_bg_pong: None,
                    pt_reproject_bgl: None, pt_reproject_bg: None, pt_reproject_bg_half: None,
                    pt_restir_reproject_bgl: None, pt_restir_reproject_bg: None, pt_restir_reproject_bg_half: None,
                    pt_bg_half: None, pt_bg_half_generation: 0, pt_bg_half_mat_tex_gen: 0,
                    pt_denoise_bg_half: None, pt_denoise_bg_half_p0_temp: None,
                    pt_denoise_bg_half_ping: None, pt_denoise_bg_half_pong: None,
                    pt_upscale_bgl: None, pt_upscale_bg: None, pt_upscale_params_buffer: None,
                    blend_bgl: None,
                    env_prefilter_bgl: None,
                },
                buffers: BufferPool {
                    uniform_buffer: init.ub,
                    prim_buffer: init.pb,
                    tex_buffer: init.tb,
                    bvh_node_buffer: init.bvh_node_buffer,
                    bvh_index_buffer: init.bvh_index_buffer,
                    light_buffer: init.light_buffer,
                    light_link_buffer: init.light_link_buffer,
                    light_cluster_buffer: init.light_cluster_buffer,
                    compositor_buffer: init.compositor_buffer,
                    rt_shadow_params_buffer: None,
                    rt_denoise_params_buffer: None,
                    rt_temporal_params_buffer: None,
                    env_buffer: init.eb,
                    // 8-byte placeholder; the real LUT replaces it in poll once the startup
                    // worker finishes (env_lut_res stays 0 → shader analytic fallback till then).
                    brdf_lut_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("BrdfLut"),size:8,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                    env_prefilter_params_buffer: None,
                    matcap_buffer: init.mcb,
                    tex_atlas_buffer: init.tex_atlas_buffer,
                    tex_meta_buffer: init.tex_meta_buffer,
                    bevel_edge_buffer: init.bevel_edge_buffer, bevel_params_buffer: init.bevel_params_buffer,
                    bevel_grid_cells_buffer: init.bevel_grid_cells_buffer, bevel_grid_indices_buffer: init.bevel_grid_indices_buffer,
                    brick_pool_dummy_buf: device.create_buffer(&wgpu::BufferDescriptor{label:Some("brick_dummy"),size:4,usage:wgpu::BufferUsages::STORAGE|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                    brick_info_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("brick_info"),size:std::mem::size_of::<graphics::brick_pool::BrickVolumeInfo>() as u64,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                    // 48 bytes: brick_res, res, iso, bounds + albedo_rough vec4 + metal_pad vec4
                    sculpt_params_buffer: device.create_buffer(&wgpu::BufferDescriptor{label:Some("sculpt_params"),size:48,usage:wgpu::BufferUsages::UNIFORM|wgpu::BufferUsages::COPY_DST,mapped_at_creation:false}),
                    fog_prev_camera_buffer: init.fog_prev_cam_buf,
                    temporal_march_flags_buffer: init.temporal_march_flags_buffer,
                    temporal_params_buffer: None,
                    pt_reproj_params_buffer: None,
                    pt_restir_reproj_params_buffer: None,
                    primary_bake_params_buffer: None,
                    lab_mesh_grid_buffer: None,
                    outline_mesh_grid_buffer: None,
                    outline_clip_curves_buffer: None,
                    outline_clip_start_buffer: None,
                    height_map_buffer: None,
                    outline_mesh_start_buffer: None,
                    lab_mesh_grid_dummy: None,
                    pt_denoise_params_buffer: None,
                    pt_denoise_pass_params_buffer: None,
                    blend_params_buffer: None,
                    pt_tlas_buffer: None, pt_instance_buffer: None, pt_blas_buffer: None,
                    pt_tri_buffer: None, pt_vert_buffer: None, pt_meta_buffer: None,
                    pt_material_buffer: None,
                    pt_moments_buffer: None, pt_half_moments_buffer: None,
                    pt_reproj_moments_buffer: None, pt_half_reproj_moments_buffer: None,
                    pt_stats_buffer: None, pt_stats_staging: None,
                    pt_stats_map_pending: std::sync::atomic::AtomicBool::new(false),
                    pt_stats_map_ready: None,
                    pt_noisy_fraction_bits: std::sync::atomic::AtomicU32::new(f32::to_bits(1.0)),
                    pt_env_cdf_buffer: None,
                    pt_scene_generation: 0,
                },
                gpu_volume: init.gv,
                bvh_node_count: 0,
                texture: None, texture_size: (0, 0), texture_id: None,
                split_pane_texture: None, split_pane_texture_id: None, split_pane_size: (0, 0),
                hdr_texture: None,
                bloom_a: None, bloom_b: None,
                gbuf_normal: None, gbuf_material: None, gbuf_albedo: None,
                ssr_texture: None,
                fog_texture: None, fog_prev_texture: None,
                upsample_texture: None,
                pt_accum_texture: None,
                pt_accum_output: None,
                pbr_accum_in: None, pbr_accum_out: None, pbr_accum_size: (0, 0),
                probe_cube: None, probe_face_size: 0, probe_sampler: None, probe_params_buffer: None,
                probe_capture_buffer: None, probe_capture_pipeline: None, probe_capture_bgl: None,
                probe_mip_pipeline: None, probe_mip_bgl: None,
                probe_next_face: 6, probe_baked: false, probe_baked_sig: 0,
                probe_rejected: false, probe_free_fraction: f32::NAN,
                probe_round_gen: 0, probe_verdict_round: 0,
                probe_verdict_staging: None, probe_verdict_rx: None,
                probe_center: [0.0; 3], probe_extent: [1.0; 3],
                probe_sh_buffer: None, probe_sh_pipeline: None, probe_sh_bgl: None,
                probe_sh_strength_written: f32::NAN,
                probe_cube_generation: 0,
                probe_view_cache_gen: 0,
                probe_capture_hdr_size: (0, 0),
                probe_face_mip0: None,
                probe_capture_bgs: None,
                probe_mip_src_views: Vec::new(),
                probe_mip_dst_views: Vec::new(),
                probe_mip_bgs: Vec::new(),
                probe_sh_view: None,
                probe_sh_bg: None,
                last_gu: None,
                pt_reproj_scratch: None,
                pt_denoise_temp: None,
                pt_half_hdr: None, pt_half_accum: None, pt_half_reproj_scratch: None, pt_half_size: (0, 0),
                pt_half_accum_output: None, pt_half_denoise_temp: None,
                pt_restir_prev: None, pt_restir_out: None,
                pt_half_restir_prev: None, pt_half_restir_out: None,
                pt_gi_prev: None, pt_gi_out: None,
                pt_half_gi_prev: None, pt_half_gi_out: None,
                gbuf_surface_id: None, gbuf_motion: None, gbuf_curvature: None, gbuf_emission: None,
                raster_depth_texture: None, _raster_depth_dummy: init.raster_depth_dummy,
                rt_shadow_vis: None,
                rt_shadow_vis_hi: None,
                rt_shadow_denoised: None,
                rt_shadow_history: None,
                rt_shadow_denoised_hi: None,
                rt_shadow_history_hi: None,
                rt_denoise_size: (0, 0),
                ssao_res: None,
                ssgi_res: None,
                bake_tex_dummy: init.bake_tex_dummy,
                // STRIPPED 2026-07-13: the VolumeAtlas (dense 3D texture, ~2GB at 812³ auto-size)
                // fed ONLY the removed SDF volume-raymarch display + the legacy TextureOnly bake
                // target. Never created — all consumers are Option-tolerant (bake falls back to
                // pad=0 / dummy texture / ResidentVolume, the pre-atlas path).
                volume_atlas: None,
                temporal_prev_normal_dist: None,
                temporal_validity_mask: None,
                temporal_dummy_mask: init.temporal_dummy_mask,
                overlay_texture: None, overlay_view: None,
                blend_temp_texture: None, blend_temp_view: None,
                viewport_size: (0, 0),
                raster: init.raster, shadows: init.shadows, picking: init.picking,
                sculpt_pick: None,
                light_depth_probe: render::depth_probe::DepthProbe::new(),
                line_overlay: init.line_overlay,
                face_overlay: init.face_overlay,
                gpu_ts: {
                    if device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
                        eprintln!("[FourthLayer] GPU timestamp queries enabled");
                        Some(gpu_timestamps::GpuTimestamps::new(&device, &queue))
                    } else {
                        eprintln!("[FourthLayer] GPU timestamp queries not available");
                        None
                    }
                },
                bake_pending: Vec::new(),
                classify_gpu_pending: Vec::new(),
                brick_fill_pending: Vec::new(),
                temporal_prev_cam: [0.0; 16],
                temporal_frame_valid: false,
                rt_shadow_history_valid: false,
                rt_shadow_history_valid_hi: false,
                voxel_mesh: None,
                voxel_mesh_inflight: None,
                voxel_mesh_density: None,
                voxel_mesh_density_res: 0,
                voxel_mesh_atlas: None,
                voxel_chunk_gpu: Vec::new(),
                voxel_mesh_src_gen: 0,
                voxel_pool_grow_to: 0,
                sdf_body_mesh: std::collections::HashMap::new(),
                sdf_mesh_pool: None,
                raster_cull: None,
                hzb: None,
                cluster_pool: render::cluster_gpu::ClusterGpuPool::default(),
                cluster_cut: None,
                sdf_body_curvature: std::collections::HashMap::new(),
                mesh_curv_pipelines: None,
                sdf_brick_atlases: std::collections::HashMap::new(),
                sdf_atlas_lru: std::collections::HashMap::new(),
                sdf_atlas_budget_bytes: crate::gpu_ctx::sdf_atlas_budget_from_vram(self.gpu_info.vram_bytes),
                // Compile the sculpt brush/gather/scatter pipelines during GPU initialisation. Lazy
                // construction on the first pen-down made the first dab absorb shader compilation and
                // feel dramatically heavier than every later dab.
                voxel_brush: Some(crate::voxel_gpu_mesh::VoxelBrushGpu::new(&device)),
                voxel_brush_inflight: None,
                surface_pick_staging: None,
                voxel_pool_rb_staging: None,
                voxel_atlas_undo: Default::default(),
                voxel_atlas_undo_building: None,
                material_textures: crate::material_textures::MaterialTextureArray::new(&device, crate::material_textures::DEFAULT_MAP_RES),
                mat_bake_pipe_cache: crate::gpu_material_bake::MatBakePipeCache::default(),
            });
            self.mark_dirty(DirtyFlags::RENDER);
            // CR-70 Fix B: Register GPU error handler for post-crash diagnostics.
            // Logs uncaptured errors (validation, OOM, device-lost) and writes a
            // breadcrumb file so we can diagnose TDR crashes after the fact.
            //
            // 2026-07-13 (the 1024 crash post-mortem): this registration REPLACES the earlier
            // visdump-writing handler (init_gpu_ctx installs one first; wgpu keeps only the
            // last), and the old fs::write OVERWROTE the file per error — after a device loss,
            // dozens of "Command encoder is invalid" cascade errors clobbered the ROOT error
            // (the owner's crash left only the cascade tail). APPEND with a size cap so the
            // FIRST error — the actual cause — always survives at the top, and mirror into the
            // visdump channel (the one artifact source the crash session was missing).
            device.on_uncaptured_error(Box::new(|error| {
                eprintln!("[GPU] UNCAPTURED ERROR: {:?}", error);
                crate::vis_recorder::GPU_UNCAPTURED_ERRORS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir().join("fourthlayer_gpu_error.txt");
                let already = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                const CAP_BYTES: u64 = 64 * 1024; // keep the head (root error), drop the cascade
                if already < CAP_BYTES {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        let _ = writeln!(f, "{:?}\ntimestamp: {:?}\n----", error, std::time::SystemTime::now());
                    }
                }
                // One visdump for the FIRST error of the session — the root cause; the
                // cascade that follows a device loss must not spam dump files.
                static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !DUMPED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let msg = format!("[GPU-ERROR] uncaptured: {error}");
                    crate::vis_recorder::visrec(0, "gpu.error", || msg.clone());
                    let _ = crate::vis_recorder::dump_to_file(&msg);
                }
            }));
            eprintln!("[Azra] PresentMode: {:?}", wgpu::PresentMode::Mailbox);
            eprintln!("[Azra] Viewport active — DF-lite ready, WGSL pipelines compiling in background");

            // Task 14.2: Enumerate all GPUs and create worker devices. This runs once.
            if self.worker_gpus.is_empty() {
                self.worker_gpus = gpu_worker::enumerate_and_open_workers(&self.gpu_info.backend);
                // Restore persisted enabled state from RenderSettings
                for w in &mut self.worker_gpus {
                    if let Some((_, enabled)) = self.render.worker_gpu_enabled.iter()
                        .find(|(name, _)| *name == w.adapter_name)
                    {
                        w.enabled = *enabled;
                    }
                }
                // Task 14.3a: Create job dispatcher with worker threads
                if !self.worker_gpus.is_empty() {
                    self.job_dispatcher = Some(gpu_jobs::GpuJobDispatcher::new(&self.worker_gpus));
                }
            }

            // Default PBR environment (owner 2026-08-06): neutral dark grey gradient
            // HDR for lookdev / hard-surface — NOT Factory_Pumps (busy, warm studio).
            // Path matches library_scan's fallback so either owner can install it without
            // stomping a project-restored or user-picked HDRI (`env_hdr_width == 0` only).
            // Decode + placeholder mips + SH9 off-thread; buffer install + GPU GGX
            // prefilter in poll (env_ibl.rs Frame Ordering Contract).
            if self.env_hdr_width == 0 && self.env_prefilter_queue.is_empty() {
                if let Some(root) = crate::content_library::find_asset_root() {
                    let candidates = [
                        root.join("HDR_Labs/HDR_Gradient/neutral_dark_0.003.hdr"),
                        // Fallbacks if the primary asset is missing from a thin install.
                        root.join("HDR_Labs/HDR_Gradient/neutral_dark_0.006.hdr"),
                        root.join("HDR_Labs/HDR_Gradient/environment_dark_gradient.hdr"),
                    ];
                    if let Some(path) = candidates.iter().find(|p| p.exists()) {
                        eprintln!("[Azra] Default HDR queued: {}", path.display());
                        self.request_env_load(path.clone());
                    }
                }
            }
    }
}
