// AZRA — SDF Bevel Preview Compute Shader (Sprint 1)
// Raymarches SDF bevel on polygon mesh edges.
// Dispatched over full viewport; AABB early-exit for non-bevel pixels.

struct BevelEdge {
    face_normal_a: vec3<f32>,
    face_offset_a: f32,
    face_normal_b: vec3<f32>,
    face_offset_b: f32,
    edge_midpoint: vec3<f32>,
    edge_half_length: f32,
    edge_direction: vec3<f32>,
    dihedral_angle: f32,
}

struct BevelParams {
    width: f32,
    depth: f32,
    shape: f32,
    profile_mode: u32,

    edge_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,

    grid_dims: vec3<u32>,
    grid_total_cells: u32,
    bbox_min: vec3<f32>,
    _pad3: f32,
    bbox_max: vec3<f32>,
    _pad4: f32,

    view_proj: mat4x4<f32>,
    view_proj_inv: mat4x4<f32>,
    camera_pos: vec3<f32>,
    max_march_steps: u32,

    viewport_size: vec2<f32>,
    near_plane: f32,
    far_plane: f32,

    light_dir: vec3<f32>,
    _pad5: f32,
    light_color: vec3<f32>,
    ambient: f32,

    bevel_color: vec3<f32>,
    bevel_roughness: f32,

    tile_offset: vec2<f32>,
    normal_blend_width: f32,
    _pad6: f32,
}

struct GridCell {
    offset: u32,
    count: u32,
}

// Bindings: single bind group
@group(0) @binding(0) var<storage, read> bevel_edges: array<BevelEdge>;
@group(0) @binding(1) var<uniform> params: BevelParams;
@group(0) @binding(2) var<storage, read> grid_cells: array<GridCell>;
@group(0) @binding(3) var<storage, read> grid_edge_indices: array<u32>;
@group(0) @binding(4) var hdr_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var gbuf_normal: texture_2d<f32>; // .w = world distance from camera

const EPSILON: f32 = 0.0005;
const MAX_DIST: f32 = 100.0;
const NORMAL_EPS: f32 = 0.001;
const FRAC_1_SQRT2: f32 = 0.70710678;

// Profile constants
const PROFILE_CHAMFER: u32 = 0u;
const PROFILE_ARC: u32 = 1u;
const PROFILE_OGEE: u32 = 2u;
const PROFILE_STEP: u32 = 3u;

// ── Ray ──

struct Ray {
    origin: vec3<f32>,
    direction: vec3<f32>,
}

fn pixel_to_ray(pixel: vec2<f32>) -> Ray {
    let ndc = vec2<f32>(
        (pixel.x / params.viewport_size.x) * 2.0 - 1.0,
        1.0 - (pixel.y / params.viewport_size.y) * 2.0
    );
    let near_clip = params.view_proj_inv * vec4<f32>(ndc, 0.0, 1.0);
    let far_clip = params.view_proj_inv * vec4<f32>(ndc, 1.0, 1.0);
    let near_world = near_clip.xyz / near_clip.w;
    let far_world = far_clip.xyz / far_clip.w;
    return Ray(params.camera_pos, normalize(far_world - near_world));
}

// ── AABB ──

struct AABBHit {
    t_enter: f32,
    t_exit: f32,
    hit: bool,
}

fn ray_aabb(ray: Ray, box_min: vec3<f32>, box_max: vec3<f32>) -> AABBHit {
    let inv_dir = 1.0 / ray.direction;
    let t1 = (box_min - ray.origin) * inv_dir;
    let t2 = (box_max - ray.origin) * inv_dir;
    let t_min = min(t1, t2);
    let t_max = max(t1, t2);
    let t_enter = max(max(t_min.x, t_min.y), t_min.z);
    let t_exit = min(min(t_max.x, t_max.y), t_max.z);
    return AABBHit(max(t_enter, 0.0), t_exit, t_enter < t_exit && t_exit > 0.0);
}

// ── Spatial grid ──

fn world_to_grid(pos: vec3<f32>) -> vec3<u32> {
    let normalized = (pos - params.bbox_min) / (params.bbox_max - params.bbox_min);
    let clamped = clamp(normalized, vec3<f32>(0.0), vec3<f32>(0.999));
    return vec3<u32>(
        u32(clamped.x * f32(params.grid_dims.x)),
        u32(clamped.y * f32(params.grid_dims.y)),
        u32(clamped.z * f32(params.grid_dims.z))
    );
}

fn grid_cell_index(cell: vec3<u32>) -> u32 {
    return cell.x + cell.y * params.grid_dims.x + cell.z * params.grid_dims.x * params.grid_dims.y;
}

// ── Blend kernels ──

fn sdf_chamfer(d1: f32, d2: f32, r: f32) -> f32 {
    return (d1 + d2 - r) * FRAC_1_SQRT2;
}

fn sdf_arc(d1: f32, d2: f32, r: f32) -> f32 {
    let u = max(vec2<f32>(r - d1, r - d2), vec2<f32>(0.0));
    return r - length(u);
}

fn sdf_ogee(d1: f32, d2: f32, r: f32) -> f32 {
    let r2 = r * 0.5;
    let mid = (d1 + d2) * 0.5;
    let inner = sdf_arc(d1, mid, r2);
    return -sdf_arc(-inner, -d2, r2);
}

fn sdf_step_profile(d1: f32, d2: f32, r: f32) -> f32 {
    let half_r = r * 0.5;
    if d1 < half_r {
        return d1 - half_r;
    }
    return d2;
}

fn sdf_shape_blend(d1: f32, d2: f32, r: f32, shape: f32) -> f32 {
    let chamfer = sdf_chamfer(d1, d2, r);
    if shape > 0.5 {
        let arc = sdf_arc(d1, d2, r);
        let t = (shape - 0.5) * 2.0;
        return mix(chamfer, arc, t);
    } else {
        let concave = -sdf_arc(-d1, -d2, r);
        let t = shape * 2.0;
        return mix(concave, chamfer, t);
    }
}

fn bevel_blend(d1: f32, d2: f32, r: f32) -> f32 {
    switch params.profile_mode {
        case PROFILE_CHAMFER: { return sdf_chamfer(d1, d2, r); }
        case PROFILE_ARC:     { return sdf_arc(d1, d2, r); }
        case PROFILE_OGEE:    { return sdf_ogee(d1, d2, r); }
        case PROFILE_STEP:    { return sdf_step_profile(d1, d2, r); }
        default:              { return sdf_shape_blend(d1, d2, r, params.shape); }
    }
}

// ── Edge SDF ──

fn eval_edge_sdf(edge: BevelEdge, pos: vec3<f32>) -> f32 {
    let d1 = dot(edge.face_normal_a, pos) - edge.face_offset_a;
    let d2 = dot(edge.face_normal_b, pos) - edge.face_offset_b;

    let w = params.width;
    // Skip points far outside the bevel zone
    if d1 > w * 2.0 || d2 > w * 2.0 { return MAX_DIST; }
    // Skip points deep inside the mesh (negative = inside both faces)
    if d1 < -w || d2 < -w { return MAX_DIST; }

    // Skip points far from the edge along its direction
    let along_edge = dot(pos - edge.edge_midpoint, edge.edge_direction);
    if abs(along_edge) > edge.edge_half_length + w { return MAX_DIST; }

    let edge_factor = smoothstep(
        edge.edge_half_length + w * 0.5,
        edge.edge_half_length - w * 0.1,
        abs(along_edge)
    );

    let beveled = bevel_blend(d1, d2, w) - params.depth;
    if edge_factor < 0.001 { return MAX_DIST; }
    return beveled;
}

// ── Full SDF ──

fn eval_bevel_sdf(pos: vec3<f32>) -> f32 {
    if any(pos < params.bbox_min) || any(pos > params.bbox_max) {
        return MAX_DIST;
    }

    let cell = world_to_grid(pos);
    let ci = grid_cell_index(cell);
    let gc = grid_cells[ci];

    var d = MAX_DIST;
    for (var i = 0u; i < gc.count; i++) {
        let edge_idx = grid_edge_indices[gc.offset + i];
        let edge = bevel_edges[edge_idx];
        d = min(d, eval_edge_sdf(edge, pos));
    }
    return d;
}

fn compute_sdf_normal(pos: vec3<f32>) -> vec3<f32> {
    let e = vec2<f32>(NORMAL_EPS, 0.0);
    let n = vec3<f32>(
        eval_bevel_sdf(pos + e.xyy) - eval_bevel_sdf(pos - e.xyy),
        eval_bevel_sdf(pos + e.yxy) - eval_bevel_sdf(pos - e.yxy),
        eval_bevel_sdf(pos + e.yyx) - eval_bevel_sdf(pos - e.yyx)
    );
    return normalize(n);
}

// ── Shading ──

fn shade_bevel(pos: vec3<f32>, normal: vec3<f32>, view_dir: vec3<f32>) -> vec4<f32> {
    let n = normalize(normal);
    let l = normalize(-params.light_dir);
    let v = normalize(-view_dir);
    let h = normalize(l + v);

    let n_dot_l = max(dot(n, l), 0.0);
    let diffuse = params.bevel_color * params.light_color * n_dot_l;

    let shininess = mix(2.0, 256.0, 1.0 - params.bevel_roughness);
    let n_dot_h = max(dot(n, h), 0.0);
    let specular = params.light_color * pow(n_dot_h, shininess) * 0.3;

    let amb = params.bevel_color * params.ambient;
    let fresnel = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.15;

    let color = amb + diffuse + specular + vec3<f32>(fresnel);
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

fn world_to_depth(pos: vec3<f32>) -> f32 {
    let clip = params.view_proj * vec4<f32>(pos, 1.0);
    return clip.z / clip.w;
}

// ── Normal blending (S21.11) ──

fn blend_bevel_normal(bevel_normal: vec3<f32>, gbuf_normal_val: vec3<f32>, sdf_dist: f32) -> vec3<f32> {
    let blend_w = params.normal_blend_width;
    if blend_w <= 0.0 { return bevel_normal; }
    // Smooth blend: as sdf_dist approaches 0 from outside, transition from gbuf to bevel normal
    let t = clamp(1.0 - sdf_dist / blend_w, 0.0, 1.0);
    let t_smooth = t * t * (3.0 - 2.0 * t); // smoothstep
    return normalize(mix(gbuf_normal_val, bevel_normal, t_smooth));
}

// ── Main entry ──

@compute @workgroup_size(8, 8)
fn bevel_preview(@builtin(global_invocation_id) gid: vec3<u32>) {
    // S21.10: tile_offset shifts dispatched region to correct viewport position
    let pixel = vec2<i32>(gid.xy) + vec2<i32>(params.tile_offset);

    if pixel.x >= i32(params.viewport_size.x) || pixel.y >= i32(params.viewport_size.y) {
        return;
    }
    if pixel.x < 0 || pixel.y < 0 {
        return;
    }

    if params.edge_count == 0u {
        return;
    }

    let ray = pixel_to_ray(vec2<f32>(pixel) + 0.5);

    let aabb_hit = ray_aabb(ray, params.bbox_min, params.bbox_max);
    if !aabb_hit.hit {
        return;
    }

    // Read existing scene depth from G-buffer normal texture (.w = world distance)
    let gbuf_val = textureLoad(gbuf_normal, pixel, 0);
    let scene_world_dist = gbuf_val.w;
    let scene_normal = gbuf_val.xyz;

    var t = aabb_hit.t_enter;
    let t_max = aabb_hit.t_exit;
    var hit = false;
    var hit_pos = vec3<f32>(0.0);
    var last_d = MAX_DIST;

    for (var step = 0u; step < params.max_march_steps; step++) {
        let pos = ray.origin + ray.direction * t;
        let d = eval_bevel_sdf(pos);

        if d < EPSILON {
            hit = true;
            hit_pos = pos;
            last_d = d;
            break;
        }

        last_d = d;
        t += max(d, EPSILON * 2.0);

        if t > t_max {
            break;
        }
    }

    if !hit {
        return;
    }

    // Depth test: bevel must be closer than existing scene geometry
    let bevel_dist = length(hit_pos - params.camera_pos);
    if scene_world_dist > 0.0 && bevel_dist > scene_world_dist {
        return;
    }

    // S21.11: Blend bevel normal with G-buffer normal at boundaries
    let raw_normal = compute_sdf_normal(hit_pos);
    let blended_normal = select(
        raw_normal,
        blend_bevel_normal(raw_normal, scene_normal, last_d),
        scene_world_dist > 0.0 && length(scene_normal) > 0.5
    );
    let color = shade_bevel(hit_pos, blended_normal, ray.direction);

    textureStore(hdr_output, pixel, color);
}
