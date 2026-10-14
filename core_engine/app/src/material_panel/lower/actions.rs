//! Doc ↔ engine lowering actions (lower/derive/refresh/reconcile).
//!
//! Pure relocation from lower.rs — no logic changes.

use super::super::model::*;
use core_math::pbr_material::{MapKind, PbrMaterial, TexMap};
use super::{
    channel_lowering, channel_lowering_full, parse_panel_channel, channel_source, ChannelSource,
    ALL_KINDS, ChannelLowering, StubReason,
};

/// Lower a panel doc into the engine material (doc → `PbrMaterial`).
///
/// P0 coverage: `ChannelState::Value` writes the mapped scalar/color field;
/// `Layered` with exactly ONE visible plain `Image` layer writes the channel's
/// `TexMap` slot (the exact inverse of [`derive_doc_from_material`], so
/// derive→lower is identity on today's materials). `GraphDriven` channels are
/// never written (C1 — the graph owns them).
///
/// STUB (P2/P3, fail-soft): multi-layer stacks, masks, `Generator` and `Constant`
/// layer content are NOT lowered yet — those channels leave the material
/// unchanged (no panic, no partial write). Clearly marked below.

/// `MapProjection` wire code -> the node graph's `ProjectionStyle`.
///
/// ONE pair of converters for both directions (`projection_style_for` / `map_projection_for`) so
/// lowering and reading-back cannot disagree — they were previously two inline `match`es in
/// different files, which is how UV survived the round trip but nothing else could.
///
/// `MapProjection` is the material/GPU-facing enum (5 variants, serialized codes); `ProjectionStyle`
/// is the node-graph one (8 variants, including the three planar axes). The mapping is therefore
/// deliberately NOT a bijection: PlanarXZ/PlanarYZ/None have no material-level code and collapse to
/// the nearest one that does. Only round trips STARTING from a `MapProjection` are lossless, which
/// is the direction the panel actually drives.
pub(crate) fn projection_style_for(code: u32) -> crate::shader_nodes::ProjectionStyle {
    use core_math::pbr_material::MapProjection as MP;
    match MP::from_u32(code) {
        MP::Box => crate::shader_nodes::ProjectionStyle::Triplanar,
        MP::Uv => crate::shader_nodes::ProjectionStyle::PlanarXY,
        MP::Sphere => crate::shader_nodes::ProjectionStyle::Spherical,
        MP::Cylinder => crate::shader_nodes::ProjectionStyle::Cylindrical,
        MP::Planar => crate::shader_nodes::ProjectionStyle::PlanarXY,
    }
}

/// Inverse of [`projection_style_for`] for the variants that have a material-level code.
pub(crate) fn map_projection_for(style: crate::shader_nodes::ProjectionStyle) -> core_math::pbr_material::MapProjection {
    use core_math::pbr_material::MapProjection as MP;
    match style {
        crate::shader_nodes::ProjectionStyle::Spherical => MP::Sphere,
        crate::shader_nodes::ProjectionStyle::Cylindrical => MP::Cylinder,
        // PlanarXY is the code UV has always round-tripped through; keep that so existing saves
        // keep their meaning. The other planar axes have no material code and fall back to Box.
        crate::shader_nodes::ProjectionStyle::PlanarXY => MP::Uv,
        _ => MP::Box,
    }
}

pub(crate) fn lower_to_material(doc: &MaterialPanelDoc, mat: &mut PbrMaterial) {
    mat.shader_type = core_math::pbr_material::ShaderType::from_u32(doc.shader_type);
    for ch in &doc.channels {
        // Eye toggle OFF → the channel contributes nothing this frame. Identical effect
        // to `StubReason::NoVisibleLayers` below (clear the map slot so the shader falls
        // back to the material's scalar), but NON-DESTRUCTIVE: the doc keeps every layer,
        // image and parameter, so clicking the eye back on restores it with no reload.
        if !ch.enabled {
            if let Some(mk) = ch.kind.map_kind() {
                *mat.map_mut(mk) = TexMap::default();
            }
            continue;
        }
        match &ch.state {
            ChannelState::GraphDriven { .. } => {} // C1: never overwrite the graph's channel
            ChannelState::Value(v) => write_value(ch.kind, *v, mat),
            ChannelState::Layered => {
                match channel_lowering(ch, doc.texture_space) {
                    // FAST PATH (P0/P2, unchanged): single visible unmasked
                    // plain image layer → the channel's direct TexMap slot.
                    ChannelLowering::Fast => {
                        let visible: Vec<&Layer> =
                            ch.layers.iter().filter(|l| l.visible).collect();
                        if let ([layer], Some(mk)) = (visible.as_slice(), ch.kind.map_kind()) {
                            if let LayerContent::Image { tex_id, strength } = layer.content {
                                let space =
                                    layer.placement.space.unwrap_or(doc.texture_space);
                                let mut tm = TexMap {
                                    tex_id,
                                    strength,
                                    // Hand-loaded Image → classic tint Mult (not absolute bake).
                                    absolute: false,
                                    ..Default::default()
                                };
                                space.apply_to_texmap(&mut tm);
                                *mat.map_mut(mk) = tm;
                            }
                        }
                    }
                    // SUBGRAPH (P3): the channel's map slot is produced by the
                    // auto-apply bake of the panel-generated node subgraph
                    // (`regen_channel_subgraph` — called at commit in edit.rs).
                    // The material is not written here: the bake install owns
                    // the TexMap slot.
                    ChannelLowering::Subgraph => {}
                    // STUB: the channel produces nothing this frame.
                    //
                    // "material unchanged (fail-soft)" is right for a channel that CANNOT be
                    // lowered yet (unsupported generator, no map slot) — the user's existing map
                    // should survive a temporary gap. It is WRONG when the channel is empty
                    // because the user just deleted the last layer (owner 2026-07-28: "when I
                    // remove a layer it should apply directly"): the layer vanished from the UI
                    // while its baked texture stayed installed on the material, so the viewport
                    // kept showing a layer that no longer exists.
                    //
                    // Only `NoVisibleLayers` means "the user emptied this channel" — that one
                    // clears the slot so the material matches the panel. The other two reasons
                    // keep the old fail-soft behaviour, deliberately.
                    ChannelLowering::Stub => {
                        if matches!(
                            channel_lowering_full(ch, doc.texture_space).1,
                            Some(StubReason::NoVisibleLayers)
                        ) {
                            if let Some(mk) = ch.kind.map_kind() {
                                *mat.map_mut(mk) = TexMap::default();
                            }
                        }
                    }
                }
            }
        }
    }
}

fn write_value(kind: ChannelKind, v: ChannelValue, mat: &mut PbrMaterial) {
    use glam::Vec3;
    let f = |v: ChannelValue| match v {
        ChannelValue::Scalar(s) => s,
        ChannelValue::Color(c) => (c[0] + c[1] + c[2]) / 3.0,
    };
    let c3 = |v: ChannelValue| match v {
        ChannelValue::Color(c) => Vec3::new(c[0], c[1], c[2]),
        ChannelValue::Scalar(s) => Vec3::splat(s),
    };
    // When the panel owns a channel as Value, drop any leftover map so the scalar
    // is not replaced in the gbuffer (map strength 1.0 → slider appears dead in PBR).
    let clear_map = |mk: MapKind, mat: &mut PbrMaterial| {
        *mat.map_mut(mk) = TexMap::default();
    };
    match kind {
        ChannelKind::BaseColor => {
            mat.albedo = c3(v);
            clear_map(MapKind::Albedo, mat);
        }
        ChannelKind::Roughness => {
            mat.roughness = f(v);
            clear_map(MapKind::Roughness, mat);
        }
        ChannelKind::Metallic => {
            mat.metallic = f(v);
            clear_map(MapKind::Metallic, mat);
        }
        ChannelKind::Opacity => mat.opacity = f(v),
        ChannelKind::Emission => {
            mat.emission_color = c3(v);
            clear_map(MapKind::Emission, mat);
        }
        ChannelKind::Normal => {
            mat.normal_strength = f(v);
            clear_map(MapKind::Normal, mat);
        }
        // Bump / Displacement have no scalar field — map-only channels (see the
        // model.rs mapping table); a Value state for them is inert by design.
        ChannelKind::Bump | ChannelKind::Displacement => {}
        ChannelKind::Clearcoat => mat.clearcoat = f(v),
        ChannelKind::ClearcoatRoughness => mat.clearcoat_roughness = f(v),
        ChannelKind::Sheen => mat.sheen = f(v),
        ChannelKind::SheenTint => mat.sheen_tint = f(v),
        ChannelKind::Anisotropy => mat.anisotropy = f(v),
        ChannelKind::AnisotropyRotation => mat.anisotropy_rotation = f(v),
        ChannelKind::Ior => mat.ior = f(v),
        ChannelKind::SssRadius => mat.sss_radius = f(v),
        ChannelKind::SssColor => {
            mat.sss_color = c3(v);
            clear_map(MapKind::SssColor, mat);
        }
        ChannelKind::GlassTint => mat.glass_tint = c3(v),
    }
}

/// Lower a panel doc into a generated node graph (doc → `NodeGraph`), the C1
/// expansion seam.
///
/// P0 coverage: builds the default Master-Material graph and seeds its socket
/// defaults from the doc's `Value` channels via the EXISTING one-material sync
/// (`NodeGraph::pull_from_pbr`) — no duplicated socket table.
///
/// STUB (P2): per-widget node codegen (image/generator/blend node emission) is
/// not implemented — Layered/GraphDriven channels contribute nothing beyond the
/// lowered defaults; the returned graph is minimal but valid (fail-soft).
pub(crate) fn lower_to_graph(doc: &MaterialPanelDoc) -> crate::shader_nodes::NodeGraph {
    let mut mat = PbrMaterial::default();
    lower_to_material(doc, &mut mat);
    let mut g = crate::shader_nodes::NodeGraph::default();
    g.pull_from_pbr(&mat);
    g
}

/// Migration (P0): build a panel doc from an existing material (+ its optional
/// material-owned graph). Every channel in the model mapping is emitted so the
/// panel is a stable scannable form (D1 spirit):
/// - `GraphDriven { node }` where the graph wires the channel's master socket
///   (same `master_socket_driven` test as `node_graph_bake::channels_from_graph`);
/// - `Layered` with one Image layer where the channel's `TexMap.is_set()`;
/// - plain `Value` from the scalar field otherwise.
pub(crate) fn derive_doc_from_material(
    mat: &PbrMaterial,
    graph: Option<&crate::shader_nodes::NodeGraph>,
) -> MaterialPanelDoc {
    use crate::shader_nodes::NodeGraph as NG;
    let _ = NG::MO_ALBEDO; // keep the use-path documented
    let out_node = graph.and_then(|g| g.output_node()).map(|n| n.id);
    // G2: wired-ness comes from the ONE decision function, keyed by CHANNEL —
    // consumers stop carrying the kind→socket mapping themselves.
    let driven = |kind: ChannelKind| -> bool {
        graph
            .map(|g| channel_source(g, kind) != ChannelSource::Unwired)
            .unwrap_or(false)
    };
    let value_of = |kind: ChannelKind| -> ChannelValue { channel_value_of(kind, mat) };
    // Only channels this material actually USES.
    //
    // Deriving all 18 of ALL_KINDS gave every material without a stored doc a panel of
    // 18 rows — 4 with real content and 14 inert. A library drop hits this because the
    // import authors no doc, but so do `create_shared_material_from_prim`,
    // `promote_embedded_material_for_selection` and `duplicate_material_node`, while a
    // Ctrl+M material stores `default_for_new()` (5 channels). Filtering HERE fixes the
    // whole class instead of special-casing the library path.
    //
    // Nothing downstream needs the padding: "+ Add channel" computes its list from
    // `doc.channel(k).is_none()`, and `lower_to_material` only writes channels present
    // in the doc, so an absent kind leaves the PbrMaterial untouched.
    let default_mat = PbrMaterial::default();
    let in_use = |kind: ChannelKind| -> bool {
        // Panel-authored subgraph, or the graph drives the socket (the real maps).
        if graph.and_then(|g| parse_panel_channel(g, kind)).is_some() {
            return true;
        }
        if driven(kind) {
            return true;
        }
        // A set map slot.
        if let Some(mk) = kind.map_kind() {
            if mat.map(mk).is_set() {
                return true;
            }
        }
        // An authored scalar/colour that differs from the default — otherwise a hand-set
        // IOR or Clearcoat would silently vanish from the panel.
        if channel_value_of(kind, mat) != channel_value_of(kind, &default_mat) {
            return true;
        }
        // Always keep the MAIN set so the panel matches a Ctrl+M material's shape.
        matches!(
            kind,
            ChannelKind::BaseColor
                | ChannelKind::Roughness
                | ChannelKind::Metallic
                | ChannelKind::Normal
                | ChannelKind::Displacement
        )
    };
    let channels: Vec<Channel> = ALL_KINDS
        .iter()
        .filter(|&&kind| in_use(kind))
        .map(|&kind| {
            // A PANEL-GENERATED subgraph re-derives as its layer stack (P3
            // round-trip) — never as GraphDriven. G4: per-channel authored state
            // (eye / bit depth / Live-Bake) rides `channel_meta`.
            if let Some(layers) = graph.and_then(|g| parse_panel_channel(g, kind)) {
                let mut c = Channel::with_state(kind, ChannelState::Layered, layers);
                if let Some(m) = graph.and_then(|g| g.channel_meta.get(&(kind as u32))) {
                    c.enabled = !m.disabled;
                    c.bit_depth = if m.bit16 {
                        ChannelBitDepth::Bit16
                    } else {
                        ChannelBitDepth::Bit8
                    };
                    c.eval_mode = if m.live {
                        ChannelEvalMode::Live
                    } else {
                        ChannelEvalMode::Bake
                    };
                }
                return c;
            }
            // Graph wins (C1: GraphDriven badge, never a lying slider).
            if let Some(node) = out_node {
                if driven(kind) {
                    return Channel::with_state(
                        kind,
                        ChannelState::GraphDriven { node },
                        vec![],
                    );
                }
            }
            // Then a set map slot → single-image layered channel.
            if let Some(mk) = kind.map_kind() {
                let tm = *mat.map(mk);
                if tm.is_set() {
                    return Channel::with_state(
                        kind,
                        ChannelState::Layered,
                        vec![Layer {
            id: 0,
            drives: Vec::new(),
content: LayerContent::Image { tex_id: tm.tex_id, strength: tm.strength },
                            mask: MaskStack::default(),
                            blend: BlendMode::Normal,
                            opacity: 1.0,
                            visible: true,
                            placement: Placement {
                                space: Some(TextureSpace {
                                    projection: tm.projection,
                                    tiling: [tm.uv_scale, tm.uv_scale],
                                    offset: [0.0, 0.0],
                                    blend: tm.blend,
                                    rot_x: tm.rot_x,
                                    rot_y: tm.rot_y,
                                    rot_z: tm.rot_z,
                                }),
                            },
                            adjust: Adjust::default(),
                        }],
                    );
                }
            }
            Channel::value(kind, value_of(kind))
        })
        .collect();
    // G4: doc-level authored fields come FROM the graph (the projection direction).
    // next_layer_id from the recovered lids so a later edit never re-issues one.
    let next_layer_id = channels
        .iter()
        .flat_map(|c| c.layers.iter().map(|l| l.id))
        .max()
        .map(|m| m + 1)
        .unwrap_or(1);
    MaterialPanelDoc {
        next_layer_id,
        generator_mode: graph.map(|g| g.generator_mode).unwrap_or(0),
        shader_type: mat.shader_type.as_u32(),
        texture_space: graph.map(|g| g.texture_space).unwrap_or_default(),
        channels,
        bake_res: graph
            .map(|g| crate::material_panel::model::snap_panel_bake_res(g.bake_res))
            .unwrap_or(256),
    }
}

/// ChannelKind → PRIMARY Master Material socket (the model.rs mapping table).
/// This is the socket whose wiring makes the channel `GraphDriven` (same test as
/// `node_graph_bake::channels_from_graph`).
pub(crate) fn kind_socket(kind: ChannelKind) -> usize {
    use crate::shader_nodes::NodeGraph as NG;
    match kind {
        ChannelKind::BaseColor => NG::MO_ALBEDO,
        ChannelKind::Roughness => NG::MO_ROUGHNESS,
        ChannelKind::Metallic => NG::MO_METALLIC,
        ChannelKind::Opacity => NG::MO_OPACITY,
        ChannelKind::Emission => NG::MO_EMISSION,
        ChannelKind::Normal => NG::MO_NORMAL,
        ChannelKind::Bump => NG::MO_HEIGHT,
        ChannelKind::Displacement => NG::MO_DISPLACEMENT,
        ChannelKind::Clearcoat => NG::MO_CLEARCOAT,
        ChannelKind::ClearcoatRoughness => NG::MO_CLEARCOAT_ROUGH,
        ChannelKind::Sheen => NG::MO_SHEEN,
        ChannelKind::SheenTint => NG::MO_SHEEN_TINT,
        ChannelKind::Anisotropy => NG::MO_ANISOTROPY,
        ChannelKind::AnisotropyRotation => NG::MO_ANISOTROPY_ROT,
        ChannelKind::Ior => NG::MO_IOR,
        ChannelKind::SssRadius => NG::MO_SSS_RADIUS,
        ChannelKind::SssColor => NG::MO_SSS_COLOR,
        ChannelKind::GlassTint => NG::MO_GLASS_TINT,
    }
}

/// ALL master sockets a channel owns for graph surgery (primary + companions).
/// Revert must clear the companion wires too (Emission intensity, Normal strength)
/// or the channel would stay half-driven.
pub(crate) fn kind_sockets(kind: ChannelKind) -> &'static [usize] {
    use crate::shader_nodes::NodeGraph as NG;
    match kind {
        ChannelKind::Emission => &[NG::MO_EMISSION, NG::MO_EMISSION_INTENSITY],
        ChannelKind::Normal => &[NG::MO_NORMAL, NG::MO_NORMAL_STRENGTH],
        ChannelKind::BaseColor => &[NG::MO_ALBEDO, NG::MO_AO],
        ChannelKind::Roughness => &[NG::MO_ROUGHNESS],
        ChannelKind::Metallic => &[NG::MO_METALLIC],
        ChannelKind::Opacity => &[NG::MO_OPACITY],
        ChannelKind::Bump => &[NG::MO_HEIGHT],
        ChannelKind::Displacement => &[NG::MO_DISPLACEMENT],
        ChannelKind::Clearcoat => &[NG::MO_CLEARCOAT],
        ChannelKind::ClearcoatRoughness => &[NG::MO_CLEARCOAT_ROUGH],
        ChannelKind::Sheen => &[NG::MO_SHEEN],
        ChannelKind::SheenTint => &[NG::MO_SHEEN_TINT],
        ChannelKind::Anisotropy => &[NG::MO_ANISOTROPY],
        ChannelKind::AnisotropyRotation => &[NG::MO_ANISOTROPY_ROT],
        ChannelKind::Ior => &[NG::MO_IOR],
        ChannelKind::SssRadius => &[NG::MO_SSS_RADIUS],
        ChannelKind::SssColor => &[NG::MO_SSS_COLOR],
        ChannelKind::GlassTint => &[NG::MO_GLASS_TINT],
    }
}

/// The engine value a channel currently holds (mirror of `write_value`).
pub(crate) fn channel_value_of(kind: ChannelKind, mat: &PbrMaterial) -> ChannelValue {
    match kind {
        ChannelKind::BaseColor => ChannelValue::Color(mat.albedo.into()),
        ChannelKind::Roughness => ChannelValue::Scalar(mat.roughness),
        ChannelKind::Metallic => ChannelValue::Scalar(mat.metallic),
        ChannelKind::Opacity => ChannelValue::Scalar(mat.opacity),
        ChannelKind::Emission => ChannelValue::Color(mat.emission_color.into()),
        ChannelKind::Normal => ChannelValue::Scalar(mat.normal_strength),
        ChannelKind::Bump | ChannelKind::Displacement => ChannelValue::Scalar(0.0),
        ChannelKind::Clearcoat => ChannelValue::Scalar(mat.clearcoat),
        ChannelKind::ClearcoatRoughness => ChannelValue::Scalar(mat.clearcoat_roughness),
        ChannelKind::Sheen => ChannelValue::Scalar(mat.sheen),
        ChannelKind::SheenTint => ChannelValue::Scalar(mat.sheen_tint),
        ChannelKind::Anisotropy => ChannelValue::Scalar(mat.anisotropy),
        ChannelKind::AnisotropyRotation => ChannelValue::Scalar(mat.anisotropy_rotation),
        ChannelKind::Ior => ChannelValue::Scalar(mat.ior),
        ChannelKind::SssRadius => ChannelValue::Scalar(mat.sss_radius),
        ChannelKind::SssColor => ChannelValue::Color(mat.sss_color.into()),
        ChannelKind::GlassTint => ChannelValue::Color(mat.glass_tint.into()),
    }
}

/// P2 undo-follow: refresh a STORED doc's engine-mirrored fields from the live
/// material, without touching authored structure (channel set/order, states,
/// layer stacks, texture space). Undo snapshots restore `PbrMaterial` (graph
/// node) but not the doc store — this keeps the panel truthful after Ctrl+Z
/// exactly like the v1 material editor (which reads the material directly).
/// - `Value` channels re-read their scalar/color;
/// - single-image `Layered` channels re-read tex_id/strength (+ uv/projection
///   into their placement) from the channel's `TexMap`;
/// - `GraphDriven` channels are untouched (C1 — the graph owns them);
/// G3 FORWARD refresh (replaces `refresh_doc_from_material`, deleted): sync the doc's
/// `Value` channels from the GRAPH's Master socket defaults, so a scalar scrubbed in the
/// node editor shows in the panel. Direction is graph → doc — a projection read, never a
/// reverse sync. Wired sockets are skipped (the channel is graph-driven there, and the
/// state badge is `mp_reconcile_graph_ownership`'s job). `shader_type` is NOT synced:
/// the panel authors it, the graph carries it, the material derives it — and a legacy
/// graph deserializes `shader_type: 0`, which must not downgrade an authored doc
/// (the TexMap-rot field-trap rule: zero in a new field decodes to OLD behavior).
pub(crate) fn refresh_doc_values_from_graph(
    doc: &mut MaterialPanelDoc,
    g: &crate::shader_nodes::NodeGraph,
) {
    let Some(out) = g.output_node() else { return };
    let out_id = out.id;
    let d = &out.input_defaults;
    for ch in &mut doc.channels {
        let ChannelState::Value(v) = ch.state else { continue };
        let sock = kind_socket(ch.kind);
        if g.is_input_connected(out_id, sock) {
            continue;
        }
        let Some(slot) = d.get(sock) else { continue };
        ch.state = ChannelState::Value(match v {
            ChannelValue::Scalar(_) => ChannelValue::Scalar(slot.value_f32),
            ChannelValue::Color(_) => {
                let c = slot.value_vec3;
                ChannelValue::Color([c[0], c[1], c[2]])
            }
        });
    }
}

/// G3 async-upload heal (the ONLY surviving material read, and it is an ADOPTION, not a
/// sync): `mp_mark_hand_loaded_map` authors a Fast image layer with `tex_id: 0` before the
/// decode finishes; the upload then lands in the material's TexMap slot. A doc layer whose
/// image is still 0 adopts the slot's id/strength. It can never overwrite authored state —
/// a non-zero tex_id is left alone — so it cannot fight the forward write path.
pub(crate) fn heal_doc_fast_texids(doc: &mut MaterialPanelDoc, mat: &PbrMaterial) {
    let ts = doc.texture_space;
    for ch in &mut doc.channels {
        if ch.state != ChannelState::Layered {
            continue;
        }
        // FAST-path channels only: for Subgraph channels the TexMap slot holds the
        // baked COMPOSITE — adopting its id would corrupt the layer's source image
        // reference (the exact trap the old refresh guarded with this same gate).
        if channel_lowering(ch, ts) != ChannelLowering::Fast {
            continue;
        }
        if let (Some(mk), [layer]) = (ch.kind.map_kind(), ch.layers.as_mut_slice()) {
            let tm = *mat.map(mk);
            if !tm.is_set() {
                continue;
            }
            if let LayerContent::Image { tex_id, strength } = &mut layer.content {
                if *tex_id == 0 {
                    *tex_id = tm.tex_id;
                    *strength = tm.strength;
                }
            }
        }
    }
}

/// P2 GraphDriven promotion/demotion (spec C1) — the ONE reconciliation of a
/// stored doc against the material's hand-edited graph. Returns true if changed.
/// - A channel whose primary Master socket is WIRED becomes `GraphDriven{out}`
///   (promotion; layer stack kept so a later revert can restore it).
/// - A `GraphDriven` channel whose socket is no longer wired demotes back to
///   `Layered` (if its map slot is still set / it kept layers) or `Value` from
///   the live material. This is graph-side unwiring — NOT a panel edit — so it
///   does not violate the one-way-promotion rule.
/// Panel-owned states (`Value`/`Layered`) are never demoted here.
pub(crate) fn reconcile_doc_with_graph(
    doc: &mut MaterialPanelDoc,
    mat: &PbrMaterial,
    graph: Option<&crate::shader_nodes::NodeGraph>,
) -> bool {
    let out_node = graph.and_then(|g| g.output_node()).map(|n| n.id);
    let mut changed = false;
    for ch in &mut doc.channels {
        // P3: a PANEL-GENERATED subgraph wiring the socket is panel ownership,
        // not a hand edit — it must never promote the channel to GraphDriven.
        // G2: "hand graph drives it" is exactly ChannelSource::HandWired — a
        // panel-generated chain (PanelStack) must never promote to GraphDriven.
        let driven = graph
            .map(|g| channel_source(g, ch.kind) == ChannelSource::HandWired)
            .unwrap_or(false);
        match (driven, ch.state) {
            (true, ChannelState::GraphDriven { .. }) => {}
            (true, _) => {
                if let Some(node) = out_node {
                    ch.state = ChannelState::GraphDriven { node };
                    changed = true;
                }
            }
            (false, ChannelState::GraphDriven { .. }) => {
                let has_map = ch
                    .kind
                    .map_kind()
                    .map(|mk| mat.map(mk).is_set())
                    .unwrap_or(false);
                if !ch.layers.is_empty() {
                    ch.state = ChannelState::Layered;
                } else if has_map {
                    // Rebuild the single-image layer from the live TexMap (derive shape).
                    let mk = ch.kind.map_kind().expect("has_map implies map slot");
                    let tm = *mat.map(mk);
                    ch.state = ChannelState::Layered;
                    ch.layers = vec![Layer {
                        id: 0,
                        drives: Vec::new(),
                        content: LayerContent::Image { tex_id: tm.tex_id, strength: tm.strength },
                        mask: MaskStack::default(),
                        blend: BlendMode::Normal,
                        opacity: 1.0,
                        visible: true,
                        placement: Placement {
                            space: Some(TextureSpace {
                                projection: tm.projection,
                                tiling: [tm.uv_scale, tm.uv_scale],
                                offset: [0.0, 0.0],
                                blend: tm.blend,
                                rot_x: tm.rot_x,
                                rot_y: tm.rot_y,
                                rot_z: tm.rot_z,
                            }),
                        },
                        adjust: Adjust::default(),
                    }];
                } else {
                    ch.state = ChannelState::Value(channel_value_of(ch.kind, mat));
                }
                changed = true;
            }
            (false, _) => {}
        }
    }
    changed
}

