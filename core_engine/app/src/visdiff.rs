//! AZRA_VISDIFF — automated end-to-end VISUAL verification harness (owner 2026-07-23:
//! "I still don't see the noise change"; LIVE-MATERIAL-FEEL handoff in COMPLETE_BUG_TRACKER.md).
//!
//! Run: `AZRA_VISDIFF=<out_dir> cargo run -p app`. The app drives ITSELF: builds the owner's
//! exact setup (default SDF box + graph material + Noise.Value → Master.Albedo), scrubs the
//! noise Scale, and captures viewport screenshots at fixed frames in BOTH Hybrid and PathTrace.
//! An external script pixel-diffs the shots — the PIXELS are the verdict, no human in the loop.
//!
//! Frame plan (compared pairs → what they prove):
//!   A  (hybrid, scale 4)                 ─┐
//!   B1 (hybrid, scale 20, +6 frames)      ├─ A↔B1: LIVE layer moves the surface (no bake wait)
//!   B2 (hybrid, scale 20, settled+baked) ─┘  B1↔B2: bake handoff changes/kills the look?
//!   C  (PT, scale 20, accumulated)       ─┐
//!   D1 (PT, scale 4, +6 frames)           ├─ C↔D1: PT reacts live; D1↔D2: PT bake convergence
//!   D2 (PT, scale 4, settled)            ─┘
//! Console prints [VISDIFF] markers per step; exits by itself.
//!
//! Gate (M0+): A↔B1 mean|Δ| must be NON-zero (live path moves pixels). Byte-identical pairs
//! are the white-on-white / live-noise-killed regression class.

use crate::{AzraApp, DirtyFlags, NodeGraphTarget, ShadingMode};
use core_math::primitives::PbrMaterial;

/// `AZRA_SHOT_FULLRES=1`: harness captures are saved at the full viewport resolution,
/// uncropped and with no size cap (read once). The default 512² centre-crop thumbnail is fine
/// for "did the pixels change" diffs but erases exactly what PT banding/noise analysis needs.
pub(crate) fn harness_full_res_shots() -> bool {
    static FULL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FULL.get_or_init(|| std::env::var("AZRA_SHOT_FULLRES").is_ok_and(|v| v == "1"))
}

/// True while `AZRA_VISDIFF` (or the `AZRA_PARITY` gate) drives the app: the harness owns
/// all input (read once).
pub(crate) fn harness_owns_input() -> bool {
    static OWNS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OWNS.get_or_init(|| {
        std::env::var("AZRA_VISDIFF").is_ok_and(|d| !d.is_empty()) || crate::parity_gate::parity_active()
    })
}

impl AzraApp {
    /// One tick of the harness state machine. Called at the top of `update()` ONLY when the
    /// AZRA_FLICKER_SHOTS=<dir>: capture N consecutive-ish screenshots of whatever
    /// scene is loaded (static camera, no scripted setup) so a temporal artifact can
    /// be pixel-diffed offline instead of eyeballed. Piggybacks the visdiff
    /// screenshot plumbing; shots spaced 4 frames so the staging map completes.
    pub(crate) fn flicker_shots_tick(&mut self) {
        let out_dir = match std::env::var("AZRA_FLICKER_SHOTS") {
            Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
            _ => return,
        };
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        self.mark_dirty(DirtyFlags::RENDER); // capture path lives inside the dirty render
        // Let the scene fully settle (DAG bake + residency + textures) first.
        if f < 400 || f % 4 != 0 {
            return;
        }
        let n = (f - 400) / 4;
        if n >= 8 {
            return;
        }
        if self.pending_screenshot.is_some() {
            return;
        }
        std::fs::create_dir_all(&out_dir).ok();
        let p = out_dir.join(format!("flick_{n:02}.png"));
        eprintln!("[FLICKSHOT] frame {f} -> {}", p.display());
        self.visdiff_shot = Some(p);
    }

    /// AZRA_VISDIFF env var is set (zero cost otherwise — a single env check at the call site).
    pub(crate) fn visdiff_tick(&mut self) {
        let out_dir = match std::env::var("AZRA_VISDIFF") {
            Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
            _ => return,
        };
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        if std::env::var_os("AZRA_PT_REGRESSION").is_some() {
            if (471..700).contains(&f) {
                self.visdiff_frame = if self.pt_pipeline_ready() && self.render.pt_accumulated_frames >= 128 { 699 } else { 471 };
            }
            if (721..950).contains(&f) {
                self.visdiff_frame = if self.render.pt_accumulated_frames >= 256 { 949 } else { 721 };
            }
            if f == 1000 && self.pending_screenshot.is_some() {
                self.visdiff_frame = 999;
                return;
            }
        }
        // CRITICAL: dispatch_gpu_render early-returns when !is_dirty && !pt_accumulating,
        // and the screenshot start/complete path lives INSIDE that function. Without a dirty
        // bit, shot requests sit forever (or all collapse onto the next dirty frame) — the
        // byte-identical A/B1/B2/C class we measured on M0. Force a re-render every harness
        // frame so captures and progressive PT actually run.
        self.mark_dirty(DirtyFlags::RENDER);

        let shot = |app: &mut AzraApp, name: &str| {
            app.visdiff_log_material_state(name);
            if std::env::var_os("AZRA_PT_REGRESSION").is_some() {
                // Honest PT evidence per capture: accumulated samples, the asynchronous GPU
                // PT-pass time (timestamp slot, EMA), traversal backend and render size.
                let (native, status, size) = app.gpu.as_ref().map_or((false, None, (0, 0)), |g| {
                    (g.native_rt_active, Some(g.native_rt_status), g.texture_size)
                });
                eprintln!(
                    "[PTPROBE] {name} frames={} spp/frame={} bounces={} pt_gpu_ms={:.2} native_rt={native} status={status:?} size={}x{}",
                    app.render.pt_accumulated_frames, app.render.pt_samples_per_frame,
                    app.render.pt_max_bounces, app.render.pt_last_frame_ms, size.0, size.1,
                );
            }
            let p = out_dir.join(format!("{name}.png"));
            eprintln!("[VISDIFF] frame {} shot -> {}", app.visdiff_frame, p.display());
            // Always set the path. viewport_render refuses to stomp an in-flight staging
            // buffer (drops this request if one is already pending — shots are spaced far
            // enough that the prior map always completes first).
            if app.pending_screenshot.is_some() {
                eprintln!("[VISDIFF] WARN: prior screenshot still pending — drop {name}");
                return;
            }
            app.visdiff_shot = Some(p);
            // Belt-and-braces: shot frames must re-render (texture copy is of THIS frame).
            app.mark_dirty(DirtyFlags::RENDER);
        };

        match f {
            // ---- Setup: the owner's exact configuration -------------------------------------
            // The default scene is EMPTY (run 1 shipped 5 byte-identical background shots —
            // the harness photographed nothing). Create the box FIRST.
            20 => {
                if std::env::var_os("AZRA_PT_REGRESSION").is_some() {
                    std::fs::create_dir_all(&out_dir).expect("create PT regression output directory");
                    self.project.project_path = Some(out_dir.join("probe.fl4"));
                }
                // Materials are only meaningful in PBR (not MatCap). Force explicitly.
                self.render.shading_mode = ShadingMode::Pbr;
                let idx = self.add_primitive_immediate(
                    core_math::primitives::SdfShape::Box {
                        half_extents: glam::Vec3::splat(0.8),
                        bevel: Default::default(),
                    },
                    glam::Vec3::ZERO,
                );
                eprintln!("[VISDIFF] created box prim idx {idx}; prim count {}", self.scene.primitives.len());
            }
            30 => {
                std::fs::create_dir_all(&out_dir).ok();
                assert!(!self.scene.primitives.is_empty(), "visdiff: scene must have the box");
                use crate::shader_nodes::{NodeGraph, NodeType};
                // Graph: Noise.Value (out 0) -> Master.Albedo (in 0). Exactly the owner's wire.
                let mut g = NodeGraph {
                    nodes: vec![],
                    connections: vec![],
                    next_id: 0,
                    wgsl_cache: None,
                    bake_res: 256,
                    shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
                };
                let noise = g.add_node(NodeType::Noise, [200.0, 100.0]);
                let out = g.add_node(NodeType::MaterialOutput, [520.0, 120.0]);
                if let Some(n) = g.find_node_mut(noise) {
                    n.input_defaults[1].value_f32 = 4.0; // Scale
                    n.input_defaults[2].value_f32 = 4.0; // Octaves
                }
                if std::env::var_os("AZRA_PT_GRAY_PROBE").is_none() {
                    assert!(g.connect(noise, 0, out, 0), "visdiff: Noise->Albedo connect failed");
                }

                let mid = self.scene.scene_graph.add_material("visdiff", PbrMaterial::default());
                // Assign to the first prim (default scene box).
                if !self.scene.primitives.is_empty() {
                    self.transaction("visdiff assign", |s| {
                        s.write_prim(0, |p| p.material_id = mid);
                    });
                }
                self.ui.node_graph = g.clone();
                self.ui.node_graph_target = Some(NodeGraphTarget::Material(mid));
                self.ui.material_node_graphs.insert(mid.0, g);
                // Same sequence as a live editor change (win_node_editor node_changed arm).
                self.commit_material_node_graph_edit(mid, false);
                if self.apply_live_gpu_noise_material(mid) {
                    self.schedule_live_material_graph_bake(mid);
                } else {
                    eprintln!("[VISDIFF] WARN: live GPU noise NOT engaged (extract returned None)");
                    self.schedule_live_material_graph_bake(mid);
                }
                self.mark_dirty(DirtyFlags::RENDER);
                self.visdiff_log_material_state("setup");
                eprintln!("[VISDIFF] setup done: material {mid:?} on prim 0, Noise->Albedo");
            }
            // ---- Hybrid probes ---------------------------------------------------------------
            200 => shot(self, "A_hybrid_scale4"),
            240 => {
                self.visdiff_set_scale(20.0);
                eprintln!("[VISDIFF] scale -> 20");
            }
            // B1: a few frames after scrub — live GPU noise must already show; bake may still
            // be in flight. (+6 was tight if a screenshot was still pending.)
            250 => shot(self, "B1_hybrid_scale20_live"),
            460 => shot(self, "B2_hybrid_scale20_settled"),
            // ---- PathTrace probes --------------------------------------------------------------
            470 => {
                self.render.shading_mode = ShadingMode::PathTrace;
                if std::env::var_os("AZRA_PT_REGRESSION").is_some() {
                    self.render.pt_stop_mode = 0;
                    self.render.pt_fast_mode = false;
                    self.render.pt_show_spp = true;
                }
                self.mark_dirty(DirtyFlags::RENDER);
                eprintln!("[VISDIFF] -> PathTrace");
            }
            700 => shot(self, "C_pt_scale20"),
            710 => {
                if std::env::var_os("AZRA_PT_REGRESSION").is_some() {
                    self.viewport.camera.orbit = glam::Quat::from_rotation_y(0.3) * self.viewport.camera.orbit;
                }
                self.visdiff_set_scale(4.0);
                eprintln!("[VISDIFF] scale -> 4");
            }
            720 => shot(self, "D1_pt_scale4_live"),
            950 => shot(self, "D2_pt_scale4_settled"),
            1000 => {
                eprintln!("[VISDIFF] complete — exiting");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    /// AZRA_MATCAP_DIAG=1 — owner repro 2026-08-07: "PBR progressive still renders in
    /// MatCap: MatCap was fast, after visiting PBR-progressive and coming back it is slow."
    /// Self-driving: box → Pbr + progressive refine warms up → switch to MatCap → then
    /// PASSIVELY measure the app's own idle cadence. The tick requests only a 1 Hz
    /// heartbeat after the switch, so anything meaningfully above ~1 fps in the verdict
    /// means the app itself is spinning the loop in MatCap. Each post-switch tick names
    /// the suspects (dirty bits, refine state, RT-sun accel residency).
    pub(crate) fn matcap_diag_tick(&mut self, ctx: &eframe::egui::Context) {
        use std::sync::OnceLock;
        use std::time::Instant;
        static SWITCH_T: OnceLock<Instant> = OnceLock::new();
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        match f {
            20 => {
                self.render.shading_mode = ShadingMode::Pbr;
                // The user's flow: progressive enabled from the settings panel while in
                // Pbr (rt_sun keeps its default = true).
                self.render.pbr_progressive_enabled = true;
                let idx = self.add_primitive_immediate(
                    core_math::primitives::SdfShape::Box {
                        half_extents: glam::Vec3::splat(0.8),
                        bevel: Default::default(),
                    },
                    glam::Vec3::ZERO,
                );
                eprintln!(
                    "[MCDIAG] frame 20: Pbr + progressive ON (rt_sun={}), box idx {idx}",
                    self.render.pbr_refine_rt_sun
                );
            }
            300 => {
                SWITCH_T.set(Instant::now()).ok();
                eprintln!(
                    "[MCDIAG] frame 300: -> MatCap (refine reached N={})",
                    self.render.pbr_refine_frames
                );
                self.render.shading_mode = ShadingMode::MatCap;
                self.mark_dirty(DirtyFlags::RENDER); // one legit redraw for the mode switch
            }
            _ => {}
        }
        if f < 300 {
            ctx.request_repaint(); // drive the setup phase; refine takes over once eligible
            return;
        }
        // Passive phase: 1 Hz heartbeat ONLY — any faster cadence is the app's own doing.
        ctx.request_repaint_after(std::time::Duration::from_millis(1000));
        if f > 300 {
            let accel = self.gpu.as_ref().map(|g| (
                g.pipelines.rt_shadow_pipeline.is_some(),
                g.buffers.pt_tlas_buffer.is_some(),
                g.buffers.pt_blas_buffer.is_some(),
                g.buffers.pt_tri_buffer.is_some(),
                g.buffers.pt_vert_buffer.is_some(),
            ));
            eprintln!(
                "[MCDIAG] f={} t+{:.2}s dirty_bits={:#x} is_dirty={} refine_active={} \
                 refine_frames={} env_q={} first_mark={} accel(rt_pipe,tlas,blas,tri,vert)={:?} \
                 repaint_causes={:?}",
                f,
                SWITCH_T.get().map_or(0.0, |t| t.elapsed().as_secs_f32()),
                self.dirty.bits(),
                self.is_dirty,
                self.render.pbr_refine_active,
                self.render.pbr_refine_frames,
                self.env_prefilter_queue.len(),
                self.perf.first_dirty_caller.as_deref().unwrap_or("-"),
                accel,
                ctx.repaint_causes(),
            );
        }
        // Verdict = frame cadence over a trailing 5s window, so the one-time compile
        // waits (DF-full etc. legitimately pump ~10 fps until they land) don't count
        // against steady state. Early-exit as soon as idle is proven healthy; hard
        // stop at 90s either way.
        use std::sync::Mutex;
        static STAMPS: Mutex<Vec<f32>> = Mutex::new(Vec::new());
        let el = SWITCH_T.get().map_or(0.0, |t| t.elapsed().as_secs_f32());
        let tail_fps = {
            let mut s = STAMPS.lock().unwrap();
            s.push(el);
            s.retain(|&t| t >= el - 5.0);
            s.len() as f32 / 5.0
        };
        let healthy = el >= 10.0 && tail_fps < 2.0;
        if healthy || el >= 90.0 {
            eprintln!(
                "[MCDIAG] VERDICT at t+{el:.1}s: trailing-5s cadence ~{tail_fps:.1} fps \
                 ({} total post-switch frames). ~1 fps = healthy idle; >2 fps = still \
                 spinning. exiting",
                f - 300,
            );
            std::process::exit(0);
        }
    }

    /// AZRA_KNIFEDIAG=free|classic|rect|circle|bevelmod — owner 2026-08-07: "the knife slice always fails to do
    /// a proper cut". Drives the REAL input path (poly_knife_hover → poly_knife_click → S)
    /// on a real created box with the real camera, then verifies the mesh and screenshots
    /// the result. `free` clicks off-mesh left/right of the box (free-plane personality);
    /// `classic` clicks ON the box surface (classic surface knife).
    pub(crate) fn knifediag_tick(&mut self, ctx: &eframe::egui::Context) {
        let mode = match std::env::var("AZRA_KNIFEDIAG") {
            Ok(m) if !m.is_empty() => m,
            _ => return,
        };
        let classic = mode == "classic";
        self.visdiff_frame += 1;
        let f = self.visdiff_frame;
        self.mark_dirty(crate::DirtyFlags::RENDER);
        ctx.request_repaint();
        let rect = self.viewport.viewport_rect;
        let center = rect.center();
        // Click targets: free = off the silhouette both sides; classic = on the box face.
        let dx = if classic { rect.width() * 0.06 } else { rect.width() * 0.35 };
        let a = eframe::egui::pos2(center.x - dx, center.y);
        let b = eframe::egui::pos2(center.x + dx, center.y);
        match f {
            20 => {
                // AZRA_KNIFEDIAG_SHAPE=cylinder → curved multi-facet body (owner ortho
                // screenshot 2026-08-07); AZRA_KNIFEDIAG_ORTHO=1 → orthographic camera.
                let shape = std::env::var("AZRA_KNIFEDIAG_SHAPE")
                    .unwrap_or_else(|_| "box".into());
                self.create_poly_mesh(&shape);
                if std::env::var("AZRA_KNIFEDIAG_ORTHO").is_ok_and(|v| v == "1") {
                    self.render.ortho = true;
                }
                eprintln!(
                    "[KNIFEDIAG] frame 20: {shape} created (mode={mode} ortho={})",
                    self.render.ortho
                );
            }
            60 => {
                self.poly_tool = crate::PolyTool::Knife;
                eprintln!("[KNIFEDIAG] frame 60: knife armed; rect={rect:?}");
            }
            70 | 80 => {
                if mode == "bevelmod" {
                    return; // no knife gesture in bevel-modifier mode
                }
                // RECT knife (owner 2026-08-07 "rectangular knife still failing"):
                // press → drag → release through the REAL input path. Release routes to
                // the cookie-cutter boolean (poly_knife_free_boolean), NOT the slice.
                // CIRCLE knife (owner 2026-08-07 "KNIFE CIRCLE FAIL TO DO, GOT SEGMENT
                // AFTER CUT") uses the SAME press→drag→release path with kind=2; the only
                // difference is that press = CENTRE and the drag vector is the RADIUS
                // (poly_knife_shape_rebuild), so the gesture starts at the viewport centre.
                if mode == "rect" || mode == "circle" {
                    let circle = mode == "circle";
                    // Rect half-size as a FRACTION of viewport width (default 0.035).
                    // Sized deliberately: at the default camera ~0.0153 world units per
                    // pixel, 0.10 spans 2.7 × 2.0 world units — which SWALLOWS a 2-unit
                    // cube, so a "cut removed everything" verdict there measures the
                    // gesture, not the tool. Keep the rect well inside the silhouette.
                    let frac: f32 = std::env::var("AZRA_KNIFEDIAG_RECT")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.035);
                    if f == 70 {
                        self.knife_shape_kind = if circle { 2 } else { 1 };
                        // Segment count of the circle ring. The default (3) is too coarse to
                        // reach the CLOSED-INTERIOR-LOOP branch (`inter.len() >= 4`), so the
                        // harness silently measured the anchored-chord path instead of the
                        // owner's case. 16 matches `circle_knife_oracle.py`.
                        if circle {
                            self.knife_shape_segments = std::env::var("AZRA_KNIFEDIAG_SEGS")
                                .ok()
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(16);
                        }
                        let p0 = if circle {
                            center // circle press = CENTRE
                        } else {
                            eframe::egui::pos2(center.x - rect.width() * frac, center.y - rect.height() * frac)
                        };
                        self.poly_knife_shape_press(p0, rect, false);
                        eprintln!(
                            "[KNIFEDIAG] {mode} press at {p0:?} anchor={:?} kind={}",
                            self.knife_shape_anchor, self.knife_shape_kind
                        );
                    } else {
                        let p1 = if circle {
                            eframe::egui::pos2(center.x + rect.width() * frac, center.y)
                        } else {
                            eframe::egui::pos2(center.x + rect.width() * frac, center.y + rect.height() * frac)
                        };
                        self.poly_knife_shape_drag(p1, rect, false, false, false, false);
                        let (v0, f0) = self.knifediag_counts();
                        eprintln!(
                            "[KNIFEDIAG] {mode} drag to {p1:?} pts={} before v={v0} f={f0}",
                            self.knife_points.len()
                        );
                        self.poly_knife_shape_release();
                        let (v1, f1) = self.knifediag_counts();
                        let mesh = self
                            .sel
                            .usel
                            .active_node()
                            .and_then(|nid| self.poly_node_mesh(nid));
                        let valid = mesh.as_ref().map(|m| m.validate().is_ok());
                        // "GOT SEGMENT AFTER CUT" = the loop landed as a SLIT spur, i.e. a
                        // face ring that revisits vertices and never divided. Census it.
                        let (mut pinched, mut worst) = (0usize, 0usize);
                        if let Some(m) = mesh.as_ref() {
                            for fi in 0..m.faces.len() as u32 {
                                let fid = core_math::half_edge::FaceId(fi);
                                if !m.is_face_live(fid) {
                                    continue;
                                }
                                let vs = m.face_vertices(fid);
                                let uniq: std::collections::HashSet<u32> =
                                    vs.iter().map(|v| v.0).collect();
                                if uniq.len() < vs.len() {
                                    pinched += 1;
                                    worst = worst.max(vs.len());
                                }
                            }
                        }
                        eprintln!(
                            "[KNIFEDIAG] {mode} RELEASE -> v={v1} (+{}) f={f1} (+{}) valid={valid:?} \
                             manifold={:?} pinched_faces={pinched} worst_ring={worst}",
                            v1 as i64 - v0 as i64,
                            f1 as i64 - f0 as i64,
                            mesh.as_ref().map(|m| m.is_manifold().is_ok())
                        );
                        eprintln!(
                            "[KNIFEDIAG] VERDICT({mode}): {}",
                            if v1 > v0 && valid == Some(true) && pinched == 0 {
                                "CUT COMMITTED"
                            } else if pinched > 0 {
                                "FAILED (slit spur / pinched ring — 'segment after cut')"
                            } else {
                                "FAILED"
                            }
                        );
                    }
                    return;
                }
                let m = if f == 70 { a } else { b };
                self.poly_knife_hover(m, rect, false, false);
                eprintln!(
                    "[KNIFEDIAG] frame {f}: click at {m:?} snap_kind={} snap={:?}",
                    self.knife_snap_kind, self.knife_snap_pos
                );
                self.poly_knife_click();
                eprintln!("[KNIFEDIAG]   -> points={}", self.knife_points.len());
            }
            90 => {
                if mode == "bevelmod" || mode == "rect" || mode == "circle" {
                    return; // shape modes already committed on release
                }
                let (v0, f0) = self.knifediag_counts();
                eprintln!("[KNIFEDIAG] frame 90: S (slice through). before v={v0} f={f0}");
                self.poly_knife_slice_through_now();
                let (v1, f1) = self.knifediag_counts();
                let leftover = self.knife_points.len();
                let valid = self
                    .sel
                    .usel
                    .active_node()
                    .and_then(|nid| self.poly_node_mesh(nid))
                    .map(|m| m.validate().is_ok());
                eprintln!(
                    "[KNIFEDIAG] after S: v={v1} (+{}) f={f1} (+{}) valid={valid:?} \
                     leftover_points={leftover} (0 = committed; >0 = SOFT-FAIL kept path)",
                    v1 as i64 - v0 as i64,
                    f1 as i64 - f0 as i64,
                );
                let verdict = if leftover == 0 && v1 > v0 && valid == Some(true) {
                    "CUT COMMITTED"
                } else {
                    "FAILED"
                };
                eprintln!("[KNIFEDIAG] VERDICT({mode}): {verdict}");
            }
            85 => {
                // bevelmod mode (owner 2026-08-07 "bevel modifier triangulates the mesh"):
                // instead of a knife cut, push a real Bevel modifier and histogram the
                // face arities of the modifier DISPLAY mesh — the exact mesh the object
                // wire draws (poly_append_display_wire).
                if mode == "bevelmod" {
                    // Show the wire — the whole point of the owner's report is what the
                    // WIRE looks like over a modifier result.
                    self.render.wire_overlay_enabled = true;
                    if let Some(nid) = self.sel.usel.active_node() {
                        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                            if let core_math::scene_graph::NodePayload::PolygonalBody {
                                modifiers, ..
                            } = &mut node.payload
                            {
                                let mut m = core_math::scene_graph::PolyModifier::new(
                                    core_math::scene_graph::PolyModifierType::Bevel,
                                );
                                eprintln!(
                                    "[KNIFEDIAG] bevelmod defaults: width={} segments={} smart={}",
                                    m.bevel_width, m.bevel_segments, m.bevel_smart_select
                                );
                                if m.bevel_width <= 1e-6 {
                                    m.bevel_width = 0.05;
                                }
                                modifiers.push(m);
                            }
                        }
                        self.mark_dirty(crate::DirtyFlags::SCENE | crate::DirtyFlags::RENDER);
                        // Arity histogram of the base mesh vs the modifier display mesh.
                        let hist = |m: &core_math::half_edge::HalfEdgeMesh| {
                            let (mut t, mut q, mut n) = (0usize, 0usize, 0usize);
                            for fi in 0..m.faces.len() as u32 {
                                let f = core_math::half_edge::FaceId(fi);
                                if !m.is_face_live(f) {
                                    continue;
                                }
                                match m.face_vertices(f).len() {
                                    3 => t += 1,
                                    4 => q += 1,
                                    _ => n += 1,
                                }
                            }
                            (t, q, n)
                        };
                        if let Some(base) = self.poly_node_mesh(nid) {
                            let (t, q, n) = hist(&base);
                            eprintln!("[KNIFEDIAG] BASE arities: tris={t} quads={q} ngons={n}");
                        }
                        if let Some(disp) = self.poly_cached_display_mesh(nid) {
                            let (t, q, n) = hist(&disp);
                            eprintln!("[KNIFEDIAG] BEVELED display arities: tris={t} quads={q} ngons={n}");
                        }
                    }
                }
            }
            92 => {
                if mode == "bevelmod" {
                    // handled at 85; skip the knife topology dump.
                    return;
                }
                let _ = &mode;
                // Topology dump: every live face ring (rounded positions) — the ground
                // truth for "was that a proper ring cut".
                if let Some(nid) = self.sel.usel.active_node() {
                    if let Some(m) = self.poly_node_mesh(nid) {
                        for fi in 0..m.faces.len() as u32 {
                            let f_id = core_math::half_edge::FaceId(fi);
                            if !m.is_face_live(f_id) {
                                continue;
                            }
                            let ring: Vec<String> = m
                                .face_vertices(f_id)
                                .iter()
                                .map(|&v| {
                                    let p = m.vert(v).position;
                                    format!("({:.2},{:.2},{:.2})", p.x, p.y, p.z)
                                })
                                .collect();
                            eprintln!("[KNIFEDIAG] face {fi}: {}", ring.join(" "));
                        }
                    }
                }
            }
            110 => {
                if self.pending_screenshot.is_none() {
                    let out = std::env::temp_dir().join(format!("knifediag_{mode}.png"));
                    eprintln!("[KNIFEDIAG] shot -> {}", out.display());
                    self.visdiff_shot = Some(out);
                }
            }
            250 => {
                eprintln!("[KNIFEDIAG] done — exiting");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    /// Live vert/face counts of the active poly body (KNIFEDIAG helper).
    fn knifediag_counts(&self) -> (usize, usize) {
        let Some(nid) = self.sel.usel.active_node() else { return (0, 0) };
        let Some(m) = self.poly_node_mesh(nid) else { return (0, 0) };
        (m.vertex_count(), m.face_count())
    }

    /// Scrub the Noise Scale exactly as a knob drag does (same downstream calls as the
    /// node_changed arm in win_node_editor).
    fn visdiff_set_scale(&mut self, scale: f32) {
        use crate::shader_nodes::NodeType;
        let noise_id = self
            .ui
            .node_graph
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::Noise))
            .map(|n| n.id);
        if let Some(nid) = noise_id {
            if let Some(n) = self.ui.node_graph.find_node_mut(nid) {
                n.input_defaults[1].value_f32 = scale;
            }
        }
        let Some(NodeGraphTarget::Material(mid)) = self.ui.node_graph_target else { return };
        self.ui.node_graph.wgsl_cache = None;
        self.commit_material_node_graph_edit(mid, false);
        if self.apply_live_gpu_noise_material(mid) {
            self.schedule_live_material_graph_bake(mid);
        } else {
            self.schedule_live_material_graph_bake(mid);
        }
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Diagnostic: prove live noise / map slot state at shot time (M0 gate).
    fn visdiff_log_material_state(&self, tag: &str) {
        let Some(NodeGraphTarget::Material(mid)) = self.ui.node_graph_target else {
            eprintln!("[VISDIFF] {tag}: no material target");
            return;
        };
        let Some((_nid, m)) = self.scene.scene_graph.material_by_id(mid) else {
            eprintln!("[VISDIFF] {tag}: material {mid:?} missing");
            return;
        };
        let abs = m.albedo_map.absolute;
        // Mesh readiness + presentation path (Resident vs CachedCpu).
        let mut mesh_note = String::from("mesh=?");
        let mut pres_note = String::from("pres=?");
        let nid = self.scene.active_body_nid;
        if let Some(node) = self.scene.scene_graph.get(nid) {
            if let core_math::scene_graph::NodePayload::SdfBody { mesh_cache_id, .. } = &node.payload {
                mesh_note = match mesh_cache_id.and_then(|c| self.cache.mesh_cache.get(c)) {
                    Some(e) => {
                        let idx = e.gpu_mesh.as_ref().map(|g| g.index_count).unwrap_or(0);
                        format!("mesh=yes idx={idx}")
                    }
                    None => "mesh=NONE".into(),
                };
                let pres = self.sdf_mesh_presentation(nid, *mesh_cache_id);
                let slot = self
                    .gpu
                    .as_ref()
                    .and_then(|g| g.sdf_body_mesh.get(&nid));
                pres_note = format!(
                    "pres={pres:?} slot={} pt_inst={}",
                    slot.is_some(),
                    self.cache.pt_instance_count,
                );
            }
        }
        eprintln!(
            "[VISDIFF] {tag}: noise en={} amt={:.2} scale={:.1} oct={} | albedo_map slot={} str={:.2} abs={abs} | albedo=({:.2},{:.2},{:.2}) | shading={:?} | {mesh_note} | {pres_note} | draws={} dirty={}",
            m.noise_pattern.enabled,
            m.noise_pattern.amount,
            m.noise_pattern.scale,
            m.noise_pattern.octaves,
            m.albedo_map.tex_id,
            m.albedo_map.strength,
            m.albedo.x,
            m.albedo.y,
            m.albedo.z,
            self.render.shading_mode,
            self.render.last_instance_draws,
            self.is_dirty,
        );
    }
}
