use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BlendProfile, BevelMode, ModifierType, MaskShape, ShaderType, MaterialQuality, Modifier, PbrMaterial};
use core_math::scene_graph::{SceneGraph, NodeId, NodePayload, BodyTransform};
use core_math::{DNA_MAX_RULES, DNA_FLOATS_PER_RULE};
use core_math::bvh::{Aabb, BvhNode, BvhPrim, SceneBvhBody, build_body_bvh, build_scene_bvh};
use std::collections::{HashSet, HashMap};
use bytemuck;

/// Feature flag: when true, prim.position is body-local (relative to body.origin).
/// When false, prim.position is world-space (legacy behavior).
#[allow(dead_code)]
const BODY_LOCAL_PRIMS: bool = true;

pub use core_math::FLOATS_PER_PRIM;

/// Resolve the effective PbrMaterial for a prim — delegates to THE canonical resolver on
/// SceneGraph (shared with the raster G-buffer pass and the albedo-map uniform).
fn resolve_prim_material<'a>(p: &'a SdfPrimitive, sg: &'a SceneGraph) -> &'a PbrMaterial {
    sg.resolve_prim_material(p)
}

/// Controls which bodies are serialized into the analytical GPU prim buffer.
#[derive(Clone, Copy, PartialEq)]
pub enum SerializeMode {
    /// Serialize all visible bodies (used for bake, save, no-cache mode, debug).
    Full,
    /// Serialize only the active body (used when hybrid cache covers all others).
    /// The NodeId is resolved to its top-level body ancestor before filtering.
    ActiveOnly(NodeId),
}

/// Walk from `nid` up through the scene graph to find the top-level SdfBody ancestor.
/// Returns `nid` itself if it has no parent body (i.e. it IS top-level).
fn resolve_top_level_body(sg: &SceneGraph, nid: NodeId) -> NodeId {
    let mut current = nid;
    while let Some(parent) = sg.parent_body(current) {
        current = parent;
    }
    current
}

/// One step of a body's UNIFIED eval order (prims + sub-bodies interleaved).
/// `Prim` carries the prim's LOCAL index in `primitives` (the `orig_idx` selection/Lipschitz
/// lookups need) plus the value; `Body` carries a direct child NodeId to recurse into.
enum OrderedChild<'a> {
    Prim(usize, &'a SdfPrimitive),
    Body(NodeId),
}

/// The SINGLE ordered walk over a body's `child_order`, shared by BOTH bake serializers
/// (`_hot` and non-hot) so they cannot diverge (CLAUDE.md: two paths for one op share semantics).
/// Resolves each `BodyChildRef`:
///   - `Prim(uid)` → `(local_index, &SdfPrimitive)` via the body's own `primitives` (resolution at
///     serialize time only — `child_order` stores PrimUid, never a flat/local index).
///   - `Body(nid)` → that direct child node (the per-child visibility check is the caller's job, to
///     match the legacy behaviour exactly).
/// Dangling refs (a uid/nid not in the live body) are skipped — `body_child_order()` already
/// reconciles, so this is defence-in-depth.
fn body_eval_order<'a>(sg: &'a SceneGraph, body_id: NodeId) -> Vec<OrderedChild<'a>> {
    let prims: &[SdfPrimitive] = match sg.body_primitives(body_id) {
        Some(p) => p,
        None => return Vec::new(),
    };
    // resolve_child_order is the SINGLE source of truth for the sequence — and it consumes prims
    // POSITIONALLY per uid, so duplicate-uid prims (SdfPrimitive::new defaults uid=0) each emit once.
    let resolved = sg.resolve_child_order(body_id);
    let mut out = Vec::with_capacity(resolved.len());
    for entry in resolved {
        match entry {
            Ok(i) => {
                if let Some(p) = prims.get(i) { out.push(OrderedChild::Prim(i, p)); }
            }
            Err(nid) => {
                // child_order stores the DIRECT child node. The legacy serializer recursed via
                // child_bodies() which descends through SdfObject wrappers; preserve that by
                // expanding an SdfObject child into its underlying bodies in order.
                match sg.get(nid).map(|n| &n.payload) {
                    Some(NodePayload::SdfObject { .. }) => {
                        for cb in sg.child_bodies(nid) { out.push(OrderedChild::Body(cb)); }
                    }
                    Some(_) => out.push(OrderedChild::Body(nid)),
                    None => {}
                }
            }
        }
    }
    out
}

/// Result of `serialize_from_graph` — avoids fragile positional tuples.
pub struct SerializeResult {
    /// Number of GPU prim-buffer entries (includes SCOPE_OPEN/CLOSE markers).
    pub num_entries: u32,
    /// Flat byte buffer for the GPU prim storage buffer.
    pub prim_bytes: Vec<u8>,
    /// Scene-level BVH nodes (one leaf per top-level body). Empty if ≤1 body.
    pub body_bvh_bytes: Vec<u8>,
    /// Per-body byte ranges in prim_bytes: (body_nid, byte_start, byte_end, flat_offset).
    /// Used for partial re-serialization — patch one body's bytes without full serialize.
    pub body_byte_ranges: Vec<(NodeId, usize, usize, usize)>,
    /// LAB poly→SDF exact-silhouette ghost. Parallel to the prim buffer's SLOTS (markers +
    /// prims, same order): `u32` grid-header offset into `mesh_grid_bytes` for each SELECTED
    /// MeshVolume prim, else `u32::MAX`. `mesh_grid_bytes` packs those grids
    /// ([res,min.xyz,cell,3 reserved]+dist). Both empty/all-MAX when no MeshVolume is selected —
    /// the prim_outline pass binds them to march the exact mesh silhouette. Cheap otherwise
    /// (one u32/slot, no grid packed).
    pub mesh_start_bytes: Vec<u8>,
    pub mesh_grid_bytes: Vec<u8>,
    /// CurveExtrude / clip-curve profiles for the outline pass (bindings 7/8). Parallel to
    /// prim slots: `u32` float-offset into `clip_curves_bytes`, else `u32::MAX`. Layout matches
    /// bake `serialize_clip_curves_for_bake` records so outline eval is the real polyline SDF.
    pub clip_start_bytes: Vec<u8>,
    pub clip_curves_bytes: Vec<u8>,
    /// T1 live-march image displace atlas (raymarch binding 7). Prim field [292] indexes
    /// into this buffer (same layout as bake height_maps). Min 1 float for wgpu when empty.
    pub height_map_bytes: Vec<u8>,
}

/// Pack one MeshVolume grid (header + distances) into `buf`, returning its start float-offset.
/// Layout matches `sample_mesh_grid_at` / prim_outline: [res, min.xyz, cell, 3 reserved] + res³.
fn pack_mesh_grid(grid: &core_math::sdf_shapes::MeshSdfData, buf: &mut Vec<f32>) -> u32 {
    let off = buf.len() as u32;
    buf.push(grid.res as f32);
    buf.push(grid.bounds_min.x); buf.push(grid.bounds_min.y); buf.push(grid.bounds_min.z);
    buf.push(grid.cell);
    buf.push(0.0); buf.push(0.0); buf.push(0.0);
    buf.extend_from_slice(&grid.dist);
    off
}

/// Pack one CurveExtrude / clip profile into the outline clip buffer (same record layout as bake).
/// Returns float-offset of the record start. `mode_slot`: 0 = closed winding polygon; nonzero =
/// open path thickness (negative = tube). `open_raw`: raw verts vs expanded boundary_polygon.
fn pack_curve_profile(
    profile: &core_math::primitives::CurveProfile,
    mode_slot: f32,
    open_raw: bool,
    buf: &mut Vec<f32>,
) -> u32 {
    let off = buf.len() as u32;
    let capped_n = profile.verts.len().min(core_math::primitives::MAX_CURVE_PROFILE_VERTS);
    let pts: Vec<glam::Vec2> = if open_raw {
        profile.verts[..capped_n].iter().map(|v| v.pos).collect()
    } else {
        core_math::curves::boundary_polygon(&profile.verts[..capped_n])
    };
    let vc = pts.len();
    buf.push(mode_slot);
    buf.push(f32::from_bits(vc as u32));
    buf.push(profile.plane.tangent_u.x);
    buf.push(profile.plane.tangent_u.y);
    buf.push(profile.plane.tangent_u.z);
    buf.push(profile.plane.tangent_v.x);
    buf.push(profile.plane.tangent_v.y);
    buf.push(profile.plane.tangent_v.z);
    for pt in &pts {
        buf.push(pt.x);
        buf.push(pt.y);
    }
    off
}

// ============================================================
// Serialize primitives -> flat f32 array for GPU
// 80 floats per primitive:
//   [0-2]   position xyz
//   [3]     shape_type (bits 0-7) | selected (bit 8) | stair_steps (bits 16-31)
//   [4-7]   quaternion xyzw
//   [8-10]  scale xyz
//   [11]    op_type
//   [12-14] shape params
//   [15]    blend_k
//   [16-18] base albedo rgb
//   [19]    base roughness
//   [20]    base metallic
//   [21]    edge_wear_amount (0=off)
//   [22-24] edge_wear_color rgb
//   [25]    edge_wear_roughness
//   [26]    edge_wear_metallic
//   [27]    edge_wear_sharpness
//   [28]    cavity_amount (0=off)
//   [29-31] cavity_color rgb
//   [32]    cavity_roughness
//   [33]    cavity_metallic
//   [34]    cavity_sharpness
//   [35]    noise_amount (0=off)
//   [36-38] noise_color rgb
//   [39]    noise_roughness
//   [40]    noise_metallic
//   [41]    noise_scale
//   [42]    noise_octaves (as f32)
//   [43]    has_texture
//   [44]    tex_tiling_scale
//   [45]    bvh_bound_r
//   [46-47] falloff dir.xy * length (packed)
//   [48-59] 12 per-edge bevel radii (edges 0-11; 0-3=bot, 4-7=top, 8-11=vert)
//   [60]    packed mode bits: 2 bits per edge × 12 edges (0=none,1=round,2=chamfer)
//   [61]    mirror_flags
//   [62]    falloff dir.z * length (packed)
//   [63]    falloff origin_offset
//   [64]    modifier_type (u32): 0=None, 1=Twist, 2=Taper, 3=Smooth, 4=Displace, 5=Mirror, 6=Shear, 7=Sculpt
//   [65]    modifier_strength (f32)
//   [66]    modifier_axis (u32): 0=Y, 1=X, 2=Z
//   [67]    modifier_param2 (f32)
//   [68]    mask_type (u32): 0=None, 1=Sphere, 2=Box, 3=Gradient, 4=Noise3D
//   [69-71] mask_center xyz
//   [72-74] mask_extents xyz
//   [75]    mask_feather
//   [76]    mask_invert (u32)
//   [77]    opacity (0=transparent, 1=opaque)
//   [78-80] mirror_center xyz
//   [81]    layer_eval_id (u32: 0=ungrouped, >0=layer id for boolean isolation)
//   [82]    object_eval_id (u32: 0=ungrouped, >0=object id for boolean isolation)
//   [83]    texture_id (u32: 0=none, >0=atlas entry)
//   [84]    shader_type (u32: 0=PBR,1=SSS,2=Glass,3=VFX,4=Advanced)
//   [85]    mat_quality (u32: 0=Optimize,1=Normal,2=God)
//   [86]    normal_strength
//   [87-89] emission_color rgb
//   [90]    emission_intensity
//   [91]    sss_radius          [92-94] sss_color rgb
//   [95]    ior                 [96-98] glass_tint rgb
//   [99]    clearcoat           [100] clearcoat_roughness
//   [101]   anisotropy          [102] anisotropy_rotation
//   [103]   sheen               [104] sheen_tint
//   [105-107] reserved
//   --- Material DNA (slots 116-279) ---
//   [116]   dna_rule_count (0 = DNA disabled)
//   [117]   dna_age (0.0-1.0)
//   [118-119] reserved
//   [120..279] 8 rules × 20 floats per rule:
//     [+0] condition_type  [+1] geo_input/angle  [+2..+4] params a/b/c
//     [+5] age_min  [+6] age_max  [+7..+9] albedo  [+10] roughness  [+11] metalness
//     [+12..+14] emission  [+15] intensity  [+16] blend_mode  [+17] height
//     [+18] noise_scale  [+19] reserved (-1 = invisible)
// ============================================================

/// Per-prim render overrides — applied during serialization without mutating source data.
/// Used by dispatch_gpu_render to handle billboard facing, edit dimming, footprint hiding,
/// and layer bake transforms as pure read-only overrides.
#[derive(Clone, Default)]
pub struct RenderPrimOverride {
    pub position: Option<glam::Vec3>,
    pub scale: Option<glam::Vec3>,
    pub rotation_deg: Option<glam::Vec3>,
    pub opacity: Option<f32>,
    pub hidden: bool,
    pub modifier: Option<Modifier>,
}

pub fn serialize_primitives(prims: &[SdfPrimitive], selected: &HashSet<usize>, textured: &HashSet<usize>) -> Vec<f32> {
    serialize_primitives_ex(prims, selected, textured, &HashSet::new())
}

/// Serialize an SdfBody and its child bodies for volume baking, with SCOPE_OPEN/CLOSE markers.
/// Mirrors `serialize_body_recursive()` but simplified for bake (no selection, textures, overrides).
/// The root body being baked gets its own scope wrapper (needed for mirror/bevel at scope level).
pub fn serialize_body_for_bake(sg: &SceneGraph, body_nid: NodeId) -> Vec<f32> {
    let empty_sel = HashSet::new();
    let empty_tex = HashSet::new();
    let mut out = Vec::new();
    serialize_body_for_bake_recursive(sg, body_nid, true, &empty_sel, &empty_tex, &mut out);
    if out.is_empty() { out.resize(FLOATS_PER_PRIM, 0.0); }
    out
}

/// Hot-only bake serialization at HOT_FLOATS_PER_PRIM (84) stride.
/// Used by volume bake path — the bake shader only needs SDF eval data, not material/DNA.
///
/// `body_origin`: the body's world-space origin. Prim positions and AABB/mirror_center
/// are offset by -body_origin so the serialized data is in body-local space.
pub fn serialize_body_for_bake_hot(sg: &SceneGraph, body_nid: NodeId, body_origin: glam::Vec3) -> Vec<f32> {
    let empty_sel = HashSet::new();
    let mut out = Vec::new();
    let origin_arr = [body_origin.x, body_origin.y, body_origin.z];
    // The bake frame is the TOP body's local frame. Under BODY_LOCAL_PRIMS a nested sub-body's prims
    // are stored relative to the SUB-body's own origin, so they must be shifted by
    // `(sub_origin - top_origin)` to bake at the same place the grid bounds cover them (mirrors the
    // identical shift in `collect_body_prims_recursive`). Without it, grouped geometry bakes offset
    // from its grid → clipped/vanished. Uniform for every prim: the top body's own prims get shift 0.
    let top_origin = match sg.get(body_nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { origin, .. }) => *origin,
        _ => glam::Vec3::ZERO,
    };
    serialize_body_for_bake_hot_recursive(sg, body_nid, true, &empty_sel, &origin_arr, top_origin, &mut out);
    if out.is_empty() { out.resize(HOT_FLOATS_PER_PRIM, 0.0); }
    out
}

/// Serialize all clip curves from a body's prims into a separate flat f32 buffer.
/// Also patches hot[85] (clip_buf_start) in the already-serialized HOT buffer so the
/// GPU shader knows where each prim's clip data starts.
///
/// Clip buffer layout per clip:
///   [0]    mode (u32)
///   [1]    vert_count (u32)
///   [2-4]  plane tangent_u xyz
///   [5-7]  plane tangent_v xyz
///   [8..8+vert_count*2] vertex positions (x,y pairs)
///
/// Returns the flat f32 buffer. Minimum 4 bytes (1 float) to satisfy wgpu.
pub fn serialize_clip_curves_for_bake(sg: &SceneGraph, body_nid: NodeId, hot_buf: &mut [f32]) -> Vec<f32> {
    let mut clip_buf: Vec<f32> = Vec::new();
    // Walk the HOT buffer prim-by-prim, find prims with clips, serialize their clip data
    let prim_count = hot_buf.len() / HOT_FLOATS_PER_PRIM;
    serialize_clip_curves_recursive(sg, body_nid, hot_buf, &mut clip_buf, &mut 0);
    if clip_buf.is_empty() { clip_buf.push(0.0); } // minimum 1 float for wgpu
    clip_buf
}

fn serialize_clip_curves_recursive(
    sg: &SceneGraph, body_id: NodeId,
    hot_buf: &mut [f32], clip_buf: &mut Vec<f32>,
    hot_prim_idx: &mut usize,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, child_bodies) = match &node.payload {
        core_math::scene_graph::NodePayload::SdfBody { primitives, .. } => {
            (primitives.as_slice(), sg.child_bodies(body_id))
        },
        _ => return,
    };
    // Skip SCOPE_OPEN marker
    *hot_prim_idx += 1;
    for p in prims {
        if !p.visible { continue; }
        let hi = *hot_prim_idx;
        let base = hi * HOT_FLOATS_PER_PRIM;
        if base + HOT_FLOATS_PER_PRIM > hot_buf.len() { *hot_prim_idx += 1; continue; }

        // Collect valid clips (from clip_curves Vec + CurveExtrude shape). Each entry carries:
        //   .1 mode_slot = the f32 written at clip[+0]:
        //        - clip CUTTER: f32::from_bits(mode) (0=Subtract, 1=KeepInside), read back as u32 by
        //          the cutter loop (volume_bake.wgsl).
        //        - CLOSED CurveExtrude shape: 0.0 → the st==9 branch treats 0 as "closed" (winding SDF).
        //        - OPEN CurveExtrude shape (P2): SIGNED thickness — |t| = band width / tube diameter,
        //          sign<0 ⇒ tube. Self-contained in the record the st==9 branch owns (the cutter loop
        //          skips st==9), so no hot-buffer slot is needed.
        //   .2 open_raw = serialize RAW path points (open polyline, no corner expansion / no closing
        //        wrap) instead of the closed boundary_polygon.
        let mut clips_to_serialize: Vec<(&core_math::primitives::CurveProfile, f32, bool)> = Vec::new();
        for c in &p.clip_curves {
            if c.is_valid() {
                clips_to_serialize.push((&c.profile, f32::from_bits(c.mode), false));
            }
        }
        if let core_math::primitives::SdfShape::CurveExtrude { ref profile, open, thickness, tube, .. } = p.shape {
            if profile.is_valid() {
                let is_open = open && thickness > 1e-5;
                let mode_slot = if is_open {
                    let t = thickness.abs().max(1e-4);
                    if tube { -t } else { t }
                } else { 0.0 };
                clips_to_serialize.push((profile, mode_slot, is_open));
            }
        }

        // SpinePipe (st==12): pack 3D points + per-point radius scales.
        // Layout: [base_radius][count][closed][pad×5]
        //         then xyz triples (n*3), then point_scale floats (n). Owned by st==12 only.
        // Phase B/E: expand Sharp/Bezier/Chamfer (+ handles) into the centerline (same as CPU)
        // so GPU bake stays a plain capsule chain with automatic parity.
        if let core_math::primitives::SdfShape::SpinePipe {
            ref points, radius, closed, ref joins, ref point_scale,
            ref handles_in, ref handles_out, ..
        } = p.shape {
            let (exp_pts, exp_sc) = core_math::curves::expand_spine_polyline(
                points, joins, point_scale, closed, handles_in, handles_out,
            );
            let n = exp_pts.len().min(core_math::curves::MAX_SPINE_POINTS);
            if n >= 1 {
                let start_idx = clip_buf.len() as u32;
                hot_buf[base + 85] = f32::from_bits(start_idx);
                let old_packed = f32::to_bits(hot_buf[base + 84]);
                let new_packed = (old_packed & 0x0000FFFF) | (1u32 << 16) | 1;
                hot_buf[base + 84] = f32::from_bits(new_packed);
                clip_buf.push(radius);
                clip_buf.push(f32::from_bits(n as u32));
                clip_buf.push(if closed { 1.0 } else { 0.0 });
                clip_buf.push(0.0);
                clip_buf.push(0.0);
                clip_buf.push(0.0);
                clip_buf.push(0.0);
                clip_buf.push(0.0);
                for q in exp_pts.iter().take(n) {
                    clip_buf.push(q.x);
                    clip_buf.push(q.y);
                    clip_buf.push(q.z);
                }
                for i in 0..n {
                    let s = exp_sc.get(i).copied().unwrap_or(1.0).max(0.001);
                    clip_buf.push(s);
                }
            }
        }

        if !clips_to_serialize.is_empty() {
            // Patch hot[85] = start index into clip_buf
            let start_idx = clip_buf.len() as u32;
            hot_buf[base + 85] = f32::from_bits(start_idx);
            // Patch hot[84] clip count
            let old_packed = f32::to_bits(hot_buf[base + 84]);
            let clip_count = clips_to_serialize.len() as u32;
            let new_packed = (old_packed & 0x0000FFFF) | ((clip_count & 0xFFFF) << 16) | 1; // bit0=has_clips
            hot_buf[base + 84] = f32::from_bits(new_packed);

            for (profile, mode_slot, open_raw) in &clips_to_serialize {
                // Cap AUTHORED verts. CLOSED/cutter: expand per-vertex corners into an explicit boundary
                // polygon (the GPU winding SDF consumes it verbatim → NO WGSL corner math, auto parity).
                // OPEN: emit the RAW path points (no corner expansion, no closing segment) — the GPU
                // open-path distance (dist_to_open_polyline_gpu) offsets them by thickness. Either way
                // the clip buffer is dynamic; `vc` records the real length.
                let capped_n = profile.verts.len().min(core_math::primitives::MAX_CURVE_PROFILE_VERTS);
                let pts: Vec<glam::Vec2> = if *open_raw {
                    profile.verts[..capped_n].iter().map(|v| v.pos).collect()
                } else {
                    core_math::curves::boundary_polygon(&profile.verts[..capped_n])
                };
                let vc = pts.len();
                clip_buf.push(*mode_slot);
                clip_buf.push(f32::from_bits(vc as u32));
                clip_buf.push(profile.plane.tangent_u.x);
                clip_buf.push(profile.plane.tangent_u.y);
                clip_buf.push(profile.plane.tangent_u.z);
                clip_buf.push(profile.plane.tangent_v.x);
                clip_buf.push(profile.plane.tangent_v.y);
                clip_buf.push(profile.plane.tangent_v.z);
                for pt in &pts {
                    clip_buf.push(pt.x);
                    clip_buf.push(pt.y);
                }
            }
        }
        *hot_prim_idx += 1;
    }
    // Recurse into child bodies
    for child_id in &child_bodies {
        if let Some(child_node) = sg.get(*child_id) {
            if !child_node.visible { continue; }
        }
        serialize_clip_curves_recursive(sg, *child_id, hot_buf, clip_buf, hot_prim_idx);
    }
    // Skip SCOPE_CLOSE marker
    *hot_prim_idx += 1;
}

/// T1/T3 — pack per-prim image height maps for GPU bake displace.
///
/// Side buffer layout (bound at `@binding(8)` in `volume_bake.wgsl`):
///   [0] w (u32 bits)  [1] h  [2] axis (u32)  [3] reserved
///   [4 .. 4+w*h] height samples as f32 in [0,1] (row-major)
///
/// Patches `hot[92]` with the float index of the header (or leaves `HEIGHT_MAP_NONE`).
/// Walk order matches SCOPE_OPEN/CLOSE + visibility of `serialize_body_for_bake_hot`.
///
/// `max_side`: T3 progressive refine — when non-zero, each map is box-filtered so both
/// dimensions ≤ `max_side` (use `DRAFT_HEIGHT_MIP_MAX_SIDE` during draft bake, `0` on settle).
/// Returns min 1 float for wgpu when empty.
pub fn serialize_height_maps_for_bake(
    sg: &SceneGraph,
    body_nid: NodeId,
    hot_buf: &mut [f32],
    max_side: u32,
) -> Vec<f32> {
    let mut atlas: Vec<f32> = Vec::new();
    serialize_height_maps_recursive(sg, body_nid, hot_buf, &mut atlas, &mut 0, max_side);
    if atlas.is_empty() {
        atlas.push(0.0);
    }
    atlas
}

/// Convenience: full-resolution pack (settle / non-draft).
pub fn serialize_height_maps_for_bake_full(
    sg: &SceneGraph,
    body_nid: NodeId,
    hot_buf: &mut [f32],
) -> Vec<f32> {
    serialize_height_maps_for_bake(sg, body_nid, hot_buf, 0)
}

fn serialize_height_maps_recursive(
    sg: &SceneGraph,
    body_id: NodeId,
    hot_buf: &mut [f32],
    atlas: &mut Vec<f32>,
    hot_prim_idx: &mut usize,
    max_side: u32,
) {
    use core_math::modifiers::ModifierType;
    let node = match sg.get(body_id) {
        Some(n) => n,
        None => return,
    };
    if !matches!(&node.payload, core_math::scene_graph::NodePayload::SdfBody { .. }) {
        return;
    }
    *hot_prim_idx += 1; // SCOPE_OPEN — lockstep with serialize_body_for_bake_hot
    for item in body_eval_order(sg, body_id) {
        match item {
            OrderedChild::Prim(_, p) => {
                if !p.visible {
                    continue;
                }
                // Array expand matches bake serialize (one hot slot per copy).
                for sp in p.expand_array_copies() {
                    let hi = *hot_prim_idx;
                    let base = hi * HOT_FLOATS_PER_PRIM;
                    if base + HOT_FLOATS_PER_PRIM > hot_buf.len() {
                        *hot_prim_idx += 1;
                        continue;
                    }
                    let m = sp.gpu_slot_modifier();
                    if m.mod_type == ModifierType::Displace {
                        if let Some(ref hm) = m.height_map {
                            if hm.w > 0 && hm.h > 0 && !hm.pixels.is_empty() {
                                // T3: pack a coarse mip during draft so GPU upload stays small.
                                let packed = hm.downsampled_to_max_side(max_side);
                                let start = pack_height_map_into(&packed, m.axis, atlas);
                                hot_buf[base + 92] = f32::from_bits(start);
                            }
                        }
                    }
                    *hot_prim_idx += 1;
                }
            }
            OrderedChild::Body(child_id) => {
                if let Some(child_node) = sg.get(child_id) {
                    if !child_node.visible {
                        continue;
                    }
                }
                serialize_height_maps_recursive(
                    sg, child_id, hot_buf, atlas, hot_prim_idx, max_side,
                );
            }
        }
    }
    *hot_prim_idx += 1; // SCOPE_CLOSE
}

/// LAB poly→SDF: serialize every `SdfShape::MeshVolume` prim's sampled grid into a separate
/// flat f32 buffer (bound at @binding(5) in volume_bake.wgsl). Mirrors
/// `serialize_clip_curves_for_bake` — walks the HOT buffer prim-by-prim in LOCKSTEP with the
/// graph (same SCOPE_OPEN/CLOSE + visibility skipping), and for each MeshVolume prim packs its
/// grid and patches `hot[86]` with the float index where the grid header starts.
///
/// Per-grid layout (matches `sample_mesh_grid_at` in volume_bake.wgsl):
///   [0]   res              [1..3] bounds_min.xyz (shape-local)   [4] cell   [5..7] reserved
///   [8 ..8+res^3] signed distances in `x + y*res + z*res*res` order
///
/// The header is shape-local (centered grid) — the shader samples at the prim-LOCAL eval point
/// (after the standard position/rotation/scale transform), so no per-body matrix is needed.
/// Returns the buffer (min 1 float for wgpu).
pub fn serialize_mesh_grids_for_bake(sg: &SceneGraph, body_nid: NodeId, hot_buf: &mut [f32]) -> Vec<f32> {
    let mut mesh_buf: Vec<f32> = Vec::new();
    serialize_mesh_grids_recursive(sg, body_nid, hot_buf, &mut mesh_buf, &mut 0);
    if mesh_buf.is_empty() { mesh_buf.push(0.0); } // minimum 1 float for wgpu
    mesh_buf
}

fn serialize_mesh_grids_recursive(
    sg: &SceneGraph, body_id: NodeId,
    hot_buf: &mut [f32], mesh_buf: &mut Vec<f32>,
    hot_prim_idx: &mut usize,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, child_bodies) = match &node.payload {
        core_math::scene_graph::NodePayload::SdfBody { primitives, .. } => {
            (primitives.as_slice(), sg.child_bodies(body_id))
        },
        _ => return,
    };
    *hot_prim_idx += 1; // SCOPE_OPEN marker
    for p in prims {
        if !p.visible { continue; }
        let hi = *hot_prim_idx;
        let base = hi * HOT_FLOATS_PER_PRIM;
        if base + HOT_FLOATS_PER_PRIM > hot_buf.len() { *hot_prim_idx += 1; continue; }
        if let core_math::primitives::SdfShape::MeshVolume { grid, .. } = &p.shape {
            let start = mesh_buf.len() as u32;
            hot_buf[base + 86] = f32::from_bits(start); // patch hot[86] = grid start index
            mesh_buf.push(grid.res as f32);
            mesh_buf.push(grid.bounds_min.x);
            mesh_buf.push(grid.bounds_min.y);
            mesh_buf.push(grid.bounds_min.z);
            mesh_buf.push(grid.cell);
            mesh_buf.push(0.0); mesh_buf.push(0.0); mesh_buf.push(0.0); // reserved header
            mesh_buf.extend_from_slice(&grid.dist);
        }
        *hot_prim_idx += 1;
    }
    for child_id in &child_bodies {
        if let Some(child_node) = sg.get(*child_id) {
            if !child_node.visible { continue; }
        }
        serialize_mesh_grids_recursive(sg, *child_id, hot_buf, mesh_buf, hot_prim_idx);
    }
    *hot_prim_idx += 1; // SCOPE_CLOSE marker
}

/// Recursive helper for hot-only bake serialization.
/// `origin_offset`: [x,y,z] to subtract from all world-space positions (body-local bake).
fn serialize_body_for_bake_hot_recursive(
    sg: &SceneGraph,
    body_id: NodeId,
    is_top_level: bool,
    empty_sel: &HashSet<usize>,
    origin_offset: &[f32; 3],
    top_origin: glam::Vec3,
    out: &mut Vec<f32>,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_prof, this_origin) = match &node.payload {
        NodePayload::SdfBody { primitives, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_profile, origin, .. } => {
            // Offset mirror_center to body-local space.
            // When BODY_LOCAL_PRIMS: mirror_center is already local — no offset needed.
            // Legacy path: mirror_center is world-space, so subtract origin_offset.
            let local_mc = if BODY_LOCAL_PRIMS {
                [mirror_center.x, mirror_center.y, mirror_center.z]
            } else {
                [
                    mirror_center.x - origin_offset[0],
                    mirror_center.y - origin_offset[1],
                    mirror_center.z - origin_offset[2],
                ]
            };
            (primitives, body_op, *mirror_flags, *mirror_sign, local_mc, *bevel_radius, *blend_profile as u32, *origin)
        },
        _ => return,
    };

    // Shift this body's prims into the top bake frame (BODY_LOCAL only). Top body → 0; sub-body →
    // (sub_origin - top_origin). Matches `collect_body_prims_recursive` so bake content and grid
    // bounds share one frame — every prim treated identically, no per-group special case.
    let prim_shift = if BODY_LOCAL_PRIMS { this_origin - top_origin } else { glam::Vec3::ZERO };

    let child_bodies = sg.child_bodies(body_id);
    if prims.is_empty() && child_bodies.is_empty() { return; }

    // Relativise the scope AABB to the bake frame (= the TOP body's origin).
    // compute_body_aabb() returns WORLD coords (it composes each body's local_to_world). The grid
    // bounds and the serialized prim positions are BOTH in the top body's local frame, so the scope
    // AABB — read by volume_bake.wgsl's per-scope cull (which, for a hard union scope_k=0, has NO
    // safety margin) — must be relativised by top_origin too, or a nested scope's cull drops geometry
    // that is actually inside the grid. Under BODY_LOCAL_PRIMS origin_offset is ZERO (prims are
    // already body-local), so the world→bake shift is top_origin. Legacy: prims are world-space and
    // origin_offset already carries the top origin.
    let aabb_shift = if BODY_LOCAL_PRIMS {
        [top_origin.x, top_origin.y, top_origin.z]
    } else {
        *origin_offset
    };
    let (mut aabb_min, mut aabb_max) = compute_body_aabb(sg, body_id);
    for axis in 0..3 {
        aabb_min[axis] -= aabb_shift[axis];
        aabb_max[axis] -= aabb_shift[axis];
    }
    if mirror_flags != 0 {
        for axis in 0..3 {
            if (mirror_flags >> axis) & 1 != 0 {
                let c = mirror_center[axis];
                let reflected_min = 2.0 * c - aabb_max[axis];
                let reflected_max = 2.0 * c - aabb_min[axis];
                aabb_min[axis] = aabb_min[axis].min(reflected_min);
                aabb_max[axis] = aabb_max[axis].max(reflected_max);
            }
        }
    }
    let open_float_idx = out.len();
    if is_top_level {
        emit_scope_open_hot(out, &BooleanOp::Union { k: 0.0 }, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    } else {
        emit_scope_open_hot(out, body_op, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    }
    // Bake path: overwrite hot[74] (body_index, unused by volume_bake.wgsl) with
    // scope max Lipschitz constant. L=1.0 for all standard shapes; future: max of
    // child prim L values when twist/bend modifiers get per-prim L > 1.
    out[open_float_idx + 74] = 1.0f32;

    // UNIFIED ORDER: walk prims AND child bodies in the body's `child_order` sequence so the
    // GPU boolean chain matches the outliner display. (Was: all prims, then all child bodies.)
    // Serialize prims in body-local space.
    // When BODY_LOCAL_PRIMS: prim.position is already local — write directly.
    // Legacy path: prim.position is world-space, subtract origin_offset to get local.
    for item in body_eval_order(sg, body_id) {
        match item {
            OrderedChild::Prim(i, p) => {
                if !p.visible { continue; }
                // Array modifier → N translated prims (GPU has no Array opcode).
                let copies = p.expand_array_copies();
                for mut sp in copies {
                    if BODY_LOCAL_PRIMS {
                        if prim_shift != glam::Vec3::ZERO {
                            sp.position += prim_shift;
                        }
                    } else {
                        sp.position.x -= origin_offset[0];
                        sp.position.y -= origin_offset[1];
                        sp.position.z -= origin_offset[2];
                    }
                    serialize_single_prim_hot(&sp, i, empty_sel, None, 0, None, out);
                    // Bake path: overwrite hot[74] with per-prim Lipschitz constant.
                    let lipschitz = 1.0f32;
                    let lip_idx = out.len() - HOT_FLOATS_PER_PRIM + 74;
                    out[lip_idx] = lipschitz;
                }
            }
            OrderedChild::Body(child_id) => {
                if let Some(child_node) = sg.get(child_id) {
                    if !child_node.visible { continue; }
                }
                serialize_body_for_bake_hot_recursive(sg, child_id, false, empty_sel, origin_offset, top_origin, out);
            }
        }
    }

    emit_scope_close_hot(out);
    patch_scope_close_hot(out, open_float_idx);
}

/// Recursive helper for body bake serialization.
fn serialize_body_for_bake_recursive(
    sg: &SceneGraph,
    body_id: NodeId,
    is_top_level: bool,
    empty_sel: &HashSet<usize>,
    empty_tex: &HashSet<usize>,
    out: &mut Vec<f32>,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_prof) = match &node.payload {
        NodePayload::SdfBody { primitives, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_profile, .. } => {
            (primitives, body_op, *mirror_flags, *mirror_sign, [mirror_center.x, mirror_center.y, mirror_center.z], *bevel_radius, *blend_profile as u32)
        },
        _ => return,
    };

    let child_bodies = sg.child_bodies(body_id);
    // Skip truly empty bodies (no own prims AND no child bodies).
    // Avoids emitting degenerate scope markers that waste GPU cycles.
    if prims.is_empty() && child_bodies.is_empty() { return; }

    let (mut aabb_min, mut aabb_max) = compute_body_aabb(sg, body_id);
    if mirror_flags != 0 {
        for axis in 0..3 {
            if (mirror_flags >> axis) & 1 != 0 {
                let c = mirror_center[axis];
                let reflected_min = 2.0 * c - aabb_max[axis];
                let reflected_max = 2.0 * c - aabb_min[axis];
                aabb_min[axis] = aabb_min[axis].min(reflected_min);
                aabb_max[axis] = aabb_max[axis].max(reflected_max);
            }
        }
    }
    let open_float_idx = out.len();
    if is_top_level {
        emit_scope_open(out, &BooleanOp::Union { k: 0.0 }, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    } else {
        emit_scope_open(out, body_op, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    }

    // UNIFIED ORDER: walk prims AND child bodies in the body's `child_order` sequence (parity with
    // the `_hot` twin via the shared `body_eval_order` helper). Was: all prims, then all child bodies.
    for item in body_eval_order(sg, body_id) {
        match item {
            OrderedChild::Prim(i, p) => {
                if !p.visible { continue; }
                let resolved_mat = resolve_prim_material(p, sg);
                serialize_single_prim(p, i, empty_sel, empty_tex, None, Some(resolved_mat), out, None);
            }
            OrderedChild::Body(child_id) => {
                if let Some(child_node) = sg.get(child_id) {
                    if !child_node.visible { continue; }
                }
                serialize_body_for_bake_recursive(sg, child_id, false, empty_sel, empty_tex, out);
            }
        }
    }

    emit_scope_close(out);
    let open_entry = open_float_idx / FLOATS_PER_PRIM;
    let close_entry = (out.len() / FLOATS_PER_PRIM) - 1;
    let offset = (close_entry - open_entry) as u32;
    out[open_float_idx + 4] = f32::from_bits(offset);
}


/// Legacy monolithic serialization — remove after shader split (Sprint 3 Prompt B).
/// Serialize a single SdfPrimitive into the flat GPU f32 buffer (280 floats).
/// `orig_idx` is the primitive's index in the original flat array (used for selection/texture lookup).
/// `ovr` provides optional per-prim render overrides (position, rotation, opacity, modifier)
/// applied during serialization without mutating the source primitive.
fn serialize_single_prim(
    p: &SdfPrimitive,
    orig_idx: usize,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    ovr: Option<&RenderPrimOverride>,
    mat_override: Option<&PbrMaterial>,
    out: &mut Vec<f32>,
    height_atlas: Option<&mut Vec<f32>>,
) {
    // Apply render overrides (read-only — source prim is not mutated)
    let pos = ovr.and_then(|o| o.position).unwrap_or(p.position);
    // P3: stack-aware single-slot packing. Multi-mod bodies force CPU bake separately.
    let stack_mod = p.gpu_slot_modifier();
    let modifier = ovr.and_then(|o| o.modifier.as_ref()).unwrap_or(&stack_mod);
    let (stack_mflags, stack_mcenter) = p.gpu_mirror_state();
    let mat_ref = mat_override.unwrap_or(&p.material);
    let opacity = ovr.and_then(|o| o.opacity).unwrap_or(mat_ref.opacity);

    // Pre-compute stair_steps so we can pack into shape_type field
    let stair_steps: Option<u32> = match &p.shape {
        SdfShape::Stair { num_steps, .. } => Some(*num_steps),
        SdfShape::SpiralStair { num_steps, .. } => Some(*num_steps),
        _ => None,
    };
    // [0-2] position
    out.push(pos.x); out.push(pos.y); out.push(pos.z);
    // [3] shape_type (bits 0-7) | selected (bit 8) | stair_steps (bits 16-31)
    let mut st: u32 = match &p.shape {
        SdfShape::Sphere{..}=>0, SdfShape::Box{..}=>1, SdfShape::Cylinder{..}=>2,
        SdfShape::Torus{..}=>3, SdfShape::Cone{..}=>4, SdfShape::Capsule{..}=>5,
        SdfShape::Stair{..}=>6, SdfShape::SpiralStair{..}=>7, SdfShape::Billboard{..}=>8,
        SdfShape::CurveExtrude{..}=>9, SdfShape::MeshVolume{..}=>10, SdfShape::HalfSpace=>11,
        SdfShape::SpinePipe{..}=>12,
    };
    if selected.contains(&orig_idx) { st |= 256; }
    if let Some(steps) = stair_steps { st |= (steps & 0xFFFF) << 16; }
    out.push(f32::from_bits(st));
    // [4-7] quaternion (with optional rotation override)
    let q = if let Some(rot) = ovr.and_then(|o| o.rotation_deg) {
        // Build quat from override rotation_deg (same logic as SdfPrimitive::rotation_quat)
        glam::Quat::from_euler(glam::EulerRot::YXZ,
            rot.y.to_radians(), rot.x.to_radians(), rot.z.to_radians())
    } else {
        p.rotation_quat()
    };
    out.push(q.x); out.push(q.y); out.push(q.z); out.push(q.w);
    // [8-10] scale (with optional body_scale override)
    let scl = ovr.and_then(|o| o.scale).unwrap_or(p.scale);
    out.push(scl.x); out.push(scl.y); out.push(scl.z);
    // [11] op_type
    // op_type: 0=Union, 1=Subtract, 2=Intersect, 3=ChamferUnion, 4=ChamferSubtract,
    //          5=Seam cut, 6=Seam raised, 7=Extrude (footprint raise/recess)
    // k=0 → hard edge, k>0 → smooth blend (smin / smax). For Seam/Extrude ops, k = WIDTH.
    // ChamferUnion/ChamferSubtract: dual-encode op 3/4 (shader compat) + blend_profile=Chamfer
    // Seam: raised dual-encoded into the op tag (5/6); cross-section profile (0=V,1=U,2=Square)
    // rides the blend_profile slot [106]; depth rides [291] (end of this fn).
    // Extrude: op 7; SIGNED strength rides the same [291] slot (an op is EITHER Seam OR
    // Extrude, so the depth slot is overloaded — zero new floats), width rides the k slot.
    let (op_type, bk, profile_override): (u32, f32, Option<u32>) = match &p.operation {
        BooleanOp::Union          { k } => (0, *k, None),
        BooleanOp::Subtract       { k } => (1, *k, None),
        BooleanOp::Intersect           => (2, 0.0, None),
        BooleanOp::ChamferUnion    { k } => (3, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::ChamferSubtract { k } => (4, *k, Some(BlendProfile::Chamfer as u32)),
        // Smooth/Chamfer intersect: op_type 2 (intersect) + k drives smax; profile picks the shape.
        BooleanOp::SmoothIntersect { k } => (2, *k, None),
        BooleanOp::Seam { width, profile, raised, .. } => (5 + *raised as u32, *width, Some(*profile as u32)),
        BooleanOp::Extrude { width, .. } => (7, *width, None),
    };
    out.push(f32::from_bits(op_type));
    // [12-14] shape params
    match &p.shape {
        SdfShape::Sphere{radius}=>{out.push(*radius);out.push(0.0);out.push(0.0);}
        SdfShape::Box{half_extents,..}=>{out.push(half_extents.x);out.push(half_extents.y);out.push(half_extents.z);}
        SdfShape::Cylinder{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Torus{major_radius,minor_radius}=>{out.push(*major_radius);out.push(*minor_radius);out.push(0.0);}
        SdfShape::Cone{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Capsule{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Stair{half_extents,..}=>{out.push(half_extents.x);out.push(half_extents.y);out.push(half_extents.z);}
        SdfShape::SpiralStair{radius,half_height,step_width,..}=>{out.push(*radius);out.push(*half_height);out.push(*step_width);}
        SdfShape::Billboard{width,height,face_camera}=>{out.push(*width);out.push(*height);out.push(if *face_camera {1.0} else {0.0});}
        // [12]=half_depth, [13]=wall_t (shell), [14]=rim (extrude-cap bevel; sign=mode, mag=radius).
        SdfShape::CurveExtrude{half_depth,wall_t,rim,..}=>{out.push(*half_depth);out.push(*wall_t);out.push(*rim);}
        // MeshVolume: grid header (res/bounds/cell) rides in the side buffer. params[12-14]
        // = conservative half-extent so the ANALYTICAL path (prim_outline ghost, march) can
        // draw a bounding-box silhouette — it can't sample the grid (bake path does). Bake
        // ignores these (its main loop overrides st==10 with eval_mesh_volume).
        SdfShape::MeshVolume{grid, ..}=>{let he=grid.corner_abs_max(); out.push(he.x);out.push(he.y);out.push(he.z);}
        // HalfSpace: infinite plane, no shape params — the transform (rotation+position) is the plane.
        SdfShape::HalfSpace=>{out.push(0.0);out.push(0.0);out.push(0.0);}
        // SpinePipe: radius + point count + closed flag; points ride the clip side buffer.
        SdfShape::SpinePipe{points,radius,closed,..}=>{
            out.push(*radius);
            let n = points.len().min(core_math::curves::MAX_SPINE_POINTS);
            out.push(f32::from_bits(n as u32));
            out.push(if *closed { 1.0 } else { 0.0 });
        }
    }
    // [15] blend_k
    out.push(bk);
    // [16-18] base albedo
    let m = mat_ref;
    out.push(m.albedo.x); out.push(m.albedo.y); out.push(m.albedo.z);
    // [19] base roughness, [20] base metallic
    out.push(m.roughness); out.push(m.metallic);
    // [21] edge_wear_amount
    let ew = &m.edge_wear;
    out.push(if ew.enabled { ew.amount } else { 0.0 });
    // [22-24] edge_wear_color
    out.push(ew.color.x); out.push(ew.color.y); out.push(ew.color.z);
    // [25-27] edge_wear roughness, metallic, sharpness
    out.push(ew.roughness); out.push(ew.metallic); out.push(ew.sharpness);
    // [28] cavity_amount
    let cd = &m.cavity_dirt;
    out.push(if cd.enabled { cd.amount } else { 0.0 });
    // [29-31] cavity_color
    out.push(cd.color.x); out.push(cd.color.y); out.push(cd.color.z);
    // [32-34] cavity roughness, metallic, sharpness
    out.push(cd.roughness); out.push(cd.metallic); out.push(cd.sharpness);
    // [35] noise_amount
    let np = &m.noise_pattern;
    out.push(if np.enabled { np.amount } else { 0.0 });
    // [36-38] noise_color
    out.push(np.color.x); out.push(np.color.y); out.push(np.color.z);
    // [39-42] noise roughness, metallic, scale, octaves
    out.push(np.roughness); out.push(np.metallic); out.push(np.scale); out.push(np.octaves as f32);
    // [43] has_texture (1.0 = use AI texture, 0.0 = procedural)
    // [44] tex_tiling_scale (world units per tile)
    // [45] bounding sphere radius (world-space, conservative) — GPU sphere cull
    // [46-47] reserved
    let s = p.scale.abs(); // Use absolute scale for correct bounds with negative/mirrored scales
    let bound_r: f32 = (match &p.shape {
        SdfShape::Sphere { radius } =>
            *radius * s.x.max(s.y).max(s.z),
        SdfShape::Box { half_extents: h, .. } =>
            (s * *h).length(),
        SdfShape::Cylinder { radius, half_height } => {
            let r = s.x.max(s.z) * *radius;
            let h = s.y * *half_height;
            (r * r + h * h).sqrt()
        },
        SdfShape::Torus { major_radius, minor_radius } =>
            s.x.max(s.z) * (*major_radius + *minor_radius),
        SdfShape::Cone { radius, half_height } => {
            let r = s.x.max(s.z) * *radius;
            let h = s.y * *half_height;
            (r * r + h * h).sqrt()
        },
        SdfShape::Capsule { radius, half_height } =>
            s.y * *half_height + s.x.max(s.z) * *radius,
        SdfShape::Stair { half_extents: h, .. } =>
            (s * *h).length(),
        SdfShape::SpiralStair { radius, half_height, .. } => {
            let r = s.x.max(s.z) * *radius;
            let h = s.y * *half_height;
            (r * r + h * h).sqrt()
        },
        SdfShape::Billboard { width, height, .. } => {
            let w = p.scale.x * *width * 0.5;
            let h = p.scale.y * *height * 0.5;
            (w * w + h * h).sqrt()
        },
        SdfShape::CurveExtrude { profile, half_depth, open, thickness, tube, .. } => {
            // OPEN (P2): ribbon/tube grows OUTWARD from the path by thickness/2 → expand the bound
            // (must match primitives.rs bounding_radius or the GPU per-prim cull drops bricks → holes).
            let max_r = profile.verts.iter().map(|v| v.pos.length()).fold(0.5f32, f32::max)
                + if *open { *thickness * 0.5 } else { 0.0 };
            let r = s.x.max(s.z) * max_r;
            let h = if *open && *tube { s.y * *thickness * 0.5 }
                    else if *half_depth > 0.001 { s.y * *half_depth }
                    else { r };
            (r * r + h * h).sqrt()
        },
        SdfShape::MeshVolume { grid, .. } => (s * grid.corner_abs_max()).length(),
        // HalfSpace: INFINITE plane cutter — its per-prim bounding-sphere must be huge or the GPU
        // bake's per-prim cull far-skips the slice and leaves stale (un-cut) geometry. The slice
        // must cut the WHOLE body, so never let it be culled.
        SdfShape::HalfSpace => 1.0e6,
        SdfShape::SpinePipe { points, radius, point_scale, .. } => {
            let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
            let max_ps = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
            let smax = s.x.max(s.y).max(s.z);
            smax * (max_pt + *radius * max_ps)
        }
    }) + bk  // bk padding covers smooth-min blend region
       + if modifier.is_active() {
            match modifier.mod_type {
                ModifierType::Twist => modifier.strength.abs() * 0.5,
                ModifierType::Taper => modifier.strength.abs() * 0.3,
                ModifierType::Smooth => modifier.strength.abs(),
                ModifierType::Shear => modifier.strength.abs() * 0.5,
                ModifierType::Sculpt => modifier.strength.abs() + modifier.mask.extents.x,
                ModifierType::Spherize => modifier.param2.max(0.5),
                ModifierType::Shell => modifier.strength.abs(),
                // Emboss pushes the surface OUTWARD by strength → must expand bound or the
                // raised relief gets culled/clipped. Deboss is inward → no outward pad (_ => 0.0).
                ModifierType::Emboss => modifier.strength.abs(),
                _ => 0.0,
            }
        } else { 0.0 }
    + p.elongation.x.max(p.elongation.y).max(p.elongation.z) * s.min_element();
    let has_tex = if textured.contains(&orig_idx) { 1.0f32 } else { 0.0 };
    // [43] has_texture, [44] tex_tiling_scale, [45] bvh_bound_r
    out.push(has_tex); out.push(2.0); out.push(bound_r);
    // [46] falloff dir.x * length, [47] falloff dir.y * length  (packed direction+length)
    let fdir = p.blend_falloff.direction.normalize_or_zero() * p.blend_falloff.length;
    out.push(fdir.x);
    out.push(fdir.y);
    // [48-59] 12 per-edge bevel radii (edges 0-11 in box_bevel_edges order)
    // [60]    packed mode bits: 2 bits × 12 edges (mode 0=none,1=round,2=chamfer)
    // [61-63] reserved
    let (edge_rs, mode_bits): ([f32; 12], u32) =
        if let SdfShape::Box { bevel, .. } = &p.shape {
            let m = |md: BevelMode| -> u32 { match md { BevelMode::None=>0, BevelMode::Round=>1, BevelMode::Chamfer=>2 } };
            let mut mb = 0u32;
            for i in 0..12 { mb |= m(bevel.modes[i]) << (i * 2); }
            // Pack up_axis into bits 24-25 (0=Y, 1=X, 2=Z)
            mb |= (bevel.up_axis as u32 & 3) << 24;
            // Pack BoxBevelAlgo into bits 26-27 (0=PerEdge, 1=Uniform)
            mb |= (bevel.algo.as_u8() as u32 & 3) << 26;
            (bevel.radii, mb)
        } else if matches!(&p.shape, SdfShape::Cylinder{..}) && p.cyl_bevel.has_any() {
            // Cylinder bevel: [48]=top, [49]=bot, rest zero. [60] bits 0-1=top mode, 2-3=bot mode.
            let m = |md: BevelMode| -> u32 { match md { BevelMode::None=>0, BevelMode::Round=>1, BevelMode::Chamfer=>2 } };
            let mut rs = [0.0f32; 12];
            rs[0] = p.cyl_bevel.top;
            rs[1] = p.cyl_bevel.bot;
            let mb = m(p.cyl_bevel.top_mode) | (m(p.cyl_bevel.bot_mode) << 2);
            (rs, mb)
        } else { ([0.0; 12], 0) };
    for &r in &edge_rs { out.push(r); }     // [48-59]
    out.push(f32::from_bits(mode_bits));                   // [60]
    out.push(f32::from_bits(stack_mflags as u32));        // [61] mirror flags (stack-aware)
    out.push(fdir.z);                                      // [62] falloff dir.z * length  (packed)
    out.push(p.blend_falloff.origin_offset);               // [63] falloff origin offset
    // [64] modifier_type
    let mod_type: u32 = match modifier.mod_type {
        ModifierType::None => 0, ModifierType::Twist => 1, ModifierType::Taper => 2,
        ModifierType::Smooth => 3, ModifierType::Displace => 4,
        ModifierType::Mirror => 5, ModifierType::Shear => 6, ModifierType::Sculpt => 7,
        ModifierType::Spherize => 8, ModifierType::Shell => 9,
        ModifierType::Emboss => 10, ModifierType::Deboss => 11,
        // Array is expanded before serialize — should not appear here.
        ModifierType::Array => 0,
    };
    out.push(f32::from_bits(mod_type));
    out.push(modifier.strength);                         // [65] modifier_strength
    out.push(f32::from_bits(modifier.axis as u32));     // [66] modifier_axis
    out.push(modifier.param2);                           // [67] modifier_param2
    // [68] mask_type
    let mask_type: u32 = match modifier.mask.shape {
        MaskShape::None => 0, MaskShape::Sphere => 1, MaskShape::Box => 2, MaskShape::Gradient => 3, MaskShape::Noise3D => 4,
    };
    out.push(f32::from_bits(mask_type));
    // [69-71] mask_center
    out.push(modifier.mask.center.x); out.push(modifier.mask.center.y); out.push(modifier.mask.center.z);
    // [72-74] mask_extents
    out.push(modifier.mask.extents.x); out.push(modifier.mask.extents.y); out.push(modifier.mask.extents.z);
    out.push(modifier.mask.feather);                     // [75] mask_feather
    out.push(f32::from_bits(if modifier.mask.invert { 1u32 } else { 0 })); // [76] mask_invert
    out.push(opacity);                                        // [77] opacity (0=transparent, 1=opaque)
    // [78-80] mirror_center xyz (stack-aware)
    out.push(stack_mcenter.x); out.push(stack_mcenter.y); out.push(stack_mcenter.z);
    // [81] layer_eval_id (0=ungrouped, >0=layer for boolean isolation)
    out.push(f32::from_bits(p.layer_id.unwrap_or(0)));
    // [82] object_eval_id (0=ungrouped, >0=object for boolean isolation)
    out.push(f32::from_bits(p.object_id.unwrap_or(0)));
    // [83] texture_id (atlas index)
    out.push(f32::from_bits(p.texture_id));
    // --- Phase B.1: Shader panel fields [84-107] ---
    let shader_t: u32 = match m.shader_type {
        ShaderType::StandardPBR=>0, ShaderType::Subsurface=>1, ShaderType::Glass=>2, ShaderType::VFX=>3, ShaderType::Advanced=>4,
    };
    out.push(f32::from_bits(shader_t));                // [84] shader_type
    let mq: u32 = match m.mat_quality {
        MaterialQuality::Optimize=>0, MaterialQuality::Normal=>1, MaterialQuality::God=>2,
    };
    out.push(f32::from_bits(mq));                  // [85] mat_quality
    out.push(m.normal_strength);          // [86] normal_strength
    out.push(m.emission_color.x);         // [87] emission_r
    out.push(m.emission_color.y);         // [88] emission_g
    out.push(m.emission_color.z);         // [89] emission_b
    out.push(m.emission_intensity);       // [90] emission_intensity
    out.push(m.sss_radius);               // [91] sss_radius
    out.push(m.sss_color.x);              // [92] sss_r
    out.push(m.sss_color.y);              // [93] sss_g
    out.push(m.sss_color.z);              // [94] sss_b
    out.push(m.ior);                      // [95] ior
    out.push(m.glass_tint.x);             // [96] glass_tint_r
    out.push(m.glass_tint.y);             // [97] glass_tint_g
    out.push(m.glass_tint.z);             // [98] glass_tint_b
    out.push(m.clearcoat);                // [99] clearcoat
    out.push(m.clearcoat_roughness);      // [100] clearcoat_roughness
    out.push(m.anisotropy);               // [101] anisotropy
    out.push(m.anisotropy_rotation);      // [102] anisotropy_rotation
    out.push(m.sheen);                    // [103] sheen
    out.push(m.sheen_tint);               // [104] sheen_tint
    out.push(f32::from_bits(p.subdivision as u32)); // [105] subdivision level (0-6)
    let effective_profile = profile_override.unwrap_or(p.blend_profile as u32);
    out.push(f32::from_bits(effective_profile)); // [106] blend_profile (0=Polynomial,1=Chamfer; seam ops: 0=V,1=U,2=Square)
    out.push(if p.node_graph.is_some() { 1.0 } else { 0.0 }); // [107] has_node_graph
    // [108-115] Noise mask parameters
    out.push(modifier.mask.noise_frequency);    // [108]
    out.push(f32::from_bits(modifier.mask.noise_octaves)); // [109]
    out.push(modifier.mask.noise_lacunarity);   // [110]
    out.push(modifier.mask.noise_gain);          // [111]
    out.push(modifier.mask.noise_threshold);     // [112]
    out.push(modifier.mask.noise_offset.x);     // [113]
    out.push(modifier.mask.noise_offset.y);     // [114]
    out.push(modifier.mask.noise_offset.z);     // [115]

    // --- Material DNA [116..279] ---
    let dna = &p.material_dna;
    let visible_rules: Vec<_> = dna.rules.iter().filter(|r| r.visible).collect();
    let rule_count = visible_rules.len().min(DNA_MAX_RULES);
    out.push(rule_count as f32);                   // [116] dna_rule_count
    out.push(dna.age);                             // [117] dna_age
    out.push(p.bevel_profile_top);
    out.push(p.bevel_profile_bot);
    // Write rules (20 floats each)  [120..279]
    for i in 0..DNA_MAX_RULES {
        if i < rule_count {
            let floats = visible_rules[i].to_gpu_floats();
            for &v in &floats { out.push(v); }
        } else {
            for _ in 0..DNA_FLOATS_PER_RULE { out.push(0.0); }
        }
    }
    // [280] bevel_profile_vert, [281-283] chamfer angles (degrees)
    out.push(p.bevel_profile_vert);
    out.push(p.chamfer_angle_top);
    out.push(p.chamfer_angle_bot);
    out.push(p.chamfer_angle_vert);
    // [284-287] blend-zone noise OR multipoint curve LUT (when noise amp≈0)
    if p.blend_noise_amp > 0.001 {
        out.push(p.blend_noise_amp);
        out.push(p.blend_noise_freq);
        out.push(f32::from_bits(p.blend_noise_octaves));
        out.push(f32::from_bits(p.blend_noise_type));
    } else {
        let lut = p.blend_curve.to_lut4();
        out.push(lut[0]);
        out.push(lut[1]);
        out.push(lut[2]);
        out.push(lut[3]);
    }
    // [288-290] elongation xyz
    out.push(p.elongation.x); out.push(p.elongation.y); out.push(p.elongation.z);
    // [291] seam depth (op 5/6) / extrude SIGNED strength (op 7); 0.0 otherwise.
    // For normal blend ops: wall_protect encoded as axis*100 + margin (axis 0..3, margin < 99).
    out.push(match &p.operation {
        BooleanOp::Seam { depth, .. } => *depth,
        BooleanOp::Extrude { strength, .. } => *strength,
        _ => {
            let m = p.blend_falloff.wall_protect.clamp(0.0, 99.0);
            let a = p.blend_falloff.wall_protect_axis.min(3) as f32;
            a * 100.0 + m
        }
    });
    // [292] height-map atlas start (T1 live march). Pack when atlas provided + image Displace.
    let hm_start = if let Some(atlas) = height_atlas {
        if modifier.mod_type == ModifierType::Displace {
            if let Some(ref hm) = modifier.height_map {
                if hm.w > 0 && hm.h > 0 && !hm.pixels.is_empty() {
                    pack_height_map_into(hm, modifier.axis, atlas)
                } else {
                    HEIGHT_MAP_NONE
                }
            } else {
                HEIGHT_MAP_NONE
            }
        } else {
            HEIGHT_MAP_NONE
        }
    } else {
        HEIGHT_MAP_NONE
    };
    out.push(f32::from_bits(hm_start));
    debug_assert_eq!(out.len() % FLOATS_PER_PRIM, 0, "full prim size mismatch");
}

/// Pack one image height map into the render-path atlas; returns float index of the header.
/// Layout matches bake / `sample_height_map` in volume_bake + sdf_prim_eval.
fn pack_height_map_into(hm: &core_math::modifiers::DisplaceHeightMap, axis: u8, atlas: &mut Vec<f32>) -> u32 {
    let start = atlas.len() as u32;
    atlas.push(f32::from_bits(hm.w));
    atlas.push(f32::from_bits(hm.h));
    atlas.push(f32::from_bits(axis as u32));
    atlas.push(0.0);
    let n = (hm.w as usize).saturating_mul(hm.h as usize);
    for i in 0..n {
        let px = hm.pixels.get(i).copied().unwrap_or(128) as f32 / 255.0;
        atlas.push(px);
    }
    start
}

// ============================================================
// HOT / COLD PRIM BUFFER SPLIT (Sprint 3)
// ============================================================
//
// The monolithic 288-float prim buffer is split into:
//   HOT (80 floats) — read by march kernel + volume bake (SDF evaluation + basic material preview)
//   COLD (200 floats) — read only by deferred shader (full material, DNA rules)
//
// Scope markers (SCOPE_OPEN/CLOSE) exist only in the hot buffer.
// The cold buffer is indexed by surface_id after hit — no scope concept needed.
//
// HOT FIELD MAP (new_idx → old_idx → purpose)
// ──────────────────────────────────────────────────
// hot[0]  = old[0]   = position.x
// hot[1]  = old[1]   = position.y
// hot[2]  = old[2]   = position.z
// hot[3]  = old[3]   = shape_type | selected | stair_steps (packed u32)
// hot[4]  = old[4]   = quaternion.x
// hot[5]  = old[5]   = quaternion.y
// hot[6]  = old[6]   = quaternion.z
// hot[7]  = old[7]   = quaternion.w
// hot[8]  = old[8]   = scale.x
// hot[9]  = old[9]   = scale.y
// hot[10] = old[10]  = scale.z
// hot[11] = old[11]  = op_type (u32: 0=Union 1=Subtract 2=Intersect/Smooth 3/4=Chamfer 5/6=Seam cut/raised 7=Extrude)
// hot[12] = old[12]  = shape_param_0
// hot[13] = old[13]  = shape_param_1
// hot[14] = old[14]  = shape_param_2
// hot[15] = old[15]  = blend_k
// --- march preview material (raymarch_march reads these) ---
// hot[16] = old[16]  = albedo.r
// hot[17] = old[17]  = albedo.g
// hot[18] = old[18]  = albedo.b
// hot[19] = old[19]  = roughness
// hot[20] = old[20]  = metallic
// hot[21] = old[77]  = opacity
// hot[22] = old[84]  = shader_type (u32)
// hot[23] = old[87]  = emission_color.r
// hot[24] = old[88]  = emission_color.g
// hot[25] = old[89]  = emission_color.b
// hot[26] = old[90]  = emission_intensity
// --- SDF modifiers & bevel ---
// hot[27] = old[45]  = bound_r
// hot[28] = old[46]  = falloff_dir.x * length
// hot[29] = old[47]  = falloff_dir.y * length
// hot[30] = old[62]  = falloff_dir.z * length
// hot[31] = old[63]  = falloff_origin_offset
// hot[32..43] = old[48..59] = bevel_radii[0..11]
// hot[44] = old[60]  = bevel_modes (packed u32)
// hot[45] = old[61]  = mirror_flags (u32)
// hot[46] = old[78]  = mirror_center.x
// hot[47] = old[79]  = mirror_center.y
// hot[48] = old[80]  = mirror_center.z
// hot[49] = old[64]  = modifier_type (u32)
// hot[50] = old[65]  = modifier_strength
// hot[51] = old[66]  = modifier_axis (u32)
// hot[52] = old[67]  = modifier_param2
// hot[53] = old[68]  = mask_type (u32)
// hot[54] = old[69]  = mask_center.x
// hot[55] = old[70]  = mask_center.y
// hot[56] = old[71]  = mask_center.z
// hot[57] = old[72]  = mask_extents.x
// hot[58] = old[73]  = mask_extents.y
// hot[59] = old[74]  = mask_extents.z
// hot[60] = old[75]  = mask_feather
// hot[61] = old[76]  = mask_invert (u32)
// hot[62] = old[108] = mask_noise_frequency
// hot[63] = old[109] = mask_noise_octaves (u32)
// hot[64] = old[110] = mask_noise_lacunarity
// hot[65] = old[111] = mask_noise_gain
// hot[66] = old[112] = mask_noise_threshold
// hot[67] = old[113] = mask_noise_offset.x
// hot[68] = old[114] = mask_noise_offset.y
// hot[69] = old[115] = mask_noise_offset.z
// hot[70] = old[81]  = layer_eval_id (u32)
// hot[71] = old[82]  = object_eval_id (u32)
// hot[72] = old[105] = subdivision (u32)
// hot[73] = old[106] = blend_profile (u32)
// hot[74] = body_index (u32) — march path; Lipschitz constant (f32) — bake path
// hot[75] = bevel_profile_top (f32, -1..+1)
// hot[76] = bevel_profile_bot (f32, -1..+1)
// hot[77] = bevel_profile_vert (f32, -1..+1)
// hot[78] = chamfer_angle_top (degrees, default 45)
// hot[79] = chamfer_angle_bot (degrees, default 45)
// hot[80] = blend_noise_amp (f32, default 0.0)
// hot[81] = blend_noise_freq (f32, default 20.0)
// hot[82] = blend_noise_octaves (u32, default 1)
// hot[83] = blend_noise_type (u32, default 0)
// --- clip curve index header ---
// hot[84] = clip_curve_packed (u32: bit0=has_clips, bits16-31=clip_count)
// hot[85] = clip_buf_start (u32 via from_bits: index into separate clip_curves buffer)
// hot[86] = seam depth (op 5/6) / extrude SIGNED strength (op 7); 0.0 otherwise — legacy layout twin is [291]
// hot[87] = wall_protect encoded as axis*100 + margin (0=off, axis 0=Auto..3=Z)
// Actual clip polygon data lives in @binding(4) clip_curves buffer.
//
// COLD FIELD MAP (new_idx → old_idx → purpose)
// ──────────────────────────────────────────────────
// cold[0]  = old[16]  = albedo.r  (duplicated from hot for deferred convenience)
// cold[1]  = old[17]  = albedo.g
// cold[2]  = old[18]  = albedo.b
// cold[3]  = old[19]  = roughness
// cold[4]  = old[20]  = metallic
// cold[5]  = old[21]  = edge_wear_amount
// cold[6]  = old[22]  = edge_wear_color.r
// cold[7]  = old[23]  = edge_wear_color.g
// cold[8]  = old[24]  = edge_wear_color.b
// cold[9]  = old[25]  = edge_wear_roughness
// cold[10] = old[26]  = edge_wear_metallic
// cold[11] = old[27]  = edge_wear_sharpness
// cold[12] = old[28]  = cavity_dirt_amount
// cold[13] = old[29]  = cavity_dirt_color.r
// cold[14] = old[30]  = cavity_dirt_color.g
// cold[15] = old[31]  = cavity_dirt_color.b
// cold[16] = old[32]  = cavity_dirt_roughness
// cold[17] = old[33]  = cavity_dirt_metallic
// cold[18] = old[34]  = cavity_dirt_sharpness
// cold[19] = old[35]  = noise_pattern_amount
// cold[20] = old[36]  = noise_pattern_color.r
// cold[21] = old[37]  = noise_pattern_color.g
// cold[22] = old[38]  = noise_pattern_color.b
// cold[23] = old[39]  = noise_pattern_roughness
// cold[24] = old[40]  = noise_pattern_metallic
// cold[25] = old[41]  = noise_pattern_scale
// cold[26] = old[42]  = noise_pattern_octaves
// cold[27] = old[43]  = has_texture
// cold[28] = old[44]  = tex_tiling_scale
// cold[29] = old[77]  = opacity
// cold[30] = old[83]  = texture_id (u32)
// cold[31] = old[84]  = shader_type (u32)
// cold[32] = old[85]  = mat_quality (u32)
// cold[33] = old[86]  = normal_strength
// cold[34] = old[87]  = emission_color.r
// cold[35] = old[88]  = emission_color.g
// cold[36] = old[89]  = emission_color.b
// cold[37] = old[90]  = emission_intensity
// cold[38] = old[91]  = sss_radius
// cold[39] = old[92]  = sss_color.r
// cold[40] = old[93]  = sss_color.g
// cold[41] = old[94]  = sss_color.b
// cold[42] = old[95]  = ior
// cold[43] = old[96]  = glass_tint.r
// cold[44] = old[97]  = glass_tint.g
// cold[45] = old[98]  = glass_tint.b
// cold[46] = old[99]  = clearcoat
// cold[47] = old[100] = clearcoat_roughness
// cold[48] = old[101] = anisotropy
// cold[49] = old[102] = anisotropy_rotation
// cold[50] = old[103] = sheen
// cold[51] = old[104] = sheen_tint
// cold[52] = old[107] = has_node_graph
// cold[53..55] = padding (zero)
// cold[56] = old[116] = dna_rule_count
// cold[57] = old[117] = dna_age
// cold[58..59] = reserved
// cold[60..219] = DNA rules: 8 × 20 floats = 160 floats (old[120..279])
// cold[220..223] = padding (zero) — round to 224

/// Hot buffer stride: fields needed by march kernel + volume bake.
/// Clip curve polygon data lives in a SEPARATE storage buffer (binding 4),
/// not inline in the HOT buffer. hot[84-87] stores only the index/header.
/// hot[92] = height-map atlas start (u32 bits; 0xFFFFFFFF = none) — T1 image displace.
pub const HOT_FLOATS_PER_PRIM: usize = 93;
/// Sentinel for hot[92]: no image height map (use FBM displace).
pub const HEIGHT_MAP_NONE: u32 = 0xFFFF_FFFF;
/// Cold buffer stride: material/DNA fields for deferred shading.
pub const COLD_FLOATS_PER_PRIM: usize = 224;

/// Serialize only the hot fields (SDF evaluation + basic material preview) for a single prim.
/// Layout: see HOT FIELD MAP comment above.
fn serialize_single_prim_hot(
    p: &SdfPrimitive,
    orig_idx: usize,
    selected: &HashSet<usize>,
    ovr: Option<&RenderPrimOverride>,
    body_index: u32,
    mat_override: Option<&PbrMaterial>,
    out: &mut Vec<f32>,
) {
    let pos = ovr.and_then(|o| o.position).unwrap_or(p.position);
    // P3: stack-aware single-slot packing for GPU bake (multi-mod → CPU path).
    let stack_mod = p.gpu_slot_modifier();
    let modifier = ovr.and_then(|o| o.modifier.as_ref()).unwrap_or(&stack_mod);
    let (stack_mflags, stack_mcenter) = p.gpu_mirror_state();
    let mat_ref = mat_override.unwrap_or(&p.material);
    let opacity = ovr.and_then(|o| o.opacity).unwrap_or(mat_ref.opacity);

    let stair_steps: Option<u32> = match &p.shape {
        SdfShape::Stair { num_steps, .. } => Some(*num_steps),
        SdfShape::SpiralStair { num_steps, .. } => Some(*num_steps),
        _ => None,
    };

    // hot[0-2] position
    out.push(pos.x); out.push(pos.y); out.push(pos.z);
    // hot[3] shape_type | selected | stair_steps
    let mut st: u32 = match &p.shape {
        SdfShape::Sphere{..}=>0, SdfShape::Box{..}=>1, SdfShape::Cylinder{..}=>2,
        SdfShape::Torus{..}=>3, SdfShape::Cone{..}=>4, SdfShape::Capsule{..}=>5,
        SdfShape::Stair{..}=>6, SdfShape::SpiralStair{..}=>7, SdfShape::Billboard{..}=>8,
        SdfShape::CurveExtrude{..}=>9, SdfShape::MeshVolume{..}=>10, SdfShape::HalfSpace=>11,
        SdfShape::SpinePipe{..}=>12,
    };
    if selected.contains(&orig_idx) { st |= 256; }
    if let Some(steps) = stair_steps { st |= (steps & 0xFFFF) << 16; }
    out.push(f32::from_bits(st));
    // hot[4-7] quaternion
    let q = if let Some(rot) = ovr.and_then(|o| o.rotation_deg) {
        glam::Quat::from_euler(glam::EulerRot::YXZ,
            rot.y.to_radians(), rot.x.to_radians(), rot.z.to_radians())
    } else {
        p.rotation_quat()
    };
    out.push(q.x); out.push(q.y); out.push(q.z); out.push(q.w);
    // hot[8-10] scale (with optional body_scale override)
    let scl = ovr.and_then(|o| o.scale).unwrap_or(p.scale);
    out.push(scl.x); out.push(scl.y); out.push(scl.z);
    // hot[11] op_type (5=Seam cut, 6=Seam raised: k slot = width, profile slot = cross-section,
    // hot[86] = depth; 7=Extrude: k slot = width, hot[86] = SIGNED strength — slot overloaded,
    // an op is either Seam or Extrude)
    let (op_type, bk, profile_override): (u32, f32, Option<u32>) = match &p.operation {
        BooleanOp::Union          { k } => (0, *k, None),
        BooleanOp::Subtract       { k } => (1, *k, None),
        BooleanOp::Intersect           => (2, 0.0, None),
        BooleanOp::ChamferUnion    { k } => (3, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::ChamferSubtract { k } => (4, *k, Some(BlendProfile::Chamfer as u32)),
        // Smooth/Chamfer intersect: op_type 2 (intersect) + k drives smax; profile picks the shape.
        BooleanOp::SmoothIntersect { k } => (2, *k, None),
        BooleanOp::Seam { width, profile, raised, .. } => (5 + *raised as u32, *width, Some(*profile as u32)),
        BooleanOp::Extrude { width, .. } => (7, *width, None),
    };
    out.push(f32::from_bits(op_type));
    // hot[12-14] shape params
    match &p.shape {
        SdfShape::Sphere{radius}=>{out.push(*radius);out.push(0.0);out.push(0.0);}
        SdfShape::Box{half_extents,..}=>{out.push(half_extents.x);out.push(half_extents.y);out.push(half_extents.z);}
        SdfShape::Cylinder{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Torus{major_radius,minor_radius}=>{out.push(*major_radius);out.push(*minor_radius);out.push(0.0);}
        SdfShape::Cone{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Capsule{radius,half_height}=>{out.push(*radius);out.push(*half_height);out.push(0.0);}
        SdfShape::Stair{half_extents,..}=>{out.push(half_extents.x);out.push(half_extents.y);out.push(half_extents.z);}
        SdfShape::SpiralStair{radius,half_height,step_width,..}=>{out.push(*radius);out.push(*half_height);out.push(*step_width);}
        SdfShape::Billboard{width,height,face_camera}=>{out.push(*width);out.push(*height);out.push(if *face_camera {1.0} else {0.0});}
        // [12]=half_depth, [13]=wall_t (shell), [14]=rim (extrude-cap bevel; sign=mode, mag=radius).
        SdfShape::CurveExtrude{half_depth,wall_t,rim,..}=>{out.push(*half_depth);out.push(*wall_t);out.push(*rim);}
        // MeshVolume: grid header (res/bounds/cell) rides in the side buffer. params[12-14]
        // = conservative half-extent so the ANALYTICAL path (prim_outline ghost, march) can
        // draw a bounding-box silhouette — it can't sample the grid (bake path does). Bake
        // ignores these (its main loop overrides st==10 with eval_mesh_volume).
        SdfShape::MeshVolume{grid, ..}=>{let he=grid.corner_abs_max(); out.push(he.x);out.push(he.y);out.push(he.z);}
        // HalfSpace: infinite plane, no shape params — the transform (rotation+position) is the plane.
        SdfShape::HalfSpace=>{out.push(0.0);out.push(0.0);out.push(0.0);}
        // SpinePipe: radius + point count + closed flag; points ride the clip side buffer.
        SdfShape::SpinePipe{points,radius,closed,..}=>{
            out.push(*radius);
            let n = points.len().min(core_math::curves::MAX_SPINE_POINTS);
            out.push(f32::from_bits(n as u32));
            out.push(if *closed { 1.0 } else { 0.0 });
        }
    }
    // hot[15] blend_k
    out.push(bk);
    // hot[16-18] albedo (march preview)
    out.push(mat_ref.albedo.x); out.push(mat_ref.albedo.y); out.push(mat_ref.albedo.z);
    // hot[19] roughness, hot[20] metallic
    out.push(mat_ref.roughness); out.push(mat_ref.metallic);
    // hot[21] opacity
    out.push(opacity);
    // hot[22] shader_type
    let shader_t: u32 = match mat_ref.shader_type {
        ShaderType::StandardPBR=>0, ShaderType::Subsurface=>1, ShaderType::Glass=>2, ShaderType::VFX=>3, ShaderType::Advanced=>4,
    };
    out.push(f32::from_bits(shader_t));
    // hot[23-26] emission
    out.push(mat_ref.emission_color.x); out.push(mat_ref.emission_color.y); out.push(mat_ref.emission_color.z);
    out.push(mat_ref.emission_intensity);
    // hot[27] bound_r
    let s = p.scale.abs();
    let bound_r: f32 = (match &p.shape {
        SdfShape::Sphere { radius } => *radius * s.x.max(s.y).max(s.z),
        SdfShape::Box { half_extents: h, .. } => (s * *h).length(),
        SdfShape::Cylinder { radius, half_height } => { let r = s.x.max(s.z) * *radius; let h = s.y * *half_height; (r*r+h*h).sqrt() },
        SdfShape::Torus { major_radius, minor_radius } => s.x.max(s.z) * (*major_radius + *minor_radius),
        SdfShape::Cone { radius, half_height } => { let r = s.x.max(s.z) * *radius; let h = s.y * *half_height; (r*r+h*h).sqrt() },
        SdfShape::Capsule { radius, half_height } => s.y * *half_height + s.x.max(s.z) * *radius,
        SdfShape::Stair { half_extents: h, .. } => (s * *h).length(),
        SdfShape::SpiralStair { radius, half_height, .. } => { let r = s.x.max(s.z) * *radius; let h = s.y * *half_height; (r*r+h*h).sqrt() },
        SdfShape::Billboard { width, height, .. } => { let w = p.scale.x * *width * 0.5; let h = p.scale.y * *height * 0.5; (w*w+h*h).sqrt() },
        SdfShape::CurveExtrude { profile, half_depth, open, thickness, tube, .. } => { let mr = profile.verts.iter().map(|v| v.pos.length()).fold(0.5f32, f32::max) + if *open { *thickness * 0.5 } else { 0.0 }; let r = s.x.max(s.z) * mr; let h = if *open && *tube { s.y * *thickness * 0.5 } else if *half_depth > 0.001 { s.y * *half_depth } else { r }; (r*r+h*h).sqrt() },
        SdfShape::MeshVolume { grid, .. } => (s * grid.corner_abs_max()).length(),
        // HalfSpace: infinite plane cutter — huge bound so the per-prim cull never far-skips the slice.
        SdfShape::HalfSpace => 1.0e6,
        SdfShape::SpinePipe { points, radius, point_scale, .. } => {
            let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
            let max_ps = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
            let smax = s.x.max(s.y).max(s.z);
            smax * (max_pt + *radius * max_ps)
        }
    }) + bk + if modifier.is_active() {
        match modifier.mod_type {
            ModifierType::Twist => modifier.strength.abs() * 0.5,
            ModifierType::Taper => modifier.strength.abs() * 0.3,
            ModifierType::Smooth => modifier.strength.abs(),
            ModifierType::Shear => modifier.strength.abs() * 0.5,
            ModifierType::Sculpt => modifier.strength.abs() + modifier.mask.extents.x,
            ModifierType::Spherize => modifier.param2.max(0.5),
            ModifierType::Shell => modifier.strength.abs(),
            // Emboss pushes OUTWARD → expand bound (Deboss inward → 0).
            ModifierType::Emboss => modifier.strength.abs(),
            _ => 0.0,
        }
    } else { 0.0 }
    + p.elongation.x.max(p.elongation.y).max(p.elongation.z) * s.min_element();
    out.push(bound_r);
    // hot[28-31] falloff
    let fdir = p.blend_falloff.direction.normalize_or_zero() * p.blend_falloff.length;
    out.push(fdir.x); out.push(fdir.y); out.push(fdir.z);
    out.push(p.blend_falloff.origin_offset);
    // hot[32-43] bevel radii [0..11]
    let (edge_rs, mode_bits): ([f32; 12], u32) =
        if let SdfShape::Box { bevel, .. } = &p.shape {
            let m = |md: BevelMode| -> u32 { match md { BevelMode::None=>0, BevelMode::Round=>1, BevelMode::Chamfer=>2 } };
            let mut mb = 0u32;
            for i in 0..12 { mb |= m(bevel.modes[i]) << (i * 2); }
            mb |= (bevel.up_axis as u32 & 3) << 24;
            mb |= (bevel.algo.as_u8() as u32 & 3) << 26;
            (bevel.radii, mb)
        } else if matches!(&p.shape, SdfShape::Cylinder{..}) && p.cyl_bevel.has_any() {
            let m = |md: BevelMode| -> u32 { match md { BevelMode::None=>0, BevelMode::Round=>1, BevelMode::Chamfer=>2 } };
            let mut rs = [0.0f32; 12]; rs[0] = p.cyl_bevel.top; rs[1] = p.cyl_bevel.bot;
            let mb = m(p.cyl_bevel.top_mode) | (m(p.cyl_bevel.bot_mode) << 2);
            (rs, mb)
        } else { ([0.0; 12], 0) };
    for &r in &edge_rs { out.push(r); }
    // hot[44] bevel modes
    out.push(f32::from_bits(mode_bits));
    // hot[45] mirror_flags, hot[46-48] mirror_center (stack-aware)
    out.push(f32::from_bits(stack_mflags as u32));
    out.push(stack_mcenter.x); out.push(stack_mcenter.y); out.push(stack_mcenter.z);
    // hot[49-52] modifier
    let mod_type: u32 = match modifier.mod_type {
        ModifierType::None=>0, ModifierType::Twist=>1, ModifierType::Taper=>2,
        ModifierType::Smooth=>3, ModifierType::Displace=>4,
        ModifierType::Mirror=>5, ModifierType::Shear=>6, ModifierType::Sculpt=>7,
        ModifierType::Spherize=>8, ModifierType::Shell=>9,
        ModifierType::Emboss=>10, ModifierType::Deboss=>11,
        ModifierType::Array => 0, // expanded before serialize
    };
    out.push(f32::from_bits(mod_type));
    out.push(modifier.strength);
    out.push(f32::from_bits(modifier.axis as u32));
    out.push(modifier.param2);
    // hot[53-61] mask
    let mask_type: u32 = match modifier.mask.shape {
        MaskShape::None=>0, MaskShape::Sphere=>1, MaskShape::Box=>2, MaskShape::Gradient=>3, MaskShape::Noise3D=>4,
    };
    out.push(f32::from_bits(mask_type));
    out.push(modifier.mask.center.x); out.push(modifier.mask.center.y); out.push(modifier.mask.center.z);
    out.push(modifier.mask.extents.x); out.push(modifier.mask.extents.y); out.push(modifier.mask.extents.z);
    out.push(modifier.mask.feather);
    out.push(f32::from_bits(if modifier.mask.invert { 1u32 } else { 0 }));
    // hot[62-69] noise mask
    out.push(modifier.mask.noise_frequency);
    out.push(f32::from_bits(modifier.mask.noise_octaves));
    out.push(modifier.mask.noise_lacunarity);
    out.push(modifier.mask.noise_gain);
    out.push(modifier.mask.noise_threshold);
    out.push(modifier.mask.noise_offset.x); out.push(modifier.mask.noise_offset.y); out.push(modifier.mask.noise_offset.z);
    // hot[70-71] layer/object eval id
    out.push(f32::from_bits(p.layer_id.unwrap_or(0)));
    out.push(f32::from_bits(p.object_id.unwrap_or(0)));
    // hot[72] subdivision, hot[73] blend_profile (seam ops: cross-section 0=V,1=U,2=Square)
    out.push(f32::from_bits(p.subdivision as u32));
    let effective_profile = profile_override.unwrap_or(p.blend_profile as u32);
    out.push(f32::from_bits(effective_profile));
    // hot[74] body_index (per-frame 1-based, for silhouette outline)
    out.push(f32::from_bits(body_index));
    // hot[75-77] per-group bevel profiles (-1..+1, 0=chamfer)
    out.push(p.bevel_profile_top);
    out.push(p.bevel_profile_bot);
    out.push(p.bevel_profile_vert);
    // hot[78] chamfer_angle_top, hot[79] chamfer_angle_bot (degrees; vert not in HOT — defaults 45)
    out.push(p.chamfer_angle_top);
    out.push(p.chamfer_angle_bot);
    // hot[80-83] blend-zone noise OR multipoint curve LUT (when noise amp≈0)
    if p.blend_noise_amp > 0.001 {
        out.push(p.blend_noise_amp);
        out.push(p.blend_noise_freq);
        out.push(f32::from_bits(p.blend_noise_octaves));
        out.push(f32::from_bits(p.blend_noise_type));
    } else {
        let lut = p.blend_curve.to_lut4();
        out.push(lut[0]);
        out.push(lut[1]);
        out.push(lut[2]);
        out.push(lut[3]);
    }
    // hot[84-87] clip curve index header.
    // Clip curve polygon data lives in a separate storage buffer (binding 4).
    // hot[84] = packed u32: bit0=has_clips, bits1-2=reserved, bits16-31=clip_count
    // hot[85] = clip_buf_start: float index into the separate clip curve buffer (u32 via from_bits)
    // hot[86-87] = reserved
    // Actual clip data is serialized by serialize_clip_curves_for_bake().
    let clip_count = p.clip_curves.iter().filter(|c| c.is_valid()).count() as u32;
    let has_clips = clip_count > 0 || matches!(p.shape, SdfShape::CurveExtrude { .. });
    let cc_packed: u32 = (has_clips as u32)
        | ((clip_count & 0xFFFF) << 16);
    out.push(f32::from_bits(cc_packed));
    out.push(0.0); // hot[85] clip_buf_start — patched later by serialize_clip_curves_for_bake
    // hot[86] seam depth (op 5/6) / extrude SIGNED strength (op 7); 0.0 otherwise — was reserved
    out.push(match &p.operation {
        BooleanOp::Seam { depth, .. } => *depth,
        BooleanOp::Extrude { strength, .. } => *strength,
        _ => 0.0,
    });
    // hot[87] protect-walls: axis*100 + margin (axis 0=Auto..3=Z, margin < 99). 0 = off.
    {
        let m = p.blend_falloff.wall_protect.clamp(0.0, 99.0);
        let a = p.blend_falloff.wall_protect_axis.min(3) as f32;
        out.push(a * 100.0 + m);
    }
    // hot[88-90] elongation xyz
    out.push(p.elongation.x); out.push(p.elongation.y); out.push(p.elongation.z);
    // hot[91] blend profile shape handle (0.25..4)
    out.push(p.blend_profile_shape.clamp(0.25, 4.0));
    // hot[92] height-map atlas start — patched by serialize_height_maps_for_bake
    out.push(f32::from_bits(HEIGHT_MAP_NONE));
    debug_assert_eq!(out.len() % HOT_FLOATS_PER_PRIM, 0, "hot prim size mismatch");
}

/// Serialize only the cold fields (material/DNA) for a single prim.
/// Layout: see COLD FIELD MAP comment above.
fn serialize_single_prim_cold(
    p: &SdfPrimitive,
    orig_idx: usize,
    textured: &HashSet<usize>,
    ovr: Option<&RenderPrimOverride>,
    mat_override: Option<&PbrMaterial>,
    out: &mut Vec<f32>,
) {
    let mat_ref = mat_override.unwrap_or(&p.material);
    let opacity = ovr.and_then(|o| o.opacity).unwrap_or(mat_ref.opacity);
    let m = mat_ref;
    let ew = &m.edge_wear;
    let cd = &m.cavity_dirt;
    let np = &m.noise_pattern;
    let dna = &p.material_dna;

    // cold[0-4] base material (duplicated from hot for deferred convenience)
    out.push(m.albedo.x); out.push(m.albedo.y); out.push(m.albedo.z);
    out.push(m.roughness); out.push(m.metallic);
    // cold[5-11] edge wear
    out.push(if ew.enabled { ew.amount } else { 0.0 });
    out.push(ew.color.x); out.push(ew.color.y); out.push(ew.color.z);
    out.push(ew.roughness); out.push(ew.metallic); out.push(ew.sharpness);
    // cold[12-18] cavity dirt
    out.push(if cd.enabled { cd.amount } else { 0.0 });
    out.push(cd.color.x); out.push(cd.color.y); out.push(cd.color.z);
    out.push(cd.roughness); out.push(cd.metallic); out.push(cd.sharpness);
    // cold[19-26] noise pattern
    out.push(if np.enabled { np.amount } else { 0.0 });
    out.push(np.color.x); out.push(np.color.y); out.push(np.color.z);
    out.push(np.roughness); out.push(np.metallic); out.push(np.scale); out.push(np.octaves as f32);
    // cold[27-28] texture
    let has_tex = if textured.contains(&orig_idx) { 1.0f32 } else { 0.0 };
    out.push(has_tex); out.push(2.0);
    // cold[29] opacity
    out.push(opacity);
    // cold[30] texture_id, cold[31] shader_type, cold[32] mat_quality
    out.push(f32::from_bits(p.texture_id));
    let shader_t: u32 = match m.shader_type {
        ShaderType::StandardPBR=>0, ShaderType::Subsurface=>1, ShaderType::Glass=>2, ShaderType::VFX=>3, ShaderType::Advanced=>4,
    };
    out.push(f32::from_bits(shader_t));
    let mq: u32 = match m.mat_quality {
        MaterialQuality::Optimize=>0, MaterialQuality::Normal=>1, MaterialQuality::God=>2,
    };
    out.push(f32::from_bits(mq));
    // cold[33] normal_strength
    out.push(m.normal_strength);
    // cold[34-37] emission
    out.push(m.emission_color.x); out.push(m.emission_color.y); out.push(m.emission_color.z);
    out.push(m.emission_intensity);
    // cold[38-41] subsurface
    out.push(m.sss_radius); out.push(m.sss_color.x); out.push(m.sss_color.y); out.push(m.sss_color.z);
    // cold[42-45] glass
    out.push(m.ior); out.push(m.glass_tint.x); out.push(m.glass_tint.y); out.push(m.glass_tint.z);
    // cold[46-51] clearcoat, anisotropy, sheen
    out.push(m.clearcoat); out.push(m.clearcoat_roughness);
    out.push(m.anisotropy); out.push(m.anisotropy_rotation);
    out.push(m.sheen); out.push(m.sheen_tint);
    // cold[52] has_node_graph
    out.push(if p.node_graph.is_some() { 1.0 } else { 0.0 });
    // cold[53-55] padding
    out.push(0.0); out.push(0.0); out.push(0.0);
    // cold[56-59] DNA header
    let visible_rules: Vec<_> = dna.rules.iter().filter(|r| r.visible).collect();
    let rule_count = visible_rules.len().min(DNA_MAX_RULES);
    out.push(rule_count as f32);  // cold[56]
    out.push(dna.age);            // cold[57]
    out.push(0.0); out.push(0.0); // cold[58-59] reserved
    // cold[60-219] DNA rules: 8 × 20 floats
    for i in 0..DNA_MAX_RULES {
        if i < rule_count {
            let floats = visible_rules[i].to_gpu_floats();
            for &v in &floats { out.push(v); }
        } else {
            for _ in 0..DNA_FLOATS_PER_RULE { out.push(0.0); }
        }
    }
    // cold[220-223] padding
    for _ in 0..4 { out.push(0.0); }
    debug_assert_eq!(out.len() % COLD_FLOATS_PER_PRIM, 0, "cold prim size mismatch");
}

/// Emit a SCOPE_OPEN marker into the hot buffer.
/// Uses HOT_FLOATS_PER_PRIM stride — only fields [3],[11],[15],[73] are meaningful.
/// Fields [0-2] and [8-10] store AABB for scope-skip.
fn emit_scope_open_hot(out: &mut Vec<f32>, op: &BooleanOp, blend_profile: u32, aabb_min: [f32; 3], aabb_max: [f32; 3], mirror_flags: u8, mirror_sign: u8, mirror_center: [f32; 3], bevel_radius: f32) {
    let start = out.len();
    out.resize(start + HOT_FLOATS_PER_PRIM, 0.0);
    // [0-2] AABB min
    out[start + 0] = aabb_min[0]; out[start + 1] = aabb_min[1]; out[start + 2] = aabb_min[2];
    // [3] shape_type = 254 (SCOPE_OPEN)
    out[start + 3] = f32::from_bits(254u32);
    // [8-10] AABB max
    out[start + 8] = aabb_max[0]; out[start + 9] = aabb_max[1]; out[start + 10] = aabb_max[2];
    // [11] op_type (5/6 = Seam cut/raised: k slot = width, profile = cross-section, [86] = depth;
    // 7 = Extrude: k slot = width, [86] = SIGNED strength)
    let (op_type, k, prof_ovr): (u32, f32, Option<u32>) = match op {
        BooleanOp::Union { k } => (0, *k, None),
        BooleanOp::Subtract { k } => (1, *k, None),
        BooleanOp::Intersect => (2, 0.0, None),
        BooleanOp::SmoothIntersect { k } => (2, *k, None),
        BooleanOp::ChamferUnion { k } => (3, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::ChamferSubtract { k } => (4, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::Seam { width, profile, raised, .. } => (5 + *raised as u32, *width, Some(*profile as u32)),
        BooleanOp::Extrude { width, .. } => (7, *width, None),
    };
    out[start + 11] = f32::from_bits(op_type);
    // [12] shape_param_2 slot reused for bevel_radius
    out[start + 14] = bevel_radius;
    // [15] blend_k
    out[start + 15] = k;
    // [86] seam depth / extrude strength (scope-level Seam/Extrude body_op)
    if let BooleanOp::Seam { depth, .. } = op { out[start + 86] = *depth; }
    if let BooleanOp::Extrude { strength, .. } = op { out[start + 86] = *strength; }
    // [45] mirror_flags, [46-48] mirror_center, [49] mirror_sign
    out[start + 45] = f32::from_bits(mirror_flags as u32);
    out[start + 46] = mirror_center[0]; out[start + 47] = mirror_center[1]; out[start + 48] = mirror_center[2];
    out[start + 49] = f32::from_bits(mirror_sign as u32);
    // [73] blend_profile
    out[start + 73] = f32::from_bits(prof_ovr.unwrap_or(blend_profile));
    // Scope close offset — patched later by patch_scope_close_hot()
    // Stored at [4] (same slot as quat.x in regular prims, unused in scope markers)
}

/// Emit a SCOPE_CLOSE marker into the hot buffer.
fn emit_scope_close_hot(out: &mut Vec<f32>) {
    let start = out.len();
    out.resize(start + HOT_FLOATS_PER_PRIM, 0.0);
    out[start + 3] = f32::from_bits(255u32); // shape_type = 255 (SCOPE_CLOSE)
}

/// Patch the scope close offset in a hot buffer SCOPE_OPEN marker.
/// `open_float_idx` is the byte-offset (in floats) of the SCOPE_OPEN entry.
fn patch_scope_close_hot(out: &mut [f32], open_float_idx: usize) {
    let open_entry = open_float_idx / HOT_FLOATS_PER_PRIM;
    let close_entry = (out.len() / HOT_FLOATS_PER_PRIM) - 1;
    let offset = (close_entry - open_entry) as u32;
    out[open_float_idx + 4] = f32::from_bits(offset);
}

/// Legacy monolithic serialization — remove after shader split (Sprint 3 Prompt B)
/// Emit a SCOPE_OPEN marker into the flat GPU buffer.
/// Uses the same FLOATS_PER_PRIM stride — only fields [3], [11], [15], [106] are meaningful
/// for boolean evaluation. Fields [0-2] and [8-10] store the body's AABB for scope-skip
/// optimization in the GPU stack machine.
fn emit_scope_open(out: &mut Vec<f32>, op: &BooleanOp, blend_profile: u32, aabb_min: [f32; 3], aabb_max: [f32; 3], mirror_flags: u8, mirror_sign: u8, mirror_center: [f32; 3], bevel_radius: f32) {
    let start = out.len();
    out.resize(start + FLOATS_PER_PRIM, 0.0);
    // field[0-2] = AABB min (x, y, z) — scope-skip optimization
    out[start + 0] = aabb_min[0];
    out[start + 1] = aabb_min[1];
    out[start + 2] = aabb_min[2];
    // field[3] = shape_type = 254 (SCOPE_OPEN)
    out[start + 3] = f32::from_bits(254u32);
    // field[5] = mirror_flags (bit 0=X, bit 1=Y, bit 2=Z)
    out[start + 5] = f32::from_bits(mirror_flags as u32);
    // field[4] = mirror_sign (bit 0=flip X, bit 1=flip Y, bit 2=flip Z)
    out[start + 4] = f32::from_bits(mirror_sign as u32);
    // field[6-7], field[12] = mirror_center (x, y, z)
    out[start + 6] = mirror_center[0];
    out[start + 7] = mirror_center[1];
    out[start + 12] = mirror_center[2];
    // field[8-10] = AABB max (x, y, z) — scope-skip optimization
    out[start + 8] = aabb_max[0];
    out[start + 9] = aabb_max[1];
    out[start + 10] = aabb_max[2];
    // field[11] = op_type (dual-encode: keep op 3/4 for shader compat, override profile for chamfer;
    // Seam = op 5/6, k slot = width, profile = cross-section, [291] = depth;
    // Extrude = op 7, k slot = width, [291] = SIGNED strength)
    let (op_type, k, profile_override): (u32, f32, Option<u32>) = match op {
        BooleanOp::Union { k } => (0, *k, None),
        BooleanOp::Subtract { k } => (1, *k, None),
        BooleanOp::Intersect => (2, 0.0, None),
        BooleanOp::SmoothIntersect { k } => (2, *k, None),
        BooleanOp::ChamferUnion { k } => (3, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::ChamferSubtract { k } => (4, *k, Some(BlendProfile::Chamfer as u32)),
        BooleanOp::Seam { width, profile, raised, .. } => (5 + *raised as u32, *width, Some(*profile as u32)),
        BooleanOp::Extrude { width, .. } => (7, *width, None),
    };
    out[start + 11] = f32::from_bits(op_type);
    // field[15] = blend_k
    out[start + 15] = k;
    // field[291] = seam depth / extrude strength (scope-level Seam/Extrude body_op)
    if let BooleanOp::Seam { depth, .. } = op { out[start + 291] = *depth; }
    if let BooleanOp::Extrude { strength, .. } = op { out[start + 291] = *strength; }
    // field[13] = bevel_radius (body-level edge rounding)
    out[start + 13] = bevel_radius;
    // field[106] = blend_profile (override with Chamfer for legacy chamfer ops)
    out[start + 106] = f32::from_bits(profile_override.unwrap_or(blend_profile));
}

/// Compute world-space AABB for a body subtree (body + child bodies, recursive).
/// Uses each prim's position ± bounding sphere radius.
/// When BODY_LOCAL_PRIMS is true, prim positions are body-local and must be
/// converted to world-space via the body's BodyTransform before accumulation.
fn compute_body_aabb(sg: &SceneGraph, body_id: NodeId) -> ([f32; 3], [f32; 3]) {
    let mut aabb_min = [f32::MAX; 3];
    let mut aabb_max = [f32::MIN; 3];

    fn accumulate(sg: &SceneGraph, nid: NodeId, amin: &mut [f32; 3], amax: &mut [f32; 3]) {
        if let Some(node) = sg.get(nid) {
            if let NodePayload::SdfBody { primitives, origin, body_rotation, body_scale, .. } = &node.payload {
                // When BODY_LOCAL_PRIMS: prim positions are body-local; convert to world.
                // When legacy: prim positions are already world-space; origin offset = zero.
                let transform = if BODY_LOCAL_PRIMS {
                    BodyTransform { origin: *origin, rotation: *body_rotation, scale: *body_scale }
                } else {
                    BodyTransform::default()
                };
                // body_scale max component: bounding_radius() is body-local,
                // so scale by the largest axis for a conservative world-space bound.
                let scale_factor = if BODY_LOCAL_PRIMS {
                    body_scale.max_element()
                } else {
                    1.0
                };
                for p in primitives {
                    if !p.visible { continue; }
                    // A HalfSpace slice is an infinite plane (bounding_radius() = 1e6 sentinel). It
                    // cannot define a finite scope AABB; excluding it keeps the scope-cull tight to
                    // the real (additive) geometry. The slice still cuts everywhere via its own
                    // per-prim bound_r = 1e6 in the bake, and material is a subset of the additive
                    // bounds, so a tighter scope AABB never drops geometry.
                    if matches!(p.shape, SdfShape::HalfSpace) { continue; }
                    // Conservative bound: position ± scaled bound_r
                    let r = p.bounding_radius() * scale_factor;
                    let pos = if BODY_LOCAL_PRIMS {
                        transform.local_to_world(p.position)
                    } else {
                        p.position
                    };
                    amin[0] = amin[0].min(pos.x - r);
                    amin[1] = amin[1].min(pos.y - r);
                    amin[2] = amin[2].min(pos.z - r);
                    amax[0] = amax[0].max(pos.x + r);
                    amax[1] = amax[1].max(pos.y + r);
                    amax[2] = amax[2].max(pos.z + r);
                }
            }
        }
        for &child in sg.children(nid) {
            if matches!(sg.get(child).map(|n| &n.payload), Some(NodePayload::SdfBody { .. })) {
                accumulate(sg, child, amin, amax);
            }
        }
    }

    accumulate(sg, body_id, &mut aabb_min, &mut aabb_max);

    // If no prims found, return a degenerate AABB at origin
    if aabb_min[0] > aabb_max[0] {
        return ([0.0; 3], [0.0; 3]);
    }
    (aabb_min, aabb_max)
}

/// Emit a SCOPE_CLOSE marker into the flat GPU buffer.
fn emit_scope_close(out: &mut Vec<f32>) {
    let start = out.len();
    out.resize(start + FLOATS_PER_PRIM, 0.0);
    out[start + 3] = f32::from_bits(255u32);
}

pub fn serialize_primitives_ex(prims: &[SdfPrimitive], selected: &HashSet<usize>, textured: &HashSet<usize>, hidden_layers: &HashSet<u32>) -> Vec<f32> {
    let mut out = Vec::with_capacity(prims.len() * FLOATS_PER_PRIM);
    // Build ordered index list: ungrouped first, then each layer's members contiguous.
    // This allows the GPU shader to evaluate layers as independent boolean trees.
    let mut ordered: Vec<usize> = Vec::with_capacity(prims.len());

    // Helper: emit prims for a given scope (layer_id filter), grouped by object.
    // Order: ungrouped prims first, then each object's prims contiguous.
    let emit_scope = |ordered: &mut Vec<usize>, scope_layer: Option<u32>| {
        // 1. Ungrouped prims in this scope (no object_id)
        for (i, p) in prims.iter().enumerate() {
            if !p.visible { continue; }
            if p.layer_id != scope_layer { continue; }
            if p.object_id.is_some() { continue; }
            ordered.push(i);
        }
        // 2. Collect unique object ids in this scope (order of first appearance)
        let mut seen_objects: Vec<u32> = Vec::new();
        for p in prims.iter() {
            if !p.visible { continue; }
            if p.layer_id != scope_layer { continue; }
            if let Some(oid) = p.object_id {
                if !seen_objects.contains(&oid) { seen_objects.push(oid); }
            }
        }
        // 3. Each object's prims contiguous
        for oid in &seen_objects {
            for (i, p) in prims.iter().enumerate() {
                if !p.visible { continue; }
                if p.layer_id != scope_layer { continue; }
                if p.object_id != Some(*oid) { continue; }
                ordered.push(i);
            }
        }
    };

    // Scene-level prims (layer_id == None): ungrouped + objects
    emit_scope(&mut ordered, None);

    // Collect unique visible layer ids in order of first appearance
    let mut seen_layers: Vec<u32> = Vec::new();
    for p in prims.iter() {
        if !p.visible { continue; }
        if let Some(lid) = p.layer_id {
            if hidden_layers.contains(&lid) { continue; }
            if !seen_layers.contains(&lid) { seen_layers.push(lid); }
        }
    }
    // Each layer's prims: ungrouped + objects
    for lid in &seen_layers {
        emit_scope(&mut ordered, Some(*lid));
    }
    for &orig_idx in &ordered {
        let p = &prims[orig_idx];
        serialize_single_prim(p, orig_idx, selected, textured, None, None, &mut out, None);
    }
    if out.is_empty() { out.resize(FLOATS_PER_PRIM, 0.0); }
    out
}

pub fn serialize_primitives_bytes(prims: &[SdfPrimitive], selected: &HashSet<usize>, textured: &HashSet<usize>) -> (u32, Vec<u8>) {
    serialize_primitives_bytes_ex(prims, selected, textured, &HashSet::new())
}

pub fn serialize_primitives_bytes_ex(prims: &[SdfPrimitive], selected: &HashSet<usize>, textured: &HashSet<usize>, hidden_layers: &HashSet<u32>) -> (u32, Vec<u8>) {
    let floats = serialize_primitives_ex(prims, selected, textured, hidden_layers);
    let count = (floats.len() / FLOATS_PER_PRIM) as u32;
    let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&floats).to_vec();
    (count, bytes)
}

/// Recursively serialize an SdfBody and its children into the flat GPU buffer.
/// Emits SCOPE_OPEN/CLOSE markers for bodies that need scoped evaluation.
fn serialize_body_recursive(
    sg: &SceneGraph,
    body_id: NodeId,
    is_top_level: bool,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    hidden_layers: &HashSet<u32>,
    overrides: &HashMap<usize, RenderPrimOverride>,
    flat_idx: &mut usize,
    out: &mut Vec<f32>,
    // LAB poly→SDF: parallel per-SLOT grid-start array + packed grid buffer for the outline
    // ghost (u32::MAX for markers / non-selected / non-MeshVolume). Aligned to `out` by
    // pushing exactly once per emitted slot in the SAME walk.
    mesh_start: &mut Vec<u32>,
    grid_buf: &mut Vec<f32>,
    // CurveExtrude outline: parallel per-SLOT clip-record start + packed profiles.
    clip_start: &mut Vec<u32>,
    clip_buf: &mut Vec<f32>,
    // T1 live march: packed height-map atlas (binding 7); field [292] indexes into it.
    height_atlas: &mut Vec<f32>,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_prof, body_transform) = match &node.payload {
        NodePayload::SdfBody { primitives, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_profile, origin, body_rotation, body_scale, .. } => {
            let bt = BodyTransform { origin: *origin, rotation: *body_rotation, scale: *body_scale };
            (primitives, body_op, *mirror_flags, *mirror_sign, [mirror_center.x, mirror_center.y, mirror_center.z], *bevel_radius, *blend_profile as u32, bt)
        },
        _ => return,
    };

    let child_bodies = sg.child_bodies(body_id);

    // Skip truly empty bodies (no own prims AND no child bodies).
    // Avoids emitting degenerate scope markers (AABB at origin) that waste
    // GPU cycles and can produce phantom geometry on some drivers.
    if prims.is_empty() && child_bodies.is_empty() {
        return;
    }

    // ALL bodies get scope markers (enables tile-based GPU culling).
    // Top-level bodies always use Union; child bodies use their body_op.
    let (mut aabb_min, mut aabb_max) = compute_body_aabb(sg, body_id);
    // Expand AABB to cover mirrored region when body mirror is active.
    // Mirror around center c on axis i: reflected range = [2c - max, 2c - min].
    // Union with original = [min(min, 2c - max), max(max, 2c - min)].
    if mirror_flags != 0 {
        for axis in 0..3 {
            if (mirror_flags >> axis) & 1 != 0 {
                let c = mirror_center[axis];
                let reflected_min = 2.0 * c - aabb_max[axis];
                let reflected_max = 2.0 * c - aabb_min[axis];
                aabb_min[axis] = aabb_min[axis].min(reflected_min);
                aabb_max[axis] = aabb_max[axis].max(reflected_max);
            }
        }
    }
    let open_float_idx = out.len();
    if is_top_level {
        emit_scope_open(out, &BooleanOp::Union { k: 0.0 }, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    } else {
        emit_scope_open(out, body_op, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    }
    mesh_start.push(u32::MAX); // SCOPE_OPEN slot has no grid
    clip_start.push(u32::MAX);

    // Emit this body's own primitives
    for p in prims {
        // Check render overrides — hidden prims are skipped entirely
        let ovr = overrides.get(flat_idx);
        if ovr.map_or(false, |o| o.hidden) { *flat_idx += 1; continue; }
        if !p.visible { *flat_idx += 1; continue; }
        if let Some(lid) = p.layer_id {
            if hidden_layers.contains(&lid) { *flat_idx += 1; continue; }
        }
        // Resolve shared material from graph (or fall back to embedded material)
        let resolved_mat = resolve_prim_material(p, sg);
        // BODY_LOCAL_PRIMS: prim.position is body-local; the GPU analytical march needs
        // world-space positions and body-scaled prim scales. Apply body transform overrides.
        // Legacy path (BODY_LOCAL_PRIMS=false): positions are already world-space.
        if BODY_LOCAL_PRIMS {
            let world_pos = body_transform.local_to_world(p.position);
            let world_scale = p.scale * body_transform.scale;
            let pos_override = ovr.map_or_else(
                || RenderPrimOverride { position: Some(world_pos), scale: Some(world_scale), ..Default::default() },
                |o| RenderPrimOverride { position: o.position.or(Some(world_pos)), scale: o.scale.or(Some(world_scale)), ..o.clone() },
            );
            serialize_single_prim(p, *flat_idx, selected, textured, Some(&pos_override), Some(resolved_mat), out, Some(height_atlas));
        } else {
            serialize_single_prim(p, *flat_idx, selected, textured, ovr, Some(resolved_mat), out, Some(height_atlas));
        }
        // Parallel grid-start slot for the prim just emitted: pack the grid only for a SELECTED
        // MeshVolume (the outline marches only selected prims), else sentinel.
        let ms = match &p.shape {
            SdfShape::MeshVolume { grid, .. } if selected.contains(flat_idx) => pack_mesh_grid(grid, grid_buf),
            _ => u32::MAX,
        };
        mesh_start.push(ms);
        // CurveExtrude outline: pack the REAL profile (not a sphere stand-in) when selected.
        let cs = if selected.contains(flat_idx) {
            if let SdfShape::CurveExtrude { ref profile, open, thickness, tube, .. } = &p.shape {
                if profile.is_valid() {
                    let is_open = *open && *thickness > 1e-5;
                    let mode_slot = if is_open {
                        let t = thickness.abs().max(1e-4);
                        if *tube { -t } else { t }
                    } else {
                        0.0
                    };
                    pack_curve_profile(profile, mode_slot, is_open, clip_buf)
                } else {
                    u32::MAX
                }
            } else {
                u32::MAX
            }
        } else {
            u32::MAX
        };
        clip_start.push(cs);
        *flat_idx += 1;
    }

    // Recurse into child bodies
    for child_id in &child_bodies {
        if let Some(child_node) = sg.get(*child_id) {
            if !child_node.visible { continue; }
        }
        serialize_body_recursive(sg, *child_id, false, selected, textured, hidden_layers, overrides, flat_idx, out, mesh_start, grid_buf, clip_start, clip_buf, height_atlas);
    }

    emit_scope_close(out);
    mesh_start.push(u32::MAX); // SCOPE_CLOSE slot has no grid
    clip_start.push(u32::MAX);
    // Fixup: store OPEN→CLOSE offset in field [4] so GPU can skip without scanning.
    let open_entry = open_float_idx / FLOATS_PER_PRIM;
    let close_entry = (out.len() / FLOATS_PER_PRIM) - 1;
    let offset = (close_entry - open_entry) as u32;
    out[open_float_idx + 4] = f32::from_bits(offset);
}

/// Recursively collect all visible SdfBody nodes under `parent`,
/// descending through non-body nodes (Group, Empty, etc.).
/// Bodies nested under other SdfBodies are NOT collected here —
/// `serialize_body_recursive` handles those via `child_bodies`.
fn collect_visible_bodies(sg: &SceneGraph, parent: NodeId) -> Vec<NodeId> {
    let mut result = Vec::new();
    for &child in sg.children(parent) {
        let node = match sg.get(child) { Some(n) => n, None => continue };
        if !node.visible { continue; }
        if matches!(node.payload, NodePayload::SdfBody { .. }) {
            // Found a body — add it. Don't recurse further here;
            // serialize_body_recursive will handle nested child bodies.
            result.push(child);
        } else {
            // Non-body node (Group, Empty, etc.) — recurse to find
            // SdfBodies nested underneath.
            result.extend(collect_visible_bodies(sg, child));
        }
    }
    result
}

/// Serialize entire scene graph for GPU upload.
/// Returns (num_entries, bytes) — num_entries includes scope markers.
/// Replaces serialize_primitives_bytes_ex for the viewport render path.
/// `overrides` provides per-prim render overrides (keyed by flat index) applied
/// during serialization without mutating the source scene graph.
///
/// Uses rayon to parallelize per-body serialization when there are multiple
/// top-level bodies. Each body's serialization is independent — the borrow
/// checker proves no shared mutable state.
/// Returns a `SerializeResult` with num_entries, prim_bytes, and body_bvh_bytes.
pub fn serialize_from_graph(
    sg: &SceneGraph,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    hidden_layers: &HashSet<u32>,
    overrides: &HashMap<usize, RenderPrimOverride>,
    mode: SerializeMode,
) -> SerializeResult {
    use rayon::prelude::*;

    let root = sg.root();

    // Recursively collect visible SdfBody nodes, descending through
    // non-body parents (Group, Empty, etc.). This finds bodies nested
    // at any depth — not just direct children of root.
    let top_bodies = match mode {
        SerializeMode::ActiveOnly(body_nid) => {
            // Resolve to top-level ancestor body (the cache unit)
            let resolved = resolve_top_level_body(sg, body_nid);
            if sg.get(resolved).map_or(false, |n| n.visible) { vec![resolved] } else { vec![] }
        }
        SerializeMode::Full => collect_visible_bodies(sg, root),
    };

    if top_bodies.is_empty() {
        // No visible SDF bodies — return count=0 so the shader early-returns
        // (num_primitives==0 → return 999.0). Buffer must be non-empty for wgpu.
        let out = vec![0.0f32; FLOATS_PER_PRIM];
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&out).to_vec();
        return SerializeResult {
            num_entries: 0, prim_bytes: bytes, body_bvh_bytes: Vec::new(), body_byte_ranges: Vec::new(),
            mesh_start_bytes: Vec::new(), mesh_grid_bytes: Vec::new(),
            clip_start_bytes: Vec::new(), clip_curves_bytes: Vec::new(),
            height_map_bytes: vec![0u8; 4],
        };
    }

    // Compute per-body prim counts and flat_idx offsets (prefix sum)
    let prim_counts: Vec<usize> = top_bodies.iter()
        .map(|&nid| sg.body_prim_count(nid))
        .collect();
    let mut flat_offsets = Vec::with_capacity(top_bodies.len());
    let mut running = 0usize;
    for &count in &prim_counts {
        flat_offsets.push(running);
        running += count;
    }

    // Parallel serialization: each body produces (prim floats, mesh starts, grids, clip starts, clips, height atlas)
    let segments: Vec<(Vec<f32>, Vec<u32>, Vec<f32>, Vec<u32>, Vec<f32>, Vec<f32>)> = top_bodies.par_iter()
        .zip(flat_offsets.par_iter())
        .zip(prim_counts.par_iter())
        .map(|((&body_nid, &offset), &count)| {
            let mut segment = Vec::with_capacity(count.max(16) * FLOATS_PER_PRIM);
            let mut flat_idx = offset;
            let mut start_seg: Vec<u32> = Vec::with_capacity(count.max(16) + 2);
            let mut grid_seg: Vec<f32> = Vec::new();
            let mut clip_start_seg: Vec<u32> = Vec::with_capacity(count.max(16) + 2);
            let mut clip_seg: Vec<f32> = Vec::new();
            let mut height_seg: Vec<f32> = Vec::new();
            serialize_body_recursive(
                sg, body_nid, true, selected, textured, hidden_layers,
                overrides, &mut flat_idx, &mut segment, &mut start_seg, &mut grid_seg,
                &mut clip_start_seg, &mut clip_seg, &mut height_seg,
            );
            (segment, start_seg, grid_seg, clip_start_seg, clip_seg, height_seg)
        })
        .collect();

    // Concatenate segments and collect body entry ranges.
    // AZRA_MASTER_ROADMAP A4: scene BVH input list only when BUILD_DEAD_SCENE_BVH (below).
    const BUILD_DEAD_SCENE_BVH: bool = false;
    let total_floats: usize = segments.iter().map(|(s, _, _, _, _, _)| s.len()).sum();
    let mut out = Vec::with_capacity(total_floats);
    let mut scene_bvh_bodies: Vec<SceneBvhBody> = if BUILD_DEAD_SCENE_BVH {
        Vec::with_capacity(segments.len())
    } else {
        Vec::new()
    };
    let mut body_byte_ranges: Vec<(NodeId, usize, usize, usize)> = Vec::with_capacity(segments.len());
    // LAB poly→SDF: reconcile per-segment grid-start arrays + grid buffers. Each segment's grid
    // offsets are segment-local; rebase by the running global grid length as we concatenate.
    let mut mesh_start: Vec<u32> = Vec::with_capacity(segments.iter().map(|(_, s, _, _, _, _)| s.len()).sum());
    let mut mesh_grid: Vec<f32> = Vec::new();
    let mut clip_start: Vec<u32> = Vec::with_capacity(segments.iter().map(|(_, _, _, c, _, _)| c.len()).sum());
    let mut clip_curves: Vec<f32> = Vec::new();
    let mut height_maps: Vec<f32> = Vec::new();

    for (i, (seg, start_seg, grid_seg, clip_start_seg, clip_seg, height_seg)) in segments.iter().enumerate() {
        if seg.is_empty() { continue; }
        let float_start = out.len();
        if BUILD_DEAD_SCENE_BVH {
            let start_entry = (float_start / FLOATS_PER_PRIM) as u32;
            let entry_count = (seg.len() / FLOATS_PER_PRIM) as u32;
            // Extract AABB from SCOPE_OPEN marker (fields [0-2] = min, [8-10] = max)
            let aabb = Aabb {
                min: [seg[0], seg[1], seg[2]],
                max: [seg[8], seg[9], seg[10]],
            };
            scene_bvh_bodies.push(SceneBvhBody {
                aabb,
                start_entry,
                entry_count,
            });
        }
        // Rebase prim field [292] height atlas starts before extending out.
        let height_base = height_maps.len() as u32;
        if height_base > 0 {
            let mut rebased = seg.clone();
            let n_slots = rebased.len() / FLOATS_PER_PRIM;
            for si in 0..n_slots {
                let i292 = si * FLOATS_PER_PRIM + 292;
                let v = rebased[i292].to_bits();
                if v != HEIGHT_MAP_NONE {
                    rebased[i292] = f32::from_bits(v.wrapping_add(height_base));
                }
            }
            out.extend_from_slice(&rebased);
        } else {
            out.extend_from_slice(seg);
        }
        height_maps.extend_from_slice(height_seg);
        // start_seg is parallel to this segment's slots — rebase non-sentinel offsets.
        let grid_base = mesh_grid.len() as u32;
        mesh_start.extend(start_seg.iter().map(|&s| if s == u32::MAX { u32::MAX } else { s + grid_base }));
        mesh_grid.extend_from_slice(grid_seg);
        let clip_base = clip_curves.len() as u32;
        clip_start.extend(clip_start_seg.iter().map(|&s| if s == u32::MAX { u32::MAX } else { s + clip_base }));
        clip_curves.extend_from_slice(clip_seg);
        body_byte_ranges.push((top_bodies[i], float_start * 4, out.len() * 4, flat_offsets[i]));
    }

    if out.is_empty() {
        // All bodies were empty — return count=0 (shader early-returns).
        out.resize(FLOATS_PER_PRIM, 0.0);
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&out).to_vec();
        return SerializeResult {
            num_entries: 0, prim_bytes: bytes, body_bvh_bytes: Vec::new(), body_byte_ranges: Vec::new(),
            mesh_start_bytes: Vec::new(), mesh_grid_bytes: Vec::new(),
            clip_start_bytes: Vec::new(), clip_curves_bytes: Vec::new(),
            height_map_bytes: vec![0u8; 4],
        };
    }
    let count = (out.len() / FLOATS_PER_PRIM) as u32;
    let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&out).to_vec();
    // Invariant: the outline grid-start array is exactly one entry per prim-buffer SLOT (markers +
    // prims). If this trips, the segment walk diverged from serialize_body_recursive → the ghost
    // would sample the wrong grid. Structural guard (production-DCC standard, not a bandaid).
    debug_assert_eq!(mesh_start.len(), count as usize,
        "outline mesh_start ({}) must be parallel to prim slots ({})", mesh_start.len(), count);
    debug_assert_eq!(clip_start.len(), count as usize,
        "outline clip_start ({}) must be parallel to prim slots ({})", clip_start.len(), count);
    if mesh_grid.is_empty() { mesh_grid.push(0.0); } // wgpu min 1 element
    if mesh_start.is_empty() { mesh_start.push(u32::MAX); }
    if clip_curves.is_empty() { clip_curves.push(0.0); }
    if clip_start.is_empty() { clip_start.push(u32::MAX); }
    if height_maps.is_empty() { height_maps.push(0.0); }
    let mesh_start_bytes: Vec<u8> = bytemuck::cast_slice::<u32, u8>(&mesh_start).to_vec();
    let mesh_grid_bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&mesh_grid).to_vec();
    let clip_start_bytes: Vec<u8> = bytemuck::cast_slice::<u32, u8>(&clip_start).to_vec();
    let clip_curves_bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&clip_curves).to_vec();
    let height_map_bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&height_maps).to_vec();

    // A4: builder kept for Phase B repoint — flip BUILD_DEAD_SCENE_BVH above to re-enable.
    let body_bvh_bytes = if BUILD_DEAD_SCENE_BVH {
        let scene_bvh_nodes = build_scene_bvh(&scene_bvh_bodies);
        if scene_bvh_nodes.is_empty() {
            Vec::new()
        } else {
            bytemuck::cast_slice::<BvhNode, u8>(&scene_bvh_nodes).to_vec()
        }
    } else {
        Vec::new()
    };

    // Debug assertions for ActiveOnly mode
    if cfg!(debug_assertions) {
        if let SerializeMode::ActiveOnly(_) = mode {
            debug_assert_eq!(
                top_bodies.len(), 1,
                "ActiveOnly should serialize exactly 1 body, got {}",
                top_bodies.len()
            );
        }
    }

    SerializeResult {
        num_entries: count, prim_bytes: bytes, body_bvh_bytes, body_byte_ranges,
        mesh_start_bytes, mesh_grid_bytes, clip_start_bytes, clip_curves_bytes,
        height_map_bytes,
    }
}

/// Re-serialize a single body's segment for partial buffer update.
/// `flat_offset` is the prim flat index where this body starts (for selection/textured bits).
/// Returns the body's serialized bytes — same format as its section in a full serialize.
pub fn serialize_single_body(
    sg: &SceneGraph,
    body_nid: NodeId,
    flat_offset: usize,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    hidden_layers: &HashSet<u32>,
    overrides: &HashMap<usize, RenderPrimOverride>,
) -> Vec<u8> {
    let cap = sg.body_prim_count(body_nid).max(16) * FLOATS_PER_PRIM;
    let mut segment = Vec::with_capacity(cap);
    let mut flat_idx = flat_offset;
    // Partial single-body path (no outline ghost) — discard the grid-start/grid buffers.
    let mut mesh_start_unused: Vec<u32> = Vec::new();
    let mut grid_unused: Vec<f32> = Vec::new();
    let mut clip_start_unused: Vec<u32> = Vec::new();
    let mut clip_unused: Vec<f32> = Vec::new();
    let mut height_unused: Vec<f32> = Vec::new();
    serialize_body_recursive(
        sg, body_nid, true, selected, textured, hidden_layers,
        overrides, &mut flat_idx, &mut segment, &mut mesh_start_unused, &mut grid_unused,
        &mut clip_start_unused, &mut clip_unused, &mut height_unused,
    );
    bytemuck::cast_slice::<f32, u8>(&segment).to_vec()
}

/// Per-body BVH patch info collected during serialization.
struct BodyBvhPatch {
    scope_open_float_idx: usize, // float index of SCOPE_OPEN within this segment's hot buffer
    nodes: Vec<BvhNode>,
    body_max_k: f32,
    direct_prim_count: u32,
}

/// Serialize entire scene graph into split hot + cold GPU buffers.
/// Returns (num_entries, hot_bytes, cold_bytes, bvh_bytes, body_index_map).
/// Hot buffer = 80 floats/entry (SDF eval + march preview).
/// Cold buffer = 224 floats/entry (deferred material + DNA).
/// Scope markers (SCOPE_OPEN/CLOSE) appear in hot buffer; cold buffer gets zero-padding for those entries.
/// `bvh_bytes` contains the BVH node buffer for bodies with > 8 prims.
/// body_index_map maps NodeId → per-frame 1-based body index (for silhouette outline).
pub fn serialize_from_graph_hot_cold(
    sg: &SceneGraph,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    hidden_layers: &HashSet<u32>,
    overrides: &HashMap<usize, RenderPrimOverride>,
    mode: SerializeMode,
) -> (u32, Vec<u8>, Vec<u8>, Vec<u8>, Vec<(NodeId, u32)>) {
    use rayon::prelude::*;

    let root = sg.root();

    let top_bodies = match mode {
        SerializeMode::ActiveOnly(body_nid) => {
            let resolved = resolve_top_level_body(sg, body_nid);
            if sg.get(resolved).map_or(false, |n| n.visible) { vec![resolved] } else { vec![] }
        }
        SerializeMode::Full => collect_visible_bodies(sg, root),
    };

    if top_bodies.is_empty() {
        let hot = vec![0.0f32; HOT_FLOATS_PER_PRIM];
        let cold = vec![0.0f32; COLD_FLOATS_PER_PRIM];
        let hb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&hot).to_vec();
        let cb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&cold).to_vec();
        return (0, hb, cb, Vec::new(), vec![]);
    }

    // Assign per-frame 1-based body indices to each top-level body
    let body_index_map: Vec<(NodeId, u32)> = top_bodies.iter()
        .enumerate()
        .map(|(i, &nid)| (nid, (i + 1) as u32))
        .collect();

    let prim_counts: Vec<usize> = top_bodies.iter()
        .map(|&nid| sg.body_prim_count(nid))
        .collect();
    let mut flat_offsets = Vec::with_capacity(top_bodies.len());
    let mut running = 0usize;
    for &count in &prim_counts {
        flat_offsets.push(running);
        running += count;
    }

    // Parallel serialization: each body produces hot + cold segments + BVH patches
    let segments: Vec<(Vec<f32>, Vec<f32>, Vec<BodyBvhPatch>)> = top_bodies.par_iter()
        .zip(flat_offsets.par_iter())
        .enumerate()
        .map(|(bi, (&body_nid, &offset))| {
            let body_idx = (bi + 1) as u32; // 1-based
            let cap = prim_counts[bi].max(16);
            let mut hot_seg = Vec::with_capacity(cap * HOT_FLOATS_PER_PRIM);
            let mut cold_seg = Vec::with_capacity(cap * COLD_FLOATS_PER_PRIM);
            let mut bvh_patches = Vec::new();
            let mut flat_idx = offset;
            serialize_body_recursive_hot_cold(
                sg, body_nid, true, selected, textured, hidden_layers,
                overrides, body_idx, &mut flat_idx, &mut hot_seg, &mut cold_seg,
                &mut bvh_patches,
            );
            (hot_seg, cold_seg, bvh_patches)
        })
        .collect();

    let total_hot: usize = segments.iter().map(|(h, _, _)| h.len()).sum();
    let total_cold: usize = segments.iter().map(|(_, c, _)| c.len()).sum();
    let mut hot_out = Vec::with_capacity(total_hot);
    let mut cold_out = Vec::with_capacity(total_cold);
    let mut all_bvh_nodes: Vec<BvhNode> = Vec::new();

    for (h, c, patches) in &segments {
        let seg_float_offset = hot_out.len();
        hot_out.extend_from_slice(h);
        cold_out.extend_from_slice(c);

        // Process BVH patches for this segment
        for patch in patches {
            let global_open_float_idx = seg_float_offset + patch.scope_open_float_idx;
            let node_offset = all_bvh_nodes.len() as u32;
            all_bvh_nodes.extend_from_slice(&patch.nodes);

            // Patch SCOPE_OPEN fields [50-53] in the hot buffer
            hot_out[global_open_float_idx + 50] = f32::from_bits(node_offset);
            hot_out[global_open_float_idx + 51] = f32::from_bits(patch.nodes.len() as u32);
            hot_out[global_open_float_idx + 52] = f32::from_bits(patch.direct_prim_count);
            hot_out[global_open_float_idx + 53] = patch.body_max_k;
        }
    }

    if hot_out.is_empty() {
        hot_out.resize(HOT_FLOATS_PER_PRIM, 0.0);
        cold_out.resize(COLD_FLOATS_PER_PRIM, 0.0);
        let hb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&hot_out).to_vec();
        let cb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&cold_out).to_vec();
        return (0, hb, cb, Vec::new(), body_index_map);
    }
    let count = (hot_out.len() / HOT_FLOATS_PER_PRIM) as u32;
    debug_assert_eq!(hot_out.len() / HOT_FLOATS_PER_PRIM, cold_out.len() / COLD_FLOATS_PER_PRIM,
        "hot/cold entry count mismatch");
    let hb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&hot_out).to_vec();
    let cb: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&cold_out).to_vec();

    // Convert BVH nodes to bytes (BvhNode is Pod, so use bytemuck)
    let bvh_bytes = if all_bvh_nodes.is_empty() {
        Vec::new()
    } else {
        bytemuck::cast_slice::<BvhNode, u8>(&all_bvh_nodes).to_vec()
    };

    // Debug diagnostics for ActiveOnly mode
    if cfg!(debug_assertions) {
        if let SerializeMode::ActiveOnly(_) = mode {
            let total_scene_prims: usize = collect_visible_bodies(sg, root)
                .iter()
                .map(|&nid| sg.body_prim_count(nid))
                .sum();
            eprintln!(
                "[SERIALIZE-HC] ActiveOnly: {} prims (was {} total)",
                count, total_scene_prims
            );
            debug_assert_eq!(
                top_bodies.len(), 1,
                "ActiveOnly should serialize exactly 1 body, got {}",
                top_bodies.len()
            );
        }
    }

    (count, hb, cb, bvh_bytes, body_index_map)
}

/// Recursively serialize an SdfBody into split hot + cold buffers.
/// Emits SCOPE_OPEN/CLOSE in hot buffer; cold buffer gets zero-padded entries for scope markers.
/// Collects BVH patches for bodies with many direct prims.
fn serialize_body_recursive_hot_cold(
    sg: &SceneGraph,
    body_id: NodeId,
    is_top_level: bool,
    selected: &HashSet<usize>,
    textured: &HashSet<usize>,
    hidden_layers: &HashSet<u32>,
    overrides: &HashMap<usize, RenderPrimOverride>,
    body_index: u32,
    flat_idx: &mut usize,
    hot_out: &mut Vec<f32>,
    cold_out: &mut Vec<f32>,
    bvh_patches: &mut Vec<BodyBvhPatch>,
) {
    let node = match sg.get(body_id) { Some(n) => n, None => return };
    let (prims, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_prof, body_transform) = match &node.payload {
        NodePayload::SdfBody { primitives, body_op, mirror_flags, mirror_sign, mirror_center, bevel_radius, blend_profile, origin, body_rotation, body_scale, .. } => {
            let bt = BodyTransform { origin: *origin, rotation: *body_rotation, scale: *body_scale };
            (primitives, body_op, *mirror_flags, *mirror_sign, [mirror_center.x, mirror_center.y, mirror_center.z], *bevel_radius, *blend_profile as u32, bt)
        },
        _ => return,
    };

    let child_bodies = sg.child_bodies(body_id);
    if prims.is_empty() && child_bodies.is_empty() { return; }

    let (mut aabb_min, mut aabb_max) = compute_body_aabb(sg, body_id);
    if mirror_flags != 0 {
        for axis in 0..3 {
            if (mirror_flags >> axis) & 1 != 0 {
                let c = mirror_center[axis];
                let reflected_min = 2.0 * c - aabb_max[axis];
                let reflected_max = 2.0 * c - aabb_min[axis];
                aabb_min[axis] = aabb_min[axis].min(reflected_min);
                aabb_max[axis] = aabb_max[axis].max(reflected_max);
            }
        }
    }

    // SCOPE_OPEN: hot buffer gets the marker, cold buffer gets zero-padding
    let open_float_idx = hot_out.len();
    if is_top_level {
        emit_scope_open_hot(hot_out, &BooleanOp::Union { k: 0.0 }, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    } else {
        emit_scope_open_hot(hot_out, body_op, blend_prof, aabb_min, aabb_max, mirror_flags, mirror_sign, mirror_center, bevel_radius);
    }
    // Cold: zero-pad for scope marker entry
    let cold_start = cold_out.len();
    cold_out.resize(cold_start + COLD_FLOATS_PER_PRIM, 0.0);

    // Emit this body's own primitives, collecting BVH data
    let mut bvh_prims = Vec::new();
    let mut direct_prim_count: u32 = 0;
    for p in prims {
        let ovr = overrides.get(flat_idx);
        if ovr.map_or(false, |o| o.hidden) { *flat_idx += 1; continue; }
        if !p.visible { *flat_idx += 1; continue; }
        if let Some(lid) = p.layer_id {
            if hidden_layers.contains(&lid) { *flat_idx += 1; continue; }
        }
        // BODY_LOCAL_PRIMS: prim.position is body-local; the GPU analytical march needs
        // world-space positions. Apply local→world as a position override.
        // Legacy path (BODY_LOCAL_PRIMS=false): positions are already world-space.
        let world_pos = if BODY_LOCAL_PRIMS {
            let local = ovr.and_then(|o| o.position).unwrap_or(p.position);
            body_transform.local_to_world(local)
        } else {
            ovr.and_then(|o| o.position).unwrap_or(p.position)
        };
        // Collect BVH prim data before serialization (BVH uses world-space positions)
        let (op_u, blend_k) = match &p.operation {
            BooleanOp::Union { k } => (0u32, *k),
            BooleanOp::Subtract { k } => (1, *k),
            BooleanOp::Intersect => (2, 0.0),
            BooleanOp::SmoothIntersect { k } => (2, *k),
            BooleanOp::ChamferUnion { k } => (3, *k),
            BooleanOp::ChamferSubtract { k } => (4, *k),
            // BVH prune metadata: width plays the blend-k role (influence pad).
            BooleanOp::Seam { width, raised, .. } => (5 + *raised as u32, *width),
            BooleanOp::Extrude { width, .. } => (7, *width),
        };
        bvh_prims.push(BvhPrim {
            center: world_pos,
            radius: p.bounding_radius() * if BODY_LOCAL_PRIMS { body_transform.scale.max_element() } else { 1.0 },
            body_local_index: direct_prim_count,
            op: op_u,
            blend_k,
        });
        // Resolve shared material from graph (or fall back to embedded material)
        let resolved_mat = resolve_prim_material(p, sg);
        if BODY_LOCAL_PRIMS {
            // Build position + scale override so serialize_single_prim_hot writes world-space.
            let world_scale = p.scale * body_transform.scale;
            let pos_override = ovr.map_or_else(
                || RenderPrimOverride { position: Some(world_pos), scale: Some(world_scale), ..Default::default() },
                |o| RenderPrimOverride { position: o.position.map(|_| world_pos).or(Some(world_pos)), scale: o.scale.or(Some(world_scale)), ..o.clone() },
            );
            serialize_single_prim_hot(p, *flat_idx, selected, Some(&pos_override), body_index, Some(resolved_mat), hot_out);
        } else {
            serialize_single_prim_hot(p, *flat_idx, selected, ovr, body_index, Some(resolved_mat), hot_out);
        }
        serialize_single_prim_cold(p, *flat_idx, textured, ovr, Some(resolved_mat), cold_out);
        direct_prim_count += 1;
        *flat_idx += 1;
    }

    // Build BVH if this body has enough direct prims
    if let Some(bvh) = build_body_bvh(&bvh_prims, 8) {
        bvh_patches.push(BodyBvhPatch {
            scope_open_float_idx: open_float_idx,
            nodes: bvh.nodes,
            body_max_k: bvh.body_max_k,
            direct_prim_count,
        });
    }

    // Recurse into child bodies
    for child_id in &child_bodies {
        if let Some(child_node) = sg.get(*child_id) {
            if !child_node.visible { continue; }
        }
        serialize_body_recursive_hot_cold(sg, *child_id, false, selected, textured, hidden_layers, overrides, body_index, flat_idx, hot_out, cold_out, bvh_patches);
    }

    // SCOPE_CLOSE: hot buffer gets marker, cold buffer gets zero-padding
    emit_scope_close_hot(hot_out);
    let cold_close_start = cold_out.len();
    cold_out.resize(cold_close_start + COLD_FLOATS_PER_PRIM, 0.0);

    // Patch scope close offset in hot buffer
    patch_scope_close_hot(hot_out, open_float_idx);
}

/// Compute the scene bounding box from serialized hot prim data.
/// Scans top-level SCOPE_OPEN markers for their AABB min/max and returns the union.
/// Returns `([min_x, min_y, min_z], [max_x, max_y, max_z])`.
pub fn compute_scene_aabb(prim_bytes: &[u8], num_entries: u32) -> ([f32; 3], [f32; 3]) {
    if num_entries == 0 || prim_bytes.is_empty() {
        return ([0.0; 3], [0.0; 3]);
    }
    let floats: &[f32] = bytemuck::cast_slice(prim_bytes);
    let n = num_entries as usize;
    let mut s_min = [f32::MAX; 3];
    let mut s_max = [f32::MIN; 3];
    let mut found = false;
    let mut i = 0usize;
    while i < n {
        let base = i * HOT_FLOATS_PER_PRIM;
        if base + HOT_FLOATS_PER_PRIM > floats.len() { break; }
        let st = f32::to_bits(floats[base + 3]) & 0xFF;
        if st == 254 {
            // SCOPE_OPEN: read AABB from fields [0-2] (min) and [8-10] (max)
            let blend_k = floats[base + 15];
            for axis in 0..3 {
                let lo = floats[base + axis] - blend_k;
                let hi = floats[base + 8 + axis] + blend_k;
                s_min[axis] = s_min[axis].min(lo);
                s_max[axis] = s_max[axis].max(hi);
            }
            found = true;
            // Skip to matching SCOPE_CLOSE (top-level bodies only — don't recurse into nested)
            let offset = f32::to_bits(floats[base + 4]) as usize;
            i += offset + 1; // jump past SCOPE_CLOSE
            continue;
        }
        i += 1;
    }
    if !found {
        return ([0.0; 3], [0.0; 3]);
    }
    (s_min, s_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shader_build::{build_deferred_wgsl, build_deferred_lite_wgsl};

    /// Guard: SDF reflection probe must be present in the full deferred shader.
    /// Without it, metallic/glossy surfaces show a black bottom hemisphere
    /// because the env map has no ground plane to reflect.
    /// GATE (owner 2026-07-30) — the RASTER branch must run split-sum IBL, not punctual lights only.
    ///
    /// Owner: "I still need to go to path tracing preview before metalness and roughness work in
    /// PBR." The raster branch (poly bodies + baked SDF/DC meshes) computed its final colour from
    /// analytic lights and returned BEFORE the split-sum block the SDF branch runs. A metal has no
    /// diffuse term and its specular is a mirror of the environment, so with nothing to reflect it
    /// rendered flat dark grey however correct the metallic map was.
    ///
    /// This asserts the three terms that make metal read, keyed on the raster branch's own `lit_`
    /// prefix so it cannot be satisfied by the SDF branch's copies. It also pins the ambient
    /// scale-down, without which IBL and flat ambient double-count.
    #[test]
    fn deferred_raster_branch_has_ibl() {
        let wgsl = build_deferred_wgsl(None);
        for (needle, why) in [
            (
                "env_sh_irradiance(lit_bent)",
                "diffuse IBL missing — raster surfaces get no environment fill",
            ),
            (
                "sample_env_specular(lit_refl, lit_rough)",
                "specular IBL missing — METAL CANNOT READ (nothing to reflect)",
            ),
            (
                "env_brdf(lit_ndv, lit_rough)",
                "split-sum second term missing — specular energy will be wrong",
            ),
            (
                "select(lit_amb_flat, lit_amb_flat * 0.12, config.env_width > 0u)",
                "ambient not scaled down under IBL — flat ambient and IBL DOUBLE-COUNT",
            ),
        ] {
            assert!(
                wgsl.contains(needle),
                "REGRESSION: raster branch lost `{needle}` — {why}. \
                 Metalness/roughness would only read correctly in Path Trace again. \
                 See deferred_entry.wgsl, the `lit_is_raster` branch."
            );
        }
    }

    #[test]
    fn deferred_shader_has_sdf_reflection_probe() {
        let wgsl = build_deferred_wgsl(None);
        assert!(
            wgsl.contains("SDF reflection probe"),
            "REGRESSION: SDF reflection probe is missing from deferred shader! \
             Metallic surfaces will show black hemispheres. \
             See deferred_entry.wgsl specular IBL section."
        );
        // Verify the sphere trace loop is present
        assert!(
            wgsl.contains("scene_sdf(rp)"),
            "REGRESSION: SDF sphere trace call missing from reflection probe!"
        );
    }

    /// Guard: SDF reflection probe must be stripped from DF-lite (it calls scene_sdf
    /// which requires SCENE_EVAL — unavailable in the lite variant).
    /// The full deferred shader includes it; lite uses env-only reflections at startup.
    #[test]
    fn deferred_lite_shader_strips_sdf_reflection_probe() {
        let wgsl = build_deferred_lite_wgsl();
        assert!(
            !wgsl.contains("scene_sdf"),
            "REGRESSION: scene_sdf leaked into deferred-lite shader! \
             DF-lite strips SCENE_EVAL, so scene_sdf must not appear."
        );
    }

    /// T1: image height maps pack into the bake side buffer and patch hot[92].
    #[test]
    fn serialize_height_maps_for_bake_patches_hot92() {
        use core_math::modifiers::{DisplaceHeightMap, Modifier, ModifierType, VolumeMask};
        use core_math::primitives::{BooleanOp, BlendProfile, PbrMaterial, SdfPrimitive, SdfShape};
        use core_math::scene_graph::{NodePayload, SceneGraph};

        let mut sg = SceneGraph::new();
        let root = sg.root();
        let mut prim = SdfPrimitive::new(
            "hill".into(),
            SdfShape::Sphere { radius: 1.0 },
            glam::Vec3::ZERO,
        );
        prim.modifiers.push(Modifier {
            mod_type: ModifierType::Displace,
            strength: 0.2,
            axis: 0,
            param2: 1.0,
            mask: VolumeMask::default(),
            height_map: Some(DisplaceHeightMap {
                w: 2,
                h: 2,
                pixels: vec![0, 64, 128, 255],
                path: String::new(),
            }),
        });
        prim.sync_modifiers_to_gpu_fields();
        let body = sg.add_child(
            root,
            NodePayload::SdfBody {
                name: "Body".into(),
                primitives: vec![prim],
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: glam::Vec3::ZERO,
                body_rotation: glam::Quat::IDENTITY,
                body_scale: glam::Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: glam::Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(),
                child_order: Vec::new(),
                mesh_modifiers: Vec::new(),
                field_modifiers: Vec::new(),
            },
        );

        let mut hot = serialize_body_for_bake_hot(&sg, body, glam::Vec3::ZERO);
        // First real prim after SCOPE_OPEN is hot index 1.
        let base = HOT_FLOATS_PER_PRIM;
        assert_eq!(
            hot[base + 92].to_bits(),
            HEIGHT_MAP_NONE,
            "pre-pack hot[92] is NONE"
        );

        let atlas = serialize_height_maps_for_bake(&sg, body, &mut hot, 0);
        assert!(atlas.len() >= 4 + 4, "header + 2×2 samples, got {}", atlas.len());
        assert_eq!(atlas[0].to_bits(), 2);
        assert_eq!(atlas[1].to_bits(), 2);
        let start = hot[base + 92].to_bits();
        assert_ne!(start, HEIGHT_MAP_NONE, "hot[92] must point at packed map");
        assert_eq!(start, 0, "first map starts at atlas float 0");
        // Sample values in [0,1]
        assert!((atlas[4] - 0.0).abs() < 1e-5);
        assert!((atlas[7] - 1.0).abs() < 1e-5);

        // T3: draft max_side packs a smaller map.
        let mut big_pixels = vec![128u8; 32 * 32];
        big_pixels[0] = 0;
        big_pixels[32 * 32 - 1] = 255;
        let mut big_prim = SdfPrimitive::new(
            "big".into(),
            SdfShape::Sphere { radius: 1.0 },
            glam::Vec3::ZERO,
        );
        big_prim.modifiers.push(Modifier {
            mod_type: ModifierType::Displace,
            strength: 0.2,
            axis: 0,
            param2: 1.0,
            mask: VolumeMask::default(),
            height_map: Some(DisplaceHeightMap {
                w: 32,
                h: 32,
                pixels: big_pixels,
                path: String::new(),
            }),
        });
        big_prim.sync_modifiers_to_gpu_fields();
        let big_body = sg.add_child(
            root,
            NodePayload::SdfBody {
                name: "BigHm".into(),
                primitives: vec![big_prim],
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: glam::Vec3::ZERO,
                body_rotation: glam::Quat::IDENTITY,
                body_scale: glam::Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: glam::Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(),
                child_order: Vec::new(),
                mesh_modifiers: Vec::new(),
                field_modifiers: Vec::new(),
            },
        );
        let mut hot_draft = serialize_body_for_bake_hot(&sg, big_body, glam::Vec3::ZERO);
        let draft = serialize_height_maps_for_bake(&sg, big_body, &mut hot_draft, 4);
        assert!(draft.len() >= 4 + 1, "draft atlas has header+samples");
        let dw = draft[0].to_bits();
        let dh = draft[1].to_bits();
        assert!(dw <= 4 && dh <= 4, "draft mip ≤4, got {dw}×{dh}");
        let full_big = serialize_height_maps_for_bake_full(&sg, big_body, &mut hot_draft);
        assert_eq!(full_big[0].to_bits(), 32, "full pack keeps native width");

        // Empty body (no image) → 1-float wgpu filler.
        let plain = SdfPrimitive::new(
            "plain".into(),
            SdfShape::Sphere { radius: 0.5 },
            glam::Vec3::ZERO,
        );
        let plain_body = sg.add_child(
            root,
            NodePayload::SdfBody {
                name: "Plain".into(),
                primitives: vec![plain],
                mesh_cache_id: None,
                body_op: BooleanOp::Union { k: 0.0 },
                origin: glam::Vec3::ZERO,
                body_rotation: glam::Quat::IDENTITY,
                body_scale: glam::Vec3::ONE,
                mirror_flags: 0,
                mirror_sign: 0,
                mirror_center: glam::Vec3::ZERO,
                bevel_radius: 0.0,
                blend_profile: BlendProfile::default(),
                material: PbrMaterial::default(),
                child_order: Vec::new(),
                mesh_modifiers: Vec::new(),
                field_modifiers: Vec::new(),
            },
        );
        let mut plain_hot = serialize_body_for_bake_hot(&sg, plain_body, glam::Vec3::ZERO);
        let empty = serialize_height_maps_for_bake(&sg, plain_body, &mut plain_hot, 0);
        assert_eq!(empty.len(), 1, "empty atlas must be ≥1 float for wgpu");
    }

    #[test]
    fn serialize_nested_bodies_scope_markers() {
        use core_math::scene_graph::{SceneGraph, NodePayload};
        use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BlendProfile, PbrMaterial};
        use std::collections::HashSet;

        let mut sg = SceneGraph::new();
        let root = sg.root();

        // Parent body with 1 prim, child body with 1 prim (Subtract)
        let parent = sg.add_child(root, NodePayload::SdfBody {
            name: "Parent".into(),
            primitives: vec![
                SdfPrimitive::new("Sphere".into(), SdfShape::Sphere { radius: 1.0 }, glam::Vec3::ZERO),
            ],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: glam::Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: glam::Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let _child = sg.add_child(parent, NodePayload::SdfBody {
            name: "Cutter".into(),
            primitives: vec![
                SdfPrimitive::new("Cyl".into(), SdfShape::Cylinder { radius: 0.3, half_height: 1.0 }, glam::Vec3::ZERO),
            ],
            mesh_cache_id: None,
            body_op: BooleanOp::Subtract { k: 0.1 },
            origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: glam::Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: glam::Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let selected = HashSet::new();
        let textured = HashSet::new();
        let hidden = HashSet::new();
        let res = super::serialize_from_graph(&sg, &selected, &textured, &hidden, &HashMap::new(), super::SerializeMode::Full);
        let (count, bytes) = (res.num_entries, res.prim_bytes);

        // Decode bytes back to f32s for inspection
        let floats: Vec<f32> = bytes.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // Count scope markers and real primitives
        let mut scope_opens = 0u32;
        let mut scope_closes = 0u32;
        let mut real_prims = 0u32;
        for entry_idx in 0..count as usize {
            let base = entry_idx * super::FLOATS_PER_PRIM;
            let shape_type = f32::to_bits(floats[base + 3]) & 0xFF;
            match shape_type {
                254 => scope_opens += 1,
                255 => scope_closes += 1,
                _ => real_prims += 1,
            }
        }

        // Must have 2 real prims (Sphere + Cylinder)
        assert_eq!(real_prims, 2, "Expected 2 real primitives, got {}", real_prims);

        // Must have balanced scope markers
        assert!(scope_opens > 0, "No SCOPE_OPEN markers found");
        assert_eq!(scope_opens, scope_closes, "Unbalanced: {} opens vs {} closes", scope_opens, scope_closes);

        // Total count = real prims + opens + closes
        assert_eq!(count, real_prims + scope_opens + scope_closes,
            "Entry count mismatch: {} total but {} prims + {} opens + {} closes",
            count, real_prims, scope_opens, scope_closes);

        // Verify the child body's SCOPE_OPEN has Subtract op (op_type = 1)
        // Walk entries to find the child scope
        let mut found_subtract_scope = false;
        for entry_idx in 0..count as usize {
            let base = entry_idx * super::FLOATS_PER_PRIM;
            let shape_type = f32::to_bits(floats[base + 3]) & 0xFF;
            if shape_type == 254 {
                let op_type = f32::to_bits(floats[base + 11]);
                let blend_k = floats[base + 15];
                if op_type == 1 && (blend_k - 0.1).abs() < 0.01 {
                    found_subtract_scope = true;
                }
            }
        }
        assert!(found_subtract_scope, "No SCOPE_OPEN with op=Subtract k=0.1 found");
    }

    #[test]
    fn serialize_single_body_has_scope_markers() {
        use core_math::scene_graph::{SceneGraph, NodePayload};
        use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, BlendProfile, PbrMaterial};
        use std::collections::HashSet;

        // ALL bodies get scope markers (enables tile-based GPU culling).
        // A single top-level body with 1 prim → 3 entries: SCOPE_OPEN + prim + SCOPE_CLOSE.
        let mut sg = SceneGraph::new();
        let root = sg.root();
        sg.add_child(root, NodePayload::SdfBody {
            name: "Solo".into(),
            primitives: vec![
                SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 1.0 }, glam::Vec3::ZERO),
            ],
            mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 },
            origin: glam::Vec3::ZERO,
            body_rotation: glam::Quat::IDENTITY,
            body_scale: glam::Vec3::ONE,
            mirror_flags: 0,
            mirror_sign: 0,
            mirror_center: glam::Vec3::ZERO,
            bevel_radius: 0.0,
            blend_profile: BlendProfile::default(),
            material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

        let res = super::serialize_from_graph(
            &sg, &HashSet::new(), &HashSet::new(), &HashSet::new(), &HashMap::new(), super::SerializeMode::Full,
        );
        let (count, bytes) = (res.num_entries, res.prim_bytes);

        let floats: Vec<f32> = bytes.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        // 3 entries: SCOPE_OPEN(254) + Sphere(0) + SCOPE_CLOSE(255)
        assert_eq!(count, 3, "Expected 3 entries (open + prim + close), got {}", count);
        let st0 = f32::to_bits(floats[0 * super::FLOATS_PER_PRIM + 3]) & 0xFF;
        let st1 = f32::to_bits(floats[1 * super::FLOATS_PER_PRIM + 3]) & 0xFF;
        let st2 = f32::to_bits(floats[2 * super::FLOATS_PER_PRIM + 3]) & 0xFF;
        assert_eq!(st0, 254, "First entry should be SCOPE_OPEN (254), got {}", st0);
        assert!(st1 < 254, "Second entry should be a real prim, got {}", st1);
        assert_eq!(st2, 255, "Third entry should be SCOPE_CLOSE (255), got {}", st2);
        // SCOPE_OPEN op should be Union (0) for top-level body
        let open_op = f32::to_bits(floats[0 * super::FLOATS_PER_PRIM + 11]);
        assert_eq!(open_op, 0, "Top-level scope op should be Union (0), got {}", open_op);
    }
}
