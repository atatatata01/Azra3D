//! Panel readout helpers (channel order table + cost summary).
//!
//! Pure relocation from lower.rs — no logic changes.

use super::super::model::*;
use core_math::pbr_material::{MapKind, PbrMaterial};

/// Panel order of all channel kinds (shared with view.rs / derive).
pub(crate) const ALL_KINDS: [ChannelKind; 18] = [
    ChannelKind::BaseColor,
    ChannelKind::Roughness,
    ChannelKind::Metallic,
    ChannelKind::Opacity,
    ChannelKind::Emission,
    ChannelKind::Normal,
    ChannelKind::Bump,
    ChannelKind::Displacement,
    ChannelKind::Clearcoat,
    ChannelKind::ClearcoatRoughness,
    ChannelKind::Sheen,
    ChannelKind::SheenTint,
    ChannelKind::Anisotropy,
    ChannelKind::AnisotropyRotation,
    ChannelKind::Ior,
    ChannelKind::SssRadius,
    ChannelKind::SssColor,
    ChannelKind::GlassTint,
];

/// Header cost readout (spec §2 / revised C2 — honest data, no permutations):
/// `"standard PBR · 4 maps"`. Pure fn.
pub(crate) fn cost_summary(mat: &PbrMaterial) -> String {
    use core_math::pbr_material::ShaderType;
    let model = match mat.shader_type {
        ShaderType::StandardPBR => "standard PBR",
        ShaderType::Subsurface => "subsurface",
        ShaderType::Glass => "glass",
        ShaderType::VFX => "VFX",
        ShaderType::Advanced => "advanced",
    };
    let maps = MapKind::ALL.iter().filter(|&&k| mat.map(k).is_set()).count();
    match maps {
        0 => model.to_string(),
        1 => format!("{model} · 1 map"),
        n => format!("{model} · {n} maps"),
    }
}

