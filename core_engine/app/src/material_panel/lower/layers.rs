//! P3 image-layer lowering decision + P6 generator backend helpers.
//!
//! Pure relocation from lower.rs — no logic changes.

use super::super::model::*;

// ==================================================================
// P3 — image-layer lowering: fast path vs generated node subgraph
// ==================================================================

/// How a `Layered` channel lowers onto the engine (the P3 decision tree,
/// documented in docs/panel_node_mapping.md):
/// - `Fast` — single visible unmasked Image layer, default adjust, Normal blend,
///   opacity 1: the direct `TexMap` slot (P0/P2 path, unchanged). Uniform tiling
///   + projection ride the TexMap whether inherited or overridden.
/// - `Subgraph` — anything richer that existing node types can express (multiple
///   layers, adjust, blend/opacity, offset / non-uniform tiling): a deterministic
///   panel-generated node subgraph; the auto-apply bake composites it into the
///   channel's one TexMap slot.
/// - `Stub` — masked layers, or generator layers whose source has no bake
///   backend (spec §10): fail-soft, not lowered. Supported generator content
///   (Curvature / Cavity) is `Subgraph` as of P6.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChannelLowering {
    Fast,
    Subgraph,
    Stub,
}

/// WHY a `Layered` channel did not lower (P0-3 fix: the stub path used to be
/// silent — three `return Stub` sites and four consumers that all treated it as
/// a no-op, so a dead channel drew the same `layers` badge as a working one).
///
/// This is the *reason* half of the ONE decision function: `channel_lowering_full`
/// computes both, `channel_lowering` keeps returning just the verdict so every
/// existing caller is unchanged. The UI badge reads the reason off the SAME
/// function the lowering uses, so panel and engine can never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StubReason {
    /// Every layer in the stack is hidden (or the stack is empty).
    NoVisibleLayers,
    /// A layer's content or mask is a `Generator` whose source has no bake
    /// backend (NormalDirection — legacy; Thickness/Position/AO are supported).
    UnsupportedGenerator(GeneratorSource),
    /// The channel kind has no `TexMap` slot, so a bake has nowhere to install
    /// (Opacity, Clearcoat, Sheen, Ior, Sss*, GlassTint, … — 11 of 18 kinds).
    NoMapSlot,
}

impl StubReason {
    /// Plain-language explanation for the badge tooltip. States the cause AND the
    /// route that does work — never invents capability that does not exist.
    pub(crate) fn explain(self) -> String {
        match self {
            StubReason::NoVisibleLayers => "This channel has no visible layers, so nothing is applied. \
Turn a layer's visibility back on, or remove the channel."
                .to_string(),
            StubReason::UnsupportedGenerator(src) => format!(
                "The {} generator has no bake backend yet, so this channel is not applied. \
Only Curvature and Cavity can be baked — switch the generator source, or drive the channel from the shader tree.",
                generator_source_label(src)
            ),
            StubReason::NoMapSlot => "This channel has no texture slot, so layers cannot be baked into it. \
Use it as a plain value here, or drive it from the shader tree (it can still be used as a mask)."
                .to_string(),
        }
    }

    /// Short badge caption (fits the row's badge strip).
    pub(crate) fn badge_text(self) -> &'static str {
        match self {
            StubReason::NoVisibleLayers => "not applied",
            StubReason::UnsupportedGenerator(_) => "no backend",
            StubReason::NoMapSlot => "no slot",
        }
    }
}

/// Display name for a generator source (shared by the stub tooltip and the view's
/// generator picker wording).
pub(crate) fn generator_source_label(s: GeneratorSource) -> &'static str {
    match s {
        GeneratorSource::Curvature => "Curvature",
        GeneratorSource::AmbientOcclusion => "Ambient Occlusion",
        GeneratorSource::Thickness => "Thickness",
        GeneratorSource::Position => "Position",
        GeneratorSource::NormalDirection => "Normal Direction",
        GeneratorSource::Cavity => "Cavity",
        GeneratorSource::Direction => "Direction",
    }
}

/// The verdict only. `material_space` is the doc-level `texture_space` a layer inherits when it
/// has no `placement.space` override — REQUIRED, because whether the fast path can represent the
/// placement depends on that effective value (see `rich()`).
pub(crate) fn channel_lowering(ch: &Channel, material_space: TextureSpace) -> ChannelLowering {
    channel_lowering_full(ch, material_space).0
}

/// THE decision function: verdict + (when `Stub`) the reason. Single source of
/// truth for both the lowering and the UI badge.
pub(crate) fn channel_lowering_full(
    ch: &Channel,
    material_space: TextureSpace,
) -> (ChannelLowering, Option<StubReason>) {
    let stub = |r: StubReason| (ChannelLowering::Stub, Some(r));
    let visible: Vec<&Layer> = ch.layers.iter().filter(|l| l.visible).collect();
    if visible.is_empty() {
        return stub(StubReason::NoVisibleLayers); // nothing to lower (fail-soft)
    }
    // Masks now lower (P3.1) to a real subgraph that multiplies the layer's mix
    // factor. Only UNSUPPORTED generator content/mask sources stub the whole
    // channel (fail-soft, never partial — the UI cannot author those anyway).
    let bad_gen = |c: &LayerContent| -> Option<GeneratorSource> {
        match c {
            LayerContent::Generator { source, .. } if !generator_source_supported(*source) => {
                Some(*source)
            }
            _ => None,
        }
    };
    for l in &visible {
        if let Some(src) = bad_gen(&l.content).or_else(|| l.mask.primary_content().and_then(bad_gen)) {
            return stub(StubReason::UnsupportedGenerator(src));
        }
    }
    // Channels without a TexMap slot cannot receive a bake install.
    if ch.kind.map_kind().is_none() {
        return stub(StubReason::NoMapSlot);
    }
    let rich = |l: &Layer| -> bool {
        // EFFECTIVE space = the layer's override, else the MATERIAL default.
        //
        // BUG (owner 2026-07-28: "changing tiling ... I expect to see a difference of scale,
        // same thing with offset expecting movement"): this used `.unwrap_or(false)`, i.e. a
        // layer that INHERITS the material space was judged "not rich" no matter what that
        // space contained. Editing Tiling/Offset at the MATERIAL level — the common case, and
        // the one the owner hit — therefore kept the channel on the FAST path, whose `TexMap`
        // carries a single `uv_scale` scalar and NO offset field (bincode-frozen). V and the
        // entire offset were silently discarded, so the sliders moved and nothing happened.
        //
        // The SUBGRAPH path already handles all of it correctly (TexCoord → UVTransform with
        // Scale X/Y and Offset X/Y), so the fix is simply to route there whenever the effective
        // space cannot be expressed by a TexMap.
        let sp = l.placement.space.unwrap_or(material_space);
        let space_rich =
            sp.offset != [0.0, 0.0] || (sp.tiling[0] - sp.tiling[1]).abs() > 1e-6;
        !l.adjust.is_default()
            || l.blend != BlendMode::Normal
            || (l.opacity - 1.0).abs() > 1e-6
            || space_rich
            || !l.mask.is_empty()
            || matches!(
                l.content,
                LayerContent::Constant { .. }
                    | LayerContent::Generator { .. }
                    | LayerContent::Noise { .. }
            )
    };
    if visible.len() == 1 && !rich(visible[0]) {
        (ChannelLowering::Fast, None)
    } else {
        (ChannelLowering::Subgraph, None)
    }
}

/// Does this channel need a panel-generated SUBGRAPH in the material's node graph?
/// Extracted from `mp_lower_rich_channels` (was a closure) so the decision is one
/// testable place. G4 EVOLUTION of the MPEYEOFFREBAKE fix: a DISABLED channel still
/// codegens — `regen_channel_subgraph` emits its chain but leaves the master socket
/// UNWIRED (+ `channel_meta.disabled`), so the bake still cannot re-install the map
/// (the original bug), while the graph keeps the layers the doc-projection needs.
pub(crate) fn channel_needs_codegen(c: &Channel, material_space: TextureSpace) -> bool {
    if channel_uses_vertex_curvature(c) {
        return true;
    }
    if c.layers.is_empty() {
        return false;
    }
    match channel_lowering_full(c, material_space) {
        (ChannelLowering::Subgraph | ChannelLowering::Fast, _) => true,
        // G4: an ALL-HIDDEN stack (Stub/NoVisibleLayers) still codegens — the chain
        // composites nothing (hidden layers never join the mix chain, socket stays
        // unwired) but the LAYERS must live in the graph or the doc-projection
        // loses them. Other stub reasons (unsupported generator, no map slot) keep
        // the old skip — their emit arms cannot express the content anyway.
        (ChannelLowering::Stub, Some(StubReason::NoVisibleLayers)) => true,
        _ => false,
    }
}

/// Panel-facing helper: the stub reason for a channel that is `Layered` and does
/// NOT lower, else `None`. `Value` / `GraphDriven` channels are never "stubbed" —
/// they are lowered by their own path, so they must not raise the badge.
pub(crate) fn channel_stub_reason(
    ch: &Channel,
    material_space: TextureSpace,
) -> Option<StubReason> {
    if ch.state != ChannelState::Layered {
        return None;
    }
    channel_lowering_full(ch, material_space).1
}

// ==================================================================
// P6 — generator layers (C4): backend truth helpers
// ==================================================================

/// P6/P5: sources with a REAL bake backend today (verified in
/// `node_graph_bake::eval_node` — spec §10 gap report):
/// - `Curvature` → `NodeType::SdfCurvature` (body SDF field, native
///   signed/convex/concave outputs);
/// - `Cavity` → `NodeType::Cavity` (concave-only, body SDF field);
/// - `Direction` → `NodeType::SurfaceDirection` (surface normal vs world axis);
/// - `AmbientOcclusion` → `NodeType::AmbientOcclusion` (IQ-style hemisphere march
///   along the SDF gradient — matches `calc_ao` in `sdf_material.wgsl`).
/// Thickness/Position/NormalDirection still have no node and stay unsupported —
/// the view never offers them and lowering stubs them fail-soft.
pub(crate) fn generator_source_supported(s: GeneratorSource) -> bool {
    matches!(
        s,
        GeneratorSource::Curvature
            | GeneratorSource::Cavity
            | GeneratorSource::Direction
            | GeneratorSource::AmbientOcclusion
            | GeneratorSource::Thickness
            | GeneratorSource::Position
    )
}

/// SdfCurvature node output socket per selected curvature output
/// (1 = signed κ −1..1, 2 = convex mask, 3 = concave mask).
pub(crate) fn generator_output_socket(o: GeneratorOutput) -> usize {
    match o {
        GeneratorOutput::Signed => 1,
        GeneratorOutput::ConvexOnly => 2,
        GeneratorOutput::ConcaveOnly => 3,
    }
}

/// C4 radius floor: FD step floors at `1% of the short AABB axis` (not full diagonal —
/// elongate / multi-prim bodies would push a useless floor from the long axis).
/// A smaller authored radius is absorbed by the backend, so the UI surfaces this floor.
pub(crate) fn curvature_radius_floor(body_aabb_diag: f32) -> f32 {
    // Legacy single-arg API: callers that only have a diagonal still get a floor.
    // Prefer `curvature_radius_floor_aabb` when bmin/bmax are available.
    (body_aabb_diag * 0.01).max(1e-4)
}

/// Preferred floor from padded body AABB — short-axis based (matches `CurvConstants`).
pub(crate) fn curvature_radius_floor_aabb(bmin: glam::Vec3, bmax: glam::Vec3) -> f32 {
    let ext = (bmax - bmin).max(glam::Vec3::splat(1e-4));
    let min_axis = ext.x.min(ext.y).min(ext.z).max(1e-4);
    (min_axis * 0.01).max(1e-4)
}

/// **Auto radius** — the radius a curvature generator should DEFAULT to for a given body.
///
/// Distinct from [`curvature_radius_floor_aabb`], which is a *clamp*, not an answer. The floor is
/// 1% of the short axis: below it the finite-difference step drops under a DC cell and the signal
/// becomes noise. But 1% was also being used as the default, and on a box — flat faces, twelve
/// sharp edges — a 1%-of-body radius marks only pixels within 1% of an edge. That is a hairline,
/// not a surface effect, which is why curvature read as "nothing showing" on an ordinary box while
/// the whole pipeline was in fact working.
///
/// 8% of the short axis is wide enough that an edge band is plainly visible at a normal viewing
/// distance, and still far below the point where neighbouring edges' bands merge and the map turns
/// into a flat wash (~25%+ on a cube). It scales with the body, so the same default reads the same
/// on a 0.5-unit bolt and a 10-unit wall — which is the point of an auto: the user tweaks from
/// something visible instead of from zero.
///
/// Clamped to the floor so a very thin body can never auto below its own noise threshold.
pub(crate) fn curvature_auto_radius_aabb(bmin: glam::Vec3, bmax: glam::Vec3) -> f32 {
    let ext = (bmax - bmin).max(glam::Vec3::splat(1e-4));
    let min_axis = ext.x.min(ext.y).min(ext.z).max(1e-4);
    (min_axis * 0.08).max(curvature_radius_floor_aabb(bmin, bmax))
}

/// Curvature/Cavity on the Vertex rung (content or mask).
pub(crate) fn is_vertex_curv_generator(c: &LayerContent) -> bool {
    matches!(
        c,
        LayerContent::Generator {
            source: GeneratorSource::Curvature | GeneratorSource::Cavity,
            rung: GeneratorRung::VertexCache,
            ..
        }
    )
}

/// Pure Vertex *content* showcase (no Noise/Image, no masks) — clear planar map, G-buffer B&W.
///
/// Deliberately requires `l.mask.is_empty()`. A **Curvature-as-mask** on a Mult/Noise layer is
/// NOT "vertex-only": the layer still needs its texture bake. That path uses
/// [`channel_uses_vertex_curvature`] + `vertex_curv_shape_from_material` (`as_mask = true`) so
/// the instance flag and buffer still arm; this function must not treat mask layers as pure
/// Vertex or it would wipe the Mult map. Two predicates, two jobs — not a flag mismatch.
///
/// Layer stacks can sit under `Layered` *or* `GraphDriven` (panel subgraph ownership keeps
/// layers). Require visible layers only — not a specific ChannelState.
pub(crate) fn channel_is_vertex_rung_only(ch: &Channel) -> bool {
    let vis: Vec<_> = ch.layers.iter().filter(|l| l.visible).collect();
    !vis.is_empty()
        && vis
            .iter()
            .all(|l| is_vertex_curv_generator(&l.content) && l.mask.is_empty())
}

/// Any visible Vertex-rung Curvature/Cavity as **content or mask**.
/// Arms GPU vertex-curvature dispatch + instance flag (when the buffer is current).
/// Includes the common "Noise Mult + Curvature mask" recipe.
///
/// Does **not** require `ChannelState::Layered` — after panel subgraph lower the channel may
/// still hold Vertex layers while the socket is treated as graph-owned. Dropping the state
/// gate was required so clear-maps / path badges / shape params keep working.
pub(crate) fn channel_uses_vertex_curvature(ch: &Channel) -> bool {
    ch.layers.iter().filter(|l| l.visible).any(|l| {
        is_vertex_curv_generator(&l.content)
            || l.mask.primary_content().is_some_and(is_vertex_curv_generator)
    })
}

/// Layers that still need a texture bake (Noise/Image/Constant/any supported Generator).
/// P2: VertexCache generators bake via SdfCurvature like Auto (no G-buffer composite).
pub(crate) fn channel_has_texture_bake_content(ch: &Channel) -> bool {
    if ch.layers.is_empty() {
        return false;
    }
    ch.layers.iter().filter(|l| l.visible).any(|l| {
        let content = match &l.content {
            LayerContent::Image { tex_id, .. } => *tex_id != 0,
            LayerContent::Noise { .. } | LayerContent::Constant { .. } => true,
            LayerContent::Generator { source, .. } => generator_source_supported(*source),
        };
        let mask = match l.mask.primary_content() {
            Some(LayerContent::Image { tex_id, .. }) => *tex_id != 0,
            Some(LayerContent::Constant { .. } | LayerContent::Noise { .. }) => true,
            Some(LayerContent::Generator { source, .. }) => generator_source_supported(*source),
            _ => false,
        };
        content || mask
    })
}

/// Skip planar map install — **always false after P2**.
///
/// HANDOFF / ADR Decision 1: the bake is the single compositor. Pure-Vertex channels
/// used to skip map install and composite live in `gbuffer.wgsl`; that second compositor
/// is deleted. VertexCache generators lower into the bake chain (SdfCurvature) so Hybrid
/// and PT both sample the same map.
pub(crate) fn channel_skip_planar_map_install(_ch: &Channel) -> bool {
    false
}

fn normalize_one_generator(c: &mut LayerContent, has_uvs: bool, floor: f32) -> bool {
    let LayerContent::Generator {
        source, radius, rung, ..
    } = c
    else {
        return false;
    };
    if !matches!(
        *source,
        GeneratorSource::Curvature
            | GeneratorSource::Cavity
            | GeneratorSource::Direction
            | GeneratorSource::AmbientOcclusion
    ) {
        return false;
    }
    let mut changed = false;
    if *radius < floor {
        *radius = floor;
        changed = true;
    }
    // "No UVs → Vertex" applies ONLY to the sources the vertex-curvature kernel can compute.
    // Direction/AO have no vertex kernel (see the emitter's rung_v comment); promoting them would
    // turn a working texture-rung layer into a silent no-op on every SDF body — the exact opposite
    // of what the rule is for. They also get DEMOTED back to Auto here if a doc arrives holding
    // VertexCache (hand-edited JSON, or a source swap from Curvature on a no-UV body, which is
    // the realistic path), so the panel's rung display and the emitter never disagree.
    if matches!(
        *source,
        GeneratorSource::Direction | GeneratorSource::AmbientOcclusion
    ) {
        if *rung != GeneratorRung::Auto {
            *rung = GeneratorRung::Auto;
            changed = true;
        }
        return changed;
    }
    if !has_uvs && *rung != GeneratorRung::VertexCache {
        *rung = GeneratorRung::VertexCache;
        changed = true;
    }
    changed
}

/// No UVs → Vertex; clamp radius. Walks content + masks.
///
/// Also promotes `Value` + non-empty layers → `Layered` (owner 2026-07-29: "Add layer → Noise"
/// on Roughness left state as Value; every commit then `write_value` → `clear_map` wiped the
/// bake install, so Mult noise never stayed visible).
pub(crate) fn normalize_generator_layers(
    doc: &mut MaterialPanelDoc,
    has_uvs: bool,
    radius_floor: Option<f32>,
) -> bool {
    let floor = radius_floor.unwrap_or(0.02).max(1e-4);
    let mut changed = false;
    for ch in &mut doc.channels {
        // Layers without Layered state cannot survive lower_to_material (Value clears maps).
        if !ch.layers.is_empty() && matches!(ch.state, ChannelState::Value(_)) {
            ch.state = ChannelState::Layered;
            changed = true;
        }
        for layer in &mut ch.layers {
            if normalize_one_generator(&mut layer.content, has_uvs, floor) {
                changed = true;
            }
            if let Some(m) = layer.mask.primary_content_mut() {
                if normalize_one_generator(m, has_uvs, floor) {
                    changed = true;
                }
            }
        }
    }
    changed
}

pub(crate) fn generators_need_normalize(
    doc: &MaterialPanelDoc,
    has_uvs: bool,
    radius_floor: Option<f32>,
) -> bool {
    let floor = radius_floor.unwrap_or(0.02).max(1e-4);
    // MUST mirror `normalize_one_generator` exactly — this is the "is it dirty?" half of the same
    // decision, and a disagreement means either a normalize that never runs or one that runs
    // every frame.
    let needs = |c: &LayerContent| -> bool {
        match c {
            LayerContent::Generator {
                source: GeneratorSource::Direction | GeneratorSource::AmbientOcclusion,
                radius,
                rung,
                ..
            } => *radius < floor || *rung != GeneratorRung::Auto,
            LayerContent::Generator {
                source: GeneratorSource::Curvature | GeneratorSource::Cavity,
                radius,
                rung,
                ..
            } => *radius < floor || (!has_uvs && *rung != GeneratorRung::VertexCache),
            _ => false,
        }
    };
    doc.channels.iter().any(|ch| {
        ch.layers.iter().any(|l| needs(&l.content) || l.mask.primary_content().is_some_and(needs))
    })
}

