// core_engine/core_math/src/material_dna.rs
// Material DNA — Materials as Mathematical Functions of Geometry
use glam::Vec3;
use serde::{Serialize, Deserialize};

/// Geometric data source for Material DNA conditions.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DnaInput {
    Curvature,      // 0: Laplacian curvature (+ = convex edge, - = concave cavity)
    Convexity,      // 1: sign(mean_curvature), 0..1 mapped
    CavityDepth,    // 2: SDF-based AO approximation
    SlopeAngle,     // 3: acos(normal.dot(up)), 0=flat, PI/2=vertical
    ViewAngle,      // 4: normal.dot(view_dir), 0=grazing, 1=head-on
    Noise3D,        // 5: procedural noise at world position
    WorldY,         // 6: world position Y
    SdfDistance,     // 7: raw SDF distance (thin shell detection)
    BoolBlend,      // 8: boolean intersection blend weight
    Thickness,      // 9: SDF medial axis distance approximation
}
impl DnaInput {
    pub fn to_u32(self) -> u32 {
        match self {
            Self::Curvature=>0, Self::Convexity=>1, Self::CavityDepth=>2,
            Self::SlopeAngle=>3, Self::ViewAngle=>4, Self::Noise3D=>5,
            Self::WorldY=>6, Self::SdfDistance=>7, Self::BoolBlend=>8, Self::Thickness=>9,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Curvature=>"Curvature", Self::Convexity=>"Convexity", Self::CavityDepth=>"Cavity Depth",
            Self::SlopeAngle=>"Slope Angle", Self::ViewAngle=>"View Angle", Self::Noise3D=>"Noise 3D",
            Self::WorldY=>"World Y", Self::SdfDistance=>"SDF Distance", Self::BoolBlend=>"Boolean Blend",
            Self::Thickness=>"Thickness",
        }
    }
    pub const ALL: [DnaInput; 10] = [
        Self::Curvature, Self::Convexity, Self::CavityDepth, Self::SlopeAngle, Self::ViewAngle,
        Self::Noise3D, Self::WorldY, Self::SdfDistance, Self::BoolBlend, Self::Thickness,
    ];
}
impl Default for DnaInput { fn default() -> Self { Self::Curvature } }

/// Condition type for a DNA rule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DnaCondition {
    Always,
    Range     { input: DnaInput, min: f32, max: f32, falloff: f32 },
    GreaterThan { input: DnaInput, threshold: f32, falloff: f32 },
    LessThan  { input: DnaInput, threshold: f32, falloff: f32 },
    Direction { direction: Vec3, angle: f32, falloff: f32 },
    NoiseThreshold { scale: f32, threshold: f32, falloff: f32 },
    AltitudeBand { min_y: f32, max_y: f32, falloff: f32 },
    BooleanZone { blend_weight_min: f32 },
}
impl Default for DnaCondition { fn default() -> Self { Self::Always } }
impl DnaCondition {
    pub fn type_index(&self) -> u32 {
        match self {
            Self::Always=>0, Self::Range{..}=>1, Self::GreaterThan{..}=>2, Self::LessThan{..}=>3,
            Self::Direction{..}=>4, Self::NoiseThreshold{..}=>5, Self::AltitudeBand{..}=>6,
            Self::BooleanZone{..}=>7,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Always=>"Always", Self::Range{..}=>"Range", Self::GreaterThan{..}=>"Greater Than",
            Self::LessThan{..}=>"Less Than", Self::Direction{..}=>"Direction",
            Self::NoiseThreshold{..}=>"Noise Threshold", Self::AltitudeBand{..}=>"Altitude Band",
            Self::BooleanZone{..}=>"Boolean Zone",
        }
    }
}

/// Blend mode for DNA rule compositing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum DnaBlendMode {
    #[default] Normal,
    Multiply,
    Screen,
    Add,
}
impl DnaBlendMode {
    pub fn to_u32(self) -> u32 { match self { Self::Normal=>0, Self::Multiply=>1, Self::Screen=>2, Self::Add=>3 } }
    pub fn label(&self) -> &'static str { match self { Self::Normal=>"Normal", Self::Multiply=>"Multiply", Self::Screen=>"Screen", Self::Add=>"Add" } }
    pub const ALL: [DnaBlendMode; 4] = [Self::Normal, Self::Multiply, Self::Screen, Self::Add];
}

/// PBR surface values applied by a DNA rule.
/// Each property has an enable flag — only enabled properties are overridden by the rule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DnaSurface {
    pub albedo: Vec3,
    pub roughness: f32,
    pub metalness: f32,
    pub emission: Vec3,
    pub height: f32,
    // Per-property enable flags (new rules start with only albedo on)
    #[serde(default = "DnaSurface::default_en_albedo")]
    pub en_albedo: bool,
    #[serde(default)]
    pub en_roughness: bool,
    #[serde(default)]
    pub en_metalness: bool,
    #[serde(default)]
    pub en_emission: bool,
    #[serde(default)]
    pub en_height: bool,
}
impl DnaSurface {
    fn default_en_albedo() -> bool { true }
}
impl Default for DnaSurface {
    fn default() -> Self {
        Self {
            albedo: Vec3::new(0.5, 0.5, 0.5), roughness: 0.5, metalness: 0.0,
            emission: Vec3::ZERO, height: 0.0,
            en_albedo: true, en_roughness: false, en_metalness: false,
            en_emission: false, en_height: false,
        }
    }
}

/// A single rule in the Material DNA stack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DnaRule {
    pub name: String,
    pub condition: DnaCondition,
    pub surface: DnaSurface,
    pub blend_mode: DnaBlendMode,
    pub intensity: f32,
    pub visible: bool,
    pub locked: bool,
    pub age_min: f32,
    pub age_max: f32,
    pub noise_scale: f32,
}
impl Default for DnaRule {
    fn default() -> Self {
        Self {
            name: "New Rule".into(), condition: DnaCondition::Always, surface: DnaSurface::default(),
            blend_mode: DnaBlendMode::Normal, intensity: 1.0, visible: true, locked: false,
            age_min: 0.0, age_max: 1.0, noise_scale: 5.0,
        }
    }
}
impl DnaRule {
    /// Serialize this rule into 20 GPU floats.
    pub fn to_gpu_floats(&self) -> [f32; 20] {
        let mut f = [0.0f32; 20];
        if !self.visible { f[19] = -1.0; return f; } // marker: invisible
        f[0] = self.condition.type_index() as f32;
        match &self.condition {
            DnaCondition::Always => { f[1] = 0.0; }
            DnaCondition::Range { input, min, max, falloff } => {
                f[1] = input.to_u32() as f32; f[2] = *min; f[3] = *max; f[4] = *falloff;
            }
            DnaCondition::GreaterThan { input, threshold, falloff } => {
                f[1] = input.to_u32() as f32; f[2] = *threshold; f[4] = *falloff;
            }
            DnaCondition::LessThan { input, threshold, falloff } => {
                f[1] = input.to_u32() as f32; f[2] = *threshold; f[4] = *falloff;
            }
            DnaCondition::Direction { direction, angle, falloff: _ } => {
                f[2] = direction.x; f[3] = direction.y; f[4] = direction.z;
                f[1] = *angle; // reuse geo_input slot for angle
            }
            DnaCondition::NoiseThreshold { scale, threshold, falloff } => {
                f[2] = *scale; f[3] = *threshold; f[4] = *falloff;
            }
            DnaCondition::AltitudeBand { min_y, max_y, falloff } => {
                f[2] = *min_y; f[3] = *max_y; f[4] = *falloff;
            }
            DnaCondition::BooleanZone { blend_weight_min } => {
                f[2] = *blend_weight_min;
            }
        }
        f[5] = self.age_min;
        f[6] = self.age_max;
        f[7] = self.surface.albedo.x; f[8] = self.surface.albedo.y; f[9] = self.surface.albedo.z;
        f[10] = self.surface.roughness;
        f[11] = self.surface.metalness;
        f[12] = self.surface.emission.x; f[13] = self.surface.emission.y; f[14] = self.surface.emission.z;
        f[15] = self.intensity;
        f[16] = self.blend_mode.to_u32() as f32;
        f[17] = self.surface.height;
        f[18] = self.noise_scale;
        // f[19]: negative = invisible marker, positive = enable bitmask
        // bit 0 = albedo, bit 1 = roughness, bit 2 = metalness, bit 3 = emission, bit 4 = height
        let mut en_bits = 0u32;
        if self.surface.en_albedo { en_bits |= 1; }
        if self.surface.en_roughness { en_bits |= 2; }
        if self.surface.en_metalness { en_bits |= 4; }
        if self.surface.en_emission { en_bits |= 8; }
        if self.surface.en_height { en_bits |= 16; }
        f[19] = en_bits as f32;
        f
    }
}

/// Material DNA: an ordered stack of geometry-responsive rules.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MaterialDna {
    pub rules: Vec<DnaRule>,
    pub age: f32,
}
impl MaterialDna {
    pub fn is_active(&self) -> bool { !self.rules.is_empty() }
    pub fn visible_rule_count(&self) -> usize { self.rules.iter().filter(|r| r.visible).count() }
}
