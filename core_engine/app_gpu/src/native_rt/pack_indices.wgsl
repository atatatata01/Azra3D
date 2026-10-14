struct PackRange {
    first_triangle: u32,
    triangle_count: u32,
    dispatch_width: u32,
    padding: u32,
}

@group(0) @binding(0) var<storage, read> source_triangles: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> packed_indices: array<u32>;
@group(0) @binding(2) var<uniform> pack_range: PackRange;

@compute @workgroup_size(128)
fn pack_indices(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let triangle_index = invocation.y * pack_range.dispatch_width + invocation.x;
    if triangle_index >= pack_range.triangle_count {
        return;
    }
    let triangle = source_triangles[pack_range.first_triangle + triangle_index];
    packed_indices[triangle_index * 3u] = triangle.x;
    packed_indices[triangle_index * 3u + 1u] = triangle.y;
    packed_indices[triangle_index * 3u + 2u] = triangle.z;
}
