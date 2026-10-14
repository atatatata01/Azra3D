// ============================================================
// Shared Bind Group Layout (BGL) Descriptors
// ============================================================
//
// Extracted from main.rs to eliminate copy-paste between startup and
// background thread BGL creation. wgpu BGLs are not Clone, so both
// sites must create their own — but from identical descriptors.

use wgpu;
use wgpu::TextureFormat as TF;

// ---- Entry helpers (one per wgpu binding type) ----

pub fn tex(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub fn sto(b: u32, fmt: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: fmt,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

pub fn uni(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub fn buf_ro(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Read-write storage buffer (compute output).
pub fn buf_rw(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Write-only 3D storage texture (volume compute output). D3 counterpart of `sto`.
pub fn sto3d(b: u32, fmt: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: fmt,
            view_dimension: wgpu::TextureViewDimension::D3,
        },
        count: None,
    }
}

/// Filterable cube texture (reflection probe).
pub fn tex_cube(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::Cube,
            multisampled: false,
        },
        count: None,
    }
}

/// Filtering sampler (pairs with `tex_cube`).
pub fn samp(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

pub fn tex_uint(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub fn tex3d(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
        count: None,
    }
}

pub fn sampler(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// Filterable D2-array texture (material PBR map array).
pub fn tex_array(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

// ---- Shared BGL descriptors (used by both startup and background thread) ----

/// Raymarch G-buffer pipeline BGL entries.
/// Bindings: uniforms(1), prim(2), cold_prim(3), tex(4), scope(5), scope_stack(6),
///   gbuf normal(9), material(10), albedo(11), height_maps(15) — T1 image displace atlas,
///   lights(22), light_links(23),
///   raster_depth(29), surface_id(30), motion(32), curvature(34), emission(36),
///   temporal_mask(44), bvh_uniforms(45).
pub fn raymarch_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        uni(1), buf_ro(2), buf_ro(3), buf_ro(4), buf_ro(5), buf_ro(6),
        sto(9, TF::Rgba16Float), sto(10, TF::Rgba16Float), sto(11, TF::Rgba16Float),
        buf_ro(15), // T1 height_maps (sdf_prim_eval sample_height_map_live)
        buf_ro(22), buf_ro(23), tex(29), sto(30, TF::R32Uint),
        // C.5: motion is R32Float (body_index exact past f16's 2048; MRT byte budget)
        sto(32, TF::R32Float), sto(34, TF::Rgba16Float), sto(36, TF::Rgba16Float),
        tex_uint(44), uni(45),
    ]
}

/// Deferred shading pipeline BGL entries.
/// Bindings: output(0), uniforms(1), voxel(2), gbuf normal(12), material(13),
///   albedo(14), DNA/env(21), prim(3), cold_prim(4), height_maps(15),
///   tex_atlas(22), tex_meta(23),
///   lights(26), light_cluster(24 — P3.1 Phase D tile×Z lists), raster_depth(29),
///   surface_id(31), motion(33), curvature(35), emission(37), ssao(38),
///   brdf_lut(39), rt_shadow_vis(40 pack0), light_links(41), matcap(42),
///   rt_shadow_vis_hi(50 pack1), light_probe_sh(51 — auto light probe irradiance).
pub fn deferred_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sto(0, TF::Rgba16Float), uni(1), buf_ro(2),
        tex(12), tex(13), tex(14),
        buf_ro(21), buf_ro(3), buf_ro(4),
        // T1 height_maps (sdf_prim_eval). NOT strippable here: deferred calls
        // scene_sdf() for SDF reflections → eval_prim → sample_height_map_live,
        // so Naga keeps binding 15 live and the pipeline layout MUST declare it.
        // Omitting it made create_compute_pipeline fail and every frame die on
        // set_pipeline("deferred") → invalid encoder → frozen viewport.
        buf_ro(15),
        buf_ro(22), buf_ro(23), buf_ro(26),
        buf_ro(24), // P3.1 Phase D light cluster lists
        tex(29), tex_uint(31), tex(33), tex(35), tex(37), tex(38), buf_ro(39),
        tex(40),
        buf_ro(41), buf_ro(42),
        tex(50), // P2.2 RT pack1 (lights 4..7)
        // Auto light probe SH9 irradiance (Phase 3b, probe_bake.rs): 10×vec4 —
        // [0..8] = RGB coefficients, [9] = (strength, 0, 0, valid).
        buf_ro(51),
        // Probe PREVIEW balls (post_flags bit 2048, "Show Probes"): the probe
        // cube + sampler + params, same resources ssr_bgl carries at 65/66/67 —
        // deferred ray-traces a chrome ball (cube sample) and an irradiance ball
        // (SH eval) at the probe center so the user can SEE both probe contents.
        tex_cube(65), samp(66), uni(67),
    ]
}

/// Volumetric fog pass BGL entries (`pass_fog`, `build_fog_wgsl`).
///
/// SINGLE SOURCE. This layout was previously hand-written in THREE places that had to be
/// kept in sync by comment ("MUST match bg_fog_bgl in app_update.rs") — `app_new.rs`
/// `fog_bgl`, `app_update.rs` `bg_fog_bgl` (background pipeline thread), and implicitly the
/// `fog_bg` bind group. That is exactly how binding 15 went missing and left the fog
/// pipeline invalid (tracker DFBGL15, 2026-08-09). Add bindings HERE and nowhere else.
///
/// Bindings: config(1), voxel(2), prim(3), height_maps(15), gbuf_normal(12),
///   fog_out(17), fog_prev(24), fog_prev_camera(25), lights(26).
///
/// `height_maps` (15) is NOT optional: `FOG_EXCLUDE` strips `SCENE_QUERY` but neither
/// `SCENE_EVAL` nor `SDF_PRIM_EVAL`, so `pass_fog` reaches
/// `scene_sdf` → `eval_prim` → `sample_height_map_live` and Naga keeps the binding live.
///
/// binding 26 = light_data (read-only storage): per-light VOLUMETRIC in-scatter (god rays).
pub fn fog_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        uni(1), buf_ro(2), buf_ro(3), buf_ro(15),
        tex(12), sto(17, TF::Rgba16Float), tex(24), uni(25), buf_ro(26),
    ]
}

/// RT projected-shadows visibility pass BGL entries (RT_SHADOWS_DESIGN Stage 1 + P2.2 dual pack).
/// Do NOT extend `deferred_bgl_entries`, whose 41=env / 42=matcap collide with the PT tlas=42.
/// Pack0 output(0), pack1 output(28), config(1), gbuf_normal(12), rt_params(25), light(26),
/// BVH tlas(42)/instances(43)/blas(44)/tris(45)/verts(46)/meta(47).
pub fn rt_shadow_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sto(0, TF::Rgba16Float), uni(1), tex(12), uni(25), buf_ro(26),
        sto(28, TF::Rgba16Float), // P2.2 pack1 (lights 4..7)
        buf_ro(42), buf_ro(43), buf_ro(44), buf_ro(45), buf_ro(46), uni(47),
    ]
}

/// Screen-space GI (SSGI_PLAN.md §4.9). ONE layout shared by all three entries
/// (`ssgi_gather`, `ssgi_blur_h`, `ssgi_blur_v`) — a binding unused by one entry
/// must still be present, exactly like the SSAO module's shared layout.
///
/// 0  = half-res delta out (the sdf_shared header's `output_texture`)
/// 1  = GpuUniforms
/// 5  = deferred-lit HDR (the radiance the gather bounces)
/// 12/13/14 = G-buffer normal+depth / material / albedo+flags
/// 29 = raster_depth snapshot — the `is_raster` receiver gate; deferred forces
///      `final_ao = 1.0` only on raster pixels, and the delta's cancellation
///      assumes it. Omitting this binding is how the pass silently over-subtracts
///      on sculpt-atlas geometry.
/// 21 = env_data (raw radiance + the SH9 tail)
/// 38 = the half-res AO factor, for `ssao_factor` / `multi_bounce_ao`
/// 60 = blur ping-pong source
/// 61 = SsgiParams
pub fn ssgi_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sto(0, TF::Rgba16Float), uni(1), tex(5),
        tex(12), tex(13), tex(14), tex(29),
        buf_ro(21), tex(38), tex(60), uni(61),
    ]
}

/// Depth-sampled 2D texture entry (Depth32Float attachment read in compute —
/// `texture_depth_2d` in WGSL; the line_overlay precedent).
pub fn tex_depth(b: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Depth,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Phase D2 HZB seed BGL (`hzb_build.wgsl` cs_hzb_seed): params uniform(0), raster depth
/// attachment `texture_depth_2d`(1), HZB mip 0 R32Float storage(2).
pub fn hzb_seed_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), tex_depth(1), sto(2, TF::R32Float)]
}

/// Phase D2 HZB downsample BGL (`hzb_build.wgsl` cs_hzb_downsample): params uniform(0),
/// HZB mip N R32Float storage(2), HZB mip N-1 sampled(3). One bind group per mip level
/// at the dispatch site (same entries, different views). Mirrored ENTRY-FOR-ENTRY.
pub fn hzb_down_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), sto(2, TF::R32Float), tex(3)]
}

/// Phase D4 cluster-cut BGL entries (`cluster_cut.wgsl`): params uniform(0),
/// instances read(1 — the shared raster instance buffer), clusters read(2),
/// draw_args rw(3 — INDIRECT|STORAGE), counters rw(4). Mirrored ENTRY-FOR-ENTRY.
pub fn cluster_cut_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), buf_ro(1), buf_ro(2), buf_rw(3), buf_rw(4)]
}

/// Phase D1 instance-cull BGL entries (`instance_cull.wgsl`): params uniform(0),
/// instances read(1), cull_meta read(2), batch_meta read(3), draw_args rw(4 —
/// INDIRECT|STORAGE buffer), visible_instances rw(5 — VERTEX|STORAGE buffer).
/// Mirrored ENTRY-FOR-ENTRY by every dispatch site (the lab-cache lesson).
/// D2 additions: hzb pyramid tex(6), occluded_flags rw(7), counters rw(8).
pub fn instance_cull_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), buf_ro(1), buf_ro(2), buf_ro(3), buf_rw(4), buf_rw(5), tex(6), buf_rw(7), buf_rw(8)]
}

/// RT-shadow spatial + temporal denoise BGL entries (RT_SHADOWS_DESIGN Stage 2). Self-contained
/// passes (NOT built from the SDF header) — every binding is local. Spatial: in_vis(0),
/// gbuf_normal(1), out_vis(2), params(3). Temporal reprojection reuses the SAME layout shape but
/// with different resources (see build sites); kept as its own entries fn for a single source.
pub fn rt_denoise_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![tex(0), tex(1), sto(2, TF::Rgba16Float), uni(3)]
}

/// RT-shadow temporal reprojection BGL entries (RT_SHADOWS_DESIGN Stage 2). cur_normal_dist(0),
/// params(1), prev_normal_dist(2), prev_vis history(3), cur_vis denoised(4), out_vis(5).
pub fn rt_temporal_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![tex(0), uni(1), tex(2), tex(3), tex(4), sto(5, TF::Rgba16Float)]
}

/// Env GGX prefilter BGL entries (env_prefilter.wgsl): params uniform(0) +
/// env buffer read_write(1). Dispatched from poll, one mip per frame.
pub fn env_prefilter_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), buf_rw(1)]
}

/// Blend/composite pipeline BGL entries.
/// Bindings: tex_base(0), tex_overlay(1), tex_output(2), params(3).
pub fn blend_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![tex(0), tex(1), sto(2, TF::Rgba16Float), uni(3)]
}

/// Path-trace pipeline BGL entries (single source — startup `app_new.rs` and the
/// background compile thread `app_update.rs` both call this; they used to carry
/// two hand-maintained inline copies).
///
/// Legacy analytical-PT bindings (kept while the old entry still compiles):
///   output HDR(0), uniforms(1), voxel(2), prim(3), tex(4), env(21),
///   tex_atlas(22), tex_meta(23), lights(26), pt_accum_in(27),
///   dummy volume tex(40) + sampler(41).
/// Triangle-PT scene (Phase 2 — the production PT):
///   tlas(42), instances(43), blas_nodes(44), tris(45), verts(46), pt_meta(47),
///   materials(48), mat_tex_array(49), mat_tex_sampler(50), pt_stats atomics(51),
///   restir_prev(52 tex rgba32f), restir_out(53 sto rgba32f) — Section G ReSTIR DI,
///   env_cdf(54) — Section K luminance CDF for HDRI importance sampling,
///   gi_prev(55 tex rgba16f), gi_out(56 sto rgba16f) — Section R temporal 1-bounce GI,
///   gbuf_normal(57) — Section T normal/depth guides for GI spatial,
///   gbuf_albedo(58) — Section U albedo guide for GI spatial.
pub fn pt_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        sto(0, TF::Rgba16Float), uni(1), buf_ro(2), buf_ro(3), buf_ro(4),
        buf_ro(21), buf_ro(22), buf_ro(23), buf_ro(26), tex(27),
        tex3d(40), sampler(41),
        // Triangle scene (Phase 2)
        buf_ro(42), buf_ro(43), buf_ro(44), buf_ro(45), buf_ro(46), uni(47),
        // PT materials (RasterMaterial array — same layout the raster pass uses)
        buf_ro(48),
        // Material PBR texture array (albedo/normal/rough/metal/emission/bump/disp maps)
        tex_array(49), sampler(50), tex_array(59),
        // Adaptive stop atomics (noisy pixel count + total)
        buf_rw(51),
        // Section G: ReSTIR DI reservoirs (prev frame read + this frame write)
        tex(52),
        sto(53, TF::Rgba32Float),
        // Section K: env luminance CDF (marginal + conditional)
        buf_ro(54),
        // Section R/AH: GI path reservoirs (oct.xy, W, M)
        tex(55),
        sto(56, TF::Rgba16Float),
        // Section T/U: full-res G-buffer guides for GI spatial edge-stop
        tex(57),
        tex(58),
        sto(60, TF::Rgba32Float),
        buf_rw(61),
    ]
}

/// PT temporal reprojection BGL entries (pt_reproject.wgsl, P3.5).
/// Bindings: cur_normal_dist(0), params(1), prev_normal_dist(2),
///   prev_accum(3), out_accum storage rgba16f(4).
pub fn pt_reproject_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![tex(0), uni(1), tex(2), tex(3), sto(4, TF::Rgba32Float), buf_ro(5), buf_rw(6)]
}

/// Section I: ReSTIR reservoir reprojection under camera motion.
/// Same guide layout as pt_reproject; prev/out are rgba32f reservoirs.
/// Bindings: cur_nd(0), params(1), prev_nd(2), prev_restir(3), out_restir sto rgba32f(4).
pub fn pt_restir_reproject_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![tex(0), uni(1), tex(2), tex(3), sto(4, TF::Rgba32Float)]
}

/// Section J / P3.6: PT SVGF-lite denoise.
/// Bindings: noisy(0), gbuf_normal(1), out_hdr sto rgba16f(2), params(3), gbuf_albedo(4).
pub fn pt_denoise_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        tex(0),
        tex(1),
        sto(2, TF::Rgba16Float),
        uni(3),
        tex(4),
        buf_ro(5),
    ]
}

// (vol_preview_bgl_entries DELETED 2026-07-17 — the "later slice" promised by the 2026-07-13
// VP-pipeline strip: shader source (volume_preview.wgsl), build fn, and this BGL are gone.)

/// Volume bake compute pipeline BGL entries (volume_bake.wgsl, entry `bake_volume`).
/// SINGLE SOURCE for every pipeline compiled from VOLUME_BAKE_WGSL: primary (lazy +
/// background-compiled + hot-swap), worker GPUs, and the GPU bake tests. Bindings:
///   prim_hot(0), params(1), output_grid(2), output_tex 3D r32f(3), clip_curves(4),
///   mesh_grid(5) — self-describing mesh-SDF operand grid
///   (header floats [res, min.xyz, cell_size, 3 reserved] + res^3 distances; res=0 disables;
///   bind a small zeroed dummy when absent),
///   brick_index(6) — SparseBricks (output_mode=2) brick→pool-handle map (u32 tags; binding 2
///   is the pool arena in that mode). Only mode 2 reads it — modes 0/1 bind a 4-byte filler
///   (the mesh_grid dummy works). Own binding by design: a sparse body can carry MeshVolume
///   prims, so mesh_grid(5) must stay live in mode 2 — the lab↔cache merge's interim
///   share-binding-5 bitcast corrupted every sparse bake.
///   curv_positions(7) — `curvature_eval` sample positions (body-local xyz, stride
///   `brick_dim` floats). ONLY the curvature entry reads it, and only in explicit-position mode
///   (seed_mode 0); the planar/texture mode derives its seeds from the AABB on-device. Every
///   other pipeline compiled from this layout binds a 4-byte filler here (the same filler the
///   dense modes already bind at 6) — the binding must EXIST at every site or
///   `create_compute_pipeline` fails layout validation, the bake_tex_dummy precedent.
///   height_maps(8) — T1 image displace atlas (packed headers + f32 heights; res=0/empty
///   dummy when no maps). hot[92] indexes into this buffer.
pub fn bake_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        buf_ro(0),
        uni(1),
        buf_rw(2),
        sto3d(3, TF::R32Float),
        buf_ro(4),
        buf_ro(5),
        buf_ro(6),
        buf_ro(7),
        buf_ro(8),
    ]
}

/// Sculpt voxel raymarch pipeline BGL entries (Plan B1).
/// Raymarches the resident SPARSE atlas (index tags + brick pool) during a brush stroke,
/// writing color+normal+depth into the SAME G-buffer slots as volume_preview (identical
/// formats/visibility). Bindings:
///   uniforms(1), atlas_index storage-ro(2), atlas_pool storage-ro(7), sculpt params uni(4),
///   gbuf normal(9), material(10), albedo(11), raster_depth tex(29),
///   surface_id(30), motion(32), curvature(34), emission(36).
pub fn sculpt_raymarch_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        uni(1), buf_ro(2), buf_ro(7), uni(4),
        sto(9, TF::Rgba16Float), sto(10, TF::Rgba16Float), sto(11, TF::Rgba16Float),
        tex(29), sto(30, TF::R32Uint),
        // C.5: motion R32Float
        sto(32, TF::R32Float), sto(34, TF::Rgba16Float), sto(36, TF::Rgba16Float),
    ]
}

// NOTE (merge lab↔cache): a second `bake_bgl_entries` was removed here to fix a duplicate
// definition. Binding 5 is dual-purpose across the two merged features — `mesh_grid`
// (poly-mesh-SDF operand, the shader's current decl) and `brick_index` (sparse mode-2
// brick→pool-handle map). Both are storage-read `buf_ro(5)`, so the BGL layout is identical
// and the single definition above serves both. The SHADER-side reconciliation (which data
// binding 5 carries per bake mode) is owned by the cache + lab windows — see
// docs/planning/lab/poly-mesh-sdf-operand.md and the sparse-3a bake path.

/// BGL for the DC-mesh curvature kernel (MESH_CURVATURE_PLAN M1). Single source of truth —
/// production and tests both build the layout from here, so a binding added to
/// `mesh_curvature.wgsl` cannot silently disagree with the pipeline (CLAUDE.md "BGL Single
/// Source"). 0=params 1=verts 2=indices 3=vertex→triangle adjacency 4=output.
pub fn mesh_curvature_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), buf_ro(1), buf_ro(2), buf_ro(3), buf_rw(4), buf_rw(5), buf_rw(6), buf_rw(7), buf_rw(8)]
}

/// GPU material-map bake BGL (`material_bake.wgsl` T1 dual-plane).
/// binding(0) uniform `MatBakeParams`, binding(1..4) = 4× **rgba16float** write-only storage
/// (albedo/roughness/metallic/emission) — LINEAR values; host encodes dual-plane 16-bit.
pub fn material_bake_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        uni(0),
        sto(1, TF::Rgba16Float),
        sto(2, TF::Rgba16Float),
        sto(3, TF::Rgba16Float),
        sto(4, TF::Rgba16Float),
        buf_ro(5),
    ]
}

pub fn material_resident_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut output = sto(1, wgpu::TextureFormat::Rgba8Unorm);
    if let wgpu::BindingType::StorageTexture { ref mut view_dimension, .. } = output.ty {
        *view_dimension = wgpu::TextureViewDimension::D2Array;
    }
    vec![uni(0), output, buf_ro(5)]
}

pub fn poly_surface_entry() -> wgpu::BindGroupLayoutEntry { buf_ro(2) }

pub fn poly_curvature_cache_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![uni(0), buf_ro(1), buf_ro(2), buf_rw(3), buf_ro(4), buf_ro(5)]
}

pub fn material_resize_bgl_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut entries = material_resident_bgl_entries();
    entries.pop();
    entries.push(buf_ro(3));
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2Array, multisampled: false,
        }, count: None,
    });
    entries
}
