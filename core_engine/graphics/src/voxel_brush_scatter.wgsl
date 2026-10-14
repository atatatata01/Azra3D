// GPU-resident brush SCATTER (sculpt Phase 5a): write the box-local brush result back into the brick
// ATLAS POOL IN PLACE, so the mesh (which already reads the atlas) updates live with NO per-frame CPU
// readback/writeback. Replaces the box-round-trip's CPU readback + set_idx writeback.
//
// Only ALLOCATED bricks (a real handle) are written; EMPTY/FULL sentinel tags are skipped — Phase 5b
// pre-materialises every touched brick into a handle on the CPU before dispatch, so the skip is a
// structural guard, not a silent data loss. Brick decode MUST bit-match voxel_brush_gather.wgsl /
// voxel_mesh.wgsl / voxel_sculpt.rs.

struct ScatterParams {
    box_min: vec4<u32>, // xyz = box origin cell, w = brick_res
    box_dim: vec4<u32>, // xyz = box dims, w = 0
};
@group(0) @binding(0) var<storage, read>       density: array<f32>; // box-local IN (brush result)
@group(0) @binding(1) var<uniform>             p:       ScatterParams;
@group(0) @binding(2) var<storage, read>       index:   array<u32>; // atlas INDEX tags
@group(0) @binding(3) var<storage, read_write> pool:    array<f32>; // atlas POOL arena (in place)

const TAG_FULL: u32 = 0xfffffffeu; // tags >= this are implicit (FULL/EMPTY) → no allocated slot

@compute @workgroup_size(4, 4, 4)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let d = p.box_dim.xyz;
    if (any(gid >= d)) { return; }
    let c = p.box_min.xyz + gid;
    let br = p.box_min.w;
    let bi = (c.x >> 4u) + (c.y >> 4u) * br + (c.z >> 4u) * br * br;
    let tag = index[bi];
    if (tag >= TAG_FULL) { return; } // EMPTY/FULL sentinel — no pool slot (5b must materialise first)
    let local = (c.x & 15u) + (c.y & 15u) * 16u + (c.z & 15u) * 256u;
    let lin = gid.x + gid.y * d.x + gid.z * d.x * d.y;
    pool[tag * 4096u + local] = density[lin];
}
