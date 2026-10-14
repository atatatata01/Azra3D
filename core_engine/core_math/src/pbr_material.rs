// core_engine/core_math/src/pbr_material.rs
// PBR Material System — material data, presets, procedural layers
use glam::Vec3;
use serde::{Serialize, Deserialize};

pub fn surface_bump_normal(base: [f32; 3], dx: f32, dy: f32, strength: f32) -> [f32; 3] {
    let normal = Vec3::from_array(base).normalize_or_zero();
    let reciprocal_x = Vec3::Y.cross(normal);
    let reciprocal_y = normal.cross(Vec3::X);
    let determinant = Vec3::X.dot(reciprocal_x);
    if determinant == 0.0 { return normal.to_array(); }
    let gradient = (dx * reciprocal_x + dy * reciprocal_y) / determinant;
    (normal - strength * gradient).normalize_or_zero().to_array()
}

/// Stable identifier for a scene material. 0 = no material / use embedded default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct MaterialId(pub u32);

impl MaterialId {
    pub const NONE: MaterialId = MaterialId(0);
    pub fn is_none(self) -> bool { self.0 == 0 }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ShaderType { #[default] StandardPBR, Subsurface, Glass, VFX, Advanced }

impl ShaderType {
    /// GPU packing ordinal (RasterMaterial / MaterialData). Append-only — never renumber.
    pub fn as_u32(self) -> u32 {
        match self {
            ShaderType::StandardPBR => 0,
            ShaderType::Subsurface => 1,
            ShaderType::Glass => 2,
            ShaderType::VFX => 3,
            ShaderType::Advanced => 4,
        }
    }
    pub fn from_u32(v: u32) -> ShaderType {
        match v {
            1 => ShaderType::Subsurface,
            2 => ShaderType::Glass,
            3 => ShaderType::VFX,
            4 => ShaderType::Advanced,
            _ => ShaderType::StandardPBR,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum MaterialQuality { Optimize, #[default] Normal, God }

fn default_one() -> f32 { 1.0 }
fn default_sss_color() -> Vec3 { Vec3::new(1.0, 0.2, 0.1) }
fn default_ior() -> f32 { 1.5 }
fn default_cc_rough() -> f32 { 0.1 }
fn default_sheen_tint() -> f32 { 0.5 }

/// A single textured channel handle. `tex_id == 0` ⇒ no map (scalar value used directly).
/// `tex_id > 0` is a 1-based slot into the GPU material texture array
/// (`app::material_textures::MaterialTextureArray`).
///
/// Color maps:
/// - **Tint mode** (`absolute == false`, default): `final = mix(scalar, scalar × sample, strength)`
///   — classic PBR albedo map (file import / hand-loaded Image).
/// - **Absolute mode** (`absolute == true`): `final = mix(scalar, sample, strength)`
///   — layered panel bake already contains the full Mult stack (base×noise1×noise2×…);
///   a second tint-multiply would crush dark Mult stacks (worse than Photoshop 8-bit).
///
/// Unset map (`tex_id == 0`) leaves the scalar behaviour completely unchanged.
///
/// All fields `#[serde(default)]` at the `PbrMaterial` level (the whole struct is added
/// with `#[serde(default)]`) → additive, v6-safe: old save files load with a zero map.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TexMap {
    /// 1-based slot into the material texture array. 0 = no map.
    pub tex_id: u32,
    /// Map influence, 0..1 (1 = full map, 0 = pure scalar tint).
    pub strength: f32,
    /// Tiling / projection scale (world units per tile for triplanar, UV multiplier for UV).
    pub uv_scale: f32,
    /// Low bit 0 = triplanar, 1 = mesh UV. Bit 1 (value 2) = absolute color (see `absolute`).
    /// Packed into GPU as a single u32; use helpers below rather than raw bits.
    pub projection: u32,
    /// Absolute layered-stack sample (no free tint Mult). Serde-default false for old docs.
    #[serde(default)]
    pub absolute: bool,
    /// Triplanar blend sharpness — the exponent on |normal| that weights the three axis samples.
    /// Higher = sharper plane transitions (crisper on flat faces, harder seams on curves); lower
    /// = softer cross-fade. Only Box projection uses it; the single-sample projections ignore it.
    ///
    /// Defaults to 4.0, the value the shader hardcoded before this control existed, so existing
    /// materials render identically. `serde(default)` alone would give 0.0, hence the explicit
    /// `default_blend` — a 0.0 here would flat-average all three planes and blur every material
    /// in every old save.
    #[serde(default = "default_blend")]
    pub blend: f32,
    /// When true, the map uses dual-plane 16-bit in the sRGB texture array (hi + lo slots).
    /// Packed into the GPU projection word at bit 10 (`TEXMAP_HIGH_PRECISION_BIT`).
    ///
    /// **WIRE: `serde(skip)` — never mid-blob.** Bincode has no field-default: a trailing bool
    /// on `TexMap` corrupts every existing `.fl4` (`invalid u8 while decoding bool` — dc_gate0
    /// ref load). Runtime flag is set on map install (`PendingMapLoad` / panel Bit16); survives
    /// the session and the projection word for GPU. Re-apply / re-bake after load restores it.
    #[serde(skip)]
    pub high_precision: bool,
    /// NORMAL maps only: the map's green channel follows the DirectX convention (green = down
    /// the image / +v). `false` (default) = OpenGL convention (green = up the image), which is
    /// what ambientCG / Blender-authored maps ship. The shader flips the sampled green for the
    /// OpenGL case (this file's samplers read v downward), so a wrong flag shows bumps lit
    /// inverted along one axis. Auto-detected at import from the filename (`NormalGL`/`NormalDX`)
    /// and the source layout; user-overridable in the material panel's Normal row.
    ///
    /// **WIRE: `serde(skip)` — never mid-blob** (same bincode field-trap as `high_precision`).
    /// Persists via bit 8 of the FL4MATX2 `MaterialChannelRecord.projection` word (app crate)
    /// — zero there decodes to OpenGL, the correct reading for every pre-flag save.
    #[serde(skip)]
    pub green_dx: bool,
    /// Per-plane 90° rotation for Box (triplanar): 0=0°, 1=90°, 2=180°, 3=270° CW in UV.
    /// `rot_x` = YZ projection, `rot_y` = XZ, `rot_z` = XY. Packed into bits 12-17 of the
    /// GPU word. RUNTIME-ONLY — `#[serde(skip)]`, NEVER `serde(default)`: TexMap is
    /// bincode-blob-serialized (PbrMaterial rides inside prims + graph nodes), bincode is
    /// positional, and a "defaulted" trailing field still consumes bytes on read — the
    /// documented field trap. The `serde(default)` version shipped for a few hours on
    /// 2026-08-07 and broke EVERY pre-existing .fl4 ("failed to fill whole buffer").
    /// Persistence rides FL4MATX2 record bits 10-15 (`MATX2_PROJ_ROT_SHIFT`) for the data
    /// channels; albedo-channel rotation is not yet persisted (no record for albedo).
    #[serde(skip)]
    pub rot_x: u8,
    #[serde(skip)]
    pub rot_y: u8,
    #[serde(skip)]
    pub rot_z: u8,
    #[serde(skip)]
    pub raw_precision: bool,
}

/// Triplanar blend range. The shader clamps to the same bounds.
pub const BLEND_MIN: f32 = 0.25;
/// Top of the control's range. Note `pack_blend` quantises to 0.25 steps with only 31 codes, so
/// the highest STORABLE value is 7.75; see its doc comment for why that trade is deliberate.
pub const BLEND_MAX: f32 = 7.75;
/// Quantisation step, chosen so the 4.0 default is exactly representable.
pub const BLEND_STEP: f32 = 0.25;

/// See `TexMap::blend` — 4.0 is the previously-hardcoded shader constant.
fn default_blend() -> f32 { 4.0 }
impl Default for TexMap {
    fn default() -> Self {
        Self {
            tex_id: 0,
            strength: 1.0,
            uv_scale: 1.0,
            projection: 0,
            absolute: false,
            blend: 4.0,
            high_precision: false,
            green_dx: false,
            rot_x: 0,
            rot_y: 0,
            rot_z: 0,
            raw_precision: false,
        }
    }
}

/// GPU projection word bit 10 — 16-bit RG-packed scalar (see [`TexMap::high_precision`]).
pub const TEXMAP_HIGH_PRECISION_BIT: u32 = 1 << 10;
/// GPU projection word bit 11 — normal-map green channel is DirectX (see [`TexMap::green_dx`]).
pub const TEXMAP_GREEN_DX_BIT: u32 = 1 << 11;
/// Bits 12-13 / 14-15 / 16-17 = triplanar rot_x / rot_y / rot_z (0..=3 quarters).
pub const TEXMAP_ROT_SHIFT: u32 = 12;
pub const TEXMAP_RAW_PRECISION_BIT: u32 = 1 << 18;

impl TexMap {
    #[inline]
    pub fn is_set(&self) -> bool { self.tex_id != 0 }

    /// Projection word for the GPU.
    ///
    /// ```text
    ///   bit0     UV(1) / Box(0)      — LEGACY, kept exactly as it was
    ///   bit1     absolute sample
    ///   bits 2-4 full MapProjection code (0..=4), added 2026-07-28 for Sphere/Cylinder/Planar
    ///   bits 5-9 blend quantisation
    ///   bit 10   high_precision (16-bit RG pack for height/data scalars)
    ///   bit 11   green_dx (normal-map green channel is DirectX / down-image)
    ///   bits 12-13 / 14-15 / 16-17  triplanar rot_x / rot_y / rot_z (0..=3 × 90°)
    ///   bit 18   raw_precision (dual-plane bytes sampled without sRGB decode)
    /// ```
    ///
    /// bit0 could not simply be widened: it sits next to `absolute` at bit1, and every existing
    /// save already encodes Box/UV there. So the full code goes in the previously-unused high
    /// bits and **bit0 is still written**, which keeps two properties that matter:
    ///   * an OLD build reading a NEW save sees the familiar bit0 and renders Box or UV rather
    ///     than garbage (the new projections degrade to Box, their nearest relative);
    ///   * a NEW build reading an OLD save finds zero in bits 2-4, which decodes to `Box` — the
    ///     same thing bit0 = 0 always meant.
    ///
    /// `MUST match gbuffer.wgsl`'s unpack (`amap_pack`) — the WGSL comment names these same bits.
    #[inline]
    pub fn projection_packed(&self) -> u32 {
        let code = self.projection.min(4);
        // bit0 keeps its legacy meaning: set only for UV, so older builds still read Box/UV.
        let legacy = if code == MapProjection::Uv.as_u32() { 1 } else { 0 };
        let rot = ((self.rot_x as u32) & 3)
            | (((self.rot_y as u32) & 3) << 2)
            | (((self.rot_z as u32) & 3) << 4);
        legacy
            | if self.absolute { 2 } else { 0 }
            | (code << 2)
            | (Self::pack_blend(self.blend) << 5)
            | if self.high_precision && self.raw_precision { TEXMAP_RAW_PRECISION_BIT } else { 0 }
            | if self.high_precision {
                TEXMAP_HIGH_PRECISION_BIT
            } else {
                0
            }
            | if self.green_dx { TEXMAP_GREEN_DX_BIT } else { 0 }
            | (rot << TEXMAP_ROT_SHIFT)
    }

    /// Unpack triplanar plane rotations from bits 12-17 → (rot_x, rot_y, rot_z) each 0..=3.
    #[inline]
    pub fn unpack_triplanar_rot(packed: u32) -> (u8, u8, u8) {
        let r = (packed >> TEXMAP_ROT_SHIFT) & 0x3F;
        ((r & 3) as u8, ((r >> 2) & 3) as u8, ((r >> 4) & 3) as u8)
    }

    /// True when the packed GPU word carries the DirectX-green normal flag (bit 11).
    #[inline]
    pub fn unpack_green_dx(packed: u32) -> bool {
        packed & TEXMAP_GREEN_DX_BIT != 0
    }

    /// True when the packed GPU word carries the 16-bit RG-pack flag (bit 10).
    #[inline]
    pub fn unpack_high_precision(packed: u32) -> bool {
        packed & TEXMAP_HIGH_PRECISION_BIT != 0
    }

    /// Quantise `blend` into 5 bits (bits 5-9 of the packed word).
    ///
    /// The GPU word is a single u32 bitcast through a `vec4<f32>` lane that is already full
    /// (x=slot, y=strength, z=tiling, w=this), so blend has no float lane of its own and rides
    /// the spare high bits instead. 0.25 steps over 0.25..=8.0 — finer than the control is worth
    /// (the visual difference between 4.0 and 4.25 is imperceptible) and the range the shader
    /// clamps to anyway.
    ///
    /// Zero is reserved to mean "not written" so a pre-blend save decodes to the 4.0 default
    /// rather than to the bottom of the range, which would blur every old material.
    ///
    /// 5 bits give codes 1..=31 once zero is reserved. The step is a flat 0.25 over
    /// 0.25..=7.75, chosen so the DEFAULT 4.0 is exactly representable — a grid that rounds the
    /// default to a neighbouring value would shift every existing material the first time its
    /// material was re-saved. The top of the requested range (8.0) is the one value that clips,
    /// to 7.75; that is the right trade, since 7.75 and 8.0 are visually identical while 4.0 vs
    /// 4.125 is the value everything already ships with.
    ///
    /// (Two earlier attempts failed the round-trip test and are worth not repeating: a flat 0.25
    /// step over 0.25..=8.0 needs code 32 and silently clipped to 7.75 without saying so; an
    /// endpoint-exact grid over the same range made the step 0.258 and moved the 4.0 default to
    /// 4.125.)
    #[inline]
    pub fn pack_blend(blend: f32) -> u32 {
        let q = (blend.clamp(BLEND_MIN, BLEND_MAX) / BLEND_STEP).round() as u32;
        q.clamp(1, 31)
    }

    /// Inverse of [`pack_blend`]; 0 (absent) decodes to the 4.0 default.
    #[inline]
    pub fn unpack_blend(packed: u32) -> f32 {
        let q = (packed >> 5) & 0x1F;
        if q == 0 { return 4.0; }
        q as f32 * BLEND_STEP
    }

    /// Unpack a GPU projection word into (`MapProjection` code, absolute).
    ///
    /// Reads the wide code from bits 2-4. A word written before those bits existed has them zero,
    /// so it decodes as `Box` unless the legacy bit0 says UV — which is what old saves meant.
    #[inline]
    pub fn unpack_projection(packed: u32) -> (u32, bool) {
        let wide = (packed >> 2) & 0x7;
        let code = if wide != 0 {
            wide
        } else if packed & 1 != 0 {
            MapProjection::Uv.as_u32()
        } else {
            MapProjection::Box.as_u32()
        };
        (code, (packed & 2) != 0)
    }
}

/// Which material channel a texture map feeds.
///
/// Color maps (Albedo, Emission) are sRGB; the rest are LINEAR data (the app pre-encodes them so
/// the sRGB texture array decodes back to raw values — see
/// `app::material_textures::decode_file_to_data_map`).
///
/// Wire tags are **append-only** (FL4MATX2) — never renumber existing variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    Albedo,
    Normal,
    Roughness,
    Metallic,
    Emission,
    Bump,
    Displacement,
    /// Subsurface-scattering color (sRGB color map, like Albedo/Emission). Appended
    /// 2026-08-20 — wire tag 7 (FL4MATX2 append-only rule).
    SssColor,
}

impl MapKind {
    pub const ALL: [MapKind; 8] = [
        MapKind::Albedo,
        MapKind::Normal,
        MapKind::Roughness,
        MapKind::Metallic,
        MapKind::Emission,
        MapKind::Bump,
        MapKind::Displacement,
        MapKind::SssColor,
    ];
    /// Non-albedo channels shown in the shared "Map Slots" UI (albedo has its own row).
    /// Also the FL4MATX2 persistence set (`project_save::collect_material_channels`).
    pub const DATA_SLOTS: [MapKind; 7] = [
        MapKind::Normal,
        MapKind::Roughness,
        MapKind::Metallic,
        MapKind::Emission,
        MapKind::Bump,
        MapKind::Displacement,
        MapKind::SssColor,
    ];
    /// Stable wire tag for the FL4MATX2 side-channel (append-only — never renumber).
    pub fn wire_tag(self) -> u32 {
        match self {
            MapKind::Albedo => 0,
            MapKind::Normal => 1,
            MapKind::Roughness => 2,
            MapKind::Metallic => 3,
            MapKind::Emission => 4,
            MapKind::Bump => 5,
            MapKind::Displacement => 6,
            MapKind::SssColor => 7,
        }
    }
    pub fn from_wire_tag(t: u32) -> Option<MapKind> {
        match t {
            0 => Some(MapKind::Albedo),
            1 => Some(MapKind::Normal),
            2 => Some(MapKind::Roughness),
            3 => Some(MapKind::Metallic),
            4 => Some(MapKind::Emission),
            5 => Some(MapKind::Bump),
            6 => Some(MapKind::Displacement),
            7 => Some(MapKind::SssColor),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MapKind::Albedo => "Albedo",
            MapKind::Normal => "Normal",
            MapKind::Roughness => "Roughness",
            MapKind::Metallic => "Metalness",
            MapKind::Emission => "Emission",
            MapKind::Bump => "Bump",
            MapKind::Displacement => "Displacement",
            MapKind::SssColor => "SSS Color",
        }
    }
    /// True for maps holding raw (non-color) data that must NOT be sRGB-decoded on sample.
    /// Emission and SSS Color are COLOR maps (like albedo). Bump/Displacement are height
    /// (linear grayscale).
    pub fn is_data(self) -> bool {
        !matches!(self, MapKind::Albedo | MapKind::Emission | MapKind::SssColor)
    }

    /// Default `TexMap.strength` when a map is first installed (library drop, file load, bake).
    ///
    /// **1.0 for every channel, including Bump and Normal.** An earlier pass shipped `Bump => 0.1`
    /// here to tame screen-space FD height gradients, but a smaller default is the wrong lever: it
    /// left the slider's usable range squeezed into its bottom tenth, so 1.0 still blew out and the
    /// number in the UI stopped meaning "full effect" (owner 2026-08-08).
    ///
    /// The gradient maps (Normal / Bump) are instead REMAPPED where a `TexMap` becomes GPU floats —
    /// `render::mesh_gpu::GRADIENT_MAP_STRENGTH_REMAP` (Bump uses the halved
    /// `BUMP_MAP_STRENGTH_REMAP`, owner 2026-08-09) — so authored `1.0` produces what `0.1` used
    /// to, the whole slider is usable, and every consumer (raster + path tracer) agrees. Keep this
    /// returning 1.0; put channel-specific scaling in the remap, not in the default.
    pub fn default_strength(self) -> f32 {
        1.0
    }
}

/// Per-material map projection. Matches `TexMap.projection`'s u32 encoding (0/1 today).
/// EXTENSION POINT: Planar / Sphere / Cylinder come later — append variants with new u32 codes,
/// never renumber (the code is serialized inside the .fl4 blob for albedo and in FL4MATX2 for the
/// other channels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapProjection {
    /// Box / triplanar: sample along the 3 body-local axes, blended by |normal|^blend.
    Box,
    /// Mesh UV coordinates (poly meshes only — SDF/voxel geometry has no UVs).
    Uv,
    /// Spherical: longitude/latitude around the body-local origin. Single-sample (no blend), so
    /// it pinches at the poles — correct for ball-like bodies, wrong for boxes.
    Sphere,
    /// Cylindrical: angle around +Y with height along Y. Single-sample; seams at the Y caps.
    Cylinder,
    /// Planar: one flat projection down body-local -Z (the XY plane). Single-sample; stretches
    /// badly on faces parallel to the projection axis — the classic "decal" mapping.
    Planar,
}

impl MapProjection {
    /// APPEND-ONLY. These codes are serialized inside the .fl4 blob (albedo) and in FL4MATX2 (the
    /// other channels), so an existing save re-read after renumbering would silently change every
    /// material's projection. New variants take the next free code; nothing is ever renumbered.
    pub fn as_u32(self) -> u32 {
        match self {
            MapProjection::Box => 0,
            MapProjection::Uv => 1,
            MapProjection::Sphere => 2,
            MapProjection::Cylinder => 3,
            MapProjection::Planar => 4,
        }
    }
    /// Unknown codes fall back to `Box`, so a project written by a NEWER build opens in an older
    /// one with a sane projection instead of a panic.
    pub fn from_u32(v: u32) -> MapProjection {
        match v {
            1 => MapProjection::Uv,
            2 => MapProjection::Sphere,
            3 => MapProjection::Cylinder,
            4 => MapProjection::Planar,
            _ => MapProjection::Box,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MapProjection::Box => "Box (triplanar)",
            MapProjection::Uv => "UV",
            MapProjection::Sphere => "Sphere",
            MapProjection::Cylinder => "Cylinder",
            MapProjection::Planar => "Planar",
        }
    }
    /// Every variant, in UI order — so a picker cannot silently omit a new one.
    pub const ALL: [MapProjection; 5] = [
        MapProjection::Box,
        MapProjection::Uv,
        MapProjection::Sphere,
        MapProjection::Cylinder,
        MapProjection::Planar,
    ];
    /// True when the projection blends multiple axis samples, i.e. when the triplanar `blend`
    /// sharpness control is meaningful. The single-sample projections ignore it.
    pub fn uses_blend(self) -> bool {
        matches!(self, MapProjection::Box)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialLayer {
    pub enabled: bool,
    pub color: Vec3,
    pub roughness: f32,
    pub metallic: f32,
    pub amount: f32,
    pub sharpness: f32,
}
impl Default for MaterialLayer {
    fn default() -> Self { Self { enabled:false, color:Vec3::new(0.5,0.5,0.5), roughness:0.5, metallic:0.0, amount:0.0, sharpness:2.0 } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseLayer {
    pub enabled: bool,
    pub color: Vec3,
    pub roughness: f32,
    pub metallic: f32,
    pub amount: f32,
    pub scale: f32,
    pub octaves: u32,
}
impl Default for NoiseLayer {
    fn default() -> Self { Self { enabled:false, color:Vec3::new(0.4,0.3,0.2), roughness:0.7, metallic:0.0, amount:0.0, scale:5.0, octaves:3 } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PbrMaterial {
    pub albedo: Vec3,
    pub roughness: f32,
    pub metallic: f32,
    pub opacity: f32,
    pub edge_wear: MaterialLayer,
    pub cavity_dirt: MaterialLayer,
    pub noise_pattern: NoiseLayer,
    // --- Phase B.1: Shader panel ---
    #[serde(default)]
    pub shader_type: ShaderType,
    #[serde(default)]
    pub mat_quality: MaterialQuality,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_one")]
    pub normal_strength: f32,
    #[serde(default)]
    pub emission_color: Vec3,
    #[serde(default)]
    pub emission_intensity: f32,
    // --- Advanced (UI in B.1, GPU in B.3) ---
    #[serde(default)]
    pub sss_radius: f32,
    #[serde(default = "default_sss_color")]
    pub sss_color: Vec3,
    #[serde(default = "default_ior")]
    pub ior: f32,
    #[serde(default)]
    pub glass_tint: Vec3,
    #[serde(default)]
    pub clearcoat: f32,
    #[serde(default = "default_cc_rough")]
    pub clearcoat_roughness: f32,
    #[serde(default)]
    pub anisotropy: f32,
    #[serde(default)]
    pub anisotropy_rotation: f32,
    #[serde(default)]
    pub sheen: f32,
    #[serde(default = "default_sheen_tint")]
    pub sheen_tint: f32,
    // --- Texture maps (additive, v6-safe) ---
    /// Albedo texture map. Multiplies the scalar `albedo` tint. `tex_id == 0` ⇒ pure scalar.
    #[serde(default)]
    pub albedo_map: TexMap,
    // --- Multi-map channels (2026-07-17) — RUNTIME-ONLY on the wire ---
    // `#[serde(skip)]`: these fields contribute ZERO bytes to bincode (serde skip = not
    // serialized at all), so the .fl4 blob stays byte-identical — the confirmed field trap
    // (a naked trailing field corrupts EVERY existing save) cannot fire. Authored values
    // round-trip through the self-delimiting `FL4MATX2` tail section (app/src/project.rs),
    // reattached by MaterialId on load — exactly mirroring how `child_orders` rides
    // `FL4ORDER`. Consequence: they persist for GRAPH materials only; embedded prim/body
    // materials cannot carry them across a save (by design — multi-map is a graph-material
    // feature, the owner's "Add Material" item).
    /// Tangent-space normal map. Perturbs the G-buffer normal (whiteout triplanar blend).
    #[serde(skip)]
    pub normal_map: TexMap,
    /// Roughness map (grayscale, linear). REPLACES the scalar roughness by `strength`.
    #[serde(skip)]
    pub roughness_map: TexMap,
    /// Metallic map (grayscale, linear). REPLACES the scalar metallic by `strength`.
    #[serde(skip)]
    pub metallic_map: TexMap,
    /// Emission color/intensity map (sRGB color). Multiplies `emission_color * emission_intensity`.
    #[serde(skip)]
    pub emission_map: TexMap,
    /// Bump / height map (grayscale, linear). Perturbs the G-buffer normal from height derivatives.
    #[serde(skip)]
    pub bump_map: TexMap,
    /// Displacement map (grayscale, linear). Offsets mesh vertices along the normal in the
    /// raster vertex shader (`strength` ≈ world-space height amplitude).
    #[serde(skip)]
    pub displacement_map: TexMap,
    /// Subsurface-scattering color map (sRGB color). REPLACES the scalar `sss_color` by
    /// `strength` (same replace semantic as roughness/metallic — the panel's layered bake
    /// already contains the full composite).
    ///
    /// **WIRE: `#[serde(skip)]` — never mid-blob** (the confirmed bincode field trap: any
    /// plainly-serialized or `serde(default)` trailing field corrupts EVERY existing .fl4).
    /// Persists via the `FL4MATX2` tail record with wire tag 7 (`MapKind::SssColor`),
    /// exactly like the other data channels; pre-feature saves carry no tag-7 record and
    /// load with a default (unset) TexMap.
    #[serde(skip)]
    pub sss_color_map: TexMap,
}
impl Default for PbrMaterial {
    fn default() -> Self {
        Self {
            albedo: Vec3::new(0.66,0.585,0.53), roughness:0.65, metallic:0.0, opacity:1.0,
            edge_wear: MaterialLayer::default(), cavity_dirt: MaterialLayer::default(), noise_pattern: NoiseLayer::default(),
            shader_type: ShaderType::StandardPBR, mat_quality: MaterialQuality::Normal, name: String::new(),
            normal_strength: 1.0, emission_color: Vec3::ZERO, emission_intensity: 0.0,
            sss_radius: 0.0, sss_color: Vec3::new(1.0, 0.2, 0.1), ior: 1.5, glass_tint: Vec3::ZERO,
            clearcoat: 0.0, clearcoat_roughness: 0.1, anisotropy: 0.0, anisotropy_rotation: 0.0,
            sheen: 0.0, sheen_tint: 0.5,
            albedo_map: TexMap::default(),
            normal_map: TexMap::default(),
            roughness_map: TexMap::default(),
            metallic_map: TexMap::default(),
            emission_map: TexMap::default(),
            bump_map: TexMap::default(),
            displacement_map: TexMap::default(),
            sss_color_map: TexMap::default(),
        }
    }
}
impl PbrMaterial {
    /// Channel accessor by kind (the ONE mapping — upload write-back, persistence and UI all use it).
    pub fn map(&self, kind: MapKind) -> &TexMap {
        match kind {
            MapKind::Albedo => &self.albedo_map,
            MapKind::Normal => &self.normal_map,
            MapKind::Roughness => &self.roughness_map,
            MapKind::Metallic => &self.metallic_map,
            MapKind::Emission => &self.emission_map,
            MapKind::Bump => &self.bump_map,
            MapKind::Displacement => &self.displacement_map,
            MapKind::SssColor => &self.sss_color_map,
        }
    }
    pub fn map_mut(&mut self, kind: MapKind) -> &mut TexMap {
        match kind {
            MapKind::Albedo => &mut self.albedo_map,
            MapKind::Normal => &mut self.normal_map,
            MapKind::Roughness => &mut self.roughness_map,
            MapKind::Metallic => &mut self.metallic_map,
            MapKind::Emission => &mut self.emission_map,
            MapKind::Bump => &mut self.bump_map,
            MapKind::Displacement => &mut self.displacement_map,
            MapKind::SssColor => &mut self.sss_color_map,
        }
    }
    pub fn preset(name: &str) -> Self {
        match name {
            "black_metal_rust" => Self {
                albedo:Vec3::new(0.02,0.02,0.03), roughness:0.25, metallic:0.95,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.55,0.22,0.08), roughness:0.85, metallic:0.15, amount:0.65, sharpness:3.0},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.01,0.005,0.002), roughness:0.95, metallic:0.0, amount:0.45, sharpness:2.5},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.4,0.18,0.06), roughness:0.75, metallic:0.1, amount:0.25, scale:6.0, octaves:4},
                ..Self::default()
            },
            "rusty_iron" => Self {
                albedo:Vec3::new(0.15,0.08,0.04), roughness:0.7, metallic:0.6,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.7,0.35,0.12), roughness:0.9, metallic:0.1, amount:0.7, sharpness:2.5},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.03,0.015,0.005), roughness:0.95, metallic:0.0, amount:0.5, sharpness:2.0},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.5,0.25,0.1), roughness:0.85, metallic:0.05, amount:0.4, scale:4.0, octaves:5},
                ..Self::default()
            },
            "worn_leather" => Self {
                albedo:Vec3::new(0.12,0.06,0.03), roughness:0.8,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.25,0.15,0.08), roughness:0.6, metallic:0.0, amount:0.5, sharpness:2.0},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.04,0.02,0.01), roughness:0.95, metallic:0.0, amount:0.3, sharpness:1.5},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.18,0.1,0.05), roughness:0.85, metallic:0.0, amount:0.35, scale:12.0, octaves:4},
                ..Self::default()
            },
            "alien_tech" => Self {
                albedo:Vec3::new(0.04,0.04,0.06), roughness:0.15, metallic:0.9,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.1,0.8,0.6), roughness:0.3, metallic:0.8, amount:0.5, sharpness:4.0},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.0,0.15,0.1), roughness:0.2, metallic:0.5, amount:0.4, sharpness:3.0},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.02,0.3,0.25), roughness:0.2, metallic:0.7, amount:0.15, scale:15.0, octaves:2},
                ..Self::default()
            },
            "rough_stone" => Self {
                albedo:Vec3::new(0.25,0.22,0.2), roughness:0.9,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.35,0.32,0.28), roughness:0.7, metallic:0.0, amount:0.4, sharpness:2.0},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.08,0.06,0.04), roughness:0.95, metallic:0.0, amount:0.5, sharpness:1.8},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.3,0.28,0.22), roughness:0.85, metallic:0.0, amount:0.4, scale:3.0, octaves:5},
                ..Self::default()
            },
            "aged_wood" => Self {
                albedo:Vec3::new(0.2,0.12,0.06), roughness:0.75,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.3,0.2,0.1), roughness:0.6, metallic:0.0, amount:0.35, sharpness:1.5},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.05,0.03,0.015), roughness:0.9, metallic:0.0, amount:0.4, sharpness:2.0},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.15,0.08,0.03), roughness:0.8, metallic:0.0, amount:0.3, scale:2.0, octaves:3},
                ..Self::default()
            },
            "chrome" => Self {
                albedo:Vec3::new(0.9,0.9,0.92), roughness:0.05, metallic:1.0,
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.02,0.02,0.02), roughness:0.3, metallic:0.5, amount:0.2, sharpness:3.0},
                ..Self::default()
            },
            "military_green" => Self {
                albedo:Vec3::new(0.08,0.1,0.05), roughness:0.6,
                edge_wear: MaterialLayer{enabled:true, color:Vec3::new(0.15,0.12,0.08), roughness:0.8, metallic:0.3, amount:0.55, sharpness:2.5},
                cavity_dirt: MaterialLayer{enabled:true, color:Vec3::new(0.02,0.015,0.01), roughness:0.9, metallic:0.0, amount:0.4, sharpness:2.0},
                noise_pattern: NoiseLayer{enabled:true, color:Vec3::new(0.06,0.08,0.04), roughness:0.7, metallic:0.0, amount:0.2, scale:8.0, octaves:3},
                ..Self::default()
            },
            // --- Phase B.1: New presets ---
            "glossy" => Self {
                albedo:Vec3::new(0.9,0.9,0.9), roughness:0.05, ..Self::default()
            },
            "matte" => Self {
                albedo:Vec3::new(0.7,0.7,0.7), roughness:0.95, ..Self::default()
            },
            "subsurface" => Self {
                albedo:Vec3::new(0.85,0.65,0.55), roughness:0.45,
                shader_type: ShaderType::Subsurface, sss_radius:0.3, sss_color:Vec3::new(1.0,0.2,0.1),
                ..Self::default()
            },
            "glass" => Self {
                albedo:Vec3::new(0.95,0.97,1.0), roughness:0.02, opacity:0.15,
                shader_type: ShaderType::Glass, ior:1.5,
                ..Self::default()
            },
            "fabric" => Self {
                albedo:Vec3::new(0.35,0.2,0.15), roughness:0.85, sheen:0.8, sheen_tint:0.5,
                ..Self::default()
            },
            "vfx_energy" => Self {
                albedo:Vec3::new(0.2,0.6,1.0), roughness:0.5, opacity:0.7,
                shader_type: ShaderType::VFX, emission_color:Vec3::new(0.2,0.6,1.0), emission_intensity:5.0,
                ..Self::default()
            },
            "gold" => Self {
                albedo:Vec3::new(1.0,0.84,0.0), roughness:0.25, metallic:1.0, ..Self::default()
            },
            "copper" => Self {
                albedo:Vec3::new(0.95,0.64,0.54), roughness:0.35, metallic:1.0, ..Self::default()
            },
            "emissive_neon" => Self {
                albedo:Vec3::new(0.0,1.0,0.4), roughness:0.5,
                shader_type: ShaderType::VFX, emission_color:Vec3::new(0.0,1.0,0.4), emission_intensity:10.0,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
    pub fn preset_names() -> &'static [(&'static str, &'static str)] {
        &[("clay","🏺 Clay"),("chrome","🪩 Chrome"),("black_metal_rust","🔩 Dark Metal"),
          ("rough_stone","🪨 Stone"),("aged_wood","🪵 Wood"),("military_green","🎖 Military"),
          ("glossy","✨ Glossy"),("matte","🫧 Matte"),("subsurface","🧴 Subsurface"),
          ("glass","🔮 Glass"),("fabric","🧶 Fabric"),("vfx_energy","⚡ VFX Energy"),
          ("gold","🥇 Gold"),("copper","🟤 Copper"),("emissive_neon","💡 Neon")]
    }
}


#[cfg(test)]
mod projection_pack_tests {
    use super::{MapProjection, TexMap, BLEND_MAX, BLEND_MIN, BLEND_STEP};

    /// Every projection must survive pack -> unpack, for both `absolute` states. This is the gate
    /// that catches a bit-field collision, which is silent at runtime (a material just renders
    /// with the wrong projection) and corrupts nothing, so nothing else would ever report it.
    #[test]
    fn every_projection_round_trips_through_the_packed_word() {
        for proj in MapProjection::ALL {
            for absolute in [false, true] {
                let mut m = TexMap::default();
                m.tex_id = 1;
                m.projection = proj.as_u32();
                m.absolute = absolute;
                let (code, abs_out) = TexMap::unpack_projection(m.projection_packed());
                assert_eq!(
                    TexMap::unpack_blend(m.projection_packed()), 4.0,
                    "{:?}: default blend must survive alongside the projection bits", proj);
                assert_eq!(
                    code,
                    proj.as_u32(),
                    "{:?} did not survive the packed word - bits 2-4 collided with something",
                    proj);
                assert_eq!(abs_out, absolute, "{:?}: absolute flag (bit1) was clobbered", proj);
            }
        }
    }

    /// A word written by the OLD encoder (bit0 + bit1 only, bits 2-4 zero) must still decode to
    /// what it meant then. Without this, every existing .fl4 silently switches projection.
    #[test]
    fn legacy_two_bit_words_still_decode() {
        assert_eq!(TexMap::unpack_projection(0b00), (MapProjection::Box.as_u32(), false));
        assert_eq!(TexMap::unpack_projection(0b01), (MapProjection::Uv.as_u32(), false));
        assert_eq!(TexMap::unpack_projection(0b10), (MapProjection::Box.as_u32(), true));
        assert_eq!(TexMap::unpack_projection(0b11), (MapProjection::Uv.as_u32(), true));
    }

    /// An OLD build reading a NEW save reads bit0 alone. It must see something sane: UV stays UV,
    /// and the three new projections degrade to Box rather than masquerading as UV.

    /// Blend must survive the packed word across its whole range, and must not disturb the
    /// projection or absolute bits sharing that word.
    #[test]
    fn blend_round_trips_without_disturbing_the_other_bits() {
        let mut q = BLEND_MIN;
        while q <= BLEND_MAX {
            for proj in MapProjection::ALL {
                let mut m = TexMap::default();
                m.tex_id = 1;
                m.projection = proj.as_u32();
                m.absolute = true;
                m.blend = q;
                let w = m.projection_packed();
                let back = TexMap::unpack_blend(w);
                let step = BLEND_STEP;
                assert!((back - q).abs() <= step * 0.5 + 1e-5,
                    "blend {q} came back as {back} (error must be within half a {step} step)");
                let (code, abs_out) = TexMap::unpack_projection(w);
                assert_eq!(code, proj.as_u32(), "blend bits leaked into the projection code");
                assert!(abs_out, "blend bits clobbered the absolute flag");
            }
            q += BLEND_STEP;
        }
    }

    /// A word written before blend existed has bits 5-9 zero and MUST decode to 4.0 — the value
    /// the shader hardcoded. Decoding it as 0.25 would silently blur every material in every old
    /// save, which no test other than this one would catch.

    /// Values above the storable maximum clip to it rather than wrapping. Wrapping a 5-bit field
    /// would turn a high blend into a very LOW one (sharpest -> blurriest), which is the kind of
    /// inversion a user would report as "the slider does the opposite at the end".
    #[test]
    fn out_of_range_blend_clips_instead_of_wrapping() {
        for v in [8.0f32, 12.0, 100.0, f32::INFINITY] {
            let code = TexMap::pack_blend(v);
            assert!(code <= 31, "blend {v} produced code {code}, which overflows 5 bits");
            assert_eq!(TexMap::unpack_blend(code << 5), BLEND_MAX,
                "blend {v} must clip to {BLEND_MAX}, not wrap");
        }
        for v in [0.0f32, -1.0, -100.0] {
            assert_eq!(TexMap::unpack_blend(TexMap::pack_blend(v) << 5), BLEND_MIN,
                "blend {v} must clip to {BLEND_MIN}");
        }
    }

    #[test]
    fn pre_blend_words_decode_to_the_legacy_default() {
        for legacy in [0b00u32, 0b01, 0b10, 0b11] {
            assert_eq!(TexMap::unpack_blend(legacy), 4.0,
                "legacy word {legacy:#04b} must decode to the previously-hardcoded blend");
        }
    }

    #[test]
    fn new_projections_degrade_to_box_for_old_readers() {
        for proj in MapProjection::ALL {
            let mut m = TexMap::default();
            m.tex_id = 1;
            m.projection = proj.as_u32();
            let legacy_bit = m.projection_packed() & 1;
            let expected = if proj == MapProjection::Uv { 1 } else { 0 };
            assert_eq!(
                legacy_bit, expected,
                "{:?}: an old build reads bit0 only and would render the wrong projection", proj);
        }
    }

    #[test]
    fn triplanar_rot_round_trips_without_disturbing_blend_or_proj() {
        for (rx, ry, rz) in [(0u8, 0, 0), (1, 2, 3), (3, 1, 2), (2, 0, 1)] {
            let mut m = TexMap::default();
            m.tex_id = 1;
            m.projection = MapProjection::Box.as_u32();
            m.blend = 4.0;
            m.rot_x = rx;
            m.rot_y = ry;
            m.rot_z = rz;
            let w = m.projection_packed();
            assert_eq!(TexMap::unpack_triplanar_rot(w), (rx, ry, rz));
            assert_eq!(TexMap::unpack_blend(w), 4.0);
            let (code, _) = TexMap::unpack_projection(w);
            assert_eq!(code, MapProjection::Box.as_u32());
            for (green_dx, raw_precision) in [(false, false), (true, false), (false, true), (true, true)] {
                m.green_dx = green_dx;
                m.high_precision = true;
                m.raw_precision = raw_precision;
                let packed = m.projection_packed();
                assert_eq!(TexMap::unpack_triplanar_rot(packed), (rx, ry, rz));
                assert_eq!(TexMap::unpack_green_dx(packed), green_dx);
                assert_eq!(packed & super::TEXMAP_RAW_PRECISION_BIT != 0, raw_precision);
                assert!(TexMap::unpack_high_precision(packed));
                assert_eq!(TexMap::unpack_blend(packed), 4.0);
            }
        }
        // Pre-rot words (bits 12-17 zero) decode as no rotation.
        assert_eq!(TexMap::unpack_triplanar_rot(0), (0, 0, 0));
    }
}
