//! Tracy profiling integration — zero-cost when `profiling` feature is off.
//!
//! Usage:
//!   profile_scope!("name")       — marks a scope for Tracy
//!   profile_function!()          — marks the enclosing function for Tracy
//!   profile_init()               — call once at startup to initialize the Tracy subscriber

/// App-start clock for the `perf_stamp()` prefix. Set on first use; `profile_init()`
/// pins it at startup so `t=` matches process age rather than first-stall age.
static APP_T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Seconds since app start (monotonic).
pub(crate) fn app_secs() -> f64 {
    APP_T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Shared correlation prefix for perf log lines: `f=<frame> t=<secs>s`.
/// Frame number is `AzraApp::diag_frame` via the `vis_recorder` mirror, so lines printed
/// from background threads (BLAS builds, bake readbacks) carry the same numbering as
/// frame-thread lines. Grep tags (`[STALL]`, `[LIB-BODY]`, …) stay unchanged — the stamp
/// is appended inside the line, never in the tag.
pub(crate) fn perf_stamp() -> String {
    format!("f={} t={:.2}s", crate::vis_recorder::vis_frame(), app_secs())
}

/// Starvation sentinel (FRAME_STALL_PLAN): time a FIXED pure-CPU workload — no
/// allocation, no egui, no locks. Normally ~0.02–0.1ms. If this inflates 10–100× on a
/// stall frame, the UI thread was preempted/starved and every pane-level timer that
/// frame is measuring the starvation, not its own code (the moving-culprit pattern:
/// seam → items → folder_col across three sessions). If it stays flat, the measured
/// closure really is slow. One number ends that debate per frame.
pub(crate) fn sentinel_ms() -> f32 {
    let t = std::time::Instant::now();
    let mut acc: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in 0..64_000u64 {
        acc = acc
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(std::hint::black_box(i));
    }
    std::hint::black_box(acc);
    t.elapsed().as_secs_f32() * 1000.0
}

/// Initialize the Tracy tracing subscriber. Call once at app start.
/// No-op when the `profiling` feature is off.
pub(crate) fn profile_init() {
    let _ = APP_T0.get_or_init(std::time::Instant::now);
    #[cfg(feature = "profiling")]
    {
        use tracing_tracy::TracyLayer;
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry().with(TracyLayer::default());
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set Tracy tracing subscriber");
        eprintln!("[PROFILING] Tracy subscriber initialized — connect Tracy profiler to inspect.");
    }
}

/// Mark a named scope for Tracy. Zero-cost when feature is off.
#[cfg(feature = "profiling")]
macro_rules! profile_scope {
    ($name:expr) => {
        let _span = tracing::info_span!($name).entered();
    };
}

#[cfg(not(feature = "profiling"))]
macro_rules! profile_scope {
    ($name:expr) => {};
}

/// Mark the current function for Tracy. Zero-cost when feature is off.
#[cfg(feature = "profiling")]
macro_rules! profile_function {
    () => {
        // function_name!() is not stable, so we use a fixed label — callers should prefer profile_scope!
        let _span = tracing::info_span!("fn").entered();
    };
}

#[cfg(not(feature = "profiling"))]
#[allow(unused_macros)]
macro_rules! profile_function {
    () => {};
}

/// Spike detection: prints [SPIKE] if a block takes longer than 10ms.
/// Usage: `time_block!("name", { ... })` — returns the block's value.
macro_rules! time_block {
    ($name:expr, $body:expr) => {{
        let _tb_t = std::time::Instant::now();
        let _tb_result = $body;
        let _tb_ms = _tb_t.elapsed().as_secs_f64() * 1000.0;
        if _tb_ms > 10.0 {
            eprintln!("[SPIKE] {}: {:.1}ms | {}", $name, _tb_ms, crate::profiling::perf_stamp());
        }
        _tb_result
    }};
}

// Macros are available crate-wide via `#[macro_use] mod profiling;` in main.rs
