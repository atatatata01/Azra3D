// AZRA — Auto Reflection Probe capture + prefilter (PROGRESSIVE_PBR_PLAN.md Phase 3)
//
// A probe face is rendered through the REAL raster + deferred pipeline (probe camera,
// ≥90° cone) into the full-res HDR texture; `probe_capture` then resolves one 128²
// cube-face layer from it, and `probe_mip_reduce` builds the roughness mip chain
// (2×2 box — the documented approximation of a GGX prefilter).
//
// ORIENTATION IS SELF-CORRECTING: for every destination texel this shader computes
// the SPEC-DEFINED cube direction (`cube_dir`, the standard GL/Vulkan/WebGPU face
// layout the hardware `textureSampleLevel(cube, dir)` uses) and re-projects that
// direction into the rendered image using the SAME camera basis the face was
// rendered with (carried in the params). Capture and sampling therefore cannot
// disagree, regardless of which handed basis the render camera used.

struct ProbeCaptureParams {
    fwd: vec4<f32>,   // xyz = render camera forward, w = cube face index (0..5)
    right: vec4<f32>, // xyz = render camera right,   w = src_min.x (texels)
    up: vec4<f32>,    // xyz = render camera up,      w = src_min.y (texels)
    // x = central-square side in src texels (the 90° cone), y = dest face size,
    // z = tan(half-fov) of the rendered image's SQUARE region (1.0 = exactly 90°).
    sizes: vec4<f32>,
};

@group(0) @binding(0) var cap_src: texture_2d<f32>;
@group(0) @binding(1) var cap_dst: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> cap: ProbeCaptureParams;
// G-buffer body_index target of the probe frame. gbuffer.wgsl writes
// PROBE_BACKFACE_MARK (-1) there for back-facing fragments in probe frames.
@group(0) @binding(8) var cap_facing: texture_2d<f32>;

// PROBE VALIDITY (AUTO-PROBE-INSIDE-GEOMETRY). Cube ALPHA = the fraction of the
// texel's directions that are FREE: they hit nothing, or hit a surface from its
// front. A back-face hit means the eye is inside solid material along that ray
// (inside a lone sphere: every ray; inside a room: none — its walls face in).
// Alpha rides the mip chain (box-averaged) to the SH mip, where `probe_sh_project`
// integrates it over the sphere; the CPU publishes the probe only when that free
// fraction clears `PROBE_MIN_FREE_FRACTION` (probe_bake.rs). Same rule as
// Unity APV's back-face validity. No consumer samples cube alpha otherwise.

// Standard cube-face direction for face-local coords uc,vc in [-1,1]
// (+u = texel x rightward, +v = texel y DOWNWARD — the t axis of the spec).
fn cube_dir(face: u32, uc: f32, vc: f32) -> vec3<f32> {
    switch face {
        case 0u: { return vec3( 1.0, -vc, -uc); } // +X
        case 1u: { return vec3(-1.0, -vc,  uc); } // -X
        case 2u: { return vec3( uc,  1.0,  vc); } // +Y
        case 3u: { return vec3( uc, -1.0, -vc); } // -Y
        case 4u: { return vec3( uc, -vc,  1.0); } // +Z
        default: { return vec3(-uc, -vc, -1.0); } // -Z
    }
}

@compute @workgroup_size(8, 8, 1)
fn probe_capture(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_size = u32(cap.sizes.y);
    if (gid.x >= dst_size || gid.y >= dst_size) { return; }
    let face = u32(cap.fwd.w);
    let src_size = cap.sizes.x;
    let src_min = vec2<f32>(cap.right.w, cap.up.w);
    let inv_tan = 1.0 / max(cap.sizes.z, 1e-4);
    let src_dim = vec2<i32>(textureDimensions(cap_src));

    // 2×2 stratified supersample over this dest texel's solid-angle footprint.
    // D4: bilinear taps (was nearest) — kills inter-bake shimmer when the capture
    // source and face texel grids are incommensurate.
    var sum = vec3<f32>(0.0);
    var free_dirs = 0.0;
    for (var j = 0u; j < 2u; j++) {
        for (var i = 0u; i < 2u; i++) {
            let uc = ((f32(gid.x) + f32(i) * 0.5 + 0.25) / f32(dst_size)) * 2.0 - 1.0;
            let vc = ((f32(gid.y) + f32(j) * 0.5 + 0.25) / f32(dst_size)) * 2.0 - 1.0;
            let d = normalize(cube_dir(face, uc, vc));
            // Re-project into the rendered image: perspective divide onto the
            // camera plane. Within a face the forward dot is ≥ 1/sqrt(3) — safe.
            let denom = max(dot(d, cap.fwd.xyz), 1e-4);
            let px = dot(d, cap.right.xyz) / denom * inv_tan; // [-1,1] across the square
            let py = dot(d, cap.up.xyz) / denom * inv_tan;
            let sx = src_min.x + (px * 0.5 + 0.5) * src_size;
            let sy = src_min.y + (0.5 - py * 0.5) * src_size; // screen y is down
            // Bilinear: load the 4 texels around (sx, sy) and weight by fractional part.
            let sx0 = floor(sx);
            let sy0 = floor(sy);
            let fx = sx - sx0;
            let fy = sy - sy0;
            let x0 = clamp(i32(sx0), 0, src_dim.x - 1);
            let y0 = clamp(i32(sy0), 0, src_dim.y - 1);
            let x1 = clamp(x0 + 1, 0, src_dim.x - 1);
            let y1 = clamp(y0 + 1, 0, src_dim.y - 1);
            let c00 = textureLoad(cap_src, vec2<i32>(x0, y0), 0).rgb;
            let c10 = textureLoad(cap_src, vec2<i32>(x1, y0), 0).rgb;
            let c01 = textureLoad(cap_src, vec2<i32>(x0, y1), 0).rgb;
            let c11 = textureLoad(cap_src, vec2<i32>(x1, y1), 0).rgb;
            let c0 = mix(c00, c10, fx);
            let c1 = mix(c01, c11, fx);
            sum += mix(c0, c1, fy);
            // Facing is a per-fragment label, not radiance — nearest texel, no blend.
            let fx_n = clamp(i32(sx), 0, src_dim.x - 1);
            let fy_n = clamp(i32(sy), 0, src_dim.y - 1);
            let mark = textureLoad(cap_facing, vec2<i32>(fx_n, fy_n), 0).r;
            free_dirs += select(1.0, 0.0, mark < -0.5);
        }
    }
    textureStore(cap_dst, vec2<i32>(gid.xy), vec4<f32>(sum * 0.25, free_dirs * 0.25));
}

@group(0) @binding(3) var mipc_src: texture_cube<f32>;  // single-mip cube view (level M−1)
@group(0) @binding(7) var mipc_samp: sampler;
@group(0) @binding(4) var mipc_dst: texture_storage_2d_array<rgba16float, write>; // level M, 6 layers

// Cube mip M−1 → M, 2×2 box in DIRECTION space, all 6 faces per dispatch
// (gid.z = face). Sampling goes THROUGH the cube with a sampler — hardware cube
// filtering is seamless across face edges, so neighbor-face radiance flows into
// the pyramid. The old per-face 2D box reduce could not see across edges; each
// face's mip content drifted toward its own average, and rough reflections
// painted the face BOUNDARIES as curved seam lines across flat walls (owner
// "still see a weird reflection" screenshot, 2026-08-07). Built once per
// completed bake round (Phase D), when all six mip-0 faces are present.
@compute @workgroup_size(8, 8, 1)
fn probe_mip_reduce_cube(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dim = textureDimensions(mipc_dst);
    if (gid.x >= dim.x || gid.y >= dim.y) { return; }
    let face = gid.z;
    // rgba: alpha (free fraction, see probe_capture) averages down with the radiance.
    var sum = vec4<f32>(0.0);
    for (var j = 0u; j < 2u; j++) {
        for (var i = 0u; i < 2u; i++) {
            let uc = ((f32(gid.x) + f32(i) * 0.5 + 0.25) / f32(dim.x)) * 2.0 - 1.0;
            let vc = ((f32(gid.y) + f32(j) * 0.5 + 0.25) / f32(dim.y)) * 2.0 - 1.0;
            let d = normalize(cube_dir(face, uc, vc));
            sum += textureSampleLevel(mipc_src, mipc_samp, d, 0.0);
        }
    }
    textureStore(mipc_dst, vec2<i32>(gid.xy), i32(face), sum * 0.25);
}

// ── Auto LIGHT probe: SH9 irradiance projection (Phase 3b, owner 2026-08-07
// "the auto probe should auto place light probe") ───────────────────────────
// Projects a small mip of the baked reflection cube (8×8×6 texels — the scene's
// low-frequency radiance) into 9 spherical-harmonic RGB coefficients. Deferred
// evaluates them per pixel (Ramamoorthi irradiance) as the room's DIFFUSE ambient:
// off-screen-stable bounce light that screen-space GI structurally cannot supply.
// One thread — 384 texels is nothing, and a single writer needs no reduction.

struct ProbeShOut {
    // [0..8] = SH9 RGB coefficients; [9] = (strength — CPU-written, free fraction —
    // GPU-written here, 0, valid — CPU-written from the readback verdict).
    coeffs: array<vec4<f32>, 10>,
};
@group(0) @binding(5) var sh_src: texture_2d_array<f32>;
@group(0) @binding(6) var<storage, read_write> sh_out: ProbeShOut;

const SH_FACE: u32 = 8u;

@compute @workgroup_size(1, 1, 1)
fn probe_sh_project(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x != 0u) { return; }
    var c: array<vec3<f32>, 9>;
    for (var k = 0u; k < 9u; k++) { c[k] = vec3<f32>(0.0); }
    var wsum = 0.0;
    var free_w = 0.0;
    for (var face = 0u; face < 6u; face++) {
        for (var y = 0u; y < SH_FACE; y++) {
            for (var x = 0u; x < SH_FACE; x++) {
                let uc = ((f32(x) + 0.5) / f32(SH_FACE)) * 2.0 - 1.0;
                let vc = ((f32(y) + 0.5) / f32(SH_FACE)) * 2.0 - 1.0;
                let d = normalize(cube_dir(face, uc, vc));
                // Per-texel solid angle of a cube-face grid cell.
                let t2 = 1.0 + uc * uc + vc * vc;
                let w = 4.0 / (sqrt(t2) * t2);
                let texel = textureLoad(sh_src, vec2<i32>(i32(x), i32(y)), i32(face), 0);
                let rad = texel.rgb;
                free_w += texel.a * w;
                c[0] += rad * (0.282095 * w);
                c[1] += rad * (0.488603 * d.y * w);
                c[2] += rad * (0.488603 * d.z * w);
                c[3] += rad * (0.488603 * d.x * w);
                c[4] += rad * (1.092548 * d.x * d.y * w);
                c[5] += rad * (1.092548 * d.y * d.z * w);
                c[6] += rad * (0.315392 * (3.0 * d.z * d.z - 1.0) * w);
                c[7] += rad * (1.092548 * d.x * d.z * w);
                c[8] += rad * (0.546274 * (d.x * d.x - d.y * d.y) * w);
                wsum += w;
            }
        }
    }
    let norm = 4.0 * 3.14159265 / max(wsum, 1e-6);
    for (var k = 0u; k < 9u; k++) {
        sh_out.coeffs[k] = vec4<f32>(c[k] * norm, 0.0);
    }
    // Preserve the CPU-owned strength in .x; publish the solid-angle-weighted free
    // fraction in .y. Valid (.w) FAILS CLOSED to 0: only the CPU sets it, after
    // reading .y back and judging it (probe_bake.rs `probe_collect_verdict`), so a
    // probe whose eye is inside geometry never lights anything.
    let strength = sh_out.coeffs[9].x;
    sh_out.coeffs[9] = vec4<f32>(strength, free_w / max(wsum, 1e-6), 0.0, 0.0);
}
