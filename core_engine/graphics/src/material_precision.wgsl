fn material_bump_normal(normal: vec3<f32>, sigma_x: vec3<f32>, sigma_y: vec3<f32>,
                        height_x: f32, height_y: f32, strength: f32) -> vec3<f32> {
    let length_x = length(sigma_x);
    let length_y = length(sigma_y);
    if (length_x == 0.0 || length_y == 0.0) { return normal; }
    let tangent_x = sigma_x / length_x;
    let tangent_y = sigma_y / length_y;
    let reciprocal_x = cross(tangent_y, normal);
    let reciprocal_y = cross(normal, tangent_x);
    let determinant = dot(tangent_x, reciprocal_x);
    if (determinant == 0.0) { return normal; }
    let gradient = (height_x / length_x * reciprocal_x
                  + height_y / length_y * reciprocal_y) / determinant;
    return normalize(normal - strength * gradient);
}

fn material_projection_weights(normal: vec3<f32>, blend: f32) -> vec3<f32> {
    let weights = pow(abs(normal), vec3<f32>(clamp(blend, 0.25, 32.0)));
    return weights / (weights.x + weights.y + weights.z + 0.0001);
}

fn material_plane_linear(maps: texture_2d_array<f32>, slot: u32, uv: vec2<f32>) -> vec3<f32> {
    let size = vec2<i32>(textureDimensions(maps, 0));
    let position = fract(uv) * vec2<f32>(size) - 0.5;
    let origin = vec2<i32>(floor(position));
    let fraction = fract(position);
    let lower = (origin % size + size) % size;
    let upper = (lower + vec2<i32>(1)) % size;
    let layer = i32(min(slot, textureNumLayers(maps) - 1u));
    let bottom_left = textureLoad(maps, lower, layer, 0).rgb;
    let bottom_right = textureLoad(maps, vec2<i32>(upper.x, lower.y), layer, 0).rgb;
    let top_left = textureLoad(maps, vec2<i32>(lower.x, upper.y), layer, 0).rgb;
    let top_right = textureLoad(maps, upper, layer, 0).rgb;
    return mix(mix(bottom_left, bottom_right, fraction.x),
               mix(top_left, top_right, fraction.x), fraction.y);
}
