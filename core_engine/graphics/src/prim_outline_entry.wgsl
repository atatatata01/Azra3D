// ============================================================
// AZRA — Selected-Prim Outline Mask Entry Point
// Part of the split compute shader. Requires sdf_shared.wgsl + sdf_prim_eval.wgsl
// (assembled by build_prim_outline_wgsl()).
// ============================================================
//
// Marches ONLY the prims whose SELECTED bit is set (prim float[3] bit 8 — written by
// the serializer from the app's selection set) and writes a coverage MASK into
// `output_texture` (the shared binding 0, bound to gpu.overlay_texture here):
//   r = additive-prim coverage, g = subtractive/intersective (cutter) coverage,
//   a = max coverage.
// The MaskOutline blend (composite_blend.wgsl mode 4) then edge-detects the mask and
// composites a crisp contour (+ a pale fill for cutter volumes) over the HDR frame.
//
// WHY a dedicated march instead of the wireframe overlay: eval_prim here is the SAME
// function the bake and the analytical march evaluate, so the silhouette is exact for
// every shape, modifier, bevel profile and elongation — the CPU wireframe could never
// track those (the owner's elongated-sphere report).
//
// Cost: full-res sphere trace over K selected prims (K is the selection size, usually
// 1-3) — one eval_prim per selected prim per step, 96 steps max. Dispatched when
// Viewport Overlays → Selection Outline is on and the GPU sel set is non-empty
// (direct prims and/or prims under selected body nodes; Hybrid + Path Trace).

// LAB poly→SDF exact-silhouette ghost. The selected MeshVolume prims' grids are packed into
// `mesh_grid` and indexed per prim SLOT by `mesh_start` (serialize_from_graph, bound only for this
// pass), so the outline marches the SAME grid the bake samples → the ghost is the precise
// converted-mesh silhouette, not a box. Bindings 5/6 are free in the shared SDF header (declares
// 0-4, 9+); referenced only here, so the pipeline's auto bind-group layout picks them up.
@group(0) @binding(5) var<storage, read> mesh_grid: array<f32>;
@group(0) @binding(6) var<storage, read> mesh_start: array<u32>;
// CurveExtrude (Polyline) outline: real profile records (same layout as bake clip_curves).
// Binding 7/8 — not tex_data (binding 4). Fail closed if missing (clip_start = 0xffffffff).
@group(0) @binding(7) var<storage, read> outline_clip_curves: array<f32>;
@group(0) @binding(8) var<storage, read> outline_clip_start: array<u32>;

fn outline_mesh_at(base: u32, x: u32, y: u32, z: u32, res: u32) -> f32 {
    return mesh_grid[base + 8u + x + y * res + z * res * res];
}
fn outline_sample_grid(base: u32, mp: vec3<f32>) -> f32 {
    if (base + 9u > arrayLength(&mesh_grid)) { return 1e6; }
    let res = u32(mesh_grid[base]);
    if (res < 2u) { return 1e6; }
    let cell = mesh_grid[base + 4u];
    if (cell <= 0.0) { return 1e6; }
    let gmin = vec3(mesh_grid[base + 1u], mesh_grid[base + 2u], mesh_grid[base + 3u]);
    let hi = f32(res - 1u);
    let g = (mp - gmin) / cell;
    let g_cl = clamp(g, vec3(0.0), vec3(hi));
    let d_out = length(g - g_cl) * cell;
    let g0 = min(vec3<u32>(g_cl), vec3(res - 2u));
    let f = clamp(g_cl - vec3<f32>(g0), vec3(0.0), vec3(1.0));
    let c000 = outline_mesh_at(base, g0.x,      g0.y,      g0.z,      res);
    let c100 = outline_mesh_at(base, g0.x + 1u, g0.y,      g0.z,      res);
    let c010 = outline_mesh_at(base, g0.x,      g0.y + 1u, g0.z,      res);
    let c110 = outline_mesh_at(base, g0.x + 1u, g0.y + 1u, g0.z,      res);
    let c001 = outline_mesh_at(base, g0.x,      g0.y,      g0.z + 1u, res);
    let c101 = outline_mesh_at(base, g0.x + 1u, g0.y,      g0.z + 1u, res);
    let c011 = outline_mesh_at(base, g0.x,      g0.y + 1u, g0.z + 1u, res);
    let c111 = outline_mesh_at(base, g0.x + 1u, g0.y + 1u, g0.z + 1u, res);
    let c00 = mix(c000, c100, f.x);
    let c10 = mix(c010, c110, f.x);
    let c01 = mix(c001, c101, f.x);
    let c11 = mix(c011, c111, f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z) + d_out;
}
// Exact MeshVolume distance for the ghost (rigid+scale+mirror, matches eval_mesh_volume in the
// bake). Falls back to the analytical box (eval_prim) when this slot has no packed grid.
fn eval_mesh_volume_outline(idx: u32, p: vec3<f32>) -> f32 {
    let base = mesh_start[idx];
    if (base == 0xffffffffu) { return eval_prim(idx, p); }
    let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
    let sc = prim_scale(idx);
    var sp = local_p / sc;
    let mb = prim_mirror_bits(idx);
    let mc = prim_mirror_center(idx);
    if ((mb & 1u) != 0u) { sp.x = abs(sp.x - mc.x) + mc.x; }
    if ((mb & 2u) != 0u) { sp.y = abs(sp.y - mc.y) + mc.y; }
    if ((mb & 4u) != 0u) { sp.z = abs(sp.z - mc.z) + mc.z; }
    return outline_sample_grid(base, sp) * min(sc.x, min(sc.y, sc.z));
}

// Closed 2D polygon SDF (winding) — same math as volume_bake.wgsl / bake path.
fn outline_sdf_polygon_2d(p: vec2<f32>, verts_start: u32, vert_count: u32) -> f32 {
    if (vert_count < 3u) { return 999.0; }
    var d_sq = 1e20;
    var winding: i32 = 0;
    for (var i = 0u; i < vert_count; i++) {
        let j = (i + 1u) % vert_count;
        let vi = vec2(outline_clip_curves[verts_start + i * 2u], outline_clip_curves[verts_start + i * 2u + 1u]);
        let vj = vec2(outline_clip_curves[verts_start + j * 2u], outline_clip_curves[verts_start + j * 2u + 1u]);
        let e = vj - vi;
        let w = p - vi;
        let t = clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        let closest = vi + e * t;
        d_sq = min(d_sq, dot(p - closest, p - closest));
        if (vi.y <= p.y) {
            if (vj.y > p.y && (e.x * w.y - e.y * w.x) > 0.0) { winding += 1; }
        } else {
            if (vj.y <= p.y && (e.x * w.y - e.y * w.x) < 0.0) { winding -= 1; }
        }
    }
    let sign_val = select(1.0, -1.0, winding != 0);
    return sqrt(d_sq) * sign_val;
}
// Open polyline distance — same as volume_bake dist_to_open_polyline_gpu.
fn outline_dist_open_polyline(p: vec2<f32>, verts_start: u32, vert_count: u32) -> f32 {
    if (vert_count < 2u) { return 1e9; }
    var d = 1e9;
    for (var i = 0u; i < vert_count - 1u; i++) {
        let a = vec2(outline_clip_curves[verts_start + i * 2u], outline_clip_curves[verts_start + i * 2u + 1u]);
        let b = vec2(outline_clip_curves[verts_start + (i + 1u) * 2u], outline_clip_curves[verts_start + (i + 1u) * 2u + 1u]);
        let e = b - a;
        let w = p - a;
        let t = clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        d = min(d, length(p - (a + e * t)));
    }
    return d;
}

// REAL CurveExtrude (Polyline) SDF — same as volume_bake st==9. No sphere stand-in.
fn eval_curve_extrude_outline(idx: u32, p: vec3<f32>) -> f32 {
    let base = outline_clip_start[idx];
    if (base == 0xffffffffu) { return 1e6; } // fail closed (outside), never d=0 flood
    let local_p = quat_inv_rotate(prim_quat(idx), p - prim_pos(idx));
    let sc = prim_scale(idx);
    var sp = local_p / sc;
    let mb = prim_mirror_bits(idx);
    let mc = prim_mirror_center(idx);
    if ((mb & 1u) != 0u) { sp.x = abs(sp.x - mc.x) + mc.x; }
    if ((mb & 2u) != 0u) { sp.y = abs(sp.y - mc.y) + mc.y; }
    if ((mb & 4u) != 0u) { sp.z = abs(sp.z - mc.z) + mc.z; }
    let elong = prim_elongation(idx);
    if (elong.x > 0.0) { sp.x = sp.x - clamp(sp.x, -elong.x, elong.x); }
    if (elong.y > 0.0) { sp.y = sp.y - clamp(sp.y, -elong.y, elong.y); }
    if (elong.z > 0.0) { sp.z = sp.z - clamp(sp.z, -elong.z, elong.z); }
    let eval_p = sp;
    let min_s = min(sc.x, min(sc.y, sc.z));
    let p0 = prim_f(idx, 12u); // half_depth
    let p1 = prim_f(idx, 13u); // wall_t
    let p2 = prim_f(idx, 14u); // rim
    let mode_slot = outline_clip_curves[base];
    let vc = bitcast<u32>(outline_clip_curves[base + 1u]);
    let cu = vec3(outline_clip_curves[base + 2u], outline_clip_curves[base + 3u], outline_clip_curves[base + 4u]);
    let cv = vec3(outline_clip_curves[base + 5u], outline_clip_curves[base + 6u], outline_clip_curves[base + 7u]);
    let verts_start = base + 8u;
    let uv = vec2(dot(eval_p, cu), dot(eval_p, cv));
    var d = 1e6;
    if (mode_slot != 0.0 && vc >= 2u) {
        let thickness = abs(mode_slot);
        let d_path = outline_dist_open_polyline(uv, verts_start, vc);
        if (mode_slot < 0.0) {
            d = length(vec2(d_path, eval_p.y)) - thickness * 0.5;
        } else {
            let d_band = d_path - thickness * 0.5;
            if (p0 > 0.001) {
                let cap = abs(eval_p.y) - p0;
                let rim_prof = select(0.0, 1.0, p2 > 0.0);
                d = bevel_2d_profiled(d_band, cap, abs(p2), rim_prof, 45.0);
            } else {
                d = d_band;
            }
        }
    } else if (vc >= 3u) {
        d = outline_sdf_polygon_2d(uv, verts_start, vc);
        if (p0 > 0.001) {
            let cap = abs(eval_p.y) - p0;
            let rim_prof = select(0.0, 1.0, p2 > 0.0);
            d = bevel_2d_profiled(d, cap, abs(p2), rim_prof, 45.0);
        }
        if (p1 > 0.0001) { d = max(d, -d - p1); }
    }
    return d * min_s;
}

@compute @workgroup_size(8, 8, 1)
fn prim_outline(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_texture);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let w = f32(dims.x);
    let h = f32(dims.y);
    let uv_x = ((f32(gid.x) + 0.5) / w) * 2.0 - 1.0;
    let uv_y = -(((f32(gid.y) + 0.5) / h) * 2.0 - 1.0);
    let ray = camera_ray(uv_x, uv_y, w / h);
    let ro = ray[0];
    let rd = ray[1];

    // Union sphere-trace over the SELECTED prims only. Every pixel writes (full clear
    // each dispatch — no stale mask residue).
    var t = 0.0;
    var hit_add = 0.0;
    var hit_sub = 0.0;
    for (var s = 0u; s < 96u; s++) {
        let p = ro + rd * t;
        var d = 1e9;
        var nearest_cutter = 0.0;
        for (var i = 0u; i < config.num_primitives; i++) {
            let st = prim_u(i, 3u);
            if ((st & 0xffu) >= 254u) { continue; }    // SCOPE_OPEN/CLOSE markers
            if (((st >> 8u) & 1u) == 0u) { continue; } // not selected
            // MeshVolume (st==10): exact packed grid. CurveExtrude (st==9): real profile SDF
            // from outline clip buffer (same as bake). Everything else: eval_prim.
            var di = eval_prim(i, p);
            if ((st & 0xffu) == 10u) { di = eval_mesh_volume_outline(i, p); }
            else if ((st & 0xffu) == 9u) { di = eval_curve_extrude_outline(i, p); }
            if (di < d) {
                d = di;
                // op_type [11]: 0=Union, 1=Subtract, 2=Intersect(+Smooth), 3=ChamferUnion,
                // 4=ChamferSubtract. Cutters (1/2/4) get the pale-volume treatment — we march
                // the cutter's OWN shape volume (eval_prim is the un-negated shape field),
                // which is exactly what the user wants to see through the body.
                let op = prim_op_type(i);
                nearest_cutter = select(0.0, 1.0, op == 1u || op == 2u || op == 4u);
            }
        }
        if (d < config.hit_threshold * 4.0) {
            hit_add = 1.0 - nearest_cutter;
            hit_sub = nearest_cutter;
            break;
        }
        t = t + max(d, config.hit_threshold);
        if (t > config.far_clip) { break; }
    }
    textureStore(output_texture, vec2<i32>(gid.xy),
        vec4<f32>(hit_add, hit_sub, 0.0, max(hit_add, hit_sub)));
}
