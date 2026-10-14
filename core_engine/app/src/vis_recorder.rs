//! Visual-bug FLIGHT RECORDER — a global ring buffer of render-pipeline decision events.
//!
//! The Object-Mode SDF pipeline (GPU bake → rv.buffer → GPU DC mesh → draw) has TRANSIENT
//! visual bugs (surface vanishes, trailing-cube ghost, inverted bake) that live for 1-N frames
//! — too short for console spam to catch. Instrumented decision points record one compact
//! event each into this ring; when a glitch is seen (or an invariant check fires), the ring +
//! a state snapshot are dumped to `%LOCALAPPDATA%/Azra/visdump_{seq}.log`.
//!
//! Recording is enabled ONLY when the process starts with `AZRA_VISLOG=1` (checked once via
//! OnceLock). When disabled, `visrec` is a no-op that never formats the message (the closure
//! defers the format cost) — a single cached bool check per call site, zero per-frame cost.
//!
//! Frame numbering REUSES the existing `AzraApp::diag_frame` counter (incremented once per
//! frame in app_update.rs). `set_vis_frame` mirrors it into a static so BodyVolumeCache
//! internals — which have no access to the app struct — stamp events with the same numbers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Fixed ring capacity — a few seconds of pipeline events at edit rates.
const RING_CAPACITY: usize = 4096;

/// Auto-dump rate limit: at most one auto-dump per this many frames (diagnostic
/// throttle only — never a correctness mechanism).
const AUTODUMP_MIN_FRAMES: u64 = 300;

/// Mirror of `AzraApp::diag_frame` (the existing per-frame counter). See module doc.
pub(crate) static VIS_FRAME: AtomicU64 = AtomicU64::new(0);

/// Sync the recorder's frame stamp from `diag_frame`. Called once per frame (app_update.rs).
pub(crate) fn set_vis_frame(frame: u64) {
    VIS_FRAME.store(frame, Ordering::Relaxed);
}

/// Current frame number (mirrors `diag_frame`). Used by call sites inside BodyVolumeCache
/// that cannot read the app struct.
pub(crate) fn vis_frame() -> u64 {
    VIS_FRAME.load(Ordering::Relaxed)
}

/// Session count of UNCAPTURED wgpu errors (validation / OOM / device-loss), incremented by
/// the `on_uncaptured_error` handler (app_update.rs). Any nonzero value means a GPU dispatch
/// silently failed — the AZRA_AUTOREPRO probes assert on it (a broken mesh dispatch is
/// invisible to CPU-side cache probes; this is the observable).
pub(crate) static GPU_UNCAPTURED_ERRORS: AtomicU64 = AtomicU64::new(0);

/// True iff the process was started with `AZRA_VISLOG=1`. Checked once, cached forever.
pub(crate) fn vis_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("AZRA_VISLOG").map_or(false, |v| v == "1"))
}

struct VisEntry {
    frame: u64,
    label: &'static str,
    msg: String,
}

fn ring() -> &'static Mutex<VecDeque<VisEntry>> {
    static RING: OnceLock<Mutex<VecDeque<VisEntry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAPACITY)))
}

/// Record one event. `msg` is a closure so the format cost is paid ONLY when recording is
/// enabled — when `AZRA_VISLOG` is off this is a single cached-bool check and returns.
pub(crate) fn visrec(frame: u64, label: &'static str, msg: impl FnOnce() -> String) {
    if !vis_enabled() {
        return;
    }
    let msg = msg(); // format outside the lock
    let mut r = ring().lock().unwrap_or_else(|e| e.into_inner());
    if r.len() >= RING_CAPACITY {
        r.pop_front();
    }
    r.push_back(VisEntry { frame, label, msg });
}

/// Rate-limit gate for the auto-dump (Part D): true at most once per
/// `AUTODUMP_MIN_FRAMES` frames. Diagnostics-only throttle.
pub(crate) fn should_autodump(frame: u64) -> bool {
    // u64::MAX sentinel = never dumped yet (so frame 0 can dump).
    static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
    let last = LAST.load(Ordering::Relaxed);
    if last != u64::MAX && frame.saturating_sub(last) < AUTODUMP_MIN_FRAMES {
        return false;
    }
    LAST.store(frame, Ordering::Relaxed);
    true
}

/// Directory for dump files: `%LOCALAPPDATA%/Azra/` — the same location pattern as
/// `gpu_init::pipeline_cache_path()` / `project::default_scene_path()`. Never a
/// hardcoded user path.
fn dump_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Azra");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Next dump sequence number. Seeded once from existing `visdump_N.log` files so a
/// restarted session never overwrites a previous session's dumps.
fn next_seq() -> u64 {
    static SEQ: OnceLock<AtomicU64> = OnceLock::new();
    let seq = SEQ.get_or_init(|| {
        let start = dump_dir()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        name.strip_prefix("visdump_")?
                            .strip_suffix(".log")?
                            .parse::<u64>()
                            .ok()
                    })
                    .max()
                    .map_or(0, |m| m + 1)
            })
            .unwrap_or(0);
        AtomicU64::new(start)
    });
    seq.fetch_add(1, Ordering::Relaxed)
}

/// Write the state snapshot + the event ring (oldest first) to
/// `%LOCALAPPDATA%/Azra/visdump_{seq}.log`. Returns the path on success.
///
/// Works even when `AZRA_VISLOG` is off (the ring is just empty) so the Developer
/// Tweaks button always produces the state snapshot.
pub(crate) fn dump_to_file(extra_state: &str) -> Option<std::path::PathBuf> {
    use std::fmt::Write as _;
    let dir = dump_dir()?;
    let path = dir.join(format!("visdump_{}.log", next_seq()));

    let mut out = String::new();
    let epoch_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(out, "==== AZRA VISUAL-STATE DUMP ====");
    let _ = writeln!(out, "frame={} unix_time={}", vis_frame(), epoch_s);
    let _ = writeln!(
        out,
        "AZRA_VISLOG={}",
        if vis_enabled() {
            "1 (event ring active)"
        } else {
            "off (ring empty — set AZRA_VISLOG=1 and restart to record events)"
        }
    );
    let _ = writeln!(out, "\n==== STATE SNAPSHOT ====");
    out.push_str(extra_state);
    if !extra_state.ends_with('\n') {
        out.push('\n');
    }
    let _ = writeln!(out, "\n==== EVENT RING (oldest first) ====");
    {
        let r = ring().lock().unwrap_or_else(|e| e.into_inner());
        for e in r.iter() {
            let _ = writeln!(out, "[f{:>8}] {:<14} {}", e.frame, e.label, e.msg);
        }
        let _ = writeln!(out, "({} events)", r.len());
    }

    std::fs::write(&path, out.as_bytes()).ok()?;
    Some(path)
}
