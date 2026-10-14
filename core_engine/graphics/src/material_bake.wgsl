// ============================================================
// AZRA — GPU material-map bake (GPU_MATERIAL_BAKE_PLAN.md P1 + T1 dual-plane)
// ============================================================
// Evaluates a node graph's generated `eval_node_material` over the `res²` UV bake grid and
// writes LINEAR rgba16float storage textures — the GPU counterpart of the CPU
// `bake_node_graph_maps_res` pixel loop (`app/src/node_graph_bake.rs`).
//
// T1 (OPTIMIZATION_PLAN_FOR_GROK): outputs are LINEAR f16, not pre-encoded 8-bit sRGB.
// The host (`gpu_material_bake.rs`) dual-plane encodes with the same CPU functions as the
// rayon bake (`encode_srgb_color_rgba16_planes` / `encode_data_scalar_rgba16_planes`) so
// Final + Bit16 install correctly. Do not re-introduce sRGB bake-in-shader: that locked the
// path to 8-bit forever.
//
// ── DOMAIN PARITY (the whole correctness contract) ─────────────────────────────────────────
// The CPU loop computes, per pixel:
//     u = (x + 0.5) / res
//     v = (y + 0.5) / res
//     p = vec3(u * 2 - 1, v * 2 - 1, 0)      // planar UV domain
//     n = vec3(0, 0, 1)
//     hit_prim = 0
// This shader reproduces those five lines EXACTLY.
//
// ── SCOPE ─────────────────────────────────────────────────────────────────────────────────
// `graph_is_gpu_bakeable` admits ONLY node types whose WGSL is faithful. Surface analyzers
// and ImageTexture stay on the CPU path.

struct MatResult {
    color: vec3<f32>,
    roughness: f32,
    metallic: f32,
    opacity: f32,
    emission: vec3<f32>,
}

struct MatBakeParams {
    res: u32,
    // Bit flags: 1=albedo 2=roughness 4=metallic 8=emission.
    channels: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> mb: MatBakeParams;
@group(0) @binding(1) var out_albedo: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var out_roughness: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var out_metallic: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var out_emission: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn material_bake(@builtin(global_invocation_id) gid: vec3<u32>) {
    let res = mb.res;
    if (gid.x >= res || gid.y >= res) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    // EXACTLY the CPU loop's domain.
    let u = (f32(gid.x) + 0.5) / f32(res);
    let v = (f32(gid.y) + 0.5) / f32(res);
    let p = vec3<f32>(u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0);
    let n = vec3<f32>(0.0, 0.0, 1.0);

    let m = eval_node_material(0u, p, n);

    // LINEAR writes — host dual-plane encodes (color vs data curves differ).
    // Albedo: CPU folds `albedo * ao`; MatResult has no ao and GPU-eligible graphs have no
    // AO node, so ao == 1.0 by construction.
    if ((mb.channels & 1u) != 0u) {
        textureStore(out_albedo, coord, vec4<f32>(m.color, 1.0));
    }
    if ((mb.channels & 2u) != 0u) {
        let r = m.roughness;
        textureStore(out_roughness, coord, vec4<f32>(r, r, r, 1.0));
    }
    if ((mb.channels & 4u) != 0u) {
        let met = m.metallic;
        textureStore(out_metallic, coord, vec4<f32>(met, met, met, 1.0));
    }
    if ((mb.channels & 8u) != 0u) {
        textureStore(out_emission, coord, vec4<f32>(m.emission, 1.0));
    }
}
