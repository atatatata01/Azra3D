// ============================================================
// Color-ID Picking Shader
// ============================================================
//
// Renders meshes with flat per-instance NodeId colors for
// GPU-based selection picking. Each pixel stores a 32-bit
// node ID encoded as 4 × u8 in an Rgba8Uint render target.
//
// NodeId 0 = background / no hit (root node, never rendered).

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    near_far: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniforms;

// Uses the same vertex + instance layout as the G-buffer pipeline.
// The `material_idx` slot carries the node ID for picking.

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // Instance data
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    @location(6) model_3: vec4<f32>,
    @location(7) normal_mat_0: vec4<f32>,
    @location(8) normal_mat_1: vec4<f32>,
    @location(9) normal_mat_2: vec4<f32>,
    @location(10) material_idx: u32, // carries node_id for picking
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) node_id: u32,
};

@vertex
fn vs_picking(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let world_pos = (model * vec4(in.position, 1.0)).xyz;

    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4(world_pos, 1.0);
    out.node_id = in.material_idx;
    return out;
}

@fragment
fn fs_picking(in: VertexOutput) -> @location(0) vec4<u32> {
    return vec4<u32>(
        in.node_id & 0xFFu,
        (in.node_id >> 8u) & 0xFFu,
        (in.node_id >> 16u) & 0xFFu,
        (in.node_id >> 24u) & 0xFFu,
    );
}
