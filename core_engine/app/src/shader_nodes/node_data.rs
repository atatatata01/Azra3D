// shader_nodes/node_data.rs
use serde::{Serialize, Deserialize};
use super::types::{Socket, InputDefault};
use super::ops::{
    MathBinaryOp, MathUnaryOp, MixBlend, NoiseBlendCurve, NoiseType, ProjectionStyle, VectorMathOp,
};
use super::node_type::NodeType;
use super::nodes;

pub type NodeId = u32;

fn default_noise_tile() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub node_type: NodeType,
    pub label: String,
    pub pos: [f32; 2],  // UI position
    // Per-node parameters
    pub math_binary_op: MathBinaryOp,
    pub math_unary_op: MathUnaryOp,
    // Overridden default values for unconnected inputs
    pub input_defaults: Vec<InputDefault>,
    // Color ramp stops
    pub ramp_color0: [f32; 3],
    pub ramp_color1: [f32; 3],
    pub ramp_pos0: f32,
    pub ramp_pos1: f32,
    /// ImageTexture: which material map channel to sample / Load Image into (MapKind::wire_tag).
    /// 0 = Albedo (default). Shared with Properties map UI â€” not a second texture system.
    #[serde(default)]
    pub image_map_kind: u32,
    /// ImageTexture: explicit 1-based texture-array slot to sample at bake time.
    /// 0 (default) = legacy behavior — sample the owning material's map slot for
    /// `image_map_kind`. Non-zero pins a SOURCE image independent of what is
    /// currently installed in the map slot (material-panel P3 layer stacks: the
    /// channel's slot receives the baked COMPOSITE, so panel-generated nodes must
    /// keep sampling the original layer image, not the composite — no feedback).
    /// Persistence: FL4NODES graph JSON (name-tagged serde) → `#[serde(default)]`
    /// is additive-safe; never rides bincode.
    #[serde(default)]
    pub image_tex_id: u32,
    /// Noise node type (default Fbm). Old FL4NODES JSON without field loads as Fbm.
    #[serde(default)]
    pub noise_type: NoiseType,
    /// VectorMath op (default Add).
    #[serde(default)]
    pub vector_math_op: VectorMathOp,
    /// MixRgb blend mode (default Mix = legacy factor-lerp). Old FL4NODES JSON
    /// without this field loads as Mix, preserving prior behavior.
    #[serde(default)]
    pub mix_blend: MixBlend,
    /// ConstantValue float.
    #[serde(default)]
    pub const_float: f32,
    /// Built-in projection for Noise / Image / Gradient / etc. Default Triplanar.
    #[serde(default)]
    pub projection: ProjectionStyle,
    /// Blender `M` mute: the node is bypassed — every output evaluates as INPUT 0 passes
    /// through (see `eval_node`'s mute arm and `extract_live_gpu_noise`'s bail). Drawn dimmed.
    /// FL4NODES graph JSON is name-tagged serde, so `default` is additive-safe (never bincode).
    #[serde(default)]
    pub muted: bool,
    /// Blender `H` collapse: header-only draw. Display state only — zero evaluation impact.
    #[serde(default)]
    pub collapsed: bool,
    /// G8 Frame only: box size in canvas units. `[0,0]` (legacy / non-frame) draws the
    /// default size. FL4NODES graph JSON is name-tagged serde — additive-safe.
    #[serde(default)]
    pub frame_size: [f32; 2],
    /// When true (default), noise is **periodic** so maps tile under Repeat (no edge seam).
    /// Scale ≈ number of tile cells across the domain.
    #[serde(default = "default_noise_tile")]
    pub noise_tile: bool,
    /// Per-side UV offset (U,V) for Triplanar / Box faces: X / Y / Z normal.
    /// Single-plane modes use `proj_off_z` + `proj_rot_z` as the active plane.
    #[serde(default)]
    pub proj_off_x: [f32; 2],
    #[serde(default)]
    pub proj_off_y: [f32; 2],
    #[serde(default)]
    pub proj_off_z: [f32; 2],
    /// Per-side UV rotation in **degrees** (2D rotate in the plane, then offset).
    #[serde(default)]
    pub proj_rot_x: f32,
    #[serde(default)]
    pub proj_rot_y: f32,
    #[serde(default)]
    pub proj_rot_z: f32,
    /// Noise output reshape curve (Linear / Smooth / Levels…). Default Linear.
    #[serde(default)]
    pub noise_curve: NoiseBlendCurve,
    /// Power exponent (Power curve) or Mid gamma (Levels). Default 1 = no change.
    #[serde(default = "default_noise_curve_k")]
    pub noise_curve_k: f32,
    /// Levels black point (input value → 0).
    #[serde(default)]
    pub noise_black: f32,
    /// Levels white point (input value → 1). Default 1.
    #[serde(default = "default_noise_white")]
    pub noise_white: f32,
    /// PrimSdfMask / SdfCurvature: which primitive (`prim_uid`) to sample. 0 = none / body field.
    #[serde(default)]
    pub sdf_prim_uid: u32,
    /// SdfCurvature region: 0=Both, 1=Convex (edges), 2=Concave (cavities).
    #[serde(default)]
    pub sdf_curv_region: u32,
}

fn default_noise_curve_k() -> f32 {
    1.0
}
fn default_noise_white() -> f32 {
    1.0
}


impl Node {
    pub fn new(id: NodeId, node_type: NodeType, pos: [f32; 2]) -> Self {
        let label = nodes::label(&node_type).to_string();

        let inputs = Self::input_defs(&node_type);
        let input_defaults: Vec<InputDefault> = inputs.iter().map(|s| {
            InputDefault { value_f32: s.default_f32, value_vec3: s.default_vec3 }
        }).collect();
        let is_const_color = matches!(node_type, NodeType::ConstantColor);

        Self {
            id, node_type, label, pos,
            math_binary_op: MathBinaryOp::Add,
            math_unary_op: MathUnaryOp::Abs,
            input_defaults,
            ramp_color0: if is_const_color {
                [0.8, 0.8, 0.8]
            } else {
                [0.0, 0.0, 0.0]
            },
            ramp_color1: [1.0, 1.0, 1.0],
            ramp_pos0: 0.0,
            ramp_pos1: 1.0,
            muted: false,
            collapsed: false,
            frame_size: [0.0, 0.0],
            image_map_kind: 0, // Albedo
            image_tex_id: 0,   // legacy: sample the material's map slot
            noise_type: NoiseType::Fbm,
            vector_math_op: VectorMathOp::Add,
            mix_blend: MixBlend::default(),
            const_float: 0.5,
            // Texture generators default to triplanar (material-map parity).
            projection: ProjectionStyle::Triplanar,
            noise_tile: true,
            proj_off_x: [0.0, 0.0],
            proj_off_y: [0.0, 0.0],
            proj_off_z: [0.0, 0.0],
            proj_rot_x: 0.0,
            proj_rot_y: 0.0,
            proj_rot_z: 0.0,
            noise_curve: NoiseBlendCurve::Linear,
            noise_curve_k: 1.0,
            noise_black: 0.0,
            noise_white: 1.0,
            sdf_prim_uid: 0,
            sdf_curv_region: 0,
        }
    }

    /// Defensive default read — never panic on short input_defaults (socket expansion).
    pub fn default_f32(node: &Node, idx: usize, fallback: f32) -> f32 {
        node.input_defaults.get(idx).map(|d| d.value_f32).unwrap_or(fallback)
    }
    pub fn default_vec3(node: &Node, idx: usize, fallback: [f32; 3]) -> [f32; 3] {
        node.input_defaults.get(idx).map(|d| d.value_vec3).unwrap_or(fallback)
    }

    /// Pad `input_defaults` to match current socket count (socket expansion / old graphs).
    pub fn pad_input_defaults(&mut self) {
        let defs = Self::input_defs(&self.node_type);
        while self.input_defaults.len() < defs.len() {
            let s = &defs[self.input_defaults.len()];
            self.input_defaults.push(InputDefault {
                value_f32: s.default_f32,
                value_vec3: s.default_vec3,
            });
        }
    }

    pub fn input_defs(node_type: &NodeType) -> Vec<Socket> {
        let mut s = nodes::inputs(node_type);
        // Append Mask so every processor/generator can be masked without a Mix node.
        // Sources / pure mappers / Master keep a clean socket list.
        if Self::has_mask_input(node_type) {
            s.push(Socket::float("Mask", 1.0));
        }
        s
    }

    pub fn output_defs(node_type: &NodeType) -> Vec<Socket> {
        nodes::outputs(node_type)
    }

    /// Whether this node type gets an extra trailing **Mask** float input (default 1).
    pub fn has_mask_input(node_type: &NodeType) -> bool {
        !matches!(
            node_type,
            NodeType::TexCoord
                | NodeType::ObjectInfo
                | NodeType::MaterialOutput
                | NodeType::ConstantValue
                | NodeType::ConstantColor
                | NodeType::SeparateXyz
                | NodeType::CombineXyz
                | NodeType::Triplanar
                | NodeType::BoxMapping
                | NodeType::CylindricalMapping
                | NodeType::SphericalMapping
                | NodeType::PlanarMapping
                | NodeType::UVTransform
                | NodeType::PrimSdfMask
                | NodeType::SdfCurvature
                | NodeType::SurfaceDirection
                | NodeType::SdfThickness
                | NodeType::SdfPosition
        )
    }

    /// Index of the Mask socket, if any (always last when present).
    pub fn mask_input_index(node_type: &NodeType) -> Option<usize> {
        if Self::has_mask_input(node_type) {
            Some(Self::input_defs(node_type).len().saturating_sub(1))
        } else {
            None
        }
    }
}

// ====================================================================
// Connection between two sockets
// ====================================================================
