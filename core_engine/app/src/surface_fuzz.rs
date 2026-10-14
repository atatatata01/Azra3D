// core_engine/app/src/surface_fuzz.rs
//
// SPIKE-HUNTING FUZZ HARNESS for the surface sculpt pipeline (2026-07-23, data-first mandate).
//
// Drives the REAL production path headlessly on the REAL `FourthLayerApp`
// (`FourthLayerApp::new_headless()` — the production constructor with `gpu: None`, exactly the
// state the app runs in before pipeline warmup):
//   • object creation      → `voxel_create_with_res` (the production `voxel_create` body)
//   • CPU chunk meshing    → `voxel_flush_dirty_chunks` (the production CPU mesher route —
//                            with `gpu: None` the GPU-mesher gate is closed, same as prod fallback)
//   • surface-mode entry   → `voxel_surface_set_mode(true)` (the ONE production toggle)
//   • dab application      → `voxel_surface_apply_dabs` / `voxel_surface_apply_snake_hook` /
//                            `voxel_surface_commit_move_grab` (the real batch pipeline: candidate
//                            chunks, displaced-AABB registry, weld, choke-point writes, snapshots)
//   • area frames / follow → `voxel_surface_area_frames` (the real live-mesh fit)
//   • stroke lifecycle     → `SculptState::surface_end_stroke` / `voxel_surface_commit_stroke`
//   • undo / redo          → `FourthLayerApp::undo()` / `redo()` (the real order-stream router)
//   • revoxelize barrier   → `voxel_surface_set_mode(false)` + blocking `voxel_land_move_bake(true)`
//                            + `voxel_flush_dirty_chunks` drain, then re-entry (the real remesh
//                            swap + registry/undo/snapshot voiding)
//
// MIRRORED (not called) — the egui-input-entangled DAB-CONSTRUCTION layer of
// `input_sculpt.rs::handle_voxel_sculpt_input`, faithfully replicated with production defaults
// and the exact line ranges documented at each site:
//   • stroke-start resets                 mirrors input_sculpt.rs:3895–3958
//   • hit acquisition                     mirrors input_sculpt.rs:4002–4060 (the g-buffer pick
//     `v.last_surface_hit` is replaced by a triangle raycast of the LIVE chunk meshes — the same
//     geometry the g-buffer renders; this is the ONE substituted data source, documented)
//   • interpolator + PER-DAB SURFACE FOLLOW  mirrors input_sculpt.rs:4431–4534
//   • per-dab direction resolve           mirrors input_sculpt.rs:4552–4596 (BrushDirection
//     ::Surface default → per-dab followed live-mesh normal, via the real core_math `resolve`)
//   • symmetry expansion                  the real `input_sculpt::expand_voxel_symmetry`
//   • surface dispatch (SurfaceDab build) mirrors input_sculpt.rs:4846–4866
//   • Snake Hook anchor/drag              mirrors input_sculpt.rs:4261–4303
//   • Move mesh-grab begin/drag/release   mirrors input_sculpt.rs:4208–4258 + 549–568
// Production defaults assumed (all are the shipped defaults): lazy 0, brush dynamics 0,
// pressure off, front_faces_only off, BrushDirection::Surface, dir_smooth_normal 0.
// NOT covered headlessly (see the coverage statement in SURFACE_PRIMARY_PLAN.md): the GPU
// resident-pool display layer (covered by `pool_consistency_gpu`), the sculpt raymarch, real
// egui input timing/pressure, and the g-buffer pick readback latency.
//
// SPIKE DETECTOR after every frame/op (surface_validate.rs shares the core):
//   (a) isolated peak  — |vert − 1-ring centroid| > K × median edge (K=8, shared detector)
//   (b) copy divergence — halo-duplicated border verts (grouped at topology time by bit-identical
//       position) must stay BIT-IDENTICAL across chunks after every op
//   (c) NaN/Inf anywhere
//   (d) travel cap — no vertex moves further in one frame than the op's analytic bound (×2 margin)
//
// Failures report (seed, op index, frame, finding) and auto-shrink to a minimal op sequence.

#![cfg(test)]

use crate::input_sculpt::{choose_voxel_surface_hit as choose_hit, expand_voxel_symmetry, VoxelCreateShape};
use crate::voxel_surface_sculpt::{SurfaceDab, SURFACE_AREA_NORMAL_FACTOR};
use crate::{FourthLayerApp, surface_validate};
use core_math::sculpt_layer::StrokeSample;
use core_math::voxel_sculpt::{BrushDirContext, VoxelBrush};
use glam::Vec3;
use std::collections::HashMap;

// ============================== deterministic RNG ==============================

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self { Rng(seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1)) }
    fn next(&mut self) -> u64 {
        // SplitMix64
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn f32(&mut self) -> f32 { (self.next() >> 40) as f32 / (1u64 << 24) as f32 }
    fn range(&mut self, lo: f32, hi: f32) -> f32 { lo + (hi - lo) * self.f32() }
    fn usize(&mut self, n: usize) -> usize { (self.next() % n.max(1) as u64) as usize }
    fn bool(&mut self, p: f32) -> bool { self.f32() < p }
    fn unit(&mut self) -> Vec3 {
        loop {
            let v = Vec3::new(self.range(-1.0, 1.0), self.range(-1.0, 1.0), self.range(-1.0, 1.0));
            let l = v.length();
            if l > 1e-3 && l <= 1.0 { return v / l; }
        }
    }
}

// ============================== op plan ==============================

#[derive(Clone, Debug)]
enum Op {
    /// One dab-brush stroke: rays orbit from `dir0` around `arc_axis` by `arc_step` per frame.
    Dabs {
        brush: VoxelBrush, invert: bool, radius: f32, strength: f32, sym: [bool; 3],
        dir0: Vec3, arc_axis: Vec3, arc_step: f32, frames: u32,
        /// Ctrl+Z fired mid-stroke after this frame (pen still down).
        mid_undo: Option<u32>,
        /// Stroke never releases (commit skipped) — the next stroke start must self-heal.
        abort: bool,
    },
    /// Snake Hook stroke: anchor picked along `dir`, cursor dragged toward `drag_to`.
    Hook { dir: Vec3, drag_to: Vec3, radius: f32, frames: u32, pinch: f32, sym: [bool; 3] },
    /// Move (mesh-grab) stroke: anchor along `dir`, dragged toward `drag_to`, committed on release.
    Grab { dir: Vec3, drag_to: Vec3, radius: f32, frames: u32, strength: f32, sym: [bool; 3] },
    Undo,
    Redo,
    /// Surface→Voxel revoxelize (blocking land) → Surface re-entry. The discard barrier.
    Remesh,
}

const DAB_BRUSHES: &[VoxelBrush] = &[
    VoxelBrush::Draw, VoxelBrush::Erase, VoxelBrush::Smooth, VoxelBrush::Flatten,
    VoxelBrush::Inflate, VoxelBrush::Pinch, VoxelBrush::Clay, VoxelBrush::ClayBuildup,
    VoxelBrush::Scrape, VoxelBrush::Crease,
];

fn plan_from_seed(seed: u64) -> Vec<Op> {
    let mut r = Rng::new(seed);
    let n_ops = 4 + r.usize(7);
    let mut plan = Vec::with_capacity(n_ops);
    for _ in 0..n_ops {
        let roll = r.f32();
        plan.push(if roll < 0.55 {
            let frames = 3 + r.usize(8) as u32;
            Op::Dabs {
                brush: DAB_BRUSHES[r.usize(DAB_BRUSHES.len())],
                invert: r.bool(0.4),
                radius: r.range(0.05, 0.35),
                // Full REACHABLE strength range: the UI slider caps at 1.0 (ui_panels.rs:770) —
                // fuzzing beyond it would tune detectors against a regime no user can reach.
                strength: r.range(0.1, 1.0),
                sym: [r.bool(0.25), r.bool(0.1), r.bool(0.1)],
                dir0: r.unit(),
                arc_axis: r.unit(),
                arc_step: r.range(0.02, 0.25),
                frames,
                mid_undo: if r.bool(0.15) { Some(1 + r.usize(frames.max(2) as usize - 1) as u32) } else { None },
                abort: r.bool(0.12),
            }
        } else if roll < 0.67 {
            let dir = r.unit();
            Op::Hook {
                dir,
                drag_to: dir * r.range(0.9, 1.6) + r.unit() * r.range(0.1, 0.6),
                radius: r.range(0.1, 0.35),
                frames: 3 + r.usize(6) as u32,
                pinch: r.range(0.0, 1.0),
                sym: [r.bool(0.2), false, false],
            }
        } else if roll < 0.75 {
            let dir = r.unit();
            Op::Grab {
                dir,
                drag_to: dir + r.unit() * r.range(0.05, 0.4),
                radius: r.range(0.1, 0.4),
                frames: 2 + r.usize(5) as u32,
                strength: r.range(0.3, 1.0),
                sym: [r.bool(0.2), false, false],
            }
        } else if roll < 0.85 {
            Op::Undo
        } else if roll < 0.90 {
            Op::Redo
        } else {
            Op::Remesh
        });
    }
    plan
}

// ============================== detectors ==============================

#[derive(Debug)]
struct Failure {
    op_idx: usize,
    frame: u32,
    what: String,
}

/// Halo-duplicate registry: verts grouped by BIT-IDENTICAL position at topology time.
/// After any op, every group must still be bit-identical (a diverged copy IS the needle class).
struct CopyMap {
    groups: Vec<Vec<(u32, u32)>>, // (chunk index, vert index), len ≥ 2
}

impl CopyMap {
    fn build(app: &FourthLayerApp) -> Self {
        let mut by_pos: HashMap<[u32; 3], Vec<(u32, u32)>> = HashMap::new();
        if let Some(v) = app.sculpt.voxel.as_ref() {
            for (ci, (&cid, &tris)) in v.chunks.iter().zip(v.chunk_tris.iter()).enumerate() {
                if tris == 0 { continue; }
                if let Some((verts, _)) = app.cache.mesh_cache.chunk_mesh_data(cid) {
                    for (vi, v8) in verts.iter().enumerate() {
                        by_pos.entry([v8[0].to_bits(), v8[1].to_bits(), v8[2].to_bits()])
                            .or_default().push((ci as u32, vi as u32));
                    }
                }
            }
        }
        // Only cross-chunk duplicates matter (same-chunk coincident verts are mesher artifacts,
        // not the halo-copy invariant).
        let groups = by_pos.into_values()
            .filter(|g| g.len() >= 2 && g.iter().any(|&(c, _)| c != g[0].0))
            .collect();
        CopyMap { groups }
    }

    fn verify(&self, app: &FourthLayerApp) -> Result<(), String> {
        let Some(v) = app.sculpt.voxel.as_ref() else { return Ok(()) };
        for g in &self.groups {
            let mut first: Option<([u32; 3], (u32, u32))> = None;
            for &(ci, vi) in g {
                let Some(&cid) = v.chunks.get(ci as usize) else { continue };
                let Some((verts, _)) = app.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
                let Some(v8) = verts.get(vi as usize) else { continue };
                let key = [v8[0].to_bits(), v8[1].to_bits(), v8[2].to_bits()];
                match first {
                    None => first = Some((key, (ci, vi))),
                    Some((k0, (c0, i0))) => {
                        if key != k0 {
                            return Err(format!(
                                "copy divergence: chunk {c0} vert {i0} vs chunk {ci} vert {vi}: \
                                 ({},{},{}) vs ({},{},{})",
                                f32::from_bits(k0[0]), f32::from_bits(k0[1]), f32::from_bits(k0[2]),
                                v8[0], v8[1], v8[2]));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Snapshot of all live chunk vert positions (for the per-frame travel-cap check).
fn snapshot_positions(app: &FourthLayerApp) -> Vec<(u32, Vec<Vec3>)> {
    let mut out = Vec::new();
    if let Some(v) = app.sculpt.voxel.as_ref() {
        for (ci, (&cid, &tris)) in v.chunks.iter().zip(v.chunk_tris.iter()).enumerate() {
            if tris == 0 { continue; }
            if let Some((verts, _)) = app.cache.mesh_cache.chunk_mesh_data(cid) {
                out.push((ci as u32, verts.iter().map(|v8| Vec3::new(v8[0], v8[1], v8[2])).collect()));
            }
        }
    }
    out
}

fn max_travel(app: &FourthLayerApp, before: &[(u32, Vec<Vec3>)]) -> f32 {
    let Some(v) = app.sculpt.voxel.as_ref() else { return 0.0 };
    let mut worst = 0.0f32;
    for (ci, prev) in before {
        let Some(&cid) = v.chunks.get(*ci as usize) else { continue };
        let Some((verts, _)) = app.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
        if verts.len() != prev.len() { continue; } // topology changed (barrier) — not a travel case
        for (v8, p0) in verts.iter().zip(prev.iter()) {
            let d = (Vec3::new(v8[0], v8[1], v8[2]) - *p0).length();
            if d > worst { worst = d; }
        }
    }
    worst
}

/// Detector suite: (a) isolated peak, (b) copy divergence, (c) NaN/Inf. Returns the worst
/// spike ratio for calibration stats.
fn check_all(app: &FourthLayerApp, copies: &CopyMap, k: f32) -> Result<f32, String> {
    let (findings, worst) = surface_validate::scan_surface_spikes(app, k);
    if let Some(f) = findings.first() {
        return Err(format!(
            "isolated peak: chunk {} vert {} ratio {:.2} at ({:.4},{:.4},{:.4}) ring_n {} ({} findings)",
            f.chunk, f.vert, f.ratio, f.pos.x, f.pos.y, f.pos.z, f.ring_n, findings.len()));
    }
    copies.verify(app)?;
    Ok(worst)
}

// ============================== the rig ==============================

struct Rig {
    app: FourthLayerApp,
    copies: CopyMap,
    worst_ratio: f32,
    worst_travel_frac: f32, // observed travel / analytic bound (calibration)
    /// (brush, per-dab (radius, strength)) of the last applied batch — feeds the travel-cap bound.
    last_dab_batch: Option<(VoxelBrush, Vec<(f32, f32)>)>,
}

impl Rig {
    /// Fresh 96³ sphere in Surface mode via the production path.
    fn new() -> Self {
        let mut app = FourthLayerApp::new_headless();
        app.voxel_create_with_res(VoxelCreateShape::Sphere, 96);
        // Production drains dirty chunks inside voxel_surface_set_mode(true) (guarded loops).
        app.voxel_surface_set_mode(true);
        assert!(app.sculpt.voxel.as_ref().map_or(false, |v| v.surface_mode),
            "surface-mode entry failed");
        let tris: u32 = app.sculpt.voxel.as_ref().map(|v| v.chunk_tris.iter().sum()).unwrap_or(0);
        assert!(tris > 0, "sphere meshed no triangles headlessly");
        let copies = CopyMap::build(&app);
        assert!(!copies.groups.is_empty(), "expected halo-duplicated border verts on a chunked sphere");
        Rig { app, copies, worst_ratio: 0.0, worst_travel_frac: 0.0, last_dab_batch: None }
    }

    /// Triangle raycast over the LIVE chunk meshes — the headless stand-in for the g-buffer pick
    /// (`v.last_surface_hit`), which renders exactly these triangles. Nearest hit + interpolated
    /// vertex normal (what the g-buffer normal target holds).
    fn mesh_raycast(&self, ro: Vec3, rd: Vec3) -> Option<(Vec3, Vec3)> {
        let v = self.app.sculpt.voxel.as_ref()?;
        let mut best: Option<(f32, Vec3, Vec3)> = None;
        for (&cid, &tris) in v.chunks.iter().zip(v.chunk_tris.iter()) {
            if tris == 0 { continue; }
            let Some((verts, idx)) = self.app.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            for t in idx.chunks_exact(3) {
                let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
                if ia >= verts.len() || ib >= verts.len() || ic >= verts.len() { continue; }
                let a = Vec3::new(verts[ia][0], verts[ia][1], verts[ia][2]);
                let b = Vec3::new(verts[ib][0], verts[ib][1], verts[ib][2]);
                let c = Vec3::new(verts[ic][0], verts[ic][1], verts[ic][2]);
                // Möller–Trumbore
                let e1 = b - a;
                let e2 = c - a;
                let p = rd.cross(e2);
                let det = e1.dot(p);
                if det.abs() < 1e-12 { continue; }
                let inv = 1.0 / det;
                let s = ro - a;
                let u = s.dot(p) * inv;
                if !(0.0..=1.0).contains(&u) { continue; }
                let q = s.cross(e1);
                let w = rd.dot(q) * inv;
                if w < 0.0 || u + w > 1.0 { continue; }
                let tt = e2.dot(q) * inv;
                if tt <= 1e-4 { continue; }
                if best.map_or(true, |(bt, _, _)| tt < bt) {
                    let na = Vec3::new(verts[ia][3], verts[ia][4], verts[ia][5]);
                    let nb = Vec3::new(verts[ib][3], verts[ib][4], verts[ib][5]);
                    let nc = Vec3::new(verts[ic][3], verts[ic][4], verts[ic][5]);
                    let n = (na * (1.0 - u - w) + nb * u + nc * w).normalize_or_zero();
                    best = Some((tt, ro + rd * tt, n));
                }
            }
        }
        best.map(|(_, h, n)| (h, n))
    }

    /// Hit acquisition — mirrors input_sculpt.rs:4002–4060 exactly, with the g-buffer pick
    /// replaced by `mesh_raycast` (documented substitution).
    fn acquire_hit(&self, ro: Vec3, rd: Vec3, dragging: bool) -> Option<(Vec3, Vec3)> {
        let v = self.app.sculpt.voxel.as_ref()?;
        let prev = v.stroke_last_surface;
        let max_jump = (v.radius * 3.0).max(v.grid.voxel_size() * 12.0);
        let cpu = v.grid.raycast(ro, rd).map(|h| {
            let n = v.grid.normal_at(h);
            (h, if n.length_squared() > 1e-12 { n.normalize_or_zero() } else { Vec3::Y })
        });
        let gpu = self.mesh_raycast(ro, rd);
        let max_gpu_lateral = (v.radius * 1.25).max(v.grid.voxel_size() * 4.0);
        let density_stale = v.cpu_grid_stale || v.surface_dirty_region.is_some();
        let mut raw = choose_hit(cpu, gpu, density_stale, ro, rd, max_gpu_lateral);
        if raw.is_none() && dragging {
            if let Some((ph, pn)) = prev {
                let pn = pn.normalize_or_zero();
                if pn.length_squared() > 1e-12 {
                    let denom = rd.dot(pn);
                    if denom.abs() > 1e-5 {
                        let t = (ph - ro).dot(pn) / denom;
                        if t > 0.0 {
                            let p = ro + rd * t;
                            if (p - ph).length() <= max_jump * 2.0 {
                                raw = Some((p, pn));
                            }
                        }
                    }
                }
            }
        }
        raw.map(|(h, n_raw)| {
            let n_d = if v.cpu_grid_stale || v.surface_dirty_region.is_some() {
                Vec3::ZERO
            } else { v.grid.normal_at(h) };
            let n = if n_d.length_squared() > 1e-12 {
                n_d.normalize_or_zero()
            } else if n_raw.length_squared() > 1e-12 {
                n_raw.normalize_or_zero()
            } else {
                Vec3::Y
            };
            (h, n)
        })
    }

    /// Stroke-start resets — mirrors input_sculpt.rs:3895–3958 (surface-relevant subset:
    /// atlas/density latches don't apply, the fields reset here are the ones surface reads).
    fn stroke_start(&mut self, brush: VoxelBrush, radius: f32, strength: f32, sym: [bool; 3]) {
        if self.app.sculpt.surface_end_stroke() {
            // stroke-start self-heal fired (previous stroke aborted) — production logs this too
        }
        let res = self.app.sculpt.voxel.as_ref().map_or(0, |v| v.grid.res);
        if let Some(v) = self.app.sculpt.voxel.as_mut() {
            v.brush = brush;
            v.radius = radius;
            v.strength = strength;
            v.symmetry = sym;
            v.first_point_normal = None;
            v.first_point_pos = None;
            v.average_normal_sum = Vec3::ZERO;
            v.average_normal_count = 0;
            v.stroke_prev_pos = None;
            v.stroke_last_surface = None;
            v.deferred_stamps.clear();
            v.mesh_grab.cancel();
            v.snake_anchor = None;
        }
        let spacing = self.app.sculpt.voxel.as_ref().map_or(0.10, |v| v.stroke_spacing.clamp(0.03, 0.50));
        self.app.sdf_stroke_interpolator.spacing = spacing;
        self.app.sdf_stroke_interpolator.reset();
        self.app.sculpt.voxel_stroke.begin(res);
        self.app.sculpt.stroke_active = true;
    }

    /// Stroke release — mirrors the surface branch input_sculpt.rs:549–568.
    fn stroke_release(&mut self) {
        if self.app.sculpt.voxel.as_ref().map_or(false, |v| v.mesh_grab.is_active()) {
            self.app.voxel_surface_commit_move_grab();
        }
        self.app.sdf_stroke_interpolator.reset();
        self.app.voxel_surface_commit_stroke();
        if let Some(v) = self.app.sculpt.voxel.as_mut() {
            v.stroke_aabb = None;
            v.move_prev_pos = None;
            v.snake_anchor = None;
        }
        self.app.sculpt.stroke_active = false;
    }

    /// One dab-stroke frame — mirrors input_sculpt.rs:4431–4534 (interpolator + per-dab surface
    /// follow), 4552–4596 (per-dab direction resolve, BrushDirection::Surface default), symmetry
    /// (`expand_voxel_symmetry`), and the surface dispatch 4846–4866. Production defaults: lazy 0,
    /// dynamics 0, pressure 1.0, no tip orientation (surface dabs don't consume tip_rotation).
    fn dab_frame(&mut self, ro: Vec3, rd: Vec3, dragging: bool, brush: VoxelBrush, invert: bool) {
        let Some((hit, normal)) = self.acquire_hit(ro, rd, dragging) else { return };
        let view_dir = (-rd).normalize_or_zero(); // input_sculpt.rs:4378
        if let Some(v) = self.app.sculpt.voxel.as_mut() {
            if normal.length_squared() > 1e-12 {
                v.stroke_last_surface = Some((hit, normal)); // :4097–4101
            }
        }
        let (radius, strength) = self.app.sculpt.voxel.as_ref()
            .map(|v| (v.radius, v.strength)).unwrap_or((0.2, 0.35));
        // Direction resolve (:4398–4425); Surface default: resolve() returns the surface normal.
        let stroke_dir = self.app.sculpt.voxel.as_ref().and_then(|v| v.stroke_prev_pos.map(|p| hit - p));
        let brush_dir = self.app.sculpt.voxel.as_ref().map(|v| v.brush_dir)
            .unwrap_or(core_math::voxel_sculpt::BrushDirection::Surface);
        let resolve_dir = |surface: Vec3, stroke: Option<Vec3>| -> Vec3 {
            brush_dir.resolve(BrushDirContext {
                surface, view: view_dir, first_point: None, average: None, stroke_dir: stroke,
            })
        };
        let surface_n = if normal.length_squared() > 1e-12 { normal } else { Vec3::Y };
        let dir = {
            let d = resolve_dir(surface_n, stroke_dir);
            if d.length_squared() > 1e-12 { d } else { surface_n }
        };
        if let Some(v) = self.app.sculpt.voxel.as_mut() { v.stroke_prev_pos = Some(hit); }
        let sample = StrokeSample::new(hit, dir, 1.0);
        let mut pts = self.app.sdf_stroke_interpolator.push(sample, radius);
        // ---- PER-DAB SURFACE FOLLOW (mirrors :4472–4509, surface-mode arm) ----
        if !pts.is_empty() {
            let probes: Vec<(Vec3, f32)> = pts.iter()
                .map(|pt| (pt.position, (radius * pt.radius_scale).max(1e-4)))
                .collect();
            let refs = vec![normal; probes.len()];
            let frames = self.app.voxel_surface_area_frames(&probes, SURFACE_AREA_NORMAL_FACTOR, &refs);
            let vs_local = self.app.sculpt.voxel.as_ref().map_or(1e-3, |v| v.grid.voxel_size());
            let mut last_surf = normal;
            let dab_surf: Vec<Vec3> = (0..pts.len()).map(|i| {
                if let Some((c, n)) = frames.get(i).copied().flatten() {
                    let p = pts[i].position;
                    let d = (p - c).dot(n);
                    let max_snap = (radius * pts[i].radius_scale).max(vs_local);
                    if d.abs() <= max_snap {
                        pts[i].position = p - n * d;
                    }
                    let n = if n.dot(last_surf) < 0.0 { last_surf } else { n };
                    last_surf = n;
                    n
                } else {
                    last_surf
                }
            }).collect();
            // ---- per-dab direction (mirrors :4552–4596; Surface wants_live_surface → dab_surf) ----
            for i in 0..pts.len() {
                let surf = dab_surf[i];
                pts[i].normal = resolve_dir(surf, None);
            }
        }
        if pts.is_empty() { return; }
        // Symmetry AFTER budget (:4826) — the real production expansion.
        let vs = self.app.sculpt.voxel.as_ref().map_or(1.0, |v| v.grid.voxel_size()).max(1e-6);
        let symmetry = self.app.sculpt.voxel.as_ref().map_or([false; 3], |v| v.symmetry);
        expand_voxel_symmetry(&mut pts, symmetry, vs * 0.5);
        // ---- surface dispatch (mirrors :4846–4866) ----
        let (hardness, preset) = self.app.sculpt.voxel.as_ref()
            .map(|v| (v.falloff, v.falloff_preset))
            .unwrap_or((core_math::voxel_sculpt::DEFAULT_BRUSH_HARDNESS,
                        core_math::voxel_sculpt::FalloffPreset::Smooth));
        let base = if matches!(brush, VoxelBrush::Erase) { -1.0f32 } else { 1.0 };
        let sgn = if invert { -base } else { base };
        let surf_stroke_tan = if matches!(brush, VoxelBrush::Pinch | VoxelBrush::Crease) && pts.len() >= 2 {
            (pts[pts.len() - 1].position - pts[0].position).normalize_or_zero()
        } else {
            Vec3::ZERO
        };
        let dabs: Vec<SurfaceDab> = pts.iter().map(|pt| SurfaceDab {
            pos: pt.position,
            dir: pt.normal.normalize_or_zero(),
            radius: (radius * pt.radius_scale).max(1e-4),
            strength: strength * pt.pressure,
            stroke_tan: surf_stroke_tan,
            tip_rotation: pt.tip_rotation,
        }).collect();
        self.last_dab_batch = Some((brush, dabs.iter().map(|d| (d.radius, d.strength)).collect()));
        self.app.voxel_surface_apply_dabs(&dabs, brush, sgn, hardness, preset);
    }

    /// Blocking revoxelize barrier — the production Surface→Voxel→Surface cycle.
    fn remesh_barrier(&mut self) {
        self.app.voxel_surface_set_mode(false);
        // Blocking land (the same voxel_land_move_bake(true) the save/leave sync points use),
        // then drain the remesh queue exactly like the mode-entry loops.
        let mut guard = 0;
        while self.app.sculpt.voxel.as_ref().map_or(false, |v| v.pending_move_bake.is_some()) {
            self.app.voxel_land_move_bake(true);
            guard += 1;
            if guard > 8 { break; }
        }
        let mut guard = 0;
        while self.app.sculpt.voxel.as_ref().map_or(false, |v| !v.dirty_chunks.is_empty()) {
            self.app.voxel_flush_dirty_chunks();
            guard += 1;
            if guard > 512 { break; }
        }
        self.app.voxel_surface_set_mode(true);
        self.copies = CopyMap::build(&self.app);
    }
}

// ============================== driver ==============================

/// Calibration stats: per op-family worst spike ratio (the data that justifies the threshold).
#[derive(Default)]
struct Stats {
    per_family: HashMap<String, f32>,
    worst_ratio: f32,
    worst_travel_frac: f32,
    baseline_ratio: f32, // untouched sphere
}

impl Stats {
    /// Attribute a reading to `family` ONLY when it RAISED this run's worst — the scan is
    /// global, so a persistent sharp feature would otherwise smear its ratio over every later
    /// op family (observed: all families plateaued at one value before this).
    fn note(&mut self, family: &str, w: f32, run_worst: &mut f32) {
        if w > *run_worst {
            *run_worst = w;
            let e = self.per_family.entry(family.to_string()).or_insert(0.0);
            if w > *e { *e = w; }
            if w > self.worst_ratio { self.worst_ratio = w; }
        }
    }
}

fn run_plan(plan: &[Op], k: f32, stats: &mut Stats) -> Result<(), Failure> {
    let mut rig = Rig::new();
    {
        let (f, base) = surface_validate::scan_surface_spikes(&rig.app, k);
        assert!(f.is_empty(), "fresh sphere already has spikes: {f:?}");
        if base > stats.baseline_ratio { stats.baseline_ratio = base; }
    }
    let mut run_worst = 0.0f32;
    exec_plan_on(&mut rig, plan, 0, k, stats, &mut run_worst)
}

/// Execute `plan` on an existing rig (op indices offset by `op_base` in failure reports).
/// Split out of `run_plan` so diagnostics can drive PREFIXES and inspect the rig between ops.
fn exec_plan_on(
    rig: &mut Rig,
    plan: &[Op],
    op_base: usize,
    k: f32,
    stats: &mut Stats,
    run_worst: &mut f32,
) -> Result<(), Failure> {
    let run_worst = &mut *run_worst;
    let mut fail = |op_idx: usize, frame: u32, what: String| Failure { op_idx, frame, what };
    for (op_idx, op) in plan.iter().enumerate() {
        let op_idx = op_base + op_idx;
        match op {
            Op::Dabs { brush, invert, radius, strength, sym, dir0, arc_axis, arc_step, frames, mid_undo, abort } => {
                rig.stroke_start(*brush, *radius, *strength, *sym);
                for f in 0..*frames {
                    // Cursor path = an arc on the view sphere: rotate the pick direction around
                    // `arc_axis` by `arc_step` per frame (crossing strokes arise from op overlap).
                    let rot = glam::Quat::from_axis_angle(arc_axis.normalize_or_zero(), *arc_step * f as f32);
                    let d = (rot * *dir0).normalize_or_zero();
                    let ro = d * 3.0;
                    let rd = -d;
                    let before = snapshot_positions(&rig.app);
                    rig.dab_frame(ro, rd, f > 0, *brush, *invert);
                    // travel cap: analytic per-frame bound × 2 margin
                    if let Some((b, ref batch)) = rig.last_dab_batch {
                        let bound: f32 = batch.iter().map(|&(r, s)| {
                            let s = s.clamp(0.0, 4.0);
                            match b {
                                // BLENDER-EXACT amps since 2026-08-20 (BLENDER_BRUSH_SPEC
                                // session log): Draw offset = s·r (spacing-overlap rides
                                // the producer's strength, which this batch already
                                // carries); Inflate 0.25·s·r (deflate 0.125 ≤ that).
                                VoxelBrush::Draw | VoxelBrush::Erase => s * r,
                                VoxelBrush::Inflate => 0.25 * s * r,
                                VoxelBrush::Crease => s * 0.12 * r + r, // draw + pinch (pinch ≤ dist to axis ≤ r)
                                VoxelBrush::Pinch => r,                  // pull ≤ dist to axis ≤ r
                                VoxelBrush::Clay | VoxelBrush::ClayBuildup => 0.5 * r, // band cap
                                VoxelBrush::Scrape | VoxelBrush::Flatten => r,          // ≤ plane distance ≤ r
                                VoxelBrush::Smooth => 4.0 * 0.05,        // ≤ kernel (2·vs·2)
                                _ => r,
                            }
                        }).sum();
                        let t = max_travel(&rig.app, &before);
                        let frac = t / (2.0 * bound + 1e-6);
                        if frac > stats.worst_travel_frac { stats.worst_travel_frac = frac; }
                        if t > 2.0 * bound + 1e-4 {
                            return Err(fail(op_idx, f, format!(
                                "travel cap: brush {b:?} moved a vert {t:.5} > 2×bound {bound:.5} ({} dabs)",
                                batch.len())));
                        }
                    }
                    match check_all(&rig.app, &rig.copies, k) {
                        Ok(w) => stats.note(&format!("{brush:?}{}", if *invert { "-inv" } else { "" }), w, run_worst),
                        Err(e) => return Err(fail(op_idx, f, e)),
                    }
                    if *mid_undo == Some(f) {
                        rig.app.undo();
                        if let Err(e) = rig.copies.verify(&rig.app) {
                            return Err(fail(op_idx, f, format!("after mid-stroke undo: {e}")));
                        }
                    }
                }
                if !*abort {
                    rig.stroke_release();
                }
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note(&format!("{brush:?}-release"), w, run_worst),
                    Err(e) => return Err(fail(op_idx, u32::MAX, format!("after release: {e}"))),
                }
            }
            Op::Hook { dir, drag_to, radius, frames, pinch, sym } => {
                rig.stroke_start(VoxelBrush::SnakeHook, *radius, 1.0, *sym);
                if let Some(v) = rig.app.sculpt.voxel.as_mut() { v.pinch_amount = *pinch; }
                let cam = *dir * 3.0;
                // First frame latches the anchor at the surface hit (mirrors :4269–4280).
                let rd0 = (-*dir).normalize_or_zero();
                let Some((hit, normal)) = rig.acquire_hit(cam, rd0, false) else {
                    rig.stroke_release();
                    continue;
                };
                let plane_n = (-rd0).normalize_or_zero();
                if let Some(v) = rig.app.sculpt.voxel.as_mut() {
                    v.snake_anchor = Some(hit);
                    v.snake_plane_n = if plane_n.length_squared() > 1e-8 { plane_n } else { Vec3::Z };
                    v.first_point_normal = Some(normal);
                    v.first_point_pos = Some(hit);
                    v.mesh_grab.cancel();
                }
                for f in 1..=*frames {
                    let t = f as f32 / *frames as f32;
                    let target = hit.lerp(*drag_to, t);
                    let rd = (target - cam).normalize_or_zero();
                    // drag = cursor projected onto the anchor plane − anchor (mirrors :4282–4296);
                    // defaults: elasticity 1, pull 1, strength 1, no invert.
                    let (anchor, drag) = {
                        let v = rig.app.sculpt.voxel.as_ref().unwrap();
                        let anchor = v.snake_anchor.unwrap_or(hit);
                        let pn = v.snake_plane_n;
                        let denom = rd.dot(pn);
                        let view_drag = if denom.abs() < 1e-5 { Vec3::ZERO } else {
                            let tt = (anchor - cam).dot(pn) / denom;
                            (cam + rd * tt) - anchor
                        };
                        let el = v.move_elasticity.clamp(0.0, 2.0);
                        let pull = v.move_pull_strength.clamp(0.0, 2.0);
                        let strength_move = v.strength.clamp(0.0, 2.0);
                        (anchor, view_drag * el * pull * strength_move)
                    };
                    let (hardness, preset, symmetry, cp) = {
                        let v = rig.app.sculpt.voxel.as_ref().unwrap();
                        (v.falloff, v.falloff_preset, v.symmetry, v.pinch_amount.clamp(0.0, 1.0))
                    };
                    let before = snapshot_positions(&rig.app);
                    rig.app.voxel_surface_apply_snake_hook(anchor, drag, *radius, cp, hardness, preset, symmetry);
                    let t_moved = max_travel(&rig.app, &before);
                    let bound = drag.length() + *radius + drag.length(); // tube pull + pinch radial
                    if t_moved > 2.0 * bound + 1e-3 {
                        return Err(fail(op_idx, f, format!(
                            "snake hook travel {t_moved:.5} > 2×bound {bound:.5}")));
                    }
                    match check_all(&rig.app, &rig.copies, k) {
                        Ok(w) => stats.note("SnakeHook", w, run_worst),
                        Err(e) => return Err(fail(op_idx, f, e)),
                    }
                }
                rig.stroke_release();
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note("SnakeHook-release", w, run_worst),
                    Err(e) => return Err(fail(op_idx, u32::MAX, format!("after hook release: {e}"))),
                }
            }
            Op::Grab { dir, drag_to, radius, frames, strength, sym } => {
                rig.stroke_start(VoxelBrush::Move, *radius, *strength, *sym);
                let cam = *dir * 3.0;
                let rd0 = (-*dir).normalize_or_zero();
                let Some((hit, normal)) = rig.acquire_hit(cam, rd0, false) else {
                    rig.stroke_release();
                    continue;
                };
                // mesh_grab begin + per-frame drag (mirrors :4210–4248).
                if let Some(v) = rig.app.sculpt.voxel.as_mut() {
                    v.mesh_grab.begin(hit, normal, 0.0, *radius, rd0);
                    v.first_point_normal = Some(normal);
                    v.first_point_pos = Some(hit);
                }
                for f in 1..=*frames {
                    let t = f as f32 / *frames as f32;
                    let target = hit.lerp(*drag_to, t);
                    let rd = (target - cam).normalize_or_zero();
                    let strength_move = (*strength).clamp(0.0, 1.0);
                    let drag = {
                        let v = rig.app.sculpt.voxel.as_ref().unwrap();
                        let plane_n = v.mesh_grab.plane_n;
                        let anchor = v.mesh_grab.anchor;
                        let denom = rd.dot(plane_n);
                        let view_drag = if denom.abs() < 1e-5 { Vec3::ZERO } else {
                            let tt = (anchor - cam).dot(plane_n) / denom;
                            (cam + rd * tt) - anchor
                        };
                        let el = v.move_elasticity.clamp(0.0, 2.0);
                        let pull = v.move_pull_strength.clamp(0.0, 2.0);
                        view_drag * el * pull * strength_move
                    };
                    if let Some(v) = rig.app.sculpt.voxel.as_mut() { v.mesh_grab.last_drag = drag; }
                }
                let before = snapshot_positions(&rig.app);
                rig.stroke_release(); // commits the grab through the production path
                let t_moved = max_travel(&rig.app, &before);
                let bound = rig.app.sculpt.voxel.as_ref()
                    .map(|_| 4.0f32).unwrap_or(4.0); // grab delta ≤ |last_drag| ≤ scene scale; generous
                if t_moved > bound {
                    return Err(fail(op_idx, u32::MAX, format!("move grab travel {t_moved:.5} > {bound}")));
                }
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note("MoveGrab", w, run_worst),
                    Err(e) => return Err(fail(op_idx, u32::MAX, format!("after grab commit: {e}"))),
                }
            }
            Op::Undo => {
                rig.app.undo();
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note("Undo", w, run_worst),
                    Err(e) => return Err(fail(op_idx, 0, format!("after undo: {e}"))),
                }
            }
            Op::Redo => {
                rig.app.redo();
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note("Redo", w, run_worst),
                    Err(e) => return Err(fail(op_idx, 0, format!("after redo: {e}"))),
                }
            }
            Op::Remesh => {
                rig.remesh_barrier();
                match check_all(&rig.app, &rig.copies, k) {
                    Ok(w) => stats.note("Remesh", w, run_worst),
                    Err(e) => return Err(fail(op_idx, 0, format!("after remesh barrier: {e}"))),
                }
            }
        }
    }
    Ok(())
}

/// Greedy shrink: repeatedly drop ops while the plan still fails.
fn shrink(plan: Vec<Op>, k: f32) -> Vec<Op> {
    let mut cur = plan;
    loop {
        let mut reduced = false;
        let mut i = 0;
        while i < cur.len() {
            let mut cand = cur.clone();
            cand.remove(i);
            if !cand.is_empty() && run_plan(&cand, k, &mut Stats::default()).is_err() {
                cur = cand;
                reduced = true;
            } else {
                i += 1;
            }
        }
        if !reduced { return cur; }
    }
}

fn fuzz_seeds(range: std::ops::Range<u64>, k: f32) -> Result<Stats, String> {
    let mut stats = Stats::default();
    for seed in range {
        let plan = plan_from_seed(seed);
        match run_plan(&plan, k, &mut stats) {
            Ok(()) => {}
            Err(f) => {
                let minimal = shrink(plan.clone(), k);
                return Err(format!(
                    "SEED {seed} FAILED at op {} frame {}: {}\nfull plan: {plan:#?}\nMINIMAL plan ({} ops): {minimal:#?}",
                    f.op_idx, f.frame, f.what, minimal.len()));
            }
        }
    }
    Ok(stats)
}

fn print_stats(label: &str, s: &Stats) {
    let mut fams: Vec<_> = s.per_family.iter().collect();
    fams.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    eprintln!("[FUZZ] {label}: baseline sphere ratio {:.2}, worst {:.2}, worst travel frac {:.2}",
        s.baseline_ratio, s.worst_ratio, s.worst_travel_frac);
    for (f, w) in fams {
        eprintln!("[FUZZ]   {f:<22} worst ratio {w:.2}");
    }
}

// ============================== tests ==============================

// ---------------------------------------------------------------------------
// FUZZ-CAUGHT MINIMAL REPROS (campaign 2026-07-23, seeds 0..10200, 4 failures).
// Two classes; every failing plan has X-symmetry ON:
//   A: [stroke] → Remesh → Inflate(sym X)  → copy divergence at FRAME 0 (seeds 563, 3985)
//   B: SnakeHook(sym X) [→ Remesh → Inflate] → isolated peak       (seeds 8965, 5296)
// Kept as permanent named regressions; must FAIL before the root-cause fix, PASS after.
// ---------------------------------------------------------------------------

fn v3(x: f32, y: f32, z: f32) -> Vec3 { Vec3::new(x, y, z) }

fn repro_seed_563() -> Vec<Op> {
    vec![
        Op::Hook { dir: v3(0.41598278, -0.31094804, 0.85455817),
                   drag_to: v3(0.7480178, -0.8058734, 1.0906643),
                   radius: 0.19367841, frames: 8, pinch: 0.773472, sym: [false; 3] },
        Op::Remesh,
        Op::Dabs { brush: VoxelBrush::Inflate, invert: false, radius: 0.08107491,
                   strength: 0.38111097, sym: [true, false, false],
                   dir0: v3(0.73183805, -0.6678843, 0.1354387),
                   arc_axis: v3(-0.10938773, 0.07988875, -0.99078363),
                   arc_step: 0.24510802, frames: 5, mid_undo: None, abort: true },
    ]
}

fn repro_seed_3985() -> Vec<Op> {
    vec![
        Op::Dabs { brush: VoxelBrush::Smooth, invert: true, radius: 0.31532478,
                   strength: 0.66074204, sym: [false; 3],
                   dir0: v3(-0.9902812, -0.047034506, -0.13088462),
                   arc_axis: v3(0.9256945, 0.33105996, 0.18300009),
                   arc_step: 0.0646625, frames: 3, mid_undo: None, abort: false },
        Op::Remesh,
        Op::Dabs { brush: VoxelBrush::Inflate, invert: true, radius: 0.2699062,
                   strength: 1.0220497, sym: [true, false, false],
                   dir0: v3(0.6073265, -0.53875625, 0.58386326),
                   arc_axis: v3(-0.5523996, -0.8032551, 0.22279166),
                   arc_step: 0.08385384, frames: 7, mid_undo: None, abort: false },
    ]
}

fn repro_seed_5296() -> Vec<Op> {
    vec![
        Op::Hook { dir: v3(0.865026, 0.48723307, -0.11972439),
                   drag_to: v3(1.0959318, 0.2108716, -0.40963995),
                   radius: 0.33047715, frames: 8, pinch: 0.14346397, sym: [true, false, false] },
        Op::Remesh,
        Op::Dabs { brush: VoxelBrush::Inflate, invert: false, radius: 0.27748272,
                   strength: 1.1040096, sym: [true, false, false],
                   dir0: v3(0.95924294, -0.0434542, -0.27922162),
                   arc_axis: v3(-0.40023783, -0.88646793, -0.23234506),
                   arc_step: 0.03676149, frames: 7, mid_undo: None, abort: false },
    ]
}

fn repro_seed_8965() -> Vec<Op> {
    vec![
        Op::Hook { dir: v3(0.06446332, 0.6898335, 0.72109246),
                   drag_to: v3(0.5527148, 1.2975999, 0.8728351),
                   radius: 0.10121722, frames: 4, pinch: 0.61350226, sym: [true, false, false] },
    ]
}

/// DIAGNOSTIC (data collection for the seed-563 class): run Hook + Remesh, then dump the
/// soon-to-diverge copy pair's post-remesh state (positions, normals) + whether both chunks
/// are in the Inflate dab batch's candidate list.
#[test]
#[ignore]
fn fuzz_diag_seed_563_post_remesh_state() {
    let plan = repro_seed_563();
    let mut rig = Rig::new();
    // Op 0: Hook, op 1: Remesh — run through the production driver pieces.
    let mut stats = Stats::default();
    // Re-use run_plan for the prefix by building a prefix plan.
    drop(stats);
    let prefix: Vec<Op> = plan[..2].to_vec();
    let mut s2 = Stats::default();
    // run the prefix on THIS rig manually: simplest is to rebuild via run_plan semantics —
    // but run_plan owns its rig, so inline the two ops here.
    match &prefix[0] {
        Op::Hook { dir, drag_to, radius, frames, pinch, sym } => {
            rig.stroke_start(VoxelBrush::SnakeHook, *radius, 1.0, *sym);
            if let Some(v) = rig.app.sculpt.voxel.as_mut() { v.pinch_amount = *pinch; }
            let cam = *dir * 3.0;
            let rd0 = (-*dir).normalize_or_zero();
            let (hit, normal) = rig.acquire_hit(cam, rd0, false).expect("hook must hit");
            let plane_n = (-rd0).normalize_or_zero();
            if let Some(v) = rig.app.sculpt.voxel.as_mut() {
                v.snake_anchor = Some(hit);
                v.snake_plane_n = plane_n;
                v.first_point_normal = Some(normal);
                v.first_point_pos = Some(hit);
            }
            for f in 1..=*frames {
                let t = f as f32 / *frames as f32;
                let target = hit.lerp(*drag_to, t);
                let rd = (target - cam).normalize_or_zero();
                let (anchor, drag) = {
                    let v = rig.app.sculpt.voxel.as_ref().unwrap();
                    let anchor = v.snake_anchor.unwrap();
                    let pn = v.snake_plane_n;
                    let denom = rd.dot(pn);
                    let vd = if denom.abs() < 1e-5 { Vec3::ZERO } else {
                        let tt = (anchor - cam).dot(pn) / denom;
                        (cam + rd * tt) - anchor
                    };
                    (anchor, vd)
                };
                let (hardness, preset, symmetry, cp) = {
                    let v = rig.app.sculpt.voxel.as_ref().unwrap();
                    (v.falloff, v.falloff_preset, v.symmetry, v.pinch_amount.clamp(0.0, 1.0))
                };
                rig.app.voxel_surface_apply_snake_hook(anchor, drag, *radius, cp, hardness, preset, symmetry);
            }
            rig.stroke_release();
        }
        _ => unreachable!(),
    }
    rig.remesh_barrier();
    let _ = &mut s2;
    // Post-remesh: find every cross-chunk copy group and dump any whose NORMALS differ.
    let app = &rig.app;
    let v = app.sculpt.voxel.as_ref().unwrap();
    let mut n_groups = 0usize;
    let mut n_normal_diverged = 0usize;
    let mut examples = Vec::new();
    for g in &rig.copies.groups {
        n_groups += 1;
        let mut vals: Vec<(u32, u32, [f32; 8])> = Vec::new();
        for &(ci, vi) in g {
            let cid = v.chunks[ci as usize];
            if let Some((verts, _)) = app.cache.mesh_cache.chunk_mesh_data(cid) {
                vals.push((ci, vi, verts[vi as usize]));
            }
        }
        if vals.len() < 2 { continue; }
        let n0 = [vals[0].2[3], vals[0].2[4], vals[0].2[5]];
        for w in &vals[1..] {
            let n1 = [w.2[3], w.2[4], w.2[5]];
            if n0.iter().zip(n1.iter()).any(|(a, b)| a.to_bits() != b.to_bits()) {
                n_normal_diverged += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "pos ({:.5},{:.5},{:.5}) chunk {} n=({:.5},{:.5},{:.5}) vs chunk {} n=({:.5},{:.5},{:.5})",
                        vals[0].2[0], vals[0].2[1], vals[0].2[2],
                        vals[0].0, n0[0], n0[1], n0[2], w.0, n1[0], n1[1], n1[2]));
                }
                break;
            }
        }
    }
    eprintln!("[DIAG] post-remesh copy groups: {n_groups}, NORMAL-diverged groups: {n_normal_diverged}");
    for e in &examples { eprintln!("[DIAG]   {e}"); }
    // Candidate-chunk check for the failing Inflate batch's first frame (mirrors dab_frame).
    if let Op::Dabs { radius, dir0, .. } = &plan[2] {
        let d = dir0.normalize_or_zero();
        let ro = d * 3.0;
        let rd = -d;
        if let Some((hit, _n)) = rig.acquire_hit(ro, rd, false) {
            let vs = rig.app.sculpt.voxel.as_ref().unwrap().grid.voxel_size();
            let probes = vec![
                (hit, radius.max(1e-5) + 2.0 * vs),
                (Vec3::new(-hit.x, hit.y, hit.z), radius.max(1e-5) + 2.0 * vs), // sym X mirror
            ];
            let cand = rig.app.voxel_surface_candidate_chunks(&probes);
            eprintln!("[DIAG] inflate frame-0 hit ({:.5},{:.5},{:.5}) candidates: {cand:?}", hit.x, hit.y, hit.z);
            eprintln!("[DIAG] contains 115: {} contains 116: {}", cand.contains(&115), cand.contains(&116));
        } else {
            eprintln!("[DIAG] inflate frame-0: no hit");
        }
    }
}

#[test]
fn fuzz_repro_seed_563_copy_divergence() {
    let r = run_plan(&repro_seed_563(), surface_validate::spike_threshold(), &mut Stats::default());
    assert!(r.is_ok(), "fuzz-caught regression (seed 563): {:?}", r.err());
}

#[test]
fn fuzz_repro_seed_3985_copy_divergence() {
    let r = run_plan(&repro_seed_3985(), surface_validate::spike_threshold(), &mut Stats::default());
    assert!(r.is_ok(), "fuzz-caught regression (seed 3985): {:?}", r.err());
}

/// DIAGNOSTIC (seed-5296 class): where does the far-out-of-bounds sliver geometry come from,
/// and why does it survive the Remesh barrier?
#[test]
#[ignore]
fn fuzz_diag_seed_5296_stale_geometry() {
    let plan = repro_seed_5296();
    let mut rig = Rig::new();
    let mut stats = Stats::default();
    let mut rw = 0.0f32;
    let dump = |rig: &Rig, label: &str| {
        let v = rig.app.sculpt.voxel.as_ref().unwrap();
        let mut far: Option<(f32, u32, Vec3)> = None;
        let mut n_out = 0usize;
        for (ci, (&cid, &tris)) in v.chunks.iter().zip(v.chunk_tris.iter()).enumerate() {
            if tris == 0 { continue; }
            if let Some((verts, _)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) {
                for v8 in verts {
                    let p = Vec3::new(v8[0], v8[1], v8[2]);
                    let m = p.abs().max_element();
                    if m > 1.05 { n_out += 1; }
                    if far.map_or(true, |(fm, _, _)| m > fm) { far = Some((m, ci as u32, p)); }
                }
            }
        }
        let (b0, b1) = (v.grid.bounds, v.grid.res);
        eprintln!("[DIAG5296] {label}: grid bounds={b0} res={b1} dirty_region={:?} verts_out_of_unit={n_out} farthest={far:?}",
            v.surface_dirty_region);
    };
    exec_plan_on(&mut rig, &plan[..1], 0, 100.0, &mut stats, &mut rw).unwrap();
    dump(&rig, "after Hook");
    // Snapshot chunk_tris to see which chunks the Remesh actually re-derives.
    let tris_before: Vec<u32> = rig.app.sculpt.voxel.as_ref().unwrap().chunk_tris.clone();
    exec_plan_on(&mut rig, &plan[1..2], 1, 100.0, &mut stats, &mut rw).unwrap();
    dump(&rig, "after Remesh");
    let v = rig.app.sculpt.voxel.as_ref().unwrap();
    // Which chunk holds the farthest vert now, and did its mesh change during the barrier?
    let mut far: Option<(f32, usize, Vec3)> = None;
    for (ci, (&cid, &tris)) in v.chunks.iter().zip(v.chunk_tris.iter()).enumerate() {
        if tris == 0 { continue; }
        if let Some((verts, _)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) {
            for v8 in verts {
                let p = Vec3::new(v8[0], v8[1], v8[2]);
                let m = p.abs().max_element();
                if far.map_or(true, |(fm, _, _)| m > fm) { far = Some((m, ci, p)); }
            }
        }
    }
    if let Some((m, ci, p)) = far {
        eprintln!("[DIAG5296] farthest post-remesh: chunk {ci} |p|max={m:.4} at {p:?} tris_before={} tris_after={}",
            tris_before.get(ci).copied().unwrap_or(0), v.chunk_tris[ci]);
    }
}

#[test]
fn fuzz_repro_seed_5296_out_of_bounds_sliver() {
    let r = run_plan(&repro_seed_5296(), surface_validate::spike_threshold(), &mut Stats::default());
    assert!(r.is_ok(), "fuzz-caught regression (seed 5296): {:?}", r.err());
}

/// DIAGNOSTIC (seed-5364): dump the failing vert's ring geometry + displacement history to
/// classify bug vs legitimate extreme-compression fold (inverted Inflate carve).
#[test]
#[ignore]
fn fuzz_diag_seed_5364_inflate_peak() {
    let plan = vec![Op::Dabs {
        brush: VoxelBrush::Inflate, invert: true, radius: 0.1758549, strength: 1.1857585,
        sym: [false, true, false],
        dir0: v3(-0.76218545, 0.64669544, -0.029295634),
        arc_axis: v3(-0.16530885, -0.4058377, -0.8988708),
        arc_step: 0.11292367, frames: 9, mid_undo: None, abort: false,
    }];
    let mut rig = Rig::new();
    let mut stats = Stats::default();
    let mut rw = 0.0f32;
    // Per-frame trajectory of the runaway vert (chunk 98, vert 307): run frame-by-frame.
    let (brush, invert, radius, strength, sym, dir0, arc_axis, arc_step, frames) = match &plan[0] {
        Op::Dabs { brush, invert, radius, strength, sym, dir0, arc_axis, arc_step, frames, .. } =>
            (*brush, *invert, *radius, *strength, *sym, *dir0, *arc_axis, *arc_step, *frames),
        _ => unreachable!(),
    };
    rig.stroke_start(brush, radius, strength, sym);
    let trace = |rig: &Rig, label: &str| {
        let v = rig.app.sculpt.voxel.as_ref().unwrap();
        let cid = v.chunks[98];
        if let Some((verts, _)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) {
            if let Some(v8) = verts.get(307) {
                eprintln!("[DIAG5364] {label}: v307 pos ({:.5},{:.5},{:.5}) n ({:.4},{:.4},{:.4})",
                    v8[0], v8[1], v8[2], v8[3], v8[4], v8[5]);
            }
        }
    };
    trace(&rig, "start");
    // Ground truth: complete cross-chunk area-weighted normal of v307's position key.
    let true_normal = |rig: &Rig| -> (Vec3, u32) {
        let v = rig.app.sculpt.voxel.as_ref().unwrap();
        let cid = v.chunks[98];
        let Some((verts, _)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) else { return (Vec3::ZERO, 0) };
        let p0 = Vec3::new(verts[307][0], verts[307][1], verts[307][2]);
        let vsz = v.grid.voxel_size();
        let eps = vsz.max(1e-6) * 1e-3;
        let key = |p: Vec3| [(p.x / eps).round() as i64, (p.y / eps).round() as i64, (p.z / eps).round() as i64];
        let k0 = key(p0);
        let mut acc = Vec3::ZERO;
        let mut nf = 0u32;
        for (&cid, &tris) in v.chunks.iter().zip(v.chunk_tris.iter()) {
            if tris == 0 { continue; }
            let Some((vs, idx)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
            for t in idx.chunks_exact(3) {
                let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
                if a >= vs.len() || b >= vs.len() || c >= vs.len() { continue; }
                let pa = Vec3::new(vs[a][0], vs[a][1], vs[a][2]);
                let pb = Vec3::new(vs[b][0], vs[b][1], vs[b][2]);
                let pc = Vec3::new(vs[c][0], vs[c][1], vs[c][2]);
                if key(pa) == k0 || key(pb) == k0 || key(pc) == k0 {
                    acc += (pb - pa).cross(pc - pa);
                    nf += 1;
                }
            }
        }
        (acc.normalize_or_zero(), nf)
    };
    for f in 0..frames {
        let rot = glam::Quat::from_axis_angle(arc_axis.normalize_or_zero(), arc_step * f as f32);
        let d = (rot * dir0).normalize_or_zero();
        rig.dab_frame(d * 3.0, -d, f > 0, brush, invert);
        trace(&rig, &format!("frame {f}"));
        let (tn, nf) = true_normal(&rig);
        eprintln!("[DIAG5364]   true welded normal ({:.4},{:.4},{:.4}) from {nf} faces", tn.x, tn.y, tn.z);
    }
    rig.stroke_release();
    let err = "frame-by-frame trace above";
    eprintln!("[DIAG5364] {err:?}");
    // Rescan to get the finding details, then dump its merged ring.
    let (findings, worst) = surface_validate::scan_surface_spikes(&rig.app, surface_validate::spike_threshold());
    eprintln!("[DIAG5364] findings={} worst={worst:.3}", findings.len());
    for f in findings.iter().take(3) {
        eprintln!("[DIAG5364] chunk {} vert {} pos ({:.5},{:.5},{:.5}) ratio {:.2} ring_n {} centroid ({:.5},{:.5},{:.5})",
            f.chunk, f.vert, f.pos.x, f.pos.y, f.pos.z, f.ratio, f.ring_n,
            f.ring_centroid.x, f.ring_centroid.y, f.ring_centroid.z);
        // Dump the vert's normal + every same-chunk ring vert position via the chunk indices.
        let v = rig.app.sculpt.voxel.as_ref().unwrap();
        let cid = v.chunks[f.chunk as usize];
        if let Some((verts, idx)) = rig.app.cache.mesh_cache.chunk_mesh_data(cid) {
            let v8 = verts[f.vert];
            eprintln!("[DIAG5364]   normal ({:.4},{:.4},{:.4})", v8[3], v8[4], v8[5]);
            let mut ring: Vec<usize> = Vec::new();
            for t in idx.chunks_exact(3) {
                let t = [t[0] as usize, t[1] as usize, t[2] as usize];
                if t.contains(&f.vert) {
                    for &o in &t { if o != f.vert && !ring.contains(&o) { ring.push(o); } }
                }
            }
            for &o in &ring {
                let w = verts[o];
                let d = (Vec3::new(w[0], w[1], w[2]) - f.pos).length();
                eprintln!("[DIAG5364]   ring vert {o} pos ({:.5},{:.5},{:.5}) dist {:.5} n ({:.3},{:.3},{:.3})",
                    w[0], w[1], w[2], d, w[3], w[4], w[5]);
            }
        }
    }
}

#[test]
fn fuzz_repro_seed_8965_symmetric_hook_peak() {
    let r = run_plan(&repro_seed_8965(), surface_validate::spike_threshold(), &mut Stats::default());
    assert!(r.is_ok(), "fuzz-caught regression (seed 8965): {:?}", r.err());
}

/// Always-on smoke fuzz: a small seed batch through every op family. Fails loudly with the
/// minimal repro plan if any detector fires.
#[test]
fn fuzz_surface_sculpt_quick() {
    match fuzz_seeds(1..25, surface_validate::spike_threshold()) {
        Ok(s) => print_stats("quick clean", &s),
        Err(e) => panic!("{e}"),
    }
}

/// Deep campaign (release): `cargo test -p app --release fuzz_surface_sculpt_deep -- --ignored --nocapture`.
/// Env: AZRA_FUZZ_START / AZRA_FUZZ_COUNT (default 0 / 2000 per invocation).
#[test]
#[ignore]
fn fuzz_surface_sculpt_deep() {
    let start: u64 = std::env::var("AZRA_FUZZ_START").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let count: u64 = std::env::var("AZRA_FUZZ_COUNT").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let t0 = std::time::Instant::now();
    match fuzz_seeds(start..start + count, surface_validate::spike_threshold()) {
        Ok(s) => {
            eprintln!("[FUZZ] deep clean: seeds {start}..{} in {:.1}s",
                start + count, t0.elapsed().as_secs_f32());
            print_stats("deep", &s);
        }
        Err(e) => panic!("{e}"),
    }
}
