// ============================================================
// AZRA — Reusable Texture Blend Compute Shader
// ============================================================
//
// Composites two textures with configurable blend mode.
// Standard building block for debug overlays, selection highlights,
// wireframe-on-shaded, x-ray, and any feature that combines two
// rendered results.
//
// Inputs:  tex_base (background), tex_overlay (foreground)
// Output:  tex_output (blended result)
// Params:  tint, mode, opacity
//
// CONSTRAINT: tex_base and tex_output MUST be different textures.
// WGSL forbids reading and writing the same texture in one dispatch.

struct BlendParams {
    tint: vec4<f32>,     // 16B — color tint applied to overlay before blending
    mode: u32,           // 4B  — 0=alpha_over, 1=additive, 2=multiply, 3=replace_where_hit
    opacity: f32,        // 4B  — master overlay opacity (0.0-1.0)
    _pad: vec2<f32>,     // 8B  — alignment to 32B
}

@group(0) @binding(0) var tex_base: texture_2d<f32>;
@group(0) @binding(1) var tex_overlay: texture_2d<f32>;
@group(0) @binding(2) var tex_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> params: BlendParams;

@compute @workgroup_size(8, 8, 1)
fn blend_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(tex_base);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let coord = vec2<i32>(gid.xy);

    let base = textureLoad(tex_base, coord, 0);
    let over_raw = textureLoad(tex_overlay, coord, 0);

    // Apply tint + master opacity
    let over = vec4<f32>(
        over_raw.rgb * params.tint.rgb,
        over_raw.a * params.tint.a * params.opacity
    );

    var result: vec4<f32>;
    switch params.mode {
        case 0u: {
            // Alpha Over (Porter-Duff)
            result = vec4<f32>(
                over.rgb * over.a + base.rgb * (1.0 - over.a),
                over.a + base.a * (1.0 - over.a)
            );
        }
        case 1u: {
            // Additive
            result = vec4<f32>(base.rgb + over.rgb * over.a, base.a);
        }
        case 2u: {
            // Multiply
            result = vec4<f32>(
                mix(base.rgb, base.rgb * over.rgb, over.a),
                base.a
            );
        }
        case 3u: {
            // Replace Where Hit (overlay.a > threshold = full replace)
            let mask = step(0.001, over.a);
            result = mix(base, vec4<f32>(over.rgb, 1.0), mask * params.opacity);
        }
        case 4u: {
            // MaskOutline: overlay is the selected-prim coverage MASK from the
            // prim_outline pass (r = additive coverage, g = cutter coverage,
            // a = max coverage). Edge = coverage discontinuity in the 3x3
            // neighborhood -> crisp 1-2px contour. Cutter interiors additionally
            // get a pale fill so the user sees the subtract volume through the
            // body. Additive edges use params.tint; cutter pixels shift to a
            // fixed red so booleans read at a glance.
            var cov_min = over_raw.a;
            var cov_max = over_raw.a;
            for (var dy = -1; dy <= 1; dy = dy + 1) {
                for (var dx = -1; dx <= 1; dx = dx + 1) {
                    let c = clamp(coord + vec2<i32>(dx, dy),
                                  vec2<i32>(0, 0),
                                  vec2<i32>(dims) - vec2<i32>(1, 1));
                    let a = textureLoad(tex_overlay, c, 0).a;
                    cov_min = min(cov_min, a);
                    cov_max = max(cov_max, a);
                }
            }
            let edge = step(0.5, cov_max - cov_min);
            let is_cutter = step(0.5, over_raw.g);
            let sub_fill = is_cutter * over_raw.a * (1.0 - edge) * 0.16;
            let alpha = clamp(edge + sub_fill, 0.0, 1.0) * params.opacity;
            let cutter_col = vec3<f32>(1.0, 0.35, 0.28);
            let col = mix(params.tint.rgb, cutter_col, is_cutter);
            result = vec4<f32>(col * alpha + base.rgb * (1.0 - alpha), base.a);
        }
        default: {
            result = base;
        }
    }

    textureStore(tex_output, coord, result);
}
