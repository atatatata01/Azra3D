//! Process-wide GPU test serialization.
//!
//! **Why:** on Windows, concurrent `Instance::request_adapter` / `request_device` from multiple
//! unit tests in the same `app` binary aborts the process (`exit 0xffffffff`). Owner isolation
//! (2026-07-30): `dc_baseline` alone 5/5, `sdf_gpu_mesh` alone 32/32, together → abort. Only those
//! modules create devices. Same class as O6 (`GPU_MATERIAL_BAKE_TEST_LOCK`) — material bake was
//! locked; SDF/DC + Gate0 were not, and they race each other under default `cargo test` threads.
//!
//! **Rule:** every headless GPU test holds [`GpuTestLock`] for the **entire** test body (not only
//! device create). Acquire via [`headless_device`] / [`require_adapter`]; drop ends the critical
//! section. Nested acquire on the same thread deadlocks — do not call these while already holding.

use eframe::wgpu;
use std::sync::{Mutex, MutexGuard};

static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

/// RAII ticket: while alive, no other GPU test in this process runs.
pub(crate) struct GpuTestLock(MutexGuard<'static, ()>);

fn acquire() -> GpuTestLock {
    GpuTestLock(GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Device limits for SDF/DC (large buffers) + optional TIMESTAMP_QUERY.
fn create_device(label: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    })).ok()?;
    let al = adapter.limits();
    let mut limits = wgpu::Limits::default();
    limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
    limits.max_buffer_size = al.max_buffer_size;
    let mut features = wgpu::Features::empty();
    if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        features |= wgpu::Features::TIMESTAMP_QUERY;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: features,
            required_limits: limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .ok()?;
    Some((device, queue))
}

/// Soft-fail headless device (no adapter → `None`, test skips). Holds [`GpuTestLock`] until drop.
///
/// Return order: `(lock, device, queue)` — bind the lock to a local so it outlives GPU work:
/// `let (_gpu_lock, device, queue) = headless_device()?;`
pub(crate) fn headless_device() -> Option<(GpuTestLock, wgpu::Device, wgpu::Queue)> {
    let lock = acquire();
    let (device, queue) = create_device("app_gpu_test")?;
    Some((lock, device, queue))
}

/// Hard-fail headless device (Gate-0). Panics if no adapter. Holds lock until drop.
pub(crate) fn require_adapter() -> (GpuTestLock, wgpu::Device, wgpu::Queue) {
    let lock = acquire();
    let (device, queue) = create_device("app_gpu_test_gate0").expect(
        "GATE 0: a GPU adapter is REQUIRED for this test but none is present. This test proves \
         NOTHING without a device — headless CI must run Gate-0 GPU tests on real hardware. (The ~40 \
         legacy SDF/DC parity tests silently skip here; this assertion is the plan's fix for that hole.)",
    );
    (lock, device, queue)
}

#[cfg(test)]
mod tests {
    /// GATE — concurrent headless_device from N threads must not abort the process.
    /// Pre-lock: Windows + default cargo threads → `exit 0xffffffff` when dc_baseline and
    /// sdf_gpu_mesh both `request_device`. Lock serializes create+use for the guard lifetime.
    #[test]
    fn concurrent_headless_device_does_not_abort() {
        const N: usize = 4;
        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            handles.push(std::thread::spawn(move || {
                let Some((_lock, device, queue)) = super::headless_device() else {
                    eprintln!("[gpu_test_harness] no adapter — skip concurrent probe (thread {i})");
                    return Ok::<(), String>(());
                };
                // Touch the device so the lock covers real GPU use, not only create.
                let _ = device.features();
                let _ = queue;
                Ok(())
            }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => panic!("thread {i} GPU error: {e}"),
                Err(_) => panic!("thread {i} panicked — concurrent GPU create is still unsafe"),
            }
        }
    }
}
