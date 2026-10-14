//! Render settings sub-panel — extracted from properties_viewport.rs.

use crate::ui_menus::DefaultMenu;
use eframe::egui;
use crate::*;

/// The bake-resolution ladder every picker steps through (legacy Grid picker,
/// ui2 Properties SDF page, outliner quick-bar, Ctrl+R resolution pie).
/// `pie_menu::resolution_pie_def` mirrors these six steps — keep in sync.
pub(crate) const RES_LADDER: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

/// Which allocation path a "Grid:" resolution click will take. The picker's selectable
/// ceiling AND the click action are BOTH derived from this one value, so they cannot
/// describe different paths.
///
/// S26-1: the ceiling was keyed on a hardcoded `let gridbacked_only = true`, while the
/// click forked on `self.cache.use_body_vol_cache`. With Per-Body Cache OFF that offered
/// 1024³ on the legacy route, whose allocation is dense res³ on BOTH sides — a 4.29 GB
/// `vec![100.0; 1024³]` memset on the UI thread, plus a res³×4 storage buffer that CANNOT
/// be bound (4.29 GB > the 2.1 GB `max_storage_buffer_binding_size` the adapter reports).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GridRoute {
    /// `set_body_resolution` + `trigger_body_vol_bake` → per-body sparse brick pipeline.
    PerBodyCache,
    /// `SdfVolume::new_empty` + `SdfGpuVolume::new` → the legacy global volume, read by the
    /// legacy SDF-sculpt raymarch (`render_mode == 1`, viewport_render.rs:669).
    LegacyGlobal,
}

impl GridRoute {
    /// The route a click takes, keyed on the SAME field the click itself forks on.
    fn for_cache_state(use_body_vol_cache: bool) -> Self {
        if use_body_vol_cache { GridRoute::PerBodyCache } else { GridRoute::LegacyGlobal }
    }

    /// The ceiling for THIS route's allocation strategy. Adding a route breaks this `match`
    /// AND the click's `match` at COMPILE TIME — a ceiling can never again describe a path
    /// the click will not take.
    ///
    /// Do NOT "reuse" `BodyVolumeCache::max_mesh_resolution()` here: it keys on
    /// `gridbacked_only`, which defaults FALSE (body_volume_cache/mod.rs:584) and only flips
    /// true on the first bake (dirty.rs:164) — the 1024³ entry would flicker during the
    /// startup window.
    fn max_resolution(self) -> u32 {
        match self {
            // Every buffer on the sparse path is surface-proportional — no dense res³ exists.
            // `trigger_body_vol_bake` forces `gridbacked_only = true` (dirty.rs:164), so a
            // ≥512 pick is always sparse-eligible by construction.
            GridRoute::PerBodyCache => crate::gpu_jobs::max_sparse_resolution(), // 2048
            // The legacy global volume is DENSE on BOTH sides: `vec![100.0; res³]` f32 on the
            // CPU + a res³×4 flat STORAGE buffer bound WHOLE at binding 2
            // (viewport_render.rs:510/532/575/647). That is exactly the dense-storage-binding
            // constraint `max_gridbacked_resolution()` encodes: 512³×4 = 512MB binds;
            // 1024³×4 = 4.29GB exceeds max_storage_buffer_binding_size (2.1GB on a 4090).
            // >512 belongs to the sparse pipeline, never here.
            GridRoute::LegacyGlobal => crate::gpu_jobs::max_gridbacked_resolution(), // 512
        }
    }
}

impl AzraApp {
    /// The body whose bake resolution the pickers target: the TOP-LEVEL ancestor of the
    /// active body (resolution is a top-level-body property — the whole subtree bakes as
    /// ONE grid; see the Grid picker below). `None` when it is not an SdfBody
    /// (poly / voxel / empty scene) — callers disable their control then.
    pub(crate) fn resolution_target_body(&self) -> Option<core_math::scene_graph::NodeId> {
        let nid = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
        self.scene.scene_graph.get(nid).and_then(|n| {
            matches!(n.payload, core_math::scene_graph::NodePayload::SdfBody { .. }).then_some(nid)
        })
    }

    /// (current resolution, selectable ceiling) for `body` — the exact values the legacy
    /// Grid picker (below) and `props_sdf_page` compute inline: per-body override falling
    /// back to the global default, and the dense-vs-sparse cap split (1024/2048 need the
    /// sparse brick pipeline).
    pub(crate) fn body_grid_res_state(&self, body: core_math::scene_graph::NodeId) -> (u32, u32) {
        let cur = if self.cache.use_body_vol_cache {
            self.cache
                .body_vol_cache
                .body_resolution(body)
                .unwrap_or(self.render.voxel_cache_res)
        } else {
            self.render.voxel_cache_res
        };
        // S2.6: sparse ≥512 is STRUCTURAL on the per-body route (sparse_bake_debug removed).
        // Same split as GridRoute: per-body cache → sparse ceiling; legacy global stays ≤512.
        let sel_max = if self.cache.use_body_vol_cache {
            crate::gpu_jobs::max_sparse_resolution()
        } else {
            crate::gpu_jobs::max_gridbacked_resolution()
        };
        (cur, sel_max)
    }

    /// Ctrl+R resolution-pie confirm: slot index → ladder step, applied to the active
    /// top-level body through the canonical apply path. Fails closed (toast, no-op) when
    /// the slot is beyond the selectable ceiling or no SDF body is active.
    pub(crate) fn apply_resolution_pie(&mut self, slot: usize) {
        let Some(res) = RES_LADDER.get(slot).copied() else { return };
        let Some(body) = self.resolution_target_body() else {
            self.toast("Resolution: no active SDF body");
            return;
        };
        let (cur, sel_max) = self.body_grid_res_state(body);
        if res > sel_max {
            self.toast("1024³/2048³ need the sparse brick pipeline (Developer Tweaks → Sparse bake)");
            return;
        }
        // Always apply (even if cur == res): re-locks + re-dirties so a stale adaptive
        // snap or failed bake can be re-asserted by re-picking the same tier.
        self.apply_body_grid_resolution(body, res);
        self.toast(&format!("Bake resolution: {res}³"));
    }

    /// Canonical resolution-apply path for the per-body volume grid (shared by the
    /// legacy Grid picker here and the ui2 Properties SDF page).
    pub(crate) fn apply_body_grid_resolution(&mut self, active: core_math::scene_graph::NodeId, res: u32) {
        if self.cache.use_body_vol_cache {
            // PER-BODY: set + rebake ONLY the active body — other bodies keep theirs.
            // This is the multi-resolution path (one body 128³, another 512³). The old
            // cell is freed; the append-only shelf allocator leaves a hole, but a
            // single change is minor — the atlas auto-compacts when it actually
            // starves (allocate failure), not on every change, so other bodies don't
            // flicker. (The structural fix for the hole-leak is the sparse atlas.)
            self.cache.body_vol_cache.set_body_resolution(active, res);
            // Drop Decimate/mesh-mod fold so it re-materializes at new res.
            self.invalidate_mesh_mod_display(active);
            // PT mesh snapshots key on (content_hash, bake_res) — clear so
            // Path Trace re-reads the new tessellation after bake lands
            // (content hash alone is blind to Grid-only bumps).
            self.invalidate_pt_sdf_snapshot(active);
            self.trigger_body_vol_bake();
        } else {
            // Legacy global path (no body volume cache): recreate the global volume.
            self.render.voxel_cache_res = res;
            let bounds = self.cache.cpu_volume.grid_bounds;
            self.cache.cpu_volume = core_math::sdf_volume::SdfVolume::new_empty(res, bounds);
            if let Some(gpu) = &mut self.gpu {
                gpu.gpu_volume = SdfGpuVolume::new(&gpu.device, [res, res, res]);
                gpu.bind_groups.bind_group = None;
                gpu.bind_groups.deferred_bg = None;
                gpu.bind_groups.pt_bg = None;
            }
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    pub(crate) fn draw_props_render(&mut self, ui: &mut egui::Ui) {
            ui.colored_label(egui::Color32::from_gray(140),"Viewport");
            ui.separator();
            let is_matcap = self.render.shading_mode == ShadingMode::MatCap;
            egui::CollapsingHeader::new(egui::RichText::new("🖥 Render").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
                .default_open(true).show(ui,|ui|{
                // (Res Scale slider removed — march resolution is in Developer Tweaks)
                ui.label("Quality:");
                ui.horizontal_wrapped(|ui|{
                    for (i,lbl) in ["⚡","🔧","🎨","✨","💎"].iter().enumerate(){
                        if ui.selectable_label(self.render.render_quality==i as u32,*lbl)
                            .on_hover_text(["Draft","Low","Medium","High","Ultra"][i]).clicked(){
                            self.render.render_quality=i as u32; self.mark_dirty(DirtyFlags::RENDER);
                        }
                    }
                });
                if self.tool.work_mode==WorkMode::Object {
                    // Per-body volume cache toggle
                    ui.horizontal(|ui|{
                        let bvc = self.cache.use_body_vol_cache;
                        if ui.selectable_label(bvc, "⚡ Per-Body Cache").clicked(){
                            self.cache.use_body_vol_cache = !self.cache.use_body_vol_cache;
                            if self.cache.use_body_vol_cache {
                                self.cache.body_vol_cache.mark_all_bodies_dirty(&self.scene.scene_graph);
                                // Don't auto-trigger bake — user decides when to bake
                                self.mark_dirty(DirtyFlags::RENDER);
                            } else {
                                // Clean reset: wipe all body cache state
                                self.cache.body_vol_cache.reset();
                                // Clear any in-flight primary GPU bakes
                                if let Some(gpu) = &mut self.gpu {
                                    gpu.bake_pending.clear();
                                }
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        }
                        // (⟐ Analytical toggle REMOVED 2026-07-14 — analytical march display path stripped)
                        if self.cache.use_body_vol_cache {
                            let hits = self.cache.body_vol_cache.cache_hits;
                            let total = self.cache.body_vol_cache.cache_total;
                            if total > 0 {
                                ui.label(format!("{}/{}", hits, total));
                            }
                        }
                    });
                    // Sparse brick pool — PARKED dev feature (BRICK_CACHE_BUG_AUDIT.md: 23 open bugs).
                    // Enabling it reroutes the bake path and breaks the production bake→DC→display
                    // (no live move/bevel updates). Gated behind Developer Tweaks so it cannot be
                    // toggled accidentally during normal modeling. Defaults OFF; a restart resets it.
                    if self.cache.use_body_vol_cache && self.ui.show_tweak_panel {
                        ui.horizontal(|ui| {
                            let bp = self.cache.body_vol_cache.use_brick_pool;
                            if ui.selectable_label(bp, "🧱 Brick Pool (dev/parked)").clicked() {
                                self.cache.body_vol_cache.use_brick_pool = !self.cache.body_vol_cache.use_brick_pool;
                                if self.cache.body_vol_cache.use_brick_pool {
                                    // Classify all existing cached body grids into bricks
                                    let (d, q) = match self.gpu.as_ref() {
                                        Some(g) => (Some(&*g.device), Some(&*g.queue)),
                                        None => (None, None),
                                    };
                                    self.cache.body_vol_cache.classify_all_cached_bodies(d, q);
                                }
                                // Switching the bake path (either direction) MUST force a clean full
                                // rebake so the display recovers — turning brick pool OFF has to
                                // restore the normal bake→DC→mesh path immediately.
                                self.cache.body_vol_cache.mark_all_bodies_dirty(&self.scene.scene_graph);
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if bp {
                                let used = self.perf.brick_used;
                                let cap = self.perf.brick_capacity;
                                if cap > 0 {
                                    ui.label(format!("{}/{}", used, cap));
                                }
                            }
                        });
                    }
                    // Smooth DC normals toggle
                    if self.cache.use_body_vol_cache {
                        ui.horizontal(|ui| {
                            let sn = self.cache.smooth_dc_normals;
                            if ui.selectable_label(sn, "Smooth Cache Normals").clicked() {
                                self.cache.smooth_dc_normals = !self.cache.smooth_dc_normals;
                            }
                        });
                    }
                    // (📦 Volume Preview toggle REMOVED 2026-07-13 — the SDF volume-raymarch
                    // display path is stripped; the DC mesh is the sole SDF display.)
                    // Voxel cache resolution picker
                    ui.horizontal(|ui| {
                        ui.label("Grid:");
                        // Per-body resolution: the picker reflects + sets the ACTIVE body's resolution,
                        // not a global. Falls back to the global default when the body has no override.
                        // Resolution is a property of the TOP-LEVEL body (the whole subtree bakes as ONE
                        // grid at ONE resolution — a sub-body/group has no independent bake resolution).
                        // So the picker reflects + sets the top-level ancestor, never the sub-body.
                        let active = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
                        let cur = if self.cache.use_body_vol_cache {
                            self.cache.body_vol_cache.body_resolution(active)
                                .unwrap_or(self.render.voxel_cache_res)
                        } else {
                            self.render.voxel_cache_res
                        };
                        // The route THIS click will take. The ceiling and the action derive from
                        // this ONE value, so they cannot disagree (S26-1). 1024³ is selectable
                        // exactly when the sparse brick pipeline will take it; the legacy global
                        // route allocates dense res³ and stays ≤512.
                        let route = GridRoute::for_cache_state(self.cache.use_body_vol_cache);
                        let sel_max = route.max_resolution();
                        for res in [64u32, 128, 256, 512, 1024, 2048] {
                            let label = format!("{}³", res);
                            let blocked = res > sel_max;
                            let resp = ui.add_enabled(!blocked, egui::SelectableLabel::new(cur == res, &label));
                            if blocked {
                                resp.clone().on_hover_text(match route {
                                    // Unreachable while the list tops out at the sparse ceiling —
                                    // kept so a future higher entry degrades honestly.
                                    GridRoute::PerBodyCache =>
                                        "Above the sparse brick pipeline's resolution ceiling.",
                                    GridRoute::LegacyGlobal =>
                                        "Per-Body Cache is OFF — the legacy global grid is a dense \
                                         CPU+GPU allocation capped at 512³ (1024³ would be 4.3 GB \
                                         of RAM and cannot be bound).\nTurn Per-Body Cache ON for \
                                         1024³ via the sparse brick pipeline.",
                                });
                            } else if res == 512 {
                                resp.clone().on_hover_text("512³ → DC triangles (sparse bricks when active: surface-proportional VRAM). Recommended: 12GB+ GPU.");
                            } else if res == 1024 {
                                resp.clone().on_hover_text("1024³ sparse bricks → DC triangles.\nSurface-proportional VRAM (~4× the 512³ pool for the same scene). Recommended: 16GB+ GPU.");
                            } else if res >= 2048 {
                                resp.clone().on_hover_text("2048³ sparse bricks → DC triangles (opt-in, 2048-virtual).\nSurface-proportional VRAM (~4× the 1024³ pool; a dense body can reach ~3GB settled). First full bake ≈ 0.5s. If the surface exceeds the GPU's buffer ceiling it self-heals down one notch. Recommended: 16-24GB GPU.");
                            }
                            if !blocked && resp.clicked() && cur != res {
                                // ONE apply path shared with the ui2 SDF page and the Ctrl+R
                                // resolution pie — forks internally on the SAME cache flag
                                // `route` derives from, so the ceiling and action agree (S26-1).
                                self.apply_body_grid_resolution(active, res);
                            }
                        }
                    });
                    // Adaptive resolution per body
                    {
                        let mut adaptive = self.cache.body_vol_cache.use_adaptive_resolution;
                        if ui.checkbox(&mut adaptive, "Adaptive Resolution").changed() {
                            self.cache.body_vol_cache.set_adaptive_resolution(
                                adaptive, &self.scene.scene_graph,
                            );
                            if self.cache.use_body_vol_cache {
                                self.trigger_body_vol_bake();
                            }
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                        if adaptive {
                            ui.label(
                                egui::RichText::new("  Resolution scales with body size")
                                    .small().weak(),
                            );
                        }
                    }
                    // Detail Weighted LO (PoC) — source-of-detail budget vs uniform/size adaptive.
                    {
                        let mut detail = self.cache.body_vol_cache.use_detail_weighted_lo;
                        if ui
                            .checkbox(&mut detail, "Detail Weighted LO (PoC)")
                            .on_hover_text(
                                "Source-of-detail LO: oversample extract + Field decimate \
                                 (melt flats, keep soft curves). Soft-K / bevel floors min res \
                                 so fillets get ≥~3 cells across.\n\
                                 Pure DC — no analytical.\n\
                                 A/B: set Grid to LO target (e.g. 64³), toggle OFF vs ON.",
                            )
                            .changed()
                        {
                            self.cache.body_vol_cache.set_detail_weighted_lo(
                                detail,
                                &self.scene.scene_graph,
                            );
                            // Bust mesh-mod / Detail LO stamps so display re-folds after bake.
                            self.invalidate_all_mesh_mod_displays();
                            if self.cache.use_body_vol_cache {
                                self.trigger_body_vol_bake();
                            }
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                        if detail {
                            let active = self
                                .scene
                                .scene_graph
                                .top_level_body(self.scene.active_body_nid);
                            let base_pick = self
                                .cache
                                .body_vol_cache
                                .body_resolution(active)
                                .unwrap_or(self.cache.body_vol_cache.resolution);
                            let eff = self.cache.body_vol_cache.effective_resolution(active);
                            let floor = self
                                .cache
                                .body_vol_cache
                                .body_detail_floor_res
                                .get(&active)
                                .copied();
                            let floor_txt = floor
                                .map(|f| format!(" floor≥{f}³"))
                                .unwrap_or_default();
                            let mode_txt = if self.cache.body_vol_cache.detail_lo_hold_compare {
                                "RAW extract (Alt hold)"
                            } else if self.cache.body_vol_cache.detail_lo_live_draft {
                                "draft (editing)"
                            } else if self.cache.body_vol_cache.detail_lo_preprocessed {
                                "quality LO (preprocessed)"
                            } else {
                                "quality fold"
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "  extract {eff}³{floor_txt} · {mode_txt}"
                                ))
                                .small()
                                .weak(),
                            );
                            if let Some(f) = floor {
                                if f > base_pick {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "  soft-K floor raised Grid {base_pick}³ → {f}³"
                                        ))
                                        .small()
                                        .color(egui::Color32::from_rgb(255, 190, 90)),
                                    );
                                }
                            }
                            // Honest budget: raw extract vs folded display + uniform estimate
                            let uniform_est = crate::body_volume_cache::BodyVolumeCache::estimate_uniform_tri_budget(
                                base_pick,
                            );
                            if let Some((nv, nt)) = self.active_body_mesh_stats() {
                                let raw_txt = match self.active_body_raw_extract_tris() {
                                    Some(r) if r > nt && nt > 0 => {
                                        let keep_pct =
                                            (100.0 * nt as f32 / r as f32).round() as u32;
                                        format!("raw {r} → display {nt} ({keep_pct}% keep)")
                                    }
                                    _ => format!("display {nt} tris"),
                                };
                                ui.label(
                                    egui::RichText::new(format!(
                                        "  {raw_txt} · {nv} verts · ~{uniform_est} uniform@{base_pick}³"
                                    ))
                                    .small()
                                    .color(egui::Color32::from_rgb(120, 200, 255)),
                                );
                            }
                            ui.label(
                                egui::RichText::new(
                                    "  Hold Alt (idle) = raw extract · never steals GRS Alt",
                                )
                                .small()
                                .weak(),
                            );
                            // Preprocess: freeze raw extract so fold sliders scrub freely.
                            ui.horizontal(|ui| {
                                let ready = self.detail_lo_base_ready_for_active()
                                    && self.cache.body_vol_cache.detail_lo_preprocessed;
                                let prep_label = if ready {
                                    "Re-preprocess LO base"
                                } else {
                                    "Preprocess LO base"
                                };
                                if ui
                                    .button(prep_label)
                                    .on_hover_text(
                                        "BAKE quality LO mesh (what you want for nice topology):\n\
                                         1) Oversampled DC extract\n\
                                         2) Soft-K protect + even edge equalize (cylinders / blend)\n\
                                         3) Flat relax + SDF snap + smooth normals\n\
                                         After this, sliders re-fold from cache only.\n\
                                         Re-run after Grid / Oversample / Soft-K changes.",
                                    )
                                    .clicked()
                                {
                                    let (n, raw_t, disp_t) =
                                        self.preprocess_detail_lo_base(true);
                                    if n > 0 {
                                        eprintln!(
                                            "[DetailLO] Preprocess bake OK: {} body · raw {} → display {} tris",
                                            n, raw_t, disp_t
                                        );
                                    } else {
                                        eprintln!(
                                            "[DetailLO] Preprocess bake FAILED — no mesh (wait for volume bake / enable body vol)"
                                        );
                                    }
                                }
                                if ui
                                    .small_button("All bodies")
                                    .on_hover_text(
                                        "Preprocess every top-level SDF body (slower).",
                                    )
                                    .clicked()
                                {
                                    let _ = self.preprocess_detail_lo_base(false);
                                }
                            });
                            {
                                let ready = self.detail_lo_base_ready_for_active();
                                let flagged = self.cache.body_vol_cache.detail_lo_preprocessed;
                                if ready && flagged {
                                    let raw_n = self
                                        .active_body_raw_extract_tris()
                                        .unwrap_or(0);
                                    let scrub = if self.cache.body_vol_cache.detail_lo_interactive
                                        || self.cache.body_vol_cache.detail_lo_quality_pending
                                    {
                                        " · scrub: fast fold → quality on settle"
                                    } else {
                                        " · sliders: active-only fast fold"
                                    };
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "  LO base ready · raw {raw_n} tris{scrub}"
                                        ))
                                        .small()
                                        .color(egui::Color32::from_rgb(120, 220, 140)),
                                    );
                                } else if ready {
                                    ui.label(
                                        egui::RichText::new(
                                            "  base warm · click Preprocess to lock fold workflow",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(
                                            "  base cold — Preprocess (or wait for bake) before scrubbing",
                                        )
                                        .small()
                                        .color(egui::Color32::from_rgb(255, 180, 90)),
                                    );
                                }
                            }
                            // Quality preset chips
                            ui.horizontal(|ui| {
                                ui.small("Preset:");
                                for (id, label) in [
                                    (0u8, "Balanced"),
                                    (1, "Soft walls"),
                                    (2, "Sharp"),
                                ] {
                                    let on = self.cache.body_vol_cache.detail_lo_preset == id;
                                    if ui.selectable_label(on, label).clicked()
                                        && self.cache.body_vol_cache.set_detail_lo_preset(id)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                            });
                            let mut settle = self.cache.body_vol_cache.detail_lo_settle_only;
                            if ui
                                .checkbox(&mut settle, "Settle-only quality")
                                .on_hover_text(
                                    "While editing: skip quality fold (fast). After idle: re-fold. \
                                     Extract res stays stable (no mid-scale rebake).",
                                )
                                .changed()
                            {
                                if self.cache.body_vol_cache.set_detail_lo_settle_only(settle) {
                                    self.cache.body_vol_cache.detail_lo_live_draft = false;
                                    self.schedule_detail_lo_scrub_refresh();
                                }
                            }
                            let mut smooth_n = self.cache.body_vol_cache.detail_lo_smooth_normals;
                            if ui
                                .checkbox(&mut smooth_n, "Smooth normals after fold")
                                .on_hover_text(
                                    "Recompute display normals after decimate — cleaner LO shading.",
                                )
                                .changed()
                            {
                                if self
                                    .cache
                                    .body_vol_cache
                                    .set_detail_lo_smooth_normals(smooth_n)
                                {
                                    self.schedule_detail_lo_scrub_refresh();
                                }
                            }
                            let mut edge_shape = self.cache.body_vol_cache.detail_lo_edge_shape;
                            if ui
                                .checkbox(&mut edge_shape, "Edge shape protect (AZRA)")
                                .on_hover_text(
                                    "AZRA-only remesh signal from the live SDF:\n\
                                     • Keep main edges straight or cleanly curved\n\
                                     • Distance-to-edge → denser mesh on edge bands\n\
                                     • Flat surfaces starved (tri budget → edges)\n\
                                     • Min edge + curve angle → fewer tris, shape holds\n\
                                     Uses SDF ridge/anisotropy + graph proximity — \
                                     no analytical rims. Toggle for A/B.",
                                )
                                .changed()
                            {
                                if self
                                    .cache
                                    .body_vol_cache
                                    .set_detail_lo_edge_shape(edge_shape)
                                {
                                    self.schedule_detail_lo_scrub_refresh();
                                }
                            }
                            if edge_shape {
                                let mut min_sc =
                                    self.cache.body_vol_cache.detail_lo_min_edge_scale;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut min_sc, 0.5..=3.0)
                                            .text("Min edge")
                                            .suffix("×"),
                                    )
                                    .on_hover_text(
                                        "Minimum edge length scale (auto base = soft-K/bevel \
                                         or ~2× avg DC edge).\n\
                                         Higher → collapse short zippers on straight edges → \
                                         fewer tris, outline stays.\n\
                                         Lower → denser edges (closer to raw extract).",
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_min_edge_scale(min_sc)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                                let mut cang = self.cache.body_vol_cache.detail_lo_curve_angle;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut cang, 6.0..=20.0)
                                            .text("Curve angle")
                                            .suffix("°"),
                                    )
                                    .on_hover_text(
                                        "Keep chain vertices when turn exceeds this angle.\n\
                                         Round surfaces / soft-K rings get even segments.\n\
                                         Straight runs collapse under Min edge.\n\
                                         Lower ° = denser rounds · Higher ° = fewer segments.",
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_curve_angle(cang)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                                let mut len_f =
                                    self.cache.body_vol_cache.detail_lo_edge_length_field;
                                if ui
                                    .checkbox(&mut len_f, "Edge-length field ℓ(x)")
                                    .on_hover_text(
                                        "Target edge length from SDF protect:\n\
                                         short on edges · long on flats.\n\
                                         QEM collapses toward ℓ — even density, fewer zippers.",
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_edge_length_field(len_f)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                                let mut relax = self.cache.body_vol_cache.detail_lo_flat_relax;
                                if ui
                                    .checkbox(&mut relax, "Flat relax + SDF snap")
                                    .on_hover_text(
                                        "After fold: Laplacian on flats only, then project \
                                         verts back to the SDF surface.\n\
                                         Kills star fans; soft-K / hard edges stay put.",
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_flat_relax(relax)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                                let mut anal = self.cache.body_vol_cache.detail_lo_analytical_project;
                                if ui
                                    .checkbox(&mut anal, "Analytical feature project")
                                    .on_hover_text(
                                        "After quality fold / Preprocess:\n\
                                         • Detect hard crease edges (dihedral)\n\
                                         • Fit a 3D line to each crease chain (PCA)\n\
                                         • Snap verts onto that line (straighten)\n\
                                         • Flatten low-detail verts to local plane\n\
                                         • Pull to SDF surface\n\
                                         Re-run Preprocess after enabling to see the effect.",
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_analytical_project(anal)
                                    {
                                        self.cache.body_vol_cache.detail_lo_interactive = false;
                                        self.cache.body_vol_cache.detail_lo_quality_pending = false;
                                        // Prefer full Preprocess if base warm; else quality re-fold.
                                        if self.detail_lo_base_ready_for_active() {
                                            let _ = self.preprocess_detail_lo_base(true);
                                        } else {
                                            self.refresh_detail_lo_displays_for(true);
                                        }
                                    }
                                }
                            }
                            ui.horizontal(|ui| {
                                let qn_label = if let Some(g) = &self.quick_normal_ghost {
                                    format!(
                                        "Clear normal bake · {}% hits",
                                        g.hit_pct
                                    )
                                } else {
                                    "Quick Normal Bake (high→low)".into()
                                };
                                if ui
                                    .button(qn_label)
                                    .on_hover_text(
                                        "REAL normal bake preview (not dense wire):\n\
                                         • HIGH = raw oversampled extract (or DC)\n\
                                         • LOW  = your current Detail LO mesh\n\
                                         Rays from each LO vertex hit the high mesh and \
                                         copy high normals onto the LO surface.\n\
                                         Shading = what a normal bake would look like.\n\
                                         Preprocess LO base first for best results.\n\
                                         Orbit / zoom / pan dismisses (restores LO normals).",
                                    )
                                    .clicked()
                                {
                                    if self.quick_normal_ghost.is_some() {
                                        self.clear_quick_normal_bake();
                                    } else {
                                        self.trigger_quick_normal();
                                    }
                                }
                            });
                            if let Some(g) = &self.quick_normal_ghost {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "  BAKE PREVIEW · high {} {}³ ({} tris) → low {} tris · {}% verts updated · orbit dismisses",
                                        g.source,
                                        g.res,
                                        g.high_tris,
                                        g.low_tris,
                                        g.hit_pct
                                    ))
                                    .small()
                                    .color(egui::Color32::from_rgb(80, 220, 255)),
                                );
                            }
                            let mut heat = self.cache.body_vol_cache.detail_lo_heat_wire;
                            if ui
                                .checkbox(&mut heat, "Heat wire (protect map)")
                                .on_hover_text(
                                    "Color DC topology wire by mesh protect importance:\n\
                                     blue = flat (melt first) → cyan/yellow → red = keep \
                                     (creases / soft curves / edge-shape bands).\n\
                                     Auto-enables topology wire. Read density after fold.",
                                )
                                .changed()
                            {
                                if self.cache.body_vol_cache.set_detail_lo_heat_wire(heat) {
                                    if heat {
                                        self.render.wire_overlay_enabled = true;
                                    }
                                    self.wire_local_cache.clear();
                                    self.overlay_lines_fp = u64::MAX;
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            }
                            let mut over = self.cache.body_vol_cache.detail_lo_oversample;
                            if ui
                                .add(
                                    egui::Slider::new(&mut over, 1.25..=3.0)
                                        .text("Oversample")
                                        .suffix("×"),
                                )
                                .on_hover_text(
                                    "Extract resolution multiplier vs Grid. Soft-feature floor \
                                     can push higher. Changes extract → rebakes.",
                                )
                                .changed()
                            {
                                if self.cache.body_vol_cache.set_detail_lo_oversample(
                                    over,
                                    &self.scene.scene_graph,
                                ) {
                                    self.invalidate_all_mesh_mod_displays();
                                    if self.cache.use_body_vol_cache {
                                        self.trigger_body_vol_bake();
                                    }
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            }
                            let mut strength = self.cache.body_vol_cache.detail_lo_strength;
                            if ui
                                .add(
                                    egui::Slider::new(&mut strength, 0.5..=2.5)
                                        .text("Curve protect"),
                                )
                                .on_hover_text(
                                    "Protect soft-K / creases / silhouette while melting flats.",
                                )
                                .changed()
                            {
                                if self.cache.body_vol_cache.set_detail_lo_strength(strength) {
                                    self.schedule_detail_lo_scrub_refresh();
                                }
                            }
                            ui.horizontal(|ui| {
                                let mut use_target =
                                    self.cache.body_vol_cache.detail_lo_target_tris > 0;
                                if ui
                                    .checkbox(&mut use_target, "Target tris")
                                    .on_hover_text(
                                        "Absolute tri budget (fair A/B). Off = Keep fraction.",
                                    )
                                    .changed()
                                {
                                    let t = if use_target { uniform_est } else { 0 };
                                    if self.cache.body_vol_cache.set_detail_lo_target_tris(t) {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                                if ui
                                    .small_button("Match uniform")
                                    .on_hover_text(format!(
                                        "Set target ≈ estimated uniform mesh at Grid {base_pick}³ \
                                         (~{uniform_est} tris) for fair Detail LO A/B."
                                    ))
                                    .clicked()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_target_tris(uniform_est)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                            });
                            if self.cache.body_vol_cache.detail_lo_target_tris > 0 {
                                let mut target =
                                    self.cache.body_vol_cache.detail_lo_target_tris as f32;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut target, 200.0..=80_000.0)
                                            .logarithmic(true)
                                            .text("Target tris"),
                                    )
                                    .changed()
                                {
                                    if self
                                        .cache
                                        .body_vol_cache
                                        .set_detail_lo_target_tris(target as u32)
                                    {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                            } else {
                                let mut ratio = self.cache.body_vol_cache.detail_lo_keep_ratio;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut ratio, 0.12..=0.6)
                                            .text("Keep tris")
                                            .suffix("×"),
                                    )
                                    .changed()
                                {
                                    if self.cache.body_vol_cache.set_detail_lo_keep_ratio(ratio) {
                                        self.schedule_detail_lo_scrub_refresh();
                                    }
                                }
                            }
                        }
                    }
                    // (VolumeAtlas auto-recreate REMOVED 2026-07-13 — the atlas is no longer created.)
                    // Worker GPU toggle (Phase 14.2) — multi-GPU settings panel
                }
                ui.separator();
                // Wireframe overlay (Shift+Z) — poly bodies + SDF DC mesh (+ mesh modifiers).
                {
                    let mut wire = self.render.wire_overlay_enabled;
                    if ui
                        .checkbox(&mut wire, "DC Mesh Topology Wire (Shift+Z)")
.default_menu("render.wire_overlay_enabled", serde_json::json!(self.render.wire_overlay_enabled))
                        .on_hover_text(
                            "True Dual Contouring triangle edges (mesh topology), aligned to the \
                             solid DC surface — not MatCap/image edges. After Decimate / Detail LO \
                             when those are on. Viewport: Wires · Shift+Z.",
                        )
                        .changed()
                    {
                        self.render.wire_overlay_enabled = wire;
                        self.overlay_lines_fp = u64::MAX;
                        if wire {
                            self.wire_local_cache.clear();
                        }
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                if self.render.wire_overlay_enabled {
                    ui.weak("Real DC triangle edges (unique mesh topology), body-aligned.");
                    if ui.add(egui::Slider::new(&mut self.render.wire_overlay_opacity,0.05..=1.0).text("Opacity")).changed(){
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui.add(egui::Slider::new(&mut self.render.wire_overlay_thickness,0.5..=5.0).text("Thickness")).changed(){
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let mut col32 = egui::Color32::from_rgb(
                        (self.render.wire_overlay_color[0]*255.0) as u8,
                        (self.render.wire_overlay_color[1]*255.0) as u8,
                        (self.render.wire_overlay_color[2]*255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut col32).changed() {
                        self.render.wire_overlay_color = [col32.r() as f32/255.0, col32.g() as f32/255.0, col32.b() as f32/255.0];
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui.checkbox(&mut self.render.wire_overlay_backface, "Show back faces (X-Ray)")
.default_menu("render.wire_overlay_backface", serde_json::json!(self.render.wire_overlay_backface)).changed() {
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                ui.separator();
                ui.label("Poly Edit Wire");
                // X-Ray is now ONE flag for display + selection (audit #1): it is the inverse of
                // `sel.visible_only` (the header ◉Visible/◎X-Ray toggle). Mirror it here so both UIs agree.
                let mut xray = !self.sel.visible_only;
                if ui.checkbox(&mut xray, "X-Ray (show all edges + select through surface)")
                    .on_hover_text("On: full see-through wireframe (every edge) AND click-select through the mesh. Off: hidden-line wireframe + visible-only selection. Same as the ◉Visible/◎X-Ray toggle in the viewport header.")
                    .changed() { self.sel.visible_only = !xray; self.mark_dirty(DirtyFlags::RENDER); }
                if ui.add(egui::Slider::new(&mut self.render.poly_wire_opacity,0.05..=1.0).text("Opacity")).changed(){
                    self.overlay_lines_fp = u64::MAX;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                if ui.add(egui::Slider::new(&mut self.render.poly_wire_thickness,0.5..=5.0).text("Thickness")).changed(){
                    self.overlay_lines_fp = u64::MAX;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
                {
                    let mut col32 = egui::Color32::from_rgb(
                        (self.render.poly_wire_color[0]*255.0) as u8,
                        (self.render.poly_wire_color[1]*255.0) as u8,
                        (self.render.poly_wire_color[2]*255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut col32).changed() {
                        self.render.poly_wire_color = [col32.r() as f32/255.0, col32.g() as f32/255.0, col32.b() as f32/255.0];
                        self.overlay_lines_fp = u64::MAX;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                }
                ui.weak("Also under Viewport Overlays (top-right) → Wire / Poly Edit Selection.");

                // === MatCap-specific settings ===
                if is_matcap {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(180, 180, 200), "MatCap");
                    // Flat BG color (default = UI panel dark gray 0x1c1c1c)
                    {
                        let mut col32 = egui::Color32::from_rgb(
                            (self.render.matcap_bg_color[0]*255.0) as u8,
                            (self.render.matcap_bg_color[1]*255.0) as u8,
                            (self.render.matcap_bg_color[2]*255.0) as u8,
                        );
                        ui.horizontal(|ui| {
                            ui.label(if self.render.matcap_bg_gradient { "BG Bottom" } else { "BG Color" });
                            if ui.color_edit_button_srgba(&mut col32).changed() {
                                self.render.matcap_bg_color = [col32.r() as f32/255.0, col32.g() as f32/255.0, col32.b() as f32/255.0];
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if !self.render.matcap_bg_gradient
                                && ui.small_button("UI gray").on_hover_text("Reset to panel dark gray (0x1c1c1c)").clicked()
                            {
                                self.render.matcap_bg_color = [0.1098, 0.1098, 0.1098];
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                    }
                    if ui.checkbox(&mut self.render.matcap_bg_gradient, "Gradient BG")
.default_menu("render.matcap_bg_gradient", serde_json::json!(self.render.matcap_bg_gradient)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    if self.render.matcap_bg_gradient {
                        let mut top32 = egui::Color32::from_rgb(
                            (self.render.matcap_bg_gradient_top[0]*255.0) as u8,
                            (self.render.matcap_bg_gradient_top[1]*255.0) as u8,
                            (self.render.matcap_bg_gradient_top[2]*255.0) as u8,
                        );
                        ui.horizontal(|ui| {
                            ui.label("BG Top");
                            if ui.color_edit_button_srgba(&mut top32).changed() {
                                self.render.matcap_bg_gradient_top = [top32.r() as f32/255.0, top32.g() as f32/255.0, top32.b() as f32/255.0];
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                    }
                    if ui.add(egui::Slider::new(&mut self.render.matcap_rotation, -180.0..=180.0).text("Rotation")).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    // Shared with the v2 MatCap panel — ONE definition (render_settings.rs).
                    self.matcap_material_tint_ui(ui);
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(180, 180, 200), "Edge Highlight");
                    if ui.checkbox(&mut self.render.matcap_edge_enabled, "Edge Lines")
.default_menu("render.matcap_edge_enabled", serde_json::json!(self.render.matcap_edge_enabled)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    if self.render.matcap_edge_enabled {
                        {
                            let mut ec32 = egui::Color32::from_rgb(
                                (self.render.matcap_edge_color[0]*255.0) as u8,
                                (self.render.matcap_edge_color[1]*255.0) as u8,
                                (self.render.matcap_edge_color[2]*255.0) as u8,
                            );
                            ui.horizontal(|ui| {
                                ui.label("Color");
                                if ui.color_edit_button_srgba(&mut ec32).changed() {
                                    self.render.matcap_edge_color = [ec32.r() as f32/255.0, ec32.g() as f32/255.0, ec32.b() as f32/255.0];
                                    self.mark_dirty(DirtyFlags::RENDER);
                                }
                            });
                        }
                        if ui.add(egui::Slider::new(&mut self.render.matcap_edge_thickness, 1.0..=3.0).text("Thickness").step_by(1.0)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                        if ui.add(egui::Slider::new(&mut self.render.matcap_edge_threshold, 0.05..=1.0).text("Threshold")).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    }
                }

                // === HDR / post-processing (hidden in MatCap mode) ===
                // Reuses the SHARED PBR post-FX body (ui2/shell/render_settings.rs) —
                // tonemapper, HDR exposure, bloom, SSR, SSAO/GTAO, sun shadows, RT
                // shadows, fog, AA. Single source of truth with the ui2 PBR panel.
                if !is_matcap {
                    if self.env_hdr_width > 0 {
                        ui.separator();
                        ui.label("HDR");
                        if ui.add(egui::Slider::new(&mut self.render.exposure,0.01..=5.0).text("Exposure").clamping(egui::SliderClamping::Never)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    }
                    ui.separator();
                    self.render_pbr_postfx_body(ui);
                } else {
                    // AA is available regardless of shading mode; the shared body
                    // already includes it, so surface it here for MatCap too.
                    ui.separator();
                    if ui.checkbox(&mut self.render.sdf_edge_aa_enabled, "SDF Edge AA")
.default_menu("render.sdf_edge_aa_enabled", serde_json::json!(self.render.sdf_edge_aa_enabled)).on_hover_text("Silhouette anti-aliasing for SDF surfaces").changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    if ui.checkbox(&mut self.render.fxaa_enabled, "FXAA")
.default_menu("render.fxaa_enabled", serde_json::json!(self.render.fxaa_enabled)).on_hover_text("Fast approximate anti-aliasing (post-process)").changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    if ui.checkbox(&mut self.render.wire_aa_enabled, "Wireframe AA")
.default_menu("render.wire_aa_enabled", serde_json::json!(self.render.wire_aa_enabled)).on_hover_text("Smooth anti-aliased overlay wireframes (edit/object wire, modifier wires). Disable to save a little GPU work on low-end machines.").changed(){self.mark_dirty(DirtyFlags::RENDER);}
                    if ui.checkbox(&mut self.render.pbr_progressive_enabled, "Progressive Refine")
.default_menu("render.pbr_progressive_enabled", serde_json::json!(self.render.pbr_progressive_enabled)).on_hover_text("PBR/Lit only: while the camera is idle, keep rendering jittered frames and average them — quality converges over time like the path tracer's progressive mode. Zero cost while interacting.").changed(){self.render.pbr_refine_frames=0;self.render.pbr_refine_sig=0;self.mark_dirty(DirtyFlags::RENDER);}
                }
                // Raytrace settings (shown when RT mode active)
                if self.render.shading_mode == ShadingMode::Raytrace {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 180, 80), "Raytrace");
                    let mut b = self.render.rt_bounces as i32;
                    if ui.add(egui::Slider::new(&mut b,1..=8).text("Bounces")).changed(){self.render.rt_bounces=b as u32;self.mark_dirty(DirtyFlags::RENDER);}
                    let mut s = self.render.rt_shadow_samples as i32;
                    if ui.add(egui::Slider::new(&mut s,1..=16).text("Shadow Samples")).changed(){self.render.rt_shadow_samples=s as u32;self.mark_dirty(DirtyFlags::RENDER);}
                    let mut e = self.render.rt_env_samples as i32;
                    if ui.add(egui::Slider::new(&mut e,1..=16).text("Env Samples")).changed(){self.render.rt_env_samples=e as u32;self.mark_dirty(DirtyFlags::RENDER);}
                    if ui.checkbox(&mut self.render.rt_caustics, "Caustics")
.default_menu("render.rt_caustics", serde_json::json!(self.render.rt_caustics)).changed(){self.mark_dirty(DirtyFlags::RENDER);}
                }
                // Path trace settings (shown when PT mode active)
                if self.render.shading_mode == ShadingMode::PathTrace {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(180, 140, 255), "Path Trace");
                    self.draw_pt_quality_presets(ui);
                    if ui
                        .checkbox(&mut self.render.pt_fast_mode, "Fast Path Trace")
.default_menu("render.pt_fast_mode", serde_json::json!(self.render.pt_fast_mode))
                        .on_hover_text(
                            "ON (default): prefer FPS — lean spp/bounces, tighter frame budget \
                             (can drop to half-res lighting when slow), snappier orbit.\n\
                             OFF: quality refine (higher multi-spp / bounce budget when noisy).",
                        )
                        .changed()
                    {
                        self.render.pt_accumulated_frames = 0;
                        self.render.pt_converged = false;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    let mut b = self.render.pt_max_bounces as i32;
                    if ui.add(egui::Slider::new(&mut b, 1..=32).text("Max Bounces")).changed() {
                        self.render.pt_max_bounces = b as u32;
                        self.render.pt_accumulated_frames = 0;
                        self.render.pt_converged = false;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if self.draw_pt_samples_per_frame_control(ui) {
                        self.render.pt_accumulated_frames = 0;
                        self.render.pt_converged = false;
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if ui.add(egui::Slider::new(&mut self.render.pt_focus_radius,0.05..=0.5).text("Focus Radius")).changed(){}
                    if ui.add(egui::Slider::new(&mut self.render.pt_dof_aperture,0.0..=1.0).text("DOF Aperture")).on_hover_text("Depth-of-field strength (0 = off)").changed(){self.render.pt_accumulated_frames=0;self.mark_dirty(DirtyFlags::RENDER);}
                    if self.render.pt_dof_aperture > 0.0 {
                        if ui.add(egui::Slider::new(&mut self.render.pt_dof_falloff,0.0..=1.0).text("DOF Falloff")).changed(){self.render.pt_accumulated_frames=0;self.mark_dirty(DirtyFlags::RENDER);}
                    }
                    if ui.add(egui::Slider::new(&mut self.render.pt_clamp,1.0..=100.0).text("Clamp").logarithmic(true)).on_hover_text("Firefly reduction").changed(){self.render.pt_accumulated_frames=0;self.mark_dirty(DirtyFlags::RENDER);}
                    // Denoise — sole home for PT denoise (removed from Viewport Overlays).
                    ui.label(egui::RichText::new("Denoise").strong().size(11.0));
                    if ui.checkbox(&mut self.render.pt_denoise, "Enable Denoise")
.default_menu("render.pt_denoise", serde_json::json!(self.render.pt_denoise)).changed() {
                        self.mark_dirty(DirtyFlags::RENDER);
                    }
                    if self.render.pt_denoise {
                        ui.indent("denoise_props", |ui| {
                            if ui.add(egui::Slider::new(&mut self.render.denoise_strength, 0.0..=1.0).text("Strength").step_by(0.01)).changed() {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            let mut r = self.render.denoise_radius as f32;
                            if ui.add(egui::Slider::new(&mut r, 1.0..=8.0).text("Radius").step_by(1.0)).changed() {
                                self.render.denoise_radius = r as u32;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui
                                .checkbox(
                                    &mut self.render.pt_denoise_end_on_hdr,
                                    "Ends on HDR (no blit)",
                                )
.default_menu("render.pt_denoise_end_on_hdr", serde_json::json!(self.render.pt_denoise_end_on_hdr))
                                .on_hover_text(
                                    "ON: even à-trous step counts invert pass0 so the final filter writes HDR directly \
                                     (no temp→hdr copy).\n\
                                     OFF: classic ping-pong + final blit (needs COPY_DST on hdr).",
                                )
                                .changed()
                            {
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                        });
                    }
                    ui.horizontal(|ui|{
                        ui.label(format!("Accumulated: {} frames", self.render.pt_accumulated_frames));
                        if ui.small_button("Reset").clicked(){
                            self.render.pt_accumulated_frames=0;
                            self.render.pt_converged = false;
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                        if self.render.pt_converged && ui.small_button("Keep going").on_hover_text("Resume sampling past the stop (raises finish budget)").clicked() {
                            self.render.pt_converged = false;
                            self.render.pt_max_accum_frames = self.render.pt_max_accum_frames.saturating_mul(2).max(4096).min(131_072);
                            self.render.pt_noise_threshold = (self.render.pt_noise_threshold * 0.5).max(0.002);
                            self.mark_dirty(DirtyFlags::RENDER);
                        }
                    });
                }
                ui.separator();
                if ui.checkbox(&mut self.render.upsample_enabled, "Upscale (CAS)")
.default_menu("render.upsample_enabled", serde_json::json!(self.render.upsample_enabled)).on_hover_text("Bicubic + contrast-adaptive sharpening when Res Scale < 1").changed(){self.mark_dirty(DirtyFlags::RENDER);}
                ui.separator();
                // Viewport DOF fields exist on RenderSettings but are NEVER read by the post-process
                // path (post_process still uses sel_rim_base as a stub focus — WORK_ORDER #24).
                // Do not offer live controls that do nothing; PathTrace DOF (pt_dof_*) is separate above.
                egui::CollapsingHeader::new("Depth of Field")
                    .default_open(false).show(ui, |ui| {
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, "Enable DOF (viewport)"));
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 170, 90),
                        "\u{26A0} Viewport DOF not wired yet — no effect in Lit/PBR",
                    );
                    ui.label(egui::RichText::new("Path Trace DOF: use the Path Trace section above.")
                        .weak().size(10.0));
                });
                // March Quality (relaxation, bisection) moved to Developer Tweaks
                ui.separator();
                egui::CollapsingHeader::new("Interaction Quality")
                    .default_open(true).show(ui, |ui| {
                    ui.checkbox(&mut self.render.interaction_half_res, "Half-Res Interaction")
.default_menu("render.interaction_half_res", serde_json::json!(self.render.interaction_half_res))
                        .on_hover_text("Halve render resolution during gizmo drags");
                    ui.checkbox(&mut self.render.interaction_shadows, "Interaction Shadows")
.default_menu("render.interaction_shadows", serde_json::json!(self.render.interaction_shadows))
                        .on_hover_text("Compute shadow/AO during interaction (off = faster)");
                });
                ui.separator();
                egui::CollapsingHeader::new("Volume Shading")
                    .default_open(true).show(ui, |ui| {
                    if ui.checkbox(&mut self.render.volume_tricubic, "Tricubic Filtering")
.default_menu("render.volume_tricubic", serde_json::json!(self.render.volume_tricubic))
                        .on_hover_text("Smooth Catmull-Rom interpolation (8 taps). OFF = blocky trilinear.")
                        .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                    // ("Volume Shadows" Opacity/Softness/Reach/Bias/Steps REMOVED 2026-07-17f:
                    // dead controls — their only shadow consumer was the VP shader, deleted
                    // 2026-07-13, and their uniform packing corrupted the matcap + fog fields.
                    // Sun shadows are the cascaded maps: see "Sun Shadows" in Post-Processing.)
                    ui.separator();
                    ui.label(egui::RichText::new("Ambient Occlusion").strong());
                    if ui.checkbox(&mut self.render.volume_ao_enabled, "Enable")
.default_menu("render.volume_ao_enabled", serde_json::json!(self.render.volume_ao_enabled))
                        .on_hover_text("SDF ambient occlusion from baked volume")
                        .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                    if self.render.volume_ao_enabled {
                        // All sliders: clamp_to_range(false) lets you type any value
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_opacity, -2.0..=2.0)
                            .text("Opacity").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_intensity, -10.0..=10.0)
                            .text("Intensity").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_bias, -2.0..=2.0)
                            .text("Bias").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        ui.horizontal(|ui| {
                            ui.label("Color");
                            let mut c = self.render.volume_ao_color;
                            if ui.color_edit_button_rgb(&mut c).changed() {
                                self.render.volume_ao_color = c;
                                self.mark_dirty(DirtyFlags::RENDER);
                            }
                            if ui.checkbox(&mut self.render.volume_ao_invert, "Invert")
.default_menu("render.volume_ao_invert", serde_json::json!(self.render.volume_ao_invert))
                                .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        });
                        ui.separator();
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_radius, -20.0..=20.0)
                            .text("Radius").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_step_scale, -6.0..=6.0)
                            .text("Spread").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        if ui.add(egui::Slider::new(&mut self.render.volume_ao_samples, 1..=32)
                            .text("Samples").clamp_to_range(false))
                            .changed() { self.mark_dirty(DirtyFlags::RENDER); }
                        ui.checkbox(&mut self.render.volume_ao_in_orbit, "AO During Orbit")
.default_menu("render.volume_ao_in_orbit", serde_json::json!(self.render.volume_ao_in_orbit));
                    }
                });
                ui.separator();
                // Background color removed — always uses HDR environment
            });

            // Light & Env (hidden in MatCap mode)
            if !is_matcap {
                ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
                egui::CollapsingHeader::new(egui::RichText::new("💡 Light & Env").strong().size(13.0).color(egui::Color32::from_rgb(30,136,229)))
                    .default_open(true).show(ui,|ui|{
                    if self.sel.usel.active_light().is_some() {
                        ui.weak("Editing light via Properties above");
                    } else if self.scene.light_count() > 0 {
                        let first_id = self.scene.scene_graph.lights().next().map(|(_, d)| d.id);
                        if ui.small_button("\u{2600} Select Sun").clicked() {
                            if let Some(id) = first_id {
                                self.select_light(id);
                            }
                        }
                    }
                    ui.separator();
                    // Shared env/IBL body (ui2/shell/render_settings.rs) — single
                    // source of truth with the ui2 Material panel.
                    self.render_env_ibl_body(ui);
                });
            }
            // Multi-GPU settings (Phase 14.2)
            self.draw_multi_gpu_settings(ui);
            ui.add_space(2.0); ui.separator(); ui.add_space(2.0);
    }

    /// Per-frame SPP — keep small so frames stay live. Total quality = many progressive frames.
    pub(crate) fn draw_pt_samples_per_frame_control(&mut self, ui: &mut egui::Ui) -> bool {
        let mut spp = self.render.pt_samples_per_frame.max(1);
        let resp = ui
            .add(egui::Slider::new(&mut spp, 1..=16).text("Samples/Frame"))
            .on_hover_text(
                "Rays per pixel *this frame only*. Keep 1–4 so the viewport never freezes. \
                 Quality comes from progressive accumulation while the camera is still \
                 (Infinite or Noise Threshold stop modes).",
            );
        if resp.changed() {
            self.render.pt_samples_per_frame = spp.max(1);
            true
        } else {
            false
        }
    }

    /// Stop mode + light presets for progressive refine (not freeze-frame SPP).
    pub(crate) fn draw_pt_quality_presets(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("While camera is still").strong().size(11.0));
        ui.horizontal_wrapped(|ui| {
            let mode = self.render.pt_stop_mode;
            if ui
                .selectable_label(mode == 0, "Infinite")
                .on_hover_text("Keep shooting light frames forever until you move the camera")
                .clicked()
            {
                self.render.pt_stop_mode = 0;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .selectable_label(mode == 1, "Noise threshold")
                .on_hover_text("Keep shooting until measured residual is low enough, then idle")
                .clicked()
            {
                self.render.pt_stop_mode = 1;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
            if ui
                .selectable_label(mode == 2, "Max frames")
                .on_hover_text("Stop after a fixed progressive frame count")
                .clicked()
            {
                self.render.pt_stop_mode = 2;
                self.render.pt_converged = false;
                self.mark_dirty(DirtyFlags::RENDER);
            }
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Quick look").strong().size(11.0));
        ui.horizontal_wrapped(|ui| {
            let apply = |app: &mut Self, stop: u8, spp: u32, bounces: u32, noise: f32, max_f: u32, denoise: bool| {
                app.render.pt_stop_mode = stop;
                app.render.pt_samples_per_frame = spp;
                app.render.pt_max_bounces = bounces;
                app.render.pt_noise_threshold = noise;
                app.render.pt_max_accum_frames = max_f;
                app.render.pt_adaptive_spp = true;
                app.render.pt_denoise = denoise;
                app.render.pt_accumulated_frames = 0;
                app.render.pt_converged = false;
                app.mark_dirty(DirtyFlags::RENDER);
            };
            if ui.selectable_label(false, "Fast").on_hover_text("1 spp · 3 bounces · Fast mode · infinite + denoise").clicked() {
                apply(self, 0, 1, 3, 0.02, 65536, true);
                self.render.pt_fast_mode = true;
            }
            if ui.selectable_label(false, "Lookdev").on_hover_text("1 spp · infinite refine · denoise").clicked() {
                apply(self, 0, 1, 6, 0.02, 65536, true);
                self.render.pt_fast_mode = true;
            }
            if ui.selectable_label(false, "Clean stop").on_hover_text("1 spp · stop on noise threshold").clicked() {
                apply(self, 1, 1, 8, 0.015, 65536, true);
                self.render.pt_fast_mode = false;
            }
            if ui.selectable_label(false, "Final").on_hover_text("2 spp · tight noise · denoise off · quality mode").clicked() {
                apply(self, 1, 2, 12, 0.005, 131_072, false);
                self.render.pt_fast_mode = false;
            }
        });
        ui.add_space(4.0);
    }

    /// Floating PT settings window — opened via right-click on PT button or Render menu
    pub(crate) fn draw_pt_settings_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_pt_settings {
            return;
        }
        let mut open = self.ui.show_pt_settings;
        egui::Window::new("Path Tracer Settings")
            .open(&mut open)
            .default_width(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Single source of truth: the shared PT settings body (also used
                // by the ui2 shading-bar PtSettings panel). No slider is authored twice.
                egui::ScrollArea::vertical()
                    .id_salt("v1_pt_window_scroll")
                    .show(ui, |ui| self.render_pt_settings_body(ui));
            });
        self.ui.show_pt_settings = open;
    }

}

#[cfg(test)]
mod tests {
    use super::GridRoute;

    /// S26-1 regression. The legacy route allocates res³ f32 on the CPU AND binds res³×4 as
    /// ONE storage buffer bound whole (viewport_render.rs:510/532/575/647). Both must stay
    /// under the 1 GiB single-allocation budget the codebase already uses for GPU pools
    /// (voxel_gpu_mesh.rs POOL_MAX_BUFFER_BYTES). 512³×4 = 512MB passes; 1024³×4 = 4 GiB fails.
    #[test]
    fn legacy_global_ceiling_fits_a_dense_allocation() {
        let r = GridRoute::LegacyGlobal.max_resolution() as u64;
        let bytes = r * r * r * 4;
        assert!(
            bytes <= 1 << 30,
            "legacy dense ceiling {}³ = {} bytes > 1 GiB — a dense res³×4 buffer this large \
             cannot be bound at binding 2 (max_storage_buffer_binding_size is 2.1GB on a 4090) \
             and the CPU-side vec![100.0; res³] would memset it on the UI thread",
            r, bytes
        );
    }

    /// The ceiling the picker offers must be the ceiling the click's allocation obeys.
    /// Device-independent: fails if anyone raises the dense ceiling above the sparse one.
    #[test]
    fn only_the_sparse_route_exceeds_the_dense_ceiling() {
        assert!(
            GridRoute::LegacyGlobal.max_resolution() <= GridRoute::PerBodyCache.max_resolution(),
            "the legacy dense route must never offer more than the sparse route"
        );
    }

    /// The route MUST be derived from `use_body_vol_cache` — the same field the click forks on.
    /// This is the actual S26-1 defect: a hardcoded `true` made the ceiling describe the sparse
    /// path while the click took the legacy one.
    #[test]
    fn route_keys_on_the_cache_toggle_and_1024_is_legacy_unreachable() {
        assert_eq!(GridRoute::for_cache_state(true), GridRoute::PerBodyCache);
        assert_eq!(GridRoute::for_cache_state(false), GridRoute::LegacyGlobal);
        // The exact reported repro: Per-Body Cache OFF must not offer 1024³.
        assert!(GridRoute::for_cache_state(false).max_resolution() < 1024);
        assert!(GridRoute::for_cache_state(true).max_resolution() >= 1024);
    }
}
