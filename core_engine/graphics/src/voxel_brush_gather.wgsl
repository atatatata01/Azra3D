// GPU-resident brush GATHER (sculpt Phase 5a): copy the brick ATLAS (INDEX + POOL) over a box into
// box-local `density` + `snapshot` buffers, so the PROVEN box-local brush kernel (voxel_brush.wgsl)
// runs UNCHANGED on top of GPU-resident density. Replaces the per-frame CPU subvolume gather + upload
// (the box round-trip) — the density never leaves the GPU during a stroke.
//
// The brick decode MUST bit-match voxel_mesh.wgsl read_density() and voxel_sculpt.rs at()/read_density
// (the source of truth): bi = (x>>4) + (y>>4)*br + (z>>4)*br²; EMPTY→0 / FULL→1 / handle→pool[h*4096 +
// (x&15) + (y&15)*16 + (z&15)*256].

struct GatherParams {
    box_min: vec4<u32>, // xyz = box origin cell, w = brick_res
    box_dim: vec4<u32>, // xyz = box dims (the buffers are box-local), w = 0
};
@group(0) @binding(0) var<storage, read_write> density:  array<f32>; // box-local OUT
@group(0) @binding(1) var<uniform>             p:        GatherParams;
@group(0) @binding(2) var<storage, read_write> snapshot: array<f32>; // box-local OUT (pre-frame copy)
@group(0) @binding(3) var<storage, read>       index:    array<u32>; // atlas INDEX tags
@group(0) @binding(4) var<storage, read>       pool:     array<f32>; // atlas POOL arena

const TAG_EMPTY: u32 = 0xffffffffu; // implicit all-0.0
const TAG_FULL:  u32 = 0xfffffffeu; // implicit all-1.0

fn read_density(c: vec3<u32>, br: u32) -> f32 {
    let bi = (c.x >> 4u) + (c.y >> 4u) * br + (c.z >> 4u) * br * br;
    let tag = index[bi];
    if (tag == TAG_EMPTY) { return 0.0; }
    if (tag == TAG_FULL) { return 1.0; }
    let local = (c.x & 15u) + (c.y & 15u) * 16u + (c.z & 15u) * 256u;
    return pool[tag * 4096u + local];
}

@compute @workgroup_size(4, 4, 4)
fn gather(@builtin(global_invocation_id) gid: vec3<u32>) {
    let d = p.box_dim.xyz;
    if (any(gid >= d)) { return; }
    let c = p.box_min.xyz + gid;
    let v = read_density(c, p.box_min.w);
    let lin = gid.x + gid.y * d.x + gid.z * d.x * d.y;
    density[lin] = v;  // brush works on this in place
    snapshot[lin] = v; // pre-frame state for Smooth/Inflate/Pinch (one consistent read for all stamps)
}
