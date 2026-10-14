@group(0) @binding(0) var<storage, read_write> hits: array<vec4<u32>>;
@group(1) @binding(0) var acceleration: acceleration_structure;
override PT_INTERACTIVE: bool = false;

@compute @workgroup_size(8, 8, 1)
fn pass_pathtrace_tri(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= 4u || invocation.y != 0u { return; }
    var origin = vec3<f32>(0.0, 0.0, 5.0);
    if invocation.x == 1u { origin.x = 3.0; }
    if invocation.x == 2u { origin.x = 10.0; }
    if invocation.x == 3u { origin.z = 0.5; }
    var query: ray_query;
    rayQueryInitialize(&query, acceleration, RayDesc(RAY_FLAG_FORCE_OPAQUE, 0xffu, 0.001, 100.0, origin, vec3<f32>(0.0, 0.0, -1.0)));
    while rayQueryProceed(&query) {}
    let intersection = rayQueryGetCommittedIntersection(&query);
    var result = vec4<u32>(0u);
    if intersection.kind == RAY_QUERY_INTERSECTION_TRIANGLE {
        result = vec4<u32>(intersection.kind, intersection.instance_custom_data, intersection.primitive_index, bitcast<u32>(intersection.t));
    }
    hits[invocation.x] = result;
}
