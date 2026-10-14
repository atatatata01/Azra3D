use super::node_type::NodeType;
use super::types::Socket;

pub mod tex_coord;
pub mod object_info;
pub mod noise;
pub mod voronoi;
pub mod math_binary;
pub mod math_unary;
pub mod remap;
pub mod color_ramp;
pub mod mix_rgb;
pub mod brightness_contrast;
pub mod separate_xyz;
pub mod combine_xyz;
pub mod triplanar;
pub mod box_mapping;
pub mod cylindrical_mapping;
pub mod spherical_mapping;
pub mod planar_mapping;
pub mod uv_transform;
pub mod texture_sample;
pub mod gradient_texture;
pub mod wave_texture;
pub mod musgrave_texture;
pub mod brick_texture;
pub mod material_output;
pub mod constant_color;
pub mod image_texture;
pub mod constant_value;
pub mod invert;
pub mod color_correct;
pub mod curvature;
pub mod cavity;
pub mod ambient_occlusion;
pub mod slope;
pub mod vector_math;
pub mod clamp;
pub mod mix_float;
pub mod smoothstep;
pub mod height_to_normal;
pub mod prim_sdf_mask;
pub mod sdf_curvature;
pub mod surface_direction;
pub mod sdf_thickness;
pub mod sdf_position;
mod reroute;
mod checker;
mod gamma;
mod frame;

pub fn label(nt: &NodeType) -> &'static str {
    match nt {
        NodeType::TexCoord => tex_coord::LABEL,
        NodeType::ObjectInfo => object_info::LABEL,
        NodeType::Noise => noise::LABEL,
        NodeType::Voronoi => voronoi::LABEL,
        NodeType::MathBinary => math_binary::LABEL,
        NodeType::MathUnary => math_unary::LABEL,
        NodeType::Remap => remap::LABEL,
        NodeType::ColorRamp => color_ramp::LABEL,
        NodeType::MixRgb => mix_rgb::LABEL,
        NodeType::BrightnessContrast => brightness_contrast::LABEL,
        NodeType::SeparateXyz => separate_xyz::LABEL,
        NodeType::CombineXyz => combine_xyz::LABEL,
        NodeType::Triplanar => triplanar::LABEL,
        NodeType::BoxMapping => box_mapping::LABEL,
        NodeType::CylindricalMapping => cylindrical_mapping::LABEL,
        NodeType::SphericalMapping => spherical_mapping::LABEL,
        NodeType::PlanarMapping => planar_mapping::LABEL,
        NodeType::UVTransform => uv_transform::LABEL,
        NodeType::TextureSample => texture_sample::LABEL,
        NodeType::GradientTexture => gradient_texture::LABEL,
        NodeType::WaveTexture => wave_texture::LABEL,
        NodeType::MusgraveTexture => musgrave_texture::LABEL,
        NodeType::BrickTexture => brick_texture::LABEL,
        NodeType::MaterialOutput => material_output::LABEL,
        NodeType::ConstantColor => constant_color::LABEL,
        NodeType::ImageTexture => image_texture::LABEL,
        NodeType::ConstantValue => constant_value::LABEL,
        NodeType::Invert => invert::LABEL,
        NodeType::ColorCorrect => color_correct::LABEL,
        NodeType::Curvature => curvature::LABEL,
        NodeType::Cavity => cavity::LABEL,
        NodeType::AmbientOcclusion => ambient_occlusion::LABEL,
        NodeType::Slope => slope::LABEL,
        NodeType::VectorMath => vector_math::LABEL,
        NodeType::Clamp => clamp::LABEL,
        NodeType::MixFloat => mix_float::LABEL,
        NodeType::Smoothstep => smoothstep::LABEL,
        NodeType::HeightToNormal => height_to_normal::LABEL,
        NodeType::PrimSdfMask => prim_sdf_mask::LABEL,
        NodeType::SdfCurvature => sdf_curvature::LABEL,
        NodeType::SurfaceDirection => surface_direction::LABEL,
        NodeType::SdfThickness => sdf_thickness::LABEL,
        NodeType::SdfPosition => sdf_position::LABEL,
        NodeType::Reroute => reroute::LABEL,
        NodeType::Frame => frame::LABEL,
        NodeType::Checker => checker::LABEL,
        NodeType::Gamma => gamma::LABEL,
    }
}

pub fn inputs(nt: &NodeType) -> Vec<Socket> {
    match nt {
        NodeType::TexCoord => tex_coord::inputs(),
        NodeType::ObjectInfo => object_info::inputs(),
        NodeType::Noise => noise::inputs(),
        NodeType::Voronoi => voronoi::inputs(),
        NodeType::MathBinary => math_binary::inputs(),
        NodeType::MathUnary => math_unary::inputs(),
        NodeType::Remap => remap::inputs(),
        NodeType::ColorRamp => color_ramp::inputs(),
        NodeType::MixRgb => mix_rgb::inputs(),
        NodeType::BrightnessContrast => brightness_contrast::inputs(),
        NodeType::SeparateXyz => separate_xyz::inputs(),
        NodeType::CombineXyz => combine_xyz::inputs(),
        NodeType::Triplanar => triplanar::inputs(),
        NodeType::BoxMapping => box_mapping::inputs(),
        NodeType::CylindricalMapping => cylindrical_mapping::inputs(),
        NodeType::SphericalMapping => spherical_mapping::inputs(),
        NodeType::PlanarMapping => planar_mapping::inputs(),
        NodeType::UVTransform => uv_transform::inputs(),
        NodeType::TextureSample => texture_sample::inputs(),
        NodeType::GradientTexture => gradient_texture::inputs(),
        NodeType::WaveTexture => wave_texture::inputs(),
        NodeType::MusgraveTexture => musgrave_texture::inputs(),
        NodeType::BrickTexture => brick_texture::inputs(),
        NodeType::MaterialOutput => material_output::inputs(),
        NodeType::ConstantColor => constant_color::inputs(),
        NodeType::ImageTexture => image_texture::inputs(),
        NodeType::ConstantValue => constant_value::inputs(),
        NodeType::Invert => invert::inputs(),
        NodeType::ColorCorrect => color_correct::inputs(),
        NodeType::Curvature => curvature::inputs(),
        NodeType::Cavity => cavity::inputs(),
        NodeType::AmbientOcclusion => ambient_occlusion::inputs(),
        NodeType::Slope => slope::inputs(),
        NodeType::VectorMath => vector_math::inputs(),
        NodeType::Clamp => clamp::inputs(),
        NodeType::MixFloat => mix_float::inputs(),
        NodeType::Smoothstep => smoothstep::inputs(),
        NodeType::HeightToNormal => height_to_normal::inputs(),
        NodeType::PrimSdfMask => prim_sdf_mask::inputs(),
        NodeType::SdfCurvature => sdf_curvature::inputs(),
        NodeType::SurfaceDirection => surface_direction::inputs(),
        NodeType::SdfThickness => sdf_thickness::inputs(),
        NodeType::SdfPosition => sdf_position::inputs(),
        NodeType::Reroute => reroute::inputs(),
        NodeType::Frame => frame::inputs(),
        NodeType::Checker => checker::inputs(),
        NodeType::Gamma => gamma::inputs(),
    }
}

pub fn outputs(nt: &NodeType) -> Vec<Socket> {
    match nt {
        NodeType::TexCoord => tex_coord::outputs(),
        NodeType::ObjectInfo => object_info::outputs(),
        NodeType::Noise => noise::outputs(),
        NodeType::Voronoi => voronoi::outputs(),
        NodeType::MathBinary => math_binary::outputs(),
        NodeType::MathUnary => math_unary::outputs(),
        NodeType::Remap => remap::outputs(),
        NodeType::ColorRamp => color_ramp::outputs(),
        NodeType::MixRgb => mix_rgb::outputs(),
        NodeType::BrightnessContrast => brightness_contrast::outputs(),
        NodeType::SeparateXyz => separate_xyz::outputs(),
        NodeType::CombineXyz => combine_xyz::outputs(),
        NodeType::Triplanar => triplanar::outputs(),
        NodeType::BoxMapping => box_mapping::outputs(),
        NodeType::CylindricalMapping => cylindrical_mapping::outputs(),
        NodeType::SphericalMapping => spherical_mapping::outputs(),
        NodeType::PlanarMapping => planar_mapping::outputs(),
        NodeType::UVTransform => uv_transform::outputs(),
        NodeType::TextureSample => texture_sample::outputs(),
        NodeType::GradientTexture => gradient_texture::outputs(),
        NodeType::WaveTexture => wave_texture::outputs(),
        NodeType::MusgraveTexture => musgrave_texture::outputs(),
        NodeType::BrickTexture => brick_texture::outputs(),
        NodeType::MaterialOutput => material_output::outputs(),
        NodeType::ConstantColor => constant_color::outputs(),
        NodeType::ImageTexture => image_texture::outputs(),
        NodeType::ConstantValue => constant_value::outputs(),
        NodeType::Invert => invert::outputs(),
        NodeType::ColorCorrect => color_correct::outputs(),
        NodeType::Curvature => curvature::outputs(),
        NodeType::Cavity => cavity::outputs(),
        NodeType::AmbientOcclusion => ambient_occlusion::outputs(),
        NodeType::Slope => slope::outputs(),
        NodeType::VectorMath => vector_math::outputs(),
        NodeType::Clamp => clamp::outputs(),
        NodeType::MixFloat => mix_float::outputs(),
        NodeType::Smoothstep => smoothstep::outputs(),
        NodeType::HeightToNormal => height_to_normal::outputs(),
        NodeType::PrimSdfMask => prim_sdf_mask::outputs(),
        NodeType::SdfCurvature => sdf_curvature::outputs(),
        NodeType::SurfaceDirection => surface_direction::outputs(),
        NodeType::SdfThickness => sdf_thickness::outputs(),
        NodeType::SdfPosition => sdf_position::outputs(),
        NodeType::Reroute => reroute::outputs(),
        NodeType::Frame => frame::outputs(),
        NodeType::Checker => checker::outputs(),
        NodeType::Gamma => gamma::outputs(),
    }
}


#[cfg(test)]
mod panel_expressiveness_gate {
    /// OWNER 2026-07-29: "I only have access to one mask — imagine I want to drive the curvature
    /// radius with noise, or mask the curvature mask with noise. Our current UI does not let us."
    ///
    /// Both limits are in the PANEL DATA MODEL, not in the node graph:
    ///
    ///   * `Layer.mask` is `Option<LayerContent>` — ONE mask, and `LayerContent` is a leaf, so a
    ///     mask cannot itself carry a mask. No mask-of-a-mask, no stacked masks.
    ///   * Generator params are plain scalars (`radius: f32`), so nothing can DRIVE them.
    ///
    /// The graph underneath already supports both: `SdfCurvature` declares Epsilon (the radius),
    /// Contrast and Strength as real INPUT SOCKETS, which are connectable. The panel just writes
    /// `value_f32` constants into them.
    ///
    /// This test records that asymmetry. It passes today — it exists so the next person reads the
    /// capability as "panel-limited", not "engine-limited", and knows the fix is in the panel's
    /// model rather than in the shader nodes.
    #[test]
    fn curvature_node_exposes_drivable_sockets_the_panel_cannot_reach() {
        let inputs = crate::shader_nodes::nodes::sdf_curvature::inputs();
        let names: Vec<&str> = inputs.iter().map(|s| s.name).collect();

        // The graph CAN be driven on these.
        for want in ["Epsilon", "Contrast", "Strength"] {
            assert!(names.contains(&want),
                "SdfCurvature no longer exposes {want} as an input socket — the panel's scalar                  params would then be the ONLY way to set it, and driving it with noise becomes                  impossible rather than merely unexposed. Sockets: {names:?}");
        }
        // "Position" being an input is what makes projection/warping drivable too.
        assert!(names.contains(&"Position"),
            "SdfCurvature must keep a Position input — it is what allows a warped/driven domain");
    }
}
