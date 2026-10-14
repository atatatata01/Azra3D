//! Developer tools — rebuild & restart helpers.

use crate::*;

impl AzraApp {
    /// Rebuild the app via `cargo build -p app`, then relaunch the new exe.
    pub(crate) fn dev_rebuild_and_restart(&mut self) {
        self.toast("Building... (cargo build -p app)");
        // Auto-save before rebuild so work isn't lost
        if self.project.scene_modified { self.save_scene(); }
        let exe = std::env::current_exe().unwrap_or_default();
        let project_dir = exe.ancestors().nth(3).unwrap_or(exe.parent().unwrap_or(std::path::Path::new("."))).to_path_buf();
        // Spawn cargo build in a separate process, wait for it, then relaunch
        std::thread::spawn(move || {
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "app"])
                .current_dir(&project_dir)
                .status();
            match status {
                Ok(s) if s.success() => {
                    // Relaunch
                    let _ = std::process::Command::new(&exe).spawn();
                    std::process::exit(0);
                }
                Ok(s) => {
                    eprintln!("[DEV] Build failed with status: {}", s);
                }
                Err(e) => {
                    eprintln!("[DEV] Failed to run cargo: {}", e);
                }
            }
        });
    }

    /// Restart the current exe without rebuilding.
    pub(crate) fn dev_restart_no_build(&mut self) {
        if self.project.scene_modified { self.save_scene(); }
        let exe = std::env::current_exe().unwrap_or_default();
        let _ = std::process::Command::new(&exe).spawn();
        std::process::exit(0);
    }

    /// Dump UI v2 panel sizes/positions/dock state to a **fixed** path for live agent attach:
    /// `%LOCALAPPDATA%/Azra/ui_layout_dump.txt` (always overwritten). Also writes a sequenced
    /// copy `ui_layout_dump_{N}.txt`. Invoked from command palette / Developer Tweaks.
    pub(crate) fn dump_ui2_layout(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let report = self.ui2.layout_debug_report(screen);
        let dir = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Azra");
        if std::fs::create_dir_all(&dir).is_err() {
            self.toast("UI layout dump FAILED — could not create %LOCALAPPDATA%/Azra");
            return;
        }
        let live = dir.join("ui_layout_dump.txt");
        let stamped = {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            dir.join(format!("ui_layout_dump_{n}.txt"))
        };
        match std::fs::write(&live, report.as_bytes()) {
            Ok(()) => {
                let _ = std::fs::write(&stamped, report.as_bytes());
                eprintln!("[UiLayoutDump]\n{report}");
                eprintln!("[UiLayoutDump] wrote {}", live.display());
                self.toast(&format!("UI layout dump → {}", live.display()));
            }
            Err(e) => {
                self.toast(&format!("UI layout dump FAILED: {e}"));
            }
        }
    }

    /// Flight-recorder STATE SNAPSHOT for `vis_recorder::dump_to_file`. Combines the
    /// BodyVolumeCache internals (formatted by `debug_state_snapshot` so no field-visibility
    /// changes are needed) with the GPU-side mesh-slot + cache-flag state that lives outside
    /// the cache. READ-ONLY — no GPU mutations (safe from an egui panel, Frame Ordering ok).
    /// Used by the Developer Tweaks "Dump visual state" button and the VISBUG auto-dump.
    pub(crate) fn build_visdump_snapshot(&self) -> String {
        use std::fmt::Write as _;
        let active = self.scene.active_body_nid;
        let mut s = self.cache.body_vol_cache.debug_state_snapshot(active);
        match self.gpu.as_ref().and_then(|g| g.sdf_body_mesh.get(&active)) {
            Some(slot) => {
                let _ = writeln!(s, "mesh_slot[active]: res={} capacity_res={}",
                    slot.res, slot.capacity_res);
            }
            None => { let _ = writeln!(s, "mesh_slot[active]: None"); }
        }
        // (S2.6: sparse_bake_debug gone — sparse ≥512 is structural, nothing to report.)
        let _ = writeln!(s,
            "dc route: {:?} | cache flags: gpu_sdf_mesh={} gpu_sdf_mesh_resident={} gpu_sdf_mesh_resident_field={} \
             gpu_sdf_mesh_lazy_readback={} gpu_full_bake_path_a={}",
            self.cache.dc_mesh_route(false),
            self.cache.gpu_sdf_mesh, self.cache.gpu_sdf_mesh_resident,
            self.cache.gpu_sdf_mesh_resident_field, self.cache.gpu_sdf_mesh_lazy_readback,
            self.cache.gpu_full_bake_path_a);
        s
    }

    /// Cached AZRA_ORBIT step (0.0 = driver off). Also read by `viewport_render`'s
    /// `user_interacting` so a scripted orbit exercises the SAME interaction-episode /
    /// half-res path a real MMB drag does — else headless runs measure the idle path.
    pub(crate) fn harness_orbit_step() -> f32 {
        use std::sync::OnceLock;
        static STEP: OnceLock<f32> = OnceLock::new();
        *STEP.get_or_init(|| {
            std::env::var("AZRA_ORBIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0)
        })
    }

    /// Harness project loader (env `AZRA_OPEN=<path to .fl4>`): opens a scene once, a few frames
    /// in, so a perf/repro run can be driven end-to-end from a command line instead of needing a
    /// human to click through the file dialog. Pairs with `AZRA_ORBIT` / `AZRA_CAM_DIST` /
    /// `AZRA_SCOREBOARD` — the same diagnostic-only class, no-op without the env var.
    ///
    /// Deliberately at frame 5, not frame 0: the GPU pipelines and the first-frame warmup are
    /// still settling before that, and a 70 MB load landing inside them measures the warmup rather
    /// than the scene.
    pub(crate) fn harness_open_tick(&mut self) {
        if self.diag_frame != 5 {
            return;
        }
        let Ok(path) = std::env::var("AZRA_OPEN") else { return };
        if path.is_empty() {
            return;
        }
        let p = std::path::PathBuf::from(&path);
        if !p.exists() {
            eprintln!("[Harness] AZRA_OPEN: no such file: {path}");
            return;
        }
        eprintln!("[Harness] AZRA_OPEN loading {path}");
        let t0 = std::time::Instant::now();
        self.load_scene_from_path(&p);
        eprintln!("[Harness] AZRA_OPEN loaded in {:.0}ms", t0.elapsed().as_secs_f32() * 1000.0);
    }

    /// Harness state driver (env `AZRA_MODE=polyface|polyedge|polyvert`): selects the first
    /// polygonal body and enters that poly sub-mode a few frames after the scene loads.
    ///
    /// Without this, a scripted run measures OBJECT mode, which never touches `poly_hover_pick`,
    /// the select buffer, the geometry-issue audit, or the outliner Manifold column — i.e. it
    /// misses every path the 2026-08-12 perf work is about. Diagnostic-only, no-op without the env.
    pub(crate) fn harness_mode_tick(&mut self) {
        if self.diag_frame != 60 {
            return;
        }
        let Ok(mode) = std::env::var("AZRA_MODE") else { return };
        let wm = match mode.as_str() {
            "polyface" => PolyWorkMode::Face,
            "polyedge" => PolyWorkMode::Edge,
            "polyvert" => PolyWorkMode::Vertex,
            _ => return,
        };
        let Some((nid, _)) = self.scene.scene_graph.poly_bodies().next() else {
            eprintln!("[Harness] AZRA_MODE: no polygonal body in scene");
            return;
        };
        self.select_single_unified(SceneItemId::Node(nid));
        self.modeling_context = ModelingContext::Polygonal;
        self.poly_work_mode = wm;
        eprintln!("[Harness] AZRA_MODE={mode} on {nid:?}");
    }

    /// AZRA_SCULPT_SHOTS=<dir> — visual repro probe (owner 2026-08-19: "flat orthographic
    /// view shader is broken, also in sculpt mode no highlight on the object"). Enters
    /// Direct PolySculpt on the first poly body (load via AZRA_OPEN), applies a few real
    /// dabs, then captures the SAME scene through the shading×projection×mode matrix so
    /// the brokenness is pixels, not a description:
    ///   01 sculpt+MatCap+persp   02 sculpt+MatCap+ortho   03 sculpt+PBR+ortho
    ///   04 object+PBR+persp      05 object+PBR+ortho
    /// `AZRA_SCULPT_SHOTS_EXIT=1` exits after the last shot. Temporary diagnostic rig —
    /// same class as AZRA_FLICKER_SHOTS (piggybacks the visdiff_shot readback).
    pub(crate) fn harness_sculpt_shots_tick(&mut self) {
        let Ok(dir) = std::env::var("AZRA_SCULPT_SHOTS") else { return };
        if dir.is_empty() { return; }
        let out = std::path::PathBuf::from(&dir);
        use std::sync::Mutex;
        static RIG: Mutex<Option<(core_math::scene_graph::NodeId, glam::Vec3, f32)>> =
            Mutex::new(None);
        self.mark_dirty(crate::DirtyFlags::RENDER); // shots + toggles must re-render
        let shot = |app: &mut crate::AzraApp, name: &str| {
            if app.pending_screenshot.is_some() || app.visdiff_shot.is_some() {
                eprintln!("[SculptShots] WARN: prior shot pending — drop {name}");
                return;
            }
            std::fs::create_dir_all(&out).ok();
            let p = out.join(format!("{name}.png"));
            eprintln!(
                "[SculptShots] frame {} -> {} (mode {:?}, ortho {}, env {}, floor overlays {})",
                app.diag_frame,
                p.display(),
                app.render.shading_mode,
                app.render.ortho,
                app.render.env_enabled,
                app.viewport.show_overlays,
            );
            app.visdiff_shot = Some(p);
        };
        match self.diag_frame {
            60 => {
                let Some((nid, _)) = self.scene.scene_graph.poly_bodies().next() else {
                    // No fixture → differential baseline: an SDF SPHERE (smooth grazing
                    // normals — the matcap disc-edge stressor) in a fresh scene, no
                    // sculpt. The dab/enter arms skip (RIG stays None); the shading×
                    // projection shot matrix still runs, isolating "ortho view shader"
                    // from sculpt state. A SYNTHETIC matcap with BLACK corners (the way
                    // real matcap PNGs are authored) replaces whatever startup matcap
                    // loaded, so disc-edge sampling shows as black — the artifact class.
                    let idx = self.add_primitive_immediate(
                        core_math::primitives::SdfShape::Sphere { radius: 0.9 },
                        glam::Vec3::new(0.0, 0.9, 0.0),
                    );
                    let n = 256u32;
                    let mut px = Vec::with_capacity((n * n * 4) as usize);
                    for y in 0..n {
                        for x in 0..n {
                            let dx = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                            let dy = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                            let d2 = dx * dx + dy * dy;
                            let g = if d2 < 1.0 {
                                let nz = (1.0 - d2).sqrt();
                                ((0.25 + 0.65 * nz) * 255.0) as u8
                            } else {
                                0 // black corners, like a real matcap PNG
                            };
                            px.extend_from_slice(&[g, g, g, 255]);
                        }
                    }
                    self.matcap_data = px;
                    self.matcap_width = n;
                    self.matcap_height = n;
                    self.upload_matcap_to_gpu();
                    self.viewport.camera.target = glam::Vec3::new(0.0, 0.9, 0.0);
                    self.viewport.camera.distance = 5.0;
                    self.viewport.camera.orbit =
                        glam::Quat::from_euler(glam::EulerRot::YXZ, 0.5, -0.45, 0.0);
                    eprintln!("[SculptShots] fresh-scene baseline: sphere prim idx {idx} + synthetic matcap");
                    return;
                };
                if let Some(node) = self.scene.scene_graph.get(nid) {
                    if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } =
                        &node.payload
                    {
                        let mut lo = glam::Vec3::splat(f32::MAX);
                        let mut hi = glam::Vec3::splat(f32::MIN);
                        for v in &mesh.vertices {
                            lo = lo.min(v.position);
                            hi = hi.max(v.position);
                        }
                        *RIG.lock().unwrap() =
                            Some((nid, (lo + hi) * 0.5, (hi - lo).length().max(1e-3)));
                    }
                }
                let Some((nid, center, size)) = *RIG.lock().unwrap() else { return };
                self.poly_sculpt_enter_edit(nid, usize::MAX);
                self.viewport.camera.target = center;
                self.viewport.camera.distance = size * 1.4;
                self.viewport.camera.orbit = glam::Quat::IDENTITY;
                eprintln!("[SculptShots] sculpting {nid:?}, center {center:?}, size {size:.3}");
            }
            90 | 92 | 94 => {
                let Some((nid, center, size)) = *RIG.lock().unwrap() else { return };
                // A few strong Draw dabs on the camera-facing side — enough deformation
                // that a stale-mesh consumer (rim, shadow, LOD) becomes visible.
                let dab = crate::voxel_surface_sculpt::SurfaceDab {
                    pos: center + glam::Vec3::Z * (size * 0.30),
                    dir: glam::Vec3::Z,
                    radius: size * 0.25,
                    strength: 2.0,
                    stroke_tan: glam::Vec3::ZERO,
                    tip_rotation: 0.0,
                };
                let moved = self.poly_sculpt_apply_dabs(
                    nid,
                    std::slice::from_ref(&dab),
                    core_math::voxel_sculpt::VoxelBrush::Draw,
                    1.0,
                );
                eprintln!("[SculptShots] dab frame {}: moved {moved}", self.diag_frame);
            }
            110 => {
                self.render.shading_mode = crate::ShadingMode::MatCap;
            }
            124 => shot(self, "01_sculpt_matcap_persp"),
            136 => {
                self.render.ortho = true;
            }
            150 => shot(self, "02_sculpt_matcap_ortho"),
            162 => {
                self.render.shading_mode = crate::ShadingMode::Pbr;
            }
            176 => shot(self, "03_sculpt_pbr_ortho"),
            188 => {
                self.poly_sculpt_leave_edit();
                self.render.ortho = false;
            }
            202 => shot(self, "04_object_pbr_persp"),
            214 => {
                self.render.ortho = true;
            }
            228 => shot(self, "05_object_pbr_ortho"),
            244 => {
                eprintln!("[SculptShots] DONE");
                if std::env::var_os("AZRA_SCULPT_SHOTS_EXIT").is_some() {
                    std::process::exit(0);
                }
            }
            _ => {}
        }
    }

    /// PR-0 (ZB_RESPONSIVE_SCULPT_PLAN §14): PolySculpt perf harness.
    ///
    /// `AZRA_POLYSCULPT_PERF=<n>` enters Direct PolySculpt on the first polygonal body
    /// (load a remesh fixture via `AZRA_OPEN`) and drives `n` synthetic stamps along an
    /// orbit on the surface — the REAL production path: `ray_pick_mesh_face` (the O(F)
    /// pick PR-4 replaces), `poly_sculpt_stamp`, `poly_sculpt_flush_display` (the full-VB
    /// clone PR-3 replaces), inside one real undo episode (the Full payload PR-5/6
    /// replaces). One JSON line per stamp + a SUMMARY line with p50/p95 — these ARE the
    /// §19 baselines. `AZRA_POLYSCULPT_EXIT=1` exits after the summary (self-service).
    pub(crate) fn harness_polysculpt_tick(&mut self) {
        let Ok(nstr) = std::env::var("AZRA_POLYSCULPT_PERF") else { return };
        let Ok(n) = nstr.parse::<u64>() else { return };
        const START: u64 = 90;
        use std::sync::Mutex;
        static RIG: Mutex<Option<(glam::Vec3, f32)>> = Mutex::new(None);
        static SAMPLES: Mutex<Vec<[f32; 5]>> = Mutex::new(Vec::new());
        if self.diag_frame == 60 {
            let Some((nid, _)) = self.scene.scene_graph.poly_bodies().next() else {
                eprintln!(
                    "[PolySculptPerf] no polygonal body — prims={} materials={} (load ok?)",
                    self.scene.primitives.len(),
                    self.scene.scene_graph.materials().count(),
                );
                return;
            };
            // AABB rig computed ONCE — the ray rig must not add O(V) work to measured frames.
            if let Some(node) = self.scene.scene_graph.get(nid) {
                if let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &node.payload {
                    let mut lo = glam::Vec3::splat(f32::MAX);
                    let mut hi = glam::Vec3::splat(f32::MIN);
                    for v in &mesh.vertices {
                        lo = lo.min(v.position);
                        hi = hi.max(v.position);
                    }
                    let tris = mesh.faces.len();
                    eprintln!(
                        "[PolySculptPerf] body {nid:?}: {} verts / {} faces",
                        mesh.vertices.len(),
                        tris
                    );
                    *RIG.lock().unwrap() = Some(((lo + hi) * 0.5, (hi - lo).length().max(1e-3)));
                }
            }
            // usize::MAX = POLY_SCULPT_DIRECT (Direct Surface — the owner's heavy case).
            self.poly_sculpt_enter_edit(nid, usize::MAX);
            eprintln!("[PolySculptPerf] entered Direct PolySculpt; {n} stamps from frame {START}");
        }
        if self.diag_frame < START {
            return;
        }
        if self.diag_frame >= START + n {
            if self.diag_frame == START + n {
                if self.pending_episode.is_some() {
                    self.commit_episode();
                }
                let samples = SAMPLES.lock().unwrap();
                if !samples.is_empty() {
                    let stat = |col: usize| -> (f32, f32) {
                        let mut v: Vec<f32> = samples.iter().map(|s| s[col]).collect();
                        v.sort_by(|a, b| a.total_cmp(b));
                        (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
                    };
                    let (pick50, pick95) = stat(0);
                    let (q50, q95) = stat(1);
                    let (k50, k95) = stat(2);
                    let (n50, n95) = stat(3);
                    let (u50, u95) = stat(4);
                    let pr = &self.sculpt.poly_profile;
                    eprintln!(
                        "[PolySculptPerf] SUMMARY {{\"stamps\":{},\"pick_ms_p50\":{pick50:.3},\"pick_ms_p95\":{pick95:.3},\"query_ms_p50\":{q50:.3},\"query_ms_p95\":{q95:.3},\"kernel_ms_p50\":{k50:.3},\"kernel_ms_p95\":{k95:.3},\"normals_ms_p50\":{n50:.3},\"normals_ms_p95\":{n95:.3},\"upload_ms_p50\":{u50:.3},\"upload_ms_p95\":{u95:.3},\"upload_bytes\":{},\"seeds_last\":{},\"undo_bytes\":{},\"undo\":\"sparse\"}}",
                        samples.len(),
                        pr.upload_bytes,
                        pr.seed_count,
                        pr.undo_bytes,
                    );
                }
                if std::env::var_os("AZRA_POLYSCULPT_EXIT").is_some() {
                    std::process::exit(0);
                }
            }
            return;
        }
        let Some((nid, _slot)) = self.sculpt.poly_edit else { return };
        let Some((center, ext)) = *RIG.lock().unwrap() else { return };
        if self.diag_frame == START && self.pending_episode.is_none() {
            // Same undo shape as the real input path (sparse first-touch episode, PR-6).
            self.begin_poly_sculpt_episode(nid, None);
        }
        let i = (self.diag_frame - START) as f32;
        let th = i * 0.15;
        let target = center + glam::Vec3::new(th.cos() * ext * 0.25, 0.0, th.sin() * ext * 0.25);
        let cam = center + glam::Vec3::new(th.cos() * ext * 0.35, ext * 1.2, th.sin() * ext * 0.35);
        let rd = (target - cam).normalize_or_zero();
        // PR-4: measure the PRODUCTION producer (`poly_sculpt_pick_hit` — bounded DDA
        // today, GPU crop when PR-4b lands), not a hand-rolled full walk. It writes
        // `poly_profile.pick_ms` itself.
        let Some(hit) = self.poly_sculpt_pick_hit(nid, cam, rd, None) else {
            eprintln!("[PolySculptPerf] stamp {i}: ray MISS (rig aims past the surface)");
            return;
        };
        let hit_local = hit.local;
        // Stamp flushes internally (its tail calls poly_sculpt_flush_display) — a second
        // flush here measured the empty-dirty path instead of the scatter.
        self.poly_sculpt_stamp(nid, hit_local, ext * 0.06, glam::Vec3::Y * (ext * 0.0015));
        let p = self.sculpt.poly_profile;
        eprintln!(
            "[PolySculptPerf] {{\"stamp\":{i},\"pick_ms\":{:.3},\"query_ms\":{:.3},\"kernel_ms\":{:.3},\"normals_ms\":{:.3},\"upload_ms\":{:.3},\"upload_bytes\":{},\"seeds\":{},\"corners\":{},\"normals_mode\":{}}}",
            p.pick_ms, p.query_ms, p.kernel_ms, p.normals_ms, p.upload_ms,
            p.upload_bytes, p.seed_count, p.corner_count, p.normals_mode,
        );
        SAMPLES
            .lock()
            .unwrap()
            .push([p.pick_ms, p.query_ms, p.kernel_ms, p.normals_ms, p.upload_ms]);
    }

    /// Harness EDIT driver (env `AZRA_EDIT=<frames>`): simulates a live vertex-grab drag.
    ///
    /// Opens one undo episode, then nudges the active poly body's vertices a hair every frame for
    /// `frames` frames, requesting a display sync each time. That is the cost profile of a real
    /// grab — mesh content changes per frame, so the display stack re-syncs and
    /// `poly_edit_generation` bumps — which is precisely the state every generation-keyed cache and
    /// every episode gate is judged on. Without it a scripted run only ever measures load + idle.
    ///
    /// It does NOT simulate the pointer, so hover-pick cost still needs a human; everything else
    /// on the edit path (display sync, LOD, cluster bake, select buffer, geometry audit, outliner
    /// manifold, overlays) is exercised. Diagnostic-only, no-op without the env var.
    pub(crate) fn harness_edit_tick(&mut self) {
        let Ok(n) = std::env::var("AZRA_EDIT") else { return };
        let Ok(frames) = n.parse::<u64>() else { return };
        const START: u64 = 90;
        // AZRA_UNDO_CHECK=1 (with AZRA_EDIT_XFORM): after the drag commits, Ctrl+Z headlessly and
        // verify the DISPLAY followed the restored mesh — compare the scene mesh AABB against the
        // cached cpu_mesh AABB once everything settles. Owner 2026-08-13: "ctrl z does not update
        // the surface back"; this makes that reproducible without eyes.
        if std::env::var_os("AZRA_UNDO_CHECK").is_some() {
            let frames: u64 = std::env::var("AZRA_EDIT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            // Two frames after release, not sixty: the real gesture is release→Ctrl+Z immediately.
            // And a SECOND undo shortly after — "does not update" reports are often the second one.
            let undo_at = START + frames + 2;
            let undo2_at = undo_at + 10;
            let check_at = undo2_at + 60;
            if self.diag_frame == undo_at || self.diag_frame == undo2_at {
                eprintln!("[UndoCheck] invoking undo at frame {}", self.diag_frame);
                self.undo();
            }
            if self.diag_frame == check_at {
                let scene_aabb = self.active_poly_nid().and_then(|nid| {
                    self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                        core_math::scene_graph::NodePayload::PolygonalBody { mesh, mesh_cache_id, .. } => {
                            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                            for v in &mesh.vertices {
                                lo = lo.min(v.position.y);
                                hi = hi.max(v.position.y);
                            }
                            Some((lo, hi, *mesh_cache_id))
                        }
                        _ => None,
                    })
                });
                if let Some((mlo, mhi, Some(cid))) = scene_aabb {
                    // TWO oracles. cpu_mesh is the CPU mirror — it can be correct while the SCREEN
                    // is stale. `imported_persistent` is the last-UPLOADED baseline (upload_pending
                    // writes it after the GPU write), i.e. the closest headless proxy for what the
                    // viewport actually shows. The first version of this check used only cpu_mesh
                    // and PASSED while the owner watched a stuck surface.
                    let cpu = self.cache.mesh_cache.get(cid).map(|c| {
                        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                        for v in &c.cpu_mesh.vertices { lo = lo.min(v.y); hi = hi.max(v.y); }
                        (lo, hi)
                    });
                    let gpu = self.cache.mesh_cache.imported_raster_persistent(cid).map(|(v, _)| {
                        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                        for x in v { lo = lo.min(x[1]); hi = hi.max(x[1]); }
                        (lo, hi)
                    });
                    let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
                    let cpu_ok = cpu.map_or(false, |(lo, hi)| near(mlo, lo) && near(mhi, hi));
                    let gpu_ok = gpu.map_or(false, |(lo, hi)| near(mlo, lo) && near(mhi, hi));
                    eprintln!(
                        "[UndoCheck] cpu={} gpu={} mesh_y=[{mlo:.5},{mhi:.5}] cpu_y={:?} uploaded_y={:?}",
                        if cpu_ok { "PASS" } else { "FAIL" },
                        if gpu_ok { "PASS" } else { "FAIL — SCREEN IS STALE" },
                        cpu, gpu,
                    );
                }
            }
        }
        if self.diag_frame < START || self.diag_frame >= START + frames {
            if self.diag_frame == START + frames && self.pending_episode.is_some() {
                self.commit_episode();
                eprintln!("[Harness] AZRA_EDIT done at frame {}", self.diag_frame);
            }
            return;
        }
        // AZRA_EDIT_XFORM=<n>: drive a REAL Poly Transform grab of `n` vertices instead of poking
        // the mesh directly — the owner's exact repro (`tool=Poly Transform`,
        // `repaint_next=pointer_down`, `input=50 ms`). Poking the mesh exercises the display sync
        // but never `poly_update_transform`, which is where that 50 ms lives.
        if let Ok(nsel) = std::env::var("AZRA_EDIT_XFORM") {
            let n: usize = nsel.parse().unwrap_or(0);
            if self.diag_frame == START {
                let Some(nid) = self.active_poly_nid() else { return };
                let count = self
                    .scene
                    .scene_graph
                    .get(nid)
                    .and_then(|node| match &node.payload {
                        core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } => {
                            Some(mesh.vertices.len())
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                let take = if n == 0 { count } else { n.min(count) };
                self.poly_sel.selected_vertices = (0..take as u32).collect();
                self.poly_work_mode = PolyWorkMode::Vertex;
                self.poly_start_transform(TransformKind::Grab, egui::pos2(400.0, 300.0));
                eprintln!("[Harness] AZRA_EDIT_XFORM grab of {take}/{count} verts");
            }
            // MONOTONIC drift, not jitter: the undo check compares AABBs, and a net-zero wiggle
            // makes "stale dragged display" and "correctly restored display" identical.
            let i = (self.diag_frame - START) as f32;
            self.poly_update_transform(egui::pos2(400.0 + i * 2.0, 300.0 - i * 1.5), false, false);
            return;
        }
        if self.diag_frame == START {
            self.begin_episode("Harness Edit");
            eprintln!("[Harness] AZRA_EDIT begin ({frames} frames)");
        }
        let Some(nid) = self.active_poly_nid() else { return };
        let i = self.diag_frame - START;
        let dy = if i % 2 == 0 { 0.001 } else { -0.001 };
        let Some(node) = self.scene.scene_graph.get_mut(nid) else { return };
        let core_math::scene_graph::NodePayload::PolygonalBody { mesh, .. } = &mut node.payload
        else {
            return;
        };
        // Touch a bounded slice, not the whole cage: a real grab moves a selection, and moving all
        // 323k verts would measure the mutation instead of the per-frame machinery around it.
        let m = std::sync::Arc::make_mut(mesh);
        let n_touch = m.vertices.len().min(2000);
        for v in m.vertices.iter_mut().take(n_touch) {
            v.position.y += dy;
        }
        self.poly_request_display_sync(nid);
    }

    /// Harness BOOLEAN matrix (env `AZRA_BOOL=<trials>`): runs the mesh boolean kernel against the
    /// loaded body with generated cutters, and reports SPEED and RELIABILITY per trial.
    ///
    /// Reliability is judged with `result_installable` — the tool's OWN acceptance oracle
    /// (validate + edge-manifold + closed + no folded coplanar pairs), so a "pass" here means the
    /// boolean tool would actually have installed the result, not merely that the kernel returned
    /// something. `None` (kernel refusal) and "returned but refused" are reported separately: they
    /// are different failures and conflating them hides which half is broken.
    ///
    /// Cutters are deterministic (no RNG — a repro has to be re-runnable): a lattice of boxes over
    /// the target AABB at three scales, plus rotated variants, so coplanar and grazing cases are
    /// exercised rather than only clean interior cuts.
    pub(crate) fn harness_bool_tick(&mut self) {
        if self.diag_frame != 100 {
            return;
        }
        let Ok(n) = std::env::var("AZRA_BOOL") else { return };
        let trials: usize = n.parse().unwrap_or(12);
        let Some(nid) = self.active_poly_nid() else {
            eprintln!("[BoolMatrix] no active poly body");
            return;
        };
        let Some(target) = self.poly_node_mesh(nid) else { return };
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &target.vertices {
            lo = lo.min(v.position);
            hi = hi.max(v.position);
        }
        let size = hi - lo;
        let center = (lo + hi) * 0.5;
        eprintln!(
            "[BoolMatrix] target verts={} faces={} aabb={:.2}x{:.2}x{:.2} closed={} manifold={}",
            target.vertices.len(), target.face_count(), size.x, size.y, size.z,
            core_math::mesh_boolean::boundary_half_edge_count(&target) == 0,
            target.is_manifold().is_ok(),
        );

        use core_math::mesh_boolean::BoolOp;
        let ops = [("Difference", BoolOp::Difference), ("Union", BoolOp::Union),
                   ("Intersect", BoolOp::Intersect)];
        let mut pass = 0usize;
        let mut refused = 0usize;
        let mut none = 0usize;
        let mut total_ms = 0.0f32;

        for t in 0..trials {
            // Deterministic sweep: scale tier, offset along the diagonal, and an axis rotation.
            let tier = (t % 3) as f32; // 0 small, 1 medium, 2 large
            let frac = 0.12 + 0.18 * tier;
            let he = size * frac * 0.5;
            let step = (t / 3) as f32;
            let off = (step * 0.17) - 0.34; // straddles the centre, exits the far side
            let pos = center + size * off * 0.5;
            let rot_deg = (t as f32) * 23.0; // grazing / coplanar-ish at some steps
            let m = glam::Mat4::from_rotation_translation(
                glam::Quat::from_rotation_y(rot_deg.to_radians()), pos,
            );
            let cutter = crate::poly::boolean::transformed(
                &core_math::half_edge::HalfEdgeMesh::new_box(he), m,
            );
            let (name, op) = ops[t % ops.len()];
            let t0 = std::time::Instant::now();
            let res = core_math::mesh_boolean::mesh_boolean(&target, &cutter, op);
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            total_ms += ms;
            match res {
                None => {
                    none += 1;
                    eprintln!("[BoolMatrix] {t:02} {name:10} tier={tier} off={off:+.2} \
                               rot={rot_deg:5.0} -> KERNEL RETURNED NONE  {ms:7.1}ms");
                }
                Some(r) => {
                    let ok = crate::poly::boolean::result_installable(&r);
                    if ok { pass += 1; } else { refused += 1; }
                    eprintln!(
                        "[BoolMatrix] {t:02} {name:10} tier={tier} off={off:+.2} rot={rot_deg:5.0} \
                         -> {} v={} f={} closed={} manifold={} folded={}  {ms:7.1}ms",
                        if ok { "INSTALLABLE" } else { "REFUSED    " },
                        r.vertices.len(), r.face_count(),
                        core_math::mesh_boolean::boundary_half_edge_count(&r) == 0,
                        r.is_manifold().is_ok(),
                        r.count_folded_coplanar_pairs(),
                    );
                }
            }
        }
        eprintln!(
            "[BoolMatrix] DONE trials={trials} installable={pass} refused={refused} none={none} \
             | total={total_ms:.0}ms avg={:.1}ms",
            total_ms / trials.max(1) as f32
        );
    }

    /// Harness orbit driver (env `AZRA_ORBIT=<deg-per-frame>`, e.g. 0.5): rotates the camera
    /// continuously so headless scoreboard runs measure MOVING-camera cost (accumulation
    /// resets, reprojection, upload churn) instead of the idle steady state. No-op without
    /// the env var. Diagnostic-only, same class as autorepro.
    pub(crate) fn harness_orbit_tick(&mut self) {
        // AZRA_CAM_DIST=<f32>: one-shot camera distance override (zoomed-in sabotage
        // runs need real frustum-out volume; stress spawners frame the whole grid).
        if let Ok(d) = std::env::var("AZRA_CAM_DIST") {
            if let Ok(d) = d.parse::<f32>() {
                if (self.viewport.camera.distance - d).abs() > 1e-3 {
                    self.viewport.camera.distance = d;
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
        }
        let step = Self::harness_orbit_step();
        if step == 0.0 {
            return;
        }
        let q = glam::Quat::from_rotation_y(step.to_radians());
        self.viewport.camera.orbit = (q * self.viewport.camera.orbit).normalize();
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// DIAGNOSTIC auto-repro (env `AZRA_AUTOREPRO=1`): replay the owner's 1024 crash sequence
    /// inside the REAL app loop with zero interaction — set the active
    /// body's Grid to 1024 (the exact picker action from properties_render), then add a cylinder
    /// prim (the exact `add_primitive_immediate` action, which auto-enters WorkMode::Vertex),
    /// then simulate a short create-drag by growing the cylinder through the production prim-edit
    /// path. Run the app with stderr captured to reproduce a startup-to-crash trace headlessly.
    /// No-op without the env var.
    pub(crate) fn autorepro_tick(&mut self) {
        use std::sync::OnceLock;
        static MODE: OnceLock<u32> = OnceLock::new();
        let mode = *MODE.get_or_init(|| match std::env::var("AZRA_AUTOREPRO") {
            Ok(v) if v == "2" || v.eq_ignore_ascii_case("multibody") => 2,
            Ok(_) => 1,
            Err(_) => 0,
        });
        if mode == 0 { return; }
        if mode == 2 { return self.autorepro_multibody_tick(); }
        let f = self.diag_frame;
        match f {
            180 => {
                let active = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
                // S2.6: no toggle to flip — sparse ≥512 Object Mode is structural.
                eprintln!("[AutoRepro] frame={f}: Grid 1024 on {active:?} (sparse is structural)");
                // The picker's exact action (properties_render.rs Grid picker):
                self.cache.body_vol_cache.set_body_resolution(active, 1024);
                self.invalidate_pt_sdf_snapshot(active);
                self.trigger_body_vol_bake();
                self.mark_dirty(DirtyFlags::RENDER);
            }
            700 => {
                eprintln!("[AutoRepro] frame={f}: add cylinder prim (owner action)");
                let idx = self.add_primitive_immediate(
                    core_math::primitives::SdfShape::Cylinder { radius: 0.3, half_height: 0.5 },
                    glam::Vec3::new(1.2, 0.0, 0.0),
                );
                eprintln!("[AutoRepro] added prim idx={idx} work_mode={:?}", self.tool.work_mode);
            }
            900..=1020 => {
                // Simulated create-drag: grow the cylinder each frame through the production
                // prim-edit path (write_prim → graph+flat cache, then the prim dirty region) —
                // the interaction that ballooned the sparse pool 355MB→1.43GB at 1024 before
                // the oversized-window escalation fix.
                let t = (f - 900) as f32 / 120.0;
                if let Some(idx) = (0..self.scene.primitives.len())
                    .find(|&i| matches!(self.scene.primitives[i].shape,
                        core_math::primitives::SdfShape::Cylinder { .. }))
                {
                    self.write_prim(idx, |p| {
                        p.shape = core_math::primitives::SdfShape::Cylinder {
                            radius: 0.3 + 0.5 * t, half_height: 0.5 + 1.2 * t };
                    });
                    let uid = self.scene.primitives[idx].prim_uid;
                    if let Some(nid) = self.scene.prim_body_map.get(idx).map(|e| e.0) {
                        self.cache.body_vol_cache.mark_prim_dirty(nid, uid, &self.scene.scene_graph);
                    }
                    self.trigger_body_vol_bake();
                }
            }
            n if n >= 800 && n % 400 == 0 => {
                // Steady-state probe: after the add, NOTHING else should be dirty. Recurring
                // dirty/in-flight state at every probe = a self-sustaining re-bake loop.
                eprintln!("[AutoRepro] frame={f}: probe (prims={} work_mode={:?})\n{}",
                    self.scene.primitives.len(), self.tool.work_mode,
                    self.build_visdump_snapshot());
            }
            _ => {}
        }
    }

    /// DIAGNOSTIC auto-repro #2 (env `AZRA_AUTOREPRO=2`): the owner's multi-body break sequence
    /// (2026-07-13 report on 459aac2+e07b13c): (1) create a cylinder in body A (production
    /// `add_primitive_immediate` + create-drag growth through `write_prim`); (2) create a NEW
    /// empty body with the N shortcut (`create_new_sdf_body` — switches active to B); (3) drag
    /// the cylinder (production `write_prim_transform` path — marks dirty regions on A while B
    /// is active). Probes: after the drag settles, body A's CPU grid must be INSIDE (<0) at the
    /// cylinder's NEW position and OUTSIDE at the OLD one; then a Grid-picker resolution change
    /// (the owner's recovery action) must rebake. Exits with code 0 (all probes pass) / 7 (fail).
    fn autorepro_multibody_tick(&mut self) {
        use std::sync::OnceLock;
        static BODY_A: OnceLock<core_math::scene_graph::NodeId> = OnceLock::new();
        static FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let f = self.diag_frame;
        // Sample body `nid`'s cached mesh INPUT at a body-local point (nearest voxel):
        // dense grid value, or — for a brick-resident SparseBricks body (empty grid by
        // design) — a synthetic sign from the FROZEN sparse index tag (-1 interior/surface,
        // +far exterior). Returns None when neither input exists, f32::MAX when the point
        // is outside the entry bounds.
        let sample = |s: &Self, nid: core_math::scene_graph::NodeId, p: glam::Vec3| -> Option<f32> {
            let (grid, res, gmin, gmax, _) = s.cache.body_vol_cache.body_grid(nid)?;
            if grid.is_empty() || res < 2 {
                // Sparse regime: probe the frozen classifier index instead.
                return match s.cache.body_vol_cache.sparse_frozen_tag_at(nid, p) {
                    Some(core_math::chunked_dc::BRICK_TAG_EXTERIOR) => Some(f32::MAX),
                    Some(_) => Some(-1.0), // interior or allocated surface brick
                    None => {
                        // Frozen index exists but point outside bounds → outside; no index → None.
                        if s.cache.body_vol_cache.brick_resident_gen(nid).is_some() {
                            Some(f32::MAX)
                        } else { None }
                    }
                };
            }
            let n = res as usize;
            if grid.len() != n * n * n { return None; }
            let t = (p - gmin) / (gmax - gmin).max(glam::Vec3::splat(1e-9));
            if t.min_element() < 0.0 || t.max_element() > 1.0 { return Some(f32::MAX); }
            let v = (t * (res - 1) as f32).round();
            let (x, y, z) = (v.x as usize, v.y as usize, v.z as usize);
            Some(grid[x.min(n-1) + y.min(n-1) * n + z.min(n-1) * n * n])
        };
        let cyl_idx = |s: &Self| (0..s.scene.primitives.len())
            .find(|&i| matches!(s.scene.primitives[i].shape,
                core_math::primitives::SdfShape::Cylinder { .. }));
        const CYL_START: glam::Vec3 = glam::Vec3::new(1.2, 0.0, 0.0);
        const DRAG_DELTA: glam::Vec3 = glam::Vec3::new(1.5, 0.0, 0.8); // total drag distance
        let mut fail = |what: &str| {
            eprintln!("[AutoRepro2] FAIL: {what}");
            FAILURES.lock().unwrap().push(what.to_string());
        };
        // Optional resolution for body A (env AZRA_AUTOREPRO_RES, e.g. 512/1024 → sparse regime).
        static RES: OnceLock<Option<u32>> = OnceLock::new();
        let res_override = *RES.get_or_init(|| std::env::var("AZRA_AUTOREPRO_RES").ok()
            .and_then(|v| v.parse().ok()));
        match f {
            200 => {
                if let Some(r) = res_override {
                    let active = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
                    eprintln!("[AutoRepro2] frame={f}: Grid picker {r} on {active:?} (owner regime)");
                    self.cache.body_vol_cache.set_body_resolution(active, r);
                    self.invalidate_pt_sdf_snapshot(active);
                    self.trigger_body_vol_bake();
                    self.mark_dirty(DirtyFlags::RENDER);
                }
            }
            240 => {
                let a = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
                let _ = BODY_A.set(a);
                eprintln!("[AutoRepro2] frame={f}: body A={a:?} — add cylinder (owner step 1)");
                let idx = self.add_primitive_immediate(
                    core_math::primitives::SdfShape::Cylinder { radius: 0.3, half_height: 0.5 },
                    CYL_START,
                );
                eprintln!("[AutoRepro2] added cylinder idx={idx} local_pos={:?} work_mode={:?}",
                    self.scene.primitives[idx].position, self.tool.work_mode);
            }
            260..=320 => {
                // Create-drag: grow the cylinder through the production prim-edit path.
                let t = (f - 260) as f32 / 60.0;
                if let Some(idx) = cyl_idx(self) {
                    self.write_prim(idx, |p| {
                        p.shape = core_math::primitives::SdfShape::Cylinder {
                            radius: 0.3 + 0.2 * t, half_height: 0.5 + 0.4 * t };
                    });
                    let uid = self.scene.primitives[idx].prim_uid;
                    if let Some(nid) = self.scene.prim_body_map.get(idx).map(|e| e.0) {
                        self.cache.body_vol_cache.mark_prim_dirty(nid, uid, &self.scene.scene_graph);
                    }
                    self.trigger_body_vol_bake();
                }
            }
            620 => {
                // Probe P1: A settled + healthy before the N step.
                let a = *BODY_A.get().unwrap();
                let p = cyl_idx(self).map(|i| self.scene.primitives[i].position).unwrap_or(CYL_START);
                let d = sample(self, a, p);
                eprintln!("[AutoRepro2] frame={f}: P1 pre-N health A={a:?} sdf(cyl)={d:?} \n{}",
                    self.build_visdump_snapshot());
                match d {
                    Some(v) if v < 0.0 => eprintln!("[AutoRepro2] P1 OK (inside)"),
                    other => fail(&format!("P1: body A grid not inside at cylinder pos (got {other:?}) — baseline broken before N")),
                }
            }
            640 => {
                eprintln!("[AutoRepro2] frame={f}: N shortcut — create_new_sdf_body (owner step 2)");
                self.create_new_sdf_body();
                eprintln!("[AutoRepro2] active_body now {:?}", self.scene.active_body_nid);
            }
            780..=900 => {
                // Owner step 3: click-drag the cylinder. The click (input_select) re-activates
                // body A; emulate it once at drag start, then move the prim per frame through the
                // production write path (write_prim_transform → mark_prim_dirty_aabb).
                let a = *BODY_A.get().unwrap();
                // AZRA_AUTOREPRO_NOCLICK=1 → drag WITHOUT re-activating A (bakes for a
                // NON-active body while the resident volume belongs to B — suspect #2 framing).
                static NOCLICK: OnceLock<bool> = OnceLock::new();
                let noclick = *NOCLICK.get_or_init(|| std::env::var_os("AZRA_AUTOREPRO_NOCLICK").is_some());
                if f == 780 && !noclick {
                    eprintln!("[AutoRepro2] frame={f}: click on cylinder (select body A) + drag start");
                    self.scene.active_body_nid = a;
                    self.select_scene_node(a);
                }
                if f == 780 && noclick {
                    eprintln!("[AutoRepro2] frame={f}: drag start WITHOUT activating A (active stays {:?})",
                        self.scene.active_body_nid);
                }
                if let Some(idx) = cyl_idx(self) {
                    let t = (f - 780) as f32 / 120.0;
                    let pos = CYL_START + DRAG_DELTA * t;
                    let (rot, scl) = {
                        let p = &self.scene.primitives[idx];
                        (p.rotation_deg, p.scale)
                    };
                    self.write_prim_transform(idx, pos, rot, scl);
                    self.trigger_body_vol_bake();
                }
            }
            1150 => {
                // Probe P3 (THE BUG PROBE): after the drag settles, A's cached grid must have
                // followed the cylinder — inside at the new position, outside at the old one.
                let a = *BODY_A.get().unwrap();
                let new_p = cyl_idx(self).map(|i| self.scene.primitives[i].position)
                    .unwrap_or(CYL_START + DRAG_DELTA);
                let d_new = sample(self, a, new_p);
                let d_old = sample(self, a, CYL_START);
                eprintln!("[AutoRepro2] frame={f}: P3 post-drag A={a:?} sdf(new={new_p:?})={d_new:?} sdf(old={CYL_START:?})={d_old:?}\n{}",
                    self.build_visdump_snapshot());
                match d_new {
                    Some(v) if v < 0.0 => eprintln!("[AutoRepro2] P3 OK: grid refreshed at the dragged position"),
                    other => fail(&format!("P3: body A grid did NOT refresh after multi-body drag (sdf at new pos = {other:?}) — the owner's 'stops updating'")),
                }
                if let Some(v) = d_old {
                    if v < 0.0 && v != f32::MAX {
                        fail(&format!("P3b: stale surface remains at the OLD position (sdf={v}) — ghost/broken surface"));
                    }
                }
            }
            1160 => {
                // Before the recovery action the owner selects the broken body (the picker acts
                // on the ACTIVE top-level body). No-op in the click variant (A already active).
                let a = *BODY_A.get().unwrap();
                if self.scene.active_body_nid != a {
                    self.scene.active_body_nid = a;
                    self.select_scene_node(a);
                }
            }
            1170 => {
                // Owner recovery action: Grid picker resolution change (exact picker code path).
                let active = self.scene.scene_graph.top_level_body(self.scene.active_body_nid);
                eprintln!("[AutoRepro2] frame={f}: Grid picker 256 on active={active:?} (owner recovery action)");
                if self.cache.use_body_vol_cache {
                    self.cache.body_vol_cache.set_body_resolution(active, 256);
                    self.trigger_body_vol_bake();
                }
                self.mark_dirty(DirtyFlags::RENDER);
            }
            1400 => {
                // Probe P4: the resolution change must have produced a CLEAN rebake of the
                // dragged body at 256 with correct content.
                let a = *BODY_A.get().unwrap();
                let new_p = cyl_idx(self).map(|i| self.scene.primitives[i].position)
                    .unwrap_or(CYL_START + DRAG_DELTA);
                let entry = self.cache.body_vol_cache.body_grid(a).map(|(g, r, ..)| (g.len(), r));
                let d_new = sample(self, a, new_p);
                eprintln!("[AutoRepro2] frame={f}: P4 post-res-change A={a:?} entry(len,res)={entry:?} sdf(new)={d_new:?}\n{}",
                    self.build_visdump_snapshot());
                match entry {
                    Some((_, r)) if r == 256 => {}
                    other => fail(&format!("P4: resolution change did not rebake body A at 256 (entry={other:?}) — the owner's 'res change no longer recovers'")),
                }
                match d_new {
                    Some(v) if v < 0.0 => eprintln!("[AutoRepro2] P4 OK: 256 rebake has the cylinder at its dragged position"),
                    other => fail(&format!("P4b: 256 rebake content wrong at dragged position (sdf={other:?})")),
                }
            }
            1450 => {
                // P5: ZERO uncaptured wgpu errors across the whole sequence. A failed DC
                // dispatch (BufferRangeTooLarge → encoder-invalid cascade) leaves the CPU cache
                // healthy but the DRAWN surface broken — this counter is the only headless
                // observable for the owner's "surface breaks and stops updating".
                let gpu_errs = crate::vis_recorder::GPU_UNCAPTURED_ERRORS
                    .load(std::sync::atomic::Ordering::Relaxed);
                if gpu_errs > 0 {
                    fail(&format!("P5: {gpu_errs} uncaptured wgpu error(s) during the sequence — a GPU dispatch failed (the on-screen break)"));
                }
                let fails = FAILURES.lock().unwrap();
                if fails.is_empty() {
                    eprintln!("[AutoRepro2] ALL PROBES PASSED");
                    std::process::exit(0);
                } else {
                    eprintln!("[AutoRepro2] {} PROBE FAILURE(S):", fails.len());
                    for m in fails.iter() { eprintln!("[AutoRepro2]   - {m}"); }
                    std::process::exit(7);
                }
            }
            _ => {}
        }
    }
}
