// app_gpu — GPU worker infrastructure (job dispatcher, timestamps, worker enumeration)
// Extracted from the app crate for faster incremental compilation.

use eframe::wgpu;

/// Secondary (worker) GPU context for background compute offloading.
#[allow(dead_code)]
pub struct WorkerGpu {
    pub device: std::sync::Arc<wgpu::Device>,
    pub queue: std::sync::Arc<wgpu::Queue>,
    pub adapter_name: String,
    pub device_type: String,
    pub enabled: bool,
}

pub mod gpu_worker;
pub mod gpu_jobs;
pub mod gpu_timestamps;
pub mod native_rt;
