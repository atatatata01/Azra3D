//! MaterialPanelDoc — the panel's authored-intent data model (spec §3, revised §9).
//!
//! Serialization: JSON only (rides the self-delimiting `FL4MPNLS` tail section in
//! `project.rs` as a JSON string per material — the FL4NODES precedent). NEVER put
//! these types in the bincode blob. All enums are APPEND-ONLY: never insert, reorder
//! or remove variants (JSON is name-tagged so this is about doc semantics + forward
//! tolerance, not positional bincode tags — but the discipline is kept identical).
//!
//! ## ChannelKind ↔ engine mapping (seed of docs/panel_node_mapping.md)
//!
//! | ChannelKind      | PbrMaterial scalar          | TexMap slot (`MapKind`)  | Master socket (`NodeGraph::MO_*`) |
//! |------------------|-----------------------------|--------------------------|-----------------------------------|
//! | BaseColor        | `albedo: Vec3`              | `albedo_map` (Albedo)    | MO_ALBEDO (0)                     |
//! | Roughness        | `roughness: f32`            | `roughness_map`          | MO_ROUGHNESS (1)                  |
//! | Metallic         | `metallic: f32`             | `metallic_map`           | MO_METALLIC (2)                   |
//! | Opacity          | `opacity: f32`              | —                        | MO_OPACITY (3)                    |
//! | Emission         | `emission_color`+`intensity`| `emission_map`           | MO_EMISSION (4) / MO_EMISSION_INTENSITY (12) |
//! | Normal           | `normal_strength: f32`      | `normal_map`             | MO_NORMAL (7) / MO_NORMAL_STRENGTH (13) |
//! | Bump             | — (map-only)                | `bump_map`               | MO_HEIGHT (6)                     |
//! | Displacement     | — (map-only)                | `displacement_map`       | MO_DISPLACEMENT (20)              |
//! | Clearcoat        | `clearcoat: f32`            | —                        | MO_CLEARCOAT (8)                  |
//! | ClearcoatRough   | `clearcoat_roughness: f32`  | —                        | MO_CLEARCOAT_ROUGH (9)            |
//! | Sheen            | `sheen: f32`                | —                        | MO_SHEEN (11)                     |
//! | SheenTint        | `sheen_tint: f32`           | —                        | MO_SHEEN_TINT (14)                |
//! | Anisotropy       | `anisotropy: f32`           | —                        | MO_ANISOTROPY (15)                |
//! | AnisotropyRot    | `anisotropy_rotation: f32`  | —                        | MO_ANISOTROPY_ROT (16)            |
//! | Ior              | `ior: f32`                  | —                        | MO_IOR (10)                       |
//! | SssRadius        | `sss_radius: f32`           | —                        | MO_SSS_RADIUS (17)                |
//! | SssColor         | `sss_color: Vec3`           | `sss_color_map`          | MO_SSS_COLOR (18)                 |
//! | GlassTint        | `glass_tint: Vec3`          | —                        | MO_GLASS_TINT (19)                |
//!
//! **There is NO Specular channel**: `PbrMaterial` has no specular field and the
//! master material has no specular socket. Per C1 ("a control with no node
//! equivalent does not belong in the panel") the D1 "specular?" is answered NO —
//! it is excluded rather than invented. Escalate if the owner wants a real
//! specular lobe added to the engine first.

use serde::{Deserialize, Serialize};

/// One material's panel document. `shader_type` mirrors (does not replace)
/// `PbrMaterial::shader_type` — §9: ShaderType IS the shader model; the doc keeps a
/// copy only so the panel can render without chasing the graph material.
fn default_doc_bake_res() -> u32 {
    256
}

/// Legal bake-target steps shown in the panel (matches node editor Map res).
/// M4 / D3: product ceiling is **2K** — 4096 is not offered until `AZRA_ALLOW_4K_MAPS=1`
/// (see [`crate::material_textures::product_map_ceiling`]). Soft steps (≤1024) still
/// install at least [`crate::material_textures::DEFAULT_MAP_RES`] via `final_install_res`.
pub(crate) const BAKE_TARGET_STEPS: [u32; 5] = [128, 256, 512, 1024, 2048];
/// Extended steps when 4K is explicitly allowed (env).
pub(crate) const BAKE_TARGET_STEPS_4K: [u32; 6] = [128, 256, 512, 1024, 2048, 4096];

/// Snap a panel bake target to a legal step (≤ product ceiling).
pub(crate) fn snap_panel_bake_res(raw: u32) -> u32 {
    let raw = if raw == 0 { default_doc_bake_res() } else { raw };
    let steps: &[u32] = if crate::material_textures::product_map_ceiling()
        >= crate::material_textures::MAX_MAP_RES
    {
        &BAKE_TARGET_STEPS_4K
    } else {
        &BAKE_TARGET_STEPS
    };
    let mut best = steps[0];
    let mut best_d = u32::MAX;
    for &s in steps {
        let d = raw.abs_diff(s);
        if d < best_d {
            best_d = d;
            best = s;
        }
    }
    best.min(crate::material_textures::product_map_ceiling())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialPanelDoc {
    /// Selected shader model — `ShaderType::as_u32()` ordinal (append-only codes).
    /// Stored as the stable u32 wire code, not the enum, so the doc JSON never
    /// depends on core_math enum serde shape.
    #[serde(default)]
    pub shader_type: u32,
    /// Material-level texture space; all maps inherit unless a layer overrides.
    #[serde(default)]
    pub texture_space: TextureSpace,
    /// Ordered, active channel set (spec §2: one row per channel).
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// Map **bake target** resolution (working res; install ceiling via `final_install_res`).
    /// Mirrored onto `NodeGraph::bake_res` on lower. 128…1024 soft→1024 install; 2048 = product
    /// ceiling (M4/D3). 4096 only with `AZRA_ALLOW_4K_MAPS=1`.
    /// Owner: choose bake quality from the panel (not only Master Material node).
    #[serde(default = "default_doc_bake_res")]
    pub bake_res: u32,
    /// Next free [`Layer::id`] (MATERIAL_COMPOSITE_ADR P1 — stable IDs, not indices).
    /// `serde(default)` = 1 so old docs load and get ids via [`Self::ensure_layer_ids`].
    #[serde(default = "default_next_layer_id")]
    pub next_layer_id: u32,
    /// Where generator layers (Curvature / Cavity) get their data — [`GeneratorMode`]'s wire code.
    ///
    /// Safe to append because a panel doc persists as **JSON** inside `MaterialPanelRecord`
    /// (`doc_json: String`), not as raw bincode — so `serde(default)` genuinely defaults a missing
    /// field instead of misreading the next one. That is why every field here carries it.
    #[serde(default)]
    pub generator_mode: u32,
}

/// How a material's generator layers obtain their signal.
///
/// **Performance is the deciding difference, and it is large.** Baked sampling is a single texture
/// fetch per texel. Live evaluation rebuilds a `res²` curvature grid for every distinct knob set
/// on every bake, and when the grid cache misses it falls back to the analytical path at roughly
/// 500 field evaluations per pixel — the measured cause of the owner's original "~45 s to add a
/// Generator layer".
///
/// Both are kept because they answer different questions, not because the choice is unclear:
///
/// * **SDF bodies have no UVs and change shape constantly.** There is nothing stable to bake
///   against — a bake would be invalidated by the next prim drag — so Live is the only correct
///   mode there, and its cost buys correctness.
/// * **A poly asset with UVs is stable and is what gets textured.** Its maps are baked once and
///   reused across every material edit, which is the whole Substance model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneratorMode {
    /// Baked when this body has UVs and baked mesh maps; Live otherwise. The default, and what
    /// every pre-existing document loads as (wire code 0).
    Auto,
    /// Always evaluate the SDF field. Correct for SDF bodies; expensive for poly.
    Live,
    /// Always sample the baked mesh maps. Requires a bake; fastest by a wide margin.
    Baked,
}

impl GeneratorMode {
    /// Append-only wire codes — this is persisted state.
    pub fn as_u32(self) -> u32 {
        match self {
            GeneratorMode::Auto => 0,
            GeneratorMode::Live => 1,
            GeneratorMode::Baked => 2,
        }
    }

    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => GeneratorMode::Live,
            2 => GeneratorMode::Baked,
            _ => GeneratorMode::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GeneratorMode::Auto => "Auto",
            GeneratorMode::Live => "Live (SDF)",
            GeneratorMode::Baked => "Baked (mesh maps)",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            GeneratorMode::Auto => {
                "Baked when the body has UVs and baked mesh maps, Live otherwise."
            }
            GeneratorMode::Live => {
                "Evaluate the SDF field every bake. Correct for SDF bodies, which have no UVs and \
                 change shape constantly so there is nothing stable to bake. Expensive on poly."
            }
            GeneratorMode::Baked => {
                "Sample the baked mesh maps — one texture fetch instead of rebuilding a curvature \
                 grid per bake. Needs Bake Mesh Maps first."
            }
        }
    }
}

fn default_next_layer_id() -> u32 {
    1
}

impl Default for MaterialPanelDoc {
    fn default() -> Self {
        Self {
            shader_type: 0,
            texture_space: TextureSpace::default(),
            channels: Vec::new(),
            bake_res: default_doc_bake_res(),
            next_layer_id: default_next_layer_id(),
            generator_mode: 0,
        }
    }
}

impl MaterialPanelDoc {
    /// D1 (owner): a NEW material opens with the MAIN channel set — base color,
    /// roughness, metalness, normal, displacement (matching the mockup; no
    /// specular — see the mapping table header). Values seeded from
    /// `PbrMaterial::default()` so the panel and the render agree from frame one.
    pub fn default_for_new() -> Self {
        let m = core_math::pbr_material::PbrMaterial::default();
        Self {
            shader_type: m.shader_type.as_u32(),
            texture_space: TextureSpace::default(),
            bake_res: default_doc_bake_res(),
            next_layer_id: default_next_layer_id(),
            generator_mode: 0,
            channels: vec![
                Channel::value(ChannelKind::BaseColor, ChannelValue::Color([m.albedo.x, m.albedo.y, m.albedo.z])),
                Channel::value(ChannelKind::Roughness, ChannelValue::Scalar(m.roughness)),
                Channel::value(ChannelKind::Metallic, ChannelValue::Scalar(m.metallic)),
                Channel::value(ChannelKind::Normal, ChannelValue::Scalar(m.normal_strength)),
                Channel::value(ChannelKind::Displacement, ChannelValue::Scalar(0.0)),
            ],
        }
    }

    pub fn channel(&self, kind: ChannelKind) -> Option<&Channel> {
        self.channels.iter().find(|c| c.kind == kind)
    }

    /// Allocate a never-reused layer id (prim_uid pattern). Call when pushing a new layer.
    pub fn alloc_layer_id(&mut self) -> u32 {
        self.ensure_layer_ids();
        let id = self.next_layer_id.max(1);
        self.next_layer_id = id.saturating_add(1);
        id
    }

    /// Assign stable ids to any layer / mask-layer still at 0 (old JSON / fixtures).
    /// Bumps `next_layer_id` past every existing id. Idempotent.
    pub fn ensure_layer_ids(&mut self) {
        let mut hi = self.next_layer_id.max(1);
        fn scan_mask(stack: &MaskStack, hi: &mut u32) {
            for ml in &stack.layers {
                if ml.id != 0 {
                    *hi = (*hi).max(ml.id.saturating_add(1));
                }
                scan_mask(&ml.mask, hi);
            }
        }
        for ch in &self.channels {
            for layer in &ch.layers {
                if layer.id != 0 {
                    hi = hi.max(layer.id.saturating_add(1));
                }
                scan_mask(&layer.mask, &mut hi);
            }
        }
        self.next_layer_id = hi;
        fn stamp_mask(stack: &mut MaskStack, next: &mut u32) {
            for ml in &mut stack.layers {
                if ml.id == 0 {
                    ml.id = *next;
                    *next = next.saturating_add(1);
                }
                stamp_mask(&mut ml.mask, next);
            }
        }
        for ch in &mut self.channels {
            for layer in &mut ch.layers {
                if layer.id == 0 {
                    layer.id = self.next_layer_id;
                    self.next_layer_id = self.next_layer_id.saturating_add(1);
                }
                stamp_mask(&mut layer.mask, &mut self.next_layer_id);
            }
        }
    }

    /// Stamp a new layer with a fresh stable id and return it.
    pub fn stamp_layer(&mut self, mut layer: Layer) -> Layer {
        if layer.id == 0 {
            layer.id = self.alloc_layer_id();
        }
        layer
    }
}

/// Graph socket that drives a panel parameter (MATERIAL_COMPOSITE_ADR Decision 2 = C).
///
/// Stored on the layer (not inside every content field) so Const values stay plain
/// `f32` for sliders/JSON. When `Some`, regen **connects** that socket to the
/// named parameter and does not overwrite it from the panel value.
///
/// `param` is a stable string key: `"radius"`, `"scale"`, `"octaves"`, …
/// Multiple drives per layer live in [`Layer::drives`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ParamDrive {
    /// Parameter name on this layer's content (`"radius"`, `"scale"`, …).
    pub param: String,
    /// Source node id in the material `NodeGraph`.
    pub node: u32,
    /// Output socket index on that node.
    pub output: u32,
}

/// Panel channel identity. APPEND-ONLY — never insert/reorder/remove variants.
/// See the module-header mapping table for the engine binding of each kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ChannelKind {
    BaseColor,
    Roughness,
    Metallic,
    Opacity,
    Emission,
    Normal,
    Bump,
    Displacement,
    Clearcoat,
    ClearcoatRoughness,
    Sheen,
    SheenTint,
    Anisotropy,
    AnisotropyRotation,
    Ior,
    SssRadius,
    SssColor,
    GlassTint,
    // APPEND new kinds here only.
}

impl ChannelKind {
    /// The TexMap slot this channel binds to, when one exists.
    pub fn map_kind(self) -> Option<core_math::pbr_material::MapKind> {
        use core_math::pbr_material::MapKind;
        match self {
            ChannelKind::BaseColor => Some(MapKind::Albedo),
            ChannelKind::Roughness => Some(MapKind::Roughness),
            ChannelKind::Metallic => Some(MapKind::Metallic),
            ChannelKind::Emission => Some(MapKind::Emission),
            ChannelKind::Normal => Some(MapKind::Normal),
            ChannelKind::Bump => Some(MapKind::Bump),
            ChannelKind::Displacement => Some(MapKind::Displacement),
            ChannelKind::SssColor => Some(MapKind::SssColor),
            _ => None,
        }
    }

    /// The inverse of [`map_kind`](Self::map_kind).
    pub fn from_map_kind(k: core_math::pbr_material::MapKind) -> ChannelKind {
        use core_math::pbr_material::MapKind;
        match k {
            MapKind::Albedo => ChannelKind::BaseColor,
            MapKind::Roughness => ChannelKind::Roughness,
            MapKind::Metallic => ChannelKind::Metallic,
            MapKind::Emission => ChannelKind::Emission,
            MapKind::Normal => ChannelKind::Normal,
            MapKind::Bump => ChannelKind::Bump,
            MapKind::Displacement => ChannelKind::Displacement,
            MapKind::SssColor => ChannelKind::SssColor,
        }
    }
}

/// Plain value carried by a `ChannelState::Value` channel (spec §3 "or Color, per
/// channel kind").
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum ChannelValue {
    Scalar(f32),
    Color([f32; 3]),
    // APPEND only.
}

/// Spec §3 `ChannelState`. `GraphDriven` (C1): the channel is owned by a wire into
/// the master material socket — the panel shows a badge, never a lying slider.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum ChannelState {
    Value(ChannelValue),
    Layered,
    GraphDriven {
        /// The master-material output node id in the owning `NodeGraph`
        /// (`shader_nodes::NodeId` = u32). Click-through target for P2/P8.
        node: u32,
    },
    // APPEND only.
}

/// Bake / map storage precision per channel (authoring intent).
/// Displacement defaults to 16-bit — height steps need headroom that 8-bit banding kills.
/// Color/roughness usually stay 8-bit for VRAM. APPEND-ONLY ordinals.
///
/// Available in **both** Live and Bake modes (owner 2026-07-29): Live stores intent and
/// applies it when maps install; Bake re-encodes dual-plane on toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum ChannelBitDepth {
    #[default]
    Bit8 = 0,
    Bit16 = 1,
    // APPEND only.
}

impl ChannelBitDepth {
    pub fn label(self) -> &'static str {
        match self {
            ChannelBitDepth::Bit8 => "8-bit",
            ChannelBitDepth::Bit16 => "16-bit",
        }
    }
    pub fn toggle(self) -> Self {
        match self {
            ChannelBitDepth::Bit8 => ChannelBitDepth::Bit16,
            ChannelBitDepth::Bit16 => ChannelBitDepth::Bit8,
        }
    }
    /// Sensible default for a brand-new channel of this kind.
    pub fn default_for(kind: ChannelKind) -> Self {
        match kind {
            // Height / displacement: 16-bit is the useful path (owner).
            ChannelKind::Displacement | ChannelKind::Bump => ChannelBitDepth::Bit16,
            _ => ChannelBitDepth::Bit8,
        }
    }
}

/// User choice: evaluate this channel LIVE (GPU, no map bake when possible) or BAKE
/// (layer stack → texture array). Owner: not auto-inferred from stack shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum ChannelEvalMode {
    /// Prefer GPU live path when the stack allows; otherwise falls back to bake.
    Live = 0,
    /// Always composite through the map bake pipeline (default for layered stacks).
    #[default]
    Bake = 1,
    // APPEND only.
}

impl ChannelEvalMode {
    pub fn label(self) -> &'static str {
        match self {
            ChannelEvalMode::Live => "Live",
            ChannelEvalMode::Bake => "Bake",
        }
    }
    pub fn toggle(self) -> Self {
        match self {
            ChannelEvalMode::Live => ChannelEvalMode::Bake,
            ChannelEvalMode::Bake => ChannelEvalMode::Live,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Channel {
    pub kind: ChannelKind,
    pub state: ChannelState,
    /// Empty unless `state == Layered` (spec §3: empty = plain value).
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// Per-channel bake precision (8 vs 16). serde-default = Bit8 so old docs load.
    /// Displacement/Bump new channels default Bit16 via [`Channel::value`] / Add channel.
    /// Shown in **both** Live and Bake modes.
    #[serde(default)]
    pub bit_depth: ChannelBitDepth,
    /// Live vs Bake — **user choice**, not inferred. Default Bake (serde) so old layered
    /// docs keep map install behaviour. Live only when the user clicks Live.
    #[serde(default)]
    pub eval_mode: ChannelEvalMode,
    /// Eye toggle — TEMPORARILY mute this channel (owner 2026-08-06: "I need an eye to
    /// click, disable temporary a channel").
    ///
    /// Non-destructive by construction: the layers, images and every parameter stay in
    /// the doc, so re-enabling restores the channel with no reload. Lowering treats a
    /// disabled channel exactly like one with no visible layers — the map slot is
    /// cleared so the shader falls back to the material's scalar.
    ///
    /// `serde(default = "yes")` — every existing FL4MPNLS doc loads as enabled.
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Channel {
    pub fn value(kind: ChannelKind, v: ChannelValue) -> Self {
        Self {
            kind,
            state: ChannelState::Value(v),
            layers: Vec::new(),
            bit_depth: ChannelBitDepth::default_for(kind),
            eval_mode: ChannelEvalMode::Bake,
            enabled: true,
        }
    }

    /// Layered / GraphDriven constructor with kind-default bit depth.
    pub fn with_state(kind: ChannelKind, state: ChannelState, layers: Vec<Layer>) -> Self {
        Self {
            kind,
            state,
            layers,
            bit_depth: ChannelBitDepth::default_for(kind),
            eval_mode: ChannelEvalMode::Bake,
            enabled: true,
        }
    }
}

/// Max nesting depth for mask-on-mask (HANDOFF P3). Prevents runaway recursion.
pub(crate) const MASK_STACK_MAX_DEPTH: usize = 4;

/// One entry in a [`MaskStack`]. Can itself carry a nested mask stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaskLayer {
    #[serde(default)]
    pub id: u32,
    pub content: LayerContent,
    /// How this mask entry combines with the mask below (default Multiply).
    #[serde(default = "mask_blend_default")]
    pub blend: BlendMode,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Nested mask stack on this entry (mask-the-mask).
    #[serde(default, deserialize_with = "deserialize_mask_stack")]
    pub mask: MaskStack,
    #[serde(default)]
    pub drives: Vec<ParamDrive>,
}

fn mask_blend_default() -> BlendMode {
    BlendMode::Multiply
}

impl MaskLayer {
    pub fn from_content(content: LayerContent) -> Self {
        Self {
            id: 0,
            content,
            blend: BlendMode::Multiply,
            opacity: 1.0,
            visible: true,
            mask: MaskStack::default(),
            drives: Vec::new(),
        }
    }
    pub fn drive(&self, param: &str) -> Option<&ParamDrive> {
        self.drives.iter().find(|d| d.param == param)
    }
}

/// Ordered mask stack on a content layer (or nested on a mask entry).
/// Empty = no mask. Bottom → top compositing with each entry's blend/opacity.
///
/// JSON backcompat: a bare `LayerContent` object still deserializes as a one-entry stack.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MaskStack {
    #[serde(default)]
    pub layers: Vec<MaskLayer>,
}

impl MaskStack {
    pub fn empty() -> Self {
        Self { layers: Vec::new() }
    }
    pub fn single(content: LayerContent) -> Self {
        Self {
            layers: vec![MaskLayer::from_content(content)],
        }
    }
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty() || self.layers.iter().all(|l| !l.visible)
    }
    /// First visible entry's content (legacy "single mask" view).
    pub fn primary_content(&self) -> Option<&LayerContent> {
        self.layers.iter().find(|l| l.visible).map(|l| &l.content)
    }
    pub fn primary_content_mut(&mut self) -> Option<&mut LayerContent> {
        self.layers.iter_mut().find(|l| l.visible).map(|l| &mut l.content)
    }
    pub fn set_single(&mut self, content: LayerContent) {
        *self = Self::single(content);
    }
    pub fn clear(&mut self) {
        self.layers.clear();
    }
}

impl<'de> serde::Deserialize<'de> for MaskStack {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_mask_stack(d)
    }
}

fn deserialize_mask_stack<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<MaskStack, D::Error> {
    let v = serde_json::Value::deserialize(d).map_err(serde::de::Error::custom)?;
    Ok(mask_stack_from_value(v))
}

fn mask_stack_from_value(v: serde_json::Value) -> MaskStack {
    match v {
        serde_json::Value::Null => MaskStack::empty(),
        serde_json::Value::Object(ref m) if m.contains_key("layers") => {
            // New shape: { "layers": [ MaskLayer, ... ] }
            #[derive(Deserialize)]
            struct Wire {
                #[serde(default)]
                layers: Vec<MaskLayerWire>,
            }
            #[derive(Deserialize)]
            struct MaskLayerWire {
                #[serde(default)]
                id: u32,
                content: LayerContent,
                #[serde(default = "mask_blend_default")]
                blend: BlendMode,
                #[serde(default = "one")]
                opacity: f32,
                #[serde(default = "yes")]
                visible: bool,
                #[serde(default)]
                mask: serde_json::Value,
                #[serde(default)]
                drives: Vec<ParamDrive>,
            }
            let Ok(w) = serde_json::from_value::<Wire>(v) else {
                return MaskStack::empty();
            };
            MaskStack {
                layers: w
                    .layers
                    .into_iter()
                    .map(|ml| MaskLayer {
                        id: ml.id,
                        content: ml.content,
                        blend: ml.blend,
                        opacity: ml.opacity,
                        visible: ml.visible,
                        mask: mask_stack_from_value(ml.mask),
                        drives: ml.drives,
                    })
                    .collect(),
            }
        }
        serde_json::Value::Object(_) => {
            // Legacy: bare LayerContent.
            if let Ok(c) = serde_json::from_value::<LayerContent>(v) {
                MaskStack::single(c)
            } else {
                MaskStack::empty()
            }
        }
        serde_json::Value::Array(arr) => {
            let mut layers = Vec::new();
            for item in arr {
                if let Ok(c) = serde_json::from_value::<LayerContent>(item.clone()) {
                    layers.push(MaskLayer::from_content(c));
                } else if let Ok(ml) = serde_json::from_value::<MaskLayer>(item) {
                    layers.push(ml);
                }
            }
            MaskStack { layers }
        }
        _ => MaskStack::empty(),
    }
}

/// Spec §2 layer model: content + mask stack + blend + opacity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Layer {
    /// Stable identity (MATERIAL_COMPOSITE_ADR P1). Survives reorder / insert;
    /// panel graph tags use this instead of stack index. 0 = unassigned (legacy
    /// JSON) — filled by [`MaterialPanelDoc::ensure_layer_ids`].
    #[serde(default)]
    pub id: u32,
    pub content: LayerContent,
    /// Mask stack (P3 recursive). Empty = no mask. Legacy JSON bare `LayerContent`
    /// still loads as a one-entry stack.
    #[serde(default, deserialize_with = "deserialize_mask_stack")]
    pub mask: MaskStack,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Per-layer projection override (owner §8: ADVANCED option per image layer).
    /// `space: None` = inherit the material `texture_space`. Generators must keep
    /// this at inherit — they have no meaningful projection (spec §2 rule).
    #[serde(default)]
    pub placement: Placement,
    /// P3 image-adjust block (spec §2: flat inside the layer, 2-level max).
    /// Maps 1:1 onto a `ColorCorrect` node (+ `Invert`) when non-default.
    #[serde(default)]
    pub adjust: Adjust,
    /// Per-parameter drives (ADR Decision 2). Empty = every param is Const(panel f32).
    /// `serde(default)` so pre-P1 docs load with no drives.
    #[serde(default)]
    pub drives: Vec<ParamDrive>,
}

impl Layer {
    /// Look up a drive for `param` (e.g. `"radius"`, `"scale"`).
    pub fn drive(&self, param: &str) -> Option<&ParamDrive> {
        self.drives.iter().find(|d| d.param == param)
    }
}

/// Image-adjust controls (spec §2 Image content: contrast, saturation, gamma,
/// invert; opacity is the layer field). Each field is EXACTLY one `ColorCorrect`
/// input default (see docs/panel_node_mapping.md P3): saturation → input 2,
/// gamma → input 4, contrast → input 6; invert → an `Invert` node.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Adjust {
    #[serde(default)]
    pub contrast: f32,
    #[serde(default = "one")]
    pub saturation: f32,
    #[serde(default = "one")]
    pub gamma: f32,
    #[serde(default)]
    pub invert: bool,
}

impl Default for Adjust {
    fn default() -> Self {
        Self { contrast: 0.0, saturation: 1.0, gamma: 1.0, invert: false }
    }
}

impl Adjust {
    pub fn is_default(&self) -> bool {
        *self == Adjust::default()
    }
}

fn one() -> f32 { 1.0 }
fn yes() -> bool { true }

/// Layer content (spec §2 table). APPEND only — never insert/reorder/remove.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum LayerContent {
    Image {
        /// 1-based slot into the material texture array (`TexMap::tex_id` domain).
        /// 0 = not yet assigned.
        tex_id: u32,
        /// Map influence 0..1 (mirrors `TexMap::strength`).
        strength: f32,
    },
    Generator {
        source: GeneratorSource,
        /// World-unit radius (C4 contract). Lowers onto the generator node's
        /// `Epsilon` input (SdfCurvature / Cavity). Drive via [`Layer::drives`]
        /// key `"radius"` (ADR Decision 2).
        radius: f32,
        invert: bool,
        /// P6: which curvature output feeds the channel (spec P6 "ship all
        /// three"). Ignored by sources with a single fixed output (Cavity).
        #[serde(default)]
        output: GeneratorOutput,
        /// P6 range remap: input `[min..max]` maps to 0..1 (Remap node,
        /// clamped). `[0,1]` = identity. On the Vertex path this is applied in
        /// the G-buffer (not the node graph).
        #[serde(default = "unit_range")]
        remap: [f32; 2],
        /// P7 bake tier (spec §6.5 ladder, C3) — per-GENERATOR, serde-default
        /// so pre-P7 docs parse unchanged. This is display-tier METADATA only:
        /// it never changes what the generator computes, only where the result
        /// lives. No cache payload is ever serialized here (C3: bake is a
        /// cache, the doc is the definition).
        #[serde(default)]
        rung: GeneratorRung,
        /// Vertex-path Soft/Hard falloff (power curve, endpoints 0 and 1 fixed):
        /// 1.0 = linear, >1 harder edge-only bands, <1 softer / blur-like falloff.
        /// Does NOT change peak intensity — use layer `opacity` for that.
        /// Default 1.0; serde-default for old docs.
        #[serde(default = "one")]
        sharpness: f32,
        /// P8: axis for `GeneratorSource::Direction`. Ignored by every other source (they have
        /// no axis), so it is stored unconditionally rather than as an `Option` — same shape as
        /// `output`, which Cavity also ignores. serde-default so pre-P8 docs parse to `+Y`.
        #[serde(default)]
        axis: DirectionAxis,
    },
    Constant {
        value: ChannelValue,
    },
    /// Procedural noise layer — lowers to a `NodeType::Noise` in the channel subgraph.
    /// APPEND-ONLY (was reserved as "Procedural" in the v1 plan; shipped as Noise).
    Noise {
        /// Noise scale (feeds Noise node Scale input; default 4.0).
        /// Drive via [`Layer::drives`] key `"scale"`.
        #[serde(default = "noise_scale_default")]
        scale: f32,
        /// Layer strength 0..1 (Mask input on the Noise node).
        #[serde(default = "one")]
        strength: f32,
        /// Octaves (Noise node Octaves input).
        #[serde(default = "noise_octaves_default")]
        octaves: f32,
        /// Noise algorithm (FBM / Perlin / Cellular / …). Default FBM; old docs omit → FBM.
        #[serde(default)]
        noise_type: crate::shader_nodes::NoiseType,
    },
    // APPEND only.
}

fn noise_scale_default() -> f32 { 4.0 }
fn noise_octaves_default() -> f32 { 4.0 }

/// C4 generator sources (spec §2). P0 declares them; evaluation is P6.
///
/// P6 backend reality (verified in `node_graph_bake::eval_node`, spec §10):
/// - `Curvature` → `NodeType::SdfCurvature` (uid 0 = whole body field): the only
///   source with a real signed/convex/concave split. SDF bodies only.
/// - `Cavity` → `NodeType::Cavity` (concave-only sugar). SDF bodies only.
/// - `AmbientOcclusion` → `NodeType::AmbientOcclusion` (P5: real SDF AO).
/// - `Thickness` → `NodeType::SdfThickness` (interior distance along −normal).
/// - `Position` → `NodeType::SdfPosition` (AABB-normalized body-local colour).
/// - `NormalDirection` has NO working bake backend (superseded by `Direction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum GeneratorSource {
    Curvature,
    AmbientOcclusion,
    Thickness,
    Position,
    NormalDirection,
    Cavity,
    /// P8: angle mask from the surface normal vs a chosen world axis — "snow on top",
    /// "dirt underneath", directional wear. Lowers to `NodeType::SurfaceDirection`, whose
    /// `sdf_curv_region` carries the axis code (see [`DirectionAxis`]).
    ///
    /// Unlike `NormalDirection` (declared in the v1 plan, never implemented, no backend), this
    /// one has a real bake backend: the CPU arm recovers the true surface normal from the body
    /// SDF gradient — it does NOT read the evaluator's `n`, which is a constant on the planar
    /// bake domain. SDF bodies only (same requirement as Curvature/Cavity).
    Direction,
    // APPEND only.
}

/// Axis a [`GeneratorSource::Direction`] layer measures against. The ordinal IS the wire code
/// stored in `Node.sdf_curv_region` (see `shader_nodes::nodes::surface_direction::axis_vec`),
/// so these must stay in lockstep with that function. APPEND-ONLY.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DirectionAxis {
    PosX,
    NegX,
    /// "Top" — the default and by far the most common authoring intent.
    #[default]
    PosY,
    NegY,
    PosZ,
    NegZ,
    // APPEND only.
}

impl DirectionAxis {
    /// Wire code stored in `Node.sdf_curv_region`.
    pub fn as_u32(self) -> u32 {
        match self {
            DirectionAxis::PosX => 0,
            DirectionAxis::NegX => 1,
            DirectionAxis::PosY => 2,
            DirectionAxis::NegY => 3,
            DirectionAxis::PosZ => 4,
            DirectionAxis::NegZ => 5,
        }
    }

    /// Inverse of [`as_u32`](Self::as_u32). Unknown codes fall back to `PosY` — a doc written by
    /// a newer build with an axis this one lacks reads as "top" rather than failing to parse.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => DirectionAxis::PosX,
            1 => DirectionAxis::NegX,
            3 => DirectionAxis::NegY,
            4 => DirectionAxis::PosZ,
            5 => DirectionAxis::NegZ,
            _ => DirectionAxis::PosY,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DirectionAxis::PosX => "+X",
            DirectionAxis::NegX => "-X",
            DirectionAxis::PosY => "+Y (top)",
            DirectionAxis::NegY => "-Y (bottom)",
            DirectionAxis::PosZ => "+Z",
            DirectionAxis::NegZ => "-Z",
        }
    }

    pub const ALL: [DirectionAxis; 6] = [
        DirectionAxis::PosY,
        DirectionAxis::NegY,
        DirectionAxis::PosX,
        DirectionAxis::NegX,
        DirectionAxis::PosZ,
        DirectionAxis::NegZ,
    ];
}

/// P6 curvature output selection (spec P6: signed / convex-only / concave-only,
/// ship all three). Signed is normalised so 0.5 = flat (Remap −1..1 → 0..1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum GeneratorOutput {
    #[default]
    Signed,
    ConvexOnly,
    ConcaveOnly,
    // APPEND only.
}

fn unit_range() -> [f32; 2] { [0.0, 1.0] }

/// Sampling-domain choice for a generator layer (CURVATURE_BOTH_MODES.md).
///
/// **Not a compositor choice.** ADR-001 Decision 1 says the bake composites every
/// layer; both rungs still lower into the map so Hybrid and PT sample the same
/// texture. The rung only selects *how the surface is measured*:
///
/// - `Auto` ("Texture" in the UI when UVs exist) — planar / SEED_PLANAR bake grid
///   over the AABB. Correct for UV-mapped meshes; on UV-less SDF bodies this is
///   the "gift-wrap" path (triplanar seeds) and is usually inferior to Vertex.
/// - `VertexCache` ("Vertex" in the UI) — SEED_POSITIONS at DC mesh vertices
///   (`encode_curvature_vertex_dispatch`). Lowering tags the generator with a
///   trailing `v` (`sgv`/`cxv`/`ccv`) so `vertex_curv_keys_for_graph` discovers
///   it. The bake still installs a map (Decision 1). Map *measurement* from the
///   per-vertex buffer (option a splat) is the remaining piece — see
///   `CURVATURE_BOTH_MODES.md` §5–6.
///
/// `normalize_generator_layers` forces Vertex when the body has no usable UVs
/// (owner rule). Direction never uses Vertex (no vertex kernel).
///
/// APPEND-ONLY (doc-compat discipline, same as every enum in this file).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum GeneratorRung {
    #[default]
    Auto,
    VertexCache,
    // APPEND only.
}

/// Layer blend modes. P0 declares the set; compositing is P3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Add,
    Overlay,
    Screen,
    // P2 Photoshop set (append-only) — maps 1:1 onto `shader_nodes::MixBlend`.
    Darken,
    Lighten,
    ColorBurn,
    ColorDodge,
    LinearBurn,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    // APPEND only.
}

impl BlendMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Add => "Add",
            Self::Overlay => "Overlay",
            Self::Screen => "Screen",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorBurn => "Color Burn",
            Self::ColorDodge => "Color Dodge",
            Self::LinearBurn => "Linear Burn",
            Self::SoftLight => "Soft Light",
            Self::HardLight => "Hard Light",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::HardMix => "Hard Mix",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::Divide => "Divide",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Self::Normal => "Norm",
            Self::Multiply => "Mult",
            Self::Add => "Add",
            Self::Overlay => "Ovly",
            Self::Screen => "Scrn",
            Self::Darken => "Drkn",
            Self::Lighten => "Lght",
            Self::ColorBurn => "CBrn",
            Self::ColorDodge => "CDdg",
            Self::LinearBurn => "LBrn",
            Self::SoftLight => "SLgt",
            Self::HardLight => "HLgt",
            Self::VividLight => "VLgt",
            Self::LinearLight => "LLgt",
            Self::PinLight => "PLgt",
            Self::HardMix => "HMix",
            Self::Difference => "Diff",
            Self::Exclusion => "Excl",
            Self::Subtract => "Sub",
            Self::Divide => "Div",
        }
    }
    pub fn all() -> &'static [BlendMode] {
        &[
            Self::Normal,
            Self::Darken,
            Self::Multiply,
            Self::ColorBurn,
            Self::LinearBurn,
            Self::Lighten,
            Self::Screen,
            Self::ColorDodge,
            Self::Add,
            Self::Overlay,
            Self::SoftLight,
            Self::HardLight,
            Self::VividLight,
            Self::LinearLight,
            Self::PinLight,
            Self::HardMix,
            Self::Difference,
            Self::Exclusion,
            Self::Subtract,
            Self::Divide,
        ]
    }
    /// Panel → graph MixRgb blend (single mapping table).
    pub fn to_mix_blend(self) -> crate::shader_nodes::MixBlend {
        use crate::shader_nodes::MixBlend;
        match self {
            Self::Normal => MixBlend::Mix,
            Self::Multiply => MixBlend::Multiply,
            Self::Add => MixBlend::Add,
            Self::Overlay => MixBlend::Overlay,
            Self::Screen => MixBlend::Screen,
            Self::Darken => MixBlend::Darken,
            Self::Lighten => MixBlend::Lighten,
            Self::ColorBurn => MixBlend::ColorBurn,
            Self::ColorDodge => MixBlend::ColorDodge,
            Self::LinearBurn => MixBlend::LinearBurn,
            Self::SoftLight => MixBlend::SoftLight,
            Self::HardLight => MixBlend::HardLight,
            Self::VividLight => MixBlend::VividLight,
            Self::LinearLight => MixBlend::LinearLight,
            Self::PinLight => MixBlend::PinLight,
            Self::HardMix => MixBlend::HardMix,
            Self::Difference => MixBlend::Difference,
            Self::Exclusion => MixBlend::Exclusion,
            Self::Subtract => MixBlend::Subtract,
            Self::Divide => MixBlend::Divide,
        }
    }
    pub fn from_mix_blend(m: crate::shader_nodes::MixBlend) -> Self {
        use crate::shader_nodes::MixBlend;
        match m {
            MixBlend::Mix => Self::Normal,
            MixBlend::Multiply => Self::Multiply,
            MixBlend::Add => Self::Add,
            MixBlend::Overlay => Self::Overlay,
            MixBlend::Screen => Self::Screen,
            MixBlend::Darken => Self::Darken,
            MixBlend::Lighten => Self::Lighten,
            MixBlend::ColorBurn => Self::ColorBurn,
            MixBlend::ColorDodge => Self::ColorDodge,
            MixBlend::LinearBurn => Self::LinearBurn,
            MixBlend::SoftLight => Self::SoftLight,
            MixBlend::HardLight => Self::HardLight,
            MixBlend::VividLight => Self::VividLight,
            MixBlend::LinearLight => Self::LinearLight,
            MixBlend::PinLight => Self::PinLight,
            MixBlend::HardMix => Self::HardMix,
            MixBlend::Difference => Self::Difference,
            MixBlend::Exclusion => Self::Exclusion,
            MixBlend::Subtract => Self::Subtract,
            MixBlend::Divide => Self::Divide,
        }
    }
}

/// Material-level (or overriding per-layer) texture space.
/// `projection` uses `MapProjection::as_u32()` codes (0 = Box/triplanar, 1 = UV;
/// append-only codes owned by core_math — see `pbr_material::MapProjection`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TextureSpace {
    pub projection: u32,
    pub tiling: [f32; 2],
    pub offset: [f32; 2],
    /// Box (triplanar) blend sharpness — higher = harder plane transitions, lower = smoother.
    /// Matches `TexMap::blend` / shader `|n|^blend`. Default 4.0 (legacy hardcoded).
    #[serde(default = "default_triplanar_blend")]
    pub blend: f32,
    /// Per-plane UV rotation in 90° steps (0..=3): X=YZ face, Y=XZ face, Z=XY face.
    #[serde(default)]
    pub rot_x: u8,
    #[serde(default)]
    pub rot_y: u8,
    #[serde(default)]
    pub rot_z: u8,
}

fn default_triplanar_blend() -> f32 {
    4.0
}

impl Default for TextureSpace {
    fn default() -> Self {
        Self {
            projection: 0,
            tiling: [1.0, 1.0],
            offset: [0.0, 0.0],
            blend: 4.0,
            rot_x: 0,
            rot_y: 0,
            rot_z: 0,
        }
    }
}

impl TextureSpace {
    /// Apply this space onto a TexMap (Fast path / install).
    pub(crate) fn apply_to_texmap(&self, tm: &mut core_math::pbr_material::TexMap) {
        tm.uv_scale = self.tiling[0];
        tm.projection = self.projection;
        tm.blend = self.blend.clamp(
            core_math::pbr_material::BLEND_MIN,
            core_math::pbr_material::BLEND_MAX,
        );
        tm.rot_x = self.rot_x & 3;
        tm.rot_y = self.rot_y & 3;
        tm.rot_z = self.rot_z & 3;
    }
}

/// Per-layer placement (spec §3): `None` = inherit the material texture space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Placement {
    #[serde(default)]
    pub space: Option<TextureSpace>,
}
