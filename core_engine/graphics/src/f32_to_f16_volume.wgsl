// f32_to_f16_volume.wgsl — GPU-side f32→f16 conversion for volume bake results.
//
// Reads f32 from the bake output buffer and writes f16 pairs (packed as u32)
// to an intermediate storage buffer. The caller then uses copy_buffer_to_texture
// to transfer the f16 data to the R16Float 3D texture.
//
// Buffer layout: tightly packed f16 values matching the 3D texture's linear layout.
// Two adjacent f16 voxels are packed into each u32 via pack2x16float.
// Even-indexed threads (along the linear voxel index) write both themselves
// and their +1 neighbor. Odd-indexed threads are no-ops.

struct ConvertParams {
    grid_res: vec3<u32>,
    _pad0: u32,
    region_min: vec3<u32>,
    _pad1: u32,
    region_max: vec3<u32>,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> src_grid: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst_f16: array<u32>;
@group(0) @binding(2) var<uniform> params: ConvertParams;

@compute @workgroup_size(4, 4, 4)
fn convert(@builtin(global_invocation_id) gid: vec3<u32>) {
    // Full-grid conversion: region_min=0, region_max=grid_res.
    // Each thread handles one voxel pair (even index writes both).
    let voxel = gid + params.region_min;
    if (any(voxel >= params.region_max) || any(voxel >= params.grid_res)) {
        return;
    }
    let idx = voxel.x + voxel.y * params.grid_res.x + voxel.z * params.grid_res.x * params.grid_res.y;

    // Only even-indexed voxels write (each u32 = 2 packed f16 texels)
    if ((idx & 1u) != 0u) {
        return;
    }

    let val0 = src_grid[idx];
    let total = params.grid_res.x * params.grid_res.y * params.grid_res.z;
    let val1 = select(0.0, src_grid[idx + 1u], idx + 1u < total);
    dst_f16[idx / 2u] = pack2x16float(vec2<f32>(val0, val1));
}
