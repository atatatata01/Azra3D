//! Secondary (worker) GPU enumeration and initialization.
//!
//! At startup, enumerates all GPU adapters. Every adapter that is NOT the
//! primary (viewport) GPU is opened as a compute-only worker device.
//! Supports multiple workers (e.g. integrated GPU + older discrete).
//!
//! Single-GPU machines: returns an empty Vec.

use eframe::wgpu;
use crate::WorkerGpu;

/// Enumerate all GPU adapters and open every non-primary one as a worker.
///
/// `primary_backend` is the backend string from the primary GPU (e.g. "Vulkan").
/// We enumerate ONLY on the OTHER backend to avoid creating a second Vulkan/DX12
/// instance that conflicts with the primary and causes device loss.
///
/// On a single-GPU machine this yields zero workers — the rayon CPU path handles
/// background compute. On multi-GPU machines, secondary discrete GPUs appear on
/// the non-primary backend and get opened as workers.
pub fn enumerate_and_open_workers(primary_backend: &str) -> Vec<WorkerGpu> {
    // Only enumerate on the backend the primary is NOT using.
    // Creating a second wgpu Instance on the same backend (especially Vulkan)
    // can destabilize the primary device and cause "Parent device is lost".
    let worker_backends = if primary_backend.contains("Vulkan") {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN
    };

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: worker_backends,
        ..Default::default()
    });

    let all_adapters: Vec<wgpu::Adapter> = instance.enumerate_adapters(worker_backends);

    if all_adapters.is_empty() {
        eprintln!("[WorkerGPU] No adapters found");
        return Vec::new();
    }

    // The primary is the first DiscreteGpu (matching eframe's HighPerformance preference).
    // If none is discrete, the first adapter is primary.
    let primary_name = all_adapters.iter()
        .find(|a| matches!(a.get_info().device_type, wgpu::DeviceType::DiscreteGpu))
        .or(all_adapters.first())
        .map(|a| a.get_info().name.clone())
        .unwrap_or_default();

    let mut workers = Vec::new();

    for adapter in &all_adapters {
        let info = adapter.get_info();
        // Skip ALL adapters matching the primary GPU name — the same physical
        // GPU can appear on multiple backends (Vulkan + DX12). Opening it on a
        // second backend while the primary already holds it causes device loss.
        let is_primary = info.name == primary_name;

        if is_primary { continue; }

        // Skip software/virtual adapters — useless for GPU compute
        if matches!(info.device_type, wgpu::DeviceType::Cpu | wgpu::DeviceType::Other)
            || info.name.to_lowercase().contains("basic render")
        {
            continue;
        }

        // Open this adapter as a compute-only worker
        let device_future = adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Azra Worker GPU"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            },
        );

        match pollster::block_on(device_future) {
            Ok((device, queue)) => {
                workers.push(WorkerGpu {
                    device: std::sync::Arc::new(device),
                    queue: std::sync::Arc::new(queue),
                    adapter_name: info.name.clone(),
                    device_type: format!("{:?}", info.device_type),
                    enabled: true,
                });
            }
            Err(e) => {
                eprintln!(
                    "[WorkerGPU] Failed to open {}: {}",
                    info.name, e
                );
            }
        }
    }

    if workers.is_empty() {
        eprintln!("[WorkerGPU] No worker GPUs available — all work on primary");
    } else {
        eprintln!("[WorkerGPU] {} worker GPU(s) ready for background compute", workers.len());
    }

    workers
}
