struct Triangle {
    a: vec4<f32>, b: vec4<f32>, c: vec4<f32>,
    na: vec4<f32>, nb: vec4<f32>, nc: vec4<f32>,
    edges: vec4<u32>,
}
struct Edge { a: vec4<f32>, b: vec4<f32>, n0: vec4<f32>, n1: vec4<f32> }
struct Surface { position: vec4<f32>, normal: vec4<f32> }
struct BvhNode { minimum: vec3<f32>, first: u32, maximum: vec3<f32>, count: u32 }
struct Params { res: u32, cells: u32, triangles: u32, pad: u32 }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> triangles: array<Triangle>;
@group(0) @binding(2) var<storage, read> edges: array<Edge>;
@group(0) @binding(3) var<storage, read_write> surfaces: array<Surface>;
@group(0) @binding(4) var<storage, read> nodes: array<BvhNode>;
@group(0) @binding(5) var<storage, read> order: array<u32>;

@compute @workgroup_size(8, 8)
fn poly_curvature_cache(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (any(gid.xy >= vec2(params.res))) { return; }
    let tile = params.res / params.cells;
    let cell = gid.xy / tile;
    let triangle = cell.y * params.cells + cell.x;
    let pixel = gid.y * params.res + gid.x;
    if (triangle >= params.triangles) {
        surfaces[pixel] = Surface(vec4(0.0), vec4(0.0)); return;
    }
    var bary = clamp((vec2<f32>(gid.xy % tile) + 0.5 - 2.0) / f32(tile - 4u), vec2(0.0), vec2(1.0));
    if (bary.x + bary.y > 1.0) { bary /= bary.x + bary.y; }
    let tri = triangles[triangle];
    let position = tri.a.xyz * (1.0 - bary.x - bary.y) + tri.b.xyz * bary.x + tri.c.xyz * bary.y;
    let normal = normalize(tri.na.xyz * (1.0 - bary.x - bary.y) + tri.nb.xyz * bary.x + tri.nc.xyz * bary.y);
    var distance = 1e30;
    var angle = 0.0;
    var stack: array<u32, 64>;
    var pending = select(0u, 1u, params.pad > 0u && tri.edges.w > 0u);
    stack[0] = 0u;
    while (pending > 0u) {
        pending--;
        let node = nodes[stack[pending]];
        let outside = max(max(node.minimum - position, position - node.maximum), vec3(0.0));
        if (dot(outside, outside) > distance * distance) { continue; }
        if (node.count == 0u) {
            stack[pending] = node.first; stack[pending + 1u] = node.first + 1u; pending += 2u; continue;
        }
        for (var offset = node.first; offset < node.first + node.count; offset++) {
        let edge = edges[order[offset]];
        if (u32(edge.n0.w) != tri.edges.z) { continue; }
        let delta = edge.b.xyz - edge.a.xyz;
        let squared = dot(delta, delta);
        if (squared == 0.0 || dot(edge.n1.xyz, edge.n1.xyz) == 0.0) { continue; }
        let dihedral = atan2(dot(delta / sqrt(squared), cross(edge.n0.xyz, edge.n1.xyz)), dot(edge.n0.xyz, edge.n1.xyz));
        if (abs(dihedral) < 1e-6) { continue; }
        let along = clamp(dot(position - edge.a.xyz, delta) / squared, 0.0, 1.0);
        let measured = length(position - edge.a.xyz - along * delta);
        if (measured < distance) { distance = measured; angle = dihedral; }
        }
    }
    surfaces[pixel] = Surface(vec4(position, distance), vec4(normal, angle));
}
