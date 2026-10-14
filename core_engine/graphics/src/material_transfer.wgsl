fn material_srgb(value: vec3<f32>) -> vec3<f32> {
    let bounded = clamp(value, vec3(0.0), vec3(1.0));
    return select(1.055 * pow(bounded, vec3(1.0 / 2.4)) - 0.055,
        bounded * 12.92, bounded <= vec3(0.0031308));
}

fn material_linear(value: vec3<f32>) -> vec3<f32> {
    return select(pow((value + 0.055) / 1.055, vec3(2.4)),
        value / 12.92, value <= vec3(0.04045));
}
