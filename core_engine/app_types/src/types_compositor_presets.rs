// Compositor Presets — extracted from types.rs
use serde::{Serialize, Deserialize};
use crate::*;

// ==================================================================
// Compositor Presets (Sprint 10)
// ==================================================================

/// A named compositor preset — serialized to/from JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorPreset {
    pub name: String,
    pub category: String,
    pub stack: CompositorStack,
}

impl CompositorStack {
    /// Create built-in presets.
    pub fn builtin_presets() -> Vec<CompositorPreset> {
        vec![
            // ---- Film Look ----
            CompositorPreset {
                name: "ACES Filmic".into(), category: "Film Look".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0;
                    s.effects.push(CompositorEffect { bloom_threshold: 0.8, bloom_intensity: 0.5, bloom_radius: 25.0, ..CompositorEffect::new(CompositorEffectType::Bloom) });
                    s.effects.push(CompositorEffect { vignette_intensity: 0.3, vignette_radius: 0.9, vignette_softness: 0.6, ..CompositorEffect::new(CompositorEffectType::Vignette) });
                    s.effects.push(CompositorEffect { grade_contrast: 1.15, grade_saturation: 1.1, grade_balance_shadows: [0.85, 0.9, 1.1], ..CompositorEffect::new(CompositorEffectType::ColorGrading) });
                    s.effects.push(CompositorEffect { grain_intensity: 0.02, ..CompositorEffect::new(CompositorEffectType::FilmGrain) });
                    s
                },
            },
            CompositorPreset {
                name: "Warm Cinema".into(), category: "Film Look".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0; s.cm_white_balance_temp = 5500.0;
                    s.effects.push(CompositorEffect { bloom_threshold: 0.9, bloom_intensity: 0.4, bloom_tint: [1.0, 0.9, 0.7], ..CompositorEffect::new(CompositorEffectType::Bloom) });
                    s.effects.push(CompositorEffect { vignette_intensity: 0.5, vignette_color: [0.1, 0.05, 0.0], ..CompositorEffect::new(CompositorEffectType::Vignette) });
                    s.effects.push(CompositorEffect { grade_contrast: 1.2, grade_saturation: 0.9, grade_shadows: -0.05, grade_balance_highlights: [1.1, 1.0, 0.85], ..CompositorEffect::new(CompositorEffectType::ColorGrading) });
                    s
                },
            },
            CompositorPreset {
                name: "Noir High Contrast".into(), category: "Film Look".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0;
                    s.effects.push(CompositorEffect { vignette_intensity: 0.8, vignette_radius: 0.6, ..CompositorEffect::new(CompositorEffectType::Vignette) });
                    s.effects.push(CompositorEffect { grade_saturation: 0.0, grade_contrast: 1.5, grade_gamma: 0.85, ..CompositorEffect::new(CompositorEffectType::ColorGrading) });
                    s.effects.push(CompositorEffect { grain_intensity: 0.04, ..CompositorEffect::new(CompositorEffectType::FilmGrain) });
                    s
                },
            },
            // ---- Game Engine Match ----
            CompositorPreset {
                name: "Unreal Engine 5".into(), category: "Game Engine".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0; // ACES (formerly UE5, which was identical)
                    s.effects.push(CompositorEffect { bloom_threshold: 1.0, bloom_intensity: 0.6, bloom_radius: 30.0, ..CompositorEffect::new(CompositorEffectType::Bloom) });
                    s.effects.push(CompositorEffect { ca_intensity: 0.1, ca_mode: 0, ..CompositorEffect::new(CompositorEffectType::ChromaticAberration) });
                    s.effects.push(CompositorEffect { vignette_intensity: 0.2, ..CompositorEffect::new(CompositorEffectType::Vignette) });
                    s
                },
            },
            // ---- Stylized ----
            CompositorPreset {
                name: "Cel Shade + Outline".into(), category: "Stylized".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 5;
                    s.effects.push(CompositorEffect { outline_source: 0, outline_width: 2.0, outline_fill_mode: 2, outline_cel_steps: 4, ..CompositorEffect::new(CompositorEffectType::Outline) });
                    s
                },
            },
            CompositorPreset {
                name: "Synthwave Neon".into(), category: "Stylized".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0; s.cm_exposure = 0.3;
                    s.effects.push(CompositorEffect { bloom_threshold: 0.6, bloom_intensity: 1.5, bloom_tint: [0.8, 0.3, 1.0], ..CompositorEffect::new(CompositorEffectType::Bloom) });
                    s.effects.push(CompositorEffect { ca_intensity: 0.5, ..CompositorEffect::new(CompositorEffectType::ChromaticAberration) });
                    s.effects.push(CompositorEffect { grade_saturation: 1.5, grade_hue_shift: 15.0, grade_balance_shadows: [0.5, 0.2, 1.0], grade_balance_highlights: [1.0, 0.5, 0.8], ..CompositorEffect::new(CompositorEffectType::ColorGrading) });
                    s.effects.push(CompositorEffect { grain_intensity: 0.03, ..CompositorEffect::new(CompositorEffectType::FilmGrain) });
                    s
                },
            },
            // ---- Technical ----
            CompositorPreset {
                name: "Clean Preview".into(), category: "Technical".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0;
                    s.effects.push(CompositorEffect { sharpen_strength: 0.5, sharpen_radius: 1.0, ..CompositorEffect::new(CompositorEffectType::Sharpen) });
                    s
                },
            },
            // ---- Photography ----
            CompositorPreset {
                name: "Portrait Soft".into(), category: "Photography".into(),
                stack: {
                    let mut s = Self::default(); s.enabled = true; s.tonemapper = 0;
                    s.effects.push(CompositorEffect { bloom_threshold: 1.2, bloom_intensity: 0.3, ..CompositorEffect::new(CompositorEffectType::Bloom) });
                    s.effects.push(CompositorEffect { dof_focus_dist: 3.0, dof_aperture: 1.8, dof_max_blur: 15.0, ..CompositorEffect::new(CompositorEffectType::DepthOfField) });
                    s.effects.push(CompositorEffect { vignette_intensity: 0.25, ..CompositorEffect::new(CompositorEffectType::Vignette) });
                    s.effects.push(CompositorEffect { grade_vibrance: 1.2, grade_contrast: 1.05, grade_balance_highlights: [1.05, 1.0, 0.95], ..CompositorEffect::new(CompositorEffectType::ColorGrading) });
                    s
                },
            },
        ]
    }

    /// Save to JSON file.
    pub fn save_preset(&self, name: &str, category: &str, path: &std::path::Path) -> std::io::Result<()> {
        let preset = CompositorPreset { name: name.to_string(), category: category.to_string(), stack: self.clone() };
        let json = serde_json::to_string_pretty(&preset).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Load from JSON file.
    pub fn load_preset(path: &std::path::Path) -> std::io::Result<CompositorPreset> {
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}
