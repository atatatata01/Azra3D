@group(1) @binding(0) var pt_hw_accel: acceleration_structure;

fn pt_trace_scene(orig: vec3<f32>, dir: vec3<f32>, t_far: f32) -> PtHit {
    var result: PtHit;
    result.t = t_far;
    if (pt_meta.instance_count == 0u || t_far <= 1e-5) { return result; }
    let t_min = bitcast<f32>(bitcast<u32>(1e-5f) + 1u);
    let t_max = bitcast<f32>(bitcast<u32>(t_far) - 1u);
    if (t_max < t_min) { return result; }
    var query: ray_query;
    rayQueryInitialize(&query, pt_hw_accel,
        RayDesc(RAY_FLAG_FORCE_OPAQUE, 0xFFu, t_min, t_max, orig, dir));
    while (rayQueryProceed(&query)) {}
    let intersection = rayQueryGetCommittedIntersection(&query);
    if (intersection.kind == RAY_QUERY_INTERSECTION_TRIANGLE) {
        result.t = intersection.t;
        result.u = intersection.barycentrics.x;
        result.v = intersection.barycentrics.y;
        result.instance = intersection.instance_custom_data;
        result.tri = pt_instances[result.instance].tri_off + intersection.primitive_index;
        result.hit = true;
    }
    return result;
}

fn pt_occluded(orig: vec3<f32>, dir: vec3<f32>, dist: f32) -> bool {
    let t_max = dist - PT_EPS * 4.0;
    if (t_max <= 1e-5 || pt_meta.instance_count == 0u) { return false; }
    let t_min = bitcast<f32>(bitcast<u32>(1e-5f) + 1u);
    let t_end = bitcast<f32>(bitcast<u32>(t_max) - 1u);
    if (t_end < t_min) { return false; }
    var query: ray_query;
    rayQueryInitialize(&query, pt_hw_accel,
        RayDesc(RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_TERMINATE_ON_FIRST_HIT,
            0xFFu, t_min, t_end, orig, dir));
    while (rayQueryProceed(&query)) {}
    return rayQueryGetCommittedIntersection(&query).kind != RAY_QUERY_INTERSECTION_NONE;
}
