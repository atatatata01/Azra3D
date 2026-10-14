// ============================================================
// Poly Sub-Element ID Picking Shader (Blender select-buffer model)
// ============================================================
//
// Renders the ACTIVE poly body's sub-elements with flat per-ELEMENT ids into
// the Rgba8Uint picking target — three range-draws over one vertex buffer:
// faces (triangles), edges (lines), verts (points), each with its own tagged
// id (see poly_select_gpu.rs PICK_TAG_*).
//
// TWO passes, because points/lines must not RACE the faces on raw depth:
//   pass 1 — faces (+ id-0 occluder bodies): normal depth test, writes depth.
//   pass 2 — edges + verts: NO depth attachment; `fs_poly_pick_elem` compares
//     its own fragment depth against the 3×3 WINDOW MAX of the pass-1 face
//     depth. The window absorbs the face's per-pixel depth slope exactly (the
//     thing a hardware bias cannot do for a zero-slope point primitive — the
//     proven failure: ~half of a sphere's verts lost the race and vanished),
//     silhouette elements pass because the window reaches background depth
//     (1.0), and genuinely occluded elements fail against the nearer surface.
//     No bias constants, no backend dependence.
//
// Positions are pre-transformed to WORLD space on the CPU (edit meshes are
// small; the mesh is static for the whole selection gesture), so there is no
// instance buffer — just {pos, id} vertices.
//
// id 0 = background/occluder; element ids are (tag | index + 1).

struct CameraUniforms {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    near_far: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) id: u32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) @interpolate(flat) id: u32,
};

@vertex
fn vs_poly_pick(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_proj * vec4(in.position, 1.0);
    out.id = in.id;
    return out;
}

fn pack_id(id: u32) -> vec4<u32> {
    return vec4<u32>(
        id & 0xFFu,
        (id >> 8u) & 0xFFu,
        (id >> 16u) & 0xFFu,
        (id >> 24u) & 0xFFu,
    );
}

@fragment
fn fs_poly_pick(in: VertexOutput) -> @location(0) vec4<u32> {
    return pack_id(in.id);
}

// Pass-2 (edges + verts): the face-pass depth, sampled as a 3x3 window MAX.
@group(0) @binding(1) var face_depth: texture_depth_2d;

fn face_depth_at(p: vec2<i32>, dims: vec2<i32>) -> f32 {
    return textureLoad(face_depth, clamp(p, vec2<i32>(0, 0), dims - vec2<i32>(1, 1)), 0);
}

// Element fragment: keep this element's id only when nothing is genuinely IN
// FRONT of it. Reference = the FARTHEST face depth in the 3x3 neighborhood…
//   * a coplanar face whose depth slope makes the center texel marginally
//     nearer than this point still yields a farther neighbor -> element wins
//     (this is the case the hardware bias could not express for points);
//   * at a silhouette some neighbor is background (1.0) -> element wins;
//   * a real occluder is nearer across the WHOLE window -> element loses.
//
// …PLUS one neighborhood of the surface's own depth SLOPE. The window max alone
// assumes some neighbouring texel is FARTHER than the element, and at a concave
// corner none is: an inside corner is a strict LOCAL MAXIMUM of surface depth,
// every direction along the surface leading toward the camera. The face depth is
// sampled at pixel CENTRES while the element sits at an arbitrary sub-pixel
// position, so the surface reads up to |grad z| * 1px NEARER than the element's
// own exact depth and it discarded itself. Widening the window cannot fix that —
// a local maximum stays the maximum at any radius (owner screenshot 2026-08-11:
// verts missing at sharp concave corners, at grazing angles, both in the overlay
// and to the picker). The per-axis MAX of the two one-sided differences is that
// sub-pixel error's own bound, measured from the depth buffer: still no tunable
// constant, and where the window already reached background/farther geometry the
// term is irrelevant because far_ref has won outright.
// EPS is a depth-buffer ULP guard for exact-equal coplanar writes, not a
// tunable world-space fudge.
@fragment
fn fs_poly_pick_elem(in: VertexOutput) -> @location(0) vec4<u32> {
    let dims = vec2<i32>(textureDimensions(face_depth));
    let c = vec2<i32>(i32(in.clip_position.x), i32(in.clip_position.y));
    var far_ref = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            far_ref = max(far_ref, face_depth_at(c + vec2<i32>(dx, dy), dims));
        }
    }
    let dc = face_depth_at(c, dims);
    let gx = max(abs(face_depth_at(c + vec2<i32>(1, 0), dims) - dc),
                 abs(dc - face_depth_at(c - vec2<i32>(1, 0), dims)));
    let gy = max(abs(face_depth_at(c + vec2<i32>(0, 1), dims) - dc),
                 abs(dc - face_depth_at(c - vec2<i32>(0, 1), dims)));
    let EPS = 1e-6;
    if (in.clip_position.z > far_ref + max(EPS, gx + gy)) {
        discard;
    }
    return pack_id(in.id);
}
