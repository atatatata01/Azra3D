//! op_bench — baseline benchmark for the procedural op-graph feasibility audit.
//!
//! READ-ONLY with respect to the kernel: calls the public HalfEdgeMesh / bevel_op /
//! mesh_boolean API exactly as the app does. No kernel source is modified.
//!
//! Methodology: std::time::Instant, explicit warmup, median + p95 over N runs
//! (N=20 default, reduced adaptively for runs costing >1s each; the actual N is
//! printed with every row). criterion is not a workspace dep, per the audit prompt.
//!
//! Run: cargo run -p op_bench --release   (dev profile is opt-level 0 — never bench it)

use core_math::bevel_op::{bevel_edges_offset, bevel_edges_offset_mut, BevelParams};
use core_math::half_edge::{FaceId, HalfEdgeId, HalfEdgeMesh, VertexId, NO_FACE, NO_TWIN};
use core_math::mesh_boolean::{mesh_boolean, BoolOp};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

// ============================================================
// G. Counting allocator — peak heap proxy for peak RSS
// ============================================================

struct CountingAlloc;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let c = CUR.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(c, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l);
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

fn reset_peak() {
    PEAK.store(CUR.load(Ordering::Relaxed), Ordering::Relaxed);
}
fn peak_mb() -> f64 {
    PEAK.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
}

// ============================================================
// Timing helpers — median + p95, never mean alone
// ============================================================

struct Stats {
    median_ms: f64,
    p95_ms: f64,
    runs: usize,
}

fn stats_from(mut samples: Vec<f64>) -> Stats {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let median = if n % 2 == 1 {
        samples[n / 2]
    } else {
        0.5 * (samples[n / 2 - 1] + samples[n / 2])
    };
    let p95 = samples[((n as f64 * 0.95).ceil() as usize).min(n) - 1];
    Stats { median_ms: median, p95_ms: p95, runs: n }
}

/// Bench `run` with a fresh value from `setup` each iteration (setup excluded from timing).
/// Adaptive run count: probe once; >1s per run → 7 runs, >100ms → 12, else 20. Warmup 2 (or 0 if slow).
fn bench<T, S: FnMut() -> T, R: FnMut(T)>(mut setup: S, mut run: R) -> Stats {
    let v = setup();
    let t0 = Instant::now();
    run(v);
    let probe = t0.elapsed().as_secs_f64() * 1000.0;
    let (runs, warmup) = if probe > 1000.0 {
        (7usize, 0usize)
    } else if probe > 100.0 {
        (12, 1)
    } else {
        (20, 2)
    };
    for _ in 0..warmup {
        let v = setup();
        run(v);
    }
    let mut samples = Vec::with_capacity(runs + 1);
    samples.push(probe);
    for _ in 0..runs {
        let v = setup();
        let t = Instant::now();
        run(v);
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    stats_from(samples)
}

fn row(name: &str, scene: &str, s: &Stats, extra: &str) {
    println!(
        "RESULT | {:<28} | {:<3} | median {:>12.4} ms | p95 {:>12.4} ms | n={:<2} | {}",
        name, scene, s.median_ms, s.p95_ms, s.runs, extra
    );
}

// ============================================================
// Deterministic LCG (no rand dep; fixed seed => reproducible scenes)
// ============================================================

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// ============================================================
// F. Op log — the graph-as-document proxy: params only, no geometry
// ============================================================

#[derive(Serialize, Deserialize, Clone)]
enum OpRecord {
    Plane { w: f32, d: f32, sx: u32, sz: u32 },
    ExtrudeFace { face: u32, offset: [f32; 3] },
    LoopCut { start_he: u32, ts: Vec<f32> },
    BevelEdges { edges: Vec<u32>, width: f32, segments: u32, shape: f32, clamp: bool },
    SubdivideFaces { faces: Vec<u32> },
    TranslateVertex { vert: u32, delta: [f32; 3] },
    Boolean { op: u8 },
}

// ============================================================
// Scene construction — mixed op sequence, fully deterministic
// ============================================================

/// Find an interior half-edge (both sides have faces) — deterministic first hit.
fn interior_he(m: &HalfEdgeMesh) -> HalfEdgeId {
    for i in 0..m.half_edges.len() as u32 {
        let he = HalfEdgeId(i);
        if !m.is_edge_live(he) {
            continue;
        }
        let e = m.he(he);
        if e.twin != NO_TWIN && e.face != NO_FACE && m.he(e.twin).face != NO_FACE {
            return he;
        }
    }
    HalfEdgeId(0)
}

struct Scene {
    mesh: HalfEdgeMesh,
    log: Vec<OpRecord>,
    name: &'static str,
}

/// Build a scene: n×n quad plane, 8 face extrudes, 1 loop cut, 1 edge-loop bevel,
/// `translates` vertex translates. Boolean is measured separately (the plane is an
/// OPEN mesh; mesh_boolean is specified for closed meshes — see report).
fn build_scene(name: &'static str, n: u32, translates: usize) -> Scene {
    let mut log = Vec::new();
    let mut mesh = HalfEdgeMesh::new_plane(4.0, 4.0, n, n);
    log.push(OpRecord::Plane { w: 4.0, d: 4.0, sx: n, sz: n });

    // 8 extrudes on spread faces
    let fcount = mesh.face_count() as u32;
    for k in 0..8u32 {
        let f = FaceId((k * (fcount / 9)).min(fcount - 1));
        let nrm = mesh.compute_face_normal(f);
        let off = nrm * 0.15;
        mesh.extrude_face(f, off);
        log.push(OpRecord::ExtrudeFace { face: f.0, offset: off.to_array() });
    }

    // 1 loop cut (from_polygons rebuild — all IDs change after this)
    let he = interior_he(&mesh);
    mesh.loop_cut_multi(he, &[0.5], None);
    log.push(OpRecord::LoopCut { start_he: he.0, ts: vec![0.5] });

    // 1 edge-loop bevel (whole-mesh rebuild via bevel_op)
    let he = interior_he(&mesh);
    let loop_edges: HashSet<u32> = mesh.select_edge_loop(he).iter().map(|h| h.0).collect();
    let params = BevelParams { width: 0.01, segments: 1, shape: 0.5, clamp: true };
    let beveled = bevel_edges_offset(&mesh, &loop_edges, &params);
    log.push(OpRecord::BevelEdges {
        edges: { let mut v: Vec<u32> = loop_edges.iter().copied().collect(); v.sort_unstable(); v },
        width: params.width,
        segments: params.segments,
        shape: params.shape,
        clamp: params.clamp,
    });
    mesh = beveled;

    // A few thousand deterministic vertex translates
    let mut rng = Lcg(0xA2FA_5EED);
    let vcount = mesh.vertices.len();
    for _ in 0..translates {
        let vi = rng.pick(vcount) as u32;
        let d = Vec3::new(
            (rng.pick(1000) as f32 / 1000.0 - 0.5) * 0.002,
            (rng.pick(1000) as f32 / 1000.0) * 0.002,
            (rng.pick(1000) as f32 / 1000.0 - 0.5) * 0.002,
        );
        mesh.vert_mut(VertexId(vi)).position += d;
        log.push(OpRecord::TranslateVertex { vert: vi, delta: d.to_array() });
    }
    mesh.recompute_normals();

    Scene { mesh, log, name }
}

/// Replay a scene log from scratch (measurement B: naive full rebuild).
fn replay(log: &[OpRecord]) -> HalfEdgeMesh {
    let mut mesh = HalfEdgeMesh::new();
    for op in log {
        match op {
            OpRecord::Plane { w, d, sx, sz } => mesh = HalfEdgeMesh::new_plane(*w, *d, *sx, *sz),
            OpRecord::ExtrudeFace { face, offset } => {
                mesh.extrude_face(FaceId(*face), Vec3::from_array(*offset));
            }
            OpRecord::LoopCut { start_he, ts } => {
                mesh.loop_cut_multi(HalfEdgeId(*start_he), ts, None);
            }
            OpRecord::BevelEdges { edges, width, segments, shape, clamp } => {
                let sel: HashSet<u32> = edges.iter().copied().collect();
                let p = BevelParams { width: *width, segments: *segments, shape: *shape, clamp: *clamp };
                mesh = bevel_edges_offset(&mesh, &sel, &p);
            }
            OpRecord::SubdivideFaces { faces } => {
                let sel: HashSet<u32> = faces.iter().copied().collect();
                mesh.subdivide_faces(&sel);
            }
            OpRecord::TranslateVertex { vert, delta } => {
                mesh.vert_mut(VertexId(*vert)).position += Vec3::from_array(*delta);
            }
            OpRecord::Boolean { .. } => {}
        }
    }
    mesh.recompute_normals();
    mesh
}

// ============================================================
// Measurements
// ============================================================

fn mesh_counts(m: &HalfEdgeMesh) -> String {
    format!("V={} HE={} F={}", m.vertex_count(), m.edge_count() * 2, m.face_count())
}

fn mesh_bytes(m: &HalfEdgeMesh) -> usize {
    m.vertices.capacity() * std::mem::size_of::<core_math::half_edge::HVertex>()
        + m.half_edges.capacity() * std::mem::size_of::<core_math::half_edge::HalfEdge>()
        + m.faces.capacity() * std::mem::size_of::<core_math::half_edge::HFace>()
}

fn per_op_suite(scene: &Scene) {
    let base = &scene.mesh;
    let sname = scene.name;

    // translate one vertex (measured as 1000 translates / 1000 — single op is sub-timer)
    let s = bench(
        || base.clone(),
        |mut m| {
            let n = m.vertices.len();
            for i in 0..1000usize {
                let vi = VertexId(((i * 7919) % n) as u32);
                m.vert_mut(vi).position += Vec3::splat(1e-6);
            }
        },
    );
    row("translate x1000", sname, &s, "per-translate = median/1000");

    // extrude one face
    let f = FaceId(base.face_count() as u32 / 2);
    let nrm = base.compute_face_normal(f);
    let s = bench(|| base.clone(), |mut m| { m.extrude_face(f, nrm * 0.1); });
    row("extrude_face x1", sname, &s, "");

    // bevel one edge loop (out-of-place; no clone needed but keep identical harness)
    let he = interior_he(base);
    let loop_edges: HashSet<u32> = base.select_edge_loop(he).iter().map(|h| h.0).collect();
    let nsel = loop_edges.len();
    let p = BevelParams { width: 0.005, segments: 1, shape: 0.5, clamp: true };
    let s = bench(|| (), |_| { let out = bevel_edges_offset(base, &loop_edges, &p); std::hint::black_box(out.face_count()); });
    row("bevel edge-loop", sname, &s, &format!("{} edges selected", nsel));

    // loop cut, one cut
    let s = bench(|| base.clone(), |mut m| { let he = interior_he(&m); m.loop_cut_multi(he, &[0.5], None); });
    row("loop_cut x1", sname, &s, "includes interior_he scan");

    // subdivide ONE face
    let one: HashSet<u32> = [f.0].into_iter().collect();
    let s = bench(|| base.clone(), |mut m| { m.subdivide_faces(&one); });
    row("subdivide_faces x1 face", sname, &s, "");

    // dissolve one interior edge
    let s = bench(|| base.clone(), |mut m| { let he = interior_he(&m); m.dissolve_edge(he); });
    row("dissolve_edge x1", sname, &s, "");

    // weld: merge the two endpoints of one interior edge
    let he = interior_he(base);
    let vb = base.he(he).vertex;
    let va = base.he_source(he);
    let target = (base.vert(va).position + base.vert(vb).position) * 0.5;
    let s = bench(|| base.clone(), |mut m| { m.merge_vertices_to(&[va, vb], target); });
    row("weld (merge 2 verts)", sname, &s, "");

    // full-mesh side effects the app actually runs after edits
    let s = bench(|| base.clone(), |mut m| { m.recompute_normals(); });
    row("recompute_normals (full)", sname, &s, "");

    let s = bench(|| (), |_| { let (v, i) = base.to_raster_data(); std::hint::black_box((v.len(), i.len())); });
    row("to_raster_data (full)", sname, &s, "render-upload tessellation");
}

fn ring_walk(m: &HalfEdgeMesh) -> f64 {
    // Full half-edge traversal: every face cycle, summing vertex positions.
    let mut acc = Vec3::ZERO;
    for fi in 0..m.faces.len() as u32 {
        let f = FaceId(fi);
        let start = m.face(f).half_edge;
        if start == core_math::half_edge::NO_EDGE {
            continue;
        }
        let mut he = start;
        loop {
            let e = m.he(he);
            acc += m.vert(e.vertex).position;
            he = e.next;
            if he == start {
                break;
            }
        }
    }
    (acc.x + acc.y + acc.z) as f64
}

/// A2. Boolean benchmark — closed meshes only. new_uv_sphere is NOT watertight
/// (boundary_half_edge_count > 0) and is rejected by mesh_to_polygons' open-mesh
/// gate, so the curved operand is a CC-subdivided box instead.
/// NOTE: mesh_boolean has a hard MAX_BSP_POLYS = 262_144 triangle ceiling
/// (mesh_boolean.rs:75) — a true L/XL-scale boolean returns None by design.
fn run_bool_bench(quick: bool) {
    println!("\n== A2. boolean (closed meshes: box − CC-subdivided box) ==");
    let bool_sizes: Vec<(&str, u32)> = if quick {
        vec![("S", 4)]
    } else {
        vec![("S", 4), ("M", 6), ("L", 7)]
    };
    for (bn, levels) in bool_sizes {
        let a = HalfEdgeMesh::new_box(Vec3::splat(0.8));
        let mut b = HalfEdgeMesh::new_box(Vec3::splat(0.75));
        for _ in 0..levels {
            b.subdivide_catmull_clark();
        }
        for v in b.vertices.iter_mut() {
            v.position += Vec3::new(0.8, 0.0, 0.0);
        }
        let fa = a.face_count();
        let fb = b.face_count();
        let t = Instant::now();
        let out = mesh_boolean(&a, &b, BoolOp::Difference);
        let probe = t.elapsed().as_secs_f64() * 1000.0;
        match out {
            Some(o) => {
                if probe > 20_000.0 {
                    row("boolean difference", bn, &stats_from(vec![probe]), &format!("in {}+{} faces out {} — SINGLE RUN (too slow to repeat)", fa, fb, o.face_count()));
                } else {
                    let s = bench(|| (), |_| { let o = mesh_boolean(&a, &b, BoolOp::Difference); std::hint::black_box(o.map(|m| m.face_count())); });
                    row("boolean difference", bn, &s, &format!("in {}+{} faces, out {}", fa, fb, o.face_count()));
                }
            }
            None => println!("RESULT | boolean difference | {} | FAILED (returned None) | in {}+{} faces", bn, fa, fb),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "--quick");

    if args.iter().any(|a| a == "--probe") {
        // Which operand shapes does the boolean accept? (box-box is the tested class)
        let bx = |c: Vec3, h: f32| {
            let mut m = HalfEdgeMesh::new_box(Vec3::splat(h));
            for v in m.vertices.iter_mut() { v.position += c; }
            m
        };
        let sph = |c: Vec3, r: f32, s: u32, rg: u32| {
            let mut m = HalfEdgeMesh::new_uv_sphere(r, s, rg);
            for v in m.vertices.iter_mut() { v.position += c; }
            m
        };
        let cases: Vec<(&str, HalfEdgeMesh, HalfEdgeMesh, BoolOp)> = vec![
            ("box-box diff", bx(Vec3::ZERO, 1.0), bx(Vec3::splat(1.0), 0.7), BoolOp::Difference),
            ("box-box union", bx(Vec3::ZERO, 1.0), bx(Vec3::splat(0.5), 1.0), BoolOp::Union),
            ("box-sphere16 diff", bx(Vec3::ZERO, 0.8), sph(Vec3::new(0.8, 0.0, 0.0), 0.6, 16, 8), BoolOp::Difference),
            ("box-sphere32 diff", bx(Vec3::ZERO, 0.8), sph(Vec3::new(0.8, 0.0, 0.0), 0.6, 32, 16), BoolOp::Difference),
            ("sphere16-box diff", sph(Vec3::ZERO, 1.0, 16, 8), bx(Vec3::new(1.0, 0.0, 0.0), 0.5), BoolOp::Difference),
            ("box-sphere16 union", bx(Vec3::ZERO, 0.8), sph(Vec3::new(0.8, 0.0, 0.0), 0.6, 16, 8), BoolOp::Union),
        ];
        for (name, a, b, op) in cases {
            let closed_a = core_math::mesh_boolean::boundary_half_edge_count(&a) == 0;
            let closed_b = core_math::mesh_boolean::boundary_half_edge_count(&b) == 0;
            let t = Instant::now();
            let r = mesh_boolean(&a, &b, op);
            println!(
                "PROBE {name}: closed_a={closed_a} closed_b={closed_b} -> {} ({:.1} ms)",
                match &r { Some(m) => format!("OK {} faces", m.face_count()), None => "None".into() },
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        return;
    }

    let bool_only = args.iter().any(|a| a == "--bool-only");
    // Locality spike: scenes + measurement C only (skip A/B/D/E/F/G + bool).
    let c_only = args.iter().any(|a| a == "--c-only");
    if !bool_only {
        println!("op_bench — procedural op-graph baseline. quick={quick} c_only={c_only}");
    }
    println!("sizes: HVertex={} HalfEdge={} HFace={} bytes",
        std::mem::size_of::<core_math::half_edge::HVertex>(),
        std::mem::size_of::<core_math::half_edge::HalfEdge>(),
        std::mem::size_of::<core_math::half_edge::HFace>());

    // ---------- scenes ----------
    let defs: Vec<(&str, u32, usize)> = if quick {
        vec![("S", 30, 500), ("M", 140, 2000)]
    } else {
        vec![("S", 30, 500), ("M", 140, 2000), ("L", 450, 3000), ("XL", 1000, 3000)]
    };

    if bool_only {
        run_bool_bench(quick);
        return;
    }
    let mut scenes = Vec::new();
    for (name, n, tr) in &defs {
        reset_peak();
        let t = Instant::now();
        let sc = build_scene(name, *n, *tr);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "SCENE {} | {} | ops={} | first-build {:.1} ms | peak-heap {:.1} MB | mesh {:.1} MB",
            sc.name, mesh_counts(&sc.mesh), sc.log.len(), build_ms, peak_mb(),
            mesh_bytes(&sc.mesh) as f64 / 1048576.0
        );
        scenes.push(sc);
    }

    if !c_only {
        // ---------- determinism: rebuild each scene, compare bincode bytes ----------
        for sc in &scenes {
            let m2 = replay(&sc.log);
            let b1 = bincode::serialize(&sc.mesh).unwrap();
            let b2 = bincode::serialize(&m2).unwrap();
            println!(
                "DETERMINISM {} | replay content_signature {:#018x} vs {:#018x} | bincode byte-identical: {}",
                sc.name, sc.mesh.content_signature(), m2.content_signature(), b1 == b2
            );
        }

        // ---------- A. per-op isolated ----------
        println!("\n== A. per-op isolated ==");
        for sc in &scenes {
            per_op_suite(sc);
        }

        run_bool_bench(quick);

        // ---------- B. full rebuild ----------
        println!("\n== B. full rebuild (replay entire op log from scratch) ==");
        for sc in &scenes {
            let s = bench(|| (), |_| { let m = replay(&sc.log); std::hint::black_box(m.face_count()); });
            row("full replay", sc.name, &s, &format!("{} ops", sc.log.len()));
        }
    }

    // ---------- C. locality: one-ring bevel and one loop cut on L vs XL ----------
    println!("\n== C. locality check (same local op, growing mesh) ==");
    for sc in &scenes {
        let he = interior_he(&sc.mesh);
        let ring: HashSet<u32> = sc.mesh.select_edge_loop(he).iter().map(|h| h.0).collect();
        let p = BevelParams { width: 0.002, segments: 1, shape: 0.5, clamp: true };
        // Locality: mut in-place on large meshes (face_count≥8192); clone+reserve outside timer.
        let s = bench(
            || {
                let mut m = sc.mesh.clone();
                m.reserve_elements(4096, 16384, 4096);
                m
            },
            |mut m| {
                bevel_edges_offset_mut(&mut m, &ring, &p, None);
                std::hint::black_box(m.face_count());
            },
        );
        row("C: bevel 1 edge-loop", sc.name, &s, &format!("{} edges — if this grows with |mesh|, locality FAILS", ring.len()));
        // Setup reserves headroom OUTSIDE the timed section. `Vec::clone` packs to exact
        // capacity; the first push then memcpys the whole mesh (O(mesh) — not the op). A
        // live mesh after any prior edit already has growth headroom; this matches that.
        let s = bench(
            || {
                let mut m = sc.mesh.clone();
                m.reserve_elements(4096, 16384, 4096);
                m
            },
            |mut m| {
                let he = interior_he(&m);
                m.loop_cut_multi(he, &[0.5], None);
            },
        );
        row("C: loop_cut 1 cut", sc.name, &s, "");
        // the honest local baseline: what a truly local op costs
        let f = FaceId(sc.mesh.face_count() as u32 / 3);
        let nrm = sc.mesh.compute_face_normal(f);
        let s = bench(
            || {
                let mut m = sc.mesh.clone();
                m.reserve_elements(64, 256, 64);
                m
            },
            |mut m| {
                m.extrude_face(f, nrm * 0.05);
            },
        );
        row("C: extrude_face (local op)", sc.name, &s, "in-place op, no rebuild");
    }

    if c_only {
        return;
    }

    // ---------- D. cache behaviour: full ring walk ----------
    println!("\n== D. cache: full half-edge ring walk ==");
    for sc in &scenes {
        let bytes = sc.mesh.vertices.len() * std::mem::size_of::<core_math::half_edge::HVertex>()
            + sc.mesh.half_edges.len() * std::mem::size_of::<core_math::half_edge::HalfEdge>()
            + sc.mesh.faces.len() * std::mem::size_of::<core_math::half_edge::HFace>();
        let s = bench(|| (), |_| { std::hint::black_box(ring_walk(&sc.mesh)); });
        let gbs = (bytes as f64 / 1e9) / (s.median_ms / 1000.0);
        row("ring walk (full mesh)", sc.name, &s, &format!("~{:.1} MB touched, effective {:.2} GB/s", bytes as f64 / 1048576.0, gbs));
    }
    println!("(cache-miss rate: NOT MEASURED — no Linux `perf` on Windows; GB/s vs mesh size is the proxy)");

    // ---------- E. name-resolution proxy: HashMap lookups ----------
    println!("\n== E. name-resolution proxy (HashMap<u64,u32> sized to element count) ==");
    for sc in &scenes {
        let elems = sc.mesh.vertices.len() + sc.mesh.half_edges.len() + sc.mesh.faces.len();
        let map: HashMap<u64, u32> = (0..elems as u64).map(|k| (k.wrapping_mul(0x9E3779B97F4A7C15), k as u32)).collect();
        for n in [100usize, 1000, 10000] {
            let keys: Vec<u64> = {
                let mut rng = Lcg(42);
                (0..n).map(|_| (rng.next() % elems as u64).wrapping_mul(0x9E3779B97F4A7C15)).collect()
            };
            let s = bench(|| (), |_| {
                let mut acc = 0u64;
                for k in &keys {
                    if let Some(v) = map.get(k) { acc = acc.wrapping_add(*v as u64); }
                }
                std::hint::black_box(acc);
            });
            row(&format!("lookup x{}", n), sc.name, &s, &format!("keyspace {} — per-lookup {:.1} ns", elems, s.median_ms * 1e6 / n as f64));
        }
    }

    // ---------- F. op-log serialization ----------
    println!("\n== F. op-log serialization (params only, bincode) ==");
    for sc in &scenes {
        let bytes = bincode::serialize(&sc.log).unwrap();
        let s = bench(|| (), |_| { std::hint::black_box(bincode::serialize(&sc.log).unwrap().len()); });
        row("op-log serialize", sc.name, &s, &format!("{} ops -> {} bytes ({:.1} B/op)", sc.log.len(), bytes.len(), bytes.len() as f64 / sc.log.len() as f64));
        let s = bench(|| (), |_| { let l: Vec<OpRecord> = bincode::deserialize(&bytes).unwrap(); std::hint::black_box(l.len()); });
        row("op-log deserialize", sc.name, &s, "");
        // contrast: full mesh blob (what .fl4 stores today)
        let mb = bincode::serialize(&sc.mesh).unwrap();
        println!("        (contrast: full HalfEdgeMesh bincode blob = {:.2} MB)", mb.len() as f64 / 1048576.0);
    }

    // ---------- G. memory ----------
    println!("\n== G. memory ==");
    for sc in &scenes {
        let (v, i) = sc.mesh.to_raster_data();
        let gpu = v.len() * 32 + i.len() * 4;
        println!(
            "MEM {} | mesh structure {:.2} MB | raster buffers {:.2} MB (verts {} x32B, idx {} x4B)",
            sc.name, mesh_bytes(&sc.mesh) as f64 / 1048576.0, gpu as f64 / 1048576.0, v.len(), i.len()
        );
    }

    println!("\ndone.");
}
