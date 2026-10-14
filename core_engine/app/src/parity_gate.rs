//! AZRA_PARITY — measured RASTER-vs-PATH-TRACE brightness parity gate
//! (RASTER-PT-BRIGHTNESS-GAP, owner 2026-10-13: "everything still feels dark").
//!
//! Run (release, isolated profile recommended; the v2 shell is REQUIRED — it is what the
//! owner runs — unless AZRA_PARITY_ALLOW_V1=1):
//!   AZRA_UI=v2 AZRA_PARITY=<out_dir> AZRA_SHOT_FULLRES=1 [AZRA_PARITY_HDRI=<file.hdr>] app.exe
//!
//! The app drives itself: one SDF sphere (r 0.8) at the origin, camera placed on the SUN's
//! azimuth 30° above the horizon (so the sphere's centre faces the sun at ~25°), selection
//! cleared, tonemapper = None (Linear), exposure 1, bloom off. For every
//! (lighting condition × material) it captures the raster PBR viewport once per raster
//! A/B variant, then the converged path tracer, and finally writes `parity_report.tsv`
//! with the mean LINEAR luminance (sRGB-decoded) of a centre crop and the raster/PT ratio.
//!
//! Conditions: `sun` (default sun, no HDRI), `hdri` (HDRI only, sun intensity 0),
//! `hdri_sun` (both). The HDRI conditions need AZRA_PARITY_HDRI. Materials: `diel_rough`
//! (albedo 0.5, r 0.8), `metal_smooth` (albedo 0.9, r 0.15), `metal_rough` (albedo 0.9, r 0.7).
//! Raster variants: `base` (app defaults), `nossao`, `nofill`, `noprobe` (auto light +
//! reflection probe off), `refine` (Progressive PBR refine ON — default OFF — which is what
//! lets the auto light/reflection probe actually bake; settles 4x longer). Subset with AZRA_PARITY_CONDITIONS / AZRA_PARITY_MATERIALS /
//! AZRA_PARITY_VARIANTS (comma lists). AZRA_PARITY_SPP (default 256) = PT frames before the
//! PT shot; AZRA_PARITY_SETTLE (default 150) = raster frames per variant.
//!
//! AZRA_PARITY_SCENE=room puts the sphere inside a closed thick-walled room with a ceiling
//! opening (the auto probe must still be served there); default is the lone sphere.
//!
//! Per-term isolation is done with lighting × material, not shader debug knobs:
//! sun+diel = direct diffuse (+ fill), hdri+diel = diffuse IBL, hdri+metal = specular IBL.

use crate::*;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// True while AZRA_PARITY drives the app (human input is dropped, see `harness_owns_input`).
pub(crate) fn parity_active() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("AZRA_PARITY").is_ok_and(|d| !d.is_empty()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cond { Sun, Hdri, HdriSun }
impl Cond {
    fn name(self) -> &'static str {
        match self { Cond::Sun => "sun", Cond::Hdri => "hdri", Cond::HdriSun => "hdri_sun" }
    }
}

#[derive(Clone, Copy, Debug)]
struct Mat { name: &'static str, albedo: f32, rough: f32, metal: f32 }
const MATS: [Mat; 3] = [
    Mat { name: "diel_rough", albedo: 0.5, rough: 0.8, metal: 0.0 },
    Mat { name: "metal_smooth", albedo: 0.9, rough: 0.15, metal: 1.0 },
    Mat { name: "metal_rough", albedo: 0.9, rough: 0.7, metal: 1.0 },
];
const VARIANTS: [&str; 5] = ["base", "nossao", "nofill", "noprobe", "refine"];

/// One capture in the plan.
#[derive(Clone, Debug)]
struct Shot { cond: Cond, mat: Mat, variant: &'static str, pt: bool }
impl Shot {
    fn file(&self) -> String {
        let v = if self.pt { "PT" } else { self.variant };
        format!("{}__{}__{}.png", self.cond.name(), self.mat.name, v)
    }
}

#[derive(Debug)]
enum Phase {
    Boot { frames: u32 },
    LoadingEnv { frames: u32 },
    Configure,
    Settle { frames: u32 },
    /// `t0`: wall clock — a cold PT pipeline compile after a shader edit takes minutes.
    PtWarm { frames: u32, t0: std::time::Instant },
    WaitSave { frames: u32 },
    Done,
}

struct Run {
    out: PathBuf,
    plan: Vec<Shot>,
    idx: usize,
    phase: Phase,
    spp: u32,
    settle: u32,
    /// Defaults captured at boot so each raster variant is applied relative to them.
    def_ssao: bool,
    def_fill: f32,
    def_light_probe: bool,
    def_refl_probe: bool,
    def_progressive: bool,
    mat_ids: Vec<(String, core_math::pbr_material::MaterialId)>,
    /// Prim index of the measured sphere (materials are assigned to it).
    sphere_idx: usize,
    /// AZRA_PARITY_HDRI — requested after boot; LoadingEnv waits for THIS file.
    hdri: Option<PathBuf>,
}

/// `AZRA_PARITY_SCENE=room`: the sphere sits inside a closed thick-walled room with a ceiling
/// opening (auto-probe placement must keep serving the probe there). Default: lone sphere.
fn parity_room_scene() -> bool {
    std::env::var("AZRA_PARITY_SCENE").is_ok_and(|s| s == "room")
}

fn list_env(key: &str) -> Option<Vec<String>> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Mean linear luminance + clipped fraction of a centred square crop (side = frac × height).
fn crop_stats(path: &std::path::Path, frac: f64) -> Option<(f64, f64)> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = img.dimensions();
    let side = ((h as f64) * frac).max(4.0) as u32;
    let (x0, y0) = ((w - side) / 2, (h - side) / 2);
    let (mut sum, mut clip, mut n) = (0.0f64, 0u64, 0u64);
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            let p = img.get_pixel(x, y).0;
            sum += 0.2126 * srgb_to_linear(p[0]) + 0.7152 * srgb_to_linear(p[1]) + 0.0722 * srgb_to_linear(p[2]);
            if p.iter().any(|&c| c >= 254) { clip += 1; }
            n += 1;
        }
    }
    Some((sum / n as f64, clip as f64 / n as f64))
}

impl AzraApp {
    /// Env-gated self-driving parity gate (see module docs). Zero cost when AZRA_PARITY unset.
    pub(crate) fn parity_gate_tick(&mut self) {
        static RUN: OnceLock<Mutex<Option<Run>>> = OnceLock::new();
        let cell = RUN.get_or_init(|| Mutex::new(None));
        let mut guard = cell.lock().expect("parity gate state");
        if guard.is_none() {
            // The owner runs the v2 shell; the gate measures THAT configuration by default.
            // AZRA_UI is read at app construction (fonts) — too early to set here, so fail closed.
            let ui_v2 = std::env::var("AZRA_UI").ok().as_deref() == Some("v2");
            assert!(
                ui_v2 || std::env::var_os("AZRA_PARITY_ALLOW_V1").is_some(),
                "AZRA_PARITY: launch with AZRA_UI=v2 (the owner's shell); AZRA_PARITY_ALLOW_V1=1 to override"
            );
            let out = PathBuf::from(std::env::var("AZRA_PARITY").expect("AZRA_PARITY set"));
            std::fs::create_dir_all(&out).expect("create AZRA_PARITY output dir");
            let hdri = std::env::var_os("AZRA_PARITY_HDRI").map(PathBuf::from);
            let conds: Vec<Cond> = match list_env("AZRA_PARITY_CONDITIONS") {
                Some(l) => l.iter().filter_map(|c| match c.as_str() {
                    "sun" => Some(Cond::Sun), "hdri" => Some(Cond::Hdri),
                    "hdri_sun" => Some(Cond::HdriSun), _ => None,
                }).collect(),
                None if hdri.is_some() => vec![Cond::Sun, Cond::Hdri, Cond::HdriSun],
                None => vec![Cond::Sun],
            };
            assert!(
                hdri.is_some() || conds.iter().all(|c| *c == Cond::Sun),
                "AZRA_PARITY: HDRI conditions need AZRA_PARITY_HDRI"
            );
            let mats: Vec<Mat> = match list_env("AZRA_PARITY_MATERIALS") {
                Some(l) => MATS.iter().copied().filter(|m| l.iter().any(|n| n == m.name)).collect(),
                None => MATS.to_vec(),
            };
            let variants: Vec<&'static str> = match list_env("AZRA_PARITY_VARIANTS") {
                Some(l) => VARIANTS.iter().copied().filter(|v| l.iter().any(|n| n == v)).collect(),
                None => VARIANTS.to_vec(),
            };
            let mut plan = Vec::new();
            for &cond in &conds {
                for &mat in &mats {
                    for &variant in &variants {
                        plan.push(Shot { cond, mat, variant, pt: false });
                    }
                    plan.push(Shot { cond, mat, variant: "PT", pt: true });
                }
            }
            eprintln!("[PARITY] plan: {} shots -> {} (AZRA_UI=v2: {ui_v2})", plan.len(), out.display());
            let num = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
            *guard = Some(Run {
                out, plan, idx: 0, phase: Phase::Boot { frames: 0 },
                spp: num("AZRA_PARITY_SPP", 256), settle: num("AZRA_PARITY_SETTLE", 150),
                def_ssao: self.render.ssao_enabled,
                def_fill: self.render.fill_light_strength,
                def_light_probe: self.render.pbr_refine_light_probe,
                def_refl_probe: self.render.auto_reflection_probe,
                def_progressive: self.render.pbr_progressive_enabled,
                mat_ids: Vec::new(),
                sphere_idx: 0,
                hdri,
            });
        }
        let run = guard.as_mut().expect("parity run");
        // Captures and progressive PT only run on dirty frames (see visdiff_tick).
        self.mark_dirty(DirtyFlags::RENDER);

        match run.phase {
            Phase::Boot { ref mut frames } => {
                *frames += 1;
                if *frames == 20 {
                    run.sphere_idx = self.parity_setup_scene();
                    // Requested HERE, not at plan time: a request made on the first tick
                    // was dropped as stale by a startup env reset (env_load_gen bump) and
                    // the gate then silently measured the default dark gradient HDR
                    // (2026-10-13: every luminance ≈ 0.0015 in raster AND PT).
                    if let Some(p) = run.hdri.clone() { self.request_env_load(p); }
                }
                if *frames >= 40 {
                    run.phase = Phase::LoadingEnv { frames: 0 };
                }
            }
            Phase::LoadingEnv { ref mut frames } => {
                *frames += 1;
                let wants_env = run.plan.iter().any(|s| s.cond != Cond::Sun);
                // The REQUESTED file must be the installed one — any env (e.g. the app's
                // default gradient) is not "ready".
                let env_ready = self.env_hdr_width > 0
                    && self.env_prefilter_queue.is_empty()
                    && self.env_source_path.is_some()
                    && self.env_source_path == run.hdri;
                if !wants_env || env_ready {
                    if wants_env {
                        eprintln!("[PARITY] env ready: {:?} ({}x{})",
                            self.env_source_path, self.env_hdr_width, self.env_hdr_height);
                    }
                    run.phase = Phase::Configure;
                } else if *frames > 3000 {
                    panic!("AZRA_PARITY: HDRI {:?} never installed (installed: {:?})",
                        run.hdri, self.env_source_path);
                }
            }
            Phase::Configure => {
                let shot = run.plan[run.idx].clone();
                self.parity_configure(run, &shot);
                run.phase = if shot.pt { Phase::PtWarm { frames: 0, t0: std::time::Instant::now() } } else { Phase::Settle { frames: 0 } };
            }
            Phase::Settle { ref mut frames } => {
                *frames += 1;
                let first = run.idx == 0;
                let refine = run.plan[run.idx].variant == "refine";
                let base_need = if refine { run.settle * 4 } else { run.settle };
                let need = if first { base_need.max(400) } else { base_need };
                if *frames >= need && self.pending_screenshot.is_none() && self.visdiff_shot.is_none() {
                    let shot = run.plan[run.idx].clone();
                    self.parity_capture(run, &shot);
                    run.phase = Phase::WaitSave { frames: 0 };
                }
            }
            Phase::PtWarm { ref mut frames, t0 } => {
                *frames += 1;
                let ready = self.pt_pipeline_ready();
                if ready && *frames > 8 && self.render.pt_accumulated_frames >= run.spp
                    && self.pending_screenshot.is_none() && self.visdiff_shot.is_none()
                {
                    let shot = run.plan[run.idx].clone();
                    self.parity_capture(run, &shot);
                    run.phase = Phase::WaitSave { frames: 0 };
                } else if t0.elapsed().as_secs() > 1800 {
                    panic!("AZRA_PARITY: PT never reached {} frames (ready={ready})", run.spp);
                }
            }
            Phase::WaitSave { ref mut frames } => {
                *frames += 1;
                if self.pending_screenshot.is_none() && self.visdiff_shot.is_none() && *frames > 4 {
                    run.idx += 1;
                    run.phase = if run.idx >= run.plan.len() { Phase::Done } else { Phase::Configure };
                }
            }
            Phase::Done => {
                // PNG encode runs on a worker thread — wait for every file before measuring.
                let all = run.plan.iter().all(|s| run.out.join(s.file()).is_file());
                static WAIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let waited = WAIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if all || waited > 600 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    parity_write_report(run);
                    eprintln!("[PARITY] complete — exiting");
                    std::process::exit(0);
                }
            }
        }
    }

    /// Builds the gate scene; returns the measured sphere's prim index.
    fn parity_setup_scene(&mut self) -> usize {
        self.render.shading_mode = ShadingMode::Pbr;
        // None (Linear) tonemapper, exposure 1: the PNG is then linear radiance → sRGB OETF,
        // so decoding sRGB recovers the HDR value (below 1.0; clip fraction is reported).
        self.render.tonemapper = 5;
        self.render.exposure = 1.0;
        self.render.bloom_enabled = false;
        self.render.dither_enabled = false;
        use core_math::primitives::{BooleanOp, SdfShape};
        let room = parity_room_scene();
        let (sphere_r, sphere_c) = if room { (0.6, glam::Vec3::new(1.2, 0.8, 0.9)) } else { (0.8, glam::Vec3::ZERO) };
        // `add_primitive_with_op` takes the position RELATIVE to the active body's origin
        // (creation.rs: world = body.origin + pos) and the origin is re-centred after every
        // add — convert each intended WORLD position at its own add.
        let add_world = |app: &mut AzraApp, shape: SdfShape, world: glam::Vec3, op: BooleanOp| -> usize {
            let origin = app
                .scene
                .scene_graph
                .body_world_transform(app.scene.active_body_nid)
                .unwrap_or_default()
                .origin;
            app.add_primitive_with_op(shape, world - origin, op)
        };
        if room {
            // Closed thick-walled room (outer box − inner box) with a ceiling opening for the
            // sky/sun. The sphere is added AFTER the subtractions (the boolean chain is ordered:
            // a sphere added first is carved away by the inner box) and rests on the floor OFF
            // the room centre, so the auto probe's AABB-centre eye sits in the room's AIR — the
            // case the placement test must keep serving.
            let bx = |h: glam::Vec3| SdfShape::Box { half_extents: h, bevel: Default::default() };
            let c = glam::Vec3::new(0.0, 2.0, 0.0);
            add_world(self, bx(glam::Vec3::new(3.0, 2.0, 3.0)), c, BooleanOp::Union { k: 0.0 });
            add_world(self, bx(glam::Vec3::new(2.8, 1.8, 2.8)), c, BooleanOp::Subtract { k: 0.0 });
            add_world(
                self,
                bx(glam::Vec3::new(1.0, 0.4, 1.0)),
                glam::Vec3::new(0.6, 3.9, 0.6),
                BooleanOp::Subtract { k: 0.0 },
            );
            // The sphere gets its OWN body: a body renders with its prevailing material, so a
            // shared body would turn the walls into the measured material (hall of mirrors).
            self.create_new_sdf_body();
        }
        // The measured surface; parity_configure assigns materials to THIS prim.
        let idx = add_world(self, SdfShape::Sphere { radius: sphere_r }, sphere_c, BooleanOp::Union { k: 0.0 });
        self.clear_selection_unified();
        // Sun at its default authored direction (az 45°, el 55°). Sphere scene: camera on the
        // sun's azimuth, 30° up. Room scene: camera inside the room looking at the sphere from
        // the room centre side (az −45°, 20° up). eye = target + orbit·(0,0,d) =
        // (sin az·cos el, sin el, −cos az·cos el) — LightData::direction()'s parametrisation
        // ⇒ yaw = π − az, pitch = −el.
        let (az, el, dist) = if room { (-45.0f32, 20.0f32, 2.0) } else { (45.0f32, 30.0f32, 3.0) };
        self.viewport.camera.orbit = glam::Quat::from_euler(
            glam::EulerRot::YXZ,
            std::f32::consts::PI - az.to_radians(),
            -el.to_radians(),
            0.0,
        );
        self.viewport.camera.distance = dist;
        self.viewport.camera.target = sphere_c;
        let lights: Vec<(f32, f32, f32)> = self.scene.scene_graph.lights()
            .map(|(_, l)| (l.azimuth, l.elevation, l.intensity)).collect();
        eprintln!("[PARITY] sphere prim idx {idx} (room={room}); lights (az, el, intensity) = {lights:?}");
        self.mark_dirty(DirtyFlags::RENDER);
        idx
    }

    fn parity_configure(&mut self, run: &mut Run, shot: &Shot) {
        // Lighting condition.
        let sun_on = shot.cond != Cond::Hdri;
        self.render.env_enabled = shot.cond != Cond::Sun;
        for (_, l) in self.scene.scene_graph.lights_mut() {
            if matches!(l.light_type, core_math::scene_graph::LightType::Directional) {
                // The default sun (irradiance π). Ratios are intensity-independent; using the
                // default keeps the absolute numbers equal to what a new scene shows.
                l.intensity = if sun_on { core_math::scene_graph::DEFAULT_SUN_IRRADIANCE } else { 0.0 };
                l.azimuth = 45.0;
                l.elevation = 55.0;
            }
        }
        self.mark_dirty(DirtyFlags::LIGHT);
        // Material: ONE graph material per material case, created on first use and RE-ASSIGNED
        // to the prim through a transaction on every later group. Deliberately reuses existing
        // ids: re-assigning an existing material is the path RASTER-MATERIAL-REASSIGN-STALE
        // broke (raster kept the previous material, PT showed the new one — fixed 2026-10-13
        // via SceneGraph::note_prim_material_write). Cross-condition raster PNGs of the same
        // material must differ by lighting only; across materials they must differ.
        let key = shot.mat.name.to_string();
        let mid = match run.mat_ids.iter().find(|(n, _)| *n == key) {
            Some((_, m)) => *m,
            None => {
                let mut m = core_math::pbr_material::PbrMaterial::default();
                m.albedo = glam::Vec3::splat(shot.mat.albedo);
                m.roughness = shot.mat.rough;
                m.metallic = shot.mat.metal;
                let mid = self.scene.scene_graph.add_material(&key, m);
                run.mat_ids.push((key.clone(), mid));
                mid
            }
        };
        let si = run.sphere_idx;
        let cur = self.scene.primitives.get(si).map(|p| p.material_id);
        if cur != Some(mid) {
            // Same sequence as the canonical assign path (material_assign.rs
            // create_shared_material_from_prim).
            self.transaction("parity material", |s| {
                s.write_prim(si, |p| p.material_id = mid);
                s.cache.prim_buf_stale = true;
                s.mark_dirty(DirtyFlags::RENDER);
            });
        }
        // Raster A/B variant (relative to the app defaults captured at boot).
        self.render.ssao_enabled = run.def_ssao && shot.variant != "nossao";
        self.render.fill_light_strength = if shot.variant == "nofill" { 0.0 } else { run.def_fill };
        let probes = shot.variant != "noprobe";
        self.render.pbr_refine_light_probe = run.def_light_probe && probes;
        self.render.auto_reflection_probe = run.def_refl_probe && probes;
        self.render.pbr_progressive_enabled = run.def_progressive || shot.variant == "refine";
        self.render.shading_mode = if shot.pt { ShadingMode::PathTrace } else { ShadingMode::Pbr };
        if shot.pt {
            self.render.pt_stop_mode = 0;
            self.render.pt_fast_mode = false;
            self.render.pt_accumulated_frames = 0;
        }
        self.mark_dirty(DirtyFlags::RENDER);
        eprintln!("[PARITY] configure {} (sun={sun_on} env={} ssao={} fill={} probes={})",
            shot.file(), self.render.env_enabled, self.render.ssao_enabled,
            self.render.fill_light_strength, probes);
    }

    fn parity_capture(&mut self, run: &Run, shot: &Shot) {
        let p = run.out.join(shot.file());
        eprintln!(
            "[PARITY] shot {} pt_frames={} refine_frames={} spp/frame={} bounces={} shading={:?} env_width_cpu={} probe_ready={}",
            shot.file(), self.render.pt_accumulated_frames, self.render.pbr_refine_frames,
            self.render.pt_samples_per_frame,
            self.render.pt_max_bounces, self.render.shading_mode, self.env_hdr_width,
            self.render.pbr_probe_ready,
        );
        self.visdiff_shot = Some(p);
    }
}

fn parity_write_report(run: &Run) {
    use std::fmt::Write as _;
    let mut tsv = String::from(
        "condition\tmaterial\tvariant\tlum_centre\tlum_wide\tclip_centre\tpt_lum_centre\tratio_centre\tratio_wide\n",
    );
    for s in run.plan.iter().filter(|s| !s.pt) {
        let pt = Shot { variant: "PT", pt: true, ..s.clone() };
        let r = run.out.join(s.file());
        let t = run.out.join(pt.file());
        let (rc, rclip) = crop_stats(&r, 0.04).unwrap_or((f64::NAN, f64::NAN));
        let (rw, _) = crop_stats(&r, 0.15).unwrap_or((f64::NAN, f64::NAN));
        let (pc, _) = crop_stats(&t, 0.04).unwrap_or((f64::NAN, f64::NAN));
        let (pw, _) = crop_stats(&t, 0.15).unwrap_or((f64::NAN, f64::NAN));
        let _ = writeln!(
            tsv, "{}\t{}\t{}\t{rc:.5}\t{rw:.5}\t{rclip:.3}\t{pc:.5}\t{:.3}\t{:.3}",
            s.cond.name(), s.mat.name, s.variant, rc / pc, rw / pw,
        );
        eprintln!(
            "[PARITY] {:<9} {:<13} {:<8} raster={rc:.4} pt={pc:.4} ratio={:.3} (wide {:.3}) clip={rclip:.3}",
            s.cond.name(), s.mat.name, s.variant, rc / pc, rw / pw,
        );
    }
    let path = run.out.join("parity_report.tsv");
    std::fs::write(&path, tsv).expect("write parity_report.tsv");
    eprintln!("[PARITY] report -> {}", path.display());
}
