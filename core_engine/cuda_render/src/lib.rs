//! CUDA-accelerated rendering backend for AZRA SDF engine.
//!
//! Feature-gated: compile with `--features cuda` to enable.
//! Without the feature, this crate provides only stub functions that report
//! CUDA as unavailable.

#[cfg(feature = "cuda")]
mod device;
#[cfg(feature = "cuda")]
mod buffers;
#[cfg(feature = "cuda")]
mod dispatch;

#[cfg(feature = "optix")]
mod optix_sys;
#[cfg(feature = "optix")]
mod optix_accel;
#[cfg(feature = "optix")]
mod optix_pipeline;
#[cfg(feature = "optix")]
mod optix_tri_pipeline;
mod triangle_query;
mod native_rt_shader;
#[cfg(all(test, feature = "optix"))]
mod triangle_query_gpu_tests;

pub use triangle_query::{
    TriangleHit, TriangleIdentity, TriangleRay, OPTIX_MATERIAL_PARITY_GAP,
    OPTIX_MATERIAL_PARITY_READY,
};
pub use native_rt_shader::{
    build_native_rt_wgsl, NATIVE_RT_ACCELERATION_BINDING, NATIVE_RT_ACCELERATION_GROUP,
};

#[cfg(feature = "cuda")]
pub use device::CudaContext;
#[cfg(feature = "cuda")]
pub use buffers::CudaBufferSet;
#[cfg(feature = "cuda")]
pub use dispatch::CudaRenderer;
#[cfg(feature = "optix")]
pub use optix_accel::HwRtMaterial;

/// Check if CUDA is available at runtime.
///
/// Without the `cuda` feature, this always returns false.
/// With the feature, it attempts to initialize the CUDA driver.
pub fn cuda_available() -> bool {
    #[cfg(feature = "cuda")]
    {
        device::CudaContext::probe()
    }
    #[cfg(not(feature = "cuda"))]
    {
        false
    }
}

/// CUDA backend selection (PTX compute vs OptiX RT cores).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CudaBackend {
    /// Pure compute kernels via cudarc (works on GTX + RTX).
    Ptx,
    /// OptiX hardware ray tracing (RTX only, falls back to Ptx on GTX).
    OptiX,
}

impl Default for CudaBackend {
    fn default() -> Self {
        Self::Ptx
    }
}

/// Render backend indicator for UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderBackend {
    Wgsl,
    CudaPtx,
    CudaOptix,
}

impl RenderBackend {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Wgsl => "WGSL",
            Self::CudaPtx => "CUDA",
            Self::CudaOptix => "OptiX",
        }
    }
}
