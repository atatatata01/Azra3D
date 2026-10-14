// shader_nodes/palette.rs
use super::node_type::NodeType;
use super::types::SocketType;
use super::node_data::Node;

pub struct NodePalette;

impl NodePalette {
    /// Canonical node list — do not duplicate elsewhere; drag-to-add filters this.
    pub fn categories() -> &'static [(&'static str, &'static [(NodeType, &'static str)])] {
        &[
            ("Input", &[
                (NodeType::TexCoord, "Texture Coordinate"),
                (NodeType::ObjectInfo, "Object Info"),
                (NodeType::ConstantColor, "Color"),
                (NodeType::ConstantValue, "Value"),
            ]),
            ("Projection", &[
                (NodeType::Triplanar, "Triplanar"),
                (NodeType::BoxMapping, "Box Mapping"),
                (NodeType::CylindricalMapping, "Cylindrical"),
                (NodeType::SphericalMapping, "Spherical"),
                (NodeType::PlanarMapping, "Planar"),
                (NodeType::UVTransform, "UV Transform"),
            ]),
            ("Texture", &[
                (NodeType::ImageTexture, "Image Texture"),
                (NodeType::TextureSample, "Procedural Sample"),
                (NodeType::GradientTexture, "Gradient"),
                (NodeType::WaveTexture, "Wave"),
                (NodeType::MusgraveTexture, "Musgrave"),
                (NodeType::BrickTexture, "Brick"),
                (NodeType::Checker, "Checker"),
            ]),
            ("Generator", &[
                (NodeType::Noise, "Noise"),
                (NodeType::Voronoi, "Voronoi"),
            ]),
            ("Surface", &[
                (NodeType::PrimSdfMask, "Prim SDF Mask"),
                (NodeType::SdfCurvature, "SDF Curvature"),
                (NodeType::Curvature, "Curvature"),
                (NodeType::Cavity, "Cavity"),
                (NodeType::AmbientOcclusion, "Ambient Occlusion"),
                (NodeType::Slope, "Slope"),
                (NodeType::SurfaceDirection, "Direction"),
                (NodeType::SdfThickness, "Thickness"),
                (NodeType::SdfPosition, "Position"),
            ]),
            ("Layout", &[
                (NodeType::Reroute, "Reroute"),
                (NodeType::Frame, "Frame"),
            ]),
            ("Color", &[
                (NodeType::MixRgb, "Multiply"),
                (NodeType::ColorRamp, "Color Ramp"),
                (NodeType::MixRgb, "Mix Color"),
                (NodeType::BrightnessContrast, "Bright/Contrast"),
                (NodeType::Gamma, "Gamma"),
                (NodeType::ColorCorrect, "Color Correct"),
                (NodeType::Invert, "Invert"),
            ]),
            ("Operations", &[
                (NodeType::MathBinary, "Add"),
                (NodeType::MathBinary, "Subtract"),
                (NodeType::MathBinary, "Value Multiply"),
                (NodeType::MathBinary, "Divide"),
                (NodeType::MathBinary, "Power"),
                (NodeType::MathBinary, "Min"),
                (NodeType::MathBinary, "Max"),
                (NodeType::MathBinary, "Modulo"),
                (NodeType::MathBinary, "Atan2"),
                (NodeType::MathBinary, "Compare"),
                (NodeType::MathBinary, "Step"),
                (NodeType::MathUnary, "Abs"),
                (NodeType::MathUnary, "Negate"),
                (NodeType::MathUnary, "Sqrt"),
                (NodeType::MathUnary, "Sin"),
                (NodeType::MathUnary, "Cos"),
                (NodeType::MathUnary, "Fract"),
                (NodeType::MathUnary, "Floor"),
                (NodeType::MathUnary, "Ceil"),
                (NodeType::MathUnary, "Saturate"),
                (NodeType::MathUnary, "1-x"),
                (NodeType::MathUnary, "Sign"),
                (NodeType::MathUnary, "Exp"),
                (NodeType::MathUnary, "Log"),
                (NodeType::MathUnary, "Round"),
                (NodeType::MathUnary, "Truncate"),
                (NodeType::Remap, "Remap"),
                (NodeType::Clamp, "Clamp"),
                (NodeType::MixFloat, "Mix"),
                (NodeType::Smoothstep, "Smoothstep"),
            ]),
            ("Vector", &[
                (NodeType::SeparateXyz, "Separate XYZ"),
                (NodeType::CombineXyz, "Combine XYZ"),
                (NodeType::VectorMath, "Vector Math"),
                (NodeType::HeightToNormal, "Height to Normal"),
            ]),
            ("Output", &[
                (NodeType::MaterialOutput, "Master Material"),
            ]),
        ]
    }

    /// Nodes that can complete a link from a dragged socket.
    /// `from_is_output`: true if user dragged FROM an output (need a compatible input on the new node).
    /// Returns (NodeType, label, socket_index on the NEW node to connect).
    pub fn link_targets(
        from_is_output: bool,
        sock: SocketType,
    ) -> Vec<(NodeType, &'static str, usize)> {
        let mut out = Vec::new();
        for (_cat, entries) in Self::categories() {
            for (nt, label) in *entries {
                if *nt == NodeType::MaterialOutput && from_is_output {
                    // Can still wire into MaterialOutput inputs
                }
                if from_is_output {
                    let inputs = Node::input_defs(nt);
                    for (i, inp) in inputs.iter().enumerate() {
                        if sock.compatible(&inp.socket_type) {
                            out.push((nt.clone(), *label, i));
                            break; // first matching input
                        }
                    }
                } else {
                    let outputs = Node::output_defs(nt);
                    for (i, o) in outputs.iter().enumerate() {
                        if o.socket_type.compatible(&sock) {
                            out.push((nt.clone(), *label, i));
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

