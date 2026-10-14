// core_engine/app/src/surface_validate.rs
//
// SURFACE-SCULPT SPIKE VALIDATOR — data-first artifact hunting (2026-07-23).
//
// One position-pure detector shared by:
//   • the headless fuzz harness (`surface_fuzz.rs`, cfg(test)) — runs it after EVERY scripted op;
//   • the LIVE app, behind `AZRA_SCULPT_VALIDATE=1` — runs it after every surface mutation
//     (dabs / Snake Hook / Move commit / surface undo) and, on a finding, prints a `[SPIKE]`
//     line with the op context and appends a machine-readable record to
//     `%TEMP%/azra_sculpt_validate/spikes_<pid>.log` so the owner's next manual repro produces
//     replayable DATA instead of a screenshot.
//
// Zero cost when off: ONE OnceLock<bool> env check per call site, nothing else.
//
// METRIC — "isolated peak": for every vertex of every live chunk mesh, the distance from the
// vertex to the CENTROID OF ITS 1-RING NEIGHBOURS, in units of the chunk's MEDIAN EDGE LENGTH.
// A smooth Surface-Nets mesh (and any honest brush deformation, which moves a vertex WITH its
// falloff-weighted neighbourhood) keeps this ratio ~O(1); a needle spike is by definition a
// vertex displaced far from where its ring stayed. NaN/Inf anywhere is an instant finding.
// Threshold: `AZRA_SCULPT_SPIKE_K` (default 8.0 × median edge; the fuzz harness calibrates —
// clean runs across all brush families stay well under this).

use crate::AzraApp;
use glam::Vec3;

/// Detector verdict for one chunk vertex.
#[derive(Debug, Clone)]
pub(crate) struct SpikeFinding {
    pub chunk: u32,
    pub vert: usize,
    pub pos: Vec3,
    /// |pos − 1-ring centroid| / median edge length (f32::INFINITY for NaN/Inf verts).
    pub ratio: f32,
    pub ring_centroid: Vec3,
    pub ring_n: u32,
}

/// Spike ratio threshold (defaults to 8.0; env `AZRA_SCULPT_SPIKE_K` overrides).
pub(crate) fn spike_threshold() -> f32 {
    static K: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *K.get_or_init(|| {
        std::env::var("AZRA_SCULPT_SPIKE_K").ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(8.0)
    })
}

/// Pure per-chunk isolated-peak scan. `median_edge` of THIS chunk is computed here too
/// (cheap: one pass over indices). Returns findings above `k` plus the worst ratio seen
/// (for harness calibration).
pub(crate) fn scan_chunk_spikes(
    chunk: u32,
    verts: &[[f32; 8]],
    indices: &[u32],
    k: f32,
    out: &mut Vec<SpikeFinding>,
) -> f32 {
    let n = verts.len();
    if n == 0 || indices.len() < 3 {
        return 0.0;
    }
    // NaN/Inf: instant findings.
    for (vi, v8) in verts.iter().enumerate() {
        if !(v8[0].is_finite() && v8[1].is_finite() && v8[2].is_finite()) {
            out.push(SpikeFinding {
                chunk, vert: vi,
                pos: Vec3::new(v8[0], v8[1], v8[2]),
                ratio: f32::INFINITY,
                ring_centroid: Vec3::ZERO, ring_n: 0,
            });
        }
    }
    // 1-ring accumulation from triangle edges (both directions), index-based within the chunk.
    let mut sum: Vec<Vec3> = vec![Vec3::ZERO; n];
    let mut cnt: Vec<u32> = vec![0; n];
    let mut ring: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut edge_lens: Vec<f32> = Vec::with_capacity(indices.len());
    let p = |i: usize| Vec3::new(verts[i][0], verts[i][1], verts[i][2]);
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if a >= n || b >= n || c >= n { continue; }
        let (pa, pb, pc) = (p(a), p(b), p(c));
        if !(pa.is_finite() && pb.is_finite() && pc.is_finite()) { continue; }
        sum[a] += pb + pc; cnt[a] += 2;
        sum[b] += pa + pc; cnt[b] += 2;
        sum[c] += pa + pb; cnt[c] += 2;
        ring[a].push(b as u32); ring[a].push(c as u32);
        ring[b].push(a as u32); ring[b].push(c as u32);
        ring[c].push(a as u32); ring[c].push(b as u32);
        edge_lens.push((pa - pb).length());
        edge_lens.push((pb - pc).length());
        edge_lens.push((pc - pa).length());
    }
    if edge_lens.is_empty() { return 0.0; }
    let mid = edge_lens.len() / 2;
    // select_nth is O(n) — no full sort per op.
    edge_lens.select_nth_unstable_by(mid, |x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let median_edge = edge_lens[mid].max(1e-9);
    // Pass 1: per-vertex ring offset d_i = |p_i − ring centroid|.
    let mut d: Vec<f32> = vec![0.0; n];
    for vi in 0..n {
        if cnt[vi] < 4 { continue; } // <2 triangles: boundary sliver, no meaningful ring
        let pv = p(vi);
        if !pv.is_finite() { continue; } // already reported above
        d[vi] = (pv - sum[vi] / cnt[vi] as f32).length();
    }
    // Pass 2: NEEDLE = a vertex whose ring offset is large in ABSOLUTE terms (> k × median edge)
    // AND large in LOCAL CONTRAST (≫ its neighbours' ring offsets). The contrast term is what
    // separates a needle (neighbours normal) from a legitimate sharp RIDGE (Pinch/Crease/Snake
    // Hook: every ridge vert's ring offset is elevated together — calibrated Pinch reaches ~7×
    // median edge in honest geometry, so the absolute test alone cannot discriminate).
    const CONTRAST: f32 = 3.0;
    let mut worst = 0.0f32; // max over verts of min(d/(k·med), contrast/CONTRAST): ≥1 ⇒ finding
    for vi in 0..n {
        if d[vi] <= 0.0 { continue; }
        let mut nb_max = 0.0f32;
        for &nj in &ring[vi] {
            let v = d[nj as usize];
            if v > nb_max { nb_max = v; }
        }
        let abs_norm = d[vi] / (k * median_edge);
        let contrast = d[vi] / (nb_max + 0.25 * median_edge);
        let score = abs_norm.min(contrast / CONTRAST);
        if score > worst { worst = score; }
        if score > 1.0 {
            out.push(SpikeFinding {
                chunk, vert: vi, pos: p(vi),
                ratio: d[vi] / median_edge,
                ring_centroid: sum[vi] / cnt[vi].max(1) as f32,
                ring_n: cnt[vi] / 2,
            });
        }
    }
    worst
}

/// Scan the WHOLE live object with POSITION-WELDED cross-chunk rings. A per-chunk scan sees
/// only half a ring at chunk borders (the neighbour chunk owns the other faces), so a border
/// vertex under strong legitimate deformation reads as an isolated peak — a detector artifact,
/// not geometry (fuzz seeds 8965/5296 exposed this: ring_n=2 findings exactly on borders).
/// Here every chunk's verts+faces merge into ONE mesh, verts welded on exact position bits —
/// the same duplication convention the mesher guarantees — so every ring is complete.
pub(crate) fn scan_surface_spikes(app: &AzraApp, k: f32) -> (Vec<SpikeFinding>, f32) {
    let mut out = Vec::new();
    let Some(v) = app.sculpt.voxel.as_ref() else { return (out, 0.0) };
    // Weld map: exact position bits → merged vertex id. (Halo copies are bit-identical by
    // construction; a DIVERGED copy welds as two verts — the copy-divergence detector, not
    // this one, owns that failure class.)
    let mut merged_id: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
    let mut mpos: Vec<Vec3> = Vec::new();
    let mut msrc: Vec<(u32, u32)> = Vec::new(); // merged id → (chunk, local vert) of first sighting
    let mut mtris: Vec<[u32; 3]> = Vec::new();
    let mut nan = false;
    for (ci, (&cid, &tris)) in v.chunks.iter().zip(v.chunk_tris.iter()).enumerate() {
        if tris == 0 { continue; }
        let Some((verts, indices)) = app.cache.mesh_cache.chunk_mesh_data(cid) else { continue };
        let mut local: Vec<u32> = Vec::with_capacity(verts.len());
        for (vi, v8) in verts.iter().enumerate() {
            if !(v8[0].is_finite() && v8[1].is_finite() && v8[2].is_finite()) {
                out.push(SpikeFinding {
                    chunk: ci as u32, vert: vi,
                    pos: Vec3::new(v8[0], v8[1], v8[2]),
                    ratio: f32::INFINITY, ring_centroid: Vec3::ZERO, ring_n: 0,
                });
                nan = true;
            }
            let key = [v8[0].to_bits(), v8[1].to_bits(), v8[2].to_bits()];
            let id = *merged_id.entry(key).or_insert_with(|| {
                mpos.push(Vec3::new(v8[0], v8[1], v8[2]));
                msrc.push((ci as u32, vi as u32));
                (mpos.len() - 1) as u32
            });
            local.push(id);
        }
        // Faces are owned by exactly ONE chunk (halo duplicates verts, never faces), so simply
        // appending every chunk's triangles yields each face once with complete rings.
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            if a >= local.len() || b >= local.len() || c >= local.len() { continue; }
            mtris.push([local[a], local[b], local[c]]);
        }
    }
    if nan || mpos.is_empty() || mtris.is_empty() {
        return (out, if nan { f32::INFINITY } else { 0.0 });
    }
    let worst = scan_welded_mesh(&mpos, &msrc, &mtris, k, &mut out);
    (out, worst)
}

/// Isolated-peak scan over a position-welded whole-object mesh (complete 1-rings).
/// Same metric as `scan_chunk_spikes`; shared by `scan_surface_spikes`.
fn scan_welded_mesh(
    pos: &[Vec3],
    src: &[(u32, u32)],
    tris: &[[u32; 3]],
    k: f32,
    out: &mut Vec<SpikeFinding>,
) -> f32 {
    let n = pos.len();
    let mut sum: Vec<Vec3> = vec![Vec3::ZERO; n];
    let mut cnt: Vec<u32> = vec![0; n];
    let mut ring: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut edge_lens: Vec<f32> = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let (pa, pb, pc) = (pos[a], pos[b], pos[c]);
        sum[a] += pb + pc; cnt[a] += 2;
        sum[b] += pa + pc; cnt[b] += 2;
        sum[c] += pa + pb; cnt[c] += 2;
        ring[a].push(t[1]); ring[a].push(t[2]);
        ring[b].push(t[0]); ring[b].push(t[2]);
        ring[c].push(t[0]); ring[c].push(t[1]);
        edge_lens.push((pa - pb).length());
        edge_lens.push((pb - pc).length());
        edge_lens.push((pc - pa).length());
    }
    if edge_lens.is_empty() { return 0.0; }
    let mid = edge_lens.len() / 2;
    edge_lens.select_nth_unstable_by(mid, |x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let median_edge = edge_lens[mid].max(1e-9);
    let mut d: Vec<f32> = vec![0.0; n];
    for vi in 0..n {
        if cnt[vi] < 4 { continue; }
        d[vi] = (pos[vi] - sum[vi] / cnt[vi] as f32).length();
    }
    const CONTRAST: f32 = 3.0;
    let mut worst = 0.0f32;
    for vi in 0..n {
        if d[vi] <= 0.0 { continue; }
        let mut nb_max = 0.0f32;
        for &nj in &ring[vi] {
            let v = d[nj as usize];
            if v > nb_max { nb_max = v; }
        }
        let abs_norm = d[vi] / (k * median_edge);
        let contrast = d[vi] / (nb_max + 0.25 * median_edge);
        let score = abs_norm.min(contrast / CONTRAST);
        if score > worst { worst = score; }
        if score > 1.0 {
            out.push(SpikeFinding {
                chunk: src[vi].0, vert: src[vi].1 as usize, pos: pos[vi],
                ratio: d[vi] / median_edge,
                ring_centroid: sum[vi] / cnt[vi].max(1) as f32,
                ring_n: cnt[vi] / 2,
            });
        }
    }
    worst
}

/// Live-validator gate: `AZRA_SCULPT_VALIDATE=1`. One OnceLock read — zero cost when off.
pub(crate) fn live_validate_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("AZRA_SCULPT_VALIDATE").map_or(false, |v| v == "1"))
}

/// Run the detector on the live mesh after a surface mutation and report findings.
/// `ctx` = op context (brush, sgn, dab positions/radii …) formatted by the call site — it is
/// what makes a finding REPLAYABLE by the fuzz harness. No-op unless AZRA_SCULPT_VALIDATE=1.
pub(crate) fn live_validate_after_op(app: &AzraApp, ctx: &str) {
    if !live_validate_enabled() { return; }
    let k = spike_threshold();
    let (findings, worst) = scan_surface_spikes(app, k);
    if findings.is_empty() { return; }
    let f0 = &findings[0];
    eprintln!(
        "[SPIKE] {} finding(s), worst ratio {:.2} (k={k}) — first: chunk {} vert {} at ({:.4},{:.4},{:.4}) ratio {:.2} ring_n {} | op: {ctx}",
        findings.len(), worst, f0.chunk, f0.vert, f0.pos.x, f0.pos.y, f0.pos.z, f0.ratio, f0.ring_n
    );
    // Machine-usable record for the harness (`surface_fuzz.rs` replays `op:` lines).
    let dir = std::env::temp_dir().join("azra_sculpt_validate");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("spikes_{}.log", std::process::id()));
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "spike\tworst={worst:.3}\tk={k}\tn={}\tchunk={}\tvert={}\tpos=({:.6},{:.6},{:.6})\tratio={:.3}\top={ctx}",
            findings.len(), f0.chunk, f0.vert, f0.pos.x, f0.pos.y, f0.pos.z, f0.ratio
        );
        eprintln!("[SPIKE] record appended to {}", path.display());
    }
}
