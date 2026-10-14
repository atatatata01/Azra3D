// shader_nodes/ops.rs
use serde::{Serialize, Deserialize};
use super::node_type::NodeType;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ProjectionStyle {
    #[default]
    Triplanar,
    PlanarXY,
    PlanarXZ,
    PlanarYZ,
    Box,
    Cylindrical,
    Spherical,
    /// Use connected Position/UV as-is (no built-in projection).
    None,
}

impl ProjectionStyle {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Triplanar => "Triplanar",
            Self::PlanarXY => "Planar XY",
            Self::PlanarXZ => "Planar XZ",
            Self::PlanarYZ => "Planar YZ",
            Self::Box => "Box",
            Self::Cylindrical => "Cylindrical",
            Self::Spherical => "Spherical",
            Self::None => "None (raw)",
        }
    }
    /// Compact badge for node header (projection is baked into the generator).
    pub fn short(&self) -> &'static str {
        match self {
            Self::Triplanar => "Tri",
            Self::PlanarXY => "XY",
            Self::PlanarXZ => "XZ",
            Self::PlanarYZ => "YZ",
            Self::Box => "Box",
            Self::Cylindrical => "Cyl",
            Self::Spherical => "Sph",
            Self::None => "Raw",
        }
    }
    pub fn all() -> &'static [ProjectionStyle] {
        &[
            Self::Triplanar,
            Self::PlanarXY,
            Self::PlanarXZ,
            Self::PlanarYZ,
            Self::Box,
            Self::Cylindrical,
            Self::Spherical,
            Self::None,
        ]
    }
}

/// Texture / noise generators that carry a built-in projection style.
pub fn node_has_builtin_projection(nt: &NodeType) -> bool {
    matches!(
        nt,
        NodeType::Noise
            | NodeType::Voronoi
            | NodeType::ImageTexture
            | NodeType::GradientTexture
            | NodeType::WaveTexture
            | NodeType::MusgraveTexture
            | NodeType::BrickTexture
            | NodeType::TextureSample
    )
}

// ====================================================================
// Noise output remap / blend curve (Advanced on Noise node)
// Shapes the 0–1 noise value before it leaves the node (levels-style control).
// ====================================================================
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum NoiseBlendCurve {
    /// No reshape — raw noise 0..1.
    #[default]
    Linear,
    /// Hermite smoothstep between Black and White.
    Smooth,
    /// Quintic smootherstep (Perlin-style) between Black and White.
    Smoother,
    /// Power / gamma: `pow(t, K)` after black/white remap. K>1 darkens mids, K<1 lifts them.
    Power,
    /// Photoshop-style levels: Black / Mid / White. Mid is gamma (1 = linear).
    Levels,
}

impl NoiseBlendCurve {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Smooth => "Smooth",
            Self::Smoother => "Smoother",
            Self::Power => "Power (K)",
            Self::Levels => "Levels",
        }
    }
    pub fn short(&self) -> &'static str {
        match self {
            Self::Linear => "Lin",
            Self::Smooth => "Sm",
            Self::Smoother => "Smr",
            Self::Power => "Pow",
            Self::Levels => "Lvl",
        }
    }
    pub fn all() -> &'static [NoiseBlendCurve] {
        &[
            Self::Linear,
            Self::Smooth,
            Self::Smoother,
            Self::Power,
            Self::Levels,
        ]
    }
    /// Mode id packed into GPU live uniforms (cavity_dirt.w when cavity off).
    pub fn as_u32(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Smooth => 1,
            Self::Smoother => 2,
            Self::Power => 3,
            Self::Levels => 4,
        }
    }
}

/// Remap noise value `t` with black/white points and curve shape.
/// `k` = power exponent (Power) or mid gamma (Levels, 1 = linear).
pub fn apply_noise_blend_curve(
    t: f32,
    curve: NoiseBlendCurve,
    black: f32,
    white: f32,
    k: f32,
) -> f32 {
    let b = black;
    let w = if (white - black).abs() < 1e-6 {
        black + 1e-6
    } else {
        white
    };
    // Normalize into 0..1 using black/white (levels-style input range).
    let mut x = ((t - b) / (w - b)).clamp(0.0, 1.0);
    match curve {
        NoiseBlendCurve::Linear => x,
        NoiseBlendCurve::Smooth => {
            // smoothstep
            x * x * (3.0 - 2.0 * x)
        }
        NoiseBlendCurve::Smoother => {
            // smootherstep (Ken Perlin)
            x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
        }
        NoiseBlendCurve::Power => {
            let exp = k.max(0.001);
            x.powf(exp)
        }
        NoiseBlendCurve::Levels => {
            // Midtone gamma: mid>1 lifts shadows, mid<1 crushes (Photoshop-ish)
            let mid = k.max(0.001);
            x.powf(1.0 / mid)
        }
    }
    .clamp(0.0, 1.0)
}

// ====================================================================
// Noise type param (on Noise node) — serde default Fbm for old graphs
// ====================================================================
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum NoiseType {
    Value,
    #[default]
    Fbm,
    White,
    Cellular,
    Ridged,
    Billow,
    DomainWarp,
    /// True gradient Perlin (not value-noise alias).
    Perlin,
    /// True simplex-style gradient noise.
    Simplex,
    /// Musgrave multi-fractal (same family as standalone Musgrave Texture node).
    /// Append-only — do not reorder variants above.
    Musgrave,
}

impl NoiseType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Value => "Value",
            Self::Fbm => "FBM",
            Self::White => "White",
            Self::Cellular => "Cellular",
            Self::Ridged => "Ridged",
            Self::Billow => "Billow",
            Self::DomainWarp => "Domain Warp",
            Self::Perlin => "Perlin",
            Self::Simplex => "Simplex",
            Self::Musgrave => "Musgrave",
        }
    }
    /// Short badge for node header.
    pub fn short(&self) -> &'static str {
        match self {
            Self::Value => "Val",
            Self::Fbm => "FBM",
            Self::White => "Wht",
            Self::Cellular => "Cell",
            Self::Ridged => "Rid",
            Self::Billow => "Bil",
            Self::DomainWarp => "Warp",
            Self::Perlin => "Perl",
            Self::Simplex => "Simp",
            Self::Musgrave => "Musg",
        }
    }
    pub fn all() -> &'static [NoiseType] {
        &[
            Self::Value,
            Self::Fbm,
            Self::Perlin,
            Self::Simplex,
            Self::White,
            Self::Cellular,
            Self::Ridged,
            Self::Billow,
            Self::DomainWarp,
            Self::Musgrave,
        ]
    }
}

// ====================================================================
// Vector math op param
// ====================================================================
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum VectorMathOp {
    #[default]
    Add,
    Subtract,
    Multiply,
    Scale,
    Dot,
    Cross,
    Length,
    Normalize,
    Reflect,
    Distance,
}

impl VectorMathOp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Multiply => "Multiply",
            Self::Scale => "Scale",
            Self::Dot => "Dot",
            Self::Cross => "Cross",
            Self::Length => "Length",
            Self::Normalize => "Normalize",
            Self::Reflect => "Reflect",
            Self::Distance => "Distance",
        }
    }
    pub fn all() -> &'static [VectorMathOp] {
        &[
            Self::Add, Self::Subtract, Self::Multiply, Self::Scale,
            Self::Dot, Self::Cross, Self::Length, Self::Normalize,
            Self::Reflect, Self::Distance,
        ]
    }
}

// ====================================================================
// Mix Color blend mode (MixRgb node) — Photoshop / Substance parity.
// APPEND-ONLY (FL4NODES JSON, name-tagged serde). Default Mix keeps the
// legacy factor-lerp behavior so old graphs load unchanged.
//
// Evaluated in TWO places that MUST stay in lockstep:
//   * CPU bake: `node_graph_bake` → [`mix_blend_channel`]
//   * WGSL codegen: `shader_nodes/graph.rs` MixRgb arm
// ====================================================================
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MixBlend {
    /// Factor-lerp Color1→Color2 (historical MixRgb / Photoshop Normal).
    #[default]
    Mix,
    Multiply,
    Screen,
    Overlay,
    /// Linear Dodge (Add).
    Add,
    // --- P2 Photoshop set (append-only; never reorder) ---
    Darken,
    Lighten,
    ColorBurn,
    ColorDodge,
    LinearBurn,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    // APPEND only.
}

impl MixBlend {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mix => "Mix",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Add => "Add (Linear Dodge)",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorBurn => "Color Burn",
            Self::ColorDodge => "Color Dodge",
            Self::LinearBurn => "Linear Burn",
            Self::SoftLight => "Soft Light",
            Self::HardLight => "Hard Light",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::HardMix => "Hard Mix",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::Divide => "Divide",
        }
    }
    pub fn all() -> &'static [MixBlend] {
        &[
            Self::Mix,
            Self::Darken,
            Self::Multiply,
            Self::ColorBurn,
            Self::LinearBurn,
            Self::Lighten,
            Self::Screen,
            Self::ColorDodge,
            Self::Add,
            Self::Overlay,
            Self::SoftLight,
            Self::HardLight,
            Self::VividLight,
            Self::LinearLight,
            Self::PinLight,
            Self::HardMix,
            Self::Difference,
            Self::Exclusion,
            Self::Subtract,
            Self::Divide,
        ]
    }
}

/// Per-channel blend of base `a` with layer `b` (Photoshop formulas, linear 0..1).
/// Used by the CPU bake; WGSL codegen must emit equivalent expressions.
#[inline]
pub fn mix_blend_channel(mode: MixBlend, a: f32, b: f32) -> f32 {
    let a = a.clamp(0.0, 1.0);
    let b = b.clamp(0.0, 1.0);
    match mode {
        MixBlend::Mix => b,
        MixBlend::Multiply => a * b,
        MixBlend::Screen => 1.0 - (1.0 - a) * (1.0 - b),
        MixBlend::Add => (a + b).min(1.0),
        MixBlend::Overlay => {
            if a < 0.5 {
                2.0 * a * b
            } else {
                1.0 - 2.0 * (1.0 - a) * (1.0 - b)
            }
        }
        MixBlend::Darken => a.min(b),
        MixBlend::Lighten => a.max(b),
        MixBlend::ColorBurn => {
            if b <= 1e-5 {
                0.0
            } else {
                (1.0 - (1.0 - a) / b).clamp(0.0, 1.0)
            }
        }
        MixBlend::ColorDodge => {
            if b >= 1.0 - 1e-5 {
                1.0
            } else {
                (a / (1.0 - b)).clamp(0.0, 1.0)
            }
        }
        MixBlend::LinearBurn => (a + b - 1.0).clamp(0.0, 1.0),
        MixBlend::SoftLight => {
            // Pegtop: a²*(1−2b) + 2ab when b<0.5 else sqrt path
            if b < 0.5 {
                a - (1.0 - 2.0 * b) * a * (1.0 - a)
            } else {
                let d = if a <= 0.25 {
                    ((16.0 * a - 12.0) * a + 4.0) * a
                } else {
                    a.sqrt()
                };
                a + (2.0 * b - 1.0) * (d - a)
            }
        }
        MixBlend::HardLight => {
            if b < 0.5 {
                2.0 * a * b
            } else {
                1.0 - 2.0 * (1.0 - a) * (1.0 - b)
            }
        }
        MixBlend::VividLight => {
            if b < 0.5 {
                // Color burn with 2b
                let bb = (2.0 * b).max(1e-5);
                (1.0 - (1.0 - a) / bb).clamp(0.0, 1.0)
            } else {
                let bb = (2.0 * (1.0 - b)).max(1e-5);
                (a / bb).clamp(0.0, 1.0).min(1.0)
            }
        }
        MixBlend::LinearLight => (a + 2.0 * b - 1.0).clamp(0.0, 1.0),
        MixBlend::PinLight => {
            if b < 0.5 {
                a.min(2.0 * b)
            } else {
                a.max(2.0 * b - 1.0)
            }
        }
        MixBlend::HardMix => {
            if a + b < 1.0 {
                0.0
            } else {
                1.0
            }
        }
        MixBlend::Difference => (a - b).abs(),
        MixBlend::Exclusion => a + b - 2.0 * a * b,
        MixBlend::Subtract => (a - b).clamp(0.0, 1.0),
        MixBlend::Divide => {
            if b <= 1e-5 {
                1.0
            } else {
                (a / b).clamp(0.0, 1.0)
            }
        }
    }
}

/// Full RGB blend + over with factor `fac` (0 = keep base, 1 = full blend).
/// This is the real over-operator: `mix(base, op(base, layer), fac)` — masks *remove*
/// rather than crushing toward black.
#[inline]
pub fn mix_blend_over(mode: MixBlend, base: [f32; 3], layer: [f32; 3], fac: f32) -> [f32; 3] {
    let fac = fac.clamp(0.0, 1.0);
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        let blended = mix_blend_channel(mode, base[i], layer[i]);
        out[i] = base[i] + (blended - base[i]) * fac;
    }
    out
}

// ====================================================================
// Math operation sub-types
// ====================================================================
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum MathBinaryOp {
    Add, Subtract, Multiply, Divide, Power, Min, Max, Modulo,
    // append-only
    Atan2, Compare, Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum MathUnaryOp {
    Abs, Negate, Sqrt, Sin, Cos, Fract, Floor, Ceil, Saturate,
    // append-only
    OneMinus, Sign, Exp, Log, Round, Truncate,
}

impl MathBinaryOp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Add => "Add", Self::Subtract => "Subtract", Self::Multiply => "Multiply",
            Self::Divide => "Divide", Self::Power => "Power", Self::Min => "Min",
            Self::Max => "Max", Self::Modulo => "Modulo",
            Self::Atan2 => "Atan2", Self::Compare => "Compare", Self::Step => "Step",
        }
    }
    pub fn all() -> &'static [MathBinaryOp] {
        &[
            Self::Add, Self::Subtract, Self::Multiply, Self::Divide, Self::Power,
            Self::Min, Self::Max, Self::Modulo, Self::Atan2, Self::Compare, Self::Step,
        ]
    }
    pub fn wgsl_expr(&self, a: &str, b: &str) -> String {
        match self {
            Self::Add => format!("({a} + {b})"),
            Self::Subtract => format!("({a} - {b})"),
            Self::Multiply => format!("({a} * {b})"),
            Self::Divide => format!("select({a} / {b}, vec3(0.0), {b} == vec3(0.0))"),
            Self::Power => format!("pow(abs({a}), {b})"),
            Self::Min => format!("min({a}, {b})"),
            Self::Max => format!("max({a}, {b})"),
            Self::Modulo => format!("({a} % {b})"),
            // Binary ops that only make sense on f32 still emit scalar-safe expressions.
            Self::Atan2 => format!("atan2({a}, {b})"),
            Self::Compare => format!("select(0.0, 1.0, {a} > {b})"),
            Self::Step => format!("step({a}, {b})"),
        }
    }
    pub fn wgsl_expr_f32(&self, a: &str, b: &str) -> String {
        match self {
            Self::Add => format!("({a} + {b})"),
            Self::Subtract => format!("({a} - {b})"),
            Self::Multiply => format!("({a} * {b})"),
            Self::Divide => format!("select({a} / {b}, 0.0, {b} == 0.0)"),
            Self::Power => format!("pow(abs({a}), {b})"),
            Self::Min => format!("min({a}, {b})"),
            Self::Max => format!("max({a}, {b})"),
            Self::Modulo => format!("({a} % {b})"),
            Self::Atan2 => format!("atan2({a}, {b})"),
            Self::Compare => format!("select(0.0, 1.0, {a} > {b})"),
            Self::Step => format!("step({a}, {b})"),
        }
    }
}

impl MathUnaryOp {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Abs => "Abs", Self::Negate => "Negate", Self::Sqrt => "Sqrt",
            Self::Sin => "Sin", Self::Cos => "Cos", Self::Fract => "Fract",
            Self::Floor => "Floor", Self::Ceil => "Ceil", Self::Saturate => "Saturate",
            Self::OneMinus => "1-x", Self::Sign => "Sign", Self::Exp => "Exp",
            Self::Log => "Log", Self::Round => "Round", Self::Truncate => "Truncate",
        }
    }
    pub fn all() -> &'static [MathUnaryOp] {
        &[
            Self::Abs, Self::Negate, Self::Sqrt, Self::Sin, Self::Cos, Self::Fract,
            Self::Floor, Self::Ceil, Self::Saturate, Self::OneMinus, Self::Sign,
            Self::Exp, Self::Log, Self::Round, Self::Truncate,
        ]
    }
    pub fn wgsl_expr(&self, v: &str) -> String {
        match self {
            Self::Abs => format!("abs({v})"),
            Self::Negate => format!("-({v})"),
            Self::Sqrt => format!("sqrt(abs({v}))"),
            Self::Sin => format!("sin({v})"),
            Self::Cos => format!("cos({v})"),
            Self::Fract => format!("fract({v})"),
            Self::Floor => format!("floor({v})"),
            Self::Ceil => format!("ceil({v})"),
            Self::Saturate => format!("saturate({v})"),
            Self::OneMinus => format!("(1.0 - ({v}))"),
            Self::Sign => format!("sign({v})"),
            Self::Exp => format!("exp({v})"),
            Self::Log => format!("log(max({v}, 1e-6))"),
            Self::Round => format!("round({v})"),
            Self::Truncate => format!("trunc({v})"),
        }
    }
}

// ====================================================================
// Node instance
// ====================================================================

#[cfg(test)]
mod mix_blend_tests {
    use super::*;

    #[test]
    fn photoshop_modes_identity_and_over_mask_removes() {
        // Multiply darkens mid-gray with black layer.
        assert!((mix_blend_channel(MixBlend::Multiply, 0.5, 0.0) - 0.0).abs() < 1e-5);
        // Screen with white ? white.
        assert!((mix_blend_channel(MixBlend::Screen, 0.3, 1.0) - 1.0).abs() < 1e-5);
        // Over with fac=0 keeps base (mask removes layer contribution).
        let base = [0.8, 0.2, 0.1];
        let layer = [0.0, 0.0, 0.0];
        let out = mix_blend_over(MixBlend::Multiply, base, layer, 0.0);
        assert!((out[0] - base[0]).abs() < 1e-5 && (out[1] - base[1]).abs() < 1e-5);
        // Over with fac=1 applies full multiply ? black.
        let out1 = mix_blend_over(MixBlend::Multiply, base, layer, 1.0);
        assert!(out1[0] < 0.01 && out1[1] < 0.01 && out1[2] < 0.01);
        // Darken/Lighten endpoints.
        assert!((mix_blend_channel(MixBlend::Darken, 0.7, 0.3) - 0.3).abs() < 1e-5);
        assert!((mix_blend_channel(MixBlend::Lighten, 0.7, 0.3) - 0.7).abs() < 1e-5);
        // Difference of equals is 0.
        assert!((mix_blend_channel(MixBlend::Difference, 0.4, 0.4)).abs() < 1e-5);
        // all() is non-empty and includes new modes.
        assert!(MixBlend::all().len() >= 20);
        assert!(MixBlend::all().contains(&MixBlend::SoftLight));
        assert!(MixBlend::all().contains(&MixBlend::HardMix));
    }

    #[test]
    fn panel_blendmode_round_trips_mixblend() {
        // Lives in material_panel; smoke that MixBlend labels stay unique.
        let mut labels = std::collections::HashSet::new();
        for m in MixBlend::all() {
            assert!(labels.insert(m.label()), "duplicate label {}", m.label());
        }
    }

    /// M2 — per-layer alpha is Factor over: fac=0 keeps base for EVERY mode (mask removes).
    /// fac=1 applies full blend. Partial fac is a linear over (not crush-to-black).
    #[test]
    fn m2_over_factor_is_true_alpha_for_all_modes() {
        let base = [0.75, 0.45, 0.25];
        let layer = [0.1, 0.9, 0.5];
        for mode in MixBlend::all() {
            let zero = mix_blend_over(*mode, base, layer, 0.0);
            for i in 0..3 {
                assert!(
                    (zero[i] - base[i]).abs() < 1e-5,
                    "{:?} fac=0 must keep base (got {:?} vs {:?})",
                    mode,
                    zero,
                    base
                );
            }
            let full = mix_blend_over(*mode, base, layer, 1.0);
            let expected_ch = [
                mix_blend_channel(*mode, base[0], layer[0]),
                mix_blend_channel(*mode, base[1], layer[1]),
                mix_blend_channel(*mode, base[2], layer[2]),
            ];
            for i in 0..3 {
                assert!(
                    (full[i] - expected_ch[i]).abs() < 1e-5,
                    "{:?} fac=1 must equal mix_blend_channel",
                    mode
                );
            }
            // Partial: over is lerp(base, blended, fac) — mask mid-grey is half the delta.
            let half = mix_blend_over(*mode, base, layer, 0.5);
            for i in 0..3 {
                let expect = base[i] + (expected_ch[i] - base[i]) * 0.5;
                assert!(
                    (half[i] - expect).abs() < 1e-5,
                    "{:?} fac=0.5 must be linear over, not crush",
                    mode
                );
            }
        }
        assert_eq!(
            MixBlend::all().len(),
            20,
            "M2/M3 scoreboard: 20 MixBlend modes (panel BlendMode maps 1:1)"
        );
    }

    /// M3 mode sheet — every mode produces a defined RGB triple for a fixed base/layer pair.
    /// Modes that must differ from Mix/Normal on this pair are checked; labels stay unique.
    #[test]
    fn m3_mode_sheet_all_modes_defined_and_distinct_from_mix() {
        let base = [0.6f32, 0.3, 0.2];
        let layer = [0.2, 0.7, 0.5];
        let mix_ref = mix_blend_over(MixBlend::Mix, base, layer, 1.0);
        let mut fingerprints: Vec<(i32, i32, i32)> = Vec::new();
        for mode in MixBlend::all() {
            let out = mix_blend_over(*mode, base, layer, 1.0);
            for c in out {
                assert!(c.is_finite() && (0.0..=1.0).contains(&c), "{mode:?} out of range");
            }
            // Fingerprint: quantize to 1e-4 so floating noise doesn't explode uniqueness.
            let key = (
                (out[0] * 10000.0).round() as i32,
                (out[1] * 10000.0).round() as i32,
                (out[2] * 10000.0).round() as i32,
            );
            fingerprints.push(key);
            // Most modes differ from Normal/Mix on this mid-tone pair.
            if !matches!(*mode, MixBlend::Mix) {
                let same = (out[0] - mix_ref[0]).abs() < 1e-5
                    && (out[1] - mix_ref[1]).abs() < 1e-5
                    && (out[2] - mix_ref[2]).abs() < 1e-5;
                // Only assert modes that must diverge on this pair.
                if matches!(
                    *mode,
                    MixBlend::Multiply | MixBlend::Screen | MixBlend::Difference | MixBlend::Overlay
                ) {
                    assert!(!same, "{mode:?} must differ from Mix on this pair");
                }
            }
        }
        assert_eq!(fingerprints.len(), MixBlend::all().len());
        eprintln!(
            "[M3 MODE SHEET] {} modes @ base={base:?} layer={layer:?} — all finite 0..1",
            MixBlend::all().len()
        );
    }
}
