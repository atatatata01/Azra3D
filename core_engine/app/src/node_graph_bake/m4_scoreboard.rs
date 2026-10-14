//! M4 GPU-bake coverage scoreboard (session counters).
//!
//! Pure metrics for the >95% coverage scoreboard. Relocated intact from the
//! former monolithic node_graph_bake.rs — no logic change.

/// M1.a session counters — GPU path taken vs CPU fallback (reject / dispatch fail).
/// Pure metrics for the >95 % coverage scoreboard; reset only in tests.
static GPU_BAKE_OK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GPU_BAKE_REJECT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GPU_BAKE_DISPATCH_FAIL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Snapshot `(ok, reject, dispatch_fail)` for scoreboard / tests.
pub(crate) fn gpu_bake_coverage_counts() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        GPU_BAKE_OK.load(Relaxed),
        GPU_BAKE_REJECT.load(Relaxed),
        GPU_BAKE_DISPATCH_FAIL.load(Relaxed),
    )
}

#[cfg(test)]
pub(crate) fn gpu_bake_coverage_reset_for_test() {
    use std::sync::atomic::Ordering::Relaxed;
    GPU_BAKE_OK.store(0, Relaxed);
    GPU_BAKE_REJECT.store(0, Relaxed);
    GPU_BAKE_DISPATCH_FAIL.store(0, Relaxed);
}

pub(super) fn gpu_bake_record_ok() {
    GPU_BAKE_OK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn gpu_bake_record_reject(reason: &str) {
    GPU_BAKE_REJECT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Always name the blocker (T2) + running coverage so real editing is measurable.
    let (ok, rej, fail) = gpu_bake_coverage_counts();
    let total = ok + rej + fail;
    let pct = if total == 0 {
        0.0
    } else {
        100.0 * ok as f64 / total as f64
    };
    eprintln!(
        "[GpuBake] CPU fallback — unsupported node in bake scope: {reason} \
         (whitelist rejects bake) | coverage ok={ok} reject={rej} fail={fail} gpu%={pct:.1}"
    );
}

pub(super) fn gpu_bake_record_dispatch_fail() {
    GPU_BAKE_DISPATCH_FAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
