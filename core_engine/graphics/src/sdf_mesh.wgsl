// GPU Dual Contouring (QEF) — extracts the iso=0 surface of a SIGNED-DISTANCE voxel grid into a
// watertight quad mesh (emitted as triangles). The GPU port of `core_math::chunked_dc` +
// `core_math::mesh::solve_qef`. This is the SHARED hard-surface mesher (Phase G); unlike
// `voxel_mesh.wgsl` (Surface Nets — averages crossings, rounds sharp features) it solves a per-cell
// QEF so bevels / boolean creases / hard corners are PRESERVED.
//
// Mesher 2.0: after the QEF solve, cells whose hermite normals disagree (crease/corner) run
// iterative plane projection so dual verts snap to feature intersections (GPU + CPU solve_qef).
//
// FIELD convention (differs from voxel_mesh.wgsl, which is occupancy density):
//   `field` is a SIGNED DISTANCE: dist < 0 inside, dist > 0 outside, surface at iso = 0.
//   inside test:  dist <= iso          (voxel was dist >= 0.5)
//   edge crossing: t = da / (da - db)  (linear root of the signed distance along the edge)
//   surface normal: normalize(grad(field))  — gradient points OUTWARD (toward increasing distance).
//
// Two passes (thread-per-cell, like volume_bake / voxel_mesh):
//   dc_vertex : one QEF vertex per surface-straddling cell. Accumulates A^T A / A^T b from each edge
//               crossing (point + gradient-normal plane), solves the 3x3 via Cramer's rule (mesh.rs
//               solve_qef), Tikhonov-regularized toward the cell center, with the same degenerate /
//               too-far fallback to the crossing-point average. Atomic-appends to `verts`.
//   dc_quads  : per anchor cell, for each +X/+Y/+Z face with a sign change, looks up the 4 shared cell
//               vertices and appends two triangles (winding by the anchor cell's inside/outside).
//
// Coordinate spaces match voxel_mesh.wgsl exactly so the resident-pool plumbing (Phase G1) is shared:
//   `field` is the GLOBAL grid (res^3), indexed by global cell coord.
//   `cell_vert` addressing is DUAL-PATH (keyed on brick_res, like fld()):
//     DENSE (brick_res == 0): REGION-LOCAL to [vmin .. a1] (size vspan), with this chunk's base
//       offset (a1.w) into a shared buffer, so batched dispatches keep compact per-chunk indices.
//     SPARSE (brick_res > 0, Tiled-DC stage A): BRICK-LOCAL — a cell owned by brick B (B = the
//       brick of its corner-0 voxel, c>>3) with pool handle H stores its entry at
//       `cell_vert[H*512 + local]`, the SAME indirection fld() uses for the field. Cells owned by
//       SENTINEL bricks have NO storage (and provably no vertex — see cell_slot). This makes
//       cell_vert surface-proportional (allocated_bricks x 512) instead of cells^3-dense.

const NO_VERT: u32 = 0xffffffffu;
// "This cell has no cell_vert storage slot" — sparse path only, for cells owned by sentinel
// bricks. Distinct name, same bit pattern as NO_VERT (a real slot index never reaches 2^32-1:
// the pool is capped far below 4G entries by the storage binding limit).
const NO_SLOT: u32 = 0xffffffffu;

struct DcParams {
    a0: vec4<u32>,    // xyz = anchor cell min (quads), w = res (grid resolution, points/axis)
    a1: vec4<u32>,    // xyz = anchor cell max (inclusive), w = cell_vert base offset for this chunk
    vmin: vec4<u32>,  // xyz = vertex region min (= max(a0-1,0)); w = LOD level (O2): cells span
                      //     (1<<level) voxels — 0 (default in every builder) = today's meshing,
                      //     bit-identical. Level > 0 is SPARSE-ONLY (CPU-asserted) and used by
                      //     the monolithic per-body LOD dispatch, never the tiled entries.
    vspan: vec4<u32>, // xyz = vertex region size (= a1 - vmin + 1)
    gmin: vec4<f32>,  // xyz = grid_min (world, body-local), w = iso (0.0 for SDF)
    gext: vec4<f32>,  // xyz = grid_max - grid_min (world box extent), w = bitcast(brick_res); 0 = dense
};

@group(0) @binding(0) var<storage, read>       field:     array<f32>; // DENSE res^3 signed distance (brick_res=0)
@group(0) @binding(1) var<uniform>             params:    DcParams;
@group(0) @binding(2) var<storage, read_write> cell_vert: array<u32>;
@group(0) @binding(3) var<storage, read_write> verts:     array<f32>; // 8 f32 / vertex: pos.xyz nrm.xyz uv.xy
@group(0) @binding(4) var<storage, read_write> indices:   array<u32>;
@group(0) @binding(5) var<storage, read_write> counters:  array<atomic<u32>>; // [0]=vert count [1]=index count
// Sparse SDF brick atlas (Phase 3). Used when brick_res() > 0; bound to 1-elem dummies on the dense
// path. `brick_index` holds per-brick tags (EXTERIOR 0xFFFFFFFF -> +far, INTERIOR 0xFFFFFFFE -> -far,
// else a pool handle); `brick_pool` is the dense-packed 8^3 (512 f32) distance arena.
@group(0) @binding(6) var<storage, read>       brick_index: array<u32>;
@group(0) @binding(7) var<storage, read>       brick_pool:  array<f32>;

// 8 cell corners (dx,dy,dz) and the 12 edges as (corner_a, corner_b) — must match CORNER/EDGES in
// core_math (mesh.rs). Exposed as functions (naga rejects dynamic indexing of a module const array).
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
// Production SDF iso is always 0. `gmin.w` is REPURPOSED as Mesher-2.0 feature flag
// (1.0 = B2 toggle only; 0.0 = stock DC — bit-identical to pre-M2 when off).
fn iso() -> f32 { return 0.0; }
fn mesher2_feature() -> bool { return params.gmin.w > 0.5; }
fn gmn() -> vec3<f32> { return params.gmin.xyz; }              // grid_min (world)
fn gex() -> vec3<f32> { return params.gext.xyz; }              // grid box extent (grid_max - grid_min)
// O2 LOD stride: a level-L cell spans (1<<L) voxels. 0 everywhere except the per-body LOD
// dispatch — every stride-dependent expression below reduces to its pre-O2 form at L=0.
fn stride() -> u32 { return 1u << params.vmin.w; }
fn vox() -> vec3<f32> { return params.gext.xyz / f32(res() - 1u) * f32(stride()); } // per-axis CELL size

// Bricks per axis, packed into gext.w via bitcast (0 = dense res^3 field). Mirrors voxel_mesh.wgsl's
// SnParams.misc.w convention so the resident-pool plumbing stays shared.
fn brick_res() -> u32 { return bitcast<u32>(params.gext.w); }

// Storage slot of cell `c` in `cell_vert` (Tiled-DC stage A). DENSE: the region-local compact
// index (unchanged addressing). SPARSE: brick-local — handle*512 + local, the identical
// indirection fld() uses; sentinel-owned cells return NO_SLOT (no storage).
//
// SOUNDNESS (why a sentinel-owned cell never needs a vertex): a DC vertex requires a DECODED sign
// change among the cell's 8 corner voxels (c .. c+1). All 8 corner bricks lie within one step of
// each other, so they are mutually 26-adjacent. (a) If ALL corner bricks are sentinels: an
// INTERIOR sentinel (every true voxel < -band) can never be 26-adjacent to an EXTERIOR sentinel
// (every true voxel > band) for a field whose per-voxel step is <= band (the classifier's standing
// band assumption, already load-bearing for the fld() decode parity) — so all sentinel corners
// decode ONE sign, no crossing. (b) If a crossing involves a real (handle-brick) voxel: sentinels
// decode the true sign (their construction), so the decoded crossing is a TRUE crossing between
// adjacent voxels; both voxels then sit within `band` of iso, making BOTH their bricks SURFACE
// bricks, and every corner brick of the cell — including the owning brick c>>3 — is in their
// 26-ring apron (dilate_surface_bricks_1ring) and holds a handle. Contrapositive: a sentinel-owned
// cell has no crossing. The quad pass's neighbor reads (cv at c-1..c) need vertices only from
// crossing cells, which by the same argument are all handle-owned — a sentinel-owned neighbor read
// correctly returns NO_VERT (it is not "missing" a vertex; none exists). Verified bit-exact vs
// dense by the sdf_dc_sparse_* parity gates incl. the brick-boundary adversarial scene.
fn cell_slot(c: vec3<u32>) -> u32 {
    let br = brick_res();
    if (br == 0u) { return local_idx(c); }
    // O2 LOD: cell → corner-0 VOXEL (v = c << level). Distinct cells map to distinct voxels
    // (stride-spaced), so slots stay collision-free at any level; at L=0 this is `c` verbatim.
    let v = c * stride();
    let b = v >> vec3<u32>(3u, 3u, 3u);
    let tag = brick_index[b.x + b.y * br + b.z * br * br];
    if (tag >= 0xfffffffeu) { return NO_SLOT; } // EXTERIOR/INTERIOR sentinel — no storage
    let l = v & vec3<u32>(7u, 7u, 7u);
    return tag * 512u + l.x + l.y * 8u + l.z * 64u;
}

// Signed distance read; +large outside the grid so out-of-bounds reads as "outside" (matches the CPU
// FlatGridAccessor clamp behaviour for the dense path). Dual path: dense res^3 buffer when brick_res==0,
// else the sparse 8^3 brick atlas (decode mirrors core_math::chunked_dc::BrickGridAccessor::sample —
// proven bit-exact by sdf_dc_sparse_brick_decode_parity).
fn fld(x: i32, y: i32, z: i32) -> f32 {
    let r = i32(res());
    if (x < 0 || y < 0 || z < 0 || x >= r || y >= r || z >= r) { return 1e6; }
    let br = brick_res();
    if (br == 0u) { return field[u32(x) + u32(y) * res() + u32(z) * res() * res()]; }
    // Sparse 8^3 brick decode.
    let ux = u32(x); let uy = u32(y); let uz = u32(z);
    let bi = (ux >> 3u) + (uy >> 3u) * br + (uz >> 3u) * br * br;
    let tag = brick_index[bi];
    if (tag == 0xffffffffu) { return 100.0; }   // EXTERIOR -> +far
    if (tag == 0xfffffffeu) { return -100.0; }  // INTERIOR -> -far
    let local = (ux & 7u) + (uy & 7u) * 8u + (uz & 7u) * 64u;
    return brick_pool[tag * 512u + local];
}

fn cell_world(c: vec3<u32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return gmn() + (vec3<f32>(c * stride()) / s) * gex(); // cell corner-0 voxel = c << level
}

fn world_to_grid(p: vec3<f32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return ((p - gmn()) / gex()) * s;
}

// Trilinear signed-distance sample at a world position.
fn sample_world(p: vec3<f32>) -> f32 {
    let g = world_to_grid(p);
    let b = vec3<i32>(floor(g));
    let f = g - floor(g);
    let c000 = fld(b.x,     b.y,     b.z);
    let c100 = fld(b.x + 1, b.y,     b.z);
    let c010 = fld(b.x,     b.y + 1, b.z);
    let c110 = fld(b.x + 1, b.y + 1, b.z);
    let c001 = fld(b.x,     b.y,     b.z + 1);
    let c101 = fld(b.x + 1, b.y,     b.z + 1);
    let c011 = fld(b.x,     b.y + 1, b.z + 1);
    let c111 = fld(b.x + 1, b.y + 1, b.z + 1);
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    let y0 = mix(x00, x10, f.y);
    let y1 = mix(x01, x11, f.y);
    return mix(y0, y1, f.z);
}

// Outward unit normal from the signed-distance gradient at world `p` (distance INCREASES outward, so
// the gradient already points out — no negation, unlike the density mesher). Returns 0 if degenerate.
fn gradient_normal(p: vec3<f32>, e: f32) -> vec3<f32> {
    let g = vec3<f32>(
        sample_world(p + vec3<f32>(e, 0.0, 0.0)) - sample_world(p - vec3<f32>(e, 0.0, 0.0)),
        sample_world(p + vec3<f32>(0.0, e, 0.0)) - sample_world(p - vec3<f32>(0.0, e, 0.0)),
        sample_world(p + vec3<f32>(0.0, 0.0, e)) - sample_world(p - vec3<f32>(0.0, 0.0, e)),
    );
    let l = length(g);
    if (l < 1e-12) { return vec3<f32>(0.0, 0.0, 0.0); }
    return g / l;
}

fn local_idx(c: vec3<u32>) -> u32 {
    let l = c - params.vmin.xyz;
    return params.a1.w + l.x + l.y * params.vspan.x + l.z * params.vspan.x * params.vspan.y;
}

fn cv(x: i32, y: i32, z: i32) -> u32 {
    let lo = vec3<i32>(params.vmin.xyz);
    let hi = vec3<i32>(params.a1.xyz);
    if (x < lo.x || y < lo.y || z < lo.z || x > hi.x || y > hi.y || z > hi.z) { return NO_VERT; }
    // Dual addressing (see cell_slot): a sentinel-owned neighbor cell has no storage AND provably
    // no vertex — NO_VERT is the correct value, not a fallback.
    let sl = cell_slot(vec3<u32>(u32(x), u32(y), u32(z)));
    if (sl == NO_SLOT) { return NO_VERT; }
    return cell_vert[sl];
}

fn solid(c: vec3<u32>) -> bool {
    let v = c * stride(); // cell corner-0 voxel (identity at L=0)
    return fld(i32(v.x), i32(v.y), i32(v.z)) <= iso();
}

// Result of the per-cell QEF solve (shared by the monolithic dc_vertex and the C1 tiled
// dc_vertex_tiled — ONE solver, two dispatch shapes). `ok == 0` = the cell straddles no surface
// (fully in/out, or no edge crossing) — the caller stores NO_VERT and emits nothing.
struct DcCellVert {
    ok: u32,
    pos: vec3<f32>,
    nrm: vec3<f32>,
};

// The QEF Dual-Contouring vertex solve for cell `c` — verbatim relocation of the dc_vertex body
// (corner sampling, edge-crossing accumulation, Tikhonov-regularized Cramer solve, crossing-normal
// average + degenerate repair). Bit-identical arithmetic; naga inlines this into both entries.
fn dc_cell_solve(c: vec3<u32>) -> DcCellVert {
    var out: DcCellVert;
    out.ok = 0u;

    // 8 corner signed distances in CORNER order; count inside.
    var d: array<f32, 8>;
    var inside = 0u;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let cc = (c + corner(k)) * stride(); // cell corners are stride-spaced voxels (O2 LOD)
        let v = fld(i32(cc.x), i32(cc.y), i32(cc.z));
        d[k] = v;
        if (v <= iso()) { inside = inside + 1u; }
    }
    if (inside == 0u || inside == 8u) { return out; } // fully in/out → no surface

    let vsz = vox(); // per-axis cell size
    let cw = cell_world(c); // world position of corner 0 of this cell
    let vmin_axis = min(vsz.x, min(vsz.y, vsz.z));
    let vmax_axis = max(vsz.x, max(vsz.y, vsz.z));
    let stencil = vmin_axis * 1.5; // scalar finite-difference epsilon for the gradient normal

    // Accumulate the QEF: A^T A (3x3 symmetric) and A^T b, from each edge crossing's (point, normal)
    // plane. Also accumulate the crossing-point average for the fallback + the regularization target.
    // Mesher 2.0: keep hermite planes for feature projection after the QEF (sharp creases).
    var ata00 = 0.0; var ata01 = 0.0; var ata02 = 0.0;
    var ata11 = 0.0; var ata12 = 0.0; var ata22 = 0.0;
    var atb = vec3<f32>(0.0);
    var psum = vec3<f32>(0.0); // sum of crossing world points
    var nsum = vec3<f32>(0.0); // sum of crossing plane normals (robust vertex normal at creases)
    var count = 0.0;
    var plane_p: array<vec3<f32>, 12>;
    var plane_n: array<vec3<f32>, 12>;
    var nplanes = 0u;

    for (var ei = 0u; ei < 12u; ei = ei + 1u) {
        let ed = edge(ei);
        let a = ed.x; let b = ed.y;
        let da = d[a]; let db = d[b];
        if ((da <= iso()) == (db <= iso())) { continue; } // no sign change
        let tt = da / (da - db); // signed-distance root along the edge
        let pa = vec3<f32>(corner(a));
        let pb = vec3<f32>(corner(b));
        let local = mix(pa, pb, tt);                 // cell-local crossing [0,1]^3
        let pw = cw + local * vsz;                   // world crossing point
        let nrm = gradient_normal(pw, stencil);      // plane normal at the crossing
        psum = psum + pw;
        nsum = nsum + nrm;
        count = count + 1.0;
        if (nplanes < 12u) {
            plane_p[nplanes] = pw;
            plane_n[nplanes] = nrm;
            nplanes = nplanes + 1u;
        }
        // A^T A += n ⊗ n ; A^T b += (n·p) n
        let np = dot(nrm, pw);
        ata00 = ata00 + nrm.x * nrm.x; ata01 = ata01 + nrm.x * nrm.y; ata02 = ata02 + nrm.x * nrm.z;
        ata11 = ata11 + nrm.y * nrm.y; ata12 = ata12 + nrm.y * nrm.z; ata22 = ata22 + nrm.z * nrm.z;
        atb = atb + np * nrm;
    }
    if (count == 0.0) { return out; }

    let avg = psum / count;                          // crossing-point average (Surface-Nets fallback)

    // Tikhonov regularization toward the cell center (mesh.rs: reg = 0.01 * n, pull toward `fallback`).
    let reg = 0.01 * count;
    ata00 = ata00 + reg; ata11 = ata11 + reg; ata22 = ata22 + reg;
    atb = atb + reg * avg;

    // Solve the symmetric 3x3 system via Cramer's rule (mesh.rs solve_qef).
    let m00 = ata00; let m01 = ata01; let m02 = ata02;
    let m10 = ata01; let m11 = ata11; let m12 = ata12; // symmetric
    let m20 = ata02; let m21 = ata12; let m22 = ata22;
    let det = m00 * (m11*m22 - m12*m21)
            - m01 * (m10*m22 - m12*m20)
            + m02 * (m10*m21 - m11*m20);

    var world: vec3<f32>;
    if (abs(det) < 1e-10) {
        world = avg; // degenerate — fall back to the crossing average
    } else {
        let inv = 1.0 / det;
        let x = inv * (atb.x * (m11*m22 - m12*m21)
                     - m01 * (atb.y*m22 - m12*atb.z)
                     + m02 * (atb.y*m21 - m11*atb.z));
        let y = inv * (m00 * (atb.y*m22 - m12*atb.z)
                     - atb.x * (m10*m22 - m12*m20)
                     + m02 * (m10*atb.z - atb.y*m20));
        let z = inv * (m00 * (m11*atb.z - atb.y*m21)
                     - m01 * (m10*atb.z - atb.y*m20)
                     + atb.x * (m10*m21 - m11*m20));
        let r = vec3<f32>(x, y, z);
        // Sanity: reject a solution that strayed too far from the cell (matches the CPU 1.5*cell guard).
        if (length(r - avg) > vmax_axis * 1.5) { world = avg; } else { world = r; }
    }

    // ── Mesher 2.0 (B2 toggle only — stock DC leaves gmin.w = 0, this block is skipped) ──
    // Feature plane projection: creases/corners snap to hermite plane intersections.
    if (mesher2_feature() && nplanes >= 2u) {
        var min_ndot = 1.0;
        for (var i = 0u; i < nplanes; i = i + 1u) {
            for (var j = i + 1u; j < nplanes; j = j + 1u) {
                min_ndot = min(min_ndot, abs(dot(plane_n[i], plane_n[j])));
            }
        }
        // ~25°+ angle between some plane pair → treat as feature cell
        if (min_ndot < 0.90) {
            for (var it = 0u; it < 4u; it = it + 1u) {
                for (var i = 0u; i < nplanes; i = i + 1u) {
                    let err = dot(plane_n[i], world - plane_p[i]);
                    world = world - plane_n[i] * err;
                }
            }
            if (length(world - avg) > vmax_axis * 1.5) {
                world = avg;
            }
        }
    }

    // Final vertex normal = AVERAGE of the edge-crossing plane normals (the same unit normals fed to
    // the QEF above). This is robust at a concave subtract crease: the placed vertex sits on the V-fold
    // of max(base,-cutter), so re-sampling a central-difference gradient there (the old approach)
    // straddles BOTH field branches → the gradient collapses to ~0 or flips erratically, and the GPU
    // path has NO degenerate-normal repair (unlike the CPU `repair_degenerate_normals`) → it wrote that
    // raw, producing the jagged/eroded crater rim ONLY along cuts (convex bevels keep a clean gradient).
    // The crossing-normal average is the outward bisector of the meeting faces and never collapses.
    // Smooth/flat surfaces are unchanged (all crossings share one normal → average == that normal).
    let nlen = length(nsum);
    var nrm: vec3<f32>;
    if (nlen > 1e-6) {
        nrm = nsum / nlen;
    } else {
        // The crossing normals cancelled — a sharp subtract crease (the V-fold of max(base,-cutter), where
        // the base and cutter face normals are near-antiparallel). Try the gradient; if THAT is also
        // degenerate (the field is non-differentiable exactly at the fold), fall back to a geometry-derived
        // direction so a zero normal is NEVER stored. A zero normal → normalize(0)=NaN in the G-buffer →
        // N·L=0 and shadow=0 → BLACK faces in Solid/HDR (matcap masks it by sampling a texel). This is the
        // GPU equivalent of the CPU `repair_degenerate_normals` (chunked_dc.rs) the GPU mesher was missing.
        nrm = gradient_normal(world, stencil);
        if (length(nrm) < 1e-6) {
            let dir = world - (cw + vsz * 0.5); // cell-centre → vertex; non-zero unless vertex == centre
            let dl = length(dir);
            nrm = select(vec3<f32>(0.0, 1.0, 0.0), dir / dl, dl > 1e-6);
        }
    }

    out.ok = 1u;
    out.pos = world;
    out.nrm = nrm;
    return out;
}

// Pass 1: one QEF Dual-Contouring vertex per surface-straddling cell in [vmin .. a1].
@compute @workgroup_size(4, 4, 4)
fn dc_vertex(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = params.vmin.xyz + gid;
    if (any(c > params.a1.xyz)) { return; }
    // Dual addressing (Tiled-DC stage A, see cell_slot). SPARSE sentinel-owned cell: no storage to
    // write and provably no surface crossing (soundness note above) — plain return; cv() reads of
    // this cell return NO_VERT structurally. This also SUBSUMES the old cell_uniform_sentinel
    // fast-reject (one brick-tag read skips the interior/exterior bulk, Phase 3B slice 1): every
    // cell it rejected has a sentinel corner-0 brick, and a handle-owned cell was never rejected
    // (its bmin tag is a handle). DENSE: li is the region-local index — behavior byte-identical.
    let li = cell_slot(c);
    if (li == NO_SLOT) { return; }

    let r = dc_cell_solve(c);
    if (r.ok == 0u) { cell_vert[li] = NO_VERT; return; }

    let vi = atomicAdd(&counters[0], 1u);
    if ((vi + 1u) * 8u > arrayLength(&verts)) { cell_vert[li] = NO_VERT; return; }
    let o = vi * 8u;
    verts[o + 0u] = r.pos.x; verts[o + 1u] = r.pos.y; verts[o + 2u] = r.pos.z;
    verts[o + 3u] = r.nrm.x; verts[o + 4u] = r.nrm.y; verts[o + 5u] = r.nrm.z;
    verts[o + 6u] = 0.0;     verts[o + 7u] = 0.0;
    cell_vert[li] = vi;
}

// Emit two triangles for a quad; winding forward if the anchor cell is solid, else reversed.
fn emit_quad(c0: u32, c1: u32, c2: u32, c3: u32, s0: bool) {
    var q = array<u32, 4>(c0, c1, c2, c3);
    if (!s0) { q = array<u32, 4>(c3, c2, c1, c0); }
    let ii = atomicAdd(&counters[1], 6u);
    if (ii + 6u > arrayLength(&indices)) { return; } // pool full — drop (counter clamped on read)
    indices[ii + 0u] = q[0]; indices[ii + 1u] = q[1]; indices[ii + 2u] = q[2];
    indices[ii + 3u] = q[0]; indices[ii + 4u] = q[2]; indices[ii + 5u] = q[3];
}

// Pass 2: per anchor cell [a0 .. a1], emit the +X/+Y/+Z face quads. Each quad is owned by exactly one
// anchor cell → no double emission. Identical structure to voxel_mesh.wgsl sn_quads.
@compute @workgroup_size(4, 4, 4)
fn dc_quads(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = params.a0.xyz + gid;
    if (any(c > params.a1.xyz)) { return; }
    let n = res();
    let xi = i32(c.x); let yi = i32(c.y); let zi = i32(c.z);
    let s_self = solid(c);

    if (c.x + 1u < n && s_self != solid(vec3<u32>(c.x + 1u, c.y, c.z))) {
        let a = cv(xi, yi - 1, zi - 1); let b = cv(xi, yi, zi - 1);
        let cc = cv(xi, yi, zi);       let dd = cv(xi, yi - 1, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }
    if (c.y + 1u < n && s_self != solid(vec3<u32>(c.x, c.y + 1u, c.z))) {
        let a = cv(xi - 1, yi, zi - 1); let b = cv(xi - 1, yi, zi);
        let cc = cv(xi, yi, zi);        let dd = cv(xi, yi, zi - 1);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }
    if (c.z + 1u < n && s_self != solid(vec3<u32>(c.x, c.y, c.z + 1u))) {
        let a = cv(xi - 1, yi - 1, zi); let b = cv(xi, yi - 1, zi);
        let cc = cv(xi, yi, zi);        let dd = cv(xi - 1, yi, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad(a, b, cc, dd, s_self); }
    }

    // Clamp the index counter so the resident DrawIndexedIndirect index_count can never exceed the
    // buffer length (OOB indirect draw = NVIDIA device loss). atomicMin after every append guarantees
    // the globally-last op on counters[1] is a min. See voxel_mesh.wgsl for the full rationale.
    atomicMin(&counters[1], arrayLength(&indices));
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// TILED incremental DC (C1) — per-tile mesh regions + per-tile DrawIndexedIndirect entries.
//
// A tile = 32³ CELLS (= 4³ atlas bricks). Each SURFACE tile (≥1 handle brick) owns:
//   - a sub-region of the body's pooled verts/idx (self-contained index space: quads reference
//     ONLY own-tile vertices; the 1-cell vertex halo below the anchor window is DUPLICATED per
//     tile — this is what lets C2 re-mesh one tile without touching its neighbors' indices),
//   - one DrawIndexedIndirect entry (index_count GPU-written by dc_tile_args from the per-tile
//     counters; base_vertex/first_index = the tile's absolute region bases),
//   - one atomic counter pair (`counters[tmeta.z*2]` verts, `[tmeta.z*2+1]` indices — tmeta.z is
//     the tile's REAL entry index; a full dispatch sets tmeta.z = entry, a C2 incremental
//     dispatch iterates a compacted scratch TileInfo array whose tmeta.z points home),
//   - a per-batch cell_vert SCRATCH window of TILE_CV_STRIDE slots (disjoint per tile within a
//     batch → ALL tiles of a batch dispatch in ONE vertex pass + ONE quads pass, no races).
//
// These entries use a 9-binding layout (0-8): the 8 monolithic bindings + `tiles` (binding 8).
// The monolithic entries above are untouched — their 8-binding pipeline layout stays valid
// (they never statically reference `tiles`).
//
// Params reuse (per batch): a0.w=res, gmin/gext as monolithic (gext.w = brick_res, always > 0 on
// this path), a1.w = first tile ENTRY of the batch, vspan.w = end entry (exclusive). The scratch
// window of a tile is (entry - a1.w) * TILE_CV_STRIDE — batch-local, so a bounded scratch buffer
// serves any number of tiles across batches (pass boundaries order the reuse).
// ═══════════════════════════════════════════════════════════════════════════════════════════════

const TILE_VSPAN: u32 = 33u;         // 32-cell tile + 1-cell vertex halo per axis
const TILE_CV_STRIDE: u32 = 35937u;  // TILE_VSPAN³ cell_vert scratch slots per batch tile

struct TileInfo {
    amin: vec4<u32>,  // xyz = quad anchor min (cells); w = vert base (absolute VERTICES in pool verts)
    amax: vec4<u32>,  // xyz = quad anchor max (inclusive); w = vert cap (this tile's region, vertices)
    vmn:  vec4<u32>,  // xyz = vertex region min (= max(amin-1, 0)); w = idx base (absolute u32s)
    // x = idx cap (u32s); y = first_instance for the args entry; z = the tile's REAL entry index
    // in the shared counter/args space (C2 compaction: an incremental dispatch ranges over a
    // COMPACTED scratch TileInfo array, so `entry` is a scratch index — counters and args always
    // address tmeta.z; a full dispatch writes tmeta.z = entry, making both paths one code path);
    // w reserved.
    tmeta: vec4<u32>,
};

@group(0) @binding(8) var<storage, read> tiles: array<TileInfo>;

fn tile_scratch_slot(entry: u32, t: TileInfo, c: vec3<u32>) -> u32 {
    let l = c - t.vmn.xyz;
    return (entry - params.a1.w) * TILE_CV_STRIDE + l.x + l.y * TILE_VSPAN + l.z * TILE_VSPAN * TILE_VSPAN;
}

// True iff cell `c`'s owning brick (corner-0 voxel's brick) is an EXTERIOR/INTERIOR sentinel — the
// same soundness argument as cell_slot: a sentinel-owned cell provably holds no DC vertex.
fn cell_brick_sentinel(c: vec3<u32>) -> bool {
    let br = brick_res();
    if (br == 0u) { return false; }
    let b = c >> vec3<u32>(3u, 3u, 3u);
    return brick_index[b.x + b.y * br + b.z * br * br] >= 0xfffffffeu;
}

// Tiled twin of cv(): vertex lookup CLIPPED to this tile's own vertex region — a cell outside
// [vmn, amax] returns NO_VERT (the halo duplication guarantees every vertex a tile's quads need
// exists INSIDE its own region). Sentinel-owned cells have no scratch slot and no vertex.
fn cv_tiled(entry: u32, t: TileInfo, x: i32, y: i32, z: i32) -> u32 {
    let lo = vec3<i32>(t.vmn.xyz);
    let hi = vec3<i32>(t.amax.xyz);
    if (x < lo.x || y < lo.y || z < lo.z || x > hi.x || y > hi.y || z > hi.z) { return NO_VERT; }
    let c = vec3<u32>(u32(x), u32(y), u32(z));
    if (cell_brick_sentinel(c)) { return NO_VERT; }
    return cell_vert[tile_scratch_slot(entry, t, c)];
}

// Tiled pass 1: QEF vertex per surface cell of each batch tile's vertex region [vmn .. amax].
// Dispatch: x = 9³ = 729 workgroups (block grid over ≤TILE_VSPAN cells/axis), y = tiles in batch.
@compute @workgroup_size(4, 4, 4)
fn dc_vertex_tiled(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let entry = params.a1.w + wg.y;
    if (entry >= params.vspan.w) { return; }
    let t = tiles[entry];
    let blk = vec3<u32>(wg.x % 9u, (wg.x / 9u) % 9u, wg.x / 81u);
    let c = t.vmn.xyz + blk * 4u + lid;
    if (any(c > t.amax.xyz)) { return; }
    // Sentinel-owned cell: no scratch write needed — cv_tiled decodes NO_VERT from the brick tag.
    if (cell_brick_sentinel(c)) { return; }
    let sl = tile_scratch_slot(entry, t, c);
    let r = dc_cell_solve(c);
    if (r.ok == 0u) { cell_vert[sl] = NO_VERT; return; }
    // Counters live at the tile's REAL entry (tmeta.z) — scratch `entry` only addresses cell_vert.
    let vi = atomicAdd(&counters[t.tmeta.z * 2u], 1u);
    // Per-tile cap guard (verts is the WHOLE pool buffer here — arrayLength cannot express the
    // tile's cap, so the cap travels in TileInfo). The counter keeps counting past the cap
    // (unclamped) — that is the overflow detection signal the poll readback consumes.
    if (vi >= t.amax.w || (t.amin.w + vi + 1u) * 8u > arrayLength(&verts)) {
        cell_vert[sl] = NO_VERT;
        return;
    }
    let o = (t.amin.w + vi) * 8u;
    verts[o + 0u] = r.pos.x; verts[o + 1u] = r.pos.y; verts[o + 2u] = r.pos.z;
    verts[o + 3u] = r.nrm.x; verts[o + 4u] = r.nrm.y; verts[o + 5u] = r.nrm.z;
    verts[o + 6u] = 0.0;     verts[o + 7u] = 0.0;
    cell_vert[sl] = vi; // TILE-LOCAL index — the draw applies base_vertex = t.amin.w
}

fn emit_quad_tiled(t: TileInfo, ci: u32, c0: u32, c1: u32, c2: u32, c3: u32, s0: bool) {
    var q = array<u32, 4>(c0, c1, c2, c3);
    if (!s0) { q = array<u32, 4>(c3, c2, c1, c0); }
    let ii = atomicAdd(&counters[ci], 6u);
    if (ii + 6u > t.tmeta.x) { return; } // tile idx region full — drop (counter clamped below)
    let b = t.vmn.w + ii;
    if (b + 6u > arrayLength(&indices)) { return; }
    indices[b + 0u] = q[0]; indices[b + 1u] = q[1]; indices[b + 2u] = q[2];
    indices[b + 3u] = q[0]; indices[b + 4u] = q[2]; indices[b + 5u] = q[3];
}

// Tiled pass 2: +X/+Y/+Z face quads per anchor cell of each batch tile's window [amin .. amax].
// Tile anchor windows PARTITION the surface (each anchor cell is owned by exactly one tile), so no
// quad is emitted twice; every vertex a quad needs lives in the tile's own halo-duplicated region.
// Dispatch: x = 8³ = 512 workgroups (block grid over ≤32 cells/axis), y = tiles in batch.
@compute @workgroup_size(4, 4, 4)
fn dc_quads_tiled(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let entry = params.a1.w + wg.y;
    if (entry >= params.vspan.w) { return; }
    let t = tiles[entry];
    let ci = t.tmeta.z * 2u + 1u; // REAL entry's index counter (see TileInfo.tmeta.z)
    let blk = vec3<u32>(wg.x % 8u, (wg.x / 8u) % 8u, wg.x / 64u);
    let c = t.amin.xyz + blk * 4u + lid;
    if (any(c > t.amax.xyz)) {
        // Still participate in the trailing clamp (threads outside the window emit nothing).
        atomicMin(&counters[ci], t.tmeta.x);
        return;
    }
    let n = res();
    let xi = i32(c.x); let yi = i32(c.y); let zi = i32(c.z);
    let s_self = solid(c);

    if (c.x + 1u < n && s_self != solid(vec3<u32>(c.x + 1u, c.y, c.z))) {
        let a = cv_tiled(entry, t, xi, yi - 1, zi - 1); let b = cv_tiled(entry, t, xi, yi, zi - 1);
        let cc = cv_tiled(entry, t, xi, yi, zi);        let dd = cv_tiled(entry, t, xi, yi - 1, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad_tiled(t, ci, a, b, cc, dd, s_self); }
    }
    if (c.y + 1u < n && s_self != solid(vec3<u32>(c.x, c.y + 1u, c.z))) {
        let a = cv_tiled(entry, t, xi - 1, yi, zi - 1); let b = cv_tiled(entry, t, xi - 1, yi, zi);
        let cc = cv_tiled(entry, t, xi, yi, zi);        let dd = cv_tiled(entry, t, xi, yi, zi - 1);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad_tiled(t, ci, a, b, cc, dd, s_self); }
    }
    if (c.z + 1u < n && s_self != solid(vec3<u32>(c.x, c.y, c.z + 1u))) {
        let a = cv_tiled(entry, t, xi - 1, yi - 1, zi); let b = cv_tiled(entry, t, xi, yi - 1, zi);
        let cc = cv_tiled(entry, t, xi, yi, zi);        let dd = cv_tiled(entry, t, xi - 1, yi, zi);
        if (a != NO_VERT && b != NO_VERT && cc != NO_VERT && dd != NO_VERT) { emit_quad_tiled(t, ci, a, b, cc, dd, s_self); }
    }

    // Per-tile clamp: the globally-last op on this tile's index counter is a min against its idx
    // cap, so dc_tile_args can never publish an index_count past the tile's region (same NVIDIA
    // OOB-indirect-draw rationale as the monolithic clamp above).
    atomicMin(&counters[ci], t.tmeta.x);
}

// Tiled pass 3 (args fill): publish each tile's DrawIndexedIndirect entry from its counters —
// replaces N per-tile copy_buffer_to_buffer commands with ONE dispatch. For THIS entry point,
// binding 4 (`indices`) is the pool's INDIRECT args buffer viewed as u32s (entry stride 5):
//   [index_count, instance_count, first_index, base_vertex, first_instance].
// instance_count is constant 1 — per-frame visibility is the CPU draw list (a hidden body's tile
// block is simply never multi-drawn), not per-entry zeroing.
@compute @workgroup_size(64)
fn dc_tile_args(@builtin(global_invocation_id) gid: vec3<u32>) {
    let entry = params.a1.w + gid.x;
    if (entry >= params.vspan.w) { return; }
    let t = tiles[entry];
    // Both the counter read and the args slot address the REAL entry (tmeta.z) — a C2 incremental
    // dispatch ranges over compacted scratch entries but publishes into the live args block.
    let ic = min(atomicLoad(&counters[t.tmeta.z * 2u + 1u]), t.tmeta.x);
    let o = t.tmeta.z * 5u;
    if (o + 5u > arrayLength(&indices)) { return; }
    indices[o + 0u] = ic;       // index_count (GPU truth, clamped)
    indices[o + 1u] = 1u;       // instance_count (draw-list visibility, not per-entry)
    indices[o + 2u] = t.vmn.w;  // first_index (absolute u32 offset in pool idx)
    indices[o + 3u] = t.amin.w; // base_vertex (absolute vertex offset in pool verts)
    indices[o + 4u] = t.tmeta.y; // first_instance (the body's instance slot; 0 on fallback devices)
}
