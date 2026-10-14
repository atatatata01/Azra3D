pub(crate) mod slice_util;
pub mod bvh;
pub mod cluster_dag;
pub mod sdf_volume;
pub mod curves;
pub mod pbr_material;
pub mod modifiers;
pub mod sdf_shapes;
pub mod material_dna;
pub mod sdf_math;
pub mod primitives;
pub mod mesh;
pub mod texgen;
pub mod scene_graph;
pub mod half_edge;
pub mod edge_select; // edge-loop selection (pure query over the public half-edge API; half_edge.rs frozen)
pub mod mesh_curvature; // discrete SURFACE curvature on a HalfEdgeMesh — docs/planning/render/POLY_CURVATURE_PLAN.md P1
pub mod mikkt; // MikkTSpace-compatible tangent frames — MESH_MAP_BAKER_PLAN G3
pub mod geometry_channel; // Modo-style geometry filters for modifiers + select-by-channel
pub mod poly_mesh; // NEW: non-serialized runtime wrapper (CustomData/TopoIndex/topo_gen) — POLY_FOUNDATION_PLAN.md §4.1
pub mod uv_unwrap; // Smart / angle-based UV unwrap → CustomData.uv (poly pillar)
pub mod mesh_ops;  // NEW: typed MeshOp operators (bevel = canonical template) — POLY_FOUNDATION_PLAN.md §4.7
pub mod bevel_op; // NEW: offset-based poly edge bevel (supersedes frozen bevel_edges_ex) — see BEVEL_PLAN.md
pub mod unbevel; // Topology-guess reverse bevel (select strip → sharp edges + live re-edit)
pub mod bevel_absorb; // Bevel Consume Geometry: absorb loops the width sweeps past (BEVEL.md)
pub mod support_edge; // Bevel Smart Clamp: relocate boolean support edges (BEVEL.md section 6)
pub mod primitive_spec; // NEW: parametric Add-Mesh primitives (PrimitiveSpec) — docs/planning/poly/PARAMETRIC_PRIMITIVES.md
pub mod primitive_round; // NEW: RoundSpec rim/corner rounding — reuses bevel_op, never new corner math
pub mod mesh_boolean;
pub mod mesh_boolean_manifold; // Manifold wrap (default mesh_boolean kernel); BSP A/B via AZRA_BSP_BOOLEAN=1
pub mod scatter; // NEW: Scatter modifier point sources + seeded instance placement (pure math)
pub mod knife; // NEW: screen-projected polyline knife cut engine — docs knife (Ctrl view-based mode) // NEW: BSP-based CSG booleans between closed half-edge meshes — docs/planning/poly/BOOLEAN_V1.md
pub mod nurbs;
pub mod nurbs_trim; // parameter-space trims + tessellation (PROMPT_TRIMMED_SURFACES)
pub mod nurbs_fillet; // planar-to-planar rolling-ball fillet (PLAN N4)
pub mod sculpt_layer;
pub mod sculpt_brush;
pub mod voxel_sculpt; // NEW: density-voxel sculpting (3D-Coat-style) + Surface Nets
pub mod brush; // NEW: composable voxel-brush data model (BrushRecipe -> StampPlan POD), Phase 0
pub mod chunked_dc;
pub mod field_eval;
pub mod display_mesh_ops; // post-DC body mesh stack (MODIFIER_SYSTEM_PLAN) — NOT mesh_ops::MeshOp
pub mod body_field_ops; // body field stack (Volume Mirror / Bevel / Smooth) → hardwired + FieldScope

/// Number of f32 values per serialized SDF primitive in the GPU buffer.
/// Single source of truth — used by graphics (wgpu), cuda_render, and build scripts.
/// 116 base + 4 DNA header + 8 rules × 20 floats + 4 bevel + 4 blend_noise + 4 elongation
/// + 1 height-map atlas start (T1 live march) = 293
pub const FLOATS_PER_PRIM: usize = 293;
pub const DNA_MAX_RULES: usize = 8;
pub const DNA_FLOATS_PER_RULE: usize = 20;
