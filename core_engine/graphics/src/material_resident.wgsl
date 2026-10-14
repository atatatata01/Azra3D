struct MatResult {
    color: vec3<f32>,
    roughness: f32,
    metallic: f32,
    opacity: f32,
    emission: vec3<f32>,
}

struct ResidentParams {
    res: u32,
    channels: u32,
    high_precision: u32,
    _pad: u32,
    slots: vec4<u32>,
    detail_slots: vec4<u32>,
}

@group(0) @binding(0) var<uniform> resident: ResidentParams;
@group(0) @binding(1) var maps: texture_storage_2d_array<rgba8unorm, write>;

fn resident_store(coord: vec2<i32>, channel: u32, value: vec3<f32>, color: bool) {
    if ((resident.channels & (1u << channel)) == 0u) { return; }
    var slot: u32;
    if (channel < 4u) { slot = resident.slots[channel]; }
    else { slot = resident.detail_slots[channel - 4u]; }
    if ((resident.high_precision & (1u << channel)) != 0u) {
        let encoded = select(clamp(value, vec3(0.0), vec3(1.0)), material_srgb(value), color);
        let quantized = vec3<u32>(encoded * 65535.0 + 0.5);
        var hi = vec3<f32>(quantized >> vec3(8u)) / 255.0;
        var lo = vec3<f32>(quantized & vec3(255u)) / 255.0;
        if (channel != 5u) { hi = material_srgb(hi); lo = material_srgb(lo); }
        textureStore(maps, coord, i32(slot), vec4(hi, 1.0));
        textureStore(maps, coord, i32(slot + 1u), vec4(lo, 1.0));
    } else {
        textureStore(maps, coord, i32(slot), vec4(material_srgb(value), 1.0));
        textureStore(maps, coord, i32(slot + 1u), vec4(0.0, 0.0, 0.0, 1.0));
    }
}

@compute @workgroup_size(8, 8, 1)
fn material_resident(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= resident.res || gid.y >= resident.res) { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / f32(resident.res);
    let material = eval_node_material(0u, vec3(uv * 2.0 - 1.0, 0.0), vec3(0.0, 0.0, 1.0));
    let coord = vec2<i32>(gid.xy);
    resident_store(coord, 0u, material.color, true);
    resident_store(coord, 1u, vec3(material.roughness), false);
    resident_store(coord, 2u, vec3(material.metallic), false);
    resident_store(coord, 3u, material.emission, true);
    let position = vec3(uv * 2.0 - 1.0, 0.0);
    if ((resident.channels & 16u) != 0u) {
        let value = eval_node_normal(0u, position, vec3(0.0, 0.0, 1.0));
        let normal = value / max(length(value), 1e-8);
        resident_store(coord, 4u, normal * 0.5 + 0.5, false);
    }
    if ((resident.channels & 32u) != 0u) {
        resident_store(coord, 5u, vec3(eval_node_height(0u, position, vec3(0.0, 0.0, 1.0))), false);
    }
}
