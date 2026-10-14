// ============================================================
// Depth-Tested Face Selection Overlay (Blender Overlay parity)
// ============================================================
//
// Selected poly faces as transparent triangle fills, occluded by the SAME
// G-buffer depth the solid raster pass wrote — Blender's Overlay engine does
// the same (`test_occlusion()` against `depth_tx` in overlay_edit_mesh_*.glsl).
//
// WHY THIS EXISTS:
// Face selection used to be flat egui `convex_polygon` with NO depth test
// (always-on-top). At grazing angles the screen-space fill "bled" past the
// real face silhouette onto adjacent faces / through the mesh. Depth-tested
// GPU triangles answer "is something else in front?" the way Blender does.
//
// OCCLUSION is resolved in the fragment shader (depth TEXTURE sample), not a
// hardware depth attachment — same architecture as line_overlay.wgsl. The fill
// never writes depth, so it cannot occlude the geometry it annotates.
//
// COPLANAR BIAS: selected faces are coplanar with the solid. Two parts:
//   1. `NDC_OFFSET` in vs_main pulls the fill toward the near plane in CLIP
//      space — Blender's `ndc_offset` trick. This replaced a CPU-side nudge
//      along the FACE NORMAL (overlay_faces.rs), which assumed the normal
//      pointed at the camera: boolean notch walls and flipped-winding faces
//      got biased INTO the solid and their highlight vanished on orbit
//      (owner 2026-08-07). A clip-space offset has no normal dependency.
//   2. A base epsilon in scene_visible() absorbs float-depth ULP noise so
//      equality draws; a one-sided min gradient keeps silhouette cliffs from
//      leaking (same cliff-safe pattern as the wire pass).

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    near_far: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var scene_depth: texture_depth_2d;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// Depth convention is standard forward-Z (`CompareFunction::Less`, render/src/pipeline.rs):
// SMALLER window z is nearer, so pulling the fill toward the viewer SUBTRACTS. Applied as a
// multiple of w, which makes it a constant offset in NDC (and therefore in the 0..1 window
// depth the solid pass wrote) at any distance or zoom — unlike a world-space nudge, which is
// invisible far away and floats off the surface up close.
const NDC_OFFSET: f32 = 1.2e-4;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    var clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    clip.z = clip.z - NDC_OFFSET * clip.w;
    out.clip_position = clip;
    out.color = in.color;
    return out;
}

// One-sided minimum gradient (cliff-safe — see line_overlay.wgsl).
fn axis_grad(d0: f32, dp: f32, dm: f32) -> f32 {
    return min(abs(dp - d0), abs(d0 - dm));
}

fn scene_visible(frag: vec4<f32>) -> bool {
    let dim = vec2<i32>(textureDimensions(scene_depth));
    let p = clamp(vec2<i32>(frag.xy), vec2<i32>(0), dim - vec2<i32>(1));
    let d0 = textureLoad(scene_depth, p, 0);
    let dx1 = textureLoad(scene_depth, clamp(p + vec2<i32>(1, 0), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dx0 = textureLoad(scene_depth, clamp(p - vec2<i32>(1, 0), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dy1 = textureLoad(scene_depth, clamp(p + vec2<i32>(0, 1), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dy0 = textureLoad(scene_depth, clamp(p - vec2<i32>(0, 1), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    // Flat coplanar faces: base eps + the vs_main NDC_OFFSET absorb raster/float noise so the
    // fill wins equality against the solid. Steep slopes
    // get slope-aware slack; cliffs keep the one-sided min so the fill cannot bleed past a
    // silhouette. Slightly looser base than the wire pass (faces are coplanar by design).
    let grad = max(axis_grad(d0, dx1, dx0), axis_grad(d0, dy1, dy0));
    let eps = grad * 1.5 + 5e-4;
    return frag.z <= d0 + eps;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (!scene_visible(in.clip_position)) {
        discard;
    }
    return in.color;
}

// X-Ray (Visible-Only OFF): draw through occluders so selection is always readable.
@fragment
fn fs_xray(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
