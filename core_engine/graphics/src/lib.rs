pub const POST_PROCESS_WGSL: &str = include_str!("post_process.wgsl");
pub const VOLUME_BAKE_WGSL: &str = include_str!("volume_bake.wgsl");
pub const VOXEL_MESH_WGSL: &str = include_str!("voxel_mesh.wgsl"); // GPU Surface Nets (sculpt Phase 1)
pub const SDF_MESH_WGSL: &str = include_str!("sdf_mesh.wgsl"); // GPU QEF Dual Contouring (Phase G, shared mesher)
pub const VOXEL_BRUSH_WGSL: &str = include_str!("voxel_brush.wgsl"); // GPU brush (sculpt Phase 2)
// GPU-resident brush (sculpt Phase 5a): atlas<->box copies wrapping the unchanged brush kernel.
// Density-only kernels — NO SDF eval, NO boolean blend, so neutralize_blend_noise_wgsl() is not needed.
pub const VOXEL_BRUSH_GATHER_WGSL: &str = include_str!("voxel_brush_gather.wgsl");
pub const VOXEL_BRUSH_SCATTER_WGSL: &str = include_str!("voxel_brush_scatter.wgsl");
// Split shader files for per-entry-point compilation (faster startup)
// sdf_shared.wgsl split into 5 files: header + prim_eval + scene + material + lighting.
// Concatenated at build time by shader_build::sdf_shared_all().
pub(crate) const SDF_HEADER_WGSL: &str = include_str!("sdf_shared.wgsl");
pub(crate) const SDF_PRIM_EVAL_WGSL: &str = include_str!("sdf_prim_eval.wgsl");
pub(crate) const SDF_SCENE_WGSL: &str = include_str!("sdf_scene.wgsl");
pub(crate) const SDF_MATERIAL_WGSL: &str = include_str!("sdf_material.wgsl");
pub(crate) const SDF_LIGHTING_WGSL: &str = include_str!("sdf_lighting.wgsl");
// (RAYMARCH_ENTRY_WGSL / RAYMARCH_MARCH_WGSL REMOVED 2026-07-14 — analytical march display path stripped)
pub(crate) const DEFERRED_ENTRY_WGSL: &str = include_str!("deferred_entry.wgsl");
pub(crate) const RAYMARCH_LITE_WGSL: &str = include_str!("raymarch_lite.wgsl");
pub(crate) const PRIM_OUTLINE_ENTRY_WGSL: &str = include_str!("prim_outline_entry.wgsl");
pub(crate) const FOG_ENTRY_WGSL: &str = include_str!("fog_entry.wgsl");
// Triangle path tracer (Phase 2 — traces the raster triangles via TLAS/BLAS).
// The analytical PT entry (pathtrace_entry.wgsl) was DELETED 2026-07-16 after the
// tri PT shipped + was owner-verified — the analytical SDF is banned from PT.
pub(crate) const PATHTRACE_TRI_ENTRY_WGSL: &str = include_str!("pathtrace_tri_entry.wgsl");
// RT projected shadows (RT_SHADOWS_DESIGN Stage 1): fires occlusion rays against the
// triangle-PT BVH (shared PT_TRAVERSAL region of pathtrace_tri_entry.wgsl) → per-light
// visibility texture the deferred point/spot lighting multiplies in. Sun stays CSM.
pub(crate) const RT_SHADOW_ENTRY_WGSL: &str = include_str!("rt_shadow_entry.wgsl");
// Screen-space GI (SSGI_PLAN.md). Assembled by build_ssgi_wgsl() with the ENV and
// AO_UPSAMPLE regions extracted — never compiled standalone.
pub(crate) const SSGI_ENTRY_WGSL: &str = include_str!("ssgi_entry.wgsl");
// RT projected-shadow denoise (RT_SHADOWS_DESIGN Stage 2): spatial bilateral over the 4-channel
// visibility texture + temporal EMA reprojection. Both self-contained (no SDF header).
// GPU material-map bake (GPU_MATERIAL_BAKE_PLAN.md P1): evaluates a node graph's generated
// `eval_node_material` over the res² UV grid into storage textures. Self-contained — assembled
// with only `MatResult` + the graph's own generated WGSL (no SDF header, no prim/lighting stack).
// DC-mesh curvature (MESH_CURVATURE_PLAN M1): per-vertex curvature from the MESH's own face
// normals (computed from neighbouring vertex POSITIONS via the index buffer), not from the
// analytical SDF. Self-contained — no SDF header, no prim stream.
pub(crate) const MESH_CURVATURE_WGSL: &str = include_str!("mesh_curvature.wgsl");
pub(crate) const MATERIAL_BAKE_WGSL: &str = include_str!("material_bake.wgsl");
pub(crate) const RT_SHADOW_DENOISE_WGSL: &str = include_str!("rt_shadow_denoise.wgsl");
pub(crate) const RT_SHADOW_TEMPORAL_WGSL: &str = include_str!("rt_shadow_temporal.wgsl");
// (VOLUME_PREVIEW_WGSL DELETED 2026-07-17 — the SDF volume-raymarch display pipeline was
// stripped 2026-07-13; the shader source + build fn + BGL were the "later slice" leftovers.
// It carried the analytical per-body material read (read_body_material) — analytical-only,
// no consumer since the deletion.)
// Sculpt voxel raymarch (Plan B1): raymarch the resident sparse atlas during a brush stroke.
// Self-contained (no SDF eval injection) — isolated to the sculpt atlas, never the analytical path.
pub const SCULPT_VOXEL_RAYMARCH_WGSL: &str = include_str!("sculpt_voxel_raymarch.wgsl");
pub const BRICK_FILL_WGSL: &str = include_str!("brick_fill.wgsl");
pub const FXAA_WGSL: &str = include_str!("fxaa.wgsl");
pub const TEMPORAL_REPROJECT_WGSL: &str = include_str!("temporal_reproject.wgsl");
// PT temporal reprojection (P3.5): moves the PT accumulation under camera motion.
// Distinct from TEMPORAL_REPROJECT_WGSL (raster validity mask — invalidates on move).
pub const PT_REPROJECT_WGSL: &str = include_str!("pt_reproject.wgsl");
// Section I: warp ReSTIR DI reservoirs under camera motion (same guides as PT reproject).
pub const PT_RESTIR_REPROJECT_WGSL: &str = include_str!("pt_restir_reproject.wgsl");
pub const PT_DENOISE_WGSL: &str = include_str!("pt_denoise.wgsl");
// Section B: depth-aware half→full lighting upscale (interactive PT).
pub const PT_UPSCALE_WGSL: &str = include_str!("pt_upscale.wgsl");
// Environment GGX prefilter (split-sum IBL first term): one budgeted dispatch per mip,
// writes the env buffer's mips 1..6 in place. Self-contained (own uniform struct).
pub const ENV_PREFILTER_WGSL: &str = include_str!("env_prefilter.wgsl");
/// Auto reflection probe capture + mip prefilter (PROGRESSIVE_PBR_PLAN.md Phase 3).
pub const PROBE_WGSL: &str = include_str!("probe.wgsl");

// Phase D1 GPU instance culling: cs_reset_args + cs_frustum_cull compacting visible
// instances into per-batch indirect draw args. Self-contained (own uniform struct);
// BGL = bgl::instance_cull_bgl_entries().
pub const INSTANCE_CULL_WGSL: &str = include_str!("instance_cull.wgsl");

// Phase D2 HZB max-depth pyramid build (seed + 2x2 max downsample). Self-contained;
// BGL = bgl::hzb_build_bgl_entries().
pub const HZB_BUILD_WGSL: &str = include_str!("hzb_build.wgsl");

// Phase D4 cluster cut (reset + error/frustum/cone selection into indirect args).
// Self-contained; BGL = bgl::cluster_cut_bgl_entries().
pub const CLUSTER_CUT_WGSL: &str = include_str!("cluster_cut.wgsl");
pub mod gpu_volume;
pub mod volume_atlas;
pub mod sdf_brick_atlas; // Phase 3A slice 1: persistent sparse SDF brick atlas (index + pool)
pub mod brick_pool;
pub mod bevel_preview;
pub mod shader_build;
// Screen-space GI: pipelines + per-resolution resources + the single dispatch
// driver (SSGI_PLAN.md). Mirrors render/src/ssao.rs ownership.
pub mod ssgi;
pub mod types;
pub mod serialize;
pub mod bgl;
/// On-disk pipeline-cache envelope (header, version, size, checksum) and the
/// atomic temp+sync+rename writer. No GPU. Validated before `create_pipeline_cache`.
pub mod pipeline_cache_store;

// Headless GPU parity test for voxel_brush.wgsl (Phase-0 GATE): dispatches the real kernel and
// compares it to the CPU `apply_stamp_plan` for every built-in preset. Test-only.
#[cfg(test)]
mod voxel_brush_gpu_parity;
// Headless GPU readback probe for env_prefilter.wgsl (split-sum IBL): constant-env
// pass-through + roughness-1 directionality, via the PRODUCTION BGL. Test-only.
#[cfg(test)]
mod env_prefilter_gpu_test;
// Headless GPU readback proof for pass_ssr (SSR quality pass): cube on a mirror
// floor — hit confidence lands where the CPU mirror-trace oracle says, zero
// where nothing reflects, deterministic. Mirrors the production ssr_bgl wiring.
#[cfg(test)]
mod ssr_gpu_test;
// SSGI Gate 0 (docs/planning/render/SSGI_PLAN.md): the headless harness, the
// synthesized floor+red-wall G-buffer, the equirect environment with its SH9
// tail, and the INDEPENDENT CPU oracle for the §4.2 energy formula. Gate 3
// cross-checks the GPU pass against it — the review of plan revision 1 found
// that every gate without an oracle passed by construction.
#[cfg(test)]
mod ssgi_gpu_test;
// Headless GPU readback proof for the viewport Tonemapper selector: a gradient
// HDR image through every curve — monotonic, [0,1], filmic highlight rolloff vs
// Linear clipping, AgX hue-shift bound, deterministic, Classic == old default.
// Mirrors the production tonemap_bgl wiring.
#[cfg(test)]
mod tonemap_gpu_test;
#[cfg(test)]
mod vol_light_gpu_test;
// Headless GPU readback proof for the RT projected-shadows visibility pass
// (RT_SHADOWS_DESIGN Stage 1): a triangle occluder between the probe surface and a
// point light — the occluded probe reads ~0, the clear probe ~1, and the WGSL any-hit
// result matches the render::pt_accel CPU oracle (trace_scene_cpu) per pixel. Mirrors
// the production rt_shadow_bgl wiring.
#[cfg(test)]
mod rt_shadow_gpu_test;
#[cfg(test)]
mod pt_material_gpu_test;
#[cfg(test)]
mod pt_progressive_gpu_test;
// GPU equivalence gate: loop-form calc_curvature / sdf_central_gradient (DF-full cold-compile
// fix, 2026-10-13) vs the original unrolled scene_sdf calls.
#[cfg(test)]
mod sdf_probe_loop_gpu_test;
// White-furnace energy measurement of the PT surface BSDF (owner: "render is too dark").
#[cfg(test)]
mod pt_furnace_gpu_test;
#[cfg(test)]
mod direct_brdf_parity_gpu_test;
// Pure-Rust mirror of the WGSL dielectric Fresnel + refract math in
// pathtrace_tri_entry.wgsl: Snell's-law bend + TIR critical-angle checks. Deterministic,
// headless, no GPU. Keeps the shader's transmission lobe honest (PT_TRANSMISSION_PLAN §7 T1).
#[cfg(test)]
mod pt_dielectric_test;

pub use shader_build::*;
pub use types::*;
pub use serialize::*;
pub use sdf_brick_atlas::SdfBrickAtlasGpu;

// ============================================================
// G-Buffer Format Contract
// ============================================================
//
// Written by: render/src/gbuffer.wgsl        (raster — slots 0,1,2,4 as MRT attachments)
//             graphics/src/raymarch_lite.wgsl (floor + workplane + background — all 7)
//             graphics/src/sculpt_voxel_raymarch.wgsl (sparse atlas overlay — all 7)
// Read by:    deferred_entry.wgsl, post_process.wgsl (SSR/tonemap/DOF/outline),
//             fog_entry.wgsl, rt_shadow_entry.wgsl, render/src/ssao.wgsl
//
// Any change to channel semantics MUST be updated in EVERY writer above, in lockstep.
// (This header named raymarch_entry.wgsl + raymarch_bvh_full.comp until 2026-08-06;
// both were deleted 2026-07-14 — see CLAUDE.md § Analytical Path Status.)
//
// DEPTH CONVENTION (slot 0 .w) — ONE quantity, no exceptions:
//   the ray parameter `t` along camera_ray(), such that  p_world = origin + dir * t.
//   perspective : t == length(p_world - cam_pos)      (dir is normalized from cam_pos)
//   orthographic: t == dot(p_world - cam_pos, forward) (per-pixel origins are shifted
//                 perpendicular to forward, so the parameter is forward distance)
//   0.0 is the background sentinel — every consumer tests `depth > 0.0`.
// Producers: gbuf_depth() in render/src/gbuffer.wgsl. Consumers comparing a computed
// position against it MUST use the same select(): ssr_metric() in post_process.wgsl,
// sample_t in render/src/ssao.wgsl. Writing a Euclidean distance here while consumers
// reconstruct a ray parameter is the 2026-08-06 ortho bug — identical in perspective,
// silently wrong in orthographic (SSR reflections were ~zero in ortho views).
//
// Slot | Write  | Read   | Format       | Channels
// -----|--------|--------|--------------|-------------------------------------------
//  0   |  @9    |  @12   | Rgba16Float  | xyz=world normal, w=ray-parameter depth t (see above)
//  1   | @10    |  @13   | Rgba16Float  | x=roughness, y=metallic, z=shadow*AO, w=opacity+em_lum
//  2   | @11    |  @14   | Rgba16Float  | rgb=linear albedo, a=flags (0=bg 0.5=floor 0.78=geo 1.0=sel)
//  3   | @30    |  @31   | R32Uint      | r=hit prim UID (0xFFFFFFFF=none)
//  4   | @32    |  @33   | R32Float     | r=body_index (dense 0-based f32; C.5 exact past f16 2048)
//  5   | @34    |  @35   | Rgba16Float  | x=κ₁, y=κ₂ (principal curvatures), zw=0
//  6   | @36    |  @37   | Rgba16Float  | rgb=emission HDR, w=1.0
//
// Packing: opacity+emission stored as sum → opacity=min(w,1), em_lum=max(w-1,0).
// Shadow*AO stored as product. Surface ID 0xFFFFFFFF = non-geometry.
// body_index 0 is valid; selected_body_index 0xFFFFFFFF = none.
// ============================================================
pub mod gbuffer_format {
    /// Number of G-buffer render targets.
    pub const SLOT_COUNT: usize = 7;
    /// Write bindings (storage textures written by raymarch).
    pub const WRITE_NORMAL: u32 = 9;
    pub const WRITE_MATERIAL: u32 = 10;
    pub const WRITE_ALBEDO: u32 = 11;
    pub const WRITE_SURFACE_ID: u32 = 30;
    pub const WRITE_MOTION: u32 = 32;
    pub const WRITE_CURVATURE: u32 = 34;
    pub const WRITE_EMISSION: u32 = 36;
    /// Read bindings (textures read by deferred).
    pub const READ_NORMAL: u32 = 12;
    pub const READ_MATERIAL: u32 = 13;
    pub const READ_ALBEDO: u32 = 14;
    pub const READ_SURFACE_ID: u32 = 31;
    pub const READ_MOTION: u32 = 33;
    pub const READ_CURVATURE: u32 = 35;
    pub const READ_EMISSION: u32 = 37;
    /// C.5: body_index is R32Float (exact ints to 2^24; keeps 4-MRT raster ≤32 B/sample).
    pub const FORMAT_MOTION: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
}


#[cfg(test)]
mod pt_map_support_gate {
    /// OWNER 2026-07-29 → P4 / HANDOFF Decision 3 (2026-07-30):
    /// PT now samples normal_map + bump_map (whiteout triplanar + FD bump), matching Hybrid.
    ///
    /// This test was inverted deliberately when the feature landed: it previously pinned the
    /// *gap*; it now pins that the gap is closed. Do not flip it back without an ADR.
    #[test]
    fn pathtracer_samples_albedo_roughness_metallic_but_not_normal_or_bump() {
        let src = include_str!("pathtrace_tri_entry.wgsl");
        // Primary shading section (first albedo sample) + helpers that apply normal maps.
        let start = src.find("let amap_slot").expect("PT albedo sampling moved");
        let shading = &src[start..];

        for m in ["albedo_map", "roughness_map", "metallic_map"] {
            assert!(shading.contains(m), "PT no longer samples {m} — a regression, not a cleanup");
        }
        // P4: normal / bump helpers + primary call (helpers sit above the amap_slot section).
        assert!(
            src.contains("pt_sample_mat_triplanar_normal")
                && src.contains("mat.normal_map"),
            "PT must sample normal_map via whiteout triplanar (P4 Decision 3)"
        );
        assert!(
            src.contains("pt_apply_normal_maps") && src.contains("mat.bump_map"),
            "PT must apply bump_map through pt_apply_normal_maps (P4)"
        );
        assert!(
            shading.contains("pt_apply_normal_maps(hit, mat"),
            "primary bounce must call pt_apply_normal_maps"
        );
    }
}

#[cfg(test)]
mod sun_direction_convention_gate {
    /// RASTER-PT-BRIGHTNESS-GAP (2026-10-13, AZRA_PARITY gate): a directional light's packed
    /// direction (light slot 4..6 = `LightData::direction()`) points FROM the surface TO the sun.
    /// The deferred PBR loop, the RT shadow rays and the CSM all read it that way; the triangle
    /// path tracer negated it at every directional site, so PT lit the ANTI-sun hemisphere
    /// (sun-facing centre of a sphere: raster 0.48, PT 0.00 linear). Every consumer must use
    /// the same, un-negated convention.
    #[test]
    fn every_renderer_reads_directional_light_dir_as_to_sun() {
        let pt = include_str!("pathtrace_tri_entry.wgsl");
        assert!(
            !pt.contains("= -light_dir_v("),
            "pathtrace_tri_entry.wgsl negates a directional light_dir_v — that lights the anti-sun hemisphere"
        );
        assert!(pt.contains("ldir = light_dir_v(li);"), "PT directional sampling site moved");
        let deferred = include_str!("deferred_entry.wgsl");
        assert!(
            deferred.contains("l_dir = light_dir_v(li);") && !deferred.contains("l_dir = -light_dir_v("),
            "deferred directional N·L must read light_dir_v as the to-sun vector"
        );
        let rt = include_str!("rt_shadow_entry.wgsl");
        assert!(rt.contains("light_dir_v(i), is_dir)"), "RT sun shadow rays must trace toward light_dir_v");
    }
}
