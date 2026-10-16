//! Crash-safety for a Rust panic while a wgpu device is alive.
//!
//! On Windows/NVIDIA, unwinding through wgpu drops the `Device` and `Queue`
//! into the driver (`vkDestroyDevice` / queue semaphore teardown) while
//! submissions and `map_async` mappings are still outstanding. The driver
//! waits inside that call and the process becomes unkillable. `pipeline_cache.bin`
//! is also rewritten from `App::drop` during that unwind (`get_data()` is a
//! driver call, and a short blob replaces the previous cache).
//!
//! The panic hook runs *before* unwind. For an uncaught panic it writes the
//! report and `process::exit`s, so those destructors never run. The OS reclaims
//! the Vulkan device on process teardown; we do not call back into the driver.
//!
//! Panics that existing `catch_unwind` sites already recover (invalid user
//! node-graph WGSL, library build, soak) go through [`catch_recoverable`],
//! which raises a thread-local depth. The hook does not exit those.

use std::cell::Cell;
use std::io::Write;
use std::panic::UnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by [`install_panic_hook`]. Tests that panic while dropping `FourthLayerApp`
/// must keep unwinding into the harness — they never install this hook.
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

std::thread_local! {
    static RECOVERABLE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// `true` when a panic on this thread should terminate the process.
/// `false` while inside [`catch_recoverable`].
pub(crate) fn panic_should_terminate() -> bool {
    RECOVERABLE_DEPTH.with(|d| d.get() == 0)
}

fn enter_recoverable() {
    RECOVERABLE_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
}

fn exit_recoverable() {
    RECOVERABLE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// Existing recovered panics. Same contract as `catch_unwind`.
///
/// The depth guard drops on every return, including after the inner panic is
/// caught, so a later uncaught panic still terminates.
pub(crate) fn catch_recoverable<R>(
    f: impl FnOnce() -> R + UnwindSafe,
) -> Result<R, Box<dyn std::any::Any + Send>> {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            exit_recoverable();
        }
    }
    enter_recoverable();
    let _guard = Guard;
    std::panic::catch_unwind(f)
}

/// Install the process panic hook. Call once from `gpu_init::run`, before the
/// window exists.
pub(crate) fn install_panic_hook() {
    HOOK_INSTALLED.store(true, Ordering::Relaxed);
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}\n{:?}", std::backtrace::Backtrace::force_capture());
        write_panic_report(&msg);
        eprintln!("PANIC: {info}");
        if !panic_should_terminate() {
            // A catch_recoverable frame is on this thread. Let it recover.
            // Do not touch the device and do not write the pipeline cache.
            return;
        }
        eprintln!(
            "[PANIC] uncaught — exiting without dropping the wgpu device or queue \
             and without writing pipeline_cache.bin (exit 101)"
        );
        let _ = std::io::stderr().flush();
        std::process::exit(101);
    }));
}

fn write_panic_report(msg: &str) {
    let path = std::env::temp_dir().join("fourthlayer_panic.txt");
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&path)?;
        file.write_all(msg.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write {
        eprintln!("[PANIC] could not write {}: {e}", path.display());
    }
}

/// `AZRA_TEST_PANIC=1` only. Unset in production, so this is a no-op.
///
/// Fires once the app constructor has created (or failed to create) the wgpu
/// device, before the event loop. The hook then exits the process.
pub(crate) fn maybe_deliberate_test_panic(device_live: bool) {
    match std::env::var("AZRA_TEST_PANIC") {
        Ok(v) if v == "1" => {}
        _ => return,
    }
    eprintln!(
        "[PANIC-TEST] AZRA_TEST_PANIC=1 deliberate panic (wgpu device live: {device_live}). \
         The panic hook exits the process without dropping the device or the queue \
         and without writing pipeline_cache.bin."
    );
    panic!("AZRA_TEST_PANIC");
}

/// Backstop if an uncaught panic unwinds into `FourthLayerApp` anyway
/// (the hook should have exited first). Must not call into wgpu.
pub(crate) fn exit_if_panicking() {
    if !std::thread::panicking() {
        return;
    }
    eprintln!(
        "[PANIC] unwind reached App::drop — skip pipeline cache write and exit \
         before the device and queue are dropped"
    );
    let _ = std::io::stderr().flush();
    if HOOK_INSTALLED.load(Ordering::Relaxed) {
        std::process::exit(101);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::AssertUnwindSafe;

    #[test]
    fn uncaught_depth_requests_termination() {
        assert!(panic_should_terminate());
    }

    #[test]
    fn recoverable_panic_is_caught_and_does_not_stick() {
        assert!(panic_should_terminate());
        let caught = catch_recoverable(AssertUnwindSafe(|| panic!("shader validation")));
        assert!(caught.is_err());
        assert!(panic_should_terminate());
    }

    #[test]
    fn nested_recoverable_stays_non_terminating_until_outer_returns() {
        catch_recoverable(AssertUnwindSafe(|| {
            assert!(!panic_should_terminate());
            let inner = catch_recoverable(AssertUnwindSafe(|| {
                assert!(!panic_should_terminate());
                7
            }));
            assert_eq!(inner.unwrap(), 7);
            assert!(!panic_should_terminate());
        }))
        .unwrap();
        assert!(panic_should_terminate());
    }
}
