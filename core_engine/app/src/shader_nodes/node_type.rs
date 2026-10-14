// shader_nodes/node_type.rs
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeType {
    // --- Input nodes (no inputs, only outputs) ---
    TexCoord,          // outputs: position, normal
    ObjectInfo,        // outputs: prim_idx (float)

    // --- Generator nodes ---
    Noise,             // Full noise suite (type param) + built-in projection → value
    Voronoi,           // Voronoi cells: inputs: position, scale â†’ output: distance, cell_id

    // --- Math nodes ---
    MathBinary,        // inputs: a, b â†’ output: result. Operation set by param
    MathUnary,         // inputs: value â†’ output: result. Operation set by param
    Remap,             // inputs: value, from_min, from_max, to_min, to_max â†’ output

    // --- Color nodes ---
    ColorRamp,         // inputs: fac â†’ output: color (2-stop gradient)
    MixRgb,            // inputs: fac, color1, color2 â†’ output: color
    BrightnessContrast,// inputs: color, brightness, contrast â†’ output

    // --- Vector nodes ---
    SeparateXyz,       // inputs: vec3 â†’ outputs: x, y, z
    CombineXyz,        // inputs: x, y, z â†’ output: vec3

    // --- Texture projection nodes ---
    Triplanar,         // inputs: position, normal, sharpness, scale â†’ output: blend_weights(vec3), uv_x/uv_y/uv_z as vec3
    BoxMapping,        // inputs: position, scale â†’ output: uv(vec3), face_id(float)
    CylindricalMapping,// inputs: position, axis(float), scale â†’ output: uv(vec3)
    SphericalMapping,  // inputs: position, scale â†’ output: uv(vec3)
    PlanarMapping,     // inputs: position, axis(float), scale â†’ output: uv(vec3)
    UVTransform,       // inputs: uv, offset_x, offset_y, rotation, scale_x, scale_y â†’ output: uv

    // --- Texture pattern nodes ---
    TextureSample,     // inputs: uv, pattern(float: 0=checker, 1=gradient, 2=brick) â†’ output: color, alpha
    GradientTexture,   // inputs: uv, mode(float: 0=linear, 1=radial, 2=spherical) â†’ output: value, color
    WaveTexture,       // inputs: uv, scale, distortion, profile(float: 0=sine, 1=saw, 2=triangle) â†’ output: value, color
    MusgraveTexture,   // inputs: position, scale, detail, dimension, lacunarity â†’ output: value
    BrickTexture,      // inputs: uv, scale, mortar_size, row_offset â†’ output: color, fac

    // --- Output node (exactly one per graph) ---
    /// Terminal Master Material — base PBR + Advanced (clearcoat/sheen/SSS/IOR/…).
    MaterialOutput,

    // --- Append-only (bincode / serde enum tag order) ---
    /// Constant solid color source (Basic Color).
    ConstantColor,
    /// Samples a map from the owning Material's map slots (same tex_id / Load Image path as Properties UI).
    /// `Node.image_map_kind` = MapKind wire tag (0=Albedo, â€¦). Load Image queues the shared PendingMapLoad.
    ImageTexture,

    // --- v1 catalog (PROCEDURAL_TEXTURING_DESIGN) — append only ---
    ConstantValue,
    Invert,
    ColorCorrect,
    Curvature,
    Cavity,
    AmbientOcclusion,
    Slope,
    VectorMath,
    Clamp,
    MixFloat,
    Smoothstep,
    /// Height float → tangent-space Normal vec3 (finite differences / derivative).
    HeightToNormal,

    // --- SDF surface tools (append only) ---
    /// Sample a scene primitive's SDF as a mask (alpha) for locating effects.
    /// `Node.sdf_prim_uid` selects the prim; grow/shrink + soft band control the mask.
    PrimSdfMask,
    /// Explicit SDF curvature (convex/concave region) with grow/shrink, contrast, strength.
    /// Prefer this over the analytic-field Curvature when body SDF is available at bake.
    SdfCurvature,
    /// Angle mask from the SURFACE NORMAL vs a chosen world axis (`sdf_curv_region` = axis code).
    /// Real normal from the body SDF gradient on the bake path; the true per-pixel geometric
    /// normal in the live WGSL path. "Snow on top", "dirt underneath", directional wear.
    SurfaceDirection,
    /// Interior thickness along −normal (IQ-style: step into the surface, read |SDF|).
    /// Artistic thickness / edge-wear mask. APPEND-ONLY — after SurfaceDirection.
    SdfThickness,
    /// Body-local position remapped to 0..1 RGB across the body AABB (Position generator).
    SdfPosition,

    // --- G8 structure nodes (append only) ---
    /// Wire passthrough (Blender Reroute): evaluates as input 0 on every output.
    /// GPU bake path DECLINES graphs containing one (not in `node_type_gpu_bakeable`) —
    /// CPU bake is correct; WGSL passthrough is a queued optimization.
    Reroute,
    /// Visual grouping box (Blender Frame). Zero sockets, zero evaluation impact —
    /// display-only; dragging its header moves every node inside its rect.
    Frame,

    // --- G9 Blender node breadth (append only) ---
    /// Blender Checker Texture: alternating-cube parity of floor(pos*scale).
    Checker,
    /// Blender Gamma: per-channel pow(max(color, 0), gamma).
    /// APPEND-ONLY — last variant; add new ones AFTER it.
    Gamma,
}

