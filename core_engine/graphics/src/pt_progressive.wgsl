@group(0) @binding(60) var pt_accum_out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(61) var<storage, read_write> pt_moments: array<vec4<f32>>;

fn pt_add_luminance_sample(moments: ptr<function, vec4<f32>>, radiance: vec3<f32>) {
    let luminance = dot(radiance, vec3(0.2126, 0.7152, 0.0722));
    let count = (*moments).z + 1.0;
    let delta = luminance - (*moments).x;
    let mean = (*moments).x + delta / count;
    *moments = vec4(mean, (*moments).y + delta * (luminance - mean), count, 0.0);
}

fn pt_store_progressive(coord: vec2<i32>, sample_mean: vec3<f32>, batch: vec4<f32>) {
    let size = textureDimensions(pt_accum_out);
    let index = u32(coord.y) * size.x + u32(coord.x);
    var mean = sample_mean;
    var moments = batch;
    if (config.pt_accumulated > 1u) {
        let previous_moments = pt_moments[index];
        if (previous_moments.z > 0.0) {
            let previous = textureLoad(pt_accum_in, coord, 0);
            let count = previous_moments.z + batch.z;
            let weight = batch.z / count;
            let delta = batch.x - previous_moments.x;
            mean = previous.rgb + (sample_mean - previous.rgb) * weight;
            moments = vec4(previous_moments.x + delta * weight,
                previous_moments.y + batch.y + delta * delta * previous_moments.z * weight,
                count, 0.0);
        }
    }
    pt_moments[index] = moments;
    textureStore(pt_accum_out, coord, vec4(mean, moments.z));
    textureStore(output_texture, coord, vec4(mean, min(moments.z, 65504.0)));
    atomicAdd(&pt_stats.total, 1u);
    var noisy = moments.z < 4.0;
    if (!noisy) {
        let variance_of_mean = max(moments.y, 0.0) / ((moments.z - 1.0) * moments.z);
        let threshold = config.pt_focus_falloff * abs(moments.x);
        noisy = variance_of_mean > threshold * threshold;
    }
    if (noisy) {
        atomicAdd(&pt_stats.noisy, 1u);
    }
}
