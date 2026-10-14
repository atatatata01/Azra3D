// Mesh-map baker GPU rays (G7). Same contract as maps.rs CPU:
// cosine hemisphere (R2 + hash2) + TriBlas any-hit (stack 64).
// mode 0 = AO, 1 = thickness, 2 = bent (writes tangent-space dir in rgb).

struct Params {
    res: u32,
    rays: u32,
    mode: u32,
    node_count: u32,
    vert_count: u32,
    idx_count: u32,
    order_count: u32,
    texel_count: u32,
    bias: f32,
    ray_len: f32,
    _p0: f32,
    _p1: f32,
}

struct BakeNode {
    aabb_min: vec3<f32>,
    first: u32,
    aabb_max: vec3<f32>,
    count: u32,
}

struct Texel {
    pos: vec4<f32>,
    nrm: vec4<f32>,
    tan: vec4<f32>,
    bit: vec4<f32>,
    low_n: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> nodes: array<BakeNode>;
@group(0) @binding(2) var<storage, read> order: array<u32>;
@group(0) @binding(3) var<storage, read> verts: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> idx: array<u32>;
@group(0) @binding(5) var<storage, read> texels: array<Texel>;
@group(0) @binding(6) var<storage, read_write> out_px: array<vec4<f32>>;

fn frac(x: f32) -> f32 {
    return x - floor(x);
}

fn hemisphere_sample(ray_i: u32, texel: u32) -> vec2<f32> {
    let u0 = frac(0.5 + 0.7548777 * f32(ray_i));
    let v0 = frac(0.5 + 0.5698403 * f32(ray_i));
    let h = texel * 0x9E3779B9u;
    let du = f32(h >> 8u) / 16777216.0;
    let dv = f32((h * 0x85EBCA6Bu) >> 8u) / 16777216.0;
    let u = frac(u0 + du);
    let v = frac(v0 + dv);
    return vec2<f32>(sqrt(u), v * 6.28318530718);
}

fn ray_aabb(orig: vec3<f32>, inv_dir: vec3<f32>, node: BakeNode, t_max: f32) -> bool {
    var t0 = 0.0;
    var t1 = t_max;
    for (var k = 0; k < 3; k = k + 1) {
        let a = (node.aabb_min[k] - orig[k]) * inv_dir[k];
        let b = (node.aabb_max[k] - orig[k]) * inv_dir[k];
        let lo = min(a, b);
        let hi = max(a, b);
        t0 = max(t0, lo);
        t1 = min(t1, hi);
        if (t1 < t0) {
            return false;
        }
    }
    return true;
}

fn ray_tri(orig: vec3<f32>, dir: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> f32 {
    let e1 = b - a;
    let e2 = c - a;
    let pv = cross(dir, e2);
    let det = dot(e1, pv);
    if (abs(det) < 1.0e-7) {
        return -1.0;
    }
    let inv = 1.0 / det;
    let tvec = orig - a;
    let u = dot(tvec, pv) * inv;
    if (u < -1.0e-7 || u > 1.0 + 1.0e-7) {
        return -1.0;
    }
    let q = cross(tvec, e1);
    let v = dot(dir, q) * inv;
    if (v < -1.0e-7 || u + v > 1.0 + 1.0e-7) {
        return -1.0;
    }
    let t = dot(e2, q) * inv;
    if (t > 1.0e-7) {
        return t;
    }
    return -1.0;
}

fn any_hit(orig: vec3<f32>, dir: vec3<f32>, t_max: f32) -> bool {
    if (t_max <= 0.0) {
        return false;
    }
    let ntri = p.idx_count / 3u;
    // Small soups: bounded brute force (avoids a TDR-class BVH walk on WARP).
    if (ntri > 0u && ntri <= 256u) {
        for (var t = 0u; t < ntri; t = t + 1u) {
            let base = t * 3u;
            let i0 = idx[base];
            let i1 = idx[base + 1u];
            let i2 = idx[base + 2u];
            if (i0 >= p.vert_count || i1 >= p.vert_count || i2 >= p.vert_count) {
                continue;
            }
            let hit = ray_tri(orig, dir, verts[i0].xyz, verts[i1].xyz, verts[i2].xyz);
            if (hit > 0.0 && hit < t_max) {
                return true;
            }
        }
        return false;
    }
    if (p.node_count == 0u) {
        return false;
    }
    let inv_dir = 1.0 / dir;
    var stack: array<u32, 64>;
    var sp = 1u;
    stack[0] = 0u;
    var visits = 0u;
    loop {
        if (sp == 0u || visits > 512u) {
            break;
        }
        visits = visits + 1u;
        sp = sp - 1u;
        let ni = stack[sp];
        if (ni >= p.node_count) {
            continue;
        }
        let node = nodes[ni];
        if (!ray_aabb(orig, inv_dir, node, t_max)) {
            continue;
        }
        if (node.count > 0u) {
            for (var k = 0u; k < node.count; k = k + 1u) {
                let oi = node.first + k;
                if (oi >= p.order_count) {
                    continue;
                }
                let t = order[oi];
                let base = t * 3u;
                if (base + 2u >= p.idx_count) {
                    continue;
                }
                let i0 = idx[base];
                let i1 = idx[base + 1u];
                let i2 = idx[base + 2u];
                if (i0 >= p.vert_count || i1 >= p.vert_count || i2 >= p.vert_count) {
                    continue;
                }
                let hit = ray_tri(orig, dir, verts[i0].xyz, verts[i1].xyz, verts[i2].xyz);
                if (hit > 0.0 && hit < t_max) {
                    return true;
                }
            }
        } else if (sp + 2u <= 64u) {
            stack[sp] = node.first;
            stack[sp + 1u] = node.first + 1u;
            sp = sp + 2u;
        }
    }
    return false;
}

@compute @workgroup_size(8, 8, 1)
fn bake_rays(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= p.res || y >= p.res) {
        return;
    }
    let i = y * p.res + x;
    if (i >= p.texel_count) {
        return;
    }
    let tx = texels[i];
    if (tx.pos.w <= 1.0e-8) {
        out_px[i] = vec4<f32>(0.0);
        return;
    }
    var nrm = tx.nrm.xyz;
    if (p.mode == 1u) {
        nrm = -nrm;
    }
    if (dot(nrm, nrm) < 1.0e-12) {
        out_px[i] = vec4<f32>(0.0);
        return;
    }
    nrm = normalize(nrm);
    let origin = tx.pos.xyz + nrm * p.bias;
    var up = vec3<f32>(0.0, 0.0, 1.0);
    if (abs(nrm.z) >= 0.9) {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let t1 = normalize(cross(nrm, up));
    let t2 = cross(nrm, t1);
    var open = 0u;
    var acc = vec3<f32>(0.0);
    let rays = p.rays;
    for (var r = 0u; r < rays; r = r + 1u) {
        let s = hemisphere_sample(r, i);
        let sin_t = s.x;
        let phi = s.y;
        let cos_t = sqrt(max(1.0 - sin_t * sin_t, 0.0));
        let d = t1 * (sin_t * cos(phi)) + t2 * (sin_t * sin(phi)) + nrm * cos_t;
        if (!any_hit(origin, d, p.ray_len)) {
            open = open + 1u;
            acc = acc + d;
        }
    }
    if (p.mode == 2u) {
        var world = nrm;
        if (open > 0u) {
            let len = length(acc);
            if (len > 1.0e-8) {
                world = acc / len;
            }
        }
        let ln = tx.low_n.xyz;
        let t = tx.tan.xyz;
        let b = tx.bit.xyz;
        let ts = vec3<f32>(dot(world, t), dot(world, b), dot(world, ln));
        out_px[i] = vec4<f32>(ts, 1.0);
        return;
    }
    let f = f32(open) / f32(rays);
    var v = f;
    if (p.mode == 1u) {
        v = 1.0 - f;
    }
    out_px[i] = vec4<f32>(v, v, v, 1.0);
}
