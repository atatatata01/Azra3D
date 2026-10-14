// core_engine/app/src/app_state.rs
//
// Slice 29 (app-crate de-god, PURE RELOCATION from main.rs): standalone application-state
// data types that are NOT methods on FourthLayerApp/AzraApp — GpuInfo (captured at init),
// InteractionEpisode (pipeline/resolution latch), and DiagState (delta-print diagnostics).

use crate::RenderMode;

// ==================================================================
// GPU Info (captured at init)
// ==================================================================
#[derive(Clone, Default)]
pub(crate) struct GpuInfo {
    pub(crate) adapter_name: String,
    pub(crate) backend: String,
    pub(crate) driver: String,
    pub(crate) device_type: String, // Discrete, Integrated, Virtual, Cpu
    pub(crate) vram_bytes: u64,     // dedicated_video_memory from adapter info
}

// ==================================================================
// Interaction Episode — latches pipeline + resolution at episode start
// to eliminate mode-switch jitter during interaction.
// ==================================================================
pub(crate) struct InteractionEpisode {
    pub active: bool,
    pub render_mode_latch: RenderMode,
    pub use_half_res: bool,
    pub start_frame: u64,
    pub cooldown_remaining: u32,
    // Jitter detection counters
    pub path_switch_count: u32,
    pub halfres_transition_count: u32,
    pub prev_pipeline_label: &'static str,
    pub prev_half_res: bool,
}

impl Default for InteractionEpisode {
    fn default() -> Self {
        Self {
            active: false,
            render_mode_latch: RenderMode::AnalyticalLite,
            use_half_res: false,
            start_frame: 0,
            cooldown_remaining: 0,
            path_switch_count: 0,
            halfres_transition_count: 0,
            prev_pipeline_label: "raymarch",
            prev_half_res: false,
        }
    }
}

impl InteractionEpisode {
    pub(crate) fn begin(
        &mut self,
        frame: u64,
        render_mode: RenderMode,
        pipeline_label: &'static str,
        half_res: bool,
    ) {
        assert!(!self.active, "Cannot relatch an active interaction episode");
        self.render_mode_latch = render_mode;
        self.prev_pipeline_label = pipeline_label;
        self.use_half_res = half_res;
        self.prev_half_res = half_res;
        self.start_frame = frame;
        self.cooldown_remaining = 0;
        self.active = true;
    }
}

#[cfg(test)]
mod interaction_episode_tests {
    use super::*;

    #[test]
    fn half_resolution_baseline_changes_only_when_starting_an_episode() {
        let mut episode = InteractionEpisode::default();
        for (frame, half_res) in [true, false, true].into_iter().enumerate() {
            episode.begin(frame as u64, RenderMode::AnalyticalLite, "raymarch_lite", half_res);
            assert!(episode.active);
            assert_eq!(episode.prev_half_res, episode.use_half_res);
            assert_eq!(episode.use_half_res, half_res);
            assert_eq!(episode.prev_pipeline_label, "raymarch_lite");
            assert_eq!(episode.start_frame, frame as u64);
            assert_eq!(episode.cooldown_remaining, 0);
            assert_eq!(episode.path_switch_count, 0);
            assert_eq!(episode.halfres_transition_count, 0);
            episode.active = false;
            episode.cooldown_remaining = 8;
        }
    }

    #[test]
    #[should_panic(expected = "Cannot relatch an active interaction episode")]
    fn an_active_episode_cannot_reset_its_safety_baseline() {
        let mut episode = InteractionEpisode::default();
        episode.begin(1, RenderMode::AnalyticalLite, "raymarch_lite", false);
        episode.begin(2, RenderMode::AnalyticalLite, "raymarch_lite", true);
    }
}

// ==================================================================
// Diagnostic state for delta-only prints (flicker detection)
// ==================================================================
#[derive(Default, Clone, PartialEq)]
pub(crate) struct DiagState {
    pub needs_prim_upload: bool,
    pub prim_buf_stale: bool,
    pub is_gizmo: bool,
    pub use_lite: bool,
    pub nv: u32,
    pub interaction: bool,
    pub pipeline: &'static str,
}
