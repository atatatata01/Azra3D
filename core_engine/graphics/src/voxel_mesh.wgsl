// GPU Surface Nets — extracts the iso surface of a DENSITY voxel grid into a watertight
// quad mesh (emitted as triangles), the GPU port of `VoxelGrid::mesh_region` (voxel_sculpt.rs).
// DELIBERATELY NOT an SDF: `density` is occupancy in [0,1], surface at `iso` (default 0.5).
//
// Two passes (thread-per-cell, like volume_bake — a 16³ chunk's 4096 cells exceed one workgroup,
// so we do NOT use a workgroup-per-chunk model; both passes dispatch over the cell region):
//   sn_vertex : one Surface-Nets vertex per surface-straddling cell. Atomic-appends the vertex to
//               `verts` and records its index in `cell_vert` (region-local), else 0xFFFFFFFF.
//   sn_quads  : per anchor cell, for each of the +X/+Y/+Z faces with a sign change, looks up the 4
//               shared cell vertices from `cell_vert` and appends two triangles (CPU winding).
//
// Coordinate spaces:
//   - `density` is the GLOBAL grid (res³), indexed by global cell coord.
//   - `cell_vert` is REGION-LOCAL to the vertex region [vmin .. a1] (size = vspan), so per-chunk
//     dispatches keep their own compact vertex indices. vmin = max(a0-1, 0) (the -1 halo).
//   - Quads are anchored only in [a0 .. a1] so adjacent chunks never emit the same quad.

const NO_VERT: u32 = 0xffffffffu;

struct SnParams {
    a0: vec4<u32>,    // xyz = anchor cell min (quads), w = res (grid resolution)
    a1: vec4<u32>,    // xyz = anchor cell max (inclusive)
    vmin: vec4<u32>,  // xyz = vertex region min (= max(a0-1,0))
    vspan: vec4<u32>, // xyz = vertex region size (= a1 - vmin + 1)
    misc: vec4<f32>,  // x = iso, y = bounds, z = voxel_size, w = bitcast<f32>(brick_res): 0 = DENSE
                      //     (read `density`); >0 = SPARSE brick atlas (read `index`/`pool`). Phase 4b.
};

@group(0) @binding(0) var<storage, read>       density:   array<f32>; // DENSE res³ (when brick_res==0)
@group(0) @binding(1) var<uniform>             params:    SnParams;
@group(0) @binding(2) var<storage, read_write> cell_vert: array<u32>;
@group(0) @binding(3) var<storage, read_write> verts:     array<f32>; // 8 f32 / vertex: pos.xyz nrm.xyz uv.xy
@group(0) @binding(4) var<storage, read_write> indices:   array<u32>;
@group(0) @binding(5) var<storage, read_write> counters:  array<atomic<u32>>; // [0]=vert count [1]=index count
@group(0) @binding(6) var<storage, read>       index:     array<u32>; // SPARSE top-level brick tags (when brick_res>0)
@group(0) @binding(7) var<storage, read>       pool:      array<f32>; // SPARSE 16³ brick arena

// 8 cell corners (dx,dy,dz) and the 12 edges as (corner_a, corner_b) — must match CORNER/EDGE in
// voxel_sculpt.rs. Exposed as FUNCTIONS (not module-scope `const` arrays): naga rejects indexing a
// const array with a dynamic index (IndexMustBeConstant); a function-local `var` array does allow it.
fn corner(k: u32) -> vec3<u32> {
    var c = array<vec3<u32>, 8>(
        vec3<u32>(0u,0u,0u), vec3<u32>(1u,0u,0u), vec3<u32>(0u,1u,0u), vec3<u32>(1u,1u,0u),
        vec3<u32>(0u,0u,1u), vec3<u32>(1u,0u,1u), vec3<u32>(0u,1u,1u), vec3<u32>(1u,1u,1u),
    );
    return c[k];
}
fn edge(k: u32) -> vec2<u32> {
    var e = array<vec2<u32>, 12>(
        vec2<u32>(0u,1u), vec2<u32>(0u,2u), vec2<u32>(0u,4u), vec2<u32>(1u,3u),
        vec2<u32>(1u,5u), vec2<u32>(2u,3u), vec2<u32>(2u,6u), vec2<u32>(3u,7u),
        vec2<u32>(4u,5u), vec2<u32>(4u,6u), vec2<u32>(5u,7u), vec2<u32>(6u,7u),
    );
    return e[k];
}

fn res() -> u32 { return params.a0.w; }
fn iso() -> f32 { return params.misc.x; }
fn bounds() -> f32 { return params.misc.y; }
fn voxel_size() -> f32 { return params.misc.z; }
fn brick_res() -> u32 { return bitcast<u32>(params.misc.w); } // 0 = dense, >0 = sparse atlas

// Density read; 0.0 outside the grid (matches CPU `at`). TWO paths, selected by `brick_res`:
//   DENSE (brick_res==0): flat res³ `density` buffer (res < 256, the proven path — byte-identical).
//   SPARSE atlas (brick_res>0): decode the brick from global coords, read the EMPTY/FULL/handle tag
//     from `index`, fetch from `pool` — EXACTLY `BrickGrid::at` (proven by read_density_formula_matches_at).
fn dens(x: i32, y: i32, z: i32) -> f32 {
    let r = i32(res());
    if (x < 0 || y < 0 || z < 0 || x >= r || y >= r || z >= r) { return 0.0; }
    let br = brick_res();
    if (br == 0u) {
        return density[u32(x) + u32(y) * res() + u32(z) * res() * res()];
    }
    let bi = (u32(x) >> 4u) + (u32(y) >> 4u) * br + (u32(z) >> 4u) * br * br;
    let tag = index[bi];
    if (tag == 0xffffffffu) { return 0.0; } // TAG_EMPTY
    if (tag == 0xfffffffeu) { return 1.0; } // TAG_FULL
    return pool[tag * 4096u + ((u32(x) & 15u) + (u32(y) & 15u) * 16u + (u32(z) & 15u) * 256u)];
}

fn cell_world(c: vec3<u32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return (vec3<f32>(c) / s) * 2.0 * bounds() - vec3<f32>(bounds());
}

// World position -> fractional grid coordinate.
fn world_to_grid(p: vec3<f32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return ((p + vec3<f32>(bounds())) / (2.0 * bounds())) * s;
}

// Trilinear density sample at a world position (matches CPU `sample`).
fn sample_world(p: vec3<f32>) -> f32 {
    let g = world_to_grid(p);
    let b = vec3<i32>(floor(g));
    let f = g - floor(g);
    let c000 = dens(b.x,     b.y,     b.z);
    let c100 = dens(b.x + 1, b.y,     b.z);
    let c010 = dens(b.x,     b.y + 1, b.z);
    let c110 = dens(b.x + 1, b.y + 1, b.z);
    let c001 = dens(b.x,     b.y,     b.z + 1);
    let c101 = dens(b.x + 1, b.y,     b.z + 1);
    let c011 = dens(b.x,     b.y + 1, b.z + 1);
    let c111 = dens(b.x + 1, b.y + 1, b.z + 1);
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    let y0 = mix(x00, x10, f.y);
    let y1 = mix(x01, x11, f.y);
    return mix(y0, y1, f.z);
}

// Outward normal from the density gradient at world `p` (density decreases outward), stencil `e`.
fn gradient_normal(p: vec3<f32>, e: f32) -> vec3<f32> {
    let g = vec3<f32>(
        sample_world(p + vec3<f32>(e, 0.0, 0.0)) - sample_world(p - vec3<f32>(e, 0.0, 0.0)),
        sample_world(p + vec3<f32>(0.0, e, 0.0)) - sample_world(p - vec3<f32>(0.0, e, 0.0)),
        sample_world(p + vec3<f32>(0.0, 0.0, e)) - sample_world(p - vec3<f32>(0.0, 0.0, e)),
    );
    let n = -g;
    let l = length(n);
    if (l < 1e-12) { return vec3<f32>(0.0, 0.0, 0.0); }
    return n / l;
}

// Local index into cell_vert for a cell in the vertex region [vmin .. vmin+vspan-1], plus this chunk's
// BASE OFFSET (params.a1.w) into a SHARED cell_vert buffer. Batched dispatch packs every chunk's region
// into one cell_vert (each chunk owns the disjoint slice [base .. base+vspan³)), so the resident mesher
// can run ONE vertex pass + ONE quads pass for the whole batch instead of two passes PER chunk (the CPU
// command-recording cost — ~0.08ms/chunk × hundreds). Both the write (sn_vertex) and read (cv) route
// through here, so the offset stays consistent. a1.w is 0 for a single-chunk/unbatched dispatch.
fn local_idx(c: vec3<u32>) -> u32 {
    let l = c - params.vmin.xyz;
    return params.a1.w + l.x + l.y * params.vspan.x + l.z * params.vspan.x * params.vspan.y;
}

// cell_vert lookup with region-bounds check — returns NO_VERT outside the meshed vertex region.
fn cv(x: i32, y: i32, z: i32) -> u32 {
    let lo = vec3<i32>(params.vmin.xyz);
    let hi = vec3<i32>(params.a1.xyz);
    if (x < lo.x || y < lo.y || z < lo.z || x > hi.x || y > hi.y || z > hi.z) { return NO_VERT; }
    return cell_vert[local_idx(vec3<u32>(u32(x), u32(y), u32(z)))];
}

fn solid(c: vec3<u32>) -> bool {
    return dens(i32(c.x), i32(c.y), i32(c.z)) >= iso();
}

// Pass 1: one Surface-Nets vertex per surface-straddling cell in [vmin .. a1].
@compute @workgroup_size(4, 4, 4)
fn sn_vertex(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = params.vmin.xyz + gid;
    if (any(c > params.a1.xyz)) { return; }
    let li = local_idx(c);

    // 8 corner densities in CORNER order; count inside.
    var d: array<f32, 8>;
    var inside = 0u;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let cc = c + corner(k);
        let v = dens(i32(cc.x), i32(cc.y), i32(cc.z));
        d[k] = v;
        if (v >= iso()) { inside = inside + 1u; }
    }
    if (inside == 0u || inside == 8u) { cell_vert[li] = NO_VERT; return; } // fully in/out → no surface

    // Average the 12 edge zero-crossings in cell-local [0,1]^3.
    var acc = vec3<f32>(0.0);
    var count = 0.0;
    for (var e = 0u; e < 12u; e = e + 1u) {
        let ed = edge(e);
        let a = ed.x;
        let b = ed.y;
        let da = d[a];
        let db = d[b];
        if ((da >= iso()) == (db >= iso())) { continue; }
        let tt = (iso() - da) / (db - da);
        let pa = vec3<f32>(corner(a));
        let pb = vec3<f32>(corner(b));
        acc = acc + mix(pa, pb, tt);
        count = count + 1.0;
    }
    if (count == 0.0) { cell_vert[li] = NO_VERT; return; }
    let local = acc / count;
    let world = cell_world(c) + local * voxel_size();
    // Stencil = HALF the fill AA ramp (2.0 voxels, one-sided): e must stay <= ramp/2 or the
    // central difference clips on the 0/1 plateaus -> latitude-quantized normals (concentric
    // rings) and zero gradients at the poles (black blotches). Matches CPU cell_surface_vertex_d.
    let nrm = gradient_normal(world, voxel_size() * 1.0);

    // Append the vertex (8 f32) and record its index, unless the buffer pool is full.
    let vi = atomicAdd(&counters[0], 1u);
    if ((vi + 1u) * 8u > arrayLength(&verts)) { cell_vert[li] = NO_VERT; return; }
    let o = vi * 8u;
    verts[o + 0u] = world.x; verts[o + 1u] = world.y; verts[o + 2u] = world.z;
    verts[o + 3u] = nrm.x;   verts[o + 4u] = nrm.y;   verts[o + 5u] = nrm.z;
    verts[o + 6u] = 0.0;     verts[o + 7u] = 0.0;
    cell_vert[li] = vi;
}

// Emit two triangles for a quad with the CPU winding (forward if cell is solid, else reversed).
fn emit_quad(c0: u32, c1: u32, c2: u32, c3: u32, s0: bool) {
    var q = array<u32, 4>(c0, c1, c2, c3);
    if (!s0) { q = array<u32, 4>(c3, c2, c1, c0); }
    let ii = atomicAdd(&counters[1], 6u);
    if (ii + 6u > arrayLength(&indices)) { return; } // pool full — drop (counter still advanced, clamped on read)
    indices[ii + 0u] = q[0]; indices[ii + 1u] = q[1]; indices[ii + 2u] = q[2];
    indices[ii + 3u] = q[0]; indices[ii + 4u] = q[2]; indices[ii + 5u] = q[3];
}

// Pass 2: per anchor cell [a0 .. a1], emit the +X/+Y/+Z face quads (same cell arrays + winding as
// VoxelGrid::mesh_region). Each quad is owned by exactly one anchor cell → no double emission.
@compute @workgroup_size(4, 4, 4)
fn sn_quads(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = params.a0.xyz + gid;
    if (any(c > params.a1.xyz)) { return; }
    let n = res();
    let xi = i32(c.x); let yi = i32(c.y); let zi = i32(c.z);
    let s_self = solid(c);

    // +X face
    if (c.x + 1u < n && s_self != solid(vec3<u32>(c.x + 1u, c.y, c.z))) {
        let a = cv(xi, yi - 1, zi - 1); let b = cv(xi, yi, zi - 1);
        let cc = cv(xi, yi, zi);       let dd = cv(xi, yi - 1, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }
    // +Y face
    if (c.y + 1u < n && s_self != solid(vec3<u32>(c.x, c.y + 1u, c.z))) {
        let a = cv(xi - 1, yi, zi - 1); let b = cv(xi - 1, yi, zi);
        let cc = cv(xi, yi, zi);        let dd = cv(xi, yi, zi - 1);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }
    // +Z face
    if (c.z + 1u < n && s_self != solid(vec3<u32>(c.x, c.y, c.z + 1u))) {
        let a = cv(xi - 1, yi - 1, zi); let b = cv(xi, yi - 1, zi);
        let cc = cv(xi, yi, zi);        let dd = cv(xi - 1, yi, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }

    // Clamp the index counter to the buffer length. emit_quad's overflow guard (line ~199) skips the
    // WRITE when full but the atomicAdd has ALREADY advanced counters[1], so the final count can exceed
    // arrayLength(&indices). The resident path copies counters[1] VERBATIM into the DrawIndexedIndirect
    // index_count (no CPU readback/clamp, unlike the non-resident path's .min(idx_cap)), so an unclamped
    // overflow → draw_indexed_indirect reads OOB → device loss on NVIDIA. This atomicMin runs AFTER every
    // append in this thread; since each thread mins after it appends, the globally-last atomic op on
    // counters[1] is a min → the final value is unconditionally <= the buffer length. Worst case of any
    // future emission change is dropped triangles (a visible hole), never an OOB indirect draw.
    atomicMin(&counters[1], arrayLength(&indices));
}
