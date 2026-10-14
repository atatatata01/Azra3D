// Phase D2 — Hi-Z (HZB) max-depth pyramid build (PHASE_D2_HZB_PLAN.md).
//
// AZRA raster depth is Depth32Float with compare=Less (standard Z: near=0, far=1),
// so the pyramid is a MAX reduce: mip N texel = farthest depth of its 2x2 mip N-1
// footprint. An instance is occluded iff its NEAREST projected depth is still
// farther than the MAX occluder depth over its screen footprint.
//
// Two entry points:
//   cs_hzb_seed       — copy the depth attachment into mip 0 of the pow2-padded
//                       R32Float pyramid. Texels outside the viewport get 1.0 (far),
//                       so padding can never occlude and every downsample level is
//                       an exact 2x2 reduce (no NPOT edge cases).
//   cs_hzb_downsample — one dispatch per mip: 2x2 max from the previous mip.
//
// Bind group layout: graphics::bgl::hzb_build_bgl_entries() — single source.

struct HzbParams {
    // Seed: viewport (valid depth) size in texels. Downsample: source mip size.
    src_size: vec2<u32>,
    // Destination mip size.
    dst_size: vec2<u32>,
}

// One WGSL global per binding slot: the seed's depth-attachment source and the
// downsample's R32Float source are DIFFERENT texture types, so they occupy
// different bindings (1 vs 3). Each entry point's BGL only lists its own bindings
// (hzb_seed_bgl_entries / hzb_down_bgl_entries).
@group(0) @binding(0) var<uniform> params: HzbParams;
// Seed source: the raster Depth32Float attachment.
@group(0) @binding(1) var src_attach: texture_depth_2d;
// Destination HZB mip (R32Float storage).
@group(0) @binding(2) var dst_mip: texture_storage_2d<r32float, write>;
// Downsample source: HZB mip N-1.
@group(0) @binding(3) var src_mip: texture_2d<f32>;

@compute @workgroup_size(8, 8)
fn cs_hzb_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_size.x || gid.y >= params.dst_size.y) { return; }
    var d = 1.0; // far — padding outside the viewport can never occlude
    if (gid.x < params.src_size.x && gid.y < params.src_size.y) {
        d = textureLoad(src_attach, vec2<i32>(gid.xy), 0);
    }
    textureStore(dst_mip, vec2<i32>(gid.xy), vec4<f32>(d, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(8, 8)
fn cs_hzb_downsample(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_size.x || gid.y >= params.dst_size.y) { return; }
    let base = vec2<i32>(gid.xy) * 2;
    let d0 = textureLoad(src_mip, base, 0).x;
    let d1 = textureLoad(src_mip, base + vec2<i32>(1, 0), 0).x;
    let d2 = textureLoad(src_mip, base + vec2<i32>(0, 1), 0).x;
    let d3 = textureLoad(src_mip, base + vec2<i32>(1, 1), 0).x;
    let m = max(max(d0, d1), max(d2, d3));
    textureStore(dst_mip, vec2<i32>(gid.xy), vec4<f32>(m, 0.0, 0.0, 0.0));
}
