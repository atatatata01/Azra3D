//! CUDA device initialization and PTX module management.

use cudarc::driver::{CudaDevice, CudaFunction};
use cudarc::nvrtc::Ptx;
use std::collections::HashSet;
use std::sync::Arc;

/// Module names → kernel function names for registration.
const MODULE_FUNCS: &[(&str, &[&str])] = &[
    ("raymarch_gbuf", &["raymarch_gbuf_kernel"]),
    ("deferred_shade", &["deferred_shade_kernel"]),
    ("pathtrace", &["pathtrace_kernel"]),
    ("bloom", &["bloom_extract_kernel", "bloom_blur_kernel"]),
    ("ssr", &["ssr_kernel"]),
    ("fog", &["fog_kernel"]),
    ("tonemap", &["tonemap_kernel"]),
    ("upsample", &["upsample_kernel"]),
];

/// Core CUDA context: owns the device handle and tracks loaded modules.
pub struct CudaContext {
    pub device: Arc<CudaDevice>,
    pub compute_capability: (u32, u32),
    loaded_modules: HashSet<String>,
}

impl CudaContext {
    /// Quick probe: can we initialize CUDA at all?
    pub fn probe() -> bool {
        CudaDevice::new(0).is_ok()
    }

    /// Initialize CUDA on device 0, load all available PTX modules.
    pub fn init() -> Result<Self, String> {
        let device = CudaDevice::new(0)
            .map_err(|e| format!("CUDA device init failed: {}", e))?;

        let (major, minor) = Self::query_compute_cap(&device);

        let mut ctx = Self {
            device,
            compute_capability: (major, minor),
            loaded_modules: HashSet::new(),
        };

        ctx.load_ptx_modules();
        Ok(ctx)
    }

    /// Returns true if this GPU has RTX cores (compute capability >= 7.5).
    pub fn is_rtx(&self) -> bool {
        let (major, minor) = self.compute_capability;
        major > 7 || (major == 7 && minor >= 5)
    }

    /// GPU name for UI display.
    pub fn device_name(&self) -> String {
        format!(
            "CUDA Device (SM {}.{}{})",
            self.compute_capability.0,
            self.compute_capability.1,
            if self.is_rtx() { " RTX" } else { "" }
        )
    }

    /// Get a kernel function from a loaded module.
    pub fn get_func(&self, module_name: &str, func_name: &str) -> Option<CudaFunction> {
        if !self.loaded_modules.contains(module_name) {
            return None;
        }
        self.device.get_func(module_name, func_name)
    }

    fn query_compute_cap(device: &Arc<CudaDevice>) -> (u32, u32) {
        use cudarc::driver::sys;
        let mut major: i32 = 0;
        let mut minor: i32 = 0;
        unsafe {
            let dev = *device.cu_device();
            sys::lib().cuDeviceGetAttribute(
                &mut major,
                sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR,
                dev,
            );
            sys::lib().cuDeviceGetAttribute(
                &mut minor,
                sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR,
                dev,
            );
        }
        (major as u32, minor as u32)
    }

    fn load_ptx_modules(&mut self) {
        let out_dir = env!("OUT_DIR", "OUT_DIR not set — run via cargo build");

        for &(name, func_names) in MODULE_FUNCS {
            let ptx_path = format!("{}/{}.ptx", out_dir, name);
            let ptx_text = match std::fs::read_to_string(&ptx_path) {
                Ok(text) => text,
                Err(_) => continue, // PTX not yet compiled — expected during incremental development
            };

            match self.device.load_ptx(
                Ptx::from_src(ptx_text),
                name,
                func_names,
            ) {
                Ok(()) => {
                    self.loaded_modules.insert(name.to_string());
                    eprintln!("[cuda_render] Loaded module '{}' ({} functions)", name, func_names.len());
                }
                Err(e) => {
                    eprintln!("[cuda_render] Failed to load PTX module '{}': {}", name, e);
                }
            }
        }
    }
}
