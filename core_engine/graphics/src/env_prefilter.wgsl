// ============================================================
// AZRA — Environment GGX Prefilter (split-sum first term)
// One dispatch per mip level, driven from poll (budgeted: 1 mip / frame).
// Reads equirect level 0 of the env buffer, writes mip `dst_offset` in place.
// Buffer layout matches sample_env_at_level in sdf_lighting.wgsl:
//   [L0: src_w*src_h*3][L1]...[L6][SH9: 27 floats]
// Level 0 is never written (mip 0 == source, by construction).
// Self-contained — no sdf_shared.wgsl dependency.
// ============================================================

struct PrefilterParams {
    src_w: u32,
    src_h: u32,
    dst_offset: u32,   // float index of the destination mip
    dst_w: u32,
    dst_h: u32,
    sample_count: u32,
    roughness: f32,    // 0..1 (mip / 6)
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: PrefilterParams;
@group(0) @binding(1) var<storage, read_write> env: array<f32>;

const PI: f32 = 3.14159265;

// Bilinear sample of level 0 (equirect, wrap-X / clamp-Y) — mirrors sample_env_at_level.
fn sample_level0(dir: vec3<f32>) -> vec3<f32> {
    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x);
    let u = (phi + PI) / (2.0 * PI);
    let v = theta / PI;
    let fw = f32(params.src_w);
    let fh = f32(params.src_h);
    let fx = u * fw - 0.5;
    let fy = v * fh - 0.5;
    let ix0 = u32(floor(fx) + fw) % params.src_w;
    let iy0 = u32(clamp(floor(fy), 0.0, fh - 1.0));
    let ix1 = (ix0 + 1u) % params.src_w;
    let iy1 = min(iy0 + 1u, params.src_h - 1u);
    let sx = fract(fx + fw);
    let sy = fract(fy + fh);
    let b00 = (iy0 * params.src_w + ix0) * 3u;
    let b10 = (iy0 * params.src_w + ix1) * 3u;
    let b01 = (iy1 * params.src_w + ix0) * 3u;
    let b11 = (iy1 * params.src_w + ix1) * 3u;
    let c00 = vec3(env[b00], env[b00 + 1u], env[b00 + 2u]);
    let c10 = vec3(env[b10], env[b10 + 1u], env[b10 + 2u]);
    let c01 = vec3(env[b01], env[b01 + 1u], env[b01 + 2u]);
    let c11 = vec3(env[b11], env[b11 + 1u], env[b11 + 2u]);
    return mix(mix(c00, c10, sx), mix(c01, c11, sx), sy);
}

fn radical_inverse_vdc(bits_in: u32) -> f32 {
    var bits = bits_in;
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
    bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
    bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
    bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
    return f32(bits) * 2.3283064e-10;
}

// GGX importance sample around +Z (Karis), alpha = roughness².
fn importance_ggx(xi: vec2<f32>, alpha: f32) -> vec3<f32> {
    let phi = 2.0 * PI * xi.x;
    let cos_th = sqrt((1.0 - xi.y) / (1.0 + (alpha * alpha - 1.0) * xi.y));
    let sin_th = sqrt(max(1.0 - cos_th * cos_th, 0.0));
    return vec3(cos(phi) * sin_th, sin(phi) * sin_th, cos_th);
}

@compute @workgroup_size(8, 8, 1)
fn prefilter_mip(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.dst_w || gid.y >= params.dst_h) { return; }

    // Destination texel direction (equirect center).
    let u = (f32(gid.x) + 0.5) / f32(params.dst_w);
    let v = (f32(gid.y) + 0.5) / f32(params.dst_h);
    let theta = v * PI;
    let phi = u * 2.0 * PI - PI;
    let n = vec3(cos(phi) * sin(theta), cos(theta), sin(phi) * sin(theta));

    // Tangent frame (N = V = R convention of the split-sum prefilter).
    var up = vec3(0.0, 1.0, 0.0);
    if (abs(n.y) > 0.99) { up = vec3(1.0, 0.0, 0.0); }
    let t = normalize(cross(up, n));
    let b = cross(n, t);

    let alpha = max(params.roughness * params.roughness, 1e-3);
    var acc = vec3(0.0);
    var wsum = 0.0;
    for (var s = 0u; s < params.sample_count; s++) {
        let xi = vec2((f32(s) + 0.5) / f32(params.sample_count), radical_inverse_vdc(s));
        let h_local = importance_ggx(xi, alpha);
        let h = t * h_local.x + b * h_local.y + n * h_local.z;
        let l = 2.0 * dot(n, h) * h - n;
        let ndl = dot(n, l);
        if (ndl > 0.0) {
            acc += sample_level0(l) * ndl;
            wsum += ndl;
        }
    }
    var col: vec3<f32>;
    if (wsum > 0.0) {
        col = acc / wsum;
    } else {
        col = sample_level0(n); // degenerate lobe fallback (roughness ~0 at a pole)
    }

    let idx = params.dst_offset + (gid.y * params.dst_w + gid.x) * 3u;
    env[idx] = col.x;
    env[idx + 1u] = col.y;
    env[idx + 2u] = col.z;
}
