//! Node identifiers, light data, poly modifiers, pivot config, and SceneNode.
//!
//! Pure relocation from former `scene_graph.rs`. Serialized field/variant order is unchanged.

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::NodePayload;

// ============================================================
// Node Identifiers
// ============================================================

/// Unique identifier for any node in the scene graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// Reference to a cached mesh in the render system.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MeshCacheId(pub u64);

/// One slot in a body's unified child ordering. References (never owns) either a
/// loose prim (by stable PrimUid) or a child node (by NodeId). The pointed-to data
/// still lives in `SdfBody.primitives` / `SceneNode.children` respectively — this is
/// purely a sequencing index over those two storage owners.
///
/// BINCODE SAFETY: this is a NEW enum, so its variant indices start fresh — appending
/// `Prim`=0, `Body`=1 is safe because nothing reads an older tag layout. Treat it
/// append-only henceforth: NEVER insert or reorder variants (bincode tags variants by
/// positional index — see the `NodePayload` append-only-tail note).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BodyChildRef {
    /// A loose primitive in this body's `primitives` Vec, by stable PrimUid.
    Prim(u32),
    /// A child node (SdfBody / SdfObject / Group) in this body's `children` edges.
    Body(NodeId),
}

// ============================================================
// Light Data (engine-level, no GPU dependency)
// ============================================================

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum LightType {
    Directional,
    Point,
    Spot,
}

/// Light linking mode: controls which objects a light affects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LightLinkMode {
    Default,  // affects all objects
    Include,  // affects ONLY listed objects
    Exclude,  // affects everything EXCEPT listed objects
}
impl Default for LightLinkMode {
    fn default() -> Self { Self::Default }
}

/// Per-light volumetric scattering ("god rays" / light shafts). RUNTIME-ONLY authored state:
/// this struct is carried on `LightData` behind `#[serde(skip)]`, so it is NEVER in the bincode
/// blob (the confirmed field trap — a naked trailing field on a bincode struct corrupts every
/// existing `.fl4`). It round-trips through the self-delimiting `FL4LTVOL` tail section in
/// `project.rs` (the `FL4ENVIR` precedent), keyed by light `id`. `zone <= 0.0` means "use the
/// light's `range` as the volume reach". Serialized to the GPU as 4 extra floats per light
/// (see `serialize_to_gpu`, stride 20 — must match `FLOATS_PER_LIGHT` in `sdf_lighting.wgsl`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LightVolumetric {
    /// Master toggle: light emits volumetric in-scatter (shafts/cone/glow) when true.
    pub enabled: bool,
    /// Scattering strength of the light's own participating medium (0 = none, 1 = default).
    pub density: f32,
    /// Beam-edge feather 0..1 (widens the cone/sphere falloff band). 0 = crisp, 1 = very soft.
    pub softness: f32,
    /// Volume reach in world units (how far shafts travel / point-spot radius). 0 = use `range`.
    pub zone: f32,
}
impl Default for LightVolumetric {
    fn default() -> Self { Self { enabled: false, density: 1.0, softness: 0.5, zone: 0.0 } }
}

/// Per-light ray-traced shadow control (RT_SHADOWS_DESIGN Stage 4 — the Approach-C hybrid).
/// RUNTIME-ONLY authored state, carried on `LightData` behind `#[serde(skip)]` (the confirmed
/// bincode field trap — a naked trailing field corrupts every existing `.fl4`); it round-trips
/// through the self-delimiting `FL4LTRT` tail section in `project.rs` (the `FL4LTVOL` precedent),
/// keyed by light `id`. Mirrors `LightVolumetric`.
///
/// `enabled` defaults to **true**: with the GLOBAL `RenderSettings.rt_shadows` master toggle OFF
/// every light stays on its legacy technique (CSM sun / SDF soft-march point-spot), so a default
/// light is a no-op until the user turns RT on globally; once global RT is ON, a default light
/// participates (reproducing the Stage-3 "global on → all point/spot use RT" behaviour — the
/// no-regression contract). The hybrid is authored by turning RT OFF on individual fill lights.
///
/// `radius` = the light's angular softness 0..1 (the per-light replacement for the old GLOBAL
/// `rt_shadow_softness`). `0` = inherit the global default softness (so an untouched scene renders
/// exactly like Stage 3); `> 0` drives that light's penumbra disc directly. Serialized to the GPU
/// as 2 extra floats per light (see `serialize_to_gpu`, stride 22 = `FLOATS_PER_LIGHT`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LightRtShadow {
    /// This light casts ray-traced (all-geometry, BVH) projected shadows when the GLOBAL RT toggle
    /// is on. `false` = keep the legacy per-light technique even while global RT is on (the hybrid).
    pub enabled: bool,
    /// Angular soft-shadow radius 0..1. `0` = inherit the global default softness; `> 0` overrides.
    pub radius: f32,
}
impl Default for LightRtShadow {
    fn default() -> Self { Self { enabled: true, radius: 0.0 } }
}

/// Light data owned by the scene graph. Single source of truth for all light state.
/// GPU serialization, gizmo positioning, and construction helpers live here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LightData {
    pub name: String,
    #[serde(default)]
    pub id: u32,
    pub light_type: LightType,
    pub azimuth: f32,
    pub elevation: f32,
    pub intensity: f32,
    pub color: [f32; 3],
    /// Flat ambient **strength** (0..1+) for the key directional light. Multiplies
    /// `ambient_color` (or the HDR-derived tint when `ambient_from_hdr` is on).
    pub ambient: f32,
    /// Flat ambient tint (linear RGB). When `ambient_from_hdr` is true, GPU packing
    /// replaces this with an SH-derived sky average so the Ambient channel tracks the HDRI.
    #[serde(default = "default_ambient_color")]
    pub ambient_color: [f32; 3],
    /// When true (directional only), ambient tint is driven by the loaded HDRI
    /// (SH irradiance average × env brightness). Strength still uses `ambient`.
    #[serde(default)]
    pub ambient_from_hdr: bool,
    pub position: Vec3,
    pub range: f32,
    pub spot_inner_deg: f32,
    pub spot_outer_deg: f32,
    pub shadow_softness: f32,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub locked: bool,
    // Light linking (persisted in graph for save/load fidelity)
    #[serde(default)]
    pub link_mode: LightLinkMode,
    #[serde(default)]
    pub link_targets: Vec<u32>,
    #[serde(default)]
    pub light_group: String,
    /// Volumetric scattering ("god rays") authored params. `#[serde(skip)]` — NEVER in the bincode
    /// blob (field trap); persisted out-of-band via the `FL4LTVOL` tail section (see `project.rs`),
    /// keyed by light `id`. Loads to `LightVolumetric::default()` (disabled) on every older `.fl4`.
    #[serde(skip)]
    pub volumetric: LightVolumetric,
    /// Per-light ray-traced shadow params (RT_SHADOWS_DESIGN Stage 4). `#[serde(skip)]` — NEVER in
    /// the bincode blob (field trap); persisted out-of-band via the `FL4LTRT` tail section (see
    /// `project.rs`), keyed by light `id`. On every older `.fl4` the skip field loads to
    /// `LightRtShadow::default()` (enabled=true, radius=0 → inherit global) → identical to Stage 3.
    #[serde(skip)]
    pub rt_shadow: LightRtShadow,
}

fn default_visible() -> bool { true }
fn default_ambient_color() -> [f32; 3] { [1.0, 1.0, 1.0] }

/// Default sun intensity = π (irradiance at normal incidence, the path tracer's and the
/// normalized raster BRDF's unit). With Lambert `albedo/π`, a sun of irradiance π lights a
/// white card facing it to exit radiance 1.0 — display white at exposure 1 — which is exactly
/// how the default scene looked before the raster direct BRDF was normalized (2026-10-13,
/// RASTER-DIRECT-UNNORMALIZED). The default point (1.5π) and spot (2π) keep the same factor.
pub const DEFAULT_SUN_IRRADIANCE: f32 = std::f32::consts::PI;

impl Default for LightData {
    fn default() -> Self {
        Self {
            name: "Sun.001".into(),
            // id 1, NOT 0: the default-scene light is a real allocated light. Scenes init
            // next_light_id = 2, so id 1 keeps allocation strictly increasing and matches
            // the "Sun.001" name. id 0 sat outside the allocator's range — any second id-0
            // light (old saves) is unreachable via light_nid (first-match-by-id) and
            // delete-by-id hits the wrong node. Load-time self-heal reassigns duplicates.
            id: 1,
            light_type: LightType::Directional,
            azimuth: 45.0,
            elevation: 55.0,
            intensity: DEFAULT_SUN_IRRADIANCE,
            color: [1.0, 0.98, 0.95],
            // ZERO, matching Point and Spot (owner 2026-07-29). This was the only light type
            // defaulting to non-zero ambient, and with an HDRI supplying environment light it
            // double-counted: two ambient contributions for one scene.
            //
            // Existing scenes that relied on it will look darker on the shadow side. That is the
            // correct behaviour — ambient belongs to the environment, not to a directional light —
            // and the slider is still there for anyone who wants it back deliberately.
            ambient: 0.0,
            ambient_color: [1.0, 1.0, 1.0],
            ambient_from_hdr: false,
            position: Vec3::ZERO,
            range: 10.0,
            spot_inner_deg: 25.0,
            spot_outer_deg: 35.0,
            shadow_softness: 12.0,
            visible: true,
            locked: false,
            link_mode: LightLinkMode::Default,
            link_targets: Vec::new(),
            light_group: String::new(),
            volumetric: LightVolumetric::default(),
            rt_shadow: LightRtShadow::default(),
        }
    }
}

impl LightData {
    pub fn direction(&self) -> [f32; 3] {
        let az = self.azimuth.to_radians();
        let el = self.elevation.to_radians();
        let x = az.sin() * el.cos();
        let y = el.sin();
        let z = -az.cos() * el.cos();
        let len = (x * x + y * y + z * z).sqrt();
        if len < 0.001 {
            return [0.0, 1.0, 0.0];
        }
        [x / len, y / len, z / len]
    }

    pub fn new_point(id: u32, name: String, pos: Vec3) -> Self {
        Self {
            id, name, light_type: LightType::Point, position: pos,
            azimuth: 0.0, elevation: 0.0, intensity: 1.5 * std::f32::consts::PI, color: [1.0, 0.95, 0.9],
            ambient: 0.0, ambient_color: [1.0, 1.0, 1.0], ambient_from_hdr: false,
            range: 10.0, spot_inner_deg: 0.0, spot_outer_deg: 0.0,
            shadow_softness: 12.0, visible: true, locked: false,
            link_mode: LightLinkMode::Default, link_targets: Vec::new(), light_group: String::new(),
            volumetric: LightVolumetric::default(), rt_shadow: LightRtShadow::default(),
        }
    }

    pub fn new_spot(id: u32, name: String, pos: Vec3) -> Self {
        Self {
            id, name, light_type: LightType::Spot, position: pos,
            azimuth: 0.0, elevation: -45.0, intensity: 2.0 * std::f32::consts::PI, color: [1.0, 0.98, 0.95],
            ambient: 0.0, ambient_color: [1.0, 1.0, 1.0], ambient_from_hdr: false,
            range: 15.0, spot_inner_deg: 25.0, spot_outer_deg: 35.0,
            shadow_softness: 12.0, visible: true, locked: false,
            link_mode: LightLinkMode::Default, link_targets: Vec::new(), light_group: String::new(),
            volumetric: LightVolumetric::default(), rt_shadow: LightRtShadow::default(),
        }
    }

    /// Viewport / transform gizmo pivot for this light.
    ///
    /// **Blender sun parity (owner 2026-08-07):** Directional lights use authored
    /// `position` as the fixed icon/gizmo origin. Rotation only changes azimuth /
    /// elevation (ray direction); the sun empty does **not** orbit when you R.
    /// Lighting for Directional is position-independent (parallel rays); `position`
    /// is only the viewport marker (and G can still re-place the marker).
    /// Point / Spot use the physical source position.
    pub fn gizmo_position(&self) -> Vec3 {
        self.position
    }

    /// Serialize to a 25-float GPU buffer entry. Layout must match the shader's light struct
    /// (`FLOATS_PER_LIGHT` in `sdf_lighting.wgsl` + the `VL_STRIDE` mirror in `fog_entry.wgsl`).
    /// Floats 0..15 are the base light; 16..19 volumetric; 20..21 RT-shadow; 22..24 ambient RGB
    /// (flat ambient tint — strength is slot 15). HDR-driven tint is applied by the packer when
    /// `ambient_from_hdr` is set (this function writes the authored color).
    pub fn serialize_to_gpu(&self) -> [f32; 25] {
        let dir = self.direction();
        let lt = match self.light_type {
            LightType::Directional => 0.0, LightType::Point => 1.0, LightType::Spot => 2.0,
        };
        let v = &self.volumetric;
        let zone_eff = if v.zone > 0.0 { v.zone } else { self.range };
        let rt = &self.rt_shadow;
        let ac = self.ambient_color;
        [
            lt,
            self.position.x, self.position.y, self.position.z,
            dir[0], dir[1], dir[2],
            self.intensity,
            self.color[0], self.color[1], self.color[2],
            self.range,
            self.spot_inner_deg.to_radians().cos(),
            self.spot_outer_deg.to_radians().cos(),
            self.shadow_softness,
            self.ambient,
            if v.enabled { 1.0 } else { 0.0 },
            v.density,
            v.softness.clamp(0.0, 1.0),
            zone_eff,
            // Slot 20 — RT participation (sun opt-in via radius; see historic comment).
            if rt.enabled && (!matches!(self.light_type, LightType::Directional) || rt.radius > 0.0) {
                1.0
            } else {
                0.0
            },
            rt.radius.max(0.0),
            ac[0], ac[1], ac[2],
        ]
    }

    /// Effective ambient tint for GPU packing. When `ambient_from_hdr` and `hdr_tint` is
    /// provided (SH sky average), use that; otherwise the authored `ambient_color`.
    pub fn ambient_tint_for_gpu(&self, hdr_tint: Option<[f32; 3]>) -> [f32; 3] {
        if self.ambient_from_hdr {
            if let Some(t) = hdr_tint {
                return t;
            }
        }
        self.ambient_color
    }
}

// ============================================================
// Poly Modifier Stack (non-destructive mesh operations)
// ============================================================

/// Modifier type for polygonal bodies — applied non-destructively on top of
/// the base mesh. The stack is evaluated sequentially; the output of each
/// modifier feeds into the next. The base mesh remains editable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum PolyModifierType {
    Subdivide,   // Catmull-Clark subdivision
    Mirror,      // Mirror across axis (with optional merge)
    Solidify,    // Add thickness / shell
    Smooth,      // Laplacian smoothing (geometry)
    Triangulate, // Convert quads to triangles
    Decimate,    // Reduce poly count (planar dissolve)
    Array,       // Repeat along axis
    Bevel,       // Bevel all edges
    // ── append-only (serde / .fl4 enum tag order) ──
    /// Mesh boolean vs another poly body (cutter). Op + target NodeId on the modifier.
    Boolean,
    /// Blender-style weighted normals (area × corner-angle; optional keep-sharp).
    WeightedNormal,
    /// Smooth shading: recompute fully averaged smooth vertex normals (no sharp splits).
    SmoothNormal,
    /// One or more offset copies merged into the display mesh (non-destructive).
    Duplicate,
    /// Radial / circular array around a local axis (Blender Array object-offset style simplified).
    ArrayCircular,
    /// PROCEDURAL PIPE (owner 2026-08-04): sweeps a circular profile along a retained
    /// polyline, so radius / segments stay editable forever -- not just in the redo panel.
    /// The path lives on the modifier (`pipe_path`), authored state via `#[serde(default)]`.
    Pipe,
    /// SCATTER (owner 2026-08-05): instance another scene object over THIS mesh's surface.
    /// The target mesh supplies the points (vertices / face centres / area-weighted random
    /// points); every random channel derives from one seed, so the result is reproducible.
    /// Source object referenced by `scatter_source_nid` — 0 / missing / self = clean no-op.
    ///
    /// APPEND-ONLY until Inset: new variants go AFTER Scatter (bincode tags by index).
    Scatter,
    /// Non-destructive Inset (Blender I): inset faces matching geometry channel.
    /// APPEND-ONLY: keep after Scatter.
    Inset,
    /// Vertex Merge by Distance (Blender Merge by Distance / Remove Doubles).
    /// Welds verts within `merge_distance`; cluster lands at the **average** position.
    /// APPEND-ONLY: keep after Inset.
    VertexMerge,
    /// Mesh Cleanup — flatten folds, merge coplanar, optional limited dissolve, T-junction repair.
    /// Wraps `mesh_boolean::cleanup_mesh_with`. APPEND-ONLY: keep after VertexMerge.
    MeshCleanup,
    /// VOXEL REMESH (3D-Coat / Blender Voxel Remesh paradigm): poly → signed-distance
    /// voxel field (`region_mesh_to_sdf`) → Surface Nets extract → uniform remesh.
    /// Non-destructive; base mesh stays editable. Uses the sculpt voxel lattice + surface
    /// nets backend — never a second render path (Display Invariant).
    ///
    /// Authored on a branch that forked before Inset/VertexMerge/MeshCleanup existed, where
    /// it sat directly after `Scatter`. Placing it there on merge would have shifted those
    /// three tags and corrupted every `.fl4` containing them — it belongs LAST.
    ///
    /// APPEND-ONLY: new variants go AFTER `Remesh` (bincode tags by index).
    Remesh,
    /// BEND (Blender Simple Deform "Bend"): wrap the mesh around an arc. A pure vertex-position
    /// warp — no topology change, so every stable id and every authored sidecar layer survives it.
    /// Params live on the `bend_*` fields, which are `#[serde(skip)]` + the `FL4PBEND` tail.
    ///
    /// APPEND-ONLY: new variants go AFTER `Bend` (bincode tags by index).
    Bend,
    /// PolySculpt (docs/UI: never "Sculpt" alone — that name is SDF `ModifierType::Sculpt`,
    /// SDF `SculptLayer`, and `SculptBackend`). Mesh-displacement layer after Remesh/Smooth.
    /// Deltas are `#[serde(skip)]` + `FL4SCLP` (S1). APPEND-ONLY: this is the LAST variant.
    Sculpt,
}

/// A single modifier in the poly modifier stack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolyModifier {
    pub mod_type: PolyModifierType,
    pub enabled: bool,
    pub expanded: bool,    // UI: show parameters
    // -- Subdivide --
    pub subdiv_levels: u32,       // 1-4
    // -- Mirror --
    /// Axis bitmask for the Mirror modifier: **X=1, Y=2, Z=4** (NOT index 0/1/2).
    /// Convert via `half_edge::MirrorAxes::from_bits` before calling `HalfEdgeMesh::mirrored`.
    pub mirror_axis: u8,
    pub mirror_merge: bool,       // merge vertices on seam
    pub mirror_merge_threshold: f32,
    /// Optional mirror-plane origin object (`NodeId.0`). 0 = local mesh origin.
    /// World position of this node is transformed into the target body's local space each eval
    /// (Empty / null / any scene object). Append-only; pre-field .fl4 loads as 0.
    #[serde(default)]
    pub mirror_object_nid: u64,
    // -- Solidify --
    pub solidify_thickness: f32,  // offset distance
    pub solidify_offset: f32,     // -1=inward, 0=center, 1=outward
    // -- Smooth --
    pub smooth_iterations: u32,   // 1-20
    pub smooth_factor: f32,       // 0.0-1.0
    // -- Array --
    pub array_count: u32,         // 2-20
    pub array_offset: Vec3,       // offset per instance
    // -- Bevel --
    pub bevel_width: f32,         // edge bevel width
    pub bevel_segments: u32,      // 1-32
    /// Profile shape [0=flat, 0.5=circular fillet (Blender default), 1=convex].
    /// `#[serde(default)]` → 0.5 for pre-field .fl4 loads (P1-1 residual honesty).
    #[serde(default = "default_bevel_shape")]
    pub bevel_shape: f32,
    /// Clamp Overlap (Blender). `#[serde(default)]` → true for pre-field .fl4 loads.
    #[serde(default = "default_bevel_clamp")]
    pub bevel_clamp: bool,
    // -- Decimate --
    pub decimate_angle: f32,      // planar angle threshold (degrees)
    // -- Boolean (append-only, #[serde(default)]) --
    /// 0 = Difference (A − B), 1 = Union, 2 = Intersect. Cutter = `bool_cutter_nid`.
    #[serde(default)]
    pub bool_op: u8,
    /// Scene `NodeId.0` of the cutter poly body. 0 = unset (modifier no-ops).
    #[serde(default)]
    pub bool_cutter_nid: u64,
    // -- Weighted / Smooth normals --
    /// Keep-sharp angle in degrees for WeightedNormal (faces steeper than this don't average).
    /// 180 = fully smooth. Default 30° (Blender-like).
    #[serde(default = "default_weighted_keep_sharp")]
    pub weighted_keep_sharp_deg: f32,
    // -- Circular array (append-only) --
    /// Total angle span in degrees (default 360 = full ring). Copies are evenly spaced.
    #[serde(default = "default_circ_angle")]
    pub circ_angle_deg: f32,
    /// 0=X, 1=Y, 2=Z rotation axis (local).
    #[serde(default = "default_circ_axis")]
    pub circ_axis: u8,
    /// Rotation center in local mesh space.
    #[serde(default)]
    pub circ_center: Vec3,
    /// Optional extra radial push per step (local units along center→centroid).
    #[serde(default)]
    pub circ_radius: f32,
    // -- Pipe (append-only, all #[serde(default)]: a pre-Pipe .fl4 loads with an empty
    //    path, and an empty path makes the modifier a no-op) --
    /// The swept polyline, in local mesh space. RETAINED so radius/segments stay editable
    /// forever (owner-ratified 2026-08-04). Empty = modifier no-ops.
    #[serde(default)]
    pub pipe_path: Vec<Vec3>,
    #[serde(default = "default_pipe_radius")]
    pub pipe_radius: f32,
    #[serde(default = "default_pipe_segments")]
    pub pipe_segments: u32,
    /// The path loops back onto its start (last ring welds to the first).
    #[serde(default)]
    pub pipe_closed: bool,
    /// Close an OPEN pipe's two ends with n-gons.
    #[serde(default = "default_pipe_caps")]
    pub pipe_caps: bool,
    /// Bevel SMART SELECT (owner 2026-08-04): skip edges that should not be beveled.
    /// Rule 2 = the tessellation of a subtracted cylinder, identified EXACTLY from boolean
    /// provenance (never guessed). Rule 3 (coplanar) is deliberately NOT an exclusion --
    /// see BEVEL.md section 6: excluding a coplanar artifact edge drives the surrounding
    /// rails into it and produces folds; such an edge must be DISSOLVED instead.
    /// Off by default, so no existing bevel changes behaviour on load.
    #[serde(default)]
    pub bevel_smart_select: bool,
    // -- Weighted Normal (Blender parity) --
    /// 0 = Corner Angle, 1 = Face Area, 2 = Face Area & Angle (Blender's three modes).
    #[serde(default = "default_wn_mode")]
    pub weighted_mode: u8,
    /// 1..=100, Blender default 50. Biases how strongly the dominant face wins; 50 is
    /// bit-exact against the pre-parity behaviour.
    #[serde(default = "default_wn_weight")]
    pub weighted_weight: u32,
    /// Faces within `max * (1 - threshold)` count as tied and blend, instead of an arbitrary
    /// first-max pick. Blender default 0.01.
    #[serde(default = "default_wn_threshold")]
    pub weighted_threshold: f32,
    // -- Bevel LIMIT METHOD: ANGLE (Blender parity, append-only #[serde(default)]) --
    /// Blender's Bevel modifier "Limit Method: Angle". When on, an edge is beveled only if
    /// its two faces deviate by at least [`Self::bevel_angle_deg`].
    ///
    /// This is a PURE FILTER and is applied LAST, after Smart Select's geometry rule, its
    /// provenance tags, and the near-90-degree force-keep. Last means it cannot be overridden:
    /// a later rule that re-inserts edges (as the force-keep does) would otherwise silently
    /// re-add edges the user's angle limit excluded. Being subtractive-only, running last can
    /// never resurrect an edge another rule removed.
    ///
    /// Off by default, so no existing bevel changes behaviour on load.
    #[serde(default)]
    pub bevel_limit_angle: bool,
    /// Angle between the two face NORMALS, in degrees (0 = coplanar, 90 = square corner) —
    /// the same convention as `edge_dihedral_cos`. Blender's default is 30.
    #[serde(default = "default_bevel_angle")]
    pub bevel_angle_deg: f32,
    // -- Scatter (append-only, every field #[serde(default)]) --
    //    A pre-Scatter .fl4 deserialises to source_nid = 0, which is exactly the no-op state.
    /// Scene `NodeId.0` of the poly body to instance. 0 = unset (modifier no-ops).
    ///
    /// A raw `u64` rather than `NodeId`, matching `bool_cutter_nid` — the ONE established way
    /// this codebase stores a cross-node reference in serialized data. It is a WEAK reference:
    /// nothing keeps the target alive, so every consumer must re-resolve it and treat a
    /// missing node (deleted, or a node that is not a poly body) as "no source" — never a
    /// panic. Self-reference (`== the owning body`) is likewise rejected at resolve time,
    /// which is what makes recursion impossible.
    #[serde(default)]
    pub scatter_source_nid: u64,
    /// Point source: 0 = vertices, 1 = face centres, 2 = area-weighted random points
    /// (`core_math::scatter::SCATTER_SRC_*`).
    #[serde(default)]
    pub scatter_source: u8,
    /// Instance count for the random-points source.
    #[serde(default = "default_scatter_count")]
    pub scatter_count: u32,
    /// Shared seed for every random channel — same seed, same scatter, forever.
    #[serde(default)]
    pub scatter_seed: u32,
    /// Rotate each instance so its local +Y follows the surface normal.
    #[serde(default = "default_scatter_align")]
    pub scatter_align_normal: bool,
    /// Random spin about the normal, degrees (drawn from 0..this).
    #[serde(default)]
    pub scatter_spin_deg: f32,
    /// Random rotation jitter per axis, degrees (drawn from -v..v).
    #[serde(default)]
    pub scatter_rot_jitter_deg: Vec3,
    /// Random positional offset per axis, local units (drawn from -v..v).
    #[serde(default)]
    pub scatter_pos_jitter: Vec3,
    /// Uniform scale range.
    #[serde(default = "default_scatter_scale")]
    pub scatter_scale_min: f32,
    #[serde(default = "default_scatter_scale")]
    pub scatter_scale_max: f32,
    /// Per-axis scale range, multiplied on top of the uniform scale.
    #[serde(default = "default_scatter_scale_axis")]
    pub scatter_scale_axis_min: Vec3,
    #[serde(default = "default_scatter_scale_axis")]
    pub scatter_scale_axis_max: Vec3,
    /// Geometry channel — Modo-style filter for which faces/edges this modifier affects.
    /// Default = All (no filter). `#[serde(default)]` so older .fl4 stacks load unchanged.
    #[serde(default)]
    pub geo_channel: crate::geometry_channel::GeometryChannel,
    // -- Inset (append-only) --
    /// Inset thickness (positive = inward). Same units as interactive Inset.
    #[serde(default = "default_inset_thickness")]
    pub inset_thickness: f32,
    /// Extra extrude along face normal after inset (Blender Depth).
    #[serde(default)]
    pub inset_depth: f32,
    /// true = Individual (per-face); false = Region / group faces (Blender default).
    #[serde(default)]
    pub inset_individual: bool,
    // -- Bevel CONSUME GEOMETRY --
    /// CAD-style fillet growth: when the requested width exceeds the distance to the next
    /// parallel edge loop, dissolve that loop (in the modifier's working copy only) so the
    /// clamp stops binding there — instead of the bevel sticking at the first ring.
    /// Quad-grid rows only; anything else fails closed to the ordinary clamped bevel.
    ///
    /// `#[serde(skip)]` — NOT in the bincode blob (the field trap: bincode is positional, an
    /// appended field breaks every existing .fl4 — proven by the serde_golden gate). It is
    /// persisted via the `FL4PMODF` tail section (`app/src/project/io.rs`), reattached by
    /// (node_id, modifier index) on load. Off by default → old files load unchanged.
    #[serde(skip)]
    pub bevel_consume: bool,
    // -- Vertex Merge / Mesh Cleanup (append-only, #[serde(default)]) --
    /// Merge-by-distance threshold for [`PolyModifierType::VertexMerge`]. Cluster survivors
    /// are placed at the **average** of the welded positions (Blender Merge by Distance).
    #[serde(default = "default_merge_distance")]
    pub merge_distance: f32,
    /// Mesh Cleanup: flatten zero-thickness coplanar flaps.
    #[serde(default = "default_cleanup_true")]
    pub cleanup_flatten_folds: bool,
    /// Mesh Cleanup: merge coplanar faces back into n-gons.
    #[serde(default = "default_cleanup_true")]
    pub cleanup_merge_coplanar: bool,
    /// Mesh Cleanup: Limited Dissolve angle in degrees (0 = off). Default 4.5°.
    #[serde(default = "default_cleanup_dissolve_angle")]
    pub cleanup_dissolve_angle: f32,
    // -- Voxel Remesh --
    /// Voxel grid resolution along the longest AABB axis (clamped 8..=128 at apply time).
    /// Higher = finer remesh, slower eval. Default 32 balances quality vs stack cost.
    ///
    /// These three arrived `#[serde(default)]` from a branch that forked before the field
    /// trap was documented. bincode is POSITIONAL — `#[serde(default)]` does not rescue a
    /// missing trailing field, so shipping them in the blob would have broken every existing
    /// `.fl4` carrying a poly modifier (the serde_golden gate catches exactly this).
    ///
    /// They ride the **`FL4PRMSH`** tail section (`PolyRemeshRecord`), reattached by
    /// (node_id, stack index) on load. *(Corrected 2026-08-09: this comment used to say
    /// "`FL4PMODF`, same as `bevel_consume`" — that carrier was never written. `PolyModFlagRecord`
    /// is only `{ node_id, mod_index, bevel_consume }`, so all four remesh params silently reset
    /// to their defaults on every save→load. Tracked as `REMESHPARAMSAVE`.)*
    #[serde(skip, default = "default_remesh_resolution")]
    pub remesh_resolution: u32,
    /// When true, Surface Nets runs Laplacian relaxation + wide-stencil normals (sculpt
    /// create/release path). When false, cheap extract only (faster scrub). See above re `skip`.
    #[serde(skip, default = "default_remesh_smooth")]
    pub remesh_smooth: bool,
    /// Extra AABB padding as a fraction of half-extent (default 0.1 = 10%). Keeps the
    /// sealed grid border clear of solid so Surface Nets can cap the surface.
    #[serde(skip, default = "default_remesh_padding")]
    pub remesh_padding: f32,
    /// Post-extract SURFACE REPROJECTION strength, 0..1. After the voxel extract, each vertex is
    /// snapped toward its closest point on the modifier's INPUT surface (bounded to ~1 cell of
    /// travel) and blended by this factor: 0 = raw voxel output, 1 = fully on the source surface.
    ///
    /// This is the detail lever, not resolution — the transient remesh grid is a dense `Vec<f32>`
    /// (512³ ≈ 537 MB per evaluation; slider commits that on release). Reprojection undoes
    /// lattice quantisation at lower res so you do not always pay 8× the memory to halve it.
    ///
    /// `#[serde(skip)]` for the same reason as its three siblings above (bincode is positional; a
    /// trailing `#[serde(default)]` field would break every existing `.fl4` carrying a poly
    /// modifier).
    #[serde(skip, default = "default_remesh_project")]
    pub remesh_project: f32,
    /// Parent-first 2³ QEF collapse on extract. Off = uniform lattice (bit-identical to
    /// pre-collapse remesh). `#[serde(skip)]` — rides `FL4PRMCL`, not the blob and not
    /// `FL4PRMSH` (widening that record would fail-load older remesh saves).
    #[serde(skip, default = "default_remesh_collapse")]
    pub remesh_collapse: bool,
    /// Skip Laplacian relax on extract while a grab is live. Mouse-up still uses
    /// `remesh_smooth`. `#[serde(skip)]` — rides `FL4PRMRL`, not the blob.
    #[serde(skip, default = "default_remesh_skip_relax_drag")]
    pub remesh_skip_relax_drag: bool,
    /// Skip whole-field enclosed-air seal while a grab is live. Mouse-up still seals
    /// if a drag deferred it. `#[serde(skip)]` — rides `FL4PRMSL`, not the blob.
    #[serde(skip, default = "default_remesh_skip_seal_drag")]
    pub remesh_skip_seal_drag: bool,
    /// During any live edit (grab / bevel / extrude / …), remesh at
    /// `REMESH_LIVE_SCRUB_MAX` (128). Mouse-up remeshes at `remesh_resolution`.
    /// `#[serde(skip)]` — rides `FL4PRMDR`, not the blob.
    #[serde(skip, default = "default_remesh_drop_res_drag")]
    pub remesh_drop_res_drag: bool,
    /// When true, Boolean-union (or soup-merge) every mesh island into one body **before**
    /// voxel remesh. Overlapping / touching shells fuse so the remesh is one solid instead of
    /// separate components. Default off — pre-feature behaviour.
    ///
    /// `#[serde(skip)]` — same field trap as its remesh siblings; rides `FL4PRMSH` (with dual
    /// load for older four-field records).
    #[serde(skip, default)]
    pub remesh_union_islands: bool,
    // -- Smooth masking --
    /// Smooth: how many 1-ring dilation steps the `geo_channel` mask is feathered over before it
    /// reaches zero. `0` = hard binary mask (visible ridge on the channel boundary — the whole
    /// reason this control exists); larger values fade the smoothing into the untouched geometry.
    /// Ignored when `geo_channel` is `All`.
    ///
    /// `#[serde(skip)]` — same field trap as `bevel_consume` / the `remesh_*` group above (bincode
    /// is positional). Persisted via the `FL4PSMTH` tail section, reattached by
    /// (node_id, modifier index) on load.
    #[serde(skip, default = "default_smooth_falloff")]
    pub smooth_falloff: u32,
    // -- Bevel VARIABLE WIDTH --
    /// Bevel: `0` = off (ONE width everywhere), `1` = auto by local size, `2` = authored edge
    /// weight. Maps to `core_math::bevel_op::BevelWidthDriver` when non-zero.
    ///
    /// One width over a selection that mixes scales is wrong in both clamp states: ON, the single
    /// smallest edge drags every other edge down to its own limit; OFF, the small detail folds.
    /// Non-zero gives every edge its own width inside `[bevel_width_min, bevel_width]`.
    ///
    /// `#[serde(skip)]` — same field trap as `smooth_falloff` / `bevel_consume` / the `remesh_*`
    /// group above (bincode is positional, so an in-blob field would break every existing `.fl4`
    /// carrying a poly modifier). Rides the **`FL4PBVAR`** tail section (`PolyBevelVarRecord`),
    /// reattached by (node_id, stack index) on load. `0` by default → existing bevels unchanged.
    #[serde(skip)]
    pub bevel_var_width: u8,
    /// SMALL end of the variable-width band (`bevel_width` is the large end). Inert while
    /// `bevel_var_width == 0`. `0.0` = purely proportional. See `bevel_var_width` re `skip`.
    #[serde(skip)]
    pub bevel_width_min: f32,
    /// Bevel AUTO MERGE VERTICES: keep ONE width, but apply Clamp Overlap's limit **per edge**
    /// instead of as one global minimum. A pair of edges beveled too near each other then pins at
    /// its meet point — where the kernel's `WeldMap` merges the two rails into one vertex — while
    /// every other edge still reaches `bevel_width`.
    ///
    /// This is NOT a merge by distance: nothing scans the mesh for nearby vertices, so no small
    /// feature can be swallowed by proximity. The only vertices that can coincide are rail points
    /// this bevel just computed, placed at a position derived from the clamp geometry.
    ///
    /// Requires `bevel_clamp` (with no limit there is no meet position). Implied by
    /// `bevel_var_width != 0`, which needs the per-edge ceiling to grade anything at all.
    ///
    /// `#[serde(skip)]`, rides `FL4PBVAR` with the two fields above. `false` by default — a default
    /// of `true` would silently change every existing scene on load, the mistake `bevel_limit_angle`
    /// above records.
    #[serde(skip)]
    pub bevel_auto_merge: bool,
    // -- Bend (Blender Simple Deform "Bend") --
    //
    // All six are `#[serde(skip)]` and ride the **`FL4PBEND`** tail section (`PolyBendRecord`),
    // reattached by (node_id, stack index) on load — the same field trap as `smooth_falloff` /
    // `bevel_consume` / the `remesh_*` and `bevel_var_*` groups above. bincode is POSITIONAL, so an
    // in-blob field (even `#[serde(default)]`) would break every existing `.fl4` carrying a poly
    // modifier; the `serde_golden` gate catches exactly that.
    //
    // Each `default = ` fn is SHARED with the `Default` impl below, and it has to stay that way:
    // `collect_poly_bend` only writes a record when a value differs from `PolyModifier::default()`,
    // so a serde default that disagreed would resurrect the wrong value for every modifier whose
    // params happen to sit exactly on the default.
    /// Total bend angle in degrees, spread across the limit band. Positive curls toward the
    /// direction given by [`Self::bend_dir_deg`]. `0` = no-op.
    #[serde(skip, default = "default_bend_angle")]
    pub bend_angle_deg: f32,
    /// The axis the geometry runs ALONG — the one whose coordinate drives the bend angle.
    /// 0=X, 1=Y, 2=Z (local mesh space). Blender's Simple Deform "Axis", same X default.
    #[serde(skip)]
    pub bend_axis: u8,
    /// Which way the arc curls, as a rotation in degrees about [`Self::bend_axis`], measured from
    /// the next axis in the cyclic order X→Y→Z (so `bend_axis = X` curls toward +Y at 0°, +Z at 90°).
    ///
    /// A continuous heading rather than a second axis dropdown, matching 3ds Max's Bend "Direction":
    /// the curl plane is genuinely continuous, and a 4-way picker would make the 87° bend the user
    /// actually wants unreachable.
    #[serde(skip)]
    pub bend_dir_deg: f32,
    /// Bend band, NORMALIZED 0..1 over the mesh's own extent along [`Self::bend_axis`] (Blender's
    /// convention). Geometry outside the band rides off rigidly along the arc's end tangent, so
    /// `0..1` bends the whole mesh and `0..0.5` bends the first half and leaves the rest straight.
    #[serde(skip)]
    pub bend_limit_lo: f32,
    #[serde(skip, default = "default_bend_limit_hi")]
    pub bend_limit_hi: f32,
    /// The bend's FIXED POINT, in local mesh space — the one place the deform leaves untouched.
    /// `Vec3::ZERO` (the body origin) is Blender's behaviour and the default; a mesh sitting wholly
    /// off to one side of it swings around it rather than bending in place, which is what this
    /// control exists to fix.
    #[serde(skip)]
    pub bend_center: Vec3,
    /// PolySculpt stored vertex deltas `(VertexId.0, delta)`. Runtime in S0a; `FL4SCLP` in S1.
    /// `#[serde(skip)]` — same positional-bincode trap as `bend_*` / remesh flags.
    #[serde(skip)]
    pub sculpt_deltas: Vec<(u32, Vec3)>,
    /// Bumped on every delta edit so the prefix/display hash cannot serve a stale sculpt slot.
    #[serde(skip)]
    pub sculpt_delta_gen: u64,
}

fn default_bevel_shape() -> f32 { 0.5 }
fn default_bevel_clamp() -> bool { true }
fn default_weighted_keep_sharp() -> f32 { 30.0 }
fn default_circ_angle() -> f32 { 360.0 }
fn default_circ_axis() -> u8 { 2 } // Z
fn default_pipe_radius() -> f32 { 0.1 }
fn default_pipe_segments() -> u32 { 12 }
fn default_pipe_caps() -> bool { true }
fn default_wn_mode() -> u8 { 2 }
fn default_wn_weight() -> u32 { 50 }
fn default_wn_threshold() -> f32 { 0.01 }
fn default_bevel_angle() -> f32 { 30.0 } // Blender's Bevel "Angle" limit default
fn default_scatter_count() -> u32 { 50 }
fn default_scatter_align() -> bool { true }
fn default_scatter_scale() -> f32 { 1.0 }
fn default_scatter_scale_axis() -> Vec3 { Vec3::ONE }
fn default_inset_thickness() -> f32 { 0.05 }
fn default_merge_distance() -> f32 { 0.001 }
fn default_cleanup_true() -> bool { true }
fn default_cleanup_dissolve_angle() -> f32 { 4.5 }
fn default_remesh_resolution() -> u32 { 32 }
fn default_remesh_smooth() -> bool { true }
fn default_remesh_padding() -> f32 { 0.1 }
/// Reprojection ON by default: the raw voxel output's terracing is the thing this exists to fix,
/// and a default of 0 would ship the fix switched off.
fn default_remesh_project() -> f32 { 1.0 }
fn default_remesh_collapse() -> bool { false }
fn default_remesh_skip_relax_drag() -> bool { false }
fn default_remesh_skip_seal_drag() -> bool { false }
fn default_remesh_drop_res_drag() -> bool { false }
/// Two rings of feather by default: enough that a masked Smooth reads as a soft falloff rather
/// than a stamped-out patch, without reaching so far that the mask stops meaning anything.
fn default_smooth_falloff() -> u32 { 2 }
/// Blender's Simple Deform angle default. A NON-zero default on purpose: adding the modifier should
/// visibly do something, and unlike a width or a thickness there is no scene-dependent scale to get
/// wrong — 45° reads the same on a 1cm bracket and a 10m beam.
fn default_bend_angle() -> f32 { 45.0 }
/// Upper limit at 1.0 = the whole mesh bends. Together with `bend_limit_lo`'s `0.0` this is the
/// full-band default; the pair only ever narrows the deform, never widens it.
fn default_bend_limit_hi() -> f32 { 1.0 }

impl Default for PolyModifier {
    fn default() -> Self {
        Self {
            mod_type: PolyModifierType::Subdivide,
            enabled: true,
            expanded: true,
            subdiv_levels: 1,
            // Default X bit (1). Under bitmask encoding, 0 = no axes = identity — never default to that.
            mirror_axis: 1, mirror_merge: true, mirror_merge_threshold: 0.001,
            mirror_object_nid: 0,
            solidify_thickness: 0.05, solidify_offset: -1.0,
            smooth_iterations: 3, smooth_factor: 0.5,
            array_count: 2, array_offset: Vec3::new(2.0, 0.0, 0.0),
            bevel_width: 0.02, bevel_segments: 1,
            bevel_shape: 0.5, bevel_clamp: true,
            decimate_angle: 5.0,
            bool_op: 0,
            bool_cutter_nid: 0,
            weighted_keep_sharp_deg: 30.0,
            circ_angle_deg: 360.0,
            circ_axis: 2,
            circ_center: Vec3::ZERO,
            circ_radius: 0.0,
            pipe_path: Vec::new(),
            pipe_radius: 0.1,
            pipe_segments: 12,
            pipe_closed: false,
            pipe_caps: true,
            bevel_smart_select: false,
            weighted_mode: 2,
            weighted_weight: 50,
            weighted_threshold: 0.01,
            // REVERTED 2026-08-07 (owner: "bevel was working perfection before",
            // BEVEL REGRSSION.fl4). Defaulting this to TRUE was a same-day speculative
            // change made while misreading "it triangulates the mesh" as an every-edge
            // bevel — that complaint was really the WIRE (a POLYGON_MODE_LINE pass over
            // triangles, fixed in raster_gbuffer). Turning the 30° limit on by default
            // silently stopped bevelling every edge softer than 30°, which on a dense or
            // curved body is most of them — a behaviour change the owner never asked for
            // and immediately felt as a regression. Blender parity is NOT a licence to
            // change a working default underneath the user.
            bevel_limit_angle: false,
            bevel_angle_deg: 30.0,
            scatter_source_nid: 0,
            scatter_source: 0,
            scatter_count: 50,
            scatter_seed: 0,
            scatter_align_normal: true,
            scatter_spin_deg: 0.0,
            scatter_rot_jitter_deg: Vec3::ZERO,
            scatter_pos_jitter: Vec3::ZERO,
            scatter_scale_min: 1.0,
            scatter_scale_max: 1.0,
            scatter_scale_axis_min: Vec3::ONE,
            scatter_scale_axis_max: Vec3::ONE,
            geo_channel: crate::geometry_channel::GeometryChannel::default(),
            inset_thickness: 0.05,
            inset_depth: 0.0,
            inset_individual: false,
            bevel_consume: false,
            merge_distance: 0.001,
            cleanup_flatten_folds: true,
            cleanup_merge_coplanar: true,
            cleanup_dissolve_angle: 4.5,
            remesh_resolution: 32,
            remesh_smooth: true,
            remesh_padding: 0.1,
            remesh_project: 1.0,
            remesh_collapse: false,
            remesh_skip_relax_drag: false,
            remesh_skip_seal_drag: false,
            remesh_drop_res_drag: false,
            remesh_union_islands: false,
            smooth_falloff: 2,
            bevel_var_width: 0,
            bevel_width_min: 0.0,
            bevel_auto_merge: false,
            // Must stay in lockstep with the `default_bend_*` fns above — `collect_poly_bend`
            // compares against THIS impl to decide what is worth persisting.
            bend_angle_deg: 45.0,
            bend_axis: 0,
            bend_dir_deg: 0.0,
            bend_limit_lo: 0.0,
            bend_limit_hi: 1.0,
            bend_center: Vec3::ZERO,
            sculpt_deltas: Vec::new(),
            sculpt_delta_gen: 0,
        }
    }
}

impl PolyModifier {
    /// Does evaluating this modifier READ the authored per-edge weight column
    /// (`CustomData.crease`, what `W` writes)?
    ///
    /// **Two independent ways to be a consumer, and that is the whole point of this function.**
    /// The obvious one is a geometry-channel MASK of kind `EdgeWeight`. The other is a Bevel
    /// whose Variable Width is driven by authored weight (`bevel_var_width == 2`), which reads
    /// the same column with its channel left at `All` — so any predicate written in terms of
    /// the channel alone silently misses it.
    ///
    /// It lives here, on the modifier, because the answer was previously spelled out at each
    /// call site and they disagreed: the display cache KEY folded in both consumers, while the
    /// code deciding whether to re-evaluate the display at all (`poly_write_edge_weights`)
    /// tested only the channel. Authoring a weight therefore invalidated a cache entry that
    /// nothing ever asked to rebuild — Variable Width by weight simply never updated on screen
    /// (owner 2026-08-11: "the variable width with weight (W) does not work"). Anyone adding a
    /// third consumer — Catmull-Clark creasing is the expected one — extends THIS, and both
    /// sites follow.
    pub fn reads_edge_weight(&self) -> bool {
        self.enabled
            && (self.geo_channel.kind == crate::geometry_channel::GeoChannelKind::EdgeWeight
                // EVERY Bevel, not just one mode: the authored weight is a percentage of the
                // width and is always live, so any Bevel is a consumer of the column.
                || self.mod_type == PolyModifierType::Bevel)
    }

    /// Re-express this modifier's LOCAL-SPACE authored state for a different body.
    ///
    /// A modifier stack is evaluated in its owner body's local mesh space, so copying a
    /// modifier from body A to body B must rebase every local-space field — otherwise a
    /// Pipe authored around A lands wherever B's origin happens to be (the tube "teleports").
    ///
    /// Field policy (the only place this decision lives):
    /// * `pipe_path`, `circ_center` — **points** in local mesh space → full affine rebase.
    /// * `array_offset` — a **direction/displacement** → rotation+scale only, no translation.
    /// * `bool_cutter_nid` — a scene reference, deliberately **kept verbatim**: the pasted
    ///   Boolean cuts against the SAME cutter object, which is what "paste this boolean onto
    ///   that object too" means. It is not local-space state and must not be rebased.
    /// * `mirror_object_nid` — scene reference (mirror plane origin), **kept verbatim**.
    /// * `scatter_source_nid` — a scene reference, **kept verbatim** for the same reason as
    ///   `bool_cutter_nid`: pasting a Scatter onto another mesh scatters the SAME object.
    ///   `scatter_pos_jitter` is a per-axis MAGNITUDE, not a point, so it is copied as-is too
    ///   (rebasing it would rotate a jitter box, which means nothing).
    /// * every scalar (widths, counts, angles, thresholds) is unit-less w.r.t. body placement
    ///   and is copied as-is.
    ///
    /// `src_world` / `dst_world` are the two bodies' world transforms. A non-invertible
    /// `dst_world` (degenerate scale) falls back to identity rebase rather than producing NaNs.
    pub fn rebased_to(&self, src_world: Mat4, dst_world: Mat4) -> Self {
        let mut out = self.clone();
        let dst_inv = dst_world.inverse();
        if !dst_inv.is_finite() {
            return out;
        }
        let m = dst_inv * src_world;
        if !m.is_finite() {
            return out;
        }
        for p in out.pipe_path.iter_mut() {
            *p = m.transform_point3(*p);
        }
        out.circ_center = m.transform_point3(out.circ_center);
        out.array_offset = m.transform_vector3(out.array_offset);
        // Bend's fixed point is a local-space POSITION, exactly like `circ_center`. Left unrebased,
        // pasting a Bend onto a body with a different origin would silently move the pivot and the
        // pasted modifier would not reproduce what was copied. `bend_axis` / `bend_dir_deg` are
        // deliberately NOT rebased: they name the target body's own local axes, not the source's.
        out.bend_center = m.transform_point3(out.bend_center);
        out
    }

    pub fn new(mod_type: PolyModifierType) -> Self {
        let mut m = Self { mod_type, ..Default::default() };
        // Sensible defaults per type
        match mod_type {
            PolyModifierType::Duplicate => {
                m.array_count = 1; // 1 extra copy
                m.array_offset = Vec3::new(1.5, 0.0, 0.0);
            }
            PolyModifierType::ArrayCircular => {
                m.array_count = 6;
                m.circ_angle_deg = 360.0;
                m.circ_axis = 2;
            }
            PolyModifierType::Array => {
                m.array_count = 3;
            }
            PolyModifierType::Pipe => {
                m.pipe_radius = 0.1;
                m.pipe_segments = 12;
                m.pipe_caps = true;
            }
            PolyModifierType::Inset => {
                m.inset_thickness = 0.05;
                m.inset_depth = 0.0;
                m.inset_individual = false;
            }
            PolyModifierType::VertexMerge => {
                m.merge_distance = 0.001;
            }
            PolyModifierType::MeshCleanup => {
                m.cleanup_flatten_folds = true;
                m.cleanup_merge_coplanar = true;
                m.cleanup_dissolve_angle = 4.5;
            }
            PolyModifierType::Remesh => {
                m.remesh_resolution = 32;
                m.remesh_smooth = true;
                m.remesh_padding = 0.1;
                m.remesh_project = 1.0;
                m.remesh_collapse = true;
                m.remesh_skip_relax_drag = true;
                m.remesh_skip_seal_drag = true;
                m.remesh_drop_res_drag = true;
                m.remesh_union_islands = false;
            }
            // Smooth Normal: full soft shading by default. Shares weighted_* + smooth_factor
            // (Strength) — no new blob fields (bincode append trap).
            PolyModifierType::SmoothNormal => {
                m.weighted_keep_sharp_deg = 180.0;
                m.weighted_mode = 2; // Face Area & Angle
                m.weighted_weight = 50;
                m.weighted_threshold = 0.01;
                m.smooth_factor = 1.0; // strength amount
            }
            PolyModifierType::WeightedNormal => {
                m.weighted_keep_sharp_deg = 30.0;
                m.weighted_mode = 2;
                m.weighted_weight = 50;
                m.weighted_threshold = 0.01;
            }
            _ => {}
        }
        m
    }

    pub fn label(&self) -> &'static str {
        match self.mod_type {
            PolyModifierType::Subdivide => "Subdivide",
            PolyModifierType::Mirror => "Mirror",
            PolyModifierType::Solidify => "Solidify",
            PolyModifierType::Smooth => "Smooth (Laplacian)",
            PolyModifierType::Triangulate => "Triangulate",
            PolyModifierType::Decimate => "Decimate",
            PolyModifierType::Array => "Array (Linear)",
            PolyModifierType::Bevel => "Bevel",
            PolyModifierType::Boolean => "Boolean",
            PolyModifierType::WeightedNormal => "Weighted Normal",
            PolyModifierType::SmoothNormal => "Smooth Normal",
            PolyModifierType::Duplicate => "Duplicate",
            PolyModifierType::ArrayCircular => "Array (Circular)",
            PolyModifierType::Pipe => "Pipe",
            PolyModifierType::Scatter => "Scatter",
            PolyModifierType::Inset => "Inset",
            PolyModifierType::VertexMerge => "Vertex Merge",
            PolyModifierType::MeshCleanup => "Mesh Cleanup",
            PolyModifierType::Remesh => "Remesh (Voxel)",
            PolyModifierType::Bend => "Bend",
            PolyModifierType::Sculpt => "Sculpt",
        }
    }
}

// ============================================================
// Pivot Configuration (shared by all node types)
// ============================================================

/// How the gizmo pivot point is determined for rotation/scale transforms.
/// Lives on SceneNode — works for SdfBody, Group, Poly, NURBS, Empty, etc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PivotMode {
    /// Pivot at the node's bounding box center (default behavior).
    #[default]
    BoundingCenter,
    /// Pivot at a user-defined position (D-key / face-selected). Stored in
    /// node-local space when `PivotConfig::local` is true so it follows the object.
    Custom,
    /// Pivot at world origin (0, 0, 0).
    WorldOrigin,
}

/// User-configurable pivot point for gizmo transforms.
/// Applies to ANY scene node — body, group, poly, nurbs, empty.
/// The D-key pie menu sets these values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PivotConfig {
    /// Custom pivot position. Local when `local` is true; world-space in old files.
    pub position: Vec3,
    /// How the pivot is determined.
    pub mode: PivotMode,
    /// If true, custom pivot persists across transforms.
    /// If false, resets to BoundingCenter after one G/R/S operation.
    pub persistent: bool,
    /// When true, `position` is in the node's local space and follows G/R/S.
    ///
    /// `#[serde(skip)]`, NOT `#[serde(default)]`: this struct rides the positional
    /// bincode blob, where `default` NEVER fires on read — the decoder consumed the
    /// next byte as this bool and shifted every later field ("invalid u8 while
    /// decoding bool, found 2"; every pre-2026-08-18 .fl4 failed to load). The flag
    /// persists via the `FL4PIVT` tail (project/io.rs); skip-default `false` means
    /// "unmigrated" and `migrate_custom_pivots_to_local` converts, which is exactly
    /// the old-file semantic.
    #[serde(skip)]
    pub local: bool,
}

impl Default for PivotConfig {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            mode: PivotMode::BoundingCenter,
            persistent: true,
            local: true,
        }
    }
}

// ============================================================
// Scene Node
// ============================================================

/// A node in the scene graph tree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub payload: NodePayload,
    /// Local transform relative to parent.
    pub local_position: Vec3,
    pub local_rotation: Vec3, // euler degrees (YXZ), matching SdfPrimitive convention
    pub local_scale: Vec3,
    pub visible: bool,
    pub locked: bool,
    /// Pivot point configuration for gizmo rotation/scale.
    /// Applies to all node types. Set via D-key pie menu.
    #[serde(default)]
    pub pivot: PivotConfig,
}

impl SceneNode {
    /// Construct a node (parent module `SceneGraph::new` / `add_child`).
    pub(super) fn new(id: NodeId, payload: NodePayload) -> Self {
        Self {
            id,
            parent: None,
            children: Vec::new(),
            payload,
            local_position: Vec3::ZERO,
            local_rotation: Vec3::ZERO,
            local_scale: Vec3::ONE,
            visible: true,
            locked: false,
            pivot: PivotConfig::default(),
        }
    }

    /// Build the local transform matrix from position/rotation/scale.
    pub fn local_transform(&self) -> Mat4 {
        let rot = Quat::from_euler(
            glam::EulerRot::YXZ,
            self.local_rotation.y.to_radians(),
            self.local_rotation.x.to_radians(),
            self.local_rotation.z.to_radians(),
        );
        Mat4::from_scale_rotation_translation(self.local_scale, rot, self.local_position)
    }
}
