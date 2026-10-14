use app_gpu::native_rt::{NativeRtBuildBudget, NativeRtDispatch, NativeRtFrame, NativeRtStatus};
use eframe::wgpu;
use std::ops::{Deref, DerefMut};

/// `AZRA_PT_SOFTWARE_TRAVERSAL=1` keeps PT on the software BVH even when hardware ray
/// queries are ready — the A/B switch for native-vs-software parity captures. Read once.
pub(crate) fn software_traversal_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| std::env::var("AZRA_PT_SOFTWARE_TRAVERSAL").is_ok_and(|v| v == "1"))
}

/// Scheduler-admitted acceleration-structure build budget for this frame. The default
/// (2 BLAS, 1M triangles) is bounded frame work. A single mesh above it is never
/// truncated and never deferred forever: it is admitted as `IdleOnly` work and builds
/// alone on the first idle frame with headroom.
pub(crate) fn build_budget(
    status: NativeRtStatus,
    scheduler: &mut crate::frame_scheduler::FrameScheduler,
) -> NativeRtBuildBudget {
    let default = NativeRtBuildBudget::default();
    let NativeRtStatus::Building { next_blas_triangles, .. } = status else { return default };
    if u64::from(next_blas_triangles) <= default.max_triangles {
        return default;
    }
    let ticket = crate::frame_scheduler::WorkTicket {
        class: crate::frame_scheduler::WorkClass::IdleOnly,
        cost_hint_ms: 4.0,
        name: "native_rt_oversized_blas",
    };
    if scheduler.can_run(&ticket) {
        scheduler.record(&ticket, ticket.cost_hint_ms);
        NativeRtBuildBudget { max_blas: 1, max_triangles: u64::from(next_blas_triangles), ..default }
    } else {
        scheduler.record_skipped(&ticket);
        default
    }
}

pub(crate) enum PtFrameEncoder {
    Software(wgpu::CommandEncoder),
    Native(NativeRtFrame),
}

impl PtFrameEncoder {
    pub(crate) fn prepare(gpu: &crate::GpuCtx, budget: NativeRtBuildBudget) -> Self {
        let encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        match (&gpu.native_rt, &gpu.pipelines.native_rt_pipelines) {
            (Some(renderer), Some(pipelines)) => match renderer.prepare(&gpu.device, encoder, pipelines, budget) {
                Ok(frame) => Self::Native(frame),
                Err(error) => {
                    eprintln!("[RTX] Scene build rejected: {error}");
                    Self::Software(gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None }))
                }
            },
            _ => Self::Software(encoder),
        }
    }

    pub(crate) fn status(&self) -> NativeRtStatus {
        match self {
            Self::Native(frame) => frame.status(),
            Self::Software(_) => NativeRtStatus::Empty,
        }
    }

    pub(crate) fn dispatch_lease(&self) -> Option<NativeRtDispatch> {
        match self {
            Self::Native(frame) => frame.dispatch_lease(),
            Self::Software(_) => None,
        }
    }

    pub(crate) fn submit(self, gpu: &mut crate::GpuCtx) {
        match self {
            Self::Software(encoder) => { gpu.queue.submit([encoder.finish()]); }
            Self::Native(frame) => {
                let submitted = frame.submit(&gpu.queue);
                let committed = gpu.native_rt.as_mut().expect("native frame owner exists").commit(submitted);
                assert!(committed, "native scene changed between frame preparation and submission");
            }
        }
    }
}

impl Deref for PtFrameEncoder {
    type Target = wgpu::CommandEncoder;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Software(encoder) => encoder,
            Self::Native(frame) => frame.encoder(),
        }
    }
}

impl DerefMut for PtFrameEncoder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Software(encoder) => encoder,
            Self::Native(frame) => frame.encoder_mut(),
        }
    }
}
