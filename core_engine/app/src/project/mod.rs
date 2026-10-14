// core_engine/app/src/project.rs
// Save/Load system — hub: structs + file I/O. Operations in project_save/load/new.

use crate::{ShadingMode, SnapMode, WorkPlaneMode, WorkPlaneOrientation, LightType, LightLinkMode,
            CompositorStack, MaterialLibrary};
use core_math::primitives::SdfPrimitive;
use serde::{Serialize, Deserialize};
use std::io::Write;
use std::path::{Path, PathBuf};

// ==================================================================
// Serializable project structs (intermediate — decoupled from app)
// ==================================================================

#[derive(Serialize, Deserialize)]
pub(crate) struct ProjectFile {
    pub version: u32,
    pub scene: SceneData,
    pub camera: CameraState,
    pub render: RenderSettings,
    pub editor: EditorSettings,
    /// Scene graph (v2+). None for v1 files.
    #[serde(default)]
    pub scene_graph: Option<core_math::scene_graph::SceneGraph>,
    /// Imported mesh vertex/index data (v2+).
    #[serde(default)]
    pub imported_meshes: Vec<ImportedMeshRecord>,
    /// Voxel sculpt grids (v7+) — the VoxelBody node's `grid` is #[serde(skip)], so this side-channel
    /// carries it; reattached to nodes by node_id on load.
    #[serde(default)]
    pub voxel_grids: Vec<VoxelNodeRecord>,
    /// Compositor post-processing stack (v4+).
    #[serde(default)]
    pub compositor: Option<CompositorStack>,
    /// Scene material library (v4+).
    #[serde(default)]
    pub material_library: Option<MaterialLibrary>,
    /// Authored poly CustomData layers (uv/crease/seam/sharp), v8+ — the runtime `PolyMesh.cd`
    /// sidecar is NOT serialized (it lives on the non-serialized wrapper / `cache.poly_custom_data`),
    /// so this IN-BLOB `#[serde(default)]` field carries it, reattached to bodies by `node_id` on
    /// load — EXACTLY mirroring `voxel_grids: Vec<VoxelNodeRecord>` (the proven precedent,
    /// `POLY_FOUNDATION_PLAN.md` §6.2). It rides INSIDE the bincode blob (a trailing `#[serde(default)]`
    /// field on a STRUCT is forward/backward-compatible), NOT a second trailing magic after
    /// `FL4ORDER` (which would break the loader's `deserialize(tail)`). Empty on every pre-v8 file →
    /// loads to empty CustomData. MUST stay a SERIALIZED field (NOT `#[serde(skip)]`): a skip field
    /// would not round-trip, defeating persistence.
    #[serde(default)]
    pub poly_layers: Vec<PolyLayerRecord>,
    /// Authored `CurveExtrude` shell/rim params (P1+). The variant's `wall_t`/`rim` are
    /// `#[serde(skip)]`, so this IN-BLOB `#[serde(default)]` side-channel carries them, reattached by
    /// `prim_uid` on load — EXACTLY mirroring `voxel_grids`. Empty on every pre-P1 file → all curves
    /// load solid, hard-capped (forward-compat). MUST stay SERIALIZED (never `#[serde(skip)]`).
    #[serde(default)]
    pub curve_params: Vec<CurveParamRecord>,
    /// Post-DC display mesh modifiers per SdfBody (`mesh_modifiers` is `#[serde(skip)]` on the
    /// payload). IN-BLOB `#[serde(default)]` side-channel — same pattern as `poly_layers` /
    /// `curve_params`. Empty on every pre-modifier-stack file. Reattached by `node_id` on load.
    #[serde(default)]
    pub body_mesh_modifiers: Vec<BodyMeshModRecord>,
    /// Body field modifier stacks (Volume Mirror / Bevel). `field_modifiers` is `#[serde(skip)]`
    /// on the payload. IN-BLOB default side-channel; reattached by `node_id` on load.
    #[serde(default)]
    pub body_field_modifiers: Vec<BodyFieldModsSide>,
    /// Mesh Terrain tile registry (node_id → lattice key). Identity is **NodeId + key**, not
    /// outliner display name — renaming a tile must not drop it from the world on reload.
    /// IN-BLOB `#[serde(default)]` (trailing field; empty on every pre-terrain-side-channel file).
    #[serde(default)]
    pub terrain_tiles: Vec<TerrainTileRecord>,
    /// Parametric poly primitives (`PolygonalBody.source`, itself `#[serde(skip)]`).
    ///
    /// **`#[serde(skip)]` + the `FL4PPRM` tail section — NOT an in-blob `#[serde(default)]`
    /// field.** This was tried in-blob first and it broke every committed `.fl4` fixture with
    /// `failed to fill whole buffer`: bincode is a positional format with no field names, so a
    /// reader expecting a trailing field it cannot find hits EOF. `#[serde(default)]` only helps
    /// when the bytes are already there. The self-delimiting tail section is the format's actual
    /// extension mechanism (see `io.rs`: "the MAGIC trailing section — not the version number — is
    /// the format-extension signal"). A pre-parametric file simply has no section → every poly
    /// body loads as `Authored`, which is correct: its geometry never was derived.
    /// See `docs/planning/poly/PARAMETRIC_PRIMITIVES.md`.
    #[serde(skip)]
    pub poly_params: Vec<PolyParamRecord>,
    /// Unified per-body `child_order` (prim + sub-body interleave). RUNTIME-ONLY carrier —
    /// `#[serde(skip)]`, so it is NEVER in the bincode blob (the blob stays byte-identical to v7).
    /// It is persisted OUT OF BAND in the `FL4ORDER` trailing section (see `save_binary`/`load_binary`),
    /// and this field is just how `load_binary` hands the decoded map to `apply_to_app`. Keyed by
    /// raw node id. A skip field carries no wire bytes, so it cannot affect backward-compat.
    #[serde(skip)]
    pub child_orders: Vec<(u64, Vec<core_math::scene_graph::BodyChildRef>)>,
    /// Persisted albedo-map pixels (#2). RUNTIME-ONLY carrier — `#[serde(skip)]`, so it is NEVER in the
    /// bincode blob: a trailing serde field would break EVERY existing .fl4 (the confirmed field trap —
    /// bincode 1.3.3 + `deserialize_from` errors on the missing field). It rides the self-delimiting FL4
    /// tail in its OWN `FL4MATTX` section (see `save_binary`/`load_binary`), exactly mirroring how
    /// `child_orders` rides `FL4ORDER`. Each record = one used texture-array slot's PNG map, restored in
    /// slot order on load so materials' saved `tex_id` line up with no remap. Empty on every pre-#2 file.
    #[serde(skip)]
    pub material_maps: Vec<MaterialMapRecord>,
    /// Per-material authored MAP-CHANNEL params for the `#[serde(skip)]` channels
    /// (normal / roughness / metallic `TexMap`s on `PbrMaterial` — zero wire bytes in the blob,
    /// see the field-trap note on those fields). RUNTIME-ONLY carrier — `#[serde(skip)]` here too;
    /// rides its OWN self-delimiting `FL4MATX2` tail section (sibling of `FL4MATTX`), reattached by
    /// `MaterialId` on load. The map PIXELS need nothing new: every used texture-array slot already
    /// persists via `FL4MATTX` regardless of which channel references it. Empty on every older file.
    #[serde(skip)]
    pub material_channels: Vec<MaterialChannelRecord>,
    /// Per-project environment HDRI (RENDERER_IBL.md §7 → BUILT). RUNTIME-ONLY carrier —
    /// `#[serde(skip)]` (the confirmed bincode field trap: a trailing serde field in the blob
    /// corrupts every existing .fl4); rides its OWN self-delimiting `FL4ENVIR` tail section
    /// (sibling of `FL4MATTX`/`FL4MATX2`). PATH + settings only, never the pixels — on load the
    /// path is resolved against the project dir and re-kicked through the `request_env_load`
    /// worker (generation-guarded); a missing file keeps the default env + toasts, never fails
    /// the load. `None` when no HDRI is loaded → the file carries NO env section (byte-compat).
    #[serde(skip)]
    pub env: Option<EnvRecord>,
    /// Per-light volumetric ("god ray") authored params. RUNTIME-ONLY carrier — `#[serde(skip)]`, so it
    /// is NEVER in the bincode blob (the confirmed field trap: a naked trailing field on `LightData`
    /// would corrupt every existing `.fl4`). Rides its OWN self-delimiting `FL4LTVOL` tail section
    /// (sibling of `FL4ENVIR`), keyed by light `id`. Only lights with `enabled == true` emit a record →
    /// a scene with no volumetric lights writes a file with NO `FL4LTVOL` magic (byte-compat with every
    /// older save). Reattached to graph lights by `id` on load.
    #[serde(skip)]
    pub light_volumetrics: Vec<LightVolRecord>,
    /// Per-light RAY-TRACED SHADOW authored params (RT_SHADOWS_DESIGN Stage 4). RUNTIME-ONLY carrier
    /// — `#[serde(skip)]`, so NEVER in the bincode blob (same field trap as `light_volumetrics`).
    /// Rides its OWN self-delimiting `FL4LTRT` tail section (append-newest-last relative to
    /// `FL4LTVOL` when it was newest; **newer still is `FL4NODES`** — see `node_graphs`). Only lights
    /// whose RT params were AUTHORED (differ from `LightRtShadow::default()` = enabled+radius0) emit a
    /// record → a scene that never touched RT shadows writes a file with NO `FL4LTRT` magic
    /// (byte-compat with every older save). Reattached to graph lights by `id` on load.
    #[serde(skip)]
    pub light_rt: Vec<LightRtRecord>,
    /// Per-prim procedural shader node graphs (PROCEDURAL_TEXTURING_DESIGN PR0). RUNTIME-ONLY carrier
    /// — `#[serde(skip)]` (field trap: never mid-blob). Rides its OWN self-delimiting `FL4NODES` tail
    /// section written **LAST after `FL4LTRTS`** (append-newest-last). Wire payload is bincode of
    /// `Vec<NodeGraphRecord>`; each graph body is **JSON** (`graph_json`) so `Node` field evolution
    /// stays `#[serde(default)]`-friendly (raw bincode of `Node` is not). Empty → no magic (byte-compat).
    /// Reattached into `ui.node_graphs` on load (source of truth for `rebuild_shader_pipelines`).
    #[serde(skip)]
    pub node_graphs: Vec<NodeGraphRecord>,
    /// Per-material MATERIAL PANEL docs (MATERIAL_PANEL_SPEC P0). RUNTIME-ONLY carrier —
    /// `#[serde(skip)]` (field trap: never mid-blob). Rides its OWN self-delimiting `FL4MPNLS`
    /// tail section written **LAST after `FL4NODES`** (append-newest-last). Body is **JSON**
    /// (`doc_json`) — like `NodeGraphRecord.graph_json` — so `MaterialPanelDoc` field evolution
    /// stays `#[serde(default)]`-friendly. Empty → no magic (byte-compat with every older save);
    /// absent on load → docs are derived on demand via
    /// `material_panel::derive_doc_from_material` (migration).
    #[serde(skip)]
    pub material_panel_docs: Vec<MaterialPanelRecord>,
    /// Per-poly-body authored AUTO SMOOTH angle (Blender-style normal control). RUNTIME-ONLY
    /// carrier — `#[serde(skip)]`, so NEVER in the bincode blob. It rides its OWN self-delimiting
    /// `FL4PNORM` tail section (sibling of `FL4MPNLS`), reattached onto `cache.poly_custom_data`
    /// by `node_id` on load.
    ///
    /// Why a section and not a field on `PolyLayerRecord`: that record lives INSIDE the blob, and
    /// bincode is positional — appending a field to it would make every existing `.fl4` that
    /// carries authored poly layers read the next record's bytes as this field (`#[serde(default)]`
    /// does NOT rescue a missing field in bincode). The tail section costs zero blob bytes.
    /// The per-edge `sharp` marks need nothing new here: they already persist as
    /// `PolyLayerRecord.sharp`. Only bodies with an AUTHORED angle emit a record → a scene that
    /// never touched Shade Smooth/Flat writes a file with no `FL4PNORM` magic (byte-compat).
    #[serde(skip)]
    pub poly_normals: Vec<PolyNormalRecord>,
    /// Per-poly-modifier `#[serde(skip)]` FLAGS (currently: Bevel "Consume Geometry"). RUNTIME-ONLY
    /// carrier — rides its OWN self-delimiting `FL4PMODF` tail section (newest, after `FL4PPRM`),
    /// reattached onto the graph's `PolyModifier`s by (node_id, modifier index) on load. Why a
    /// section and not a field on `PolyModifier`: that struct lives INSIDE the blob and bincode is
    /// positional — appending a field breaks every existing `.fl4` (the serde_golden gate catches
    /// it). Only modifiers with a non-default flag emit a record → no section, byte-compat,
    /// whenever the feature is unused.
    #[serde(skip)]
    pub poly_mod_flags: Vec<PolyModFlagRecord>,
    /// Per-poly-modifier Smooth mask feather (`PolyModifier.smooth_falloff`, `#[serde(skip)]`).
    /// RUNTIME-ONLY carrier — rides its OWN self-delimiting `FL4PSMTH` tail section (newest,
    /// after `FL4PMODF`), reattached by (node_id, modifier index) on load. Separate from
    /// `poly_mod_flags` on purpose: see `POLY_SMOOTH_MAGIC` in `io.rs` for why widening that
    /// record would break loading files an older build already wrote.
    #[serde(skip)]
    pub poly_smooth: Vec<PolySmoothRecord>,
    /// Per-poly-modifier Voxel Remesh params (all four `#[serde(skip)]` on `PolyModifier`).
    /// RUNTIME-ONLY carrier — rides its OWN self-delimiting `FL4PRMSH` tail section (newest,
    /// after `FL4PSMTH`), reattached by (node_id, modifier index) on load. Separate section
    /// rather than widening `poly_mod_flags` / `poly_smooth`: see `POLY_REMESH_MAGIC` in `io.rs`
    /// for why widening either would break loading files an older build already wrote.
    #[serde(skip)]
    pub poly_remesh: Vec<PolyRemeshRecord>,
    /// Per-poly-modifier remesh collapse flag (`PolyModifier.remesh_collapse`). Own
    /// `FL4PRMCL` tail — do not widen `FL4PRMSH` (that would fail-load older remesh saves).
    #[serde(skip)]
    pub poly_remesh_collapse: Vec<PolyRemeshCollapseRecord>,
    /// Per-poly-modifier remesh skip-relax-while-dragging (`PolyModifier.remesh_skip_relax_drag`).
    /// Own `FL4PRMRL` tail — do not widen `FL4PRMCL`.
    #[serde(skip)]
    pub poly_remesh_skip_relax: Vec<PolyRemeshSkipRelaxRecord>,
    /// Per-poly-modifier remesh skip-seal-while-dragging. Own `FL4PRMSL` tail.
    #[serde(skip)]
    pub poly_remesh_skip_seal: Vec<PolyRemeshSkipSealRecord>,
    /// Per-poly-modifier remesh draft-res-while-dragging. Own `FL4PRMDR` tail.
    #[serde(skip)]
    pub poly_remesh_drop_res: Vec<PolyRemeshDropResRecord>,
    /// Per-body mesh-map bake settings (G1). RUNTIME-ONLY carrier — `#[serde(skip)]`, rides
    /// its OWN self-delimiting `FL4BAKE` tail. Never an in-blob field.
    #[serde(skip)]
    pub bake_settings: Vec<BakeSettingsRecord>,
    /// G4 named baked-image registry. Rides the SAME `FL4BAKE` tail as `bake_settings`.
    #[serde(skip)]
    pub bake_images: Vec<BakedImageRecord>,
    /// G7 16-bit flag per body. Own tail `FL4B16`.
    #[serde(skip)]
    pub bake_bit16: Vec<BakeBit16Record>,
    /// G7 mesh-cage nids per body. Own tail `FL4CAGE`.
    #[serde(skip)]
    pub bake_cage: Vec<BakeCageRecord>,
    /// G7 UDIM flag + per-image tile numbers. Own tail `FL4UDIM`.
    #[serde(skip)]
    pub bake_udim: BakeUdimTail,
    /// G7 GPU-ray flag per body. Own tail `FL4GRAY`.
    #[serde(skip)]
    pub bake_gpu_rays: Vec<BakeGpuRayRecord>,
    /// Per-poly-modifier bevel VARIABLE WIDTH. Own `FL4PBVAR` tail.
    #[serde(skip)]
    pub poly_bevel_var: Vec<PolyBevelVarRecord>,
    /// Per-poly-body Smart Select `op_created` tags. Own `FL4PTAGS` tail.
    #[serde(skip)]
    pub poly_op_tags: Vec<PolyOpTagRecord>,
    /// Per-poly-modifier BEND params. Own `FL4PBEND` tail.
    #[serde(skip)]
    pub poly_bend: Vec<PolyBendRecord>,
    /// Per-poly-modifier PolySculpt deltas. Own `FL4SCLP` tail (after FL4PBEND).
    #[serde(skip)]
    pub poly_sculpt: Vec<PolySculptRecord>,
    /// Node ids whose Custom pivot `position` is NODE-LOCAL (`PivotConfig.local`,
    /// `#[serde(skip)]` — the in-blob `serde(default)` bool of 2026-08-18 broke every
    /// older .fl4). Own `FL4PIVT` tail (after FL4SCLP). Absent = no local Custom
    /// pivots = pre-fix semantics (migration converts world→local on load).
    #[serde(skip)]
    pub pivot_local_nodes: Vec<u64>,
}

/// One poly body's Smart Select tags for the `FL4PTAGS` side-channel.
///
/// `op_created` marks an edge a destructive OPERATION built (a bevel's segments, a boolean's
/// cutter seams) rather than one the user modelled, and the Bevel modifier's Smart Select skips
/// those edges.
#[derive(Serialize, Deserialize)]
pub(crate) struct PolyOpTagRecord {
    pub node_id: u64,
    /// Per-corner "created by an operation" bits: `HalfEdgeId.0`.
    pub op_created: Vec<u32>,
}

/// One poly body's authored Auto Smooth angle for the `FL4PNORM` side-channel.
/// `0` = Shade Flat, `180` = Shade Smooth, anything between = Smooth by Angle. Bodies with no
/// authored angle emit NO record and fall back to `poly_mesh::AUTO_SMOOTH_ANGLE_DEG`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyNormalRecord {
    pub node_id: u64,
    pub auto_smooth_deg: f32,
}

/// One poly modifier's authored `#[serde(skip)]` flags for the `FL4PMODF` side-channel.
/// `mod_index` is the modifier's position in the body's stack at save time (the stack itself is
/// in the blob, so the index is stable across one save/load round-trip). Dangling node_id /
/// out-of-range index / non-poly node are skipped on load, never a crash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyModFlagRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub bevel_consume: bool,
}

/// One modifier's `#[serde(skip)]` MASK params for the `FL4PSMTH` side-channel: the Smooth mask
/// feather plus the geometry-channel params that cannot live in the blob (`GeometryChannel` is
/// serialized inside `PolyModifier`, and bincode is positional — a new field there shifts every
/// byte after it and corrupts any `.fl4` that already stored a channel).
///
/// Keyed the same way as [`PolyModFlagRecord`] — `mod_index` is the modifier's position in the
/// body's stack at save time (the stack itself is in the blob, so the index is stable across one
/// round-trip). Dangling node_id / out-of-range index / non-poly node are skipped on load, never a
/// crash. Applies to ANY modifier with a channel, not just Smooth (Bevel and Inset have one too).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolySmoothRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub smooth_falloff: u32,
    /// `GeometryChannel::group_id` — which authored polygroup the channel matches.
    pub group_id: u16,
    /// `GeometryChannel::weight_min` / `weight_max` — authored edge-weight range.
    pub weight_min: f32,
    pub weight_max: f32,
}

/// One modifier's authored VOXEL REMESH params for the `FL4PRMSH` side-channel — the
/// `#[serde(skip)]` fields `PolyModifier.remesh_resolution` / `_smooth` / `_padding` / `_project`
/// / `_union_islands`.
///
/// Keyed the same way as [`PolyModFlagRecord`] and [`PolySmoothRecord`] — `mod_index` is the
/// modifier's position in the body's stack at save time (the stack itself is in the blob, so the
/// index is stable across one round-trip). Dangling node_id / out-of-range index / non-poly node
/// are skipped on load, never a crash.
///
/// Travel together in one record rather than one section per field: they are authored as a group
/// on the same modifier. `union_islands` is **trailing** so load dual-decodes older four-field
/// records (missing flag → false).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyRemeshRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub resolution: u32,
    pub smooth: bool,
    pub padding: f32,
    pub project: f32,
    /// Pre-remesh island union. Default false on old files (see load dual-decode).
    #[serde(default)]
    pub union_islands: bool,
}

/// Pre-`union_islands` remesh record (four params). Used only when decoding older `FL4PRMSH`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PolyRemeshRecordV0 {
    pub node_id: u64,
    pub mod_index: u32,
    pub resolution: u32,
    pub smooth: bool,
    pub padding: f32,
    pub project: f32,
}

impl From<PolyRemeshRecordV0> for PolyRemeshRecord {
    fn from(o: PolyRemeshRecordV0) -> Self {
        Self {
            node_id: o.node_id,
            mod_index: o.mod_index,
            resolution: o.resolution,
            smooth: o.smooth,
            padding: o.padding,
            project: o.project,
            union_islands: false,
        }
    }
}

/// One modifier's authored bevel VARIABLE WIDTH for the `FL4PBVAR` side-channel — the two
/// `#[serde(skip)]` fields `PolyModifier.bevel_var_width` (0=off / 1=auto by size / 2=authored
/// weight) and `bevel_width_min` (the small end of the band; `bevel_width` in the blob is the large
/// end).
///
/// Keyed the same way as its sibling records — `mod_index` is the modifier's position in the body's
/// stack at save time (the stack itself is in the blob, so the index is stable across one
/// round-trip). Dangling node_id / out-of-range index / non-poly node are skipped on load, never a
/// crash.
///
/// Emitted only when the modifier authored one of them: both off is exactly the pre-feature state,
/// so an untouched scene writes no section at all.
///
/// `auto_merge` = the "Auto Merge Vertices" toggle (`PolyModifier.bevel_auto_merge`), added to THIS
/// record rather than to a new tail section. That is allowed **only** because `FL4PBVAR` has never
/// shipped: it was introduced in the same unreleased change as the field, so no `.fl4` anywhere
/// carries a 13-byte version of this record that a 14-byte reader could mis-decode. Once this
/// section is committed the rule flips — widening it would then fail the ENTIRE load for any file an
/// older build wrote (`load_binary` propagates a section decode error with `?`), and the next
/// addition MUST be a new magic written last. See `POLY_BEVEL_VAR_MAGIC` in `io.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyBevelVarRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub var_width: u8,
    pub width_min: f32,
    pub auto_merge: bool,
}

/// One modifier's authored BEND params for the `FL4PBEND` side-channel — the six `#[serde(skip)]`
/// fields `PolyModifier.bend_angle_deg` / `_axis` / `_dir_deg` / `_limit_lo` / `_limit_hi` /
/// `_center`.
///
/// Keyed the same way as its sibling records — `mod_index` is the modifier's position in the body's
/// stack at save time (the stack itself is in the blob, so the index is stable across one
/// round-trip). Dangling node_id / out-of-range index / non-poly node are skipped on load, never a
/// crash.
///
/// All six travel together in one record rather than one section per field: they are authored as a
/// group on the same modifier, and one record keeps the reattach a single lookup. `center` is a
/// plain `[f32; 3]` so the record stays a flat POD — it is rebuilt into a `Vec3` at reattach time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyBendRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub angle_deg: f32,
    pub axis: u8,
    pub dir_deg: f32,
    pub limit_lo: f32,
    pub limit_hi: f32,
    pub center: [f32; 3],
}

/// One modifier's authored PolySculpt deltas for `FL4SCLP`.
///
/// `deltas` is `(VertexId.0, [dx,dy,dz])` so the record stays a flat POD. Empty + gen 0 is
/// the unused state and must not emit a section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolySculptRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub gen: u64,
    pub deltas: Vec<(u32, [f32; 3])>,
}

/// Collapse flag for `FL4PRMCL`. Only emitted when `collapse` is true (struct default is off).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyRemeshCollapseRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub collapse: bool,
}

/// Skip-relax-while-dragging for `FL4PRMRL`. Only emitted when the flag is true.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyRemeshSkipRelaxRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub skip_relax_drag: bool,
}

/// Skip-seal-while-dragging for `FL4PRMSL`. Only emitted when the flag is true.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyRemeshSkipSealRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub skip_seal_drag: bool,
}

/// Draft-res-while-dragging for `FL4PRMDR`. Only emitted when the flag is true.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PolyRemeshDropResRecord {
    pub node_id: u64,
    pub mod_index: u32,
    pub drop_res_drag: bool,
}

/// One body's authored mesh-map bake settings for the `FL4BAKE` tail.
///
/// Integers + `Vec<u64>` only: `source_kind` is append-only (0 = SelfMesh, 1 = HighPoly);
/// `enabled_maps` is the `MeshMapKind::bit()` OR. Never serde the kind enum as a map key.
/// Dangling `node_id`s are skipped on load, never a crash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeSettingsRecord {
    pub node_id: u64,
    pub enabled_maps: u32,
    pub res_global: u32,
    pub supersample: u32,
    pub padding_px: u32,
    pub source_kind: u32,
    pub high_nids: Vec<u64>,
    pub match_by_name: bool,
    pub max_front: f32,
    pub max_rear: f32,
    pub ao_rays: u32,
    pub curv_radius: f32,
}

/// G4 FL4BAKE payload. G1 files stored a bare `Vec<BakeSettingsRecord>`; the loader
/// accepts both (probe V2, fall back to V1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeTail {
    pub settings: Vec<BakeSettingsRecord>,
    pub images: Vec<BakedImageRecord>,
}

/// One named baked mesh-map for the `FL4BAKE` tail. `kind_bit` is `MeshMapKind::bit()`
/// — never an enum ordinal. `slot` indexes the FL4MATTX pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakedImageRecord {
    pub name: String,
    pub node_id: u64,
    pub kind_bit: u32,
    pub res: u32,
    pub slot: u32,
    pub display_key_lo: u64,
    pub display_key_hi: u64,
    pub baked_at_secs: u64,
}

/// Per-body 16-bit output flag (`FL4B16`). `bit16 != 0` → dual-plane vector maps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeBit16Record {
    pub node_id: u64,
    pub bit16: u32,
}

/// Per-body mesh-cage nids (`FL4CAGE`). Empty vec is omitted at save.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeCageRecord {
    pub node_id: u64,
    pub cage_nids: Vec<u64>,
}

/// G7 UDIM (`FL4UDIM`). Empty both vecs → no section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeUdimTail {
    pub flags: Vec<BakeUdimFlagRecord>,
    pub tiles: Vec<BakeUdimTileRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeUdimFlagRecord {
    pub node_id: u64,
    /// Non-zero → UDIM bake on.
    pub udim: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeUdimTileRecord {
    pub name: String,
    pub udim: u32,
}

/// Per-body GPU-ray flag (`FL4GRAY`). `gpu_rays != 0` → worker wgpu path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BakeGpuRayRecord {
    pub node_id: u64,
    pub gpu_rays: u32,
}

/// One persisted material-panel doc for the `FL4MPNLS` side-channel. `material_id` is the
/// stable `MaterialId.0`; `doc_json` is `serde_json::to_string` of
/// `material_panel::MaterialPanelDoc`. Dangling ids / corrupt JSON are skipped on load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialPanelRecord {
    pub material_id: u32,
    pub doc_json: String,
}

/// One persisted per-prim procedural node graph for the `FL4NODES` side-channel.
/// `prim_uid` matches `SdfPrimitive.prim_uid` / `ui.node_graphs` keys. `graph_json` is
/// `serde_json::to_string` of `shader_nodes::NodeGraph` (not bincode — Node field appends).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct NodeGraphRecord {
    pub prim_uid: u32,
    pub graph_json: String,
}

/// One body's post-DC mesh modifier stack for the in-blob `body_mesh_modifiers` side-channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BodyMeshModRecord {
    pub node_id: u64,
    pub modifiers: Vec<core_math::display_mesh_ops::DisplayMeshModRecord>,
}

/// One Mesh Terrain tile for the in-blob `terrain_tiles` side-channel.
/// `node_id` is the SdfBody; `(tx, tz)` is the lattice key. Display names are not identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TerrainTileRecord {
    pub node_id: u64,
    pub tx: i32,
    pub tz: i32,
    /// Island group NodeId (0 = unknown / recover from parent).
    #[serde(default)]
    pub island_group: u64,
    #[serde(default)]
    pub half_window: i32,
    #[serde(default)]
    pub stream_enabled: bool,
}

/// One parametric poly primitive for the in-blob `poly_params` side-channel.
///
/// `built_sig` is saved with the spec deliberately: it is the `content_signature()` of the mesh
/// the spec produced, so a body whose geometry was edited before saving still reads back as
/// authored rather than having its edits silently rebuilt away on the next slider drag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolyParamRecord {
    pub node_id: u64,
    pub spec: core_math::primitive_spec::PrimitiveSpec,
    pub built_sig: u64,
}

/// One body's field modifier stack for the in-blob `body_field_modifiers` side-channel.
/// Named `BodyFieldModsSide` to avoid clashing with `core_math::body_field_ops::BodyFieldModRecord`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BodyFieldModsSide {
    pub node_id: u64,
    pub modifiers: Vec<core_math::body_field_ops::BodyFieldModRecord>,
}

/// One persisted per-light volumetric record for the `FL4LTVOL` side-channel. `light_id` is the
/// stable `LightData.id`; the four params mirror `core_math::scene_graph::LightVolumetric`. Unknown
/// future lights (id not found on load) are skipped (forward-tolerant).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LightVolRecord {
    pub light_id: u32,
    pub enabled: bool,
    pub density: f32,
    pub softness: f32,
    pub zone: f32,
}

/// One persisted per-light ray-traced-shadow record for the `FL4LTRT` side-channel (Stage 4).
/// `light_id` is the stable `LightData.id`; the two params mirror
/// `core_math::scene_graph::LightRtShadow`. Unknown future lights (id not found on load) are
/// skipped (forward-tolerant).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LightRtRecord {
    pub light_id: u32,
    pub enabled: bool,
    pub radius: f32,
}

/// The persisted environment for the `FL4ENVIR` side-channel. `path` is stored RELATIVE to the
/// project file's directory when the HDRI lives under it (portable projects), absolute otherwise
/// (see `env_path_for_save`/`resolve_env_path`). `brightness` mirrors the in-blob
/// `RenderSettings.env_brightness` (same value at save time — kept in the record so the trio the
/// owner authored travels as one unit); `rotation_deg` exists ONLY here (the runtime
/// `env_rotation_deg` has no blob field — adding one would be the field trap). Contrast /
/// saturation / blur are NOT duplicated: they already persist as in-blob v3 fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct EnvRecord {
    pub path: String,
    pub brightness: f32,
    pub rotation_deg: f32,
}

/// Path string to persist for the loaded HDRI: relative to the project file's directory when the
/// HDRI is under it (portable), the absolute path otherwise. Pure fn (round-trip tested).
pub(crate) fn env_path_for_save(hdr: &Path, project_path: Option<&Path>) -> String {
    project_path
        .and_then(|pp| pp.parent())
        .and_then(|dir| hdr.strip_prefix(dir).ok())
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|| hdr.to_string_lossy().to_string())
}

/// Resolve a persisted env path against the directory the project file was loaded from.
/// Absolute paths pass through; relative ones join `base_dir`. Pure fn (round-trip tested);
/// existence is checked by the caller (missing file → toast + default env, never a load failure).
pub(crate) fn resolve_env_path(stored: &str, base_dir: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(stored);
    if p.is_absolute() {
        p
    } else {
        base_dir.map(|d| d.join(&p)).unwrap_or(p)
    }
}

/// High bit of [`MaterialChannelRecord::projection`] carrying `TexMap::green_dx` for NORMAL
/// channel records (the runtime field is `serde(skip)` — the TexMap bincode field-trap). The
/// projection codes only use values 0..=4, so the high bits are free. Zero decodes to OpenGL
/// green — the correct reading for every record written before the flag existed. An OLD build
/// loading a NEW save sees a projection value it clamps/ignores; the normal channel ignores
/// projection entirely in the shader (always triplanar), so this degrades safely.
pub(crate) const MATX2_PROJ_GREEN_DX_BIT: u32 = 1 << 8;

/// Bit 9 of [`MaterialChannelRecord::projection`] carrying `TexMap::high_precision` (the
/// 16-bit dual-plane flag — hi bytes at `tex_id`, lo bytes at the `__lo16` layer). Same
/// side-channel pattern as bit 8. Zero decodes to FALSE (single-plane 8-bit) — the correct
/// reading for every record written before the flag existed; a genuinely 16-bit map saved
/// by an older build degrades to its hi plane (quantized height), never to reading a
/// NEIGHBORING map as low bytes.
pub(crate) const MATX2_PROJ_HIGH_PREC_BIT: u32 = 1 << 9;

/// Bits 10-15 of [`MaterialChannelRecord::projection`] carrying the triplanar per-plane
/// rotations (`TexMap::rot_x/y/z`, 2 bits each — same layout as GPU-word bits 12-17).
/// Runtime fields are `serde(skip)` (the bincode field trap — see `TexMap`); zero decodes
/// to no rotation, the correct reading for every earlier record.
pub(crate) const MATX2_PROJ_ROT_SHIFT: u32 = 10;

/// One persisted non-albedo map channel for the `FL4MATX2` side-channel. `kind` is
/// `MapKind::wire_tag()` (1=Normal, 2=Roughness, 3=Metallic, 4=Emission, 5=Bump,
/// 6=Displacement, 7=SssColor — 0/Albedo never emitted: its TexMap is a serialized blob
/// field already). Unknown future tags are skipped on load (forward-tolerant).
#[derive(Serialize, Deserialize)]
pub(crate) struct MaterialChannelRecord {
    pub material_id: u32,
    pub kind: u32,
    pub tex_id: u32,
    pub strength: f32,
    pub uv_scale: f32,
    /// Projection code (0..=4) + flag bits: bit 8 = [`MATX2_PROJ_GREEN_DX_BIT`] (Normal only).
    pub projection: u32,
    /// Triplanar blend sharpness. `serde(default)` -> 0.0 for records written before this field
    /// existed; the loader maps a non-positive value onto `TexMap`'s 4.0 default, which is what
    /// those saves rendered with. Added rather than version-bumped per the FL4 extension rule.
    #[serde(default)]
    pub blend: f32,
}

/// One persisted albedo map for the #2 side-channel. `slot` = the 1-based texture-array layer the
/// material's `tex_id` referenced at save time; `png` = the PNG-encoded MAP_RES² RGBA8 pixels.
#[derive(Serialize, Deserialize)]
pub(crate) struct MaterialMapRecord {
    pub slot: u32,
    pub png: Vec<u8>,
}

/// Serialized vertex/index data for an imported mesh.
#[derive(Serialize, Deserialize)]
pub(crate) struct ImportedMeshRecord {
    pub cache_id: u64,
    pub vertices: Vec<[f32; 8]>,
    pub indices: Vec<u32>,
    pub triangle_count: u32,
}

/// Persistent density grid for a VoxelBody node (v7+). The node's `grid` field is #[serde(skip)]
/// (VoxelGrid carries non-serde GPU-sync state), so the grid round-trips through this side-channel,
/// reattached by node_id on load. `VoxelGridRecord` is the compact dense/sparse snapshot.
#[derive(Serialize, Deserialize)]
pub(crate) struct VoxelNodeRecord {
    pub node_id: u64,
    pub grid: core_math::voxel_sculpt::VoxelGridRecord,
}

/// Authored `CurveExtrude` shell/rim/open params for one prim (P1+P2). `wall_t`/`rim`/`open`/
/// `thickness`/`tube` are all `#[serde(skip)]` on the `SdfShape::CurveExtrude` variant (so the
/// bincode blob stays byte-identical to every pre-P1 `.fl4` — no field-trap), so their authored
/// values round-trip through this record, reattached by `prim_uid` on load — EXACTLY mirroring
/// `VoxelNodeRecord` (the `voxel_grids` precedent). Only prims with a non-default param emit a
/// record → a scene with none writes a file identical to pre-P1.
///
/// NOTE (P2, 2026-07-14): the `open`/`thickness`/`tube` fields were APPENDED to this record. Because
/// `curve_params` is empty on every pre-P1 file (all real scenes), those load with no records → the
/// record shape is irrelevant to them (fully safe). The only affected files are P1-era experimental
/// scenes (from 2026-07-14) that authored a shell/rim; those would need a re-save. New saves are
/// forward-safe.
#[derive(Serialize, Deserialize)]
pub(crate) struct CurveParamRecord {
    pub prim_uid: u32,
    pub wall_t: f32,
    pub rim: f32,
    #[serde(default)] pub open: bool,
    #[serde(default)] pub thickness: f32,
    #[serde(default)] pub tube: bool,
}

/// Authored poly CustomData layers for one PolygonalBody node (v8+). The body's runtime
/// `PolyMesh.cd` sidecar is NOT serialized (it lives on the non-serialized wrapper /
/// `cache.poly_custom_data`), so the authored layers round-trip through this record, reattached by
/// `node_id` on load — EXACTLY mirroring `VoxelNodeRecord` (the `voxel_grids` precedent,
/// `POLY_FOUNDATION_PLAN.md` §6.2). Stable-id keyed (raw `u32`/`EdgeKey` per element), NEVER by
/// positional slot — so the layers reattach correctly regardless of node ordering.
#[derive(Serialize, Deserialize)]
pub(crate) struct PolyLayerRecord {
    pub node_id: u64,
    /// Per-corner UV: `(HalfEdgeId.0, uv)`.
    pub uv: Vec<(u32, glam::Vec2)>,
    /// Per-edge crease: `(EdgeKey, weight)`.
    pub crease: Vec<(core_math::poly_mesh::EdgeKey, f32)>,
    /// Per-corner UV-seam bits: `HalfEdgeId.0`.
    pub seam: Vec<u32>,
    /// Per-corner hard-edge/sharp bits: `HalfEdgeId.0`.
    pub sharp: Vec<u32>,
    /// Per-face polygroup: `(FaceId.0, group_id)`. Empty on pre-polygroup files.
    #[serde(default)]
    pub groups: Vec<(u32, u16)>,
    /// Polygroup metadata (name/colour/kind). Empty on pre-polygroup files.
    #[serde(default)]
    pub group_meta: Vec<core_math::poly_mesh::PolyGroupMeta>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SceneData {
    pub primitives: Vec<SdfPrimitive>,
    pub layers: Vec<LayerData>,
    pub objects: Vec<ObjectData>,
    pub lights: Vec<LightData>,
    pub next_prim_id: u32,
    pub next_layer_id: u32,
    pub next_object_id: u32,
    pub next_light_id: u32,
    /// Naming counter for new scene-graph nodes (Body.003, Group.004, etc.).
    /// Added in v5+; old files default to 0 (recalculated on load).
    #[serde(default)]
    pub next_node_name_id: u32,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct LayerData {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
    pub visible: bool,
    pub position: [f32; 3],
    pub modifier: core_math::primitives::Modifier,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ObjectData {
    pub id: u32,
    pub name: String,
    pub collapsed: bool,
    pub locked: bool,
    pub layer_id: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct LightData {
    pub id: u32,
    pub name: String,
    pub light_type: LightType,
    pub azimuth: f32,
    pub elevation: f32,
    pub intensity: f32,
    pub color: [f32; 3],
    pub ambient: f32,
    #[serde(default = "default_save_ambient_color")]
    pub ambient_color: [f32; 3],
    #[serde(default)]
    pub ambient_from_hdr: bool,
    pub position: [f32; 3],
    pub range: f32,
    pub spot_inner_deg: f32,
    pub spot_outer_deg: f32,
    pub shadow_softness: f32,
    pub visible: bool,
    pub locked: bool,
    // Light linking (v4+)
    #[serde(default)]
    pub link_mode: LightLinkMode,
    #[serde(default)]
    pub link_targets: Vec<u32>,
    #[serde(default)]
    pub light_group: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct CameraState {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub distance: f32,
    pub target: [f32; 3],
    pub ortho: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RenderSettings {
    pub shading_mode: ShadingMode,
    pub resolution_scale: f32,
    pub render_quality: u32,
    pub exposure: f32,
    pub bloom_enabled: bool,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub ssr_enabled: bool,
    pub fog_enabled: bool,
    pub fog_density: f32,
    pub fog_height_falloff: f32,
    pub fog_scatter: f32,
    #[serde(default = "default_fog_color")]
    pub fog_color: [f32; 3],
    pub upsample_enabled: bool,
    pub rt_bounces: u32,
    pub rt_shadow_samples: u32,
    pub pt_max_bounces: u32,
    pub pt_samples_per_frame: u32,
    pub bg_color_top: [f32; 3],
    pub bg_color_bottom: [f32; 3],
    #[serde(default = "default_fov_deg")]
    pub fov_deg: f32,
    // --- v3 fields (serde defaults for backward compat) ---
    #[serde(default = "default_env_brightness")]
    pub env_brightness: f32,
    #[serde(default = "default_env_contrast")]
    pub env_contrast: f32,
    #[serde(default = "default_env_saturation")]
    pub env_saturation: f32,
    #[serde(default)]
    pub env_blur: f32,
    #[serde(default)]
    pub pt_dof_aperture: f32,
    #[serde(default = "default_pt_dof_falloff")]
    pub pt_dof_falloff: f32,
    #[serde(default)]
    pub pt_denoise: bool,
    #[serde(default = "default_denoise_strength")]
    pub denoise_strength: f32,
    #[serde(default = "default_denoise_radius")]
    pub denoise_radius: u32,
    #[serde(default = "default_fog_phase_g")]
    pub fog_phase_g: f32,
    #[serde(default = "default_fog_absorption")]
    pub fog_absorption: f32,
    #[serde(default = "default_fog_noise_scale")]
    pub fog_noise_scale: f32,
    #[serde(default = "default_fog_noise_strength")]
    pub fog_noise_strength: f32,
    #[serde(default = "default_fog_max_distance")]
    pub fog_max_distance: f32,
    #[serde(default = "default_fog_shadow_steps")]
    pub fog_shadow_steps: f32,
    /// Voxel cache resolution (64, 128, 256, 512).
    #[serde(default = "default_voxel_cache_res")]
    pub voxel_cache_res: u32,
    /// Adaptive resolution per body (scales with AABB extent).
    #[serde(default = "default_true")]
    pub adaptive_resolution: bool,
    /// Atlas texture XY resolution (256, 512, 1024). Higher = sharper edges, more VRAM.
    /// Requires app restart or atlas rebuild to take effect.
    #[serde(default = "default_atlas_xy")]
    pub atlas_xy_resolution: u32,
    /// Per-worker GPU enabled state: Vec of (adapter_name, enabled) pairs.
    /// Unmatched adapters default to enabled=true on load.
    #[serde(default)]
    pub worker_gpu_enabled: Vec<(String, bool)>,
    /// SDF silhouette anti-aliasing (v5+).
    #[serde(default = "default_true")]
    pub sdf_edge_aa_enabled: bool,
    /// FXAA post-process anti-aliasing (v5+).
    #[serde(default = "default_true")]
    pub fxaa_enabled: bool,
    // MatCap viewport settings (v6+)
    #[serde(default = "default_matcap_bg")]
    pub matcap_bg_color: [f32; 3],
    #[serde(default)]
    pub matcap_bg_gradient: bool,
    #[serde(default = "default_matcap_bg_top")]
    pub matcap_bg_gradient_top: [f32; 3],
    #[serde(default)]
    pub matcap_rotation: f32,
    #[serde(default = "default_true")]
    pub matcap_edge_enabled: bool,
    #[serde(default = "default_matcap_edge_color")]
    pub matcap_edge_color: [f32; 3],
    #[serde(default = "default_one")]
    pub matcap_edge_thickness: f32,
    #[serde(default = "default_matcap_edge_threshold")]
    pub matcap_edge_threshold: f32,
}
fn default_true() -> bool { true }
fn default_one() -> f32 { 1.0 }
fn default_save_ambient_color() -> [f32; 3] { [1.0, 1.0, 1.0] }
/// MatCap flat BG default — near-black R2 G2 B2 (owner-approved “UI gray” swatch).
fn default_matcap_bg() -> [f32; 3] { [2.0 / 255.0, 2.0 / 255.0, 2.0 / 255.0] }
fn default_matcap_bg_top() -> [f32; 3] { [0.14, 0.14, 0.14] }
fn default_matcap_edge_color() -> [f32; 3] { [0.078, 0.078, 0.086] }
fn default_matcap_edge_threshold() -> f32 { 0.3 }
fn default_wp_locked_normal() -> [f32; 3] { [0.0, 1.0, 0.0] }
fn default_snap_rotation_deg() -> f32 { 15.0 }
fn default_voxel_cache_res() -> u32 { 128 }
fn default_atlas_xy() -> u32 { 256 }
fn default_fov_deg() -> f32 { 90.0 }
fn default_env_brightness() -> f32 { 1.0 }
fn default_env_contrast() -> f32 { 1.0 }
fn default_env_saturation() -> f32 { 1.0 }
fn default_pt_dof_falloff() -> f32 { 0.5 }
fn default_denoise_strength() -> f32 { 0.5 }
fn default_denoise_radius() -> u32 { 3 }
fn default_fog_phase_g() -> f32 { 0.7 }
fn default_fog_color() -> [f32; 3] { [0.35, 0.40, 0.50] }
fn default_fog_absorption() -> f32 { 0.3 }
fn default_fog_noise_scale() -> f32 { 3.0 }
fn default_fog_noise_strength() -> f32 { 0.4 }
fn default_fog_max_distance() -> f32 { 30.0 }
fn default_fog_shadow_steps() -> f32 { 4.0 }

#[derive(Serialize, Deserialize)]
pub(crate) struct EditorSettings {
    pub snap_mode: SnapMode,
    pub snap_grid: f32,
    #[serde(default = "default_snap_rotation_deg")]
    pub snap_rotation_deg: f32,
    pub grid_cell_size: f32,
    pub grid_opacity: f32,
    pub grid_visible: bool,
    pub wp_mode: WorkPlaneMode,
    pub wp_settings: WpSettingsData,
    pub wp_saved: Vec<SavedWpData>,
    /// Locked workplane origin (M88 — persisted so locked WP survives save/load).
    #[serde(default)]
    pub wp_locked_origin: [f32; 3],
    /// Locked workplane normal (M88 — persisted so locked WP survives save/load).
    #[serde(default = "default_wp_locked_normal")]
    pub wp_locked_normal: [f32; 3],
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WpSettingsData {
    pub spacing: f32,
    pub color: [f32; 3],
    pub opacity: f32,
    pub orientation: WorkPlaneOrientation,
    pub origin: [f32; 3],
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SavedWpData {
    pub name: String,
    pub origin: [f32; 3],
    pub normal: [f32; 3],
}

// ==================================================================
// File I/O
// ==================================================================


mod io;
mod paths;
pub(crate) use io::*;
pub(crate) use paths::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod diag_edge_loop_22;
#[cfg(test)]
mod diag_owner_repro;
#[cfg(test)]
mod diag_edge_loop_1234;
#[cfg(test)]
mod diag_circle_knife;
#[cfg(test)]
mod diag_edgesel_bool;
#[cfg(test)]
mod diag_unbevel;
