struct ResizeParams { source_res: u32, target_res: u32, layers: u32, mip: u32 }
@group(0) @binding(0) var<uniform> resize: ResizeParams;
@group(0) @binding(1) var resized_maps: texture_storage_2d_array<rgba8unorm, write>;
@group(0) @binding(2) var source: texture_2d_array<f32>;
@group(0) @binding(3) var<storage, read> codecs: array<u32>;

fn resize_load(coord: vec2<i32>, layer: i32) -> vec4<f32> {
    let position = clamp(coord, vec2(0), vec2(i32(resize.source_res) - 1));
    if (codecs[layer] != 0u) {
        let high_layer = layer - select(0, 1, (codecs[layer] & 1u) == 0u);
        var hi = textureLoad(source, position, high_layer, 0).rgb;
        var lo = textureLoad(source, position, high_layer + 1, 0).rgb;
        if (codecs[layer] >= 3u) {
            hi = round(material_linear(hi) * 255.0) / 255.0;
            lo = round(material_linear(lo) * 255.0) / 255.0;
        }
        var value = (hi * 65280.0 + lo * 255.0) / 65535.0;
        if (codecs[layer] >= 5u) { value = material_linear(value); }
        return vec4(value, 1.0);
    }
    let value = textureLoad(source, position, layer, 0);
    return vec4(material_linear(value.rgb), value.a);
}

@compute @workgroup_size(8, 8, 1)
fn material_resize(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= resize.target_res || gid.y >= resize.target_res || gid.z >= resize.layers) { return; }
    let position = (vec2<f32>(gid.xy) + 0.5) * f32(resize.source_res) / f32(resize.target_res) - 0.5;
    let base = vec2<i32>(floor(position));
    let weight = fract(position);
    let layer = i32(gid.z);
    let top_left = resize_load(base, layer);
    let top_right = resize_load(base + vec2(1, 0), layer);
    let bottom_left = resize_load(base + vec2(0, 1), layer);
    let bottom_right = resize_load(base + vec2(1, 1), layer);
    if (all(top_left == top_right) && all(top_left == bottom_left) && all(top_left == bottom_right)) {
        let position = clamp(base, vec2(0), vec2(i32(resize.source_res) - 1));
        textureStore(resized_maps, vec2<i32>(gid.xy), layer, textureLoad(source, position, layer, 0));
        return;
    }
    let top = mix(top_left, top_right, weight.x);
    let bottom = mix(bottom_left, bottom_right, weight.x);
    var result = mix(top, bottom, weight.y);
    if (codecs[layer] != 0u) {
        var value = result.rgb;
        if (codecs[layer] >= 5u) { value = material_srgb(value); }
        let quantized = vec3<u32>(value * 65535.0 + 0.5);
        let bytes = select(quantized >> vec3(8u), quantized & vec3(255u), (codecs[layer] & 1u) == 0u);
        var encoded = vec3<f32>(bytes) / 255.0;
        if (codecs[layer] >= 3u) { encoded = material_srgb(encoded); }
        result = vec4(encoded, 1.0);
    } else {
        result = vec4(material_srgb(result.rgb), result.a);
    }
    textureStore(resized_maps, vec2<i32>(gid.xy), layer, result);
}
