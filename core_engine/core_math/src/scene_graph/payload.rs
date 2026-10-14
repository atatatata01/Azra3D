//! NodePayload — what a scene node contains.
//!
//! Pure relocation from former `scene_graph.rs`.
//!
//! # SERDE / BINCODE — APPEND-ONLY ENUM (CRITICAL)
//!
//! `NodePayload` is serialized with bincode, which tags enum variants by **positional index**.
//! NEVER reorder, insert mid-list, or remove variants. New variants append ONLY after the current
//! tail (`NurbsSolid`). The VoxelBody / SdfObject / NurbsSolid doc comments below are authoritative
//! — they travel WITH this enum (GOD_FILES_SPLIT_PLAN serde gate).

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::half_edge::HalfEdgeMesh;
use crate::nurbs::{NurbsFace, NurbsSurface as NurbsSurfaceData};
use crate::primitives::{BlendProfile, BooleanOp, MaterialId, PbrMaterial, SdfPrimitive};

use super::{BodyChildRef, LightData, MeshCacheId, PolyModifier};

// ============================================================
// Node Payload — what a scene node contains
// ============================================================

/// The data carried by each scene node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodePayload {
    /// Scene root (exactly one per graph).
    Root,

    /// Grouping folder (replaces PrimLayer and provides nesting).
    Group { name: String },

    /// An SDF boolean body: a set of primitives evaluated together.
    /// The first primitive is the base shape; subsequent ones are boolean operations.
    SdfBody {
        name: String,
        /// The primitives that form this body's SDF tree (OWNED).
        primitives: Vec<SdfPrimitive>,
        /// Cached rasterization mesh (None = needs meshing).
        mesh_cache_id: Option<MeshCacheId>,
        /// How this body's evaluated SDF result combines with its parent scope.
        /// Ignored for top-level bodies (direct children of Root — always union).
        #[serde(default = "default_body_op")]
        body_op: BooleanOp,
        /// World-space AABB center (auto-recentered after prim moves).
        /// Used by the bake system to keep the grid tight around geometry.
        /// NOT the user's rotation/scale pivot — see SceneNode.pivot for that.
        #[serde(default)]
        origin: Vec3,
        /// Body-level rotation quaternion (default identity).
        #[serde(default = "default_body_rotation")]
        body_rotation: Quat,
        /// Body-level scale (default Vec3::ONE).
        #[serde(default = "default_body_scale")]
        body_scale: Vec3,
        /// Body-level mirror: bit 0 = X, bit 1 = Y, bit 2 = Z.
        /// Folds the evaluation point before evaluating any prims in this body.
        #[serde(default)]
        mirror_flags: u8,
        /// Mirror fold direction: bit 0 = flip X, bit 1 = flip Y, bit 2 = flip Z.
        /// Default 0 = keep positive side. Setting a bit = keep negative side for that axis.
        #[serde(default)]
        mirror_sign: u8,
        /// Mirror plane center (default = body origin).
        #[serde(default)]
        mirror_center: Vec3,
        /// Global edge rounding applied to body SDF result (default 0.0).
        #[serde(default)]
        bevel_radius: f32,
        /// Blend profile for body_op smooth transitions (only meaningful when k > 0).
        #[serde(default)]
        blend_profile: BlendProfile,
        /// Body-level PBR material for raster preview (auto-synced from first prim).
        #[serde(default)]
        material: PbrMaterial,
        /// Unified display + eval ordering over this body's loose prims AND child bodies.
        /// References only (PrimUid / NodeId); `primitives` and `SceneNode.children` remain the
        /// data owners. This is THE one sequence the outliner draws, the bake serializer walks,
        /// and the user reorders.
        ///
        /// SERIALIZATION — `#[serde(skip)]` (NOT in the bincode `ProjectFile` blob). MANDATORY for
        /// backward-compat: bincode is positional and NOT self-describing, so a trailing `Vec` field
        /// makes a genuine v7 blob (which has no bytes here) misread the NEXT field's bytes as this
        /// Vec's length prefix → garbage → corrupts the whole file. `#[serde(default)]` does NOT
        /// rescue a missing trailing field in bincode (empirically proven: garbage variant tag /
        /// `unexpected end of file`). Instead `child_order` round-trips OUT OF BAND via an `FL4ORDER`
        /// side-channel appended AFTER the blob (see `project.rs` `save_binary`/`load_binary`),
        /// reattached by node id on load and self-healed to the legacy prims-then-bodies order for
        /// any body the side-channel doesn't cover. The blob layout thus stays byte-identical to v7.
        /// Mirrors the existing `VoxelBody.grid` `#[serde(skip)]` + `voxel_grids` side-channel pattern.
        #[serde(skip)]
        child_order: Vec<BodyChildRef>,
        /// Post-DC display mesh modifier stack (Smooth / Mesh Mirror / …). RUNTIME + side-channel only:
        /// `#[serde(skip)]` so the bincode body layout stays stable. Round-trips via ProjectFile
        /// `body_mesh_modifiers` (`#[serde(default)]` in-blob), reattached by node id on load
        /// (same pattern as `poly_layers` / `voxel_grids`). Empty = identity display mesh.
        #[serde(skip)]
        mesh_modifiers: Vec<crate::display_mesh_ops::DisplayMeshModRecord>,
        /// Body **field** modifier stack (Volume Mirror, Body Bevel, …). RUNTIME + side-channel.
        /// Syncs into hardwired `mirror_*` / `bevel_radius` for bake/GPU (`BodyField` parity).
        /// Round-trips via ProjectFile `body_field_modifiers`. Empty may be seeded from hardwired.
        #[serde(skip)]
        field_modifiers: Vec<crate::body_field_ops::BodyFieldModRecord>,
    },

    /// An imported triangle mesh (glTF, OBJ) — not SDF-based.
    ImportedMesh {
        name: String,
        mesh_cache_id: MeshCacheId,
        materials: Vec<PbrMaterial>,
    },

    /// A light source.
    Light(LightData),

    /// A camera bookmark.
    Camera {
        name: String,
        position: Vec3,
        target: Vec3,
        fov: f32,
    },

    /// A polygonal mesh body (half-edge topology, editable).
    /// Mesh is `Arc`-wrapped for copy-on-write undo snapshots.
    /// `modifiers` is a non-destructive modifier stack applied on top of the base mesh.
    ///
    /// `source` records whether `mesh` is authored geometry or the derived output of a
    /// [`crate::primitive_spec::PrimitiveSpec`] the user can still re-parameterize. It is
    /// **`#[serde(skip)]`**: `modifiers` is already this variant's trailing serialized field, and
    /// `#[serde(default)]` does NOT rescue a missing trailing field in bincode (see the
    /// `SdfBody.child_order` note above), so a naked new field here would corrupt every existing
    /// `.fl4`. It persists instead through `ProjectFile.poly_params`, keyed by node id — the same
    /// in-blob side-table pattern as `poly_layers` / `voxel_grids`.
    PolygonalBody {
        name: String,
        mesh: Arc<HalfEdgeMesh>,
        #[serde(skip)]
        source: crate::primitive_spec::MeshSource,
        mesh_cache_id: Option<MeshCacheId>,
        material: PbrMaterial,
        #[serde(default)]
        modifiers: Vec<PolyModifier>,
    },

    /// A NURBS surface (control-point grid, tessellated for display).
    /// Surface is `Arc`-wrapped for copy-on-write undo snapshots.
    NurbsBody {
        name: String,
        surface: Arc<NurbsSurfaceData>,
        mesh_cache_id: Option<MeshCacheId>,
        material: PbrMaterial,
    },

    /// A shared PBR material. Referenced from SdfPrimitive via MaterialId.
    Material {
        id: MaterialId,
        name: String,
        data: PbrMaterial,
    },

    /// Transform-only locator (for grouping, attachment points, etc.).
    Empty { name: String },

    /// A density-voxel sculpt object (3D-Coat-style). Peer of SdfBody. The `grid` is the persistent
    /// CPU cache of the sculpt; the GPU sparse atlas is authority DURING an active stroke. `grid` is
    /// `#[serde(skip)]` (VoxelGrid carries non-serde GPU-sync state) — it round-trips via the
    /// VoxelGridRecord side-channel (project save/load). `mesh_cache_id` = the Object-Mode display mesh.
    /// APPEND-ONLY TAIL (VoxelBody = index 10, SdfObject = index 11): bincode serializes enum variants by
    /// positional index, so these two are the LAST variants — appending keeps every pre-existing index
    /// (Root..VoxelBody = 0..10) stable → old save files still load. NEVER insert a variant ABOVE VoxelBody
    /// (doing so shifts Material/Empty indices and corrupts all existing scenes). New variants append AFTER
    /// SdfObject only.
    VoxelBody {
        name: String,
        #[serde(skip, default = "default_voxel_grid")]
        grid: crate::voxel_sculpt::VoxelGrid,
        /// Object-Mode display mesh (None until first extracted). Placement is via the SceneNode
        /// transform (like ImportedMesh) — the grid spans [-bounds,+bounds] in its own local frame.
        mesh_cache_id: Option<MeshCacheId>,
        #[serde(default)]
        material: PbrMaterial,
    },

    /// A typed SDF OBJECT — a PURE CONTAINER for SdfBody children, giving the Object → Body → Primitive
    /// hierarchy. It holds NO primitives and NO SDF parameters; its child-body NodeIds live in the
    /// standard `SceneNode.children` edges (ONE source of truth — never duplicated into the payload). The
    /// Object owns its world placement via the SceneNode transform (local_position/rotation/scale, exactly
    /// like Group/Empty); each child body keeps its OWN body-local frame (origin/body_rotation/body_scale),
    /// composed at render/pick/eval time via `effective_body_transform()`. The Object is TRANSPARENT to
    /// render/bake — `is_renderable()` deliberately excludes it; only its bodies hold meshes and draw, and
    /// in Object Mode the object's surface is the union of those instanced baked meshes (NEVER a fused
    /// raymarch — Display Invariant). `mesh_cache_id` is reserved for a future pre-baked union mesh and
    /// stays None for now. Appended at index 11 (after VoxelBody) — see the append-only-tail note above.
    SdfObject {
        name: String,
        #[serde(default)]
        mesh_cache_id: Option<MeshCacheId>,
    },

    /// Multi-face NURBS solid — one outliner node, faces as sub-elements (Plasticity-style).
    /// APPEND-ONLY after `SdfObject` (index 12). Never insert above this or shift tags.
    /// Do NOT change `NurbsBody` — single-patch type stays for plane/sphere/etc. and old saves.
    NurbsSolid {
        name: String,
        faces: Vec<NurbsFace>,
        mesh_cache_id: Option<MeshCacheId>,
        material: PbrMaterial,
    },
}

fn default_body_op() -> BooleanOp { BooleanOp::Union { k: 0.0 } }
fn default_body_rotation() -> Quat { Quat::IDENTITY }
fn default_body_scale() -> Vec3 { Vec3::ONE }
/// Placeholder grid for a deserialized VoxelBody before the VoxelGridRecord side-channel reattaches the
/// real density (serde skips `grid`). 64³ empty — replaced on load (Step 7) or by voxel_create.
fn default_voxel_grid() -> crate::voxel_sculpt::VoxelGrid { crate::voxel_sculpt::VoxelGrid::new_empty(64, 2.0) }

impl NodePayload {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Root => "Scene",
            Self::Group { name } => name,
            Self::SdfBody { name, .. } => name,
            Self::ImportedMesh { name, .. } => name,
            Self::VoxelBody { name, .. } => name,
            Self::PolygonalBody { name, .. } => name,
            Self::NurbsBody { name, .. } => name,
            Self::Light(data) => &data.name,
            Self::Camera { name, .. } => name,
            Self::Material { name, .. } => name,
            Self::Empty { name } => name,
            Self::SdfObject { name, .. } => name,
            Self::NurbsSolid { name, .. } => name,
        }
    }

    pub fn is_renderable(&self) -> bool {
        matches!(
            self,
            Self::SdfBody { .. }
                | Self::ImportedMesh { .. }
                | Self::VoxelBody { .. }
                | Self::PolygonalBody { .. }
                | Self::NurbsBody { .. }
                | Self::NurbsSolid { .. }
        )
    }
}
