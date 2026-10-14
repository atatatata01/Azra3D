// Viewport Compositor types — extracted from types.rs
use serde::{Serialize, Deserialize};

// ==================================================================
// Viewport Compositor — Post-Process Stack
// ==================================================================

/// Effect types available in the compositor stack.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CompositorEffectType {
    Bloom,
    DepthOfField,
    MotionBlur,
    ChromaticAberration,
    Sharpen,
    LensDistortion,
    Fog,
    Outline,
    Vignette,
    FilmGrain,
    ColorGrading,
}

impl CompositorEffectType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bloom => "Bloom",
            Self::DepthOfField => "Depth of Field",
            Self::MotionBlur => "Motion Blur",
            Self::ChromaticAberration => "Chromatic Aberration",
            Self::Sharpen => "Sharpen",
            Self::LensDistortion => "Lens Distortion",
            Self::Fog => "Fog",
            Self::Outline => "Outline",
            Self::Vignette => "Vignette",
            Self::FilmGrain => "Film Grain",
            Self::ColorGrading => "Color Grading",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::Bloom => "\u{2728}",
            Self::DepthOfField => "\u{1F4F7}",
            Self::MotionBlur => "\u{1F4A8}",
            Self::ChromaticAberration => "\u{1F308}",
            Self::Sharpen => "\u{1F5E1}",
            Self::LensDistortion => "\u{1F50D}",
            Self::Fog => "\u{1F32B}",
            Self::Outline => "\u{270F}",
            Self::Vignette => "\u{1F305}",
            Self::FilmGrain => "\u{1F3AC}",
            Self::ColorGrading => "\u{1F3A8}",
        }
    }
    pub fn all() -> &'static [CompositorEffectType] {
        &[
            Self::Bloom, Self::DepthOfField, Self::MotionBlur,
            Self::ChromaticAberration, Self::Sharpen, Self::LensDistortion,
            Self::Fog, Self::Outline, Self::Vignette, Self::FilmGrain,
            Self::ColorGrading,
        ]
    }
}

/// Mask source for masking compositor effects.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CompositorMaskSource {
    None,
    Noise3D { scale: f32, octaves: u32, persistence: f32, lacunarity: f32, contrast: f32, seed: u32 },
    SurfaceId { ids: Vec<u32>, invert: bool },
    MaterialId { ids: Vec<u32>, invert: bool },
    Texture { slot: u32, channel: u32, invert: bool },
    SdfDistance { near: f32, far: f32, invert: bool },
    Curvature { min_k: f32, max_k: f32, invert: bool },
    Depth { near: f32, far: f32, invert: bool },
    NormalDirection { threshold: f32, invert: bool },
    Fresnel { power: f32, invert: bool },
    Vignette { radius: f32, softness: f32 },
}

impl Default for CompositorMaskSource {
    fn default() -> Self { Self::None }
}

/// Combinator for multiple masks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum MaskCombinator {
    Multiply,
    Add,
    Subtract,
}

/// A single mask layer (source + combinator).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorMaskLayer {
    pub source: CompositorMaskSource,
    pub combinator: MaskCombinator,
    pub invert: bool,
}

/// A single effect modifier in the compositor stack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorEffect {
    pub effect_type: CompositorEffectType,
    pub enabled: bool,
    pub solo: bool,
    pub expanded: bool,
    pub masks: Vec<CompositorMaskLayer>,
    #[serde(default)]
    pub mask_expanded: bool,
    // -- Bloom --
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    pub bloom_tint: [f32; 3],
    pub bloom_anamorphic: f32,
    pub bloom_emission_only: bool,
    // -- Depth of Field --
    pub dof_focus_dist: f32,
    pub dof_aperture: f32,
    pub dof_max_blur: f32,
    pub dof_bokeh_shape: u32,
    pub dof_chromatic: f32,
    // -- Motion Blur --
    pub mblur_intensity: f32,
    pub mblur_samples: u32,
    pub mblur_max_length: f32,
    pub mblur_shutter_angle: f32,
    // -- Chromatic Aberration --
    pub ca_intensity: f32,
    pub ca_mode: u32,
    // -- Sharpen --
    pub sharpen_strength: f32,
    pub sharpen_radius: f32,
    pub sharpen_threshold: f32,
    pub sharpen_clarity: bool,
    // -- Lens Distortion --
    pub distort_barrel: f32,
    pub distort_chromatic: bool,
    pub distort_scale: f32,
    // -- Fog --
    pub fog_color: [f32; 3],
    pub fog_density: f32,
    pub fog_start: f32,
    pub fog_end: f32,
    pub fog_mode: u32,
    pub fog_height_density: f32,
    pub fog_height_baseline: f32,
    pub fog_scatter: f32,
    // -- Outline --
    pub outline_source: u32,
    pub outline_width: f32,
    pub outline_color: [f32; 4],
    pub outline_normal_thresh: f32,
    pub outline_depth_thresh: f32,
    pub outline_surface_id: bool,
    pub outline_fill_mode: u32,
    pub outline_cel_steps: u32,
    // -- Vignette --
    pub vignette_intensity: f32,
    pub vignette_color: [f32; 3],
    pub vignette_shape: u32,
    pub vignette_radius: f32,
    pub vignette_softness: f32,
    pub vignette_blend: u32,
    // -- Film Grain --
    pub grain_noise_type: u32,
    pub grain_intensity: f32,
    pub grain_size: f32,
    pub grain_monochromatic: bool,
    pub grain_animate: bool,
    pub grain_blend: u32,
    pub grain_luminance_respond: bool,
    // -- Color Grading --
    pub grade_hue_shift: f32,
    pub grade_saturation: f32,
    pub grade_vibrance: f32,
    pub grade_brightness: f32,
    pub grade_contrast: f32,
    pub grade_gamma: f32,
    pub grade_highlights: f32,
    pub grade_shadows: f32,
    pub grade_balance_shadows: [f32; 3],
    pub grade_balance_midtones: [f32; 3],
    pub grade_balance_highlights: [f32; 3],
}

impl CompositorEffect {
    pub fn new(effect_type: CompositorEffectType) -> Self {
        Self {
            effect_type, enabled: true, solo: false, expanded: true, masks: Vec::new(), mask_expanded: false,
            bloom_threshold: 1.0, bloom_intensity: 1.0, bloom_radius: 20.0,
            bloom_tint: [1.0, 1.0, 1.0], bloom_anamorphic: 0.0, bloom_emission_only: false,
            dof_focus_dist: 5.0, dof_aperture: 2.8, dof_max_blur: 20.0,
            dof_bokeh_shape: 0, dof_chromatic: 0.0,
            mblur_intensity: 1.0, mblur_samples: 8, mblur_max_length: 20.0, mblur_shutter_angle: 180.0,
            ca_intensity: 0.3, ca_mode: 0,
            sharpen_strength: 1.0, sharpen_radius: 1.5, sharpen_threshold: 0.05, sharpen_clarity: false,
            distort_barrel: 0.0, distort_chromatic: false, distort_scale: 1.0,
            fog_color: [0.7, 0.75, 0.8], fog_density: 0.3, fog_start: 5.0, fog_end: 50.0,
            fog_mode: 0, fog_height_density: 0.0, fog_height_baseline: 0.0, fog_scatter: 0.0,
            outline_source: 0, outline_width: 1.0, outline_color: [0.0, 0.0, 0.0, 1.0],
            outline_normal_thresh: 0.5, outline_depth_thresh: 0.01, outline_surface_id: true,
            outline_fill_mode: 0, outline_cel_steps: 4,
            vignette_intensity: 0.5, vignette_color: [0.0, 0.0, 0.0],
            vignette_shape: 0, vignette_radius: 0.8, vignette_softness: 0.5, vignette_blend: 0,
            grain_noise_type: 0, grain_intensity: 0.03, grain_size: 1.0,
            grain_monochromatic: true, grain_animate: true, grain_blend: 2, grain_luminance_respond: false,
            grade_hue_shift: 0.0, grade_saturation: 1.0, grade_vibrance: 1.0,
            grade_brightness: 0.0, grade_contrast: 1.0, grade_gamma: 1.0,
            grade_highlights: 0.0, grade_shadows: 0.0,
            grade_balance_shadows: [1.0, 1.0, 1.0],
            grade_balance_midtones: [1.0, 1.0, 1.0],
            grade_balance_highlights: [1.0, 1.0, 1.0],
        }
    }
}

/// The full compositor state: an ordered stack of effects + color management.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorStack {
    pub effects: Vec<CompositorEffect>,
    pub enabled: bool,
    #[serde(default)]
    pub mask_preview_effect: Option<usize>,
    pub tonemapper: u32,
    pub cm_exposure: f32,
    pub cm_auto_exposure: bool,
    pub cm_white_balance_temp: f32,
    pub cm_white_balance_tint: f32,
    pub lut_intensity: f32,
}

impl Default for CompositorStack {
    fn default() -> Self {
        Self {
            effects: Vec::new(),
            enabled: false,
            mask_preview_effect: None,
            tonemapper: 0,
            cm_exposure: 0.0,
            cm_auto_exposure: false,
            cm_white_balance_temp: 6500.0,
            cm_white_balance_tint: 0.0,
            lut_intensity: 1.0,
        }
    }
}
