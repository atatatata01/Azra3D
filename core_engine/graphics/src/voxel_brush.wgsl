// GPU brush — applies ONE stamp to the density grid: the GPU port of `VoxelGrid::apply_stamp_plan`
// (voxel_sculpt.rs). One thread per voxel in the brush AABB.
//
// PHASE 0 (BRUSH_SYSTEM_ARCHITECTURE.md §4.2): the kernel now consumes the compiled `StampPlan` POD —
// it reads the plan's `op_list[0..op_count]` and LOOPS the operators, dispatching each through ONE
// 8-arm jump table (`apply_operator`) that mirrors the CPU `apply_stamp_plan` op dispatch EXACTLY (one
// opcode table, two languages; per-voxel-outer, per-op-inner). For Phase 0 `op_count` is ALWAYS 1, so
// the result is byte-identical to the legacy single-op switch — but the MECHANISM is the bounded
// op_count loop, not the old single `aabb_max.w` read.
//
// TDR-SAFETY (the whole point of the uber-kernel): the loop is `for i in 0u..op_count` with `op_count`
// a UNIFORM value bounded ≤ MAX_OPS (4). It is dynamic OVER DATA, so Naga does NOT unroll/inline it into
// the exponential code chain that caused the noisy-blend TDR. `apply_operator` is one function with an
// 8-arm `switch` (a jump table), not a recursive call chain. A 1-op Draw and a 4-op brush produce the
// SAME SPIR-V — only the uniform `StampPlan` differs (zero recompiles per brush).
//
// Independent ops (Add/Subtract/PlanePush/PlaneFill/PlaneClip) read & write the live per-voxel value
// `d` — each thread is independent, no race. Neighbourhood ops (Relax/Inflate/Advect) read a pre-stamp
// `snapshot` of the AABB instead of the live grid (parallel threads must not read each other's in-flight
// writes — the CPU used a snapshot for the same reason / determinism).
//
// `density` is the GLOBAL grid (res³), indexed by global cell coord. `snapshot` is the pre-stamp
// (pre-FRAME) state of the edited Z-slices [snap.x .. snap.y], FULL XY — same XY layout as density.
// Neighbourhood reads index it globally in XY, slice-relative in Z (`snap_g`), so multiple stamps in
// a frame all read ONE consistent pre-frame snapshot (deterministic, race-free).

struct BrushParams {
    aabb_min: vec4<u32>, // xyz = per-stamp effect AABB min cell, w = res
    aabb_max: vec4<u32>, // xyz = per-stamp effect AABB max cell (inclusive), w = UNUSED (legacy op code; the op now rides op_list)
    center:   vec4<f32>, // xyz = brush center (world), w = radius
    normal:   vec4<f32>, // xyz = surface normal (Direction resolved CPU-side), w = per-stamp strength (incl. pressure)
    misc:     vec4<f32>, // x = iso, y = bounds, z = voxel_size, w = sgn (invert: -1 / +1)
    box_min:  vec4<u32>, // xyz = BOX origin (= the uploaded density/snapshot buffers' min cell), w = falloff hardness BITS (f32)
    box_dim:  vec4<u32>, // xyz = BOX dims (the buffers are box-LOCAL, not res³ — Phase 4 bound), w = 0
    // --- StampPlan op-driving tail (rides the SAME UBO — no new binding, §6 of the architecture) ---
    kern:     vec4<u32>, // x = kernel_code, y = kernel_exp BITS (f32), z = chan_mask (unused on GPU: COLOR is dense-only), w = op_count (1..=MAX_OPS)
    op0:      vec4<u32>, // PackedOp[0]: x = op_code, y = flags, z = amount BITS (f32), w = plane_offset BITS (f32)
    op1:      vec4<u32>, // PackedOp[1]
    op2:      vec4<u32>, // PackedOp[2]
    op3:      vec4<u32>, // PackedOp[3]
    // Tip alpha (GPU path — 3D-Coat-style always-on alpha without CPU force):
    tip_t:    vec4<f32>, // xyz = stamp-plane tangent, w = contrast (0 = no tip / identity)
    tip_b:    vec4<f32>, // xyz = stamp-plane bitangent, w = tip resolution (texels/side, e.g. 64)
    stroke_tan: vec4<f32>, // xyz = UNIT stroke tangent for the stroke-axis Pinch (0 = axisymmetric fallback), w unused
};

// `density` + `snapshot` are BOX-LOCAL dense buffers (the brush AABB box, not res³): index
// (x-box_min.x) + (y-box_min.y)·box_dim.x + (z-box_min.z)·box_dim.x·box_dim.y. The dispatch only
// touches the per-stamp effect AABB ⊆ the box; neighbourhood reads clamp to the box (matching the
// CPU's AABB-snapshot clamp). This is what lets the brush scale to 1024³ (buffer ∝ brush, not volume).
@group(0) @binding(0) var<storage, read_write> density:  array<f32>;
@group(0) @binding(1) var<uniform>             params:   BrushParams;
@group(0) @binding(2) var<storage, read>       snapshot: array<f32>; // pre-stamp BOX (box-local layout)
// Protect-mask factors 0..1 (box-local, same layout as density). 0 = free, 1 = fully protected.
// When a stamp has no mask, aabb_max.w=0 and binding 3 is a persistent one-float zero dummy.
@group(0) @binding(3) var<storage, read>       mask:     array<f32>;
// Brush tip alpha disc (row-major, tip_b.w² floats, UV in [-1,1]). All 1.0 = identity (no tip).
@group(0) @binding(4) var<storage, read>       tip_alpha: array<f32>;

// Op codes — the FROZEN CPU↔GPU `op_code` contract (core_math::brush::Operator, APPEND-ONLY).
const OP_ADD: u32 = 0u;        // Draw
const OP_SUBTRACT: u32 = 1u;   // Erase / Crease (Crease = Subtract + Kernel::Power{3})
const OP_RELAX: u32 = 2u;      // Smooth
const OP_PLANEPUSH: u32 = 3u;  // Flatten
const OP_PLANEFILL: u32 = 4u;  // Clay / Fill / ClayBuildup
const OP_PLANECLIP: u32 = 5u;  // Scrape
const OP_INFLATE: u32 = 6u;    // Inflate
const OP_ADVECT: u32 = 7u;     // Pinch (tangential) — Move (DragVector) is CPU-only, never dispatched here

// PackedOp.flags bit layout — mirror of core_math::brush::plan::flags (the shared contract).
const SIDE_SHIFT: u32 = 0u;    const SIDE_MASK: u32 = 3u;   // PlaneSide: 0=Both, 1=Above, 2=Below
const BLEND_SHIFT: u32 = 2u;   const BLEND_MASK: u32 = 3u;  // OpBlend (Phase 0 single-op → always Accumulate)
const ADVECT_SHIFT: u32 = 4u;  const ADVECT_MASK: u32 = 1u; // AdvectSrc: 0=Tangential (Pinch), 1=DragVector (Move, CPU-only)
const SHAVE_BIT: u32 = 32u;       // 1 << 5  — PlaneFill shaves above the plane (Clay = true)
const SGN_ON_SIDE_BIT: u32 = 64u; // 1 << 6  — PlaneFill legacy-Fill sign mode (sign flips WHICH side fills)
const CONVERGE_BIT: u32 = 128u;   // 1 << 7  — PlaneFill converging fill (ClayBuildup, Clay-1)

fn res() -> u32 { return params.aabb_min.w; }
fn iso() -> f32 { return params.misc.x; }

// UI Height Limit → absolute density ceiling. 1 / unset = no cap (return 2 so min is a no-op).
// Floor just above iso so h=0.5 cannot collapse the surface. Callers must only limit GROWTH
// (never min solid density down to the ceiling — that erased clay).
fn density_cap_from_hlim(h_ui: f32) -> f32 {
    if (h_ui < 1e-4 || h_ui >= 0.999) { return 2.0; }
    let iso_v = clamp(iso(), 0.0, 0.95);
    return clamp(max(clamp(h_ui, 0.0, 1.0), iso_v + 0.02), 0.0, 1.0);
}

// Limit upward deposit only; never dig existing solid down to the ceiling.
fn apply_density_cap(cur: f32, nxt: f32, dcap: f32) -> f32 {
    if (dcap > 1.5) { return nxt; } // off
    if (nxt > cur) {
        if (cur >= dcap) { return cur; }
        return min(nxt, dcap);
    }
    return nxt;
}
fn bounds() -> f32 { return params.misc.y; }
fn voxel_size() -> f32 { return params.misc.z; }
fn sgn() -> f32 { return params.misc.w; }
fn radius() -> f32 { return params.center.w; }
fn strength() -> f32 { return params.normal.w; }

fn kernel_code() -> u32 { return params.kern.x; }
fn kernel_exp() -> f32 { return bitcast<f32>(params.kern.y); }
fn op_count() -> u32 { return params.kern.w; }

// One PackedOp by index (the op-list rides the UBO as 4 fixed vec4 slots; index is bounded < op_count ≤ 4).
fn packed_op(i: u32) -> vec4<u32> {
    switch i {
        case 0u: { return params.op0; }
        case 1u: { return params.op1; }
        case 2u: { return params.op2; }
        default: { return params.op3; }
    }
}

fn smoothstep01(x: f32) -> f32 { let t = clamp(x, 0.0, 1.0); return t * t * (3.0 - 2.0 * t); }

// Continuous falloff HARDNESS (0..1) lives in box_min.w as f32 BITS — bitcast it back.
fn falloff_hardness() -> f32 { return bitcast<f32>(params.box_min.w); }
// Falloff preset id (FalloffPreset as u32) rides box_dim.w — 0=Smooth … 8=Blend (F1b).
fn falloff_preset() -> u32 { return params.box_dim.w; }
// aabb_max.w is free after StampPlan moved the op code to op0.x; use it to avoid indexing the
// one-float dummy mask bound by unmasked box/atlas strokes.
fn mask_enabled() -> bool { return params.aabb_max.w != 0u; }

// Blender-style hardness remap: full strength while dist/radius < hardness, curve on outer shell.
// MUST match CPU `hardness_remap_e` (voxel_sculpt.rs). e = 1 centre, 0 rim.
fn hardness_remap_e(e: f32) -> f32 {
    let x = clamp(e, 0.0, 1.0);
    let h = clamp(falloff_hardness(), 0.0, 1.0);
    if (h <= 1e-6) { return x; }
    if (h >= 0.999) { return select(0.0, 1.0, x > 1e-6); }
    let core = 1.0 - h;
    if (x >= core) { return 1.0; }
    return clamp(x / core, 0.0, 1.0);
}

// Radial brush weight at "inside-ness" `e = 1 - t` (1 = centre, 0 = rim). MUST match the CPU
// `falloff_weight_preset` (voxel_sculpt.rs) OP-FOR-OP. Hardness applies to EVERY preset (Blender).
fn falloff_weight(e: f32) -> f32 {
    let x = hardness_remap_e(e);
    let preset = falloff_preset();
    // 0 Smooth, 1 Smoother, 2 Sphere, 3 Root, 4 Sharp, 5 Linear, 6 InvSquare, 7 Constant, 8 Blend
    if (preset == 1u) {
        let x2 = x * x;
        let x3 = x2 * x;
        return x3 * (10.0 + x * (-15.0 + 6.0 * x));
    }
    if (preset == 2u) {
        let d = 1.0 - x;
        return sqrt(max(1.0 - d * d, 0.0));
    }
    if (preset == 3u) { return sqrt(x); }
    if (preset == 4u) { return x * x * x; }
    if (preset == 5u) { return x; }
    if (preset == 6u) { return x * x; }
    if (preset == 7u) { return select(0.0, 1.0, x > 0.0); }
    if (preset == 8u) {
        // Blend curve shape after hardness plateau (extra sharp rim).
        let soft = x * x * (3.0 - 2.0 * x);
        let sharp = pow(x, 8.0);
        return soft * 0.35 + sharp * 0.65;
    }
    // 0 Smooth (default)
    return x * x * (3.0 - 2.0 * x);
}

// Kernel-powered weight `fall^exp · strength`, BIT-IDENTICAL to the CPU `apply_stamp_plan`: the integer
// exponents are LITERAL repeated multiplies (`fall*fall` / `fall*fall*fall`), NOT `pow` — `pow` rounds
// differently (exp(n·ln fall)) and drifts ~1 ULP. exp==1 → `fall·strength`. Phase-0 built-ins only use
// exp ∈ {1,2,3}; a future non-integer exp falls back to `pow` (no Phase-0 built-in reaches it).
fn kernel_weight(fall: f32, exp: f32) -> f32 {
    if (exp == 2.0) { return (fall * fall) * strength(); }
    if (exp == 3.0) { return (fall * fall * fall) * strength(); }
    if (exp == 1.0) { return fall * strength(); }
    return pow(fall, exp) * strength();
}

fn cell_world(c: vec3<u32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return (vec3<f32>(c) / s) * 2.0 * bounds() - vec3<f32>(bounds());
}

fn box_min() -> vec3<u32> { return params.box_min.xyz; }
fn box_dim() -> vec3<u32> { return params.box_dim.xyz; }

// Flat BOX-LOCAL index of a GLOBAL cell (caller guarantees it is inside the box).
fn lin_box(c: vec3<u32>) -> u32 {
    let l = c - box_min();
    let d = box_dim();
    return l.x + l.y * d.x + l.z * d.x * d.y;
}

// World position → fractional GLOBAL grid coordinate.
fn world_to_grid(p: vec3<f32>) -> vec3<f32> {
    let s = f32(res() - 1u);
    return ((p + vec3<f32>(bounds())) / (2.0 * bounds())) * s;
}

// Read the pre-stamp SNAPSHOT at a GLOBAL cell, CLAMPED to the box bounds. The snapshot is the
// box-local pre-stamp buffer; clamping at the box edge matches the CPU's AABB-snapshot clamp
// (`sub_at`), so the Relax/Inflate stencil + the Pinch gather agree with the CPU at the edge.
fn snap_g(gx: i32, gy: i32, gz: i32) -> f32 {
    let bmin = vec3<i32>(box_min());
    let bmax = bmin + vec3<i32>(box_dim()) - vec3<i32>(1);
    let c = clamp(vec3<i32>(gx, gy, gz), bmin, bmax);
    return snapshot[lin_box(vec3<u32>(c))];
}

// Trilinear sample of the snapshot at fractional GLOBAL grid coords (Pinch gather).
fn snap_sample_g(g: vec3<f32>) -> f32 {
    let b = vec3<i32>(floor(g));
    let f = g - floor(g);
    let c00 = mix(snap_g(b.x,b.y,b.z),     snap_g(b.x+1,b.y,b.z),     f.x);
    let c10 = mix(snap_g(b.x,b.y+1,b.z),   snap_g(b.x+1,b.y+1,b.z),   f.x);
    let c01 = mix(snap_g(b.x,b.y,b.z+1),   snap_g(b.x+1,b.y,b.z+1),   f.x);
    let c11 = mix(snap_g(b.x,b.y+1,b.z+1), snap_g(b.x+1,b.y+1,b.z+1), f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}

// Apply ONE operator to the current per-voxel density `cur` and return the new density (UNCLAMPED — the
// caller clamps once after the op loop, matching the CPU's per-write `clamp`). This is the 8-arm jump
// table that mirrors `VoxelGrid::apply_stamp_plan`'s `match op` EXACTLY (op-parity is structural).
//
//   pop   — the PackedOp (x=op_code, y=flags, z=amount BITS, w=plane_offset BITS)
//   cur   — this voxel's current density
//   p     — this voxel's world position;  center/nrm — brush plane;  s — sgn (+1 / -1)
//   fall  — base radial falloff (1 centre → 0 rim);  w = fall·strength (base weight / gate);
//   wk    — kernel-powered weight (fall^exp·strength) — Add/Subtract/PlaneFill use this so Crease &
//           ClayBuildup reproduce the cubed/squared falloff; every exp==1 op has wk==w.
//   g     — this voxel's global cell coords (snap_g clamps them to the box)
fn apply_operator(pop: vec4<u32>, cur: f32, p: vec3<f32>, center: vec3<f32>, nrm: vec3<f32>,
                  s: f32, fall: f32, w: f32, wk: f32, g: vec3<i32>) -> f32 {
    let op = pop.x;
    let flags = pop.y;
    let plane_offset = bitcast<f32>(pop.w);
    let side = (flags >> SIDE_SHIFT) & SIDE_MASK;
    let shave = (flags & SHAVE_BIT) != 0u;
    let sgn_on_side = (flags & SGN_ON_SIDE_BIT) != 0u;
    let converge = (flags & CONVERGE_BIT) != 0u;

    var out = cur;
    switch op {
        case 0u: {
            // Draw (Add op): soft dual approach solid/empty so Add and Remove feel the same.
            // Accu OFF (blend==1 Max): rate = wk. Accu ON: stronger rate 1.35× (still dual).
            // Pure `cur+s*wk` made Add balloon while Remove dug cleanly.
            let blend_add = (flags >> BLEND_SHIFT) & BLEND_MASK;
            let solid_tgt = select(0.0, 1.0, s >= 0.0);
            let rate = select(1.35, 1.0, blend_add == 1u);
            let t = clamp(wk * rate, 0.0, 1.0);
            out = cur + (solid_tgt - cur) * t;
        }
        case 1u: {                         // Subtract: Crease Depth/Width (Erase defaults 1).
            var depth = bitcast<f32>(pop.z);
            if (depth < 1e-6) { depth = 1.0; }
            var width = plane_offset;
            if (width < 1e-6) { width = 1.0; }
            width = clamp(width, 0.1, 4.0);
            var fall_c = fall;
            if (abs(width - 1.0) >= 1e-4) {
                fall_c = pow(fall, 1.0 / width);
            }
            let expv = kernel_exp();
            var wk_c = fall_c * strength() * depth;
            if (abs(expv - 3.0) < 1e-4) {
                wk_c = fall_c * fall_c * fall_c * strength() * depth;
            } else if (abs(expv - 2.0) < 1e-4) {
                wk_c = fall_c * fall_c * strength() * depth;
            }
            out = cur - s * wk_c;
        }
        case 2u: {                         // Relax (Smooth): toward the 6-neighbour pre-stamp average (base w).
            // sgn reverses Smooth (Ctrl / Remove polarity) → push away from average.
            let avg = (snap_g(g.x+1,g.y,g.z) + snap_g(g.x-1,g.y,g.z)
                     + snap_g(g.x,g.y+1,g.z) + snap_g(g.x,g.y-1,g.z)
                     + snap_g(g.x,g.y,g.z+1) + snap_g(g.x,g.y,g.z-1)) / 6.0;
            out = cur + s * (avg - cur) * w;
        }
        case 3u: {                         // PlanePush (Flatten): Flatten Amount + plane offset/bias + vol preserve.
            let flat_amt = select(0.5, bitcast<f32>(pop.z), bitcast<f32>(pop.z) > 1e-6);
            let poff = radius() * plane_offset; // plane_offset already = offset + bias from fold
            let plane = dot(p - center, nrm) - poff;
            // Match Rust f32::signum: +1 for plane >= 0, -1 for plane < 0 (not WGSL sign(0)=0).
            let splane = select(-1.0, 1.0, plane >= 0.0);
            let vol_u = (flags >> 16u) & 0xFFu;
            let vol_pres = f32(vol_u) / 255.0;
            let solid = clamp(abs(cur - 0.5) * 2.0, 0.0, 1.0);
            let vp = 1.0 - vol_pres * solid;
            // CPU: fall * flat_amt * strength * vp (not w*flat_amt which double-counts less clearly)
            let push = fall * flat_amt * strength() * vp;
            var nxt = cur - s * splane * push;
            // Height Clamp: absolute density ceiling; growth-only (never dig solid to ceiling).
            let hlim_u = (flags >> 8u) & 0xFFu;
            var h_ui = f32(hlim_u) / 255.0;
            if (h_ui < 1e-4) { h_ui = 1.0; } // unset → off
            let dcap = density_cap_from_hlim(h_ui);
            out = apply_density_cap(cur, nxt, dcap);
        }
        case 4u: {                         // PlaneFill (Clay / Fill / ClayBuildup) — semantics by flag.
            let blend = (flags >> 2u) & 0x3u; // 0=Accumulate, 1=Max
            let hlim_u = (flags >> 8u) & 0xFFu;
            // 0 = unset (legacy) → off (no cap)
            var h_ui = f32(hlim_u) / 255.0;
            if (h_ui < 1e-4) { h_ui = 1.0; }
            let dcap = density_cap_from_hlim(h_ui);
            if (sgn_on_side) {
                // FILL: side test includes Plane Offset (CPU match). Height Limit caps density growth.
                let off = radius() * plane_offset;
                let sd = (dot(p - center, nrm) - off) * s;
                if (sd < 0.0) {
                    var nxt = cur + wk;
                    nxt = select(nxt, max(cur, nxt), blend == 1u);
                    out = apply_density_cap(cur, nxt, dcap);
                }
            } else if (converge) {
                // Build: plane_offset = layer; amount = build_rate; height_limit caps target.
                // Rate = 1-(1-wk)^rate_mul so Build Rate >1 still changes the core (not clamp-dead).
                let off = radius() * plane_offset;
                let plane = dot(p - center, nrm) - off;
                let band = voxel_size() * 1.5;
                var tgt = clamp(0.5 - plane / band, 0.0, 1.0);
                if (dcap <= 1.5) {
                    tgt = min(tgt, dcap);
                    tgt = max(tgt, iso() + 1e-4);
                }
                let d0 = tgt - cur;
                var d = max(d0, 0.0);
                if (s < 0.0) { d = min(d0, 0.0); }
                var rate_mul = bitcast<f32>(pop.z);
                if (rate_mul < 1e-6) { rate_mul = 2.0; }
                rate_mul = clamp(rate_mul, 0.15, 8.0);
                let t = clamp(wk, 0.0, 1.0);
                let rate = 1.0 - pow(1.0 - t, rate_mul);
                // Clay Strips slab normal-band (BLENDER_BRUSH_SPEC §3a, mirror of CPU converge arm):
                // z = depth surface→plane, z→1 at the CONTACT face (cursor), z→0 at the proud plane.
                // smoothstep plateau ~1 across z∈[0.6,1] tapering to 0 by the proud plane — strong at the
                // cursor (clay_strips_ref.json flat-topped slab). A pure z·(1−z) parabola was 0 at the
                // cursor (z clamps to 1) → deposited nothing where you sculpt (regression bc9ff30).
                var z_fac = 1.0;
                if (off > 1e-6) {
                    let z = clamp((off - plane) / off, 0.0, 1.0);
                    let sz = clamp(z / 0.6, 0.0, 1.0);
                    z_fac = sz * sz * (3.0 - 2.0 * sz);
                }
                var nxt = cur + d * rate * z_fac;
                nxt = select(nxt, max(cur, nxt), blend == 1u);
                out = apply_density_cap(cur, nxt, dcap);
            } else {
                // CLAY: plane offset + optional shave; Height Limit caps growth; Accumulate vs Max.
                let off = radius() * plane_offset;
                let plane = dot(p - center, nrm) - off;
                if (plane <= 0.0) {
                    var nxt = cur + s * wk;
                    nxt = select(nxt, max(cur, nxt), blend == 1u);
                    out = apply_density_cap(cur, nxt, dcap);
                } else if (shave) {
                    out = cur + s * (-wk * 0.5);
                }
            }
        }
        case 5u: {                         // PlaneClip (Trim/Scrape): angle, cut depth, fill holes.
            var cut_depth = bitcast<f32>(pop.z);
            if (cut_depth < 1e-6) { cut_depth = 1.0; }
            let angle = plane_offset; // plane angle ×radius + bias
            let sd = dot(p - center, nrm) * s - radius() * angle;
            var cut = true;
            if (side == 1u) { cut = sd > 0.0; }
            else if (side == 2u) { cut = sd < 0.0; }
            let fill_holes = converge;
            if (cut) {
                out = cur - w * cut_depth;
            } else if (fill_holes) {
                out = cur + w * cut_depth * 0.35;
            }
        }
        case 6u: {                         // Inflate: speed, direction, vol preserve, thickness clamp.
            let dx = snap_g(g.x+1,g.y,g.z) - snap_g(g.x-1,g.y,g.z);
            let dy = snap_g(g.x,g.y+1,g.z) - snap_g(g.x,g.y-1,g.z);
            let dz = snap_g(g.x,g.y,g.z+1) - snap_g(g.x,g.y,g.z-1);
            let gmag = min(sqrt(dx*dx + dy*dy + dz*dz), 1.0);
            var speed = bitcast<f32>(pop.z);
            if (speed < 1e-6) { speed = 1.0; }
            let dir_mode = (flags >> 24u) & 0x3u;
            var factor = gmag;
            if (dir_mode == 1u) {
                let glen = max(sqrt(dx*dx + dy*dy + dz*dz), 1e-6);
                let align = abs(dx * nrm.x + dy * nrm.y + dz * nrm.z) / glen;
                factor = gmag * (0.35 + 0.65 * align);
            } else if (dir_mode == 2u) {
                factor = gmag * (0.25 + 0.75 * fall);
            }
            let vol_u = (flags >> 16u) & 0xFFu;
            let vol_pres = f32(vol_u) / 255.0;
            let solid = clamp(abs(cur - 0.5) * 2.0, 0.0, 1.0);
            let vp = 1.0 - vol_pres * solid;
            // CPU: fall * strength * speed * factor * vp
            var nxt = cur + s * fall * strength() * speed * factor * vp;
            var tclamp = plane_offset;
            if (tclamp < 1e-6) { tclamp = 1.0; }
            tclamp = clamp(tclamp, 0.05, 1.0);
            if (tclamp < 0.999) {
                let lo = max(0.5 - 0.5 * tclamp, 0.0);
                let hi = min(0.5 + 0.5 * tclamp, 1.0);
                nxt = clamp(nxt, lo, hi);
            }
            out = nxt;
        }
        default: {                         // Advect Pinch: Strength=reach, Width=footprint, Amount=blend.
            // CPU match: width reshapes fall via pow(fall, 1/width); reach scales sample distance;
            // brush strength × pinch amount scale blend only.
            // STROKE-AXIS PINCH (BLENDER_BRUSH_SPEC §2, mirror of CPU apply_stamp_plan): pull toward the
            // stroke AXIS LINE, not the dab point. stroke_tan.xyz = unit stroke tangent; the gather uses
            // the BITANGENT (tangent × normal) so only the cross-stroke component squeezes. Zero tangent
            // (first frame / no motion) → axisymmetric radial gather (identical to the CPU fallback).
            let rel = p - center;
            let tang_full = rel - nrm * dot(rel, nrm);
            let strk = params.stroke_tan.xyz;
            var tang = tang_full;
            if (dot(strk, strk) > 1e-10) {
                let bitan = normalize(cross(strk, nrm));
                if (dot(bitan, bitan) > 1e-10) {
                    tang = bitan * dot(tang_full, bitan);
                }
            }
            let tlen = length(tang);
            if (tlen >= 1e-6) {
                var reach = max(bitcast<f32>(pop.z), 0.0);
                if (reach < 1e-6) { reach = 1.0; }
                var width = plane_offset;
                if (width < 1e-6) { width = 1.0; }
                width = clamp(width, 0.15, 4.0);
                let hlim_u = (flags >> 8u) & 0xFFu;
                var pamt = f32(hlim_u) / 255.0 * 2.0;
                if (pamt < 1e-4) { pamt = 1.0; }
                var fall_w = fall;
                if (abs(width - 1.0) >= 1e-3) {
                    fall_w = pow(fall, clamp(1.0 / width, 0.2, 5.0));
                }
                let off = fall_w * radius() * 0.65 * reach * s;
                let gp = p + (tang / tlen) * off;
                let sampled = snap_sample_g(world_to_grid(gp));
                let blend = clamp(fall_w * strength() * pamt, 0.0, 1.0);
                out = cur + (sampled - cur) * blend;
            }
        }
    }
    return out;
}

// Bilinear tip sample in brush-disk UV ([-1,1]²). Matches CPU BrushTipAlpha::sample + contrast.
fn tip_sample(u: f32, v: f32, contrast: f32) -> f32 {
    let resf = max(params.tip_b.w, 1.0);
    let resi = u32(resf);
    let n = resi * resi;
    if (arrayLength(&tip_alpha) < n) { return 1.0; }
    let fx = clamp((u * 0.5 + 0.5) * resf - 0.5, 0.0, resf - 1.0);
    let fy = clamp((v * 0.5 + 0.5) * resf - 0.5, 0.0, resf - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, resi - 1u);
    let y1 = min(y0 + 1u, resi - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a00 = tip_alpha[y0 * resi + x0];
    let a10 = tip_alpha[y0 * resi + x1];
    let a01 = tip_alpha[y1 * resi + x0];
    let a11 = tip_alpha[y1 * resi + x1];
    let a0 = a00 + (a10 - a00) * tx;
    let a1 = a01 + (a11 - a01) * tx;
    var a = clamp(a0 + (a1 - a0) * ty, 0.0, 1.0);
    let c = clamp(contrast, 0.05, 4.0);
    if (abs(c - 1.0) >= 1e-4) {
        a = clamp((a - 0.5) * c + 0.5, 0.0, 1.0);
    }
    return a;
}

fn tip_factor(p: vec3<f32>, center: vec3<f32>) -> f32 {
    let contrast = params.tip_t.w;
    // contrast <= 0 → no tip installed (identity). Always-on alpha path when contrast > 0.
    if (contrast < 1e-4) { return 1.0; }
    let tip_t = params.tip_t.xyz;
    let tip_b = params.tip_b.xyz;
    let rel = p - center;
    let inv_r = 1.0 / max(radius(), 1e-6);
    let u = dot(rel, tip_t) * inv_r;
    let v = dot(rel, tip_b) * inv_r;
    return tip_sample(u, v, contrast);
}

@compute @workgroup_size(4, 4, 4)
fn apply_brush(@builtin(global_invocation_id) gid: vec3<u32>) {
    let c = params.aabb_min.xyz + gid;
    if (any(c > params.aabb_max.xyz)) { return; }

    let p = cell_world(c);
    let center = params.center.xyz;
    let dist = length(p - center);
    let t = dist / max(radius(), 1e-6);
    if (t >= 1.0) { return; }
    let i = lin_box(c); // box-local index into the density / mask buffers
    // Base radial falloff (1 at centre → 0 at rim), tip alpha (GPU, no CPU force), then protect-mask.
    var fall = falloff_weight(1.0 - t);
    fall = fall * tip_factor(p, center);
    if (mask_enabled()) {
        fall = fall * (1.0 - mask[i]);
    }
    let w = fall * strength();
    if (w < 1e-5) { return; }
    let wk = kernel_weight(fall, kernel_exp()); // fall^exp·strength (Crease/ClayBuildup); ==w for exp==1.

    let cur = density[i];
    let nrm = params.normal.xyz;
    let s = sgn();
    let g = vec3<i32>(c); // global cell coords (snap_g clamps them to the box)

    // BOUNDED, DYNAMIC-OVER-DATA op loop (op_count ≤ MAX_OPS = 4). Naga does NOT unroll it. Phase 0:
    // op_count == 1, so this runs the single op once — byte-identical to the legacy switch. The
    // per-op output feeds the next op's `cur` (per-voxel sequential ops compose; Phase-3 stacking).
    var d = cur;
    let n = min(op_count(), 4u);
    // Pure DEPOSIT stack (Add/Subtract/PlaneFill only) → eligible for the buildup cap below.
    var deposit_only = true;
    for (var oi: u32 = 0u; oi < n; oi = oi + 1u) {
        let code = packed_op(oi).x;
        if (!(code == OP_ADD || code == OP_SUBTRACT || code == OP_PLANEFILL)) {
            deposit_only = false;
        }
        d = apply_operator(packed_op(oi), d, p, center, nrm, s, fall, w, wk, g);
    }
    // Additive buildup control — MUST match the CPU `apply_stamp_plan` cap bit-for-bit.
    // Iso band ≈ 1.5 voxels (ClayBuildup band = vs·1.5) ⇒ Δdensity 1.0 ≈ 1.5 voxels of surface
    // motion; 0.5·strength caps one stamp at ≤ ~0.75 voxel of iso motion at strength 1.
    if (deposit_only) {
        let cap = clamp(strength(), 0.0, 1.0) * 0.5;
        d = cur + clamp(d - cur, -cap, cap);
    }
    density[i] = clamp(d, 0.0, 1.0);
}
