// core_engine/core_math/src/primitives.rs
// SdfPrimitive + BooleanOp + re-exports from split modules
use glam::{Vec3, Mat3, Quat, EulerRot};
use serde::{Serialize, Deserialize};

// ============================================================
// Re-exports for backward compatibility
// All existing `use core_math::primitives::X` statements continue to work.
// ============================================================
pub use crate::sdf_shapes::{
    SdfShape, BevelMode, BoxBevel, BoxBevelAlgo, CylinderBevel, spine_pipe_preset,
};
pub use crate::pbr_material::{PbrMaterial, MaterialId, MaterialLayer, NoiseLayer, ShaderType, MaterialQuality, TexMap, MapKind, MapProjection};
pub use crate::modifiers::{
    Modifier, ModifierType, BlendProfile, BlendCurve, BlendCurveKnot, MaskShape, VolumeMask,
    BlendFalloff, FalloffAxis, SculptOp, smoothstep_f32, DisplaceHeightMap,
};
// pub(crate) helpers used by SdfPrimitive::world_distance
use crate::modifiers::{apply_twist, apply_taper, apply_shear, apply_sculpt, sdf_extruded_circle};
pub use crate::material_dna::{MaterialDna, DnaRule, DnaCondition, DnaInput, DnaSurface, DnaBlendMode};
pub use crate::curves::{
    CurveProfile, CurveVertex, CurvePlane, ClipCurve,
    sdf_polygon_2d, MAX_CURVE_PROFILE_VERTS, MAX_SPINE_POINTS,
};
pub use crate::sdf_math::{
    smin, chamfer_min, smin_cubic, smin_exp, smin_stairs, smin_profiled,
    mask_noise_3d, fbm_noise, eval_scene, eval_normal, scene_sdf, scene_normal,
};

// Legacy aliases for backward compat with existing code
pub type ClipCurveVertex = CurveVertex;
pub type ClipCurvePlane = CurvePlane;

// ============================================================
// Serde defaults for SdfPrimitive fields
// ============================================================
fn default_chamfer_angle() -> f32 { 45.0 }
fn default_blend_noise_freq() -> f32 { 20.0 }
fn default_blend_noise_octaves() -> u32 { 1 }
fn default_blend_profile_shape() -> f32 { 1.0 }

// ============================================================
// Boolean CSG operation
// ============================================================

/// Boolean CSG operation. `k` is the blend radius: 0.0 = hard edge, >0.0 = polynomial smooth-min.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BooleanOp {
    Union    { k: f32 },         // k=0 → hard min;   k>0 → polynomial smin
    Subtract { k: f32 },         // k=0 → hard max;   k>0 → smooth subtract
    Intersect,                   // hard intersection (k=0)
    ChamferUnion    { k: f32 },  // 45° bevel blend
    ChamferSubtract { k: f32 },
    // NOTE: bincode tags variants by position — only ever APPEND below this line (see CLAUDE.md).
    SmoothIntersect { k: f32 },  // k>0 → smooth intersection (smax). Chamfer-intersect = this + Chamfer profile.
    /// SEAM boolean: carves (or raises) a feature line along the curve where this prim's
    /// SURFACE crosses the accumulated surface — panel lines, scribed seams, welds/beads.
    /// The prim's own volume is never added/subtracted; only a groove/bead of the given
    /// cross-section appears within `width` of the prim's surface. Far from the prim the
    /// result is EXACTLY the accumulator (see `seam_cut` in sdf_math.rs for the proof).
    /// `profile`: 0=V (chamfer), 1=U (round), 2=Square (flat bottom) — SEAM_PROFILE_*.
    /// `raised`: false = cut a groove into the surface, true = raise a bead/tongue.
    Seam { width: f32, depth: f32, profile: u8, raised: bool },
    /// EXTRUDE boolean: displaces the accumulated surface within this prim's FOOTPRINT —
    /// `acc' = acc - strength * mask(b)` with `mask(b) = 1 - smoothstep(0, width, b)`
    /// (1 everywhere inside B, fading to 0 at `b >= width` outside). `strength > 0`
    /// raises the surface (emboss/extrude out), `strength < 0` recesses it (deboss/in).
    /// The prim's own volume is never added/subtracted — it only drives the displacement.
    /// Far from the prim (`b >= width`) the result is EXACTLY the accumulator (the
    /// smoothstep clamp saturates — see `extrude_op` in sdf_math.rs for the proof).
    /// Supersedes the pie's old ModifierType::Emboss/Deboss self-stamp (which only
    /// inflated the prim's own field = looked like a scale).
    Extrude { strength: f32, width: f32 },
}

/// Seam cross-section profile ids (BooleanOp::Seam.profile). u8 (not an enum) so the
/// GPU serialization (blend_profile u32 slot) and bincode stay trivially in sync.
pub const SEAM_PROFILE_V: u8 = 0;
pub const SEAM_PROFILE_U: u8 = 1;
pub const SEAM_PROFILE_SQUARE: u8 = 2;

impl BooleanOp {
    /// Returns true for additive ops (Union, ChamferUnion) that produce visible geometry.
    /// Subtract/Intersect/SmoothIntersect/ChamferSubtract are "cutters" — they carve into existing shapes.
    /// Seam is NOT additive: it never contributes its own volume (even `raised` only decorates
    /// the accumulated surface within `width` of the seam prim's surface).
    pub fn is_additive(&self) -> bool {
        matches!(self, BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. })
    }

    /// Coarse CSG category, ignoring the blend radius `k`.
    /// 0 = Additive (Union/ChamferUnion), 1 = Subtractive (Subtract/ChamferSubtract),
    /// 2 = Intersective (Intersect/SmoothIntersect), 3 = Seam (surface decoration),
    /// 4 = Extrude (footprint surface displacement).
    ///
    /// A change of category restructures how the prim participates in the body's
    /// boolean chain and its bake silhouette (`is_additive` decides bounds membership;
    /// a cutter category change re-carves globally), so it requires a FULL volume
    /// re-bake — whereas a pure `k` change (same category) is a local field edit that a
    /// partial region bake handles. This is the discriminator the volume cache uses to
    /// decide full-vs-partial on a boolean-op edit.
    pub fn category(&self) -> u8 {
        match self {
            BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. } => 0,
            BooleanOp::Subtract { .. } | BooleanOp::ChamferSubtract { .. } => 1,
            BooleanOp::Intersect | BooleanOp::SmoothIntersect { .. } => 2,
            // Seam: its own category — a groove carves (carver-like for the interior proof,
            // see BrickClassifier) but never re-carves globally; width/depth/profile/raised
            // edits are local field edits (partial bake), switching to/from Seam is a
            // category change (full bake).
            BooleanOp::Seam { .. } => 3,
            // Extrude: its own category — a recess (strength<0) carves like a subtract
            // (carver for the interior proof, see BrickClassifier), a raise (strength>0)
            // adds material but ONLY within {b < width} ⊆ the prim's influence sphere
            // (bounding_radius pads width + |strength|), so the exterior proof stays sound.
            // strength/width edits are local field edits (partial bake); switching to/from
            // Extrude (incl. Seam↔Extrude) is a category change (full bake).
            BooleanOp::Extrude { .. } => 4,
        }
    }

    /// Returns the blend radius k (0.0 for hard Intersect).
    /// For Seam/Extrude this is the WIDTH — it rides the same GPU k slot and provides the
    /// same bounding-radius padding role (both ops only alter the field within `width`
    /// of the prim's surface).
    pub fn blend_k(&self) -> f32 {
        match self {
            Self::Union { k } | Self::Subtract { k } => *k,
            Self::ChamferUnion { k } | Self::ChamferSubtract { k } => *k, // normalize: deprecated
            Self::SmoothIntersect { k } => *k,
            Self::Intersect => 0.0,
            Self::Seam { width, .. } => *width,
            Self::Extrude { width, .. } => *width,
        }
    }

    /// Sets the blend radius k. Dragging k>0 on a hard Intersect upgrades it to SmoothIntersect.
    /// For Seam/Extrude this sets the width (the k-slot value).
    pub fn set_blend_k(&mut self, new_k: f32) {
        match self {
            Self::Union { k } | Self::Subtract { k } => *k = new_k,
            Self::ChamferUnion { k } | Self::ChamferSubtract { k } => *k = new_k, // normalize: deprecated
            Self::SmoothIntersect { k } => *k = new_k,
            Self::Intersect => { if new_k > 0.0 { *self = BooleanOp::SmoothIntersect { k: new_k }; } }
            Self::Seam { width, .. } => *width = new_k.max(0.001),
            Self::Extrude { width, .. } => *width = new_k.max(0.001),
        }
    }

    /// Normalize legacy ChamferUnion/ChamferSubtract into Union/Subtract + Chamfer profile.
    /// Returns `(base_op, Some(BlendProfile::Chamfer))` for chamfer ops, `(self, None)` otherwise.
    pub fn normalize(&self) -> (BooleanOp, Option<BlendProfile>) {
        match self {
            BooleanOp::ChamferUnion { k } => (BooleanOp::Union { k: *k }, Some(BlendProfile::Chamfer)),
            BooleanOp::ChamferSubtract { k } => (BooleanOp::Subtract { k: *k }, Some(BlendProfile::Chamfer)),
            other => (other.clone(), None),
        }
    }
}

// ============================================================
// SDF Primitive
// ============================================================
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SdfPrimitive {
    pub name: String,
    pub shape: SdfShape,
    pub position: Vec3,
    pub rotation_deg: Vec3,
    pub scale: Vec3,
    pub operation: BooleanOp,
    pub visible: bool,
    pub material: PbrMaterial,
    /// Reference to a shared scene material. NONE = use embedded `material` field (migration default).
    #[serde(default)]
    pub material_id: MaterialId,
    /// Mirror modifier flags: bit 0=X, bit 1=Y, bit 2=Z.
    /// Applies abs() in local space after transform, creating instant symmetry.
    pub mirror_flags: u8,
    /// Position-dependent blend falloff mask for smooth boolean operations.
    pub blend_falloff: BlendFalloff,
    /// Spatial modifier (Twist, Taper, Smooth) with optional volumetric mask.
    pub modifier: Modifier,
    /// Layer membership (None = ungrouped).
    pub layer_id: Option<u32>,
    /// Prevents transform edits when true.
    pub locked: bool,
    /// Unique ID that survives reorder (unlike index).
    pub prim_uid: u32,
    /// If Some(uid), this prim is an instance that syncs shape/material from the master with that uid.
    pub instance_master_id: Option<u32>,
    /// Mirror fold center offset in local scaled space (default ZERO = fold at origin).
    pub mirror_center: Vec3,
    /// Per-primitive texture atlas index (0 = no per-prim texture, 1+ = atlas entry).
    pub texture_id: u32,
    /// Object membership (None = ungrouped, Some(id) = belongs to SceneObject with that id).
    pub object_id: Option<u32>,
    /// SDF surface rounding level (0-6). Higher = smoother edges via distance offset.
    #[serde(default)]
    pub subdivision: u32,
    /// Blend profile for smooth boolean transition shape (Smooth / Chamfer / Circular).
    #[serde(default)]
    pub blend_profile: BlendProfile,
    /// Curve handle for the blend profile (0.25..4). Legacy scalar; also scales K with the curve.
    #[serde(default = "default_blend_profile_shape")]
    pub blend_profile_shape: f32,
    /// Editable multi-point blend profile curve (add/remove/drag). Reset fills a type preset.
    #[serde(default)]
    pub blend_curve: crate::modifiers::BlendCurve,
    /// Generated WGSL from node graph (None = use default material eval).
    #[serde(default)]
    pub node_graph: Option<String>,
    /// Ordered modifier stack (applied sequentially: mirrors first, then domain warps, then surface alters).
    /// When non-empty, this overrides the legacy `modifier` field for CPU evaluation.
    /// GPU serialization syncs from this stack into the legacy single-modifier GPU slots.
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    /// True once legacy modifier/mirror have been migrated into `modifiers` vec.
    /// Prevents re-migration after user deletes all modifiers.
    #[serde(default)]
    pub legacy_migrated: bool,
    /// Material DNA: geometry-responsive material rule stack (replaces legacy edge_wear/cavity_dirt when active).
    #[serde(default)]
    pub material_dna: MaterialDna,
    /// Sculpt layers: accumulative brush stroke delta grids (replaces per-stroke Sculpt modifiers).
    #[serde(default)]
    pub sculpt_layers: Vec<crate::sculpt_layer::SculptLayer>,
    /// Index into sculpt_layers for the active painting target.
    #[serde(default)]
    pub active_sculpt_layer: Option<usize>,
    /// Cylinder edge bevel (only meaningful when shape is Cylinder).
    #[serde(default)]
    pub cyl_bevel: CylinderBevel,
    /// Per-edge-group bevel profile: -1.0 = concave scoop, 0.0 = chamfer (flat 45°), +1.0 = round fillet.
    /// Groups: top (edges 4-7), bottom (edges 0-3), vertical (edges 8-11).
    #[serde(default, alias = "bevel_profile")]
    pub bevel_profile_top: f32,
    #[serde(default)]
    pub bevel_profile_bot: f32,
    #[serde(default)]
    pub bevel_profile_vert: f32,
    /// Per-edge-group chamfer angle in degrees (0-90, default 45).
    /// Controls the tilt of the chamfer cut: 30° = shallow, 45° = standard, 80° = steep.
    #[serde(default = "default_chamfer_angle")]
    pub chamfer_angle_top: f32,
    #[serde(default = "default_chamfer_angle")]
    pub chamfer_angle_bot: f32,
    #[serde(default = "default_chamfer_angle")]
    pub chamfer_angle_vert: f32,
    // Blend-zone noise displacement
    #[serde(default)]
    pub blend_noise_amp: f32,
    #[serde(default = "default_blend_noise_freq")]
    pub blend_noise_freq: f32,
    #[serde(default = "default_blend_noise_octaves")]
    pub blend_noise_octaves: u32,
    #[serde(default)]
    pub blend_noise_type: u32,
    /// SDF elongation: extends shape along each axis without distorting cross-section.
    /// Domain warp: `p.axis -= clamp(p.axis, -h, h)`. Vec3::ZERO = no elongation.
    #[serde(default)]
    pub elongation: Vec3,
    /// Clip curves: stacked boolean cutters using the shared CurveProfile system.
    /// Each ClipCurve is evaluated sequentially — multiple clips on one prim.
    /// GPU serializes only the first valid clip; CPU evaluates all of them.
    #[serde(default)]
    pub clip_curves: Vec<ClipCurve>,
    // Legacy fields — kept for backward compat with old saves.
    #[serde(default)]
    pub clip_curve_enabled: bool,
    #[serde(default)]
    pub clip_curve_radius: f32,
    #[serde(default)]
    pub clip_curve_extrude_axis: u8,
}

impl SdfPrimitive {
    pub fn new(name: String, shape: SdfShape, position: Vec3) -> Self {
        Self::with_uid(name, shape, position, 0)
    }
    pub fn with_uid(name: String, shape: SdfShape, position: Vec3, uid: u32) -> Self {
        Self { name, shape, position, rotation_deg:Vec3::ZERO, scale:Vec3::ONE,
               operation: BooleanOp::Union{k:0.15}, visible:true, material: PbrMaterial::default(),
               material_id: MaterialId::NONE,
               mirror_flags: 0, blend_falloff: BlendFalloff::default(), modifier: Modifier::default(),
               layer_id: None, locked: false, prim_uid: uid, instance_master_id: None,
               mirror_center: Vec3::ZERO, texture_id: 0, object_id: None, subdivision: 0,
               blend_profile: BlendProfile::default(),
               blend_profile_shape: 1.0,
               blend_curve: crate::modifiers::BlendCurve::default(),
               node_graph: None, modifiers: Vec::new(),
               material_dna: MaterialDna::default(),
               sculpt_layers: Vec::new(), active_sculpt_layer: None, legacy_migrated: false,
               cyl_bevel: CylinderBevel::default(),
               bevel_profile_top: 0.0, bevel_profile_bot: 0.0, bevel_profile_vert: 0.0,
               chamfer_angle_top: 45.0, chamfer_angle_bot: 45.0, chamfer_angle_vert: 45.0,
               blend_noise_amp: 0.0, blend_noise_freq: 20.0, blend_noise_octaves: 1, blend_noise_type: 0,
               elongation: Vec3::ZERO,
               clip_curves: Vec::new(),
               clip_curve_enabled: false, clip_curve_radius: 0.0, clip_curve_extrude_axis: 0 }
    }
    pub fn rotation_quat(&self) -> Quat {
        let r = self.rotation_deg * (std::f32::consts::PI / 180.0);
        Quat::from_euler(EulerRot::YXZ, r.y, r.x, r.z)
    }

    /// Default clip-curve radius: 80% of the larger cross-section half-extent in the clip plane.
    /// Must be large enough for the bore to visibly cut through the surface.
    pub fn default_clip_curve_radius(&self) -> f32 {
        match &self.shape {
            SdfShape::Box { half_extents, .. } => {
                let (a, b) = match self.clip_curve_extrude_axis {
                    1 => (half_extents.y, half_extents.z),
                    2 => (half_extents.x, half_extents.y),
                    _ => (half_extents.x, half_extents.z),
                };
                0.8 * a.max(b)
            }
            _ => {
                let he = self.bounding_aabb_half_extents();
                0.8 * he.x.max(he.y).max(he.z)
            }
        }
    }

    /// Apply all clip curves to the SDF distance value at a point in prim-local scaled space.
    /// Each clip is evaluated sequentially as a boolean cutter.
    fn apply_clip_curves(&self, mut d: f32, local_p: Vec3) -> f32 {
        for clip in &self.clip_curves {
            if !clip.is_valid() { continue; }
            let clip_d = clip.profile.sdf_3d(local_p, clip.depth);
            if clip_d >= f32::MAX - 1.0 { continue; }
            if clip.mode == 0 {
                d = d.max(-clip_d);  // Subtract
            } else {
                d = d.max(clip_d);   // KeepInside
            }
        }
        // Legacy circle fallback for old saves
        if self.clip_curve_enabled && self.clip_curve_radius > 0.001 {
            let clip_d = sdf_extruded_circle(local_p, self.clip_curve_radius, self.clip_curve_extrude_axis, 0.0);
            d = d.max(-clip_d);
        }
        d
    }

    /// Conservative WORLD-space padding for everything `world_distance` evaluates AFTER the base
    /// shape: the modifier STACK when non-empty (exactly `world_distance`'s selection — every
    /// UI-added modifier lives in `modifiers`; the legacy single `modifier` + `mirror_flags` only
    /// apply when the stack is empty). ONE shared implementation for `bounding_radius` and
    /// `bounding_aabb_half_extents` so the two bounds can never drift.
    ///
    /// THIS IS A CONTAINMENT CONTRACT, not a heuristic: `eval_scene`'s per-prim cull, the GPU
    /// `bound_r` cull, and the sparse classifier's exterior PROOF (`provably_exterior` /
    /// the classify prefilter) all treat "outside bounding_radius" as "this prim's field cannot
    /// place surface here". Under-padding = dropped bricks = HOLES (the S2.5g soundness bug:
    /// stack modifiers contributed ZERO pad, and Displace had no pad even on the legacy arm).
    ///
    /// Units: surface-alteration strengths shift `d` in scaled-local units and the zero-set's
    /// world position moves by up to `strength × max_scale` (a scaled-local offset `s` maps to a
    /// world offset ≤ s·max_scale) — hence the uniform `× max_scale`. Domain-warp factors
    /// (Twist 0.5 / Taper 0.3 / Shear 0.5) keep their long-standing heuristic ratios, likewise
    /// scaled. A Mirror fold duplicates the surface about `mask.center`: the reflected copy
    /// reaches up to `2·|center|` further per active axis (`|2c − y| ≤ 2|c| + |y|`).
    ///
    /// Sculpt layers are NOT padded here: their sparse delta grid is spatially unbounded and its
    /// occupancy cannot be bounded cheaply per call (and caching on the serialized struct is
    /// barred by the bincode save-format trap) — consumers needing an ABSOLUTE bound must treat
    /// sculpt-layer prims as unbounded via [`Self::has_unbounded_field`].
    fn modifier_pad(&self) -> f32 {
        let max_s = self.scale.x.abs().max(self.scale.y.abs()).max(self.scale.z.abs());
        let per_mod = |m: &Modifier| -> f32 {
            if !m.is_active() { return 0.0; }
            let s = m.strength.abs();
            match m.mod_type {
                ModifierType::Twist => s * 0.5 * max_s,
                ModifierType::Taper => s * 0.3 * max_s,
                ModifierType::Shear => s * 0.5 * max_s,
                ModifierType::Smooth => s * max_s,
                // fbm_noise (4 octaves) ∈ ±0.9375, mask ∈ [0,1] → outward ≤ strength.
                ModifierType::Displace => s * max_s,
                ModifierType::Sculpt => (s + m.mask.extents.x) * max_s,
                ModifierType::Spherize => m.param2.max(0.5) * max_s,
                ModifierType::Shell => s * max_s,
                // Emboss raises the surface OUTWARD by strength (Deboss is inward → 0).
                ModifierType::Emboss => s * max_s,
                // Fold: reflected copy reaches 2·|center| further on each active axis.
                ModifierType::Mirror => {
                    let c = m.mask.center;
                    let masked = Vec3::new(
                        if m.axis & 1 != 0 { c.x } else { 0.0 },
                        if m.axis & 2 != 0 { c.y } else { 0.0 },
                        if m.axis & 4 != 0 { c.z } else { 0.0 },
                    );
                    2.0 * (masked * self.scale.abs()).length()
                }
                // Cover the span of all array copies (linear span or circle diameter).
                ModifierType::Array => {
                    use crate::modifiers::ArrayMode;
                    match m.array_mode() {
                        Some(ArrayMode::Linear { count, offset }) => {
                            let span = offset.abs() * (count.saturating_sub(1) as f32);
                            (span * self.scale.abs()).max_element()
                        }
                        Some(ArrayMode::Circular { radius_vec, .. }) => {
                            // Diameter of the ring.
                            2.0 * (radius_vec.abs() * self.scale.abs()).length()
                        }
                        None => 0.0,
                    }
                }
                _ => 0.0,
            }
        };
        if !self.modifiers.is_empty() {
            self.modifiers.iter().map(per_mod).sum()
        } else {
            let mut pad = per_mod(&self.modifier);
            // Legacy prim-level mirror (mirror_flags/mirror_center) — applied unconditionally
            // by world_distance's legacy branch; same reflected-copy reach as the stack fold.
            if self.mirror_flags != 0 {
                let c = self.mirror_center;
                let masked = Vec3::new(
                    if self.mirror_flags & 1 != 0 { c.x } else { 0.0 },
                    if self.mirror_flags & 2 != 0 { c.y } else { 0.0 },
                    if self.mirror_flags & 4 != 0 { c.z } else { 0.0 },
                );
                pad += 2.0 * (masked * self.scale.abs()).length();
            }
            pad
        }
    }

    /// O1 (hierarchical classify): true when a COARSE center-sample cull over this prim set is
    /// SOUND — i.e. the folded field is 1-Lipschitz in world space, so one evaluation at a
    /// super-cell center bounds every sample inside it by ± the half-diagonal (+ the classifier's
    /// own non-Lipschitz margin, scaled to the coarse diagonal). Standard shapes are exact SDFs
    /// (L=1); booleans (min/max/smin/smax/chamfer/bevel joins), prim mirror folds, elongation
    /// (Minkowski), non-uniform scale (underestimating correction), and clip curves (max with a
    /// polygon SDF) all preserve L ≤ 1. DISQUALIFIERS (conservative, the interior-proof
    /// "disabled body-wide" precedent): any modifier (domain warps change L; surface alters add
    /// mask-gradient slope), Seam/Extrude boolean ops (surface displacement within a width), and
    /// MeshVolume grids (trilinear interpolation can locally exceed L=1 between samples).
    pub fn coarse_cull_safe(prims: &[SdfPrimitive]) -> bool {
        prims.iter().all(|p| {
            p.modifier.mod_type == crate::modifiers::ModifierType::None
                && p.modifiers.iter().all(|m| m.mod_type == crate::modifiers::ModifierType::None)
                && !matches!(p.operation, BooleanOp::Seam { .. } | BooleanOp::Extrude { .. })
                && !matches!(p.shape, SdfShape::MeshVolume { .. })
        })
    }

    /// True when this prim's field can place surface at UNBOUNDED distance from the prim origin —
    /// today: any visible, non-empty sculpt layer (its sparse delta grid has no spatial bound and
    /// `Min`/`Add`/`Subtract` blends can create surface wherever it has cells). Consumers that use
    /// `bounding_radius()` as an ABSOLUTE exterior proof (the sparse classifier) MUST skip the
    /// proof for such prims and fall back to exact field evaluation. Comparative uses
    /// (`eval_scene`'s relative cull) degrade gracefully and keep using the radius.
    pub fn has_unbounded_field(&self) -> bool {
        self.sculpt_layers.iter().any(|l| l.visible && !l.delta_grid.cells.is_empty())
    }

    /// INSCRIBED sphere radius in world space — the dual of [`bounding_radius`]: a sphere of
    /// this radius centered at `position` lies FULLY INSIDE the prim's world-space solid, so
    /// for any point `q` with `|q − position| + s ≤ inscribed_radius()`, the prim's signed
    /// distance satisfies `d(q) ≤ −s` (the complement is at least `s` away; signed distance is
    /// 1-Lipschitz). This is the DONOR CONTRACT of the sparse classifier's `provably_interior`
    /// proof — an OVER-estimate here fabricates interior sentinels inside real surface
    /// (invisible holes), so the bound is aggressively conservative and pinned by the
    /// `inscribed_radius_depth_contract` property test.
    ///
    /// Returns 0.0 (no proof possible from this prim) unless the prim is PRISTINE:
    /// - any active modifier (stack or legacy) → 0. Outward warps are padded in
    ///   `bounding_radius`; the INWARD intrusion of Taper/Shell/Sculpt/etc. has no shared
    ///   bound, and Shell hollows the interior entirely — rather than derive per-modifier
    ///   inward bounds, modified prims simply never donate (they still classify exactly).
    /// - any visible sculpt layer (`has_unbounded_field`) → 0 (carving layers).
    /// - a shape without a simple centered inscribed sphere (Torus, Cone, Stair, CurveExtrude,
    ///   MeshVolume, HalfSpace, …) → 0.
    /// - a Box with ANY bevel → 0 (a large chamfer cuts deeper than the face-inscribed
    ///   sphere; deriving the exact beveled inscribed radius isn't worth the risk).
    ///
    /// Elongation only ADDS material (Minkowski sweep) so it keeps the base. Prim-level
    /// `mirror_flags` → 0: the legacy mirror is a domain FOLD, not a union of copies — with an
    /// offset `mirror_center` the fold maps one side of the center plane away from the solid
    /// entirely, so no centered sphere is guaranteed (caught by the depth-contract property
    /// test's first run: a mirrored capsule read d=+0.35 inside the claimed sphere).
    /// Non-uniform scale bounds the sphere by the SMALLEST axis; rotation is irrelevant
    /// (a centered sphere is rotation-invariant).
    pub fn inscribed_radius(&self) -> f32 {
        if self.has_unbounded_field() { return 0.0; }
        if self.mirror_flags != 0 { return 0.0; }
        if self.modifiers.iter().any(|m| m.is_active()) { return 0.0; }
        if self.modifiers.is_empty() && self.modifier.is_active() { return 0.0; }
        let base = match &self.shape {
            SdfShape::Sphere { radius } => *radius,
            SdfShape::Box { half_extents, bevel } => {
                let beveled = bevel.radii.iter().any(|&r| r > 0.0);
                if beveled { 0.0 } else { half_extents.min_element() }
            }
            SdfShape::Cylinder { radius, half_height } => radius.min(*half_height),
            SdfShape::Capsule { radius, .. } => *radius,
            _ => 0.0,
        };
        let min_s = self.scale.x.abs().min(self.scale.y.abs()).min(self.scale.z.abs());
        (base * min_s).max(0.0)
    }

    /// Conservative bounding sphere radius in world space.
    /// Includes shape extents, scale, elongation, blend k, and modifier padding.
    pub fn bounding_radius(&self) -> f32 {
        let bk = match &self.operation {
            BooleanOp::Union { k } | BooleanOp::Subtract { k }
            | BooleanOp::ChamferUnion { k } | BooleanOp::ChamferSubtract { k }
            | BooleanOp::SmoothIntersect { k } => *k,
            BooleanOp::Intersect => 0.0,
            // Seam surface only exists where |prim field| <= width (wall clamp in seam_cut),
            // so width alone preserves the containment contract; + depth additionally covers
            // the interior field inflation (min(a+depth, ..) term) for partial-bake windows.
            BooleanOp::Seam { width, depth, .. } => *width + *depth,
            // Extrude only alters the field where b < width (smoothstep mask saturates to 0
            // beyond), so width alone preserves containment; + |strength| additionally covers
            // the field displacement magnitude for partial-bake windows / interior proofs
            // (same rationale as Seam's + depth). Both signs covered by abs().
            BooleanOp::Extrude { strength, width } => *width + strength.abs(),
        };
        let shape_r = match &self.shape {
            SdfShape::Sphere { radius } =>
                *radius * self.scale.x.max(self.scale.y).max(self.scale.z),
            SdfShape::Box { half_extents: h, .. } =>
                (self.scale * *h).length(),
            SdfShape::Cylinder { radius, half_height } => {
                let r = self.scale.x.max(self.scale.z) * *radius;
                let h = self.scale.y * *half_height;
                (r * r + h * h).sqrt()
            },
            SdfShape::Torus { major_radius, minor_radius } =>
                self.scale.x.max(self.scale.z) * (*major_radius + *minor_radius),
            SdfShape::Cone { radius, half_height } => {
                let r = self.scale.x.max(self.scale.z) * *radius;
                let h = self.scale.y * *half_height;
                (r * r + h * h).sqrt()
            },
            SdfShape::Capsule { radius, half_height } =>
                self.scale.y * *half_height + self.scale.x.max(self.scale.z) * *radius,
            SdfShape::Stair { half_extents: h, .. } =>
                (self.scale * *h).length(),
            SdfShape::SpiralStair { radius, half_height, .. } => {
                let r = self.scale.x.max(self.scale.z) * *radius;
                let h = self.scale.y * *half_height;
                (r * r + h * h).sqrt()
            },
            SdfShape::Billboard { width, height, .. } => {
                let w = self.scale.x * *width * 0.5;
                let h = self.scale.y * *height * 0.5;
                (w * w + h * h).sqrt()
            },
            SdfShape::CurveExtrude { profile, half_depth, open, thickness, tube, .. } => {
                // Estimate from vertex bounding box. Closed mode: shell/rim/chamfer only cut INWARD,
                // so the authored-vertex bound covers it. OPEN mode (P2): the ribbon/tube grows
                // OUTWARD from the path by thickness/2 in-plane, so the bound MUST expand or the
                // sparse cache drops bricks → holes (the P0-B1 lesson).
                let max_r = profile.verts.iter().map(|v| v.pos.length()).fold(0.0f32, f32::max);
                let half_t = if *open { *thickness * 0.5 } else { 0.0 };
                let r = self.scale.x.max(self.scale.z) * (max_r + half_t);
                let h = if *open && *tube {
                    // Round tube: out-of-plane extent = the tube radius (half_depth ignored).
                    self.scale.y * *thickness * 0.5
                } else if *half_depth > 0.001 {
                    self.scale.y * *half_depth
                } else {
                    r
                };
                (r * r + h * h).sqrt()
            },
            SdfShape::MeshVolume { grid, .. } =>
                (self.scale * grid.corner_abs_max()).length(),
            // Infinite plane: its influence is unbounded, so a finite bounding sphere would let the
            // per-prim cull far-skip the slice and leave stale (un-cut) geometry. Return a very large
            // radius so the slice is NEVER culled (matches the GPU bake's bound_r = 1e6). Note:
            // scene/export bounds explicitly exclude HalfSpace (see compute_scene_bounds), so this
            // large value never balloons a bake grid.
            SdfShape::HalfSpace => 1.0e6,
            SdfShape::SpinePipe { points, radius, point_scale, .. } => {
                let max_pt = points.iter().map(|q| q.length()).fold(0.0f32, f32::max);
                let max_s = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
                let smax = self.scale.x.abs().max(self.scale.y.abs()).max(self.scale.z.abs());
                smax * (max_pt + *radius * max_s)
            }
        };
        // Elongation extends the shape along axes in scaled-local space (the domain warp at
        // world_distance's elongation clamp) → world extension = per-axis scale × elongation.
        // bounding_aabb_half_extents already covers this via local_half_extents; the sphere
        // bound was MISSING it (a sphere r=0.3 elongated by 1.0 reaches |x|=1.3 — the exterior
        // proof amputated it).
        let elong = (self.elongation * self.scale.abs()).length();
        shape_r + elong + bk + self.modifier_pad()
    }

    /// Tight axis-aligned bounding box half-extents in body-local space.
    /// Unlike `bounding_radius()` (circumscribed sphere), this returns per-axis
    /// Per-shape local half-extents in the prim's own frame — BEFORE scale, rotation,
    /// blend_k, or modifier padding. Includes `elongation` (the domain-warp stretch).
    /// Axis conventions match `SdfShape::distance()`: Y is up for cylinders/cones/capsules,
    /// XZ is the radial plane for torus/spiral. Shared by `bounding_aabb_half_extents()`
    /// and selection-wireframe drawing so the two never drift.
    pub fn local_half_extents(&self) -> Vec3 {
        let local_he = match &self.shape {
            SdfShape::Sphere { radius } => Vec3::splat(*radius),
            SdfShape::Box { half_extents, .. } => *half_extents,
            SdfShape::Cylinder { radius, half_height } => Vec3::new(*radius, *half_height, *radius),
            SdfShape::Torus { major_radius, minor_radius } => {
                let r_total = *major_radius + *minor_radius;
                Vec3::new(r_total, *minor_radius, r_total)
            }
            SdfShape::Cone { radius, half_height } => Vec3::new(*radius, *half_height, *radius),
            SdfShape::Capsule { radius, half_height } => {
                Vec3::new(*radius, *half_height + *radius, *radius)
            }
            SdfShape::Stair { half_extents, .. } => *half_extents,
            SdfShape::SpiralStair { radius, half_height, .. } => {
                Vec3::new(*radius, *half_height, *radius)
            }
            SdfShape::Billboard { width, height, .. } => {
                Vec3::new(*width * 0.5, *height * 0.5, 0.005)
            }
            SdfShape::CurveExtrude { profile, half_depth, open, thickness, tube, .. } => {
                let mut max_r = profile.verts.iter().map(|v| v.pos.length()).fold(0.5f32, f32::max);
                if *open { max_r += *thickness * 0.5; } // OPEN: ribbon/tube grows outward from the path
                let h = if *open && *tube { *thickness * 0.5 }
                        else if *half_depth > 0.001 { *half_depth }
                        else { max_r };
                Vec3::new(max_r, h, max_r)
            }
            SdfShape::MeshVolume { grid, .. } => grid.corner_abs_max(),
            // Infinite plane — no finite AABB. A large half-extent makes an edit to the slice dirty
            // the WHOLE body (the plane cuts everywhere); `mark_prim_dirty_aabb` clamps the region to
            // the body grid bounds, so this becomes a correct full re-cut rather than a partial one.
            SdfShape::HalfSpace => Vec3::splat(1.0e6),
            SdfShape::SpinePipe { points, radius, point_scale, .. } => {
                let mut mn = Vec3::splat(f32::MAX);
                let mut mx = Vec3::splat(f32::MIN);
                let max_s = point_scale.iter().copied().fold(1.0f32, f32::max).max(1.0);
                let rpad = *radius * max_s;
                if points.is_empty() {
                    return Vec3::splat(rpad) + self.elongation;
                }
                for q in points {
                    mn = mn.min(*q);
                    mx = mx.max(*q);
                }
                let half = (mx - mn) * 0.5;
                let pad = Vec3::splat(rpad);
                // Half-extents of AABB around points, expanded by tube radius (local frame).
                // Using max abs corner of expanded box relative to origin for a safe OBB shell.
                let c = (mn + mx) * 0.5;
                let he = half + pad;
                Vec3::new(
                    c.x.abs() + he.x,
                    c.y.abs() + he.y,
                    c.z.abs() + he.z,
                )
            }
        };
        local_he + self.elongation
    }

    /// half-extents that account for the prim's rotation via OBB→AABB projection.
    /// Includes blend_k and modifier padding as uniform expansion.
    pub fn bounding_aabb_half_extents(&self) -> Vec3 {
        let bk = match &self.operation {
            BooleanOp::Union { k } | BooleanOp::Subtract { k }
            | BooleanOp::ChamferUnion { k } | BooleanOp::ChamferSubtract { k }
            | BooleanOp::SmoothIntersect { k } => *k,
            BooleanOp::Intersect => 0.0,
            // Same pad as bounding_radius — the two bounds must not drift.
            BooleanOp::Seam { width, depth, .. } => *width + *depth,
            BooleanOp::Extrude { strength, width } => *width + strength.abs(),
        };
        // Apply per-axis scale (abs for negative/mirrored scales).
        // local_half_extents() already folds in elongation.
        let scaled = self.local_half_extents() * self.scale.abs();

        // Rotation cannot inflate a rotation-INVARIANT world shape: a sphere with uniform
        // |scale| and no elongation has the same AABB at every orientation. The generic
        // OBB→AABB projection below treats it as a rotated BOX of half-extent r and inflates
        // by up to √3 — owner-visible as "the sphere doesn't use the same G/R/S": after any
        // R on a sphere, every G drag carried ~24%+ fatter dirty regions and tight bounds
        // (log signature: a 0.5-radius sphere reporting ±0.622), ballooning the partial bake
        // windows to near-full-grid (23-29ms/frame vs the cube's 6-14ms). A NON-uniform scale
        // makes it an ellipsoid (orientation matters) and elongation breaks the symmetry —
        // both keep the projection.
        let s = self.scale.abs();
        let sphere_invariant = matches!(self.shape, SdfShape::Sphere { .. })
            && (s.x - s.y).abs() < 1e-6 && (s.y - s.z).abs() < 1e-6
            && self.elongation == Vec3::ZERO;
        let rotated_he = if sphere_invariant {
            scaled // exact — the sphere's AABB is orientation-free
        } else {
            // OBB→AABB: project each rotated basis vector onto world axes.
            // abs(rotation_matrix) * half_extents = tight AABB half-extents of the rotated box.
            let rot = Mat3::from_quat(self.rotation_quat());
            let abs_rot = Mat3::from_cols(
                rot.x_axis.abs(),
                rot.y_axis.abs(),
                rot.z_axis.abs(),
            );
            abs_rot * scaled
        };

        // Uniform padding for blend_k + modifiers — the SHARED bound (same fn as
        // bounding_radius, covers the modifier STACK + legacy + prim mirror folds).
        rotated_he + Vec3::splat(bk + self.modifier_pad())
    }

    /// First active Array modifier placement, if any.
    pub fn array_mode(&self) -> Option<crate::modifiers::ArrayMode> {
        self.modifiers.iter().find_map(|m| m.array_mode())
    }

    /// Expand Array modifiers into concrete prim copies (GPU bake / serialize).
    /// Circular: N copies on a ring around `position` (empty center).  
    /// Linear: N copies along `position + i*offset`.
    pub fn expand_array_copies(&self) -> Vec<SdfPrimitive> {
        use crate::modifiers::{rotate_around_axis, ArrayMode};
        let Some(mode) = self.array_mode() else {
            return vec![self.clone()];
        };
        match mode {
            ArrayMode::Linear { count, offset } => {
                let mut out = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let mut c = self.clone();
                    c.position += offset * i as f32;
                    c.modifiers.retain(|m| m.mod_type != ModifierType::Array);
                    out.push(c);
                }
                out
            }
            ArrayMode::Circular {
                count,
                radius_vec,
                axis,
            } => {
                let mut out = Vec::with_capacity(count as usize);
                let n = count.max(1) as f32;
                for i in 0..count {
                    let a = std::f32::consts::TAU * i as f32 / n;
                    let off = rotate_around_axis(radius_vec, axis, a);
                    let mut c = self.clone();
                    c.position += off;
                    c.modifiers.retain(|m| m.mod_type != ModifierType::Array);
                    out.push(c);
                }
                out
            }
        }
    }

    pub fn world_distance(&self, world_p: Vec3) -> f32 {
        use crate::modifiers::{rotate_around_axis, ArrayMode};
        // Array: min over N placed copies (linear step or circle ring).
        if let Some(mode) = self.array_mode() {
            let mut best = f32::MAX;
            match mode {
                ArrayMode::Linear { count, offset } => {
                    for i in 0..count {
                        best = best.min(self.world_distance_no_array(world_p - offset * i as f32));
                    }
                }
                ArrayMode::Circular {
                    count,
                    radius_vec,
                    axis,
                } => {
                    let n = count.max(1) as f32;
                    for i in 0..count {
                        let a = std::f32::consts::TAU * i as f32 / n;
                        let off = rotate_around_axis(radius_vec, axis, a);
                        best = best.min(self.world_distance_no_array(world_p - off));
                    }
                }
            }
            return best;
        }
        self.world_distance_no_array(world_p)
    }

    fn world_distance_no_array(&self, world_p: Vec3) -> f32 {
        let local_p = self.rotation_quat().inverse() * (world_p - self.position);
        let mut scaled_p = local_p / self.scale;

        // Use modifier stack if available, otherwise fall back to legacy fields
        if !self.modifiers.is_empty() {
            // Pass 1: Mirror folds (must happen first)
            for m in &self.modifiers {
                if m.mod_type == ModifierType::Mirror {
                    let mc = m.mask.center;
                    let flags = m.axis;
                    if (flags & 1) != 0 { scaled_p.x = (scaled_p.x - mc.x).abs() + mc.x; }
                    if (flags & 2) != 0 { scaled_p.y = (scaled_p.y - mc.y).abs() + mc.y; }
                    if (flags & 4) != 0 { scaled_p.z = (scaled_p.z - mc.z).abs() + mc.z; }
                }
            }
            // Elongation domain warp: extend shape along axes preserving cross-section
            if self.elongation.x > 0.0 { scaled_p.x -= scaled_p.x.clamp(-self.elongation.x, self.elongation.x); }
            if self.elongation.y > 0.0 { scaled_p.y -= scaled_p.y.clamp(-self.elongation.y, self.elongation.y); }
            if self.elongation.z > 0.0 { scaled_p.z -= scaled_p.z.clamp(-self.elongation.z, self.elongation.z); }
            // Pass 2: Domain warps (Twist, Taper, Shear)
            let mut eval_p = scaled_p;
            for m in &self.modifiers {
                if !m.is_active() { continue; }
                let mask_w = m.mask.evaluate(eval_p) * m.strength;
                eval_p = match m.mod_type {
                    ModifierType::Twist => apply_twist(eval_p, mask_w, m.axis),
                    ModifierType::Taper => apply_taper(eval_p, mask_w, m.axis, m.param2),
                    ModifierType::Shear => apply_shear(eval_p, mask_w, m.axis, m.param2),
                    _ => eval_p,
                };
            }
            let mut d = if self.bevel_profile_top.abs() > 0.001 || self.bevel_profile_bot.abs() > 0.001 || self.bevel_profile_vert.abs() > 0.001 {
                self.shape.distance_profiled(eval_p,
                    self.bevel_profile_top, self.bevel_profile_bot, self.bevel_profile_vert,
                    self.chamfer_angle_top, self.chamfer_angle_bot, self.chamfer_angle_vert)
            } else { self.shape.distance(eval_p) };
            // Pass 3: Surface alterations (Smooth, Displace, Sculpt)
            for m in &self.modifiers {
                if !m.is_active() { continue; }
                let mask_w = m.mask.evaluate(scaled_p);
                match m.mod_type {
                    ModifierType::Smooth => { d -= m.strength * mask_w; }
                    ModifierType::Displace => {
                        let freq = m.param2.max(0.1);
                        let noise_val = if let Some(ref hm) = m.height_map {
                            let (u, v) = crate::modifiers::DisplaceHeightMap::uv_from_local(
                                scaled_p, m.axis, freq,
                            );
                            // Map [0,1] height → [-1,1] like FBM so strength is comparable.
                            (hm.sample(u, v) * 2.0 - 1.0)
                        } else {
                            fbm_noise(scaled_p * freq, 4)
                        };
                        d += noise_val * m.strength * mask_w;
                    }
                    ModifierType::Sculpt => {
                        let op = SculptOp::from_f32(m.param2);
                        d = apply_sculpt(scaled_p, d, m.strength, op, mask_w, m.axis, m.mask.center);
                    }
                    ModifierType::Spherize => {
                        // Morph result toward a sphere: mix(d, sphere_d, t). Lipschitz-safe.
                        let t = (m.strength * mask_w).clamp(0.0, 1.0);
                        let r = if m.param2 > 0.0 { m.param2 } else { 0.5 };
                        let sphere_d = scaled_p.length() - r;
                        d += (sphere_d - d) * t;
                    }
                    ModifierType::Shell => {
                        // Onion / hollow shell: abs(d) - thickness.
                        d = d.abs() - m.strength * mask_w;
                    }
                    ModifierType::Emboss => { d -= m.strength * mask_w; } // raised relief (stamp primitive = mask shape)
                    ModifierType::Deboss => { d += m.strength * mask_w; } // recessed relief
                    _ => {}
                }
            }
            // Pass 4: Sculpt layers — accumulative delta blending
            for layer in &self.sculpt_layers {
                if !layer.visible { continue; }
                let delta = layer.delta_grid.sample(scaled_p);
                if delta.abs() < 1e-7 { continue; }
                let contribution = delta * layer.opacity;
                match layer.blend_mode {
                    crate::sculpt_layer::LayerBlendMode::Add      => d += contribution,
                    crate::sculpt_layer::LayerBlendMode::Subtract => d -= contribution,
                    crate::sculpt_layer::LayerBlendMode::Max      => d = d.max(contribution),
                    crate::sculpt_layer::LayerBlendMode::Min      => d = d.min(contribution),
                }
            }
            // Clip curves: stacked boolean cutters
            if !self.clip_curves.is_empty() || (self.clip_curve_enabled && self.clip_curve_radius > 0.001) {
                d = self.apply_clip_curves(d, eval_p);
            }
            d * self.scale.x.min(self.scale.y).min(self.scale.z)
        } else {
            // Legacy path: single modifier + mirror_flags
            let mc = self.mirror_center;
            if (self.mirror_flags & 1) != 0 { scaled_p.x = (scaled_p.x - mc.x).abs() + mc.x; }
            if (self.mirror_flags & 2) != 0 { scaled_p.y = (scaled_p.y - mc.y).abs() + mc.y; }
            if (self.mirror_flags & 4) != 0 { scaled_p.z = (scaled_p.z - mc.z).abs() + mc.z; }
            // Elongation domain warp: extend shape along axes preserving cross-section
            if self.elongation.x > 0.0 { scaled_p.x -= scaled_p.x.clamp(-self.elongation.x, self.elongation.x); }
            if self.elongation.y > 0.0 { scaled_p.y -= scaled_p.y.clamp(-self.elongation.y, self.elongation.y); }
            if self.elongation.z > 0.0 { scaled_p.z -= scaled_p.z.clamp(-self.elongation.z, self.elongation.z); }
            let eval_p = if self.modifier.is_active() {
                let mask_w = self.modifier.mask.evaluate(scaled_p) * self.modifier.strength;
                match self.modifier.mod_type {
                    ModifierType::Twist => apply_twist(scaled_p, mask_w, self.modifier.axis),
                    ModifierType::Taper => apply_taper(scaled_p, mask_w, self.modifier.axis, self.modifier.param2),
                    ModifierType::Shear => apply_shear(scaled_p, mask_w, self.modifier.axis, self.modifier.param2),
                    _ => scaled_p,
                }
            } else { scaled_p };
            let mut d = if self.bevel_profile_top.abs() > 0.001 || self.bevel_profile_bot.abs() > 0.001 || self.bevel_profile_vert.abs() > 0.001 {
                self.shape.distance_profiled(eval_p,
                    self.bevel_profile_top, self.bevel_profile_bot, self.bevel_profile_vert,
                    self.chamfer_angle_top, self.chamfer_angle_bot, self.chamfer_angle_vert)
            } else { self.shape.distance(eval_p) };
            if self.modifier.is_active() {
                let mask_w = self.modifier.mask.evaluate(scaled_p);
                match self.modifier.mod_type {
                    ModifierType::Smooth => { d -= self.modifier.strength * mask_w; }
                    ModifierType::Displace => {
                        let freq = self.modifier.param2.max(0.1);
                        let noise_val = if let Some(ref hm) = self.modifier.height_map {
                            let (u, v) = crate::modifiers::DisplaceHeightMap::uv_from_local(
                                scaled_p, self.modifier.axis, freq,
                            );
                            (hm.sample(u, v) * 2.0 - 1.0)
                        } else {
                            fbm_noise(scaled_p * freq, 4)
                        };
                        d += noise_val * self.modifier.strength * mask_w;
                    }
                    ModifierType::Sculpt => {
                        let op = SculptOp::from_f32(self.modifier.param2);
                        d = apply_sculpt(scaled_p, d, self.modifier.strength, op, mask_w, self.modifier.axis, self.modifier.mask.center);
                    }
                    ModifierType::Spherize => {
                        let t = (self.modifier.strength * mask_w).clamp(0.0, 1.0);
                        let r = if self.modifier.param2 > 0.0 { self.modifier.param2 } else { 0.5 };
                        let sphere_d = scaled_p.length() - r;
                        d += (sphere_d - d) * t;
                    }
                    ModifierType::Shell => {
                        d = d.abs() - self.modifier.strength * mask_w;
                    }
                    ModifierType::Emboss => { d -= self.modifier.strength * mask_w; } // raised relief
                    ModifierType::Deboss => { d += self.modifier.strength * mask_w; } // recessed relief
                    _ => {}
                }
            }
            // Clip curves: stacked boolean cutters
            if !self.clip_curves.is_empty() || (self.clip_curve_enabled && self.clip_curve_radius > 0.001) {
                d = self.apply_clip_curves(d, eval_p);
            }
            d * self.scale.x.min(self.scale.y).min(self.scale.z)
        }
    }

    /// Sync the modifier stack into legacy fields for GPU serialization.
    /// Call after modifying `modifiers` to keep GPU rendering in sync.
    ///
    /// GPU bake only has **one** non-mirror modifier slot. When the stack needs more
    /// (`needs_cpu_full_stack_bake`), production bake routes to CPU `BodyField` instead
    /// (P3 honesty). This sync still fills the single slot for GPU paths that only
    /// have one active non-mirror mod.
    #[allow(clippy::needless_update)] // kept for call-site clarity after stack edits
    pub fn sync_modifiers_to_gpu_fields(&mut self) {
        if self.modifiers.is_empty() { return; }
        // Sync mirror modifiers → mirror_flags + mirror_center
        self.mirror_flags = 0;
        self.mirror_center = Vec3::ZERO;
        for m in &self.modifiers {
            if m.mod_type == ModifierType::Mirror {
                self.mirror_flags |= m.axis;
                self.mirror_center = m.mask.center;
            }
        }
        // Sync one non-mirror active modifier → legacy modifier field
        // Prefer Sculpt if present (real-time brush), otherwise first active
        let sculpt = self.modifiers.iter()
            .find(|m| m.mod_type == ModifierType::Sculpt && m.is_active());
        let fallback = self.modifiers.iter()
            .find(|m| m.is_active() && m.mod_type != ModifierType::Mirror);
        self.modifier = sculpt.or(fallback).cloned().unwrap_or_default();
    }

    /// True when this prim's stack cannot be represented by the GPU bake's single
    /// non-mirror modifier slot (P3). Mirrors alone are fine (axis flags). CPU
    /// `world_distance` / `BodyField` always evaluate the full stack.
    ///
    /// T1: single-slot image Displace is GPU-ok — height maps pack into the bake
    /// side buffer (`serialize_height_maps_for_bake`, hot[92], binding 8). Multi
    /// non-mirror stacks still force CPU.
    pub fn needs_cpu_full_stack_bake(&self) -> bool {
        let mut non_mirror = 0usize;
        for m in &self.modifiers {
            if !m.is_active() || m.mod_type == ModifierType::None || m.mod_type == ModifierType::Mirror {
                continue;
            }
            non_mirror += 1;
            if non_mirror > 1 {
                return true;
            }
        }
        false
    }

    /// Modifier packed into the GPU single slot (stack-aware; mirrors stay in flags).
    pub fn gpu_slot_modifier(&self) -> Modifier {
        if self.modifiers.is_empty() {
            return self.modifier.clone();
        }
        let sculpt = self.modifiers.iter()
            .find(|m| m.mod_type == ModifierType::Sculpt && m.is_active());
        let fallback = self.modifiers.iter()
            .find(|m| m.is_active() && m.mod_type != ModifierType::Mirror);
        sculpt.or(fallback).cloned().unwrap_or_default()
    }

    /// Mirror flags/center from stack (or legacy fields).
    pub fn gpu_mirror_state(&self) -> (u8, Vec3) {
        if self.modifiers.is_empty() {
            return (self.mirror_flags, self.mirror_center);
        }
        let mut flags = 0u8;
        let mut center = Vec3::ZERO;
        for m in &self.modifiers {
            if m.is_active() && m.mod_type == ModifierType::Mirror {
                flags |= m.axis;
                center = m.mask.center;
            }
        }
        (flags, center)
    }
}

// ============================================================
// QA Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn scene_sdf_single_sphere() {
        let sphere = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        let prims = vec![sphere];
        let d_center = scene_sdf(Vec3::ZERO, &prims);
        assert!((d_center - (-0.5)).abs() < 0.01, "center d={}", d_center);
        let d_surface = scene_sdf(Vec3::new(0.5, 0.0, 0.0), &prims);
        assert!(d_surface.abs() < 0.01, "surface d={}", d_surface);
        let d_outside = scene_sdf(Vec3::new(1.0, 0.0, 0.0), &prims);
        assert!((d_outside - 0.5).abs() < 0.01, "outside d={}", d_outside);
    }

    #[test]
    fn scene_sdf_boolean_subtract() {
        let mut a = SdfPrimitive::new("A".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        a.operation = BooleanOp::Union { k: 0.0 };
        let mut b = SdfPrimitive::new("B".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(0.5, 0.0, 0.0));
        b.operation = BooleanOp::Subtract { k: 0.0 };
        let prims = vec![a, b];
        let d = scene_sdf(Vec3::new(0.5, 0.0, 0.0), &prims);
        assert!(d > 0.0, "carved point should be outside, got d={}", d);
    }

    #[test]
    fn scene_sdf_boolean_union() {
        let mut a = SdfPrimitive::new("A".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        a.operation = BooleanOp::Union { k: 0.0 };
        let mut b = SdfPrimitive::new("B".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::new(0.8, 0.0, 0.0));
        b.operation = BooleanOp::Union { k: 0.0 };
        let prims = vec![a, b];
        let d = scene_sdf(Vec3::new(0.4, 0.0, 0.0), &prims);
        assert!(d < 0.0, "midpoint in union should be inside, got d={}", d);
    }

    #[test]
    fn world_distance_with_position() {
        let prim = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::new(5.0, 0.0, 0.0));
        let d = prim.world_distance(Vec3::new(5.0, 0.0, 0.0));
        assert!((d - (-1.0)).abs() < 0.01, "center d={}", d);
        let d2 = prim.world_distance(Vec3::new(6.0, 0.0, 0.0));
        assert!(d2.abs() < 0.01, "surface d={}", d2);
    }

    #[test]
    fn invisible_prim_excluded_from_scene() {
        let mut sphere = SdfPrimitive::new("S".into(), SdfShape::Sphere { radius: 1.0 }, Vec3::ZERO);
        sphere.visible = false;
        let prims = vec![sphere];
        let d = scene_sdf(Vec3::ZERO, &prims);
        assert!(d > 50.0, "invisible prim should be excluded, got d={}", d);
    }

    /// CONTAINMENT CONTRACT property test (the S2.5g soundness fix): for every prim configuration,
    /// `world_distance(p) > 0` for all p with `|p − position| > bounding_radius()`. This is the
    /// exact invariant the sparse classifier's exterior PROOF (`provably_exterior` + the S2.5h
    /// classify prefilter), `eval_scene`'s per-prim cull, and the GPU `bound_r` all consume.
    /// Before the fix, the modifier STACK contributed ZERO pad (every UI-added modifier),
    /// Displace had no pad even on the legacy arm, and the sphere bound missed elongation —
    /// each a proven-holes case.
    ///
    /// Deterministic seeded LCG (no rand dep); directions from a fixed sphere-ish set; radial
    /// probes just outside the bound (surface extrema live near the bound, not at infinity).
    #[test]
    fn bounding_radius_contains_world_surface() {
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut rng = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32) / (u32::MAX >> 1) as f32 // [0, 2)
        };
        let dirs: Vec<Vec3> = {
            let mut v = vec![
                Vec3::X, Vec3::Y, Vec3::Z, -Vec3::X, -Vec3::Y, -Vec3::Z,
                Vec3::new(1.0, 1.0, 1.0).normalize(), Vec3::new(-1.0, 1.0, -1.0).normalize(),
                Vec3::new(1.0, -1.0, 1.0).normalize(), Vec3::new(-1.0, -1.0, 1.0).normalize(),
            ];
            for _ in 0..14 {
                v.push(Vec3::new(rng() - 1.0, rng() - 1.0, rng() - 1.0).normalize_or_zero());
            }
            v
        };

        let mk_mod = |t: ModifierType, strength: f32, param2: f32, axis: u8| Modifier {
            mod_type: t, strength, axis, param2, mask: VolumeMask::default(), height_map: None,
        };

        let mut checked = 0usize;
        for case in 0..240 {
            let shape = match case % 6 {
                0 => SdfShape::Sphere { radius: 0.2 + rng() * 0.5 },
                1 => SdfShape::Box { half_extents: Vec3::new(0.2 + rng() * 0.4, 0.1 + rng() * 0.4, 0.15 + rng() * 0.4), bevel: crate::sdf_shapes::BoxBevel::default() },
                2 => SdfShape::Cylinder { radius: 0.15 + rng() * 0.3, half_height: 0.2 + rng() * 0.4 },
                3 => SdfShape::Capsule { radius: 0.1 + rng() * 0.25, half_height: 0.2 + rng() * 0.3 },
                4 => SdfShape::Torus { major_radius: 0.25 + rng() * 0.3, minor_radius: 0.05 + rng() * 0.15 },
                _ => SdfShape::Cone { radius: 0.15 + rng() * 0.3, half_height: 0.2 + rng() * 0.4 },
            };
            let mut p = SdfPrimitive::new("t".into(), shape, Vec3::new(rng() - 1.0, rng() - 1.0, rng() - 1.0));
            p.scale = Vec3::new(0.4 + rng(), 0.4 + rng(), 0.4 + rng()); // 0.4 .. 2.4 — covers >1 (the ×max_scale term)
            p.rotation_deg = Vec3::new(rng() * 180.0, rng() * 180.0, rng() * 180.0);
            p.operation = BooleanOp::Union { k: rng() * 0.2 };

            match case % 8 {
                // The pre-fix holes cases first:
                0 => p.modifiers.push(mk_mod(ModifierType::Displace, 0.1 + rng() * 0.4, 1.0 + rng() * 4.0, 0)), // STACK Displace
                1 => p.elongation = Vec3::new(rng(), rng() * 0.5, rng() * 0.8),                                  // elongation (sphere bound)
                2 => { // STACK multi: Smooth + Shell
                    p.modifiers.push(mk_mod(ModifierType::Smooth, 0.05 + rng() * 0.2, 0.5, 0));
                    p.modifiers.push(mk_mod(ModifierType::Shell, 0.02 + rng() * 0.1, 0.5, 0));
                }
                3 => { // STACK Mirror with offset center (reflected copy reach)
                    let mut m = mk_mod(ModifierType::Mirror, 1.0, 0.5, 1 | 2);
                    m.mask.center = Vec3::new(rng() * 0.6, rng() * 0.6, 0.0);
                    p.modifiers.push(m);
                }
                4 => { // legacy single modifier (regression: old path stays covered)
                    p.modifier = mk_mod(ModifierType::Displace, 0.1 + rng() * 0.3, 2.0, 0);
                }
                5 => { // legacy prim mirror_flags with offset center
                    p.mirror_flags = 1 | 4;
                    p.mirror_center = Vec3::new(rng() * 0.5, 0.0, rng() * 0.5);
                }
                6 => { // STACK domain warps
                    p.modifiers.push(mk_mod(ModifierType::Twist, rng() * 2.0, 0.5, 0));
                    p.modifiers.push(mk_mod(ModifierType::Taper, rng(), 0.5, 0));
                }
                _ => {} // bare shape (baseline)
            }

            let r = p.bounding_radius();
            assert!(r.is_finite() && r > 0.0, "case {case}: bad radius {r}");
            // Probe shells just OUTSIDE the bound — surface extrema live near the bound.
            for &eps in &[1e-3, 0.02, 0.1, 0.5] {
                for d in &dirs {
                    let q = p.position + *d * (r * (1.0 + eps) + 1e-4);
                    let dist = p.world_distance(q);
                    assert!(
                        dist > 0.0,
                        "CONTAINMENT VIOLATED case {case} ({:?} mods={} legacy={:?} elong={:?} scale={:?}): \
                         d={dist} at |q-pos|={} vs bounding_radius={r}",
                        p.shape, p.modifiers.len(), p.modifier.mod_type, p.elongation, p.scale,
                        (q - p.position).length()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 20_000, "vacuous: only {checked} probes ran");
    }

    /// DONOR CONTRACT property test (P1-#6c, the dual of the containment test above): for every
    /// PRISTINE donor prim, `world_distance(q) ≤ −s` for all q with
    /// `|q − position| + s ≤ inscribed_radius()`. This is the exact depth bound the sparse
    /// classifier's `provably_interior` proof consumes — an over-estimate fabricates INTERIOR
    /// sentinels inside real surface (invisible holes in the solid), the mirror-image failure of
    /// the exterior proof's under-bound. Also pins the conservative gates: any active modifier,
    /// sculpt layer, bevel, or non-donor shape must return exactly 0.0.
    #[test]
    fn inscribed_radius_depth_contract() {
        let mut seed: u64 = 0xC0FFEE0DDBA5EBA5;
        let mut rng = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32) / (u32::MAX >> 1) as f32 // [0, 2)
        };
        let dirs: Vec<Vec3> = {
            let mut v = vec![
                Vec3::X, Vec3::Y, Vec3::Z, -Vec3::X, -Vec3::Y, -Vec3::Z,
                Vec3::new(1.0, 1.0, 1.0).normalize(), Vec3::new(-1.0, 1.0, -1.0).normalize(),
            ];
            for _ in 0..10 {
                v.push(Vec3::new(rng() - 1.0, rng() - 1.0, rng() - 1.0).normalize_or_zero());
            }
            v
        };

        let mut checked = 0usize;
        for case in 0..160 {
            let shape = match case % 4 {
                0 => SdfShape::Sphere { radius: 0.2 + rng() * 0.5 },
                1 => SdfShape::Box { half_extents: Vec3::new(0.2 + rng() * 0.4, 0.15 + rng() * 0.4, 0.25 + rng() * 0.4), bevel: crate::sdf_shapes::BoxBevel::default() },
                2 => SdfShape::Cylinder { radius: 0.15 + rng() * 0.3, half_height: 0.2 + rng() * 0.4 },
                _ => SdfShape::Capsule { radius: 0.1 + rng() * 0.25, half_height: 0.2 + rng() * 0.3 },
            };
            let mut p = SdfPrimitive::new("t".into(), shape, Vec3::new(rng() - 1.0, rng() - 1.0, rng() - 1.0));
            p.scale = Vec3::new(0.4 + rng(), 0.4 + rng(), 0.4 + rng());
            p.rotation_deg = Vec3::new(rng() * 180.0, rng() * 180.0, rng() * 180.0);
            p.operation = BooleanOp::Union { k: rng() * 0.2 };
            // Elongation (Minkowski sweep — only adds material) stays a donor; prim mirror
            // does NOT (fold semantics — see the negative control below).
            if case % 4 == 1 {
                p.elongation = Vec3::new(rng() * 0.5, rng() * 0.3, rng() * 0.4);
            }

            let r_in = p.inscribed_radius();
            assert!(r_in.is_finite() && r_in > 0.0, "case {case}: pristine donor must have r_in > 0, got {r_in}");
            // Probe at |q-pos| = r_in − s for a range of depths s: distance must be ≤ −s (+tol).
            for &s in &[1e-3f32, 0.02, 0.1] {
                if s >= r_in { continue; }
                for d in &dirs {
                    let q = p.position + *d * (r_in - s);
                    let dist = p.world_distance(q);
                    assert!(
                        dist <= -s + 1e-3,
                        "DEPTH CONTRACT VIOLATED case {case} ({:?} scale={:?} elong={:?} mf={}): \
                         d={dist} at |q-pos|={} vs r_in={r_in} (needed ≤ {})",
                        p.shape, p.scale, p.elongation, p.mirror_flags,
                        (q - p.position).length(), -s + 1e-3
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 5_000, "vacuous: only {checked} probes ran");

        // NEGATIVE CONTROLS — the conservative gates must all return exactly 0.0.
        let mk = |shape: SdfShape| SdfPrimitive::new("n".into(), shape, Vec3::ZERO);
        assert_eq!(mk(SdfShape::Torus { major_radius: 0.5, minor_radius: 0.1 }).inscribed_radius(), 0.0,
            "no centered inscribed sphere in a torus");
        assert_eq!(mk(SdfShape::Cone { radius: 0.3, half_height: 0.4 }).inscribed_radius(), 0.0);
        let mut beveled = mk(SdfShape::Box {
            half_extents: Vec3::splat(0.5),
            bevel: { let mut b = crate::sdf_shapes::BoxBevel::default(); b.radii[0] = 0.2; b },
        });
        assert_eq!(beveled.inscribed_radius(), 0.0, "beveled box never donates");
        beveled.shape = SdfShape::Sphere { radius: 0.5 };
        beveled.modifiers.push(Modifier {
            mod_type: ModifierType::Shell, strength: 0.05, axis: 0, param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        });
        assert_eq!(beveled.inscribed_radius(), 0.0, "modified (Shell-hollowed) prim never donates");
        // Prim-level mirror is a domain FOLD — with an offset center it removes the solid on one
        // side of the center plane entirely (the property test's first run caught d=+0.35 inside
        // the claimed sphere on a mirrored capsule). Never a donor.
        let mut mirrored = mk(SdfShape::Sphere { radius: 0.5 });
        mirrored.mirror_flags = 1 | 2;
        mirrored.mirror_center = Vec3::new(0.4, 0.4, 0.0);
        assert_eq!(mirrored.inscribed_radius(), 0.0, "prim-mirror (fold) never donates");
    }

    /// Rotation must NOT inflate a rotation-invariant shape's AABB (the "sphere doesn't use
    /// the same G/R/S" bug): a uniformly-scaled sphere's half-extents are exactly r·s at every
    /// orientation. The generic OBB→AABB projection blew a rotated 0.5-sphere up to ±0.622+
    /// (up to √3×), fattening every dirty region and tight bound the transform tools produce.
    /// Ellipsoids (non-uniform scale) and elongated spheres legitimately keep the projection.
    #[test]
    fn sphere_aabb_rotation_invariant() {
        let mut p = SdfPrimitive::new("s".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        p.operation = BooleanOp::Union { k: 0.0 }; // default op carries k=0.15 → uniform pad
        p.scale = Vec3::splat(1.3);
        let he0 = p.bounding_aabb_half_extents();
        assert!((he0 - Vec3::splat(0.65)).abs().max_element() < 1e-6, "unrotated: exact r·s, got {he0}");
        for rot in [Vec3::new(45.0, 0.0, 0.0), Vec3::new(30.0, 60.0, 15.0), Vec3::new(0.0, 45.0, 45.0)] {
            p.rotation_deg = rot;
            let he = p.bounding_aabb_half_extents();
            assert!((he - he0).abs().max_element() < 1e-6,
                "rotated sphere AABB inflated: {he} vs {he0} at rot={rot}");
        }
        // Ellipsoid (non-uniform scale): rotation DOES change the AABB — the projection must stay.
        p.scale = Vec3::new(1.0, 0.5, 0.5);
        p.rotation_deg = Vec3::ZERO;
        let flat = p.bounding_aabb_half_extents();
        p.rotation_deg = Vec3::new(0.0, 0.0, 45.0);
        let tilted = p.bounding_aabb_half_extents();
        // The (conservative box-)projection must still apply: y grows toward the long axis.
        assert!(tilted.y > flat.y + 0.1 && (tilted - flat).abs().max_element() > 0.1,
            "tilted ellipsoid must project ({flat} -> {tilted})");
        // The box keeps the exact projection behavior (regression guard on the refactor).
        let mut b = SdfPrimitive::new("b".into(),
            SdfShape::Box { half_extents: Vec3::splat(0.5), bevel: crate::sdf_shapes::BoxBevel::default() },
            Vec3::ZERO);
        b.operation = BooleanOp::Union { k: 0.0 };
        b.rotation_deg = Vec3::new(0.0, 45.0, 0.0);
        let bhe = b.bounding_aabb_half_extents();
        assert!((bhe.x - 0.5 * 2.0f32.sqrt()).abs() < 1e-4, "rotated box must still project, got {bhe}");
    }

    #[test]
    fn needs_cpu_full_stack_bake_multi_non_mirror() {
        let mut p = SdfPrimitive::new("t".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        assert!(!p.needs_cpu_full_stack_bake());
        p.modifiers.push(Modifier {
            mod_type: ModifierType::Twist, strength: 0.5, axis: 0, param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        });
        assert!(!p.needs_cpu_full_stack_bake(), "single non-mirror is GPU-ok");
        p.modifiers.push(Modifier {
            mod_type: ModifierType::Displace, strength: 0.1, axis: 0, param2: 2.0,
            mask: VolumeMask::default(),
            height_map: None,
        });
        assert!(p.needs_cpu_full_stack_bake(), "Twist+Displace needs CPU full stack");
        // Mirror does not count toward the single-slot budget
        let mut m = SdfPrimitive::new("m".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        m.modifiers.push(Modifier {
            mod_type: ModifierType::Mirror, strength: 1.0, axis: 1, param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        });
        m.modifiers.push(Modifier {
            mod_type: ModifierType::Smooth, strength: 0.05, axis: 0, param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        });
        assert!(!m.needs_cpu_full_stack_bake(), "Mirror + one surface alter is GPU-ok");
        // T1: single image Displace is GPU-ok (height maps pack into bake binding 8).
        let mut img = SdfPrimitive::new("img".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        img.modifiers.push(Modifier {
            mod_type: ModifierType::Displace,
            strength: 0.1,
            axis: 0,
            param2: 1.0,
            mask: VolumeMask::default(),
            height_map: Some(crate::modifiers::DisplaceHeightMap {
                w: 2,
                h: 2,
                pixels: vec![0, 64, 128, 255],
                path: String::new(),
            }),
        });
        img.sync_modifiers_to_gpu_fields();
        assert!(
            !img.needs_cpu_full_stack_bake(),
            "single image Displace must not force CPU full-stack bake (T1)"
        );
        assert!(img.modifier.has_image_displace());
    }

    #[test]
    fn gpu_slot_modifier_prefers_sculpt_then_first_active() {
        let mut p = SdfPrimitive::new("t".into(), SdfShape::Sphere { radius: 0.5 }, Vec3::ZERO);
        p.modifiers.push(Modifier {
            mod_type: ModifierType::Twist, strength: 0.5, axis: 0, param2: 0.5,
            mask: VolumeMask::default(),
            height_map: None,
        });
        p.modifiers.push(Modifier {
            mod_type: ModifierType::Sculpt, strength: 0.2, axis: 0, param2: 0.0,
            mask: VolumeMask::default(),
            height_map: None,
        });
        assert_eq!(p.gpu_slot_modifier().mod_type, ModifierType::Sculpt);
    }
}
