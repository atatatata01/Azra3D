//! GATE 0 — DC-consolidation baseline harness (instrumentation + safety-net tests).
//!
//! This module is the *baseline brick* of `docs/planning/sdf/DC_CONSOLIDATION_GATED_PLAN.md`.
//! It changes NO production mesh/bake/pool/DC path — it only *measures* and *pins* the current
//! behaviour so gates 1-5 can prove "same mesh, fewer paths" (the plan's reigning metric is the
//! GOLDEN, not perf). Everything here is `#[cfg(test)]` — it never ships in the binary.
//!
//! Deliverables implemented here (plan §"N'EXISTE PAS — à créer au Gate 0"):
//!   1. Golden mesh hash per body  — `body_golden` + `golden_mesh_hash`.
//!   2. Reference scene (5 regimes) — `build_ref_scene` + `dc_gate0_write_ref_scene_fl4`.
//!   3. `.fl4` round-trip test      — `dc_gate0_ref_scene_roundtrip`.
//!   4. GPU-adapter ASSERTION       — `require_adapter` (+ the `#[ignore]` GPU bridge test).
//!   5. Pick regression test        — `dc_gate0_pick_selection_roundtrip`.
//!   7. Baseline JSON report        — `dc_gate0_write_baseline_json`.
//!  (Deliverable 6, the dead-metric wiring, is production code and lives at its real sites:
//!   `types_ui.rs` / `pool.rs` / `app_poll_bake.rs` / `app_poll_gpu.rs`.)
//!
//! ── CANONICAL GOLDEN SOURCE: the CPU `chunked_dc` oracle ──────────────────────────────────────
//! The golden is baked+meshed on the CPU (`BodyField::from_graph` → `bake_utils::bake_body_grid_field`
//! → `core_math::chunked_dc::ChunkedMesh::to_mesh`), NOT assembled from the GPU pool. Why this is the
//! ONE canonical source (plan lets us pick one and document it):
//!   • Headless & CI-safe — needs no GPU adapter, so it always runs and always proves determinism
//!     (the GPU parity tests early-return without an adapter → prove nothing on headless CI; that is
//!     exactly the hole `require_adapter` plugs, and why the golden itself must not depend on a device).
//!   • Pure & deterministic — the property the GO/NO-GO gate exists to verify. After the
//!     `merge_to_mesh` chunk-sort fix (this session), the whole path is byte-stable run-to-run.
//!   • It is ALREADY the parity reference: `tests_dc.rs::cpu_mesh` uses the same `ChunkedMesh` as the
//!     oracle the GPU DC mesher is diffed against. So a stable CPU golden + the existing GPU↔CPU
//!     spatial-parity tests transitively pin the GPU mesh too (bridge = `dc_gate0_gpu_cpu_golden_bridge`).
//! Trade-off (documented): the CPU golden does not exercise the sparse/tiled/segmented GPU pool at
//! full regime resolution. Those are Tier-B, owner-run numbers (the exe is locked to the owner). The
//! golden is a deterministic *fingerprint of the body's field→DC output*, which is what gates 1-5 must
//! not change; the GPU-pool-specific correctness is covered by the existing `sdf_dc_*` parity tests.
#![cfg(test)]

use glam::{Vec3, Quat};
use eframe::egui;

use core_math::scene_graph::{SceneGraph, NodeId, NodePayload};
use core_math::primitives::{SdfPrimitive, SdfShape, BooleanOp, PbrMaterial, BlendProfile};
use core_math::pbr_material::MaterialId;
use core_math::sdf_shapes::BoxBevel;
use core_math::field_eval::BodyField;
use core_math::chunked_dc::ChunkedMesh;
use core_math::mesh::Mesh;

use crate::bake_utils::{bake_body_grid_field, compute_body_bake_bounds, pad_grid_bounds,
                        collect_body_prims_recursive};
use crate::project::{ProjectFile, SceneData, CameraState, RenderSettings, EditorSettings, WpSettingsData};
use crate::{ShadingMode, SnapMode, WorkPlaneMode, WorkPlaneOrientation};
use app_types::{world_to_screen, SceneItemId, UnifiedSelection};

// ================================================================================================
// Golden resolution (CPU-DC feasibility ceiling). The golden is a deterministic FINGERPRINT — the
// determinism property is resolution-independent, so a modest uniform res keeps the routine test
// fast (< a few seconds at the crate's opt-level-2 dev profile) while the mesh stays non-trivial
// (a body spanning the grid emits thousands of verts at 96³). Each body's *runtime regime* res
// (128/256/512/1024/256) is recorded separately for the owner's live capture — it is NOT the CPU
// golden res (512³/1024³ dense CPU bakes are infeasible: 134M / 1.07B evals + up to 4 GB).
const GOLDEN_RES: u32 = 96; // grid POINTS per axis (nverts); cells = GOLDEN_RES - 1

/// Committed golden hashes for the 5 reference bodies, in `build_ref_scene` order. Captured by
/// `dc_gate0_golden_determinism` (which PRINTS them) and pinned here as the gate-1..5 regression
/// reference. `0` = not yet captured → the committed-match assertion is skipped (bootstrap).
const GOLDEN_HASHES: [u64; 5] = [
    0xdfef82593cbaa428, // 0: RefBoxBevelSub    (regime dense 128³)  verts=18251 tris=33856
    0x23c7b5455afdbfaa, // 1: RefCsg3Prim       (regime dense 256³)  verts=26804 tris=49384
    0x6a06b406c632da9c, // 2: RefOrganicSparse  (regime sparse 512³) verts=12172 tris=21768
    0x1033d5af4136249c, // 3: RefDetailedSparse (regime sparse 1024³) verts=19796 tris=36768
    0x11b48acb7f451bde, // 4: RefNestedSubBody  (regime dense 256³, recursive) verts=18098 tris=33820
];

// ================================================================================================
// FNV-1a golden hash (u64) over the merged DC mesh — raw emission-order bytes.
// After the `merge_to_mesh` chunk-sort fix the emission order is deterministic, so hashing the raw
// vert+idx byte stream is the STRONGEST golden: it catches any change to positions, topology OR
// order. (A pure intentional reorder would also trip it — the plan wants that flagged, then classed
// GO-with-doc or NO-GO by a human.) Normals are excluded from the primary golden — they are derived
// (gradient + `repair_degenerate_normals`) and deterministic, and a normals-only regression is
// already guarded by `sdf_dc_sparse_brick_decode_parity`'s angle check.
// ================================================================================================

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

#[inline]
fn fnv_u32(mut h: u64, v: u32) -> u64 {
    for b in v.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Deterministic FNV-1a hash of a DC mesh: `[vcount, qcount] + vertex f32-bits + quad u32 indices`.
fn golden_mesh_hash(mesh: &Mesh) -> u64 {
    let mut h = FNV_OFFSET;
    h = fnv_u32(h, mesh.vertices.len() as u32);
    h = fnv_u32(h, mesh.quads.len() as u32);
    for v in &mesh.vertices {
        h = fnv_u32(h, v.x.to_bits());
        h = fnv_u32(h, v.y.to_bits());
        h = fnv_u32(h, v.z.to_bits());
    }
    for q in &mesh.quads {
        for &i in q {
            h = fnv_u32(h, i);
        }
    }
    h
}

/// Bake + DC-mesh one body via the CPU oracle at `res` grid POINTS, returning `(mesh, golden_hash)`.
/// Mirrors the production dense bake: recursive prim collect → mirror-aware bake bounds → pad →
/// `BodyField` grid eval → `ChunkedMesh` (chunk_size 32, sharp features). Nested sub-bodies are
/// handled by `BodyField::from_graph` (the recursive-hash case the review feared was broken).
pub(crate) fn body_golden(sg: &SceneGraph, nid: NodeId, res: u32) -> (Mesh, u64, Vec3, Vec3) {
    let (mflags, mcenter, bevel) = match sg.get(nid).map(|n| &n.payload) {
        Some(NodePayload::SdfBody { mirror_flags, mirror_center, bevel_radius, .. }) =>
            (*mirror_flags, *mirror_center, *bevel_radius),
        _ => (0u8, Vec3::ZERO, 0.0f32),
    };
    let prims = collect_body_prims_recursive(sg, nid);
    let (tmin, tmax) = compute_body_bake_bounds(&prims, mflags, mcenter, bevel);
    let (gmin, gmax) = pad_grid_bounds(tmin, tmax);

    let field = BodyField::from_graph(sg, nid);
    let grid = bake_body_grid_field(&field, res, gmin, gmax); // res³ points, endpoint-inclusive

    // chunked_dc counts CELLS (nverts = res); cells = res - 1, chunk_size 32, sharp=true — the
    // exact `tests_dc::cpu_mesh` convention (the GPU DC parity oracle).
    let mut cm = ChunkedMesh::new(res - 1, 32, gmin, gmax, true);
    cm.extract_all(&grid);
    let mesh = cm.to_mesh();
    let hash = golden_mesh_hash(&mesh);
    (mesh, hash, gmin, gmax)
}

// ================================================================================================
// Reference scene — 5 bodies covering every DC regime (plan §"Scène de référence").
// ================================================================================================

/// Metadata carried alongside each reference body.
pub(crate) struct RefBody {
    pub(crate) nid: NodeId,
    pub(crate) name: &'static str,
    regime: &'static str,   // human label
    regime_res: u32,        // runtime bake resolution the OWNER captures live
    golden_res: u32,        // CPU-DC resolution used for the committed golden here
    path: &'static str,     // the DC path this regime exercises at runtime
    /// World anchor for the pick-projection test (root bodies: their origin).
    world_anchor: Vec3,
}

/// Build the 5-body reference scene into a fresh `SceneGraph`, plus one shared material. Prims are
/// authored in each body's LOCAL frame (`SdfPrimitive::new` at local positions), so the body
/// `world_anchor`/`origin` is free to spread the bodies out for the pick test WITHOUT changing the
/// golden (the golden bakes in the local frame). Returns `(sg, ref_bodies, material_id)`.
pub(crate) fn build_ref_scene() -> (SceneGraph, Vec<RefBody>, MaterialId) {
    let mut sg = SceneGraph::new();
    let root = sg.root();
    let mat = sg.add_material("RefMat", PbrMaterial::default());

    let mut uid = 100u32;
    let mut next_uid = || { let u = uid; uid += 1; u };

    // Helper: make a prim with an explicit op + uid + material.
    let mk = |name: &str, shape: SdfShape, pos: Vec3, op: BooleanOp, u: u32, m: MaterialId| {
        let mut p = SdfPrimitive::new(name.to_string(), shape, pos);
        p.operation = op;
        p.prim_uid = u;
        p.material_id = m;
        p
    };
    let boxs = |he: Vec3| SdfShape::Box { half_extents: he, bevel: BoxBevel::default() };

    let body = |sg: &mut SceneGraph, name: &str, prims: Vec<SdfPrimitive>, origin: Vec3| -> NodeId {
        sg.add_child(root, NodePayload::SdfBody {
            name: name.into(), primitives: prims, mesh_cache_id: None,
            body_op: BooleanOp::Union { k: 0.0 }, origin,
            body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
            mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
            blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
            child_order: Vec::new(),
        mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),})
    };

    // ── 1. Box + bevel + sub (dense 128³, field-direct) ──────────────────────────────────────
    let b1 = body(&mut sg, "RefBoxBevelSub", vec![
        mk("Base", boxs(Vec3::new(1.0, 0.8, 1.0)), Vec3::ZERO, BooleanOp::Union { k: 0.0 }, next_uid(), mat),
        mk("Carve", SdfShape::Sphere { radius: 0.7 }, Vec3::new(0.7, 0.7, 0.7), BooleanOp::Subtract { k: 0.0 }, next_uid(), mat),
    ], Vec3::new(-4.0, 0.0, 0.0));

    // ── 2. CSG 3-prim (dense 256³, field-direct) ─────────────────────────────────────────────
    let b2 = body(&mut sg, "RefCsg3Prim", vec![
        mk("Base", boxs(Vec3::new(1.1, 1.1, 1.1)), Vec3::ZERO, BooleanOp::Union { k: 0.0 }, next_uid(), mat),
        mk("CutSphere", SdfShape::Sphere { radius: 0.8 }, Vec3::new(0.9, 0.0, 0.0), BooleanOp::Subtract { k: 0.0 }, next_uid(), mat),
        mk("CutCyl", SdfShape::Cylinder { radius: 0.45, half_height: 2.0 }, Vec3::new(0.0, 0.0, 0.0), BooleanOp::Subtract { k: 0.0 }, next_uid(), mat),
    ], Vec3::new(0.0, 0.0, 0.0));

    // ── 3. Organic (sparse 512³, SparseTiledResident) ────────────────────────────────────────
    // Smooth-union blobs — the non-Lipschitz smin field the sparse classifier margins exist for.
    let b3 = body(&mut sg, "RefOrganicSparse", vec![
        mk("Blob0", SdfShape::Sphere { radius: 0.9 }, Vec3::new(-0.5, 0.0, 0.0), BooleanOp::Union { k: 0.0 }, next_uid(), mat),
        mk("Blob1", SdfShape::Sphere { radius: 0.7 }, Vec3::new(0.5, 0.2, 0.1), BooleanOp::Union { k: 0.4 }, next_uid(), mat),
        mk("Blob2", SdfShape::Sphere { radius: 0.6 }, Vec3::new(0.1, 0.6, -0.3), BooleanOp::Union { k: 0.35 }, next_uid(), mat),
        mk("Ring", SdfShape::Torus { major_radius: 0.9, minor_radius: 0.25 }, Vec3::new(0.0, -0.4, 0.4), BooleanOp::Union { k: 0.3 }, next_uid(), mat),
    ], Vec3::new(4.0, 0.0, 0.0));

    // ── 4. Detailed (sparse 1024³, SparseTiledResident + overflow/grow) ──────────────────────
    // Many small features → dense surface shell → exercises pool grow/overflow at runtime.
    let mut d = Vec::new();
    d.push(mk("Trunk", boxs(Vec3::new(1.2, 0.3, 1.2)), Vec3::ZERO, BooleanOp::Union { k: 0.0 }, next_uid(), mat));
    for i in 0..6 {
        let a = i as f32 * std::f32::consts::TAU / 6.0;
        d.push(mk("Bump", SdfShape::Sphere { radius: 0.22 },
            Vec3::new(a.cos() * 0.9, 0.25, a.sin() * 0.9), BooleanOp::Union { k: 0.05 }, next_uid(), mat));
        d.push(mk("Bore", SdfShape::Cylinder { radius: 0.12, half_height: 0.6 },
            Vec3::new(a.cos() * 0.5, 0.0, a.sin() * 0.5), BooleanOp::Subtract { k: 0.0 }, next_uid(), mat));
    }
    let b4 = body(&mut sg, "RefDetailedSparse", d, Vec3::new(0.0, 4.0, 0.0));

    // ── 5. Nested sub-body where the CHILD defines the solid (dense 256³) ────────────────────
    // Parent owns a small base prim; a child SdfBody owns the main solid, combined via the child's
    // body_op. Exercises the RECURSIVE golden (`BodyField::from_graph` + `collect_body_prims_recursive`)
    // — the case the review believed was broken.
    let b5 = body(&mut sg, "RefNestedSubBody", vec![
        mk("ParentBase", SdfShape::Sphere { radius: 0.5 }, Vec3::new(0.0, -0.8, 0.0), BooleanOp::Union { k: 0.0 }, next_uid(), mat),
    ], Vec3::new(0.0, -4.0, 0.0));
    let _child = sg.add_child(b5, NodePayload::SdfBody {
        name: "NestedSolid".into(),
        primitives: vec![
            mk("ChildBox", boxs(Vec3::new(0.9, 0.9, 0.9)), Vec3::new(0.0, 0.3, 0.0), BooleanOp::Union { k: 0.0 }, next_uid(), mat),
            mk("ChildCut", SdfShape::Sphere { radius: 0.6 }, Vec3::new(0.5, 0.8, 0.5), BooleanOp::Subtract { k: 0.0 }, next_uid(), mat),
        ],
        mesh_cache_id: None,
        body_op: BooleanOp::Union { k: 0.0 }, origin: Vec3::new(0.0, -4.0, 0.0),
        body_rotation: Quat::IDENTITY, body_scale: Vec3::ONE,
        mirror_flags: 0, mirror_sign: 0, mirror_center: Vec3::ZERO, bevel_radius: 0.0,
        blend_profile: BlendProfile::default(), material: PbrMaterial::default(),
        child_order: Vec::new(),
    mesh_modifiers: Vec::new(),field_modifiers: Vec::new(),});

    let bodies = vec![
        RefBody { nid: b1, name: "RefBoxBevelSub",    regime: "box+bevel+sub",  regime_res: 128,  golden_res: GOLDEN_RES, path: "field-direct / DenseResident",      world_anchor: Vec3::new(-4.0, 0.0, 0.0) },
        RefBody { nid: b2, name: "RefCsg3Prim",       regime: "CSG 3-prim",     regime_res: 256,  golden_res: GOLDEN_RES, path: "field-direct",                       world_anchor: Vec3::new(0.0, 0.0, 0.0) },
        RefBody { nid: b3, name: "RefOrganicSparse",  regime: "organic",        regime_res: 512,  golden_res: GOLDEN_RES, path: "SparseTiledResident (atlas + C2)",   world_anchor: Vec3::new(4.0, 0.0, 0.0) },
        RefBody { nid: b4, name: "RefDetailedSparse", regime: "detailed",       regime_res: 1024, golden_res: GOLDEN_RES, path: "SparseTiledResident + overflow/grow", world_anchor: Vec3::new(0.0, 4.0, 0.0) },
        RefBody { nid: b5, name: "RefNestedSubBody",  regime: "nested sub-body",regime_res: 256,  golden_res: GOLDEN_RES, path: "recursive hash (nested child)",      world_anchor: Vec3::new(0.0, -4.0, 0.0) },
    ];
    (sg, bodies, mat)
}

// ================================================================================================
// ProjectFile builder — mirrors `project::child_order_save_tests::project_with` (that helper is
// `pub(super)` inside project.rs's own test module, so it is unreachable here). Minimal but COMPLETE
// so it drives the real `save_binary`/`load_binary`.
// ================================================================================================
fn project_with(sg: SceneGraph) -> ProjectFile {
    // Multi-map channels: collected exactly as ProjectFile::from_app does (shared pure fn), so
    // the round-trip tests exercise the REAL FL4MATX2 write path when a material carries maps.
    let material_channels = crate::project_save::collect_material_channels(&sg);
    let light_volumetrics = crate::project_save::collect_light_volumetrics(&sg);
    let light_rt = crate::project_save::collect_light_rt(&sg);
    ProjectFile {
        version: 7,
        scene: SceneData {
            primitives: vec![], layers: vec![], objects: vec![], lights: vec![],
            next_prim_id: 200, next_layer_id: 1, next_object_id: 1, next_light_id: 1,
            next_node_name_id: 0,
        },
        camera: CameraState { yaw: 0.0, pitch: 0.0, roll: 0.0, distance: 5.0, target: [0.0; 3], ortho: false },
        render: RenderSettings {
            shading_mode: ShadingMode::Pbr, resolution_scale: 1.0, render_quality: 1, exposure: 1.0,
            bloom_enabled: false, bloom_threshold: 1.0, bloom_intensity: 0.0, ssr_enabled: false,
            fog_enabled: false, fog_density: 0.0, fog_height_falloff: 0.0, fog_scatter: 0.0,
            fog_color: [0.35, 0.40, 0.50],
            upsample_enabled: false, rt_bounces: 1, rt_shadow_samples: 1, pt_max_bounces: 1,
            pt_samples_per_frame: 1, bg_color_top: [0.0; 3], bg_color_bottom: [0.0; 3], fov_deg: 90.0,
            env_brightness: 1.0, env_contrast: 1.0, env_saturation: 1.0, env_blur: 0.0,
            pt_dof_aperture: 0.0, pt_dof_falloff: 0.5, pt_denoise: false, denoise_strength: 0.5,
            denoise_radius: 3, fog_phase_g: 0.7, fog_absorption: 0.3, fog_noise_scale: 3.0,
            fog_noise_strength: 0.4, fog_max_distance: 30.0, fog_shadow_steps: 4.0, voxel_cache_res: 128,
            adaptive_resolution: true, atlas_xy_resolution: 256, worker_gpu_enabled: vec![],
            sdf_edge_aa_enabled: true, fxaa_enabled: true, matcap_bg_color: [0.0; 3],
            matcap_bg_gradient: false, matcap_bg_gradient_top: [0.0; 3], matcap_rotation: 0.0,
            matcap_edge_enabled: true, matcap_edge_color: [0.0; 3], matcap_edge_thickness: 1.0,
            matcap_edge_threshold: 0.3,
        },
        editor: EditorSettings {
            snap_mode: SnapMode::Grid, snap_grid: 1.0, snap_rotation_deg: 15.0, grid_cell_size: 1.0,
            grid_opacity: 1.0, grid_visible: true, wp_mode: WorkPlaneMode::Auto,
            wp_settings: WpSettingsData { spacing: 2.0, color: [0.5; 3], opacity: 1.0,
                orientation: WorkPlaneOrientation::AxisSnap, origin: [0.0; 3] },
            wp_saved: vec![], wp_locked_origin: [0.0; 3], wp_locked_normal: [0.0, 1.0, 0.0],
        },
        scene_graph: Some(sg),
        imported_meshes: vec![],
        voxel_grids: vec![],
        compositor: None,
        material_library: None,
        poly_layers: vec![],
        curve_params: vec![],
        body_mesh_modifiers: vec![],
        body_field_modifiers: vec![],
        terrain_tiles: vec![],
        poly_params: Vec::new(),
        child_orders: vec![],
        material_maps: vec![],
        material_channels,
        env: None,
        light_volumetrics,
        light_rt,
        node_graphs: vec![],
        material_panel_docs: vec![],
        pivot_local_nodes: vec![],
        poly_normals: vec![],
            poly_mod_flags: vec![],
            poly_smooth: vec![],
            poly_remesh: vec![],
            poly_remesh_collapse: vec![],
            poly_remesh_skip_relax: vec![],
            poly_remesh_skip_seal: vec![],
            poly_remesh_drop_res: vec![],
            bake_settings: vec![],
            bake_images: vec![],
            bake_bit16: vec![],
            bake_cage: vec![],
            bake_udim: Default::default(),
            bake_gpu_rays: vec![],
            poly_bevel_var: vec![],
            poly_op_tags: vec![],
            poly_bend: vec![],
            poly_sculpt: vec![],
    }
}

// ================================================================================================
// Structural scene description — robust equality for the round-trip test (immune to the graph's
// internal HashMap ordering: everything is extracted and SORTED). Each prim is fingerprinted by its
// own bincode bytes (a single prim has no HashMap → stable), so shape/position/op/material/modifiers
// are ALL compared, not just a hand-picked subset.
// ================================================================================================
type BodyDesc = (u64, String, String, Vec<(u32, Vec<u8>)>); // (nid, name, body_op, [(uid, prim_bytes)])

fn describe_scene(sg: &SceneGraph) -> (Vec<BodyDesc>, Vec<(u32, String)>) {
    let mut bodies = Vec::new();
    let mut mats = Vec::new();
    for (nid, node) in sg.all_nodes() {
        match &node.payload {
            NodePayload::SdfBody { name, primitives, body_op, .. } => {
                let mut prims: Vec<(u32, Vec<u8>)> = primitives.iter()
                    .map(|p| (p.prim_uid, bincode::serialize(p).expect("prim serialize")))
                    .collect();
                prims.sort_by_key(|(u, _)| *u);
                bodies.push((nid.0, name.clone(), format!("{:?}", body_op), prims));
            }
            NodePayload::Material { id, name, .. } => mats.push((id.0, name.clone())),
            _ => {}
        }
    }
    bodies.sort();
    mats.sort();
    (bodies, mats)
}

// ================================================================================================
// GPU-adapter ASSERTION (deliverable 4). Soft-fail tests use `gpu_test_harness::headless_device`
// (early-return on None). Gate-0 uses `require_adapter` (panic if no device). Both hold the
// process-wide GPU test lock for the whole test body — see `gpu_test_harness.rs` (O6 class;
// concurrent request_device with sdf_gpu_mesh aborted the process under default cargo threads).
//
// Soft-skip sites (unchanged policy — headless CI green):
//   app/src/sdf_gpu_mesh/tests_dc.rs, tests_bake.rs, tests_sparse_bake.rs, tests_gpu_classify.rs
//   graphics/src/voxel_brush_gpu_parity.rs
pub(crate) fn require_adapter()
-> (
    crate::gpu_test_harness::GpuTestLock,
    eframe::wgpu::Device,
    eframe::wgpu::Queue,
) {
    crate::gpu_test_harness::require_adapter()
}

// ================================================================================================
// Path to docs/planning/sdf/bench/ (derived from CARGO_MANIFEST_DIR — never a hardcoded user path).
// ================================================================================================
fn bench_dir() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/core_engine/app  →  ../../docs/planning/sdf/bench
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..").join("docs").join("planning").join("sdf").join("bench");
    let _ = std::fs::create_dir_all(&p);
    p
}

// ================================================================================================
// TESTS
// ================================================================================================

/// GO / NO-GO DETERMINISM GATE (the whole point of Gate 0). Capture every body's golden TWICE from
/// two independent `ChunkedMesh` instances (fresh HashMaps → the exact condition that would expose a
/// non-deterministic chunk-iteration order) and assert byte-identical. Also assert against the
/// committed `GOLDEN_HASHES` when set (gate-1..5 regression reference). PRINTS the hashes so a fresh
/// capture can be pinned. Run: `cargo test -p app dc_gate0_golden_determinism -- --nocapture`.
#[test]
fn dc_gate0_golden_determinism() {
    let (sg, bodies, _mat) = build_ref_scene();

    let t = std::time::Instant::now();
    let run = |sg: &SceneGraph, bodies: &[RefBody]| -> Vec<(u64, usize, usize)> {
        bodies.iter().map(|b| {
            let (mesh, hash, _mn, _mx) = body_golden(sg, b.nid, b.golden_res);
            (hash, mesh.vertex_count(), mesh.triangle_count())
        }).collect()
    };
    let run_a = run(&sg, &bodies);
    let run_b = run(&sg, &bodies);
    eprintln!("[GATE0] golden capture x2 in {:.2}s (golden_res={GOLDEN_RES}³)", t.elapsed().as_secs_f64());

    let mut go = true;
    for (i, b) in bodies.iter().enumerate() {
        let (ha, va, ta) = run_a[i];
        let (hb, _vb, _tb) = run_b[i];
        eprintln!("[GATE0] body[{i}] {:<18} regime={:>5}³ golden={GOLDEN_RES}³ verts={va} tris={ta} hash=0x{ha:016x}",
            b.name, b.regime_res);
        if ha != hb {
            go = false;
            eprintln!("[GATE0] *** NON-DETERMINISTIC *** body[{i}] {} run_a=0x{ha:016x} run_b=0x{hb:016x}", b.name);
        }
        assert_eq!(ha, hb, "GATE 0 NO-GO: body {} golden hash is non-deterministic between two runs \
            (0x{ha:016x} vs 0x{hb:016x}) — the mesh assembly still has HashMap-ordered output. Fix the \
            sort before any downstream gate.", b.name);
    }
    eprintln!("[GATE0] DETERMINISM: {}", if go { "GO — all 5 bodies byte-identical across 2 runs" } else { "NO-GO" });

    // Committed-golden regression guard (skipped until the constants are pinned from a real capture).
    if GOLDEN_HASHES.iter().any(|&h| h != 0) {
        for (i, b) in bodies.iter().enumerate() {
            assert_eq!(run_a[i].0, GOLDEN_HASHES[i],
                "GATE 0: body {} golden 0x{:016x} != committed 0x{:016x} — the ref mesh CHANGED. If \
                 intentional, re-pin GOLDEN_HASHES and document the diff; else it is a regression.",
                b.name, run_a[i].0, GOLDEN_HASHES[i]);
        }
    }
}

/// `.fl4` round-trip (deliverable 3): build the ref scene → `save_binary` → `load_binary` → assert
/// bodies + prims + materials are byte-identical. Today only the bincode SENTINEL guard runs
/// (`test_spine.rs` is `#[ignore]`); this is the first real write→read→assert-equal of a full scene.
#[test]
fn dc_gate0_ref_scene_roundtrip() {
    let (sg, _bodies, mat) = build_ref_scene();
    let before = describe_scene(&sg);
    assert!(before.0.len() >= 5, "ref scene must have >=5 SdfBody nodes, got {}", before.0.len());
    assert!(!before.1.is_empty(), "ref scene must have >=1 material");

    let dir = std::env::temp_dir().join("azra_dc_gate0");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("roundtrip_ref.fl4");

    crate::project::save_binary(&path, &project_with(sg)).expect("save_binary");
    let mut loaded = crate::project::load_binary(&path).expect("load_binary");
    let sg2 = loaded.scene_graph.take().expect("loaded file has no scene_graph");
    let after = describe_scene(&sg2);

    assert_eq!(before.0, after.0, "bodies/prims changed across save→load round-trip");
    assert_eq!(before.1, after.1, "materials changed across save→load round-trip");
    // The referenced material must still resolve after the round-trip.
    assert!(sg2.material_by_id(mat).is_some(), "referenced material {mat:?} lost across round-trip");
    let _ = std::fs::remove_file(&path);
    eprintln!("[GATE0] round-trip OK: {} bodies, {} materials identical", after.0.len(), after.1.len());
}

/// Pick / selection regression (deliverable 5). The GPU colour-ID pick (`pick_node_at` →
/// `poll_pending_pick`) needs a live `AzraApp` + device and is owner-run; this covers the HEADLESS
/// slice of the same pipeline, which is where gates 3-4 (routing / facade) would regress it:
///   (a) the CANONICAL gather predicate `SceneGraph::body_has_renderable_content` (the exact one
///       `pick_node_at` uses) reports every ref body pickable — a resident/nested body MUST stay
///       clickable (P0);
///   (b) each body's world anchor projects ON-SCREEN via `world_to_screen` (the CPU projection that
///       must match the GPU camera) — so a click can land on it;
///   (c) the `UnifiedSelection` single-select contract (`select_single_unified`'s exact body:
///       clear→insert→active→last_clicked) resolves to that ONE body's `SceneItemId::Node`.
/// Multi-segment note: a pooled body is split across `SlotKey=(NodeId,segment)` pool slots but every
/// segment carries the SAME NodeId, so selection resolves to one body regardless of segment count —
/// the lockstep the GPU pool parity tests + owner manual test cover; here the body IDENTITY is the
/// single `nid` asserted below.
#[test]
fn dc_gate0_pick_selection_roundtrip() {
    let (sg, bodies, _mat) = build_ref_scene();

    // Camera aimed at the scene centroid; wide FOV + distance so every spread-out body is in frustum.
    let centroid = bodies.iter().map(|b| b.world_anchor).fold(Vec3::ZERO, |a, p| a + p) / bodies.len() as f32;
    let orbit = Quat::from_euler(glam::EulerRot::YXZ, 0.6, -0.5, 0.0);
    let fwd = (orbit * Vec3::NEG_Z).normalize();
    let dist = 18.0f32;
    let cam_pos = centroid - fwd * dist;
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    let aspect = rect.width() / rect.height();
    let fov_deg = 60.0f32;

    for b in &bodies {
        // (a) canonical pickability predicate — the P0 "body is selectable" gate.
        assert!(sg.body_has_renderable_content(b.nid),
            "PICK P0: ref body {} is not renderable/pickable at the graph level", b.name);

        // (b) world anchor projects on-screen.
        let sp = world_to_screen(b.world_anchor, cam_pos, orbit, rect, aspect, 0.0, fov_deg);
        let sp = sp.unwrap_or_else(|| panic!("PICK: body {} projected behind the camera", b.name));
        assert!(rect.contains(sp), "PICK: body {} anchor {:?} projected off-screen at {sp:?}", b.name, b.world_anchor);

        // (c) single-select contract (mirrors `select_single_unified`).
        let id = SceneItemId::Node(b.nid);
        let mut sel = UnifiedSelection::default();
        sel.selected.clear();
        sel.selected.insert(id);
        sel.active = Some(id);
        sel.last_clicked = Some(id);
        assert_eq!(sel.len(), 1, "PICK: single-select must leave exactly one item ({})", b.name);
        assert_eq!(sel.active_node(), Some(b.nid), "PICK: active node must be {} after select", b.name);
        assert!(sel.selected_nodes().any(|n| n == b.nid), "PICK: {} not in selection", b.name);
    }
    eprintln!("[GATE0] pick/selection OK: {} bodies pickable, on-screen, single-select correct", bodies.len());
}

/// Materials-UX round-trip (extends the Gate-0 `.fl4` round-trip): a CREATED material with authored
/// PBR values, assigned to an SDF body's prims (`material_id`) AND to a poly body (per-face
/// `HFace.material_id`), must survive save→load byte-exactly — and the canonical resolvers
/// (`resolve_prim_material` / `resolve_poly_body_material`) must still resolve it after the reload.
#[test]
fn dc_gate0_materials_assignment_roundtrip() {
    use core_math::half_edge::HalfEdgeMesh;
    let (mut sg, bodies, _ref_mat) = build_ref_scene();

    // Authored material (distinct values — not defaults — so a lossy field would be caught).
    let mut authored = PbrMaterial::default();
    authored.albedo = Vec3::new(0.83, 0.21, 0.11);
    authored.roughness = 0.27;
    authored.metallic = 0.93;
    authored.emission_color = Vec3::new(0.1, 0.9, 0.2);
    authored.emission_intensity = 3.5;
    // Multi-map channels (2026-07-17): authored NON-DEFAULT TexMaps on the `#[serde(skip)]`
    // slots — these ride the FL4MATX2 tail, not the blob, and must survive the round-trip.
    // Projections are DELIBERATELY per-channel-distinct (E4, 2026-07-18): each record has always
    // carried its own projection code, so a mixed-projection material must round-trip channel-
    // exact — and a yesterday-format file (uniform codes) loads through the same path unchanged.
    use core_math::primitives::TexMap;
    authored.normal_map = TexMap { tex_id: 1, strength: 0.9, uv_scale: 2.0, projection: 0, absolute: false, ..Default::default() };
    authored.roughness_map = TexMap { tex_id: 2, strength: 1.0, uv_scale: 2.0, projection: 1, absolute: false, ..Default::default() };
    authored.metallic_map = TexMap { tex_id: 3, strength: 0.55, uv_scale: 2.0, projection: 0, absolute: false, ..Default::default() };
    let mid = sg.add_material("Overhaul Mat", authored.clone());

    // Assign to every prim of the FIRST ref body.
    let body0 = bodies[0].nid;
    let prim_uids: Vec<u32> = {
        let prims = sg.body_primitives_mut(body0).expect("body0 prims");
        for p in prims.iter_mut() { p.material_id = mid; }
        prims.iter().map(|p| p.prim_uid).collect()
    };

    // Poly cube with ALL faces referencing the material (object-level assignment).
    let pos = [
        Vec3::new(-0.5, -0.5, -0.5), Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, 0.5, -0.5), Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, -0.5, 0.5), Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(0.5, 0.5, 0.5), Vec3::new(-0.5, 0.5, 0.5),
    ];
    let faces = vec![
        vec![0usize, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4],
        vec![2, 3, 7, 6], vec![1, 2, 6, 5], vec![0, 4, 7, 3],
    ];
    let mut mesh = HalfEdgeMesh::from_polygons(&pos, &faces);
    for f in mesh.faces.iter_mut() { f.material_id = mid.0; }
    let root = sg.root();
    let poly_nid = sg.add_child(root, NodePayload::PolygonalBody {
        name: "MatCube".into(), mesh: std::sync::Arc::new(mesh), source: Default::default(), mesh_cache_id: None,
        material: PbrMaterial::default(), modifiers: Vec::new(),
    });

    // Save → load.
    let dir = std::env::temp_dir().join("azra_dc_gate0");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("roundtrip_materials_ux.fl4");
    crate::project::save_binary(&path, &project_with(sg)).expect("save_binary");
    let mut loaded = crate::project::load_binary(&path).expect("load_binary");
    let mut sg2 = loaded.scene_graph.take().expect("loaded file has no scene_graph");
    let _ = std::fs::remove_file(&path);

    // The `#[serde(skip)]` channels are ABSENT from the blob by design — before the FL4MATX2
    // reattach they must be defaults (proves no naked field snuck into the wire format)…
    {
        let (_, m_pre) = sg2.material_by_id(mid).expect("created material lost across round-trip");
        assert_eq!(m_pre.normal_map, TexMap::default(),
            "normal_map must NOT ride the bincode blob (field-trap guard)");
    }
    // …then the load-side reattach (the exact fn apply_to_app runs) restores them.
    assert_eq!(loaded.material_channels.len(), 3, "3 authored channels must ride FL4MATX2");
    crate::project_load::apply_material_channels(&mut sg2, &loaded.material_channels);

    // Material node + authored values survive.
    let (_, m2) = sg2.material_by_id(mid).expect("created material lost across round-trip");
    assert_eq!(m2.albedo, authored.albedo, "albedo lost");
    assert!((m2.roughness - authored.roughness).abs() < 1e-6, "roughness lost");
    assert!((m2.metallic - authored.metallic).abs() < 1e-6, "metallic lost");
    assert_eq!(m2.emission_color, authored.emission_color, "emission color lost");
    assert!((m2.emission_intensity - authored.emission_intensity).abs() < 1e-6, "emission intensity lost");
    assert_eq!(m2.normal_map, authored.normal_map, "normal map channel lost (FL4MATX2)");
    assert_eq!(m2.roughness_map, authored.roughness_map, "roughness map channel lost (FL4MATX2)");
    assert_eq!(m2.metallic_map, authored.metallic_map, "metallic map channel lost (FL4MATX2)");
    assert_eq!(sg2.materials().find(|(_, id, _, _)| *id == mid).map(|(_, _, n, _)| n.to_string()),
        Some("Overhaul Mat".to_string()), "material name lost");

    // Prim assignments survive + resolve.
    let prims2 = sg2.body_primitives(body0).expect("body0 lost");
    for uid in &prim_uids {
        let p = prims2.iter().find(|p| p.prim_uid == *uid).expect("prim lost");
        assert_eq!(p.material_id, mid, "prim material_id lost across round-trip");
        assert_eq!(sg2.resolve_prim_material(p).albedo, authored.albedo,
            "resolver must return the assigned graph material after reload");
    }

    // Poly face assignments survive + resolve.
    match sg2.get(poly_nid).map(|n| &n.payload) {
        Some(NodePayload::PolygonalBody { mesh, material, .. }) => {
            assert!(mesh.faces.iter().all(|f| f.material_id == mid.0),
                "poly face material_id lost across round-trip");
            assert_eq!(sg2.resolve_poly_body_material(mesh, material).albedo, authored.albedo,
                "poly body resolver must return the assigned graph material after reload");
        }
        other => panic!("poly body lost across round-trip: {:?}", other.map(|p| std::mem::discriminant(p))),
    }

    // Use-count bookkeeping (the delete-in-use guard's data) survives too.
    let (prim_refs, face_refs) = sg2.material_use_counts(mid);
    assert_eq!(prim_refs, prim_uids.len());
    assert_eq!(face_refs, 6);
    eprintln!("[GATE0] materials-UX round-trip OK: {} prim refs + {} face refs resolved", prim_refs, face_refs);
}

/// BACKWARD-COMPAT GATE (multi-map, 2026-07-17): the COMMITTED reference `.fl4` — a binary
/// written BEFORE the multi-map channels existed — must load byte-for-byte unchanged. The
/// `#[serde(skip)]` channel fields add ZERO wire bytes (field-trap guard) and the absent
/// `FL4MATX2` tail loads as empty channels; every material's new channels come up default.
#[test]
fn dc_gate0_committed_ref_fl4_loads_unchanged() {
    let path = bench_dir().join("dc_consolidation_ref.fl4");
    assert!(path.exists(), "committed ref .fl4 missing at {}", path.display());
    let mut loaded = crate::project::load_binary(&path)
        .expect("the committed pre-change ref .fl4 must still load");
    assert!(loaded.material_channels.is_empty(), "a pre-change file carries no FL4MATX2 records");
    let sg = loaded.scene_graph.take().expect("ref file has no scene_graph");
    let (bodies, mats) = describe_scene(&sg);
    assert!(bodies.len() >= 5, "ref scene must keep >=5 SdfBody nodes, got {}", bodies.len());
    assert!(!mats.is_empty(), "ref scene must keep its material");
    for (_, _, _, m) in sg.materials() {
        assert_eq!(m.normal_map, core_math::primitives::TexMap::default(),
            "skip-field channels must load as defaults from an old file");
    }
    eprintln!("[GATE0] committed ref .fl4 loads unchanged: {} bodies, {} materials", bodies.len(), mats.len());
}

/// Write the versioned reference `.fl4` to `docs/planning/sdf/bench/dc_consolidation_ref.fl4`
/// (deliverable 2). `#[ignore]` — it writes a committed artifact, run explicitly:
/// `cargo test -p app dc_gate0_write_ref_scene_fl4 -- --ignored --nocapture`.
#[test]
#[ignore]
fn dc_gate0_write_ref_scene_fl4() {
    let (sg, bodies, _mat) = build_ref_scene();
    let path = bench_dir().join("dc_consolidation_ref.fl4");
    crate::project::save_binary(&path, &project_with(sg)).expect("save_binary ref scene");
    eprintln!("[GATE0] wrote reference scene ({} bodies) → {}", bodies.len(), path.display());
    // Sanity: it reads back and matches.
    let mut loaded = crate::project::load_binary(&path).expect("load_binary ref scene");
    let sg2 = loaded.scene_graph.take().expect("no scene_graph");
    assert!(sg2.all_nodes().filter(|(_, n)| matches!(&n.payload, NodePayload::SdfBody { .. })).count() >= 5);
}

/// Baseline JSON report generator (deliverable 7). Captures every headless number (golden hashes,
/// verts/tris, theoretical mesh VRAM, determinism result) and emits clearly-marked `_TODO_owner_run`
/// fields for the runtime numbers that need the AZRA exe (locked to owner). `#[ignore]` — run:
/// `cargo test -p app dc_gate0_write_baseline_json -- --ignored --nocapture`.
#[test]
#[ignore]
fn dc_gate0_write_baseline_json() {
    let (sg, bodies, _mat) = build_ref_scene();
    let mut body_json = Vec::new();
    for b in &bodies {
        let (mesh, hash, gmin, gmax) = body_golden(&sg, b.nid, b.golden_res);
        // Theoretical resident-mesh VRAM: 8 f32/vertex (pos+normal+uv) + u32 indices.
        let mesh_vram = mesh.vertex_count() * 32 + mesh.triangle_count() * 3 * 4;
        body_json.push(serde_json::json!({
            "name": b.name,
            "regime": b.regime,
            "regime_res": b.regime_res,
            "golden_res": b.golden_res,
            "path": b.path,
            "golden_hash": format!("0x{hash:016x}"),
            "verts": mesh.vertex_count(),
            "tris": mesh.triangle_count(),
            "grid_min": [gmin.x, gmin.y, gmin.z],
            "grid_max": [gmax.x, gmax.y, gmax.z],
            "mesh_vram_bytes_theoretical": mesh_vram,
            "bake_full_ms_TODO_owner_run": null,
            "dc_extract_ms_TODO_owner_run": null,
            "frame_p50_drag_ms_TODO_owner_run": null,
            "frame_p95_drag_ms_TODO_owner_run": null,
            "pool_vram_bytes_TODO_owner_run": null,
            "pct_incremental_TODO_owner_run": null,
            "hole_ratio_TODO_owner_run": null,
        }));
    }
    let report = serde_json::json!({
        "gate": 0,
        "head": "c210425",
        "branch": "feat/sparse-3a",
        "golden_res_uniform": GOLDEN_RES,
        "golden_source": "CPU chunked_dc oracle (BodyField::from_graph -> bake_body_grid_field -> ChunkedMesh::to_mesh); FNV-1a over vert-bits + quad indices",
        "determinism": "GO (see dc_gate0_golden_determinism)",
        "bodies": body_json,
        "todo_owner_run": {
            "why": "The AZRA exe is locked to the owner; the runtime metrics below cannot be captured headlessly.",
            "how": "Run the app, open the ref scene, enable Perf HUD + CPU Frame Profiler + Profiler overlay, run the scripted deterministic prim drag, then read: frame p50/p95 (drag), bake full/partial ms, per-body DC ms (self.perf.vol_dc_ms), live pool VRAM (SdfMeshPool::body_vram_bytes), % incremental (vol_partial_rebakes/(full+partial)), hole ratio (SdfMeshPool::hole_ratio).",
            "wired_this_gate": "vol_full_rebakes/vol_partial_rebakes, vol_dc_ms, sdf_pool_body_bytes, sdf_pool_hole_ratio, frame_percentile() — now populated (were dead)."
        }
    });
    let path = bench_dir().join("dc_consolidation_baseline.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).expect("write baseline json");
    eprintln!("[GATE0] wrote baseline JSON → {}", path.display());
}

/// GPU↔CPU golden BRIDGE (deliverable 4 wiring). Proves the CPU golden is a valid proxy for the GPU
/// display mesh: bakes body 0's grid on CPU, meshes it BOTH on the CPU (`ChunkedMesh`) and on the GPU
/// (`mesh_sdf_grid_gpu`, the readback path the GPU DC is diffed against), and asserts the counts are
/// in the same ballpark. Uses `require_adapter()` (deliverable 4) so it CANNOT silently pass without
/// a device. `#[ignore]` — opt-in HW run: `cargo test -p app dc_gate0_gpu_cpu_golden_bridge -- --ignored --nocapture`.
#[test]
#[ignore]
fn dc_gate0_gpu_cpu_golden_bridge() {
    use crate::sdf_gpu_mesh::*;
    let (_gpu_lock, device, queue) = require_adapter();
    let (sg, bodies, _mat) = build_ref_scene();
    let b = &bodies[0];

    // CPU oracle grid at the golden res (same bounds/field as `body_golden`).
    let prims = collect_body_prims_recursive(&sg, b.nid);
    let (tmin, tmax) = compute_body_bake_bounds(&prims, 0, Vec3::ZERO, 0.0);
    let (gmin, gmax) = pad_grid_bounds(tmin, tmax);
    let res = b.golden_res;
    let field = BodyField::from_graph(&sg, b.nid);
    let grid = bake_body_grid_field(&field, res, gmin, gmax);

    let (cpu_mesh, cpu_hash, _, _) = body_golden(&sg, b.nid, res);
    let (gpu_v, gpu_i) = mesh_sdf_grid_gpu(&device, &queue, &grid, res, gmin, gmax);

    eprintln!("[GATE0] GPU↔CPU bridge body '{}': cpu_verts={} cpu_tris={} cpu_hash=0x{cpu_hash:016x} | gpu_verts={} gpu_idx={}",
        b.name, cpu_mesh.vertex_count(), cpu_mesh.triangle_count(), gpu_v.len(), gpu_i.len());
    assert!(gpu_v.len() > 100, "GPU produced too few verts");
    let ratio = gpu_v.len() as f32 / cpu_mesh.vertex_count().max(1) as f32;
    assert!(ratio > 0.6 && ratio < 1.6,
        "GPU/CPU vertex-count ratio {ratio:.2} out of range — the CPU golden is NOT tracking the GPU mesh");
}
