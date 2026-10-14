// ============================================================
// Depth-Tested Overlay Line Shader
// ============================================================
//
// Draws world-space line segments (edit wireframe, object wireframe overlay)
// depth-tested against the G-buffer raster pass's depth buffer, so an edge behind
// a nearer surface is HIDDEN — the same depth buffer, the same camera, one view.
//
// Each segment is ONE INSTANCE expanded in the vertex shader into a 2-triangle
// screen-space quad (WebGPU has no line-width state; a line-list is locked to 1px,
// which would drop the 0.5–5.0px thickness sliders).
//
// ANTI-ALIASING (params.viewport.z, default on): the quad is expanded by a 1px feather
// ring + round-cap endpoint extensions, and the fragment shader converts exact
// distance-to-segment into a 1px coverage ramp multiplied into alpha. This is the
// single-pass variant of Blender's overlay line AA (Blender packs per-pixel line
// direction+distance into a line-data texture and resolves in a fullscreen pass;
// the ramp math — smoothstep of analytic distance around the true edge — is the same).
// The pass runs at 1spp with no MSAA and draws AFTER FXAA, so without this the quads
// have raw rasterizer staircase edges. AA off = the exact legacy hard-edged quad.
//
// OCCLUSION IS RESOLVED IN THE FRAGMENT SHADER, not by the hardware depth test
// (Blender-style robust wire). History of why:
//   - Hardware LessEqual + DepthBiasState z-fought: the quad reuses the EDGE depth
//     for every thickness-offset pixel, while the solid face depth at those pixels
//     follows the plane equation and is often slightly closer; slope bias only sees
//     the quad's own slope (~0 across its thickness).
//   - A world-space "pull toward camera" (16339cc) fixed coplanar edges but moved
//     the projected xy too → hidden edges behind a SILHOUETTE poked past the contour.
//   - A depth-only pull fixed the leak but any CONSTANT pull loses against steep
//     faces near corners (deficit = face slope × pixel offset, unbounded).
// The correct tolerance is exactly the local depth gradient of the surface under
// the fragment × how far this fragment can sit from the true edge (half the wire
// width). We sample the depth texture and compute a slope-aware epsilon per pixel.
// Cliff-safety: at a silhouette the gradient is a discontinuity, not a slope — using
// the per-axis ONE-SIDED MINIMUM gradient keeps eps small there (one side of a cliff
// is flat), so hidden edges still cannot leak through contours.

// Prefix of `render::camera::CameraUniforms` — the same buffer the G-buffer raster
// pass uses (`RasterPipeline.camera_buffer`), so line depth and mesh depth are
// generated from a bit-identical view_proj. A uniform buffer may be larger than the
// struct a shader declares, so the trailing grab_* fields are simply not named here
// (same prefix trick as picking.wgsl).
struct CameraUniforms {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    near_far: vec2<f32>,
    _pad: vec2<f32>,
};

// xy = viewport size in pixels (needed to expand a pixel width into NDC).
// z  = wire anti-aliasing enable (1.0 on / 0.0 off — Settings → Anti-aliasing,
//      `render.wire_aa_enabled`). Off = the exact legacy hard-edged quad.
// w  = unused padding (uniform buffers are 16-byte aligned).
struct LineParams {
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniforms;
@group(0) @binding(1) var<uniform> params: LineParams;
// The raster pass's depth buffer, bound as a TEXTURE (the overlay pass has no depth
// attachment — occlusion is the fragment shader's job, see header).
@group(0) @binding(2) var scene_depth: texture_depth_2d;

struct LineInstance {
    @location(0) p0: vec3<f32>,
    @location(1) p1: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) width: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Half the rasterized quad width in pixels (INCLUDES the AA feather when on) —
    // scales the slope-aware depth tolerance in fs_main (a fragment can sit up to
    // this far from the true edge).
    @location(1) half_width_px: f32,
    // Screen-space line frame for analytic AA, in PIXELS. These are screen-linear
    // quantities, so they interpolate WITHOUT perspective correction (the quad's two
    // ends carry different clip w; perspective interpolation would bow the ramp).
    //   x = along-axis coordinate relative to the p0 endpoint (< 0 before it)
    //   y = signed perpendicular offset from the centerline
    //   z = segment screen length
    //   w = AA enable flag (constant across the quad)
    @location(2) @interpolate(linear) lpos: vec4<f32>,
};

// Clip-space w below which an endpoint is at/behind the eye. Interpolating a vertex
// with w <= 0 produces a garbage screen position (the projection flips), so a segment
// crossing the near plane is clipped to this plane first.
const EPS_W: f32 = 1e-4;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: LineInstance) -> VertexOutput {
    var out: VertexOutput;

    // Plain projection — NO depth or position pulls (see header: occlusion tolerance
    // lives in the fragment shader where the surface slope is actually known).
    var c0 = camera.view_proj * vec4<f32>(inst.p0, 1.0);
    var c1 = camera.view_proj * vec4<f32>(inst.p1, 1.0);

    // Both endpoints behind the eye — emit a vertex the clipper is guaranteed to reject
    // (wgpu clip volume is 0 <= z <= w, so z < 0 with w = 1 never rasterizes).
    if (c0.w < EPS_W && c1.w < EPS_W) {
        out.clip_position = vec4<f32>(0.0, 0.0, -1.0, 1.0);
        out.color = vec4<f32>(0.0);
        out.half_width_px = 0.0;
        out.lpos = vec4<f32>(0.0);
        return out;
    }
    // One endpoint behind: clip the segment to the near plane. A line is linear in clip
    // space, so a plain lerp of the clip positions is the exact intersection.
    if (c0.w < EPS_W) {
        c0 = mix(c0, c1, (EPS_W - c0.w) / (c1.w - c0.w));
    }
    if (c1.w < EPS_W) {
        c1 = mix(c1, c0, (EPS_W - c1.w) / (c0.w - c1.w));
    }

    let half_vp = params.viewport.xy * 0.5;
    let s0 = (c0.xy / c0.w) * half_vp; // pixel-space endpoints
    let s1 = (c1.xy / c1.w) * half_vp;

    var dir = s1 - s0;
    let len = length(dir);
    // A segment that projects to a single pixel has no direction to offset along; pick an
    // arbitrary axis so it renders as a dot instead of collapsing to NaN via 0/0.
    if (len < 1e-6) {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = dir / len;
    }

    // Sub-pixel widths cannot be rasterized without MSAA: a 0.5px quad falls between pixel
    // centres and drops out entirely. Widen to 1px and carry the lost coverage as alpha —
    // which is what an antialiasing rasterizer does, so the thickness slider keeps its
    // perceptual meaning at the low end instead of the line vanishing.
    // `inst.width` is authored in POINTS (the same slider also drives egui strokes,
    // which are point-based). The render target is now sized in PHYSICAL pixels, so
    // convert here — otherwise a 1.5x DPI target renders every wire at 67% of its
    // authored thickness and the GPU wire visibly disagrees with the egui overlay
    // drawn on top of it. params.viewport.w carries the scale (1.0 = points == texels).
    let dpi = max(params.viewport.w, 0.0001);
    let w_px = max(inst.width * dpi, 1.0);
    let fade = clamp(inst.width * dpi / w_px, 0.0, 1.0);

    // Analytic AA (Blender-style single-pass variant): widen the quad by a 1px feather
    // ring and extend it past each endpoint by the cap radius, then let the fragment
    // shader turn exact distance-to-segment into a 1px coverage ramp + round caps.
    // AA off = zero expansion = the exact legacy hard-edged quad.
    let aa = select(0.0, 1.0, params.viewport.z > 0.5);
    let feather = aa;                          // 1px ramp margin outside the true edge
    let half_w = w_px * 0.5 + feather;         // perpendicular half-extent of the quad
    let cap_ext = aa * half_w;                 // along-axis extension past each endpoint
    let nrm = vec2<f32>(-dir.y, dir.x) * half_w;

    // 6 vertices → 2 triangles over quad corners
    //   0 = (p0, +n)  1 = (p0, -n)  2 = (p1, +n)  3 = (p1, -n)
    // tri A = 0,1,2 ; tri B = 2,1,3
    var idx = vi;
    if (vi == 3u) { idx = 2u; } else if (vi == 4u) { idx = 1u; } else if (vi == 5u) { idx = 3u; }
    let end = idx >> 1u;  // 0 = p0 end, 1 = p1 end
    let side = idx & 1u;  // 0 = +normal, 1 = -normal

    var sp: vec2<f32>;
    var cz: f32;
    var cw: f32;
    var along: f32;
    if (end == 0u) {
        sp = s0 - dir * cap_ext; cz = c0.z; cw = c0.w; along = -cap_ext;
    } else {
        sp = s1 + dir * cap_ext; cz = c1.z; cw = c1.w; along = len + cap_ext;
    }
    let perp = select(half_w, -half_w, side == 1u);
    sp = sp + select(nrm, -nrm, side == 1u);

    // Rebuild a clip position from the offset pixel position, keeping this endpoint's own
    // z/w so the fragment depth stays perspective-correct and comparable to the mesh depth
    // the raster pass wrote. (x,y) are pre-multiplied by w to survive the perspective divide.
    let ndc = sp / half_vp;
    out.clip_position = vec4<f32>(ndc * cw, cz, cw);
    out.color = vec4<f32>(inst.color.rgb, inst.color.a * fade);
    out.half_width_px = half_w;
    out.lpos = vec4<f32>(along, perp, len, aa);
    return out;
}

// Analytic coverage of this pixel by the true (unexpanded) wire: exact distance to the
// segment → a 1px ramp centered on the wire edge, with round end caps. The cap extension
// also mends the notch two adjacent edges used to leave at a shared vertex (butt caps
// meeting at an angle). With AA off, coverage is binary — identical to the legacy quad.
fn wire_coverage(lpos: vec4<f32>, half_width_px: f32) -> f32 {
    if (lpos.w < 0.5) {
        return 1.0; // AA off — the quad was not expanded; every covered pixel is solid
    }
    let along = clamp(lpos.x, 0.0, lpos.z);
    let d = length(vec2<f32>(lpos.x - along, lpos.y));
    // half_width_px includes the 1px feather; the true wire half-width is 1 less.
    return clamp(half_width_px - 1.0 + 0.5 - d, 0.0, 1.0);
}

// One-sided minimum gradient along one axis: |d+ − d0| vs |d0 − d−|, take the SMALLER.
// On a planar slope both sides agree (≈ the true slope); at a silhouette cliff one side
// is flat → the min stays small and hidden edges cannot leak through the contour.
fn axis_grad(d0: f32, dp: f32, dm: f32) -> f32 {
    return min(abs(dp - d0), abs(d0 - dm));
}

fn scene_visible(frag: vec4<f32>, half_width_px: f32) -> bool {
    let dim = vec2<i32>(textureDimensions(scene_depth));
    let p = clamp(vec2<i32>(frag.xy), vec2<i32>(0), dim - vec2<i32>(1));
    let d0 = textureLoad(scene_depth, p, 0);
    let dx1 = textureLoad(scene_depth, clamp(p + vec2<i32>(1, 0), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dx0 = textureLoad(scene_depth, clamp(p - vec2<i32>(1, 0), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dy1 = textureLoad(scene_depth, clamp(p + vec2<i32>(0, 1), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    let dy0 = textureLoad(scene_depth, clamp(p - vec2<i32>(0, 1), vec2<i32>(0), dim - vec2<i32>(1)), 0);
    // Slope-aware tolerance: the surface under this pixel may legitimately be closer than
    // the edge-interpolated wire depth by (its slope × the fragment's distance from the
    // true edge). ×2 covers the diagonal case + interpolation slack; +1.5px covers the
    // rasterization footprint; 1e-5 base absorbs float-depth ULP noise at any distance.
    let grad = max(axis_grad(d0, dx1, dx0), axis_grad(d0, dy1, dy0));
    let eps = grad * (half_width_px + 1.5) * 2.0 + 1e-5;
    return frag.z <= d0 + eps;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (!scene_visible(in.clip_position, in.half_width_px)) {
        discard;
    }
    let cov = wire_coverage(in.lpos, in.half_width_px);
    if (cov <= 0.0) {
        discard; // outside the ramp (feather margin) — skip the blend entirely
    }
    return vec4<f32>(in.color.rgb, in.color.a * cov);
}

// X-Ray: draw through everything (opt-in escape hatch — same as the old Always pipeline).
// Coverage still applies: the quad is feather-expanded in the shared vertex shader, so
// skipping it here would draw the margin solid and overshoot the endpoints.
@fragment
fn fs_xray(in: VertexOutput) -> @location(0) vec4<f32> {
    let cov = wire_coverage(in.lpos, in.half_width_px);
    if (cov <= 0.0) {
        discard;
    }
    return vec4<f32>(in.color.rgb, in.color.a * cov);
}
