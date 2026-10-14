//! Panel subgraph codegen and round-trip parse (maps/tags/masks).
//!
//! Pure relocation from lower.rs — no logic changes.

use super::super::model::*;
use super::{
    channel_lowering, channel_value_of, generator_output_socket, kind_socket,
    map_projection_for, projection_style_for, ALL_KINDS, ChannelLowering,
};
use core_math::pbr_material::PbrMaterial;

/// Panel-generated node tag: `mp:{kind}:{role}:{layer_id}[:{flag}]` in `Node.label`.
/// `layer_id` is [`Layer::id`] (stable), not the stack index (ADR P1 / HANDOFF_FOR_GROK).
/// Hand-authored nodes have no `mp:` prefix and are never stripped by panel regen.
fn panel_tag(kind: ChannelKind, role: &str, layer_id: u32) -> String {
    format!("mp:{}:{}:{}", kind as u32, role, layer_id)
}

/// Parse a panel tag → (kind code, role, **stable layer id**, optional flag).
///
/// `pub(crate)`: the GPU vertex-curvature dispatch (`node_graph_bake::vertex_curv_keys_for_graph`,
/// GPU_CURVATURE_KERNEL_PLAN.md P2) reuses THIS parser to detect a `VertexCache`-rung Generator
/// node from its label flag, rather than duplicating the `v`-suffix convention documented at the
/// `role("gen")`/`role("cav")` call sites below (Mandatory Reuse — one label-tag grammar, one
/// parser).
pub(crate) fn parse_panel_tag(label: &str) -> Option<(u32, &str, u32, Option<&str>)> {
    let mut it = label.split(':');
    if it.next()? != "mp" {
        return None;
    }
    let kind: u32 = it.next()?.parse().ok()?;
    let role = it.next()?;
    let layer_id: u32 = it.next()?.parse().ok()?;
    Some((kind, role, layer_id, it.next()))
}

fn is_panel_node_for(n: &crate::shader_nodes::Node, kind: ChannelKind) -> bool {
    parse_panel_tag(&n.label).map(|(k, ..)| k == kind as u32).unwrap_or(false)
}

/// True when `kind`'s PRIMARY master socket is wired from a PANEL-GENERATED
/// subgraph (every node upstream of the wire carries this channel's panel tag).
pub(crate) fn panel_authored_socket(
    g: &crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
) -> bool {
    let Some(out) = g.output_node() else { return false };
    let Some(c) = g
        .connections
        .iter()
        .find(|c| c.to_node == out.id && c.to_input == kind_socket(kind))
    else {
        return false;
    };
    // Walk upstream from the wire source; every reachable node must be tagged.
    let mut stack = vec![c.from_node];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        match g.find_node(id) {
            Some(n) if is_panel_node_for(n, kind) => {}
            _ => return false,
        }
        for cc in &g.connections {
            if cc.to_node == id {
                // G4: a DRIVE edge into a generator's radius socket is a SANCTIONED
                // attachment point for arbitrary graph (ADR "Driven radius"): it must
                // not un-panel the chain, or authoring a drive collapses the channel
                // to a GraphDriven badge and the projection loses the layer view.
                let is_drive_edge = g
                    .find_node(id)
                    .and_then(|n| generator_drive_socket(&n.node_type))
                    .map(|sock| sock == cc.to_input)
                    .unwrap_or(false);
                if !is_drive_edge {
                    stack.push(cc.from_node);
                }
            }
        }
    }
    true
}

/// G2 — THE one decision function (MATERIAL_GRAPH_SOT_PLAN §G2): who drives this
/// channel, asked OF THE GRAPH. Every "is this channel unwired / panel-owned /
/// hand-authored" question routes through here — the panel reconcile, the doc
/// migration, `panel_generated_mask`, and `parse_panel_channel`'s gate. Never
/// re-derive it from `master_socket_driven`/`panel_authored_socket` combinations
/// at a call site: two combinators is how the 2026-08-15 curvature bug happened
/// (two builders, opposite conclusions about one body).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChannelSource {
    /// Primary master socket not wired — the scalar/colour value owns the channel.
    Unwired,
    /// Wired by a panel-generated subgraph (every upstream node carries the
    /// channel's `mp:` tag) — the panel layer stack owns it and can re-project it.
    PanelStack,
    /// Wired by anything else — a hand-authored graph. The panel must degrade
    /// (GraphDriven badge), never mangle (R5).
    HandWired,
}

pub(crate) fn channel_source(
    g: &crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
) -> ChannelSource {
    if !g.master_socket_driven(kind_socket(kind)) {
        return ChannelSource::Unwired;
    }
    if panel_authored_socket(g, kind) {
        return ChannelSource::PanelStack;
    }
    ChannelSource::HandWired
}

/// G4 prep: is `kind` a panel FAST channel — a PanelStack whose projected layer
/// stack lowers to `ChannelLowering::Fast` (single plain image, direct TexMap
/// install)? Composes the G2 one-query + the projection + the ONE lowering
/// classifier, so it can never disagree with what `lower_to_material` will do.
/// Bake deciders subtract these channels: a Fast install is not a bake.
pub(crate) fn panel_fast_channel(
    g: &crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    ts: TextureSpace,
) -> bool {
    if channel_source(g, kind) != ChannelSource::PanelStack {
        return false;
    }
    let Some(layers) = parse_panel_channel(g, kind) else { return false };
    let ch = Channel::with_state(kind, ChannelState::Layered, layers);
    channel_lowering(&ch, ts) == ChannelLowering::Fast
}

/// Bitmask (`ChannelKind as u32` bits) of channels whose master wiring is
/// panel-generated — the bake-install gate's allow-list (node_graph_bake).
pub(crate) fn panel_generated_mask(g: &crate::shader_nodes::NodeGraph) -> u32 {
    let mut mask = 0u32;
    for &kind in ALL_KINDS.iter() {
        if channel_source(g, kind) == ChannelSource::PanelStack {
            mask |= 1u32 << (kind as u32);
        }
    }
    mask
}

/// Remove every panel-generated node for `kind` (and its master wire, if it was
/// panel-authored). Hand-authored wiring is never touched.
pub(crate) fn strip_channel_subgraph(g: &mut crate::shader_nodes::NodeGraph, kind: ChannelKind) {
    g.channel_meta.remove(&(kind as u32));
    if panel_authored_socket(g, kind) {
        if let Some(out_id) = g.output_node().map(|n| n.id) {
            g.disconnect(out_id, kind_socket(kind));
        }
    }
    let doomed: Vec<u32> = g
        .nodes
        .iter()
        .filter(|n| is_panel_node_for(n, kind))
        .map(|n| n.id)
        .collect();
    for id in doomed {
        g.remove_node(id);
    }
}

/// G2 (deferred from G1): the ONE generator-node emitter, shared by the CONTENT
/// path (roles `gen`/`cav`/`dir`/`ao`/`thk`/`pos`) and the MASK path (`msk`-prefixed).
/// The two copies drifted once already — the mask emitter dropped region/sharpness/
/// remap and the parser hardcoded ConcaveOnly: the owner-reported "mask does not use
/// the same curvature as the generator" class. Emits exactly ONE node (allocation
/// order — deterministic codegen — preserved), wires the optional radius drive, and
/// returns `(node_id, output_socket)`. `None` = unsupported source (caller decides
/// continue vs bail).
/// ONE radius-drive socket mapping — used by BOTH `emit_generator_node` (wiring the
/// authored drive) and `parse_panel_channel` (recovering it). NodeType-keyed because
/// parse works from nodes; emit resolves through the node it just created.
fn generator_drive_socket(nt: &crate::shader_nodes::NodeType) -> Option<usize> {
    use crate::shader_nodes::NodeType as NT;
    match nt {
        NT::SdfCurvature | NT::Cavity | NT::SurfaceDirection => Some(1),
        NT::SdfThickness => Some(2),
        _ => None,
    }
}

fn emit_generator_node(
    g: &mut crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    lid: u32,
    pos: [f32; 2],
    for_mask: bool,
    source: &GeneratorSource,
    output: GeneratorOutput,
    radius: f32,
    sharpness: f32,
    axis_u32: u32,
    vertex_rung: bool,
    drive: Option<(u32, usize)>,
) -> Option<(u32, usize)> {
    use crate::shader_nodes::NodeType;
    let role = |base: &str| -> String {
        if for_mask {
            format!("msk{base}")
        } else {
            base.to_string()
        }
    };
    // BOTH MODES (CURVATURE_BOTH_MODES.md / ADR-001 amendment): the rung is a
    // SAMPLING DOMAIN tag, not a compositor choice — trailing `v` routes the node
    // through `vertex_curv_keys_for_graph` (SEED_POSITIONS); no `v` = SEED_PLANAR
    // map grid. Direction has no vertex kernel — never tags `v`.
    let rung_v = vertex_rung
        && matches!(
            *source,
            GeneratorSource::Curvature | GeneratorSource::Cavity
        );
    let (gen, from_out) = match source {
        GeneratorSource::Curvature => {
            let gen = g.add_node(NodeType::SdfCurvature, pos);
            if let Some(n) = g.find_node_mut(gen) {
                let flag = match (output, rung_v) {
                    (GeneratorOutput::Signed, false) => "sg",
                    (GeneratorOutput::ConvexOnly, false) => "cx",
                    (GeneratorOutput::ConcaveOnly, false) => "cc",
                    (GeneratorOutput::Signed, true) => "sgv",
                    (GeneratorOutput::ConvexOnly, true) => "cxv",
                    (GeneratorOutput::ConcaveOnly, true) => "ccv",
                };
                n.label = format!("{}:{}", panel_tag(kind, &role("gen"), lid), flag);
                n.sdf_prim_uid = 0;
                n.sdf_curv_region = match output {
                    GeneratorOutput::Signed => 0,
                    GeneratorOutput::ConvexOnly => 1,
                    GeneratorOutput::ConcaveOnly => 2,
                };
                n.pad_input_defaults();
                if let Some(s) = n.input_defaults.get_mut(1) {
                    s.value_f32 = radius.max(1e-4);
                }
                // Soft/Hard -> CONTRAST. SdfCurvature's contrast is socket 4 —
                // NOT 2, which is Cavity's index.
                if let Some(s) = n.input_defaults.get_mut(4) {
                    s.value_f32 = sharpness.max(1e-3);
                }
            }
            (gen, generator_output_socket(output))
        }
        GeneratorSource::Cavity => {
            let gen = g.add_node(NodeType::Cavity, pos);
            if let Some(n) = g.find_node_mut(gen) {
                n.label = if rung_v {
                    format!("{}:v", panel_tag(kind, &role("cav"), lid))
                } else {
                    panel_tag(kind, &role("cav"), lid)
                };
                n.pad_input_defaults();
                if let Some(s) = n.input_defaults.get_mut(1) {
                    s.value_f32 = radius.max(1e-4);
                }
                // Soft/Hard -> contrast (Cavity's contrast is socket 2).
                if let Some(s) = n.input_defaults.get_mut(2) {
                    s.value_f32 = sharpness.max(1e-3);
                }
            }
            (gen, 0usize)
        }
        GeneratorSource::Direction => {
            let gen = g.add_node(NodeType::SurfaceDirection, pos);
            if let Some(n) = g.find_node_mut(gen) {
                // The axis rides the tag flag so `parse_panel_channel` can recover
                // it without depending on the node's u32 surviving a round-trip —
                // same trick the curvature output selector uses.
                n.label = format!("{}:a{}", panel_tag(kind, &role("dir"), lid), axis_u32);
                n.sdf_prim_uid = 0;
                n.sdf_curv_region = axis_u32;
                n.pad_input_defaults();
                // Epsilon (FD step for the SDF gradient) — the layer Radius.
                if let Some(s) = n.input_defaults.get_mut(1) {
                    s.value_f32 = radius.max(1e-4);
                }
                // Soft/Hard -> Contrast (socket 4), matching SdfCurvature's mapping.
                if let Some(s) = n.input_defaults.get_mut(4) {
                    s.value_f32 = sharpness.max(1e-3);
                }
            }
            (gen, 0usize)
        }
        GeneratorSource::AmbientOcclusion => {
            // P5: real SDF AO — IQ-style samples along the outward normal. No
            // radius/sharpness sockets on the node — the authored values ride the
            // tag comma-packed purely for round-trip fidelity (G1): a backend that
            // ignores them is fine, LOSING them is not.
            let gen = g.add_node(NodeType::AmbientOcclusion, pos);
            if let Some(n) = g.find_node_mut(gen) {
                n.label = format!(
                    "{}:r{:.6},s{:.6}",
                    panel_tag(kind, &role("ao"), lid),
                    radius,
                    sharpness
                );
                n.sdf_prim_uid = 0;
                n.pad_input_defaults();
            }
            (gen, 0usize)
        }
        GeneratorSource::Thickness => {
            let gen = g.add_node(NodeType::SdfThickness, pos);
            if let Some(n) = g.find_node_mut(gen) {
                n.label = panel_tag(kind, &role("thk"), lid);
                n.sdf_prim_uid = 0;
                n.pad_input_defaults();
                // MaxDistance <- layer Radius.
                if let Some(s) = n.input_defaults.get_mut(2) {
                    s.value_f32 = radius.max(1e-4);
                }
                if let Some(s) = n.input_defaults.get_mut(3) {
                    s.value_f32 = sharpness.max(1e-3);
                }
            }
            (gen, 0usize)
        }
        GeneratorSource::Position => {
            let gen = g.add_node(NodeType::SdfPosition, pos);
            if let Some(n) = g.find_node_mut(gen) {
                // Axis code in tag + node (0=RGB, 1=X, 2=Y, 3=Z).
                n.label = format!(
                    "{}:a{},r{:.6},s{:.6}",
                    panel_tag(kind, &role("pos"), lid),
                    axis_u32 % 4,
                    radius,
                    sharpness
                );
                n.sdf_prim_uid = 0;
                n.sdf_curv_region = axis_u32 % 4;
                n.pad_input_defaults();
                if let Some(s) = n.input_defaults.get_mut(1) {
                    s.value_f32 = (axis_u32 % 4) as f32;
                }
            }
            // Content taps output 0; the mask factor taps Scalar Y (height dirt).
            (gen, if for_mask { 2usize } else { 0usize })
        }
        _ => return None,
    };
    let drive_sock = g.find_node(gen).and_then(|n| generator_drive_socket(&n.node_type));
    if let (Some((dn, dout)), Some(sock)) = (drive, drive_sock) {
        let _ = g.connect(dn, dout, gen, sock);
    }
    Some((gen, from_out))
}

/// DETERMINISTIC codegen (P3): (re)generate the channel's panel subgraph from
/// its layer stack. Same doc → identical structure (types, labels, params,
/// wiring) every time; node ids are allocation-order (identical from a fresh
/// graph). Existing panel nodes for the channel are stripped first, so regen is
/// idempotent.
///
/// ## One panel feature → one node (C1 / image-editor base)
///
/// Every authored control is a **named, discrete node** (or a single clearly
/// owned socket on that feature's node). Nothing is smuggled into a sibling
/// feature's defaults when a real node type exists:
///
/// | Feature | Tag role | Node |
/// |---------|----------|------|
/// | Image sample | `img` | `ImageTexture` (Mask fixed at 1 — strength is separate) |
/// | Noise | `nse` | `Noise` (Mask fixed at 1 — strength is separate) |
/// | Strength | `str` | `VectorMath` Scale after content |
/// | Constant | `kf`/`kc` | `ConstantColor` |
/// | Generator | `gen`/`cav`/`dir` + `grm`/`ginv`… | SDF nodes + Remap + Invert |
/// | UV tiling/offset | `tc` + `uvx` | `TexCoord` → `UVTransform` |
/// | Adjust | `adj` | `ColorCorrect` |
/// | Invert (adjust) | `inv` | `Invert` |
/// | Opacity | `op` | `ConstantValue` |
/// | Mask × opacity | `mfac` | `MathBinary` Multiply |
/// | Blend mode | `mix` | `MixRgb` (`mix_blend` + Factor from `op`/`mfac`) |
/// | Mask content | `msk*` | same content nodes, mask tags |
///
/// Chain per VISIBLE layer (bottom → top):
///
///   content → [str]? → [adj]? → [inv]? ─┐
///   [base]? ────────────────────────────┼→ mix → … → master socket
///   [mask → mfac ← op] ──→ mix.Factor ──┘
///
/// Returns true when a subgraph was emitted (socket wired).
pub(crate) fn regen_channel_subgraph(
    g: &mut crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    ch: &Channel,
    material_ts: TextureSpace,
    mat: &PbrMaterial,
) -> bool {
    use crate::shader_nodes::{NodeType, ProjectionStyle};
    strip_channel_subgraph(g, kind);
    // G4 PREP: FAST channels (single plain image) also emit their chain — the graph
    // must hold every authored construct before the doc can become a projection (G4).
    // Their TexMap install stays the DIRECT one (`lower_to_material`'s Fast arm) and the
    // bake path SUBTRACTS them (`bake_channels_from_graph`) — emitting the nodes changes
    // which sockets are wired, never what bakes or installs.
    if !matches!(
        crate::material_panel::lower::channel_lowering_full(ch, material_ts),
        (ChannelLowering::Subgraph | ChannelLowering::Fast, _)
            | (
                ChannelLowering::Stub,
                Some(crate::material_panel::lower::StubReason::NoVisibleLayers)
            )
    ) {
        return false;
    }
    let Some(out_id) = g.output_node().map(|n| n.id) else { return false };
    let mk = ch.kind.map_kind().expect("Subgraph requires a map slot");
    let x0 = -560.0f32;
    let mut prev: Option<u32> = None; // (node id) of the running composite, output 0
    let mut li_vis = 0usize;
    for (li, layer) in ch.layers.iter().enumerate() {
        // HIDDEN LAYERS ARE EMITTED, NOT SKIPPED (G1 gap 1). Skipping made the graph a lossy
        // representation: under graph-as-source-of-truth, hiding a layer and saving would
        // permanently DELETE it. A hidden layer emits its full node set (content, adjust, mask,
        // and a mix carrying blend/opacity) tagged with an `h` flag on the mix — but the
        // composite chain never consumes it, so evaluation is identical to the old skip.
        let hidden = !layer.visible;
        // Stable id in tags (P1). Fallback to 1-based stack index only if ensure_layer_ids was skipped.
        let lid = if layer.id != 0 { layer.id } else { (li as u32).saturating_add(1) };
        let y = li_vis as f32 * 220.0;
        // --- content ---
        let (inherited, space) = match layer.placement.space {
            Some(s) => (false, s),
            None => (true, material_ts),
        };
        let content = match &layer.content {
            LayerContent::Image { tex_id, strength } => {
                let img = g.add_node(NodeType::ImageTexture, [x0 + 160.0, y]);
                // Each layer's effective TextureSpace (own or inherited) is baked via
                // UVTransform when non-identity. Mult absolute maps display at uv_scale=1
                // so per-layer tiling lives in the composite (not one material-wide tile).
                let needs_uvx = space.tiling != [1.0, 1.0] || space.offset != [0.0, 0.0];
                if needs_uvx {
                    let tc = g.add_node(NodeType::TexCoord, [x0 - 160.0, y]);
                    let uvx = g.add_node(NodeType::UVTransform, [x0, y]);
                    if let Some(n) = g.find_node_mut(tc) {
                        n.label = panel_tag(kind, "tc", lid);
                    }
                    if let Some(n) = g.find_node_mut(uvx) {
                        n.label = panel_tag(kind, "uvx", lid);
                        n.pad_input_defaults();
                        let d = &mut n.input_defaults;
                        if let Some(s) = d.get_mut(1) { s.value_f32 = space.offset[0]; }
                        if let Some(s) = d.get_mut(2) { s.value_f32 = space.offset[1]; }
                        if let Some(s) = d.get_mut(4) { s.value_f32 = space.tiling[0]; }
                        if let Some(s) = d.get_mut(5) { s.value_f32 = space.tiling[1]; }
                    }
                    let _ = g.connect(tc, 0, uvx, 0); // position → UV
                    let _ = g.connect(uvx, 0, img, 0); // scaled/offset pos → Image
                }
                if let Some(n) = g.find_node_mut(img) {
                    // Flag encodes inherit-vs-override so derive round-trips
                    // `placement.space: None` exactly.
                    n.label = format!(
                        "{}:{}",
                        panel_tag(kind, "img", lid),
                        if inherited { "inh" } else { "ovr" }
                    );
                    n.image_map_kind = mk.wire_tag();
                    n.image_tex_id = *tex_id;
                    n.projection = projection_style_for(space.projection);
                    // Strength is its OWN node (`str` VectorMath Scale) — ImageTexture
                    // Mask stays at 1 so the sample and the strength feature stay 1:1
                    // inspectable / editable in the node graph.
                    n.pad_input_defaults();
                    if let Some(mi) =
                        crate::shader_nodes::Node::mask_input_index(&NodeType::ImageTexture)
                    {
                        if let Some(s) = n.input_defaults.get_mut(mi) {
                            s.value_f32 = 1.0;
                        }
                    }
                }
                emit_strength_scale(g, kind, lid, img, *strength, x0 + 250.0, y)
            }
            LayerContent::Constant { value } => {
                let k = g.add_node(NodeType::ConstantColor, [x0 + 160.0, y]);
                if let Some(n) = g.find_node_mut(k) {
                    let (role, c) = match value {
                        ChannelValue::Scalar(s) => ("kf", [*s, *s, *s]),
                        ChannelValue::Color(c) => ("kc", *c),
                    };
                    n.label = panel_tag(kind, role, lid);
                    n.ramp_color0 = c;
                }
                k
            }
            LayerContent::Noise {
                scale,
                strength,
                octaves,
                noise_type,
            } => {
                let n_id = g.add_node(NodeType::Noise, [x0 + 160.0, y]);
                if let Some(n) = g.find_node_mut(n_id) {
                    n.label = panel_tag(kind, "nse", lid);
                    n.noise_type = *noise_type;
                    n.projection = projection_style_for(space.projection);
                    n.pad_input_defaults();
                    let d = &mut n.input_defaults;
                    // Noise inputs: Offset, Scale, Octaves, Lacunarity, Gain, Distortion, Seed
                    if let Some(s) = d.get_mut(1) { s.value_f32 = *scale; }
                    if let Some(s) = d.get_mut(2) { s.value_f32 = *octaves; }
                    // Strength is its own `str` node (see emit_strength_scale).
                    if let Some(mi) =
                        crate::shader_nodes::Node::mask_input_index(&NodeType::Noise)
                    {
                        if let Some(s) = d.get_mut(mi) {
                            s.value_f32 = 1.0;
                        }
                    }
                }
                // ADR Decision 2: Driven scale → connect graph socket to Scale (input 1).
                if let Some(d) = layer.drive("scale") {
                    let _ = g.connect(d.node, d.output as usize, n_id, 1);
                }
                emit_strength_scale(g, kind, lid, n_id, *strength, x0 + 250.0, y)
            }
            // P6 generator content:
            //   SdfCurvature|Cavity → Remap(range, always) → [Invert]? → chain.
            // The range Remap is ALWAYS emitted (identity when remap==[0,1]) so
            // the layer's tail node exposes the value on OUTPUT 0 regardless of
            // which SdfCurvature output socket was selected — the downstream
            // blend chain wires output 0 unconditionally.
            LayerContent::Generator {
                source,
                radius,
                invert,
                sharpness,
                output,
                remap,
                rung,
                axis,
                ..
            } => {
                // BOTH MODES (CURVATURE_BOTH_MODES.md / ADR-001 amendment):
                // - Bake always owns COMPOSITING (Decision 1) — VertexCache still wires the map.
                // - The rung is a SAMPLING DOMAIN tag, not a compositor choice:
                //     Texture/Auto → flag without trailing `v` → SEED_PLANAR map grid
                //     VertexCache  → flag ends with `v` (`sgv`/`cxv`/`ccv`/`v`) →
                //                    `vertex_curv_keys_for_graph` → SEED_POSITIONS dispatch
                // Direction has no vertex kernel — never tags `v`.
                // G2: ONE generator-node emitter for content AND mask paths —
                // the copies drifted once already (the G1 curvature class).
                let drive = layer.drive("radius").map(|d| (d.node, d.output as usize));
                let Some((gen, from_out)) = emit_generator_node(
                    g,
                    kind,
                    lid,
                    [x0 + 160.0, y],
                    false,
                    source,
                    *output,
                    *radius,
                    *sharpness,
                    axis.as_u32(),
                    *rung == GeneratorRung::VertexCache,
                    drive,
                ) else {
                    continue;
                };
                let mut sig = false;
                // Signed κ (−1..1) → normalised 0..1 with 0.5 = flat (spec P6).
                if *source == GeneratorSource::Curvature && *output == GeneratorOutput::Signed {
                    sig = true;
                }
                let mut tail_from = (gen, from_out);
                if sig {
                    let gn = g.add_node(NodeType::Remap, [x0 + 300.0, y]);
                    if let Some(n) = g.find_node_mut(gn) {
                        n.label = panel_tag(kind, "gnorm", lid);
                        n.pad_input_defaults();
                        let d = &mut n.input_defaults;
                        if let Some(s) = d.get_mut(1) { s.value_f32 = -1.0; }
                        if let Some(s) = d.get_mut(2) { s.value_f32 = 1.0; }
                        if let Some(s) = d.get_mut(3) { s.value_f32 = 0.0; }
                        if let Some(s) = d.get_mut(4) { s.value_f32 = 1.0; }
                    }
                    let _ = g.connect(tail_from.0, tail_from.1, gn, 0);
                    tail_from = (gn, 0);
                }
                // Range remap [min..max] → 0..1 (Remap clamps) — always emitted.
                let rm = g.add_node(NodeType::Remap, [x0 + 440.0, y]);
                if let Some(n) = g.find_node_mut(rm) {
                    n.label = panel_tag(kind, "grm", lid);
                    n.pad_input_defaults();
                    let d = &mut n.input_defaults;
                    if let Some(s) = d.get_mut(1) { s.value_f32 = remap[0]; }
                    if let Some(s) = d.get_mut(2) { s.value_f32 = remap[1]; }
                    if let Some(s) = d.get_mut(3) { s.value_f32 = 0.0; }
                    if let Some(s) = d.get_mut(4) { s.value_f32 = 1.0; }
                }
                let _ = g.connect(tail_from.0, tail_from.1, rm, 0);
                let mut tail = rm;
                if *invert {
                    let gi = g.add_node(NodeType::Invert, [x0 + 560.0, y]);
                    if let Some(n) = g.find_node_mut(gi) {
                        n.label = panel_tag(kind, "ginv", lid);
                    }
                    let _ = g.connect(tail, 0, gi, 0);
                    tail = gi;
                }
                tail
            }
        };
        // --- adjust ---
        let mut top = content;
        if !layer.adjust.is_default() {
            let cc = g.add_node(NodeType::ColorCorrect, [x0 + 340.0, y]);
            if let Some(n) = g.find_node_mut(cc) {
                n.label = panel_tag(kind, "adj", lid);
                n.pad_input_defaults();
                let d = &mut n.input_defaults;
                if let Some(s) = d.get_mut(2) { s.value_f32 = layer.adjust.saturation; }
                if let Some(s) = d.get_mut(4) { s.value_f32 = layer.adjust.gamma; }
                if let Some(s) = d.get_mut(6) { s.value_f32 = layer.adjust.contrast; }
            }
            let _ = g.connect(top, 0, cc, 0);
            top = cc;
        }
        if layer.adjust.invert {
            let inv = g.add_node(NodeType::Invert, [x0 + 480.0, y]);
            if let Some(n) = g.find_node_mut(inv) {
                n.label = panel_tag(kind, "inv", lid);
            }
            let _ = g.connect(top, 0, inv, 0);
            top = inv;
        }
        // --- blend into the running composite ---
        let below = match prev {
            Some(p) => Some(p),
            None if (layer.opacity - 1.0).abs() > 1e-6 || layer.blend != BlendMode::Normal => {
                // Bottom layer blending needs a base: the channel's live value.
                let base = g.add_node(NodeType::ConstantColor, [x0, y + 120.0]);
                if let Some(n) = g.find_node_mut(base) {
                    n.label = panel_tag(kind, "base", lid);
                    n.ramp_color0 = match channel_value_of(kind, mat) {
                        ChannelValue::Scalar(s) => [s, s, s],
                        ChannelValue::Color(c) => c,
                    };
                }
                Some(base)
            }
            None => None,
        };
        // Per-layer MASK stack (P3 recursive) → scalar node multiplies mix Factor.
        let mask_tail = emit_mask_stack(g, kind, lid, &layer.mask, mk, x0 + 620.0, y + 160.0, 0);
        // A masked Normal layer at opacity 1 still needs a blend (the mask makes
        // it partial), so force a base when a mask is present.
        let has_mask = mask_tail.is_some();
        let below = match below {
            Some(b) => Some(b),
            None if has_mask || hidden => {
                let base = g.add_node(NodeType::ConstantColor, [x0, y + 120.0]);
                if let Some(n) = g.find_node_mut(base) {
                    n.label = panel_tag(kind, "base", lid);
                    n.ramp_color0 = match channel_value_of(kind, mat) {
                        ChannelValue::Scalar(s) => [s, s, s],
                        ChannelValue::Color(c) => c,
                    };
                }
                Some(base)
            }
            None => None,
        };
        let composed = match below {
            None => top,
            Some(below) => {
                // Blend mode = MixRgb node ONLY (mix_blend param). Opacity is a
                // SEPARATE ConstantValue (`op`) so each panel feature is its own
                // node in the graph (C1 / image-editor base). Factor =
                // op | mask*op via mfac.
                let mix = g.add_node(NodeType::MixRgb, [x0 + 780.0, y]);
                if let Some(n) = g.find_node_mut(mix) {
                    n.label = panel_tag(kind, "mix", lid);
                    n.mix_blend = layer.blend.to_mix_blend();
                    n.pad_input_defaults();
                    // Factor default is a fallback only — normally driven by `op`.
                    if let Some(s) = n.input_defaults.get_mut(0) {
                        s.value_f32 = 1.0;
                    }
                }
                // Opacity as its own ConstantValue node (always emitted when we blend).
                let op = g.add_node(NodeType::ConstantValue, [x0 + 560.0, y + 80.0]);
                if let Some(n) = g.find_node_mut(op) {
                    n.label = panel_tag(kind, "op", lid);
                    n.const_float = layer.opacity;
                }
                // factor = mask * opacity (MathBinary Multiply) when masked.
                // P2: every mask (including former VertexCache κ) drives Mix Factor in the bake.
                if let Some(mtail) = mask_tail {
                    let mul = g.add_node(NodeType::MathBinary, [x0 + 640.0, y + 80.0]);
                    if let Some(n) = g.find_node_mut(mul) {
                        n.label = panel_tag(kind, "mfac", lid);
                        n.math_binary_op = crate::shader_nodes::MathBinaryOp::Multiply;
                        n.pad_input_defaults();
                    }
                    let _ = g.connect(mtail, 0, mul, 0); // A = mask
                    let _ = g.connect(op, 0, mul, 1); // B = opacity (own node)
                    let _ = g.connect(mul, 0, mix, 0); // → Factor
                } else {
                    // No mask: opacity node drives Mix Factor directly.
                    let _ = g.connect(op, 0, mix, 0);
                }
                let ok1 = g.connect(below, 0, mix, 1); // Color1 = below (base)
                let ok2 = g.connect(top, 0, mix, 2); // Color2 = this layer
                // Silent connect fail left Color2 at default white → Mult became identity
                // (base×1 = base): "noise Mult does nothing". Fail loud in debug.
                debug_assert!(
                    ok1 && ok2,
                    "panel Mult/blend wire failed: below→mix.Color1={ok1} layer→mix.Color2={ok2}"
                );
                if !ok1 || !ok2 {
                    eprintln!(
                        "[MaterialPanel] blend wire failed for {:?} layer {} (below={}, layer={}) — Mult may look broken",
                        kind, li, ok1, ok2
                    );
                }
                mix
            }
        };
        if hidden {
            // `below` was forced Some above, so `composed` IS this layer's mix node.
            // The `h` flag is how the parser recovers `visible: false`; leaving `prev`
            // untouched is how evaluation stays byte-identical to the old skip.
            if let Some(n) = g.find_node_mut(composed) {
                n.label = format!("{}:h", panel_tag(kind, "mix", lid));
            }
        } else {
            prev = Some(composed);
            li_vis += 1;
        }
    }
    // G4: per-channel authored state with no node home rides `channel_meta`
    // (bit depth, Live/Bake, the eye). Written on EVERY regen so it can't go stale.
    g.channel_meta.insert(
        kind as u32,
        crate::shader_nodes::ChannelMeta {
            bit16: matches!(ch.bit_depth, crate::material_panel::model::ChannelBitDepth::Bit16),
            live: matches!(ch.eval_mode, crate::material_panel::model::ChannelEvalMode::Live),
            disabled: !ch.enabled,
        },
    );
    match prev {
        Some(top) => {
            // NOISE (or any scalar generator) IN THE NORMAL CHANNEL IS A HEIGHT FIELD, NOT A
            // NORMAL (owner 2026-07-28: "adding a noise inside normal should automatically
            // change it to a normal output").
            //
            // Wiring a scalar straight into MO_NORMAL feeds the shader a grey RGB value where it
            // expects a tangent-space vector — the surface reads flat-but-tinted, never bumpy.
            // The bake ALREADY knows how to do the right thing: `bake_node_graph_maps_res` runs
            // a Sobel height→normal pass, but only `if height_wired && !normal_wired`
            // (node_graph_bake.rs:955). Wiring MO_NORMAL is exactly what suppressed it.
            //
            // So route scalar-producing content in the Normal channel to MO_HEIGHT instead and
            // let that existing pass derive the normal. An IMAGE layer keeps MO_NORMAL — a
            // loaded normal map is already a vector and must NOT be re-derived.
            let socket = if kind == ChannelKind::Normal && stack_is_scalar_source(ch) {
                crate::shader_nodes::NodeGraph::MO_HEIGHT
            } else {
                kind_socket(kind)
            };
            // G4 EYE TOGGLE (MPEYEOFFREBAKE, evolved): a DISABLED channel keeps its full
            // chain in the graph — the projection needs the layers to survive — but the
            // master socket stays UNWIRED, which makes it inert for bake/eval BY
            // CONSTRUCTION: no bake decider, mask builder or evaluator needs to know
            // about the eye at all. Re-enabling regens with the wire restored.
            if ch.enabled {
                g.connect(top, 0, out_id, socket)
            } else {
                true
            }
        }
        None => false,
    }
}

/// Strength as its own node: `VectorMath` Scale after content.
///
/// Image/Noise used to bury strength in the trailing Mask socket of the content
/// node. That violated "one panel feature → one node" — the graph could not show
/// or rewire Strength independently. Always emitted (even at 1.0) so the feature
/// is always present and round-trips through `role("str")`.
fn emit_strength_scale(
    g: &mut crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    lid: u32,
    content: u32,
    strength: f32,
    x: f32,
    y: f32,
) -> u32 {
    use crate::shader_nodes::{NodeType, VectorMathOp};
    let str_id = g.add_node(NodeType::VectorMath, [x, y]);
    if let Some(n) = g.find_node_mut(str_id) {
        n.label = panel_tag(kind, "str", lid);
        n.vector_math_op = VectorMathOp::Scale;
        n.pad_input_defaults();
        // VectorMath inputs: A(vec3), B(vec3), Scale(float) — Scale is index 2.
        if let Some(s) = n.input_defaults.get_mut(2) {
            s.value_f32 = strength;
        }
    }
    let _ = g.connect(content, 0, str_id, 0); // A = content color/value
    str_id
}

/// True when every visible layer in the stack produces a SCALAR height-like signal rather than a
/// ready-made vector — i.e. Noise / Constant / a supported Generator. An `Image` layer is assumed
/// to be an authored normal map and excluded, so loading one still wires MO_NORMAL directly.
///
/// Used only for the Normal channel (see the call site): scalar content there means "height",
/// which the bake's Sobel pass converts into a real tangent-space normal.
// pub(super): tests in lower/mod.rs call this via `use super::*` (was same-file private).
pub(super) fn stack_is_scalar_source(ch: &Channel) -> bool {
    let vis: Vec<&Layer> = ch.layers.iter().filter(|l| l.visible).collect();
    !vis.is_empty()
        && vis.iter().all(|l| {
            matches!(
                l.content,
                LayerContent::Noise { .. }
                    | LayerContent::Constant { .. }
                    | LayerContent::Generator { .. }
            )
        })
}

/// Emit a full mask **stack** (P3 recursive). Visible entries composite bottom→top
/// with each entry's blend/opacity. Nested `MaskLayer.mask` is evaluated first
/// (mask-on-mask) up to [`MASK_STACK_MAX_DEPTH`].
fn emit_mask_stack(
    g: &mut crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    lid: u32,
    stack: &MaskStack,
    mk: core_math::pbr_material::MapKind,
    x: f32,
    y: f32,
    depth: usize,
) -> Option<u32> {
    use crate::shader_nodes::{NodeType, MathBinaryOp};
    if stack.is_empty() || depth >= MASK_STACK_MAX_DEPTH {
        return None;
    }
    let mut prev: Option<u32> = None;
    let mut i = 0usize;
    for ml in stack.layers.iter().filter(|l| l.visible) {
        // Tag with parent content layer `lid` so parse_panel_tag groups mask with content.
        // Nested depth uses role suffixes (msknest) rather than retagging layer id.
        let mut tail = emit_mask_content(g, kind, lid, &ml.content, mk, x, y + i as f32 * 40.0)?;
        // Nested mask on this entry multiplies coverage.
        if let Some(nested) = emit_mask_stack(
            g,
            kind,
            lid,
            &ml.mask,
            mk,
            x + 80.0,
            y + i as f32 * 40.0 + 20.0,
            depth + 1,
        ) {
            let mul = g.add_node(NodeType::MathBinary, [x + 120.0, y + i as f32 * 40.0]);
            if let Some(n) = g.find_node_mut(mul) {
                n.label = panel_tag(kind, "msknest", lid);
                n.math_binary_op = MathBinaryOp::Multiply;
                n.pad_input_defaults();
            }
            let _ = g.connect(tail, 0, mul, 0);
            let _ = g.connect(nested, 0, mul, 1);
            tail = mul;
        }
        // Opacity on this mask entry.
        if (ml.opacity - 1.0).abs() > 1e-4 {
            let op = g.add_node(NodeType::ConstantValue, [x + 160.0, y + i as f32 * 40.0]);
            if let Some(n) = g.find_node_mut(op) {
                n.label = panel_tag(kind, "mskop", lid);
                n.const_float = ml.opacity;
            }
            let mul = g.add_node(NodeType::MathBinary, [x + 200.0, y + i as f32 * 40.0]);
            if let Some(n) = g.find_node_mut(mul) {
                n.label = panel_tag(kind, "mskopa", lid);
                n.math_binary_op = MathBinaryOp::Multiply;
                n.pad_input_defaults();
            }
            let _ = g.connect(tail, 0, mul, 0);
            let _ = g.connect(op, 0, mul, 1);
            tail = mul;
        }
        prev = Some(match prev {
            None => tail,
            Some(below) => {
                // Combine with below using MixRgb + blend (over with fac=1 for mask stack).
                let mix = g.add_node(NodeType::MixRgb, [x + 280.0, y + i as f32 * 40.0]);
                if let Some(n) = g.find_node_mut(mix) {
                    n.label = panel_tag(kind, "mskmix", lid);
                    n.mix_blend = ml.blend.to_mix_blend();
                    n.pad_input_defaults();
                    if let Some(s) = n.input_defaults.get_mut(0) {
                        s.value_f32 = 1.0;
                    }
                }
                let _ = g.connect(below, 0, mix, 1);
                let _ = g.connect(tail, 0, mix, 2);
                mix
            }
        });
        i += 1;
    }
    prev
}

/// Emit a single mask **content** node (leaf). Output 0 is scalar 0..1.
fn emit_mask_content(
    g: &mut crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
    lid: u32,
    mask: &LayerContent,
    mk: core_math::pbr_material::MapKind,
    x: f32,
    y: f32,
) -> Option<u32> {
    use crate::shader_nodes::{NodeType, ProjectionStyle};
    match mask {
        LayerContent::Image { tex_id, strength } => {
            let img = g.add_node(NodeType::ImageTexture, [x, y]);
            if let Some(n) = g.find_node_mut(img) {
                n.label = format!("{}:inh", panel_tag(kind, "mskimg", lid));
                n.image_map_kind = mk.wire_tag();
                n.image_tex_id = *tex_id;
                n.projection = ProjectionStyle::Triplanar;
                n.pad_input_defaults();
                if let Some(mi) =
                    crate::shader_nodes::Node::mask_input_index(&NodeType::ImageTexture)
                {
                    if let Some(s) = n.input_defaults.get_mut(mi) {
                        s.value_f32 = *strength;
                    }
                }
            }
            Some(img)
        }
        LayerContent::Constant { value } => {
            let k = g.add_node(NodeType::ConstantColor, [x, y]);
            if let Some(n) = g.find_node_mut(k) {
                n.label = panel_tag(kind, "mskk", lid);
                n.ramp_color0 = match value {
                    ChannelValue::Scalar(s) => [*s, *s, *s],
                    ChannelValue::Color(c) => *c,
                };
            }
            Some(k)
        }
        LayerContent::Generator {
            source,
            radius,
            invert,
            sharpness,
            output,
            remap,
            rung,
            axis,
            ..
        } => {
            // BOTH MODES: VertexCache masks still bake (Decision 1) but tag trailing `v` so the
            // SEED_POSITIONS path can discover them. No white mskfull / G-buffer κ composite.
            // G2: same ONE emitter as the content path; masks pass no drive
            // (unchanged behavior) and Position taps Scalar Y via `for_mask`.
            let Some((gen, from_out)) = emit_generator_node(
                g,
                kind,
                lid,
                [x, y],
                true,
                source,
                *output,
                *radius,
                *sharpness,
                axis.as_u32(),
                *rung == GeneratorRung::VertexCache,
                None,
            ) else {
                return None;
            };
            let mut tail = (gen, from_out);
            if *invert {
                let inv = g.add_node(NodeType::Invert, [x + 140.0, y]);
                if let Some(n) = g.find_node_mut(inv) {
                    n.label = panel_tag(kind, "mskinv", lid);
                }
                let _ = g.connect(tail.0, tail.1, inv, 0);
                tail = (inv, 0);
            }
            // SIGNED CURVATURE MUST BE NORMALIZED — the same way the CONTENT generator does it
            // (owner 2026-07-28: "curvature mask do not use the same curvature as the
            // generator").
            //
            // `SdfCurvature`'s Signed output is [-1, 1] (0 = flat). The content path wraps it in
            // a Remap [-1,1] -> [0,1] (`gnorm`, see `emit_generator_layer`), so the user's radius
            // produces the value they see in the layer. The mask path did NOT: it emitted an
            // IDENTITY remap (0..1 -> 0..1) and only when `tail.1 != 0`, so a Signed mask fed raw
            // negative values straight into the mix factor, where everything concave clamped to 0.
            // Same node, same radius, visibly different result — exactly the report.
            //
            // Convex/Concave outputs are already 0..1 masks and must NOT be re-ranged, so the
            // normalization is gated on Signed only, matching the content path's `sig` condition.
            if matches!(output, GeneratorOutput::Signed) {
                let gn = g.add_node(NodeType::Remap, [x + 210.0, y]);
                if let Some(n) = g.find_node_mut(gn) {
                    n.label = panel_tag(kind, "msknorm", lid);
                    n.pad_input_defaults();
                    let d = &mut n.input_defaults;
                    if let Some(s) = d.get_mut(1) { s.value_f32 = -1.0; }
                    if let Some(s) = d.get_mut(2) { s.value_f32 = 1.0; }
                    if let Some(s) = d.get_mut(3) { s.value_f32 = 0.0; }
                    if let Some(s) = d.get_mut(4) { s.value_f32 = 1.0; }
                }
                let _ = g.connect(tail.0, tail.1, gn, 0);
                tail = (gn, 0);
            }
            // Range remap — ALWAYS emitted, carrying the AUTHORED [min..max] (content-path
            // parity). The old form emitted identity, and only when the source socket was
            // non-zero: the user's remap knob was silently ignored in mask evaluation AND
            // lost on round-trip. Honoring it is both the fidelity fix (G1) and the
            // owner-reported "mask does not use the same curvature as the generator" class.
            {
                let rm = g.add_node(NodeType::Remap, [x + 280.0, y]);
                if let Some(n) = g.find_node_mut(rm) {
                    n.label = panel_tag(kind, "mskrm", lid);
                    n.pad_input_defaults();
                    let d = &mut n.input_defaults;
                    if let Some(s) = d.get_mut(1) { s.value_f32 = remap[0]; }
                    if let Some(s) = d.get_mut(2) { s.value_f32 = remap[1]; }
                    if let Some(s) = d.get_mut(3) { s.value_f32 = 0.0; }
                    if let Some(s) = d.get_mut(4) { s.value_f32 = 1.0; }
                }
                let _ = g.connect(tail.0, tail.1, rm, 0);
                tail = (rm, 0);
            }
            Some(tail.0)
        }
        // Photoshop-style mask can be procedural noise (luminance 0..1).
        LayerContent::Noise {
            scale,
            strength,
            octaves,
            noise_type,
        } => {
            let n_id = g.add_node(NodeType::Noise, [x, y]);
            if let Some(n) = g.find_node_mut(n_id) {
                n.label = panel_tag(kind, "msknse", lid);
                n.noise_type = *noise_type;
                n.projection = ProjectionStyle::Triplanar;
                n.pad_input_defaults();
                let d = &mut n.input_defaults;
                if let Some(s) = d.get_mut(1) {
                    s.value_f32 = *scale;
                }
                if let Some(s) = d.get_mut(2) {
                    s.value_f32 = *octaves;
                }
                if let Some(mi) = crate::shader_nodes::Node::mask_input_index(&NodeType::Noise) {
                    if let Some(s) = d.get_mut(mi) {
                        s.value_f32 = *strength;
                    }
                }
            }
            Some(n_id)
        }
    }
}

/// ROUND-TRIP inverse of [`regen_channel_subgraph`]: rebuild the layer stack
/// from a panel-generated subgraph. Returns `None` when the socket is not
/// panel-authored (hand-edited graphs stay `GraphDriven`, exactly as P2).
/// Hidden layers are not representable in the graph and cannot be recovered —
/// derive-from-graph is the doc-store-lost migration path only (documented).
pub(crate) fn parse_panel_channel(
    g: &crate::shader_nodes::NodeGraph,
    kind: ChannelKind,
) -> Option<Vec<Layer>> {
    use crate::shader_nodes::{NodeType, ProjectionStyle};
    // G4: `channel_meta` presence is the EXPLICIT "the panel authored this channel"
    // marker — regen writes it, strip removes it, hand graphs never have it. It lets
    // the projection recover chains whose socket is DELIBERATELY UNWIRED (the eye
    // toggle, an all-hidden stack) without guessing from leftover tags (R4).
    // It must NOT extend to a WIRED-but-not-PanelStack socket: a hand node spliced
    // into the chain (Invert, Reroute, …) means the user modified the channel, and
    // recovering "the layers without their edit" would let the next regen destroy
    // it — the exact R5 mangling this projection must never do. Those degrade.
    let panel_meta = g.channel_meta.contains_key(&(kind as u32));
    match channel_source(g, kind) {
        ChannelSource::PanelStack => {}
        ChannelSource::Unwired if panel_meta => {}
        _ => return None,
    }
    // Group this channel's panel nodes by layer index.
    let mut by_layer: std::collections::BTreeMap<usize, Vec<&crate::shader_nodes::Node>> =
        std::collections::BTreeMap::new();
    for n in &g.nodes {
        if let Some((k, _role, li, _)) = parse_panel_tag(&n.label) {
            if k == kind as u32 {
                by_layer.entry(li as usize).or_default().push(n);
            }
        }
    }
    let mut layers = Vec::new();
    for (li, nodes) in by_layer {
        let role = |r: &str| -> Option<&crate::shader_nodes::Node> {
            nodes
                .iter()
                .copied()
                .find(|n| parse_panel_tag(&n.label).map(|(_, ro, ..)| ro == r).unwrap_or(false))
        };
        // Strength feature node (`str` VectorMath Scale). Falls back to legacy
        // content-node Mask default for pre-split graphs still in memory.
        let strength_from_nodes = |content_nt: crate::shader_nodes::NodeType,
                                   content: &crate::shader_nodes::Node|
         -> f32 {
            if let Some(str_n) = role("str") {
                return crate::shader_nodes::Node::default_f32(str_n, 2, 1.0);
            }
            crate::shader_nodes::Node::mask_input_index(&content_nt)
                .map(|mi| crate::shader_nodes::Node::default_f32(content, mi, 1.0))
                .unwrap_or(1.0)
        };
        // Content
        let (content, placement) = if let Some(img) = role("img") {
            debug_assert!(matches!(img.node_type, NodeType::ImageTexture));
            let strength = strength_from_nodes(NodeType::ImageTexture, img);
            let inherited = parse_panel_tag(&img.label)
                .and_then(|(.., f)| f)
                .map(|f| f == "inh")
                .unwrap_or(false);
            let projection = map_projection_for(img.projection).as_u32();
            let (tiling, offset) = match role("uvx") {
                Some(uvx) => (
                    [
                        crate::shader_nodes::Node::default_f32(uvx, 4, 1.0),
                        crate::shader_nodes::Node::default_f32(uvx, 5, 1.0),
                    ],
                    [
                        crate::shader_nodes::Node::default_f32(uvx, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(uvx, 2, 0.0),
                    ],
                ),
                None => ([1.0, 1.0], [0.0, 0.0]),
            };
            (
                LayerContent::Image { tex_id: img.image_tex_id, strength },
                Placement {
                    space: if inherited {
                        None
                    } else {
                        Some(TextureSpace {
                            projection,
                            tiling,
                            offset,
                            ..TextureSpace::default()
                        })
                    },
                },
            )
        } else if let Some(nse) = role("nse") {
            let scale = crate::shader_nodes::Node::default_f32(nse, 1, 4.0);
            let octaves = crate::shader_nodes::Node::default_f32(nse, 2, 4.0);
            let strength = strength_from_nodes(NodeType::Noise, nse);
            (
                LayerContent::Noise {
                    scale,
                    strength,
                    octaves,
                    noise_type: nse.noise_type,
                },
                Placement::default(),
            )
        } else if let Some(gen) = role("gen") {
            // P6 Curvature generator (SdfCurvature node).
            debug_assert!(matches!(gen.node_type, NodeType::SdfCurvature));
            let flag = parse_panel_tag(&gen.label).and_then(|(.., f)| f);
            let output = match flag {
                Some("cx") | Some("cxv") => GeneratorOutput::ConvexOnly,
                Some("cc") | Some("ccv") => GeneratorOutput::ConcaveOnly,
                _ => GeneratorOutput::Signed,
            };
            // P7: trailing `v` on the flag = VertexCache rung (round-trip only;
            // the emitted subgraph is identical either way).
            let rung = if flag.is_some_and(|f| f.ends_with('v')) {
                GeneratorRung::VertexCache
            } else {
                GeneratorRung::Auto
            };
            let radius = crate::shader_nodes::Node::default_f32(gen, 1, 0.02);
            // Soft/Hard round-trips from the CONTRAST socket it is emitted into (see the
            // emitter). Without this the slider reset to 1.0 — a hard edge — every reload.
            // 1.5 is SdfCurvature's OWN default for socket 4 (`graph.rs`: contrast), not 1.0 —
            // reading back with the wrong fallback made an untouched node parse as a CHANGED
            // value and broke the round-trip.
            let sharpness = crate::shader_nodes::Node::default_f32(gen, 4, 1.5);
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            (
                LayerContent::Generator {
                    source: GeneratorSource::Curvature,
                    radius,
                    invert: role("ginv").is_some(),
                    output,
                    remap,
                    rung,
                    sharpness,
                    // Not a Direction layer — the field is inert, keep the default.
                    axis: DirectionAxis::default(),
                },
                Placement::default(),
            )
        } else if let Some(dir) = role("dir") {
            // P8 Direction generator (SurfaceDirection node). Axis rides the `:aN` tag flag,
            // falling back to the node's own `sdf_curv_region` and finally to +Y.
            debug_assert!(matches!(dir.node_type, NodeType::SurfaceDirection));
            let axis = parse_panel_tag(&dir.label)
                .and_then(|(.., f)| f)
                .and_then(|f| f.strip_prefix('a'))
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(dir.sdf_curv_region);
            let radius = crate::shader_nodes::Node::default_f32(dir, 1, 0.02);
            // Soft/Hard round-trips from CONTRAST (socket 4). The fallback MUST be
            // SurfaceDirection's OWN socket-4 default (1.0) or an untouched node parses as a
            // changed value and the round-trip test fails — the exact trap SdfCurvature's 1.5
            // comment documents.
            let sharpness = crate::shader_nodes::Node::default_f32(dir, 4, 1.0);
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            (
                LayerContent::Generator {
                    source: GeneratorSource::Direction,
                    radius,
                    invert: role("ginv").is_some(),
                    output: GeneratorOutput::default(),
                    remap,
                    // Always Auto — Direction has no vertex rung (see the emitter).
                    rung: GeneratorRung::Auto,
                    sharpness,
                    axis: DirectionAxis::from_u32(axis),
                },
                Placement::default(),
            )
        } else if let Some(ao) = role("ao") {
            // P5 Ambient Occlusion (body SDF, IQ-style calc_ao).
            debug_assert!(matches!(ao.node_type, NodeType::AmbientOcclusion));
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            // Tag-carried params (no sockets on the AO node) — see the emitter.
            let flags = parse_panel_tag(&ao.label).and_then(|(.., f)| f.map(|s| s.to_string()));
            let tagf = |prefix: char, dflt: f32| -> f32 {
                flags
                    .as_ref()
                    .and_then(|f| {
                        f.split(',').find_map(|p| {
                            p.strip_prefix(prefix).and_then(|v| v.parse::<f32>().ok())
                        })
                    })
                    .unwrap_or(dflt)
            };
            (
                LayerContent::Generator {
                    source: GeneratorSource::AmbientOcclusion,
                    // Radius is UI-only for the node today; the TAG keeps it lossless (G1).
                    radius: tagf('r', 0.02),
                    invert: role("ginv").is_some(),
                    output: GeneratorOutput::default(),
                    remap,
                    rung: GeneratorRung::Auto,
                    sharpness: tagf('s', 1.0),
                    axis: DirectionAxis::default(),
                },
                Placement::default(),
            )
        } else if let Some(thk) = role("thk") {
            debug_assert!(matches!(thk.node_type, NodeType::SdfThickness));
            let radius = crate::shader_nodes::Node::default_f32(thk, 2, 0.25);
            let sharpness = crate::shader_nodes::Node::default_f32(thk, 3, 1.0);
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            (
                LayerContent::Generator {
                    source: GeneratorSource::Thickness,
                    radius,
                    invert: role("ginv").is_some(),
                    output: GeneratorOutput::default(),
                    remap,
                    rung: GeneratorRung::Auto,
                    sharpness,
                    axis: DirectionAxis::default(),
                },
                Placement::default(),
            )
        } else if let Some(pos) = role("pos") {
            debug_assert!(matches!(pos.node_type, NodeType::SdfPosition));
            // Flag is comma-packed ("a3,r0.03,s0.7") — split before stripping, or the axis
            // parse fails on "3,r0.03..." and silently falls back (the G1 PosY->PosX gap).
            let flags = parse_panel_tag(&pos.label).and_then(|(.., f)| f.map(|s| s.to_string()));
            let axis = flags
                .as_ref()
                .and_then(|f| {
                    f.split(',')
                        .find_map(|p| p.strip_prefix('a').and_then(|v| v.parse::<u32>().ok()))
                })
                .unwrap_or(pos.sdf_curv_region);
            let tagf = |prefix: char, dflt: f32| -> f32 {
                flags
                    .as_ref()
                    .and_then(|f| {
                        f.split(',').find_map(|p| {
                            p.strip_prefix(prefix).and_then(|v| v.parse::<f32>().ok())
                        })
                    })
                    .unwrap_or(dflt)
            };
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            (
                LayerContent::Generator {
                    source: GeneratorSource::Position,
                    radius: tagf('r', 0.02),
                    invert: role("ginv").is_some(),
                    output: GeneratorOutput::default(),
                    remap,
                    rung: GeneratorRung::Auto,
                    sharpness: tagf('s', 1.0),
                    axis: DirectionAxis::from_u32(axis),
                },
                Placement::default(),
            )
        } else if let Some(cav) = role("cav") {
            // P6 Cavity generator (concave-only; output selector inert).
            debug_assert!(matches!(cav.node_type, NodeType::Cavity));
            let radius = crate::shader_nodes::Node::default_f32(cav, 1, 0.02);
            let sharpness = crate::shader_nodes::Node::default_f32(cav, 2, 1.0);
            let remap = role("grm")
                .map(|r| {
                    [
                        crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                        crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                    ]
                })
                .unwrap_or([0.0, 1.0]);
            (
                LayerContent::Generator {
                    source: GeneratorSource::Cavity,
                    radius,
                    invert: role("ginv").is_some(),
                    output: GeneratorOutput::default(),
                    remap,
                    rung: if parse_panel_tag(&cav.label).and_then(|(.., f)| f) == Some("v") {
                        GeneratorRung::VertexCache
                    } else {
                        GeneratorRung::Auto
                    },
                sharpness,
                    axis: DirectionAxis::default(),
                },
                Placement::default(),
            )
        } else if let Some(k) = role("kf") {
            (
                LayerContent::Constant { value: ChannelValue::Scalar(k.ramp_color0[0]) },
                Placement::default(),
            )
        } else if let Some(k) = role("kc") {
            (
                LayerContent::Constant { value: ChannelValue::Color(k.ramp_color0) },
                Placement::default(),
            )
        } else {
            continue; // base/mix-only groups are not layers
        };
        let adjust = match role("adj") {
            Some(cc) => Adjust {
                saturation: crate::shader_nodes::Node::default_f32(cc, 2, 1.0),
                gamma: crate::shader_nodes::Node::default_f32(cc, 4, 1.0),
                contrast: crate::shader_nodes::Node::default_f32(cc, 6, 0.0),
                invert: role("inv").is_some(),
            },
            None => Adjust { invert: role("inv").is_some(), ..Adjust::default() },
        };
        // Blend mode rides MixRgb.mix_blend; opacity is its own ConstantValue (`op`).
        // Factor = mask × opacity is the over-alpha (removes rather than crushing to black).
        let blend = role("mix")
            .map(|n| BlendMode::from_mix_blend(n.mix_blend))
            .unwrap_or(BlendMode::Normal);
        // Opacity: prefer the dedicated `op` ConstantValue node. Fallbacks keep
        // older in-memory graphs (opacity on mfac.B or Mix Factor) parseable.
        let mask = parse_mask(&nodes);
        let opacity = role("op")
            .map(|n| n.const_float)
            .or_else(|| {
                if !mask.is_empty() {
                    role("mfac").map(|m| crate::shader_nodes::Node::default_f32(m, 1, 1.0))
                } else {
                    role("mix").map(|m| crate::shader_nodes::Node::default_f32(m, 0, 1.0))
                }
            })
            .unwrap_or(1.0);
        // `h` on the mix flag = the layer was authored hidden (G1 gap 1). No mix node (a
        // visible first layer without mask) can only mean visible.
        let visible = role("mix")
            .and_then(|m| parse_panel_tag(&m.label).and_then(|(.., f)| f))
            .map(|f| !f.contains('h'))
            .unwrap_or(true);
        // G4 projection fidelity: (a) the layer id IS the tag's lid — discarding it
        // (the old `id: 0`) made every projected doc renumber on the next regen;
        // (b) authored radius DRIVES are the connection INTO the generator's radius
        // socket — recover them or a projected doc silently unwires them.
        let drives: Vec<ParamDrive> = ["gen", "cav", "dir", "thk"]
            .iter()
            .filter_map(|r| role(r))
            .filter_map(|n| {
                let sock = generator_drive_socket(&n.node_type)?;
                g.connections
                    .iter()
                    .find(|c| c.to_node == n.id && c.to_input == sock)
                    .map(|c| ParamDrive {
                        param: "radius".into(),
                        node: c.from_node,
                        output: c.from_output as u32,
                    })
            })
            .collect();
        layers.push(Layer {
            id: li as u32,
            drives,
            content,
            mask,
            blend,
            opacity,
            visible,
            placement,
            adjust,
        });
    }
    if layers.is_empty() {
        None
    } else {
        Some(layers)
    }
}

/// Reconstruct a layer's MASK from its panel nodes (inverse of [`emit_mask_stack`]).
/// Nested stacks re-parse as a single primary content for now (full nested round-trip
/// is best-effort via live doc, not graph labels alone).
fn parse_mask(nodes: &[&crate::shader_nodes::Node]) -> MaskStack {
    let role = |r: &str| -> Option<&crate::shader_nodes::Node> {
        nodes
            .iter()
            .copied()
            .find(|n| parse_panel_tag(&n.label).map(|(_, ro, ..)| ro == r).unwrap_or(false))
    };
    let invert = role("mskinv").is_some();
    if let Some(img) = role("mskimg") {
        let strength = crate::shader_nodes::Node::mask_input_index(
            &crate::shader_nodes::NodeType::ImageTexture,
        )
        .map(|mi| crate::shader_nodes::Node::default_f32(img, mi, 1.0))
        .unwrap_or(1.0);
        return MaskStack::single(LayerContent::Image {
            tex_id: img.image_tex_id,
            strength,
        });
    }
    if let Some(k) = role("mskk") {
        return MaskStack::single(LayerContent::Constant {
            value: ChannelValue::Scalar(k.ramp_color0[0]),
        });
    }
    // Flag-packed param reader: tags carry comma-separated entries ("sg", "a3", "r0.03",
    // "s0.7", "v"). One reader for every mask arm — the hardcoded defaults these arms used to
    // return are exactly the G1 fidelity gaps.
    let flag_of = |n: &crate::shader_nodes::Node| -> Option<String> {
        parse_panel_tag(&n.label).and_then(|(.., f)| f.map(|s| s.to_string()))
    };
    let flag_num = |flags: &Option<String>, prefix: char| -> Option<f32> {
        flags.as_ref().and_then(|f| {
            f.split(',')
                .find_map(|p| p.strip_prefix(prefix).and_then(|v| v.parse::<f32>().ok()))
        })
    };
    let mask_remap = || -> [f32; 2] {
        role("mskrm")
            .map(|r| {
                [
                    crate::shader_nodes::Node::default_f32(r, 1, 0.0),
                    crate::shader_nodes::Node::default_f32(r, 2, 1.0),
                ]
            })
            .unwrap_or([0.0, 1.0])
    };
    if let Some(gen) = role("mskgen") {
        let radius = crate::shader_nodes::Node::default_f32(gen, 1, 0.02);
        // Soft/Hard round-trips from CONTRAST (socket 4 on SdfCurvature) — see the emitter.
        let sharpness = crate::shader_nodes::Node::default_f32(gen, 4, 1.5);
        // Output rides the tag flag exactly like the content path ("sg"/"cx"/"cc", ±"v") —
        // the old hardcoded ConcaveOnly overwrote the authored selector on every reload.
        let flags = flag_of(gen);
        let (output, rung) = match flags.as_deref().map(|f| f.split(',').next().unwrap_or("")) {
            Some("sg") => (GeneratorOutput::Signed, GeneratorRung::Auto),
            Some("cx") => (GeneratorOutput::ConvexOnly, GeneratorRung::Auto),
            Some("cc") => (GeneratorOutput::ConcaveOnly, GeneratorRung::Auto),
            Some("sgv") => (GeneratorOutput::Signed, GeneratorRung::VertexCache),
            Some("cxv") => (GeneratorOutput::ConvexOnly, GeneratorRung::VertexCache),
            Some("ccv") => (GeneratorOutput::ConcaveOnly, GeneratorRung::VertexCache),
            _ => (GeneratorOutput::default(), GeneratorRung::Auto),
        };
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius,
            invert,
            output,
            remap: mask_remap(),
            rung,
            sharpness,
            axis: DirectionAxis::default(),
        });
    }
    if let Some(dir) = role("mskdir") {
        let axis = parse_panel_tag(&dir.label)
            .and_then(|(.., f)| f)
            .and_then(|f| f.strip_prefix('a'))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(dir.sdf_curv_region);
        let radius = crate::shader_nodes::Node::default_f32(dir, 1, 0.02);
        // Soft/Hard round-trips from CONTRAST (socket 4); default is SurfaceDirection's own 1.0.
        let sharpness = crate::shader_nodes::Node::default_f32(dir, 4, 1.0);
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::Direction,
            radius,
            invert,
            output: GeneratorOutput::default(),
            remap: mask_remap(),
            rung: GeneratorRung::Auto,
            sharpness,
            axis: DirectionAxis::from_u32(axis),
        });
    }
    if let Some(cav) = role("mskcav") {
        let radius = crate::shader_nodes::Node::default_f32(cav, 1, 0.02);
        // Cavity's contrast is socket 2.
        let sharpness = crate::shader_nodes::Node::default_f32(cav, 2, 1.0);
        let rung = if flag_of(cav).as_deref() == Some("v") {
            GeneratorRung::VertexCache
        } else {
            GeneratorRung::Auto
        };
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::Cavity,
            radius,
            invert,
            output: GeneratorOutput::default(),
            remap: mask_remap(),
            rung,
            sharpness,
            axis: DirectionAxis::default(),
        });
    }
    if let Some(ao) = role("mskao") {
        let flags = flag_of(ao);
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::AmbientOcclusion,
            // Tag-carried (no sockets on the AO node) — see the emitter.
            radius: flag_num(&flags, 'r').unwrap_or(0.02),
            invert,
            output: GeneratorOutput::default(),
            remap: mask_remap(),
            rung: GeneratorRung::Auto,
            sharpness: flag_num(&flags, 's').unwrap_or(1.0),
            axis: DirectionAxis::default(),
        });
    }
    if let Some(thk) = role("mskthk") {
        let radius = crate::shader_nodes::Node::default_f32(thk, 2, 0.25);
        let sharpness = crate::shader_nodes::Node::default_f32(thk, 3, 1.0);
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::Thickness,
            radius,
            invert,
            output: GeneratorOutput::default(),
            remap: mask_remap(),
            rung: GeneratorRung::Auto,
            sharpness,
            axis: DirectionAxis::default(),
        });
    }
    if let Some(pos) = role("mskpos") {
        // Comma-packed flag ("a3,r0.03,s0.7") — split before stripping (G1 PosY->PosX gap).
        let flags = flag_of(pos);
        let axis = flags
            .as_ref()
            .and_then(|f| {
                f.split(',')
                    .find_map(|p| p.strip_prefix('a').and_then(|v| v.parse::<u32>().ok()))
            })
            .unwrap_or(0);
        return MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::Position,
            radius: flag_num(&flags, 'r').unwrap_or(0.02),
            invert,
            output: GeneratorOutput::default(),
            remap: mask_remap(),
            rung: GeneratorRung::Auto,
            sharpness: flag_num(&flags, 's').unwrap_or(1.0),
            axis: DirectionAxis::from_u32(axis),
        });
    }
    MaskStack::empty()
}

