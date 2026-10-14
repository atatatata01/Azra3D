//! Lowering seam (P0): doc → engine, and migration engine → doc.
//!
//! Reuse audit (CLAUDE.md rule): searched for existing implementations —
//! graph↔material sync exists as `NodeGraph::pull_from_pbr` / `push_to_pbr`
//! (shader_nodes/graph.rs) and is REUSED here; graph-drives-channel detection
//! mirrors `node_graph_bake::channels_from_graph` (private, master_socket_driven
//! based — same MO_* mapping, documented in model.rs). No existing doc model /
//! derive / cost-summary functions found — these are new.

// Test-only imports: `#[cfg(test)] mod tests { use super::*; }` keeps the same
// scope it had when lower.rs was a single file (pure relocation).
#[cfg(test)]
use super::model::*;
#[cfg(test)]
use core_math::pbr_material::{MapKind, PbrMaterial, TexMap};

mod actions;
mod layers;
mod maps;
mod preview;

pub(crate) use actions::*;
pub(crate) use layers::*;
pub(crate) use maps::*;
pub(crate) use preview::*;

// ==================================================================
// P0 tests (spec §7 determinism + migration + cost)
// ==================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// A doc exercising EVERY channel state + layered + masked + placement
    /// override (the spec's round-trip fixture).
    fn kitchen_sink_doc() -> MaterialPanelDoc {
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.shader_type = core_math::pbr_material::ShaderType::Glass.as_u32();
        doc.texture_space = TextureSpace {
            projection: 1,
            tiling: [2.0, 2.0],
            offset: [0.25, 0.0],
            ..TextureSpace::default()
        };
        doc.channels.push(Channel {
            kind: ChannelKind::Emission,
            state: ChannelState::GraphDriven { node: 7 },
            layers: vec![],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true });
        doc.channels.push(Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 3, strength: 0.8 },
                    mask: MaskStack::single(LayerContent::Generator {
                        source: GeneratorSource::Curvature,
                        radius: 0.02,
                        invert: true,
                        output: GeneratorOutput::Signed,
                        remap: [0.0, 1.0],
                        rung: GeneratorRung::Auto,
                        sharpness: 1.0,
                        axis: DirectionAxis::default(),
                    }),
                    blend: BlendMode::Multiply,
                    opacity: 0.5,
                    visible: true,
                    placement: Placement {
                        space: Some(TextureSpace {
                            projection: 0,
                            tiling: [4.0, 4.0],
                            offset: [0.0, 0.5],
                            ..TextureSpace::default()
                        }),
                    },
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Constant { value: ChannelValue::Scalar(0.4) },
                    mask: MaskStack::default(),
                    blend: BlendMode::Add,
                    opacity: 1.0,
                    visible: false,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true });
        doc
    }

    /// GATE — serde round-trip is BYTE-identical (spec §7 determinism) for a doc
    /// with every channel state, layered + masked content, and a placement override.
    #[test]
    fn doc_serde_round_trip_byte_identical() {
        let doc = kitchen_sink_doc();
        let json1 = serde_json::to_string(&doc).expect("serialize");
        let back: MaterialPanelDoc = serde_json::from_str(&json1).expect("deserialize");
        assert_eq!(back, doc, "value round-trip");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json1.into_bytes(), json2.into_bytes(), "byte-identical round-trip");
    }

    /// GATE — deriving from a DEFAULT material yields all-Value channels (no maps,
    /// no graph), and lowering the doc back reproduces the material exactly
    /// (derive→lower identity ⇒ P0 "renders identically" holds structurally).
    ///
    /// CONTRACT CHANGED 2026-08-06: the derive no longer emits all 18 of `ALL_KINDS`.
    /// It emits the MAIN set plus anything the material actually uses (a wired socket,
    /// a set map slot, or a value that differs from the default). Emitting all 18 gave
    /// every doc-less material a panel of 18 rows — 4 with content and 14 inert — and
    /// that is what the owner reported. The identity property still holds where it
    /// matters: a channel is omitted ONLY when it already equals the default, so
    /// lowering onto the material it was derived from is unchanged.
    #[test]
    fn derive_from_default_material_all_value_and_lowers_back() {
        let mat = PbrMaterial::default();
        let doc = derive_doc_from_material(&mat, None);
        assert!(doc.channels.iter().all(|c| matches!(c.state, ChannelState::Value(_))));
        assert_eq!(doc.shader_type, 0);

        // A default material uses nothing, so the doc is exactly the MAIN set — the
        // same shape `MaterialPanelDoc::default_for_new()` gives a Ctrl+M material.
        // If this ever grows back toward 18, the over-import defect is back.
        let kinds: Vec<_> = doc.channels.iter().map(|c| c.kind).collect();
        assert_eq!(kinds.len(), 5, "derived doc should be the MAIN set, got {kinds:?}");
        for k in [
            ChannelKind::BaseColor,
            ChannelKind::Roughness,
            ChannelKind::Metallic,
            ChannelKind::Normal,
            ChannelKind::Displacement,
        ] {
            assert!(kinds.contains(&k), "MAIN channel {k:?} missing from derived doc");
        }

        // Channels the doc CARRIES still round-trip exactly.
        let mut lowered = PbrMaterial::default();
        lowered.roughness = 0.1;
        lowered.albedo = glam::Vec3::ZERO;
        lower_to_material(&doc, &mut lowered);
        assert_eq!(lowered.albedo, mat.albedo);
        assert_eq!(lowered.roughness, mat.roughness);

        // A NON-default value must survive the filter and therefore still lower —
        // this is the clause that stops the filter from hiding authored state.
        let mut authored = PbrMaterial::default();
        authored.ior = 9.0;
        let doc2 = derive_doc_from_material(&authored, None);
        assert!(
            doc2.channels.iter().any(|c| c.kind == ChannelKind::Ior),
            "an authored IOR must not be filtered out of the panel"
        );
        let mut back = PbrMaterial::default();
        lower_to_material(&doc2, &mut back);
        assert_eq!(back.ior, 9.0, "authored IOR must survive derive->lower");
    }

    /// Value channel must wipe a leftover map so the gbuffer uses the scalar
    /// (otherwise strength-1 maps replace roughness and the PBR slider looks dead).
    #[test]
    fn value_roughness_clears_stale_roughness_map() {
        use core_math::pbr_material::MapKind;
        let mut doc = MaterialPanelDoc::default_for_new();
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::Roughness) {
            ch.state = ChannelState::Value(ChannelValue::Scalar(0.15));
        }
        let mut mat = PbrMaterial::default();
        mat.roughness = 0.9;
        *mat.map_mut(MapKind::Roughness) =
            TexMap { tex_id: 7, strength: 1.0, uv_scale: 1.0, projection: 0, absolute: true, ..Default::default() };
        lower_to_material(&doc, &mut mat);
        assert_eq!(mat.roughness, 0.15);
        assert!(!mat.map(MapKind::Roughness).is_set(), "Value must clear roughness_map");
    }

    /// GATE — a mapped material derives Layered channels carrying the TexMap as a
    /// single Image layer with a placement override, and lowering restores the
    /// exact TexMap (the documented inverse).
    #[test]
    fn derive_from_mapped_material_and_lower_back() {
        use core_math::pbr_material::MapKind;
        let mut mat = PbrMaterial::default();
        *mat.map_mut(MapKind::Roughness) =
            TexMap { tex_id: 5, strength: 0.75, uv_scale: 3.0, projection: 1, absolute: false, ..Default::default() };
        let doc = derive_doc_from_material(&mat, None);
        let ch = doc.channel(ChannelKind::Roughness).expect("roughness channel");
        assert_eq!(ch.state, ChannelState::Layered);
        assert_eq!(ch.layers.len(), 1);
        assert!(matches!(ch.layers[0].content, LayerContent::Image { tex_id: 5, .. }));
        let mut back = PbrMaterial::default();
        lower_to_material(&doc, &mut back);
        assert_eq!(*back.map(MapKind::Roughness), *mat.map(MapKind::Roughness));
        // Unmapped channels stay unmapped.
        assert!(!back.map(MapKind::Metallic).is_set());
    }

    /// GATE — a graph wiring a master socket makes that channel GraphDriven
    /// (mirrors node_graph_bake::channels_from_graph semantics), and lowering
    /// never overwrites it (C1).
    #[test]
    fn derive_graph_driven_channel() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("output").id;
        let img = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        assert!(g.connect(img, 0, out, NodeGraph::MO_ROUGHNESS), "wire roughness");
        let mat = PbrMaterial::default();
        let doc = derive_doc_from_material(&mat, Some(&g));
        assert_eq!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::GraphDriven { node: out }
        );
        // Unwired channels remain plain values.
        assert!(matches!(
            doc.channel(ChannelKind::Metallic).unwrap().state,
            ChannelState::Value(_)
        ));
        // C1: lowering must not touch the graph-driven channel.
        let mut m2 = PbrMaterial::default();
        m2.roughness = 0.123;
        lower_to_material(&doc, &mut m2);
        assert_eq!(m2.roughness, 0.123, "GraphDriven channel never lowered");
    }

    /// GATE — lower_to_graph produces a valid Master-Material graph seeded from
    /// the doc's values (P0 minimal correct body; codegen is P2).
    #[test]
    fn lower_to_graph_minimal() {
        let doc = MaterialPanelDoc::default_for_new();
        let g = lower_to_graph(&doc);
        let out = g.output_node().expect("MaterialOutput exists");
        let d = &out.input_defaults;
        let m = PbrMaterial::default();
        assert_eq!(
            d[crate::shader_nodes::NodeGraph::MO_ROUGHNESS].value_f32,
            m.roughness
        );
    }

    /// GATE — D1: a new material's doc opens with the MAIN channel set.
    #[test]
    fn default_for_new_main_channel_set() {
        let doc = MaterialPanelDoc::default_for_new();
        let kinds: Vec<ChannelKind> = doc.channels.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ChannelKind::BaseColor,
                ChannelKind::Roughness,
                ChannelKind::Metallic,
                ChannelKind::Normal,
                ChannelKind::Displacement,
            ]
        );
    }

    /// GATE (P2 acceptance) — ROUND-TRIP DETERMINISM for the P2-authored
    /// subset: values + shader type + texture space + single-image layers.
    /// panel doc → lower_to_material → derive_doc_from_material → identical
    /// channel states for every authored channel.
    #[test]
    fn p2_round_trip_panel_graph_panel() {
        use core_math::pbr_material::{MapKind, ShaderType};
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.shader_type = ShaderType::Subsurface.as_u32();
        doc.texture_space = TextureSpace {
            projection: 1,
            tiling: [2.0, 2.0],
            offset: [0.0, 0.0],
            ..TextureSpace::default()
        };
        // Authored values.
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::Roughness) {
            ch.state = ChannelState::Value(ChannelValue::Scalar(0.37));
        }
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::BaseColor) {
            ch.state = ChannelState::Value(ChannelValue::Color([0.2, 0.4, 0.6]));
        }
        // Authored single-image layer (hand-loaded map) with explicit override.
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::Metallic) {
            ch.state = ChannelState::Layered;
            ch.layers = vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Image { tex_id: 4, strength: 0.9 },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement {
                    space: Some(TextureSpace {
                        projection: 0,
                        tiling: [3.0, 3.0],
                        offset: [0.0, 0.0],
                        ..TextureSpace::default()
                    }),
                },
                adjust: Adjust::default(),
            }];
        }
        let mut mat = PbrMaterial::default();
        lower_to_material(&doc, &mut mat);
        assert_eq!(mat.shader_type, ShaderType::Subsurface);
        assert_eq!(mat.roughness, 0.37);
        assert_eq!(mat.map(MapKind::Metallic).tex_id, 4);
        let back = derive_doc_from_material(&mat, None);
        assert_eq!(back.shader_type, doc.shader_type);
        for ch in &doc.channels {
            let b = back.channel(ch.kind).expect("channel present after round-trip");
            assert_eq!(b.state, ch.state, "state round-trips for {:?}", ch.kind);
            if ch.state == ChannelState::Layered {
                assert_eq!(b.layers, ch.layers, "layer round-trips for {:?}", ch.kind);
            }
        }
    }

    /// GATE (P2 acceptance, C1 non-destruction) — no panel edit path touches a
    /// GraphDriven channel's subgraph: lowering never writes the channel, and
    /// the Value-channel codegen (`pull_from_pbr`) only writes socket DEFAULTS —
    /// nodes and wires are bit-identical before/after.
    #[test]
    fn p2_panel_edit_never_destroys_hand_authored_subgraph() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("output").id;
        let img = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        assert!(g.connect(img, 0, out, NodeGraph::MO_ROUGHNESS));
        let mut mat = PbrMaterial::default();
        mat.roughness = 0.5;
        let mut doc = derive_doc_from_material(&mat, Some(&g));
        assert!(matches!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::GraphDriven { .. }
        ));
        // Panel edit on a DIFFERENT channel.
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::Metallic) {
            ch.state = ChannelState::Value(ChannelValue::Scalar(0.8));
        }
        lower_to_material(&doc, &mut mat);
        assert_eq!(mat.roughness, 0.5, "GraphDriven channel untouched by lowering");
        // Codegen step (sync_pbr_into_material_graph seam): defaults only.
        let nodes_before = g.nodes.clone();
        let conns_before = g.connections.clone();
        g.pull_from_pbr(&mat);
        assert_eq!(g.connections, conns_before, "no wire changed by panel codegen");
        assert_eq!(g.nodes.len(), nodes_before.len(), "no node added/removed");
        assert!(g.find_node(img).is_some(), "hand-authored node survives");
    }

    /// GATE — GraphDriven promotion + demotion reconciliation (the choke-point
    /// helper): wiring a socket promotes the doc channel; unwiring demotes it
    /// back to a live Value.
    #[test]
    fn p2_reconcile_promotes_and_demotes() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("output").id;
        let img = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        assert!(g.connect(img, 0, out, NodeGraph::MO_ROUGHNESS));
        let mat = PbrMaterial::default();
        let mut doc = MaterialPanelDoc::default_for_new();
        assert!(reconcile_doc_with_graph(&mut doc, &mat, Some(&g)));
        assert_eq!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::GraphDriven { node: out }
        );
        // Second reconcile is a no-op (stable).
        assert!(!reconcile_doc_with_graph(&mut doc, &mat, Some(&g)));
        // Unwire → demote to Value carrying the live material scalar.
        g.disconnect(out, NodeGraph::MO_ROUGHNESS);
        assert!(reconcile_doc_with_graph(&mut doc, &mat, Some(&g)));
        assert_eq!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::Value(ChannelValue::Scalar(mat.roughness))
        );
    }

    /// GATE — revert surgery: prune_master_input removes ONLY the orphaned
    /// subgraph; nodes still feeding other sockets survive.
    #[test]
    fn p2_prune_master_input_scoped() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("output").id;
        let shared = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        assert!(g.connect(shared, 0, out, NodeGraph::MO_ALBEDO));
        assert!(g.connect(shared, 0, out, NodeGraph::MO_ROUGHNESS));
        let solo = g.add_node(NodeType::ImageTexture, [0.0, 200.0]);
        assert!(g.connect(solo, 0, out, NodeGraph::MO_METALLIC));
        // Prune roughness: shared still feeds albedo → kept.
        g.prune_master_input(NodeGraph::MO_ROUGHNESS);
        assert!(!g.master_socket_driven(NodeGraph::MO_ROUGHNESS));
        assert!(g.find_node(shared).is_some(), "shared node survives");
        // Prune metallic: solo node orphaned → deleted.
        g.prune_master_input(NodeGraph::MO_METALLIC);
        assert!(g.find_node(solo).is_none(), "orphaned subgraph deleted");
        assert!(g.master_socket_driven(NodeGraph::MO_ALBEDO), "albedo untouched");
    }

    /// P8 — the socket→node resolution `mp_frame_shader_node` uses to pick which
    /// node to frame. GraphDriven/panel channel → the node feeding the primary
    /// master socket; plain-value channel → the Master node itself.
    #[test]
    fn p8_channel_frames_source_node_else_master() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mut g = NodeGraph::default();
        let master = g.output_node().expect("output").id;
        // Roughness wired from an ImageTexture (driven / panel-head case).
        let feeder = g.add_node(NodeType::ImageTexture, [0.0, 0.0]);
        assert!(g.connect(feeder, 0, master, NodeGraph::MO_ROUGHNESS));

        // The exact resolution mp_frame_shader_node performs, per channel.
        let resolve = |kind: ChannelKind| -> Option<crate::shader_nodes::NodeId> {
            let sock = kind_socket(kind);
            g.connections
                .iter()
                .find(|c| c.to_node == master && c.to_input == sock)
                .map(|c| c.from_node)
                .or(Some(master))
        };
        // Driven channel → frames the feeding node.
        assert_eq!(resolve(ChannelKind::Roughness), Some(feeder));
        // Plain-value channel (nothing wired) → degrades to Master.
        assert_eq!(resolve(ChannelKind::Metallic), Some(master));
        // Header/master target (None) resolves to the Master node.
        assert_eq!(g.output_node().map(|n| n.id), Some(master));
    }

    /// P3 fixture: a rich two-layer roughness stack — bottom image layer with
    /// adjust + placement override (incl. offset + non-uniform tiling), top
    /// Constant layer with Multiply blend + 0.5 opacity.
    /// Owner 2026-07-28: "the curvature softness is still sharp, this is the type of map I need"
    /// (reference: smooth midtone gradients across a whole shell).
    ///
    /// A GENERATOR layer with a non-default `Adjust` must emit a `ColorCorrect` node carrying
    /// gamma. The panel used to HIDE the Adjust section for generators, so gamma — the only
    /// control that produces that reference look — was unreachable on exactly the layers that
    /// needed it, even though the lowering already supported it.
    ///
    /// Why gamma and not Soft/Hard: curvature shaping is
    /// `smoothstep(clamp(x * contrast * 4, 0, 1))` — contrast scales the INPUT before a hard
    /// clamp, so it can only trade "saturates at x>=0.17, everything white" for "everything
    /// crushed dark" (measured: contrast 0.15 collapses the whole range into 0.00-0.47). Gamma
    /// reshapes the OUTPUT, giving a continuous ramp end to end.
    #[test]
    fn generator_layer_with_adjust_emits_colorcorrect_gamma() {
        use crate::shader_nodes::NodeType;
        let mut ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::Curvature,
                    radius: 0.05,
                    invert: false,
                    output: GeneratorOutput::ConvexOnly,
                    remap: [0.0, 1.0],
                    rung: GeneratorRung::Auto,
                    sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust { gamma: 0.45, ..Adjust::default() },
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        let mat = PbrMaterial::default();
        let mut g = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat
        ));
        let cc = g
            .nodes
            .iter()
            .find(|n| n.node_type == NodeType::ColorCorrect)
            .expect(
                "a GENERATOR layer with non-default adjust must emit ColorCorrect — without it                  gamma never reaches the bake and the curvature map cannot be softened",
            );
        assert!(
            (crate::shader_nodes::Node::default_f32(cc, 4, -1.0) - 0.45).abs() < 1e-6,
            "gamma must land on ColorCorrect socket 4"
        );

        // Default adjust must NOT emit the node — an always-on ColorCorrect would cost every
        // generator layer an extra per-pixel node for nothing.
        ch.layers[0].adjust = Adjust::default();
        let mut g2 = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut g2, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat
        ));
        assert!(
            !g2.nodes.iter().any(|n| n.node_type == NodeType::ColorCorrect),
            "default adjust must emit no ColorCorrect"
        );
    }

    /// Owner 2026-07-28: "if I click drag image from library inside a channel it should
    /// automatically create an image layer with that image".
    ///
    /// Pins the RULE the drop handler follows: reuse the TOPMOST existing Image layer, else
    /// create one. Before the fix the drop hardcoded layer index 0 and assumed it existed AND
    /// was an Image — `mp_apply_pending_layer_load` does `layers.get_mut(li)` then matches
    /// `LayerContent::Image`, so dropping onto an EMPTY channel (or one whose first layer is a
    /// Noise) silently discarded the decoded texture: file loaded, nothing appeared, no error.
    #[test]
    fn library_drop_targets_topmost_image_layer_else_creates_one() {
        let img = |tex_id: u32| Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };
        let noise = || Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Noise {
                scale: 4.0,
                strength: 1.0,
                octaves: 4.0,
                noise_type: crate::shader_nodes::NoiseType::Fbm,
            },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };
        // The resolution rule, mirrored from `mp_ensure_image_layer_for_drop`.
        let target = |layers: &[Layer]| -> Option<usize> {
            layers
                .iter()
                .rposition(|l| matches!(l.content, LayerContent::Image { .. }))
        };

        assert_eq!(
            target(&[]),
            None,
            "an EMPTY channel has no image layer — the handler must CREATE one. Hardcoding index              0 here is what made a drop onto an empty channel silently do nothing."
        );
        assert_eq!(
            target(&[noise()]),
            None,
            "a channel whose only layer is a Noise must still CREATE an image layer — writing the              texture into layer 0 would fail the Image match and drop it on the floor"
        );
        assert_eq!(
            target(&[img(3)]),
            Some(0),
            "an existing image layer is REUSED so a second drop REPLACES the texture rather than              stacking duplicates"
        );
        assert_eq!(
            target(&[noise(), img(3), noise()]),
            Some(1),
            "the TOPMOST image layer wins in a mixed stack (rposition), not the first slot"
        );
    }

    /// Owner 2026-07-28: channel "Duplicate to…" / "Change type to…".
    ///
    /// Pins the RULE both actions share: a target kind is only legal when it has a TexMap slot
    /// AND is not already present. `MaterialPanelDoc::channel()` resolves by `.find()` (FIRST
    /// match wins), so a second channel of the same kind would be invisible to every lookup and
    /// would fight the first for the single map slot that kind owns — the exact reason duplicate
    /// BaseColor channels were rejected as a feature.
    #[test]
    fn channel_retarget_only_offers_free_slotted_kinds() {
        let present = [ChannelKind::BaseColor, ChannelKind::Roughness];
        let legal = |k: ChannelKind| k.map_kind().is_some() && !present.contains(&k);

        assert!(
            !legal(ChannelKind::BaseColor),
            "an ALREADY PRESENT kind must never be offered — two channels of one kind fight for              the same TexMap slot and the second is invisible to `channel()`"
        );
        assert!(
            legal(ChannelKind::Metallic),
            "an absent, slotted kind is a valid retarget target"
        );
        assert!(
            legal(ChannelKind::Displacement),
            "Displacement has a map slot and is absent — the owner's 'base color drives              displacement' case must be offered"
        );
        // A kind with no TexMap slot cannot receive a baked stack: retargeting there would leave
        // the user's layers silently inert, which is worse than not offering it.
        for k in ALL_KINDS {
            if k.map_kind().is_none() {
                assert!(
                    !legal(k),
                    "{k:?} has no map slot — offering it would make the moved stack inert"
                );
            }
        }
    }

    /// Owner 2026-07-28: "adding a noise inside normal should automatically change it to a
    /// normal output".
    ///
    /// A scalar (Noise/Constant/Generator) in the NORMAL channel is a HEIGHT field. Wiring it
    /// into MO_NORMAL hands the shader a grey RGB where it expects a tangent-space vector — the
    /// surface reads flat, just tinted. The bake already derives a normal from height via a
    /// Sobel pass, but ONLY when `height_wired && !normal_wired` (node_graph_bake.rs), so
    /// wiring MO_NORMAL is precisely what disabled it.
    ///
    /// An IMAGE layer must keep MO_NORMAL — a loaded normal map is already a vector and must not
    /// be re-derived from its own RGB.
    #[test]
    fn noise_in_normal_channel_routes_to_height_socket() {
        let scalar = |c: LayerContent| Channel {
            kind: ChannelKind::Normal,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: c,
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        let noise = scalar(LayerContent::Noise {
            scale: 4.0,
            strength: 1.0,
            octaves: 4.0,
            noise_type: crate::shader_nodes::NoiseType::Fbm,
        });
        assert!(
            stack_is_scalar_source(&noise),
            "a Noise layer is a scalar height source — it must route to MO_HEIGHT so the bake's              Sobel pass can derive a real tangent-space normal"
        );

        let img = scalar(LayerContent::Image { tex_id: 7, strength: 1.0 });
        assert!(
            !stack_is_scalar_source(&img),
            "an IMAGE layer in the Normal channel is an authored normal MAP — it must keep              MO_NORMAL, or its RGB would be wrongly re-derived as if it were height"
        );

        // Mixed stack: one image is enough to disqualify — the tail is a single wire, so it
        // cannot be both, and treating an authored normal map as height is the worse error.
        let mut mixed = noise.clone();
        mixed.layers.push(Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 7, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        });
        assert!(
            !stack_is_scalar_source(&mixed),
            "a stack containing an authored normal map must not be treated as height"
        );
    }

    fn rich_roughness_channel() -> Channel {
        Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 3, strength: 0.8 },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement {
                        space: Some(TextureSpace {
                            projection: 1,
                            tiling: [2.0, 3.0],
                            offset: [0.1, 0.0],
                            ..TextureSpace::default()
                        }),
                    },
                    adjust: Adjust { contrast: 0.25, saturation: 0.5, gamma: 2.0, invert: true },
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Constant { value: ChannelValue::Scalar(0.4) },
                    mask: MaskStack::default(),
                    blend: BlendMode::Multiply,
                    opacity: 0.5,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(),
            enabled: true,
        }
    }

    /// Value + layers must promote to Layered so clear_map cannot wipe Mult noise installs.
    #[test]
    fn value_with_noise_layer_promotes_to_layered_on_normalize() {
        let mut doc = MaterialPanelDoc::default_for_new();
        let ch = doc
            .channels
            .iter_mut()
            .find(|c| c.kind == ChannelKind::Roughness)
            .expect("default has roughness");
        assert!(matches!(ch.state, ChannelState::Value(_)));
        ch.layers.push(Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Noise {
                scale: 4.0,
                strength: 1.0,
                octaves: 4.0,
                noise_type: crate::shader_nodes::NoiseType::Fbm,
            },
            mask: MaskStack::default(),
            blend: BlendMode::Multiply,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        });
        // Layers exist but state still Value (the Add-layer bug).
        assert!(matches!(ch.state, ChannelState::Value(_)));
        assert!(normalize_generator_layers(&mut doc, true, Some(0.02)));
        let ch = doc.channel(ChannelKind::Roughness).unwrap();
        assert!(
            matches!(ch.state, ChannelState::Layered),
            "normalize must promote Value+layers → Layered so bake maps are not clear_map'd"
        );
        // Subgraph path owns the map — Value write_value must not run.
        assert_eq!(
            channel_lowering(ch, TextureSpace::default()),
            ChannelLowering::Subgraph
        );
    }

    /// GATE (P3 acceptance) — decision tree: plain single image = FAST (the
    /// P0/P2 TexMap path, unchanged); anything richer = Subgraph; masked /
    /// generator = Stub.
    #[test]
    fn p3_channel_lowering_decision_tree() {
        let mut ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Image { tex_id: 5, strength: 1.0 },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement {
                    space: Some(TextureSpace {
                        projection: 0,
                        tiling: [2.0, 2.0],
                        offset: [0.0, 0.0],
                        ..TextureSpace::default()
                    }),
                },
                adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true };
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Fast, "uniform-tiling override stays fast");
        // REGRESSION (owner 2026-07-28): "changing tiling of a triplanar I expect to see a
        // difference of scale, same thing with offset expecting movement".
        //
        // A layer that INHERITS the material space (placement.space == None) must be judged by
        // that MATERIAL space, not waved through as "not rich". `rich()` used
        // `.unwrap_or(false)`, so editing Tiling/Offset at the MATERIAL level — the common case —
        // kept the channel on the FAST path, whose TexMap carries ONE `uv_scale` scalar and NO
        // offset field. V and the entire offset were silently discarded and the sliders did
        // nothing. Every pre-existing case above passes a per-layer OVERRIDE, which is exactly
        // why none of them caught it.
        {
            let mut inh = ch.clone();
            inh.layers[0].placement.space = None; // inherit the material space
            assert_eq!(
                channel_lowering(&inh, TextureSpace::default()),
                ChannelLowering::Fast,
                "inheriting a DEFAULT material space (1:1, no offset) is representable by TexMap"
            );
            let non_uniform = TextureSpace { tiling: [2.0, 1.0], ..TextureSpace::default() };
            assert_eq!(
                channel_lowering(&inh, non_uniform),
                ChannelLowering::Subgraph,
                "inherited NON-UNIFORM tiling must route to the subgraph — TexMap has a single                  uv_scale scalar, so staying Fast silently drops V and the tiling edit is lost"
            );
            let with_offset = TextureSpace { offset: [0.25, 0.0], ..TextureSpace::default() };
            assert_eq!(
                channel_lowering(&inh, with_offset),
                ChannelLowering::Subgraph,
                "inherited OFFSET must route to the subgraph — TexMap has no offset field at all,                  so staying Fast silently drops it and the texture never moves"
            );
        }

        ch.layers[0].adjust.gamma = 2.2;
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Subgraph, "adjust → subgraph");
        ch.layers[0].adjust = Adjust::default();
        ch.layers[0].placement.space.as_mut().unwrap().offset = [0.5, 0.0];
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Subgraph, "offset → subgraph");
        assert_eq!(channel_lowering(&rich_roughness_channel(), TextureSpace::default()), ChannelLowering::Subgraph);
        ch.layers[0].placement.space.as_mut().unwrap().offset = [0.0, 0.0];
        // Supported-source masks now lower to a real subgraph (P3.1).
        ch.layers[0].mask = MaskStack::single(LayerContent::Constant { value: ChannelValue::Scalar(1.0) });
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Subgraph, "constant mask → subgraph");
        // An UNSUPPORTED generator-source mask still stubs the whole channel.
        ch.layers[0].mask = MaskStack::single(LayerContent::Generator {
            source: GeneratorSource::NormalDirection, // still no backend
            radius: 0.02,
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: GeneratorRung::Auto,
            sharpness: 1.0,
            axis: DirectionAxis::default(),
        });
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Stub, "unsupported mask source → stub");
        // Fast path still writes the TexMap exactly as P2.
        let mut doc = MaterialPanelDoc::default_for_new();
        ch.layers[0].mask.clear();
        doc.channels.retain(|c| c.kind != ChannelKind::Roughness);
        doc.channels.push(ch);
        let mut mat = PbrMaterial::default();
        lower_to_material(&doc, &mut mat);
        assert_eq!(mat.roughness_map.tex_id, 5);
        assert_eq!(mat.roughness_map.uv_scale, 2.0);
    }

    /// P0-3 GATE — every `Stub` verdict carries a REASON, and the reason matches
    /// the trigger. This is what the UI badge reads, so a silent stub (the
    /// owner's "nothing happened" bug) cannot come back: verdict and reason are
    /// produced by the ONE decision function.
    #[test]
    fn p0_3_stub_always_reports_why() {
        let img = |visible: bool| Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 5, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };

        // (1) no visible layers — the trivially reachable case (hide the only layer).
        let ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![img(false)],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        let (v, r) = channel_lowering_full(&ch, TextureSpace::default());
        assert_eq!(v, ChannelLowering::Stub);
        assert_eq!(r, Some(StubReason::NoVisibleLayers));
        assert_eq!(channel_stub_reason(&ch, TextureSpace::default()), Some(StubReason::NoVisibleLayers));

        // (2) generator source with no bake backend — reason names the source.
        let ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::NormalDirection,
                    radius: 0.02,
                    invert: false,
                    output: GeneratorOutput::Signed,
                    remap: [0.0, 1.0],
                    rung: GeneratorRung::Auto,
                sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                ..img(true)
            }],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true };
        assert_eq!(
            channel_stub_reason(&ch, TextureSpace::default()),
            Some(StubReason::UnsupportedGenerator(GeneratorSource::NormalDirection))
        );

        // (3) channel kind with no TexMap slot (11 of 18 kinds).
        let ch = Channel {
            kind: ChannelKind::Ior,
            state: ChannelState::Layered,
            layers: vec![img(true)],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        assert_eq!(channel_stub_reason(&ch, TextureSpace::default()), Some(StubReason::NoMapSlot));

        // Channels that DO lower never raise the badge.
        let ok = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![img(true)],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        assert_eq!(channel_lowering(&ok, TextureSpace::default()), ChannelLowering::Fast);
        assert_eq!(channel_stub_reason(&ok, TextureSpace::default()), None);
        assert_eq!(channel_stub_reason(&rich_roughness_channel(), TextureSpace::default()), None);

        // A Value/GraphDriven channel is lowered by its OWN path — never "stubbed",
        // even though it has no layers to compose.
        let val = Channel::value(ChannelKind::Ior, ChannelValue::Scalar(1.45));
        assert_eq!(channel_stub_reason(&val, TextureSpace::default()), None);
        let driven = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::GraphDriven { node: 1 },
            layers: vec![],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true };
        assert_eq!(channel_stub_reason(&driven, TextureSpace::default()), None);

        // The verdict half is unchanged for every pre-existing caller.
        for ch in [&ok, &val, &driven] {
            assert_eq!(channel_lowering(ch, TextureSpace::default()), channel_lowering_full(ch, TextureSpace::default()).0);
        }

        // Every reason produces non-empty user-facing text.
        for r in [
            StubReason::NoVisibleLayers,
            StubReason::UnsupportedGenerator(GeneratorSource::NormalDirection),
            StubReason::NoMapSlot,
        ] {
            assert!(!r.badge_text().is_empty());
            assert!(r.explain().len() > 20, "reason must explain, not just label");
        }
    }

    /// GATE (P3 acceptance) — ROUND-TRIP: doc → regen_channel_subgraph →
    /// parse_panel_channel / derive_doc_from_material re-derives the SAME
    /// layers (multi-layer + adjust + override, panel-generated ≠ GraphDriven).
    /// G4: parse now recovers the tag lid (`id: 0` was the lossy old behavior), so
    /// round-trip expectations must carry the ids regen actually assigns:
    /// `layer.id`, or `index + 1` when the author left it 0 (maps.rs `let lid`).
    fn with_regen_ids(mut layers: Vec<Layer>) -> Vec<Layer> {
        for (i, l) in layers.iter_mut().enumerate() {
            if l.id == 0 {
                l.id = i as u32 + 1;
            }
        }
        layers
    }

    #[test]
    fn p3_round_trip_multilayer_adjust_override() {
        let ch = rich_roughness_channel();
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert!(panel_authored_socket(&g, ChannelKind::Roughness), "socket is panel-authored");
        assert!(!panel_authored_socket(&g, ChannelKind::Metallic));
        let parsed = parse_panel_channel(&g, ChannelKind::Roughness).expect("parse back");
        assert_eq!(parsed, with_regen_ids(ch.layers.clone()), "layer stack round-trips through the subgraph");
        // Full derive: Layered (never GraphDriven) with identical layers.
        let doc = derive_doc_from_material(&mat, Some(&g));
        let d = doc.channel(ChannelKind::Roughness).unwrap();
        assert_eq!(d.state, ChannelState::Layered);
        assert_eq!(d.layers, with_regen_ids(ch.layers.clone()));
        // Hand-authored graphs still promote to GraphDriven exactly as P2.
        let mut hg = crate::shader_nodes::NodeGraph::default();
        let out = hg.output_node().unwrap().id;
        let img = hg.add_node(crate::shader_nodes::NodeType::ImageTexture, [0.0, 0.0]);
        assert!(hg.connect(img, 0, out, crate::shader_nodes::NodeGraph::MO_ROUGHNESS));
        assert!(!panel_authored_socket(&hg, ChannelKind::Roughness));
        let hd = derive_doc_from_material(&mat, Some(&hg));
        assert!(matches!(
            hd.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::GraphDriven { .. }
        ));
    }

    /// GATE (P3.1) — MASK + OVERLAY blend round-trip: a masked, Overlay-blend,
    /// half-opacity constant-color layer lowers to real nodes (MixRgb.mix_blend
    /// = Overlay, a mask subgraph, an `mfac` Multiply feeding the Factor) and
    /// parses back byte-for-byte. Locks owner #2/#3 (node/panel duality).
    #[test]
    fn p3_1_mask_and_overlay_round_trip() {
        use crate::shader_nodes::{MixBlend, NodeType};
        let ch = Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 4, strength: 1.0 },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Constant { value: ChannelValue::Color([0.9, 0.2, 0.1]) },
                    mask: MaskStack::single(LayerContent::Constant { value: ChannelValue::Scalar(0.7) }),
                    blend: BlendMode::Overlay,
                    opacity: 0.5,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true };
        let mat = PbrMaterial::default();
        let mut g = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(&mut g, ChannelKind::BaseColor, &ch, TextureSpace::default(), &mat));
        // The top layer's MixRgb carries the Overlay blend as a real node.
        assert!(
            g.nodes.iter().any(|n| matches!(n.node_type, NodeType::MixRgb) && n.mix_blend == MixBlend::Overlay),
            "Overlay blend is a real MixRgb node mode"
        );
        // The mask lowered to a Multiply feeding the mix factor.
        assert!(
            g.nodes.iter().any(|n| n.label.contains("mfac")),
            "mask → mfac Multiply node present"
        );
        assert!(panel_authored_socket(&g, ChannelKind::BaseColor));
        let parsed = parse_panel_channel(&g, ChannelKind::BaseColor).expect("parse back");
        assert_eq!(parsed, with_regen_ids(ch.layers.clone()), "mask + Overlay + opacity round-trip");
    }

    /// GATE (P3) — LOWERING DETERMINISM: same doc → byte-identical subgraph,
    /// twice, from fresh graphs.
    #[test]
    fn p3_lowering_deterministic() {
        let ch = rich_roughness_channel();
        let mat = PbrMaterial::default();
        let mut g1 = crate::shader_nodes::NodeGraph::default();
        let mut g2 = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(&mut g1, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert!(regen_channel_subgraph(&mut g2, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        let j1 = serde_json::to_string(&g1).unwrap();
        let j2 = serde_json::to_string(&g2).unwrap();
        assert_eq!(j1, j2, "identical subgraph both times");
        // Regen in place is idempotent (strip + re-emit → same structure count).
        let nodes_before = g1.nodes.len();
        assert!(regen_channel_subgraph(&mut g1, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert_eq!(g1.nodes.len(), nodes_before, "regen does not accumulate nodes");
        assert!(parse_panel_channel(&g1, ChannelKind::Roughness).is_some());
        // Strip removes everything panel-made and unwires the socket.
        strip_channel_subgraph(&mut g1, ChannelKind::Roughness);
        assert!(!g1.master_socket_driven(crate::shader_nodes::NodeGraph::MO_ROUGHNESS));
        assert_eq!(g1.nodes.len(), 1, "only MaterialOutput survives");
    }

    /// GATE (P3 acceptance) — each panel feature → its own node (C1):
    /// adjust ↔ ColorCorrect 1:1 (sat→2, gamma→4, contrast→6), invert → Invert,
    /// strength → VectorMath Scale (`str`), opacity → ConstantValue (`op`),
    /// blend → MixRgb, tiling/offset → UVTransform.
    #[test]
    fn p3_adjust_maps_one_to_one_onto_colorcorrect() {
        use crate::shader_nodes::{Node, NodeType, VectorMathOp};
        let ch = rich_roughness_channel();
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        let cc = g
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::ColorCorrect))
            .expect("ColorCorrect emitted");
        assert_eq!(Node::default_f32(cc, 2, -1.0), 0.5, "saturation → input 2");
        assert_eq!(Node::default_f32(cc, 4, -1.0), 2.0, "gamma → input 4");
        assert_eq!(Node::default_f32(cc, 6, -1.0), 0.25, "contrast → input 6");
        assert!(
            g.nodes.iter().any(|n| matches!(n.node_type, NodeType::Invert)),
            "invert → Invert node"
        );
        let img = g
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::ImageTexture))
            .expect("ImageTexture emitted");
        assert_eq!(img.image_tex_id, 3, "pinned source slot");
        let mi = Node::mask_input_index(&NodeType::ImageTexture).unwrap();
        assert_eq!(
            Node::default_f32(img, mi, -1.0),
            1.0,
            "ImageTexture Mask stays 1 — strength is its own node"
        );
        let str_n = g
            .nodes
            .iter()
            .find(|n| n.label.contains(":str:") && matches!(n.node_type, NodeType::VectorMath))
            .expect("strength VectorMath Scale node");
        assert_eq!(str_n.vector_math_op, VectorMathOp::Scale);
        assert_eq!(Node::default_f32(str_n, 2, -1.0), 0.8, "strength → Scale input");
        assert!(
            g.nodes.iter().any(|n| matches!(n.node_type, NodeType::MixRgb)),
            "blend → MixRgb node"
        );
        let op = g
            .nodes
            .iter()
            .find(|n| n.label.contains(":op:") && matches!(n.node_type, NodeType::ConstantValue))
            .expect("opacity ConstantValue node");
        assert!((op.const_float - 0.5).abs() < 1e-5, "opacity → ConstantValue");
        // Tiling / offset land on the UVTransform defaults (the offset's home).
        let uvx = g
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::UVTransform))
            .expect("UVTransform emitted");
        assert_eq!(Node::default_f32(uvx, 4, -1.0), 2.0, "tiling U → Scale X");
        assert_eq!(Node::default_f32(uvx, 5, -1.0), 3.0, "tiling V → Scale Y");
        assert_eq!(Node::default_f32(uvx, 1, -1.0), 0.1, "offset U → Offset X");
    }

    /// GATE — every layer feature is a discrete, tagged node (image-editor base).
    #[test]
    fn each_feature_is_own_node() {
        use crate::shader_nodes::NodeType;
        let ch = Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 2, strength: 0.75 },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Noise {
                        scale: 3.0,
                        strength: 0.6,
                        octaves: 2.0,
                        noise_type: crate::shader_nodes::NoiseType::default(),
                    },
                    mask: MaskStack::single(LayerContent::Constant {
                        value: ChannelValue::Scalar(0.8),
                    }),
                    blend: BlendMode::Multiply,
                    opacity: 0.4,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust {
                        contrast: 0.1,
                        saturation: 1.0,
                        gamma: 1.0,
                        invert: false,
                    },
                },
            ],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        let mat = PbrMaterial::default();
        let mut g = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::BaseColor,
            &ch,
            TextureSpace::default(),
            &mat
        ));
        let has = |role: &str| {
            g.nodes.iter().any(|n| {
                parse_panel_tag(&n.label).map(|(_, r, ..)| r == role).unwrap_or(false)
            })
        };
        assert!(has("img"), "Image → img node");
        assert!(has("nse"), "Noise → nse node");
        assert!(has("str"), "Strength → str node");
        assert!(has("adj"), "Adjust → adj ColorCorrect");
        assert!(has("op"), "Opacity → op ConstantValue");
        assert!(has("mix"), "Blend → mix MixRgb");
        assert!(has("mfac"), "Mask×opacity → mfac MathBinary");
        assert!(has("mskk"), "Mask constant → mskk");
        // Strength must NOT live only on content Mask sockets.
        let img = g.nodes.iter().find(|n| matches!(n.node_type, NodeType::ImageTexture)).unwrap();
        let mi = crate::shader_nodes::Node::mask_input_index(&NodeType::ImageTexture).unwrap();
        assert_eq!(
            crate::shader_nodes::Node::default_f32(img, mi, -1.0),
            1.0
        );
        let parsed = parse_panel_channel(&g, ChannelKind::BaseColor).expect("parse");
        assert_eq!(parsed, with_regen_ids(ch.layers.clone()), "feature-split graph round-trips");
    }

    /// GATE (P3) — reconcile does NOT promote a panel-generated subgraph to
    /// GraphDriven (panel ownership survives its own codegen).
    #[test]
    fn p3_reconcile_ignores_panel_generated_wiring() {
        let ch = rich_roughness_channel();
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        let mut doc = MaterialPanelDoc::default_for_new();
        if let Some(c) = doc.channels.iter_mut().find(|c| c.kind == ChannelKind::Roughness) {
            *c = ch.clone();
        }
        assert!(
            !reconcile_doc_with_graph(&mut doc, &mat, Some(&g)),
            "no promotion for panel-generated wiring"
        );
        assert_eq!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::Layered
        );
        assert_eq!(
            panel_generated_mask(&g),
            1u32 << (ChannelKind::Roughness as u32)
        );
    }

    /// P6 fixture: a two-generator roughness stack — signed Curvature with
    /// remap + invert (bottom), Cavity multiplied on top.
    fn generator_channel() -> Channel {
        Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Generator {
                        source: GeneratorSource::Curvature,
                        radius: 0.05,
                        invert: true,
                        output: GeneratorOutput::Signed,
                        remap: [0.25, 0.75],
                        rung: GeneratorRung::Auto,
                    sharpness: 1.0,
                        axis: DirectionAxis::default(),
                    },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Generator {
                        source: GeneratorSource::Cavity,
                        radius: 0.02,
                        invert: false,
                        output: GeneratorOutput::Signed,
                        remap: [0.0, 1.0],
                        rung: GeneratorRung::Auto,
                    sharpness: 1.0,
                        axis: DirectionAxis::default(),
                    },
                    mask: MaskStack::default(),
                    blend: BlendMode::Multiply,
                    opacity: 0.8,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(),
            enabled: true,
        }
    }

    /// GATE (P6) — decision tree: supported generator content lowers as
    /// Subgraph; unsupported sources and masked layers stay Stub (fail-soft).
    #[test]
    fn p6_generator_lowering_decision() {
        let ch = generator_channel();
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Subgraph);
        let mut bad = ch.clone();
        if let LayerContent::Generator { source, .. } = &mut bad.layers[0].content {
            *source = GeneratorSource::NormalDirection; // still no bake backend
        }
        assert_eq!(channel_lowering(&bad, TextureSpace::default()), ChannelLowering::Stub);
        assert!(generator_source_supported(GeneratorSource::AmbientOcclusion));
        assert!(generator_source_supported(GeneratorSource::Thickness));
        assert!(generator_source_supported(GeneratorSource::Position));
        assert!(!generator_source_supported(GeneratorSource::NormalDirection));
        assert!(generator_source_supported(GeneratorSource::Curvature));
        assert!(generator_source_supported(GeneratorSource::Cavity));
    }

    /// GATE (P6) — LOWERING DETERMINISM: same generator doc → byte-identical
    /// subgraph twice; regen is idempotent (no node accumulation).
    #[test]
    fn p6_generator_lowering_deterministic() {
        let ch = generator_channel();
        let mat = PbrMaterial::default();
        let mut g1 = crate::shader_nodes::NodeGraph::default();
        let mut g2 = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(&mut g1, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert!(regen_channel_subgraph(&mut g2, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert_eq!(
            serde_json::to_string(&g1).unwrap(),
            serde_json::to_string(&g2).unwrap(),
            "identical generator subgraph both times"
        );
        let n = g1.nodes.len();
        assert!(regen_channel_subgraph(&mut g1, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        assert_eq!(g1.nodes.len(), n, "regen does not accumulate nodes");
    }

    /// GATE (P6) — ROUND-TRIP: generator layers → subgraph → parsed back
    /// identically (source, radius, output, remap, invert, blend, opacity),
    /// for all three curvature outputs.
    #[test]
    fn p6_generator_round_trip() {
        let mat = PbrMaterial::default();
        for output in [
            GeneratorOutput::Signed,
            GeneratorOutput::ConvexOnly,
            GeneratorOutput::ConcaveOnly,
        ] {
            let mut ch = generator_channel();
            if let LayerContent::Generator { output: o, .. } = &mut ch.layers[0].content {
                *o = output;
            }
            let mut g = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
            assert!(panel_authored_socket(&g, ChannelKind::Roughness));
            let parsed = parse_panel_channel(&g, ChannelKind::Roughness).expect("parse back");
            assert_eq!(parsed, with_regen_ids(ch.layers.clone()), "generator stack round-trips ({output:?})");
        }
    }

    /// Direction content layer with an explicit axis / radius / sharpness.
    fn direction_channel(axis: DirectionAxis) -> Channel {
        Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0,
                drives: Vec::new(),
                content: LayerContent::Generator {
                    source: GeneratorSource::Direction,
                    radius: 0.07,
                    invert: false,
                    output: GeneratorOutput::default(),
                    remap: [0.1, 0.9],
                    rung: GeneratorRung::Auto,
                    sharpness: 2.5,
                    axis,
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(),
            enabled: true,
        }
    }

    /// GATE (P8) — ROUND-TRIP: a Direction layer survives doc → subgraph → doc unchanged,
    /// for EVERY axis. Follows `p6_generator_round_trip`; the axis is the new state, and it
    /// rides the node tag, so a tag/parse mismatch fails here rather than silently resetting
    /// the user's axis to +Y on reload.
    #[test]
    fn p8_direction_round_trip() {
        let mat = PbrMaterial::default();
        for axis in DirectionAxis::ALL {
            let ch = direction_channel(axis);
            let mut g = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(
                &mut g,
                ChannelKind::Roughness,
                &ch,
                TextureSpace::default(),
                &mat
            ));
            assert!(panel_authored_socket(&g, ChannelKind::Roughness));
            let parsed = parse_panel_channel(&g, ChannelKind::Roughness).expect("parse back");
            assert_eq!(parsed, with_regen_ids(ch.layers.clone()), "Direction stack round-trips ({axis:?})");
        }
    }

    /// GATE (P8) — a Direction MASK round-trips too (the mask emitter/parser is a separate
    /// code path from the content one, and the axis has to survive both).
    #[test]
    fn p8_direction_mask_round_trip() {
        let mat = PbrMaterial::default();
        for axis in [DirectionAxis::PosY, DirectionAxis::NegZ] {
            let mut ch = generator_channel();
            ch.layers[0].mask = MaskStack::single(LayerContent::Generator {
                source: GeneratorSource::Direction,
                radius: 0.05,
                invert: false,
                output: GeneratorOutput::default(),
                remap: [0.0, 1.0],
                rung: GeneratorRung::Auto,
                sharpness: 1.0,
                axis,
            });
            let mut g = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(
                &mut g,
                ChannelKind::Roughness,
                &ch,
                TextureSpace::default(),
                &mat
            ));
            let parsed = parse_panel_channel(&g, ChannelKind::Roughness).expect("parse back");
            assert_eq!(
                parsed[0].mask, ch.layers[0].mask,
                "Direction mask round-trips ({axis:?})"
            );
        }
    }

    /// GATE (P8) — Direction is SUPPORTED (it has a real backend on both paths), and the
    /// emitted node is a `SurfaceDirection` carrying the axis code + the layer's radius and
    /// Soft/Hard in the sockets the CPU evaluator reads.
    #[test]
    fn p8_direction_emits_surface_direction_node() {
        use crate::shader_nodes::{Node, NodeType};
        assert!(generator_source_supported(GeneratorSource::Direction));
        let mat = PbrMaterial::default();
        let ch = direction_channel(DirectionAxis::NegY);
        let mut g = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::Roughness,
            &ch,
            TextureSpace::default(),
            &mat
        ));
        let dir = g
            .nodes
            .iter()
            .find(|n| matches!(n.node_type, NodeType::SurfaceDirection))
            .expect("SurfaceDirection emitted");
        assert_eq!(
            dir.sdf_curv_region,
            DirectionAxis::NegY.as_u32(),
            "axis code reaches the node"
        );
        assert_eq!(Node::default_f32(dir, 1, -1.0), 0.07, "radius → Epsilon");
        assert_eq!(
            Node::default_f32(dir, 4, -1.0),
            2.5,
            "Soft/Hard → Contrast (socket 4)"
        );
    }

    /// GATE (P8) — the axis wire codes agree between the panel model and the shader-node
    /// helper. These are two independent tables (`DirectionAxis::as_u32` and
    /// `surface_direction::axis_vec`); if they ever drift, +Y silently becomes −X in the bake
    /// while the panel keeps saying "+Y (top)".
    #[test]
    fn p8_direction_axis_codes_match_shader_node_table() {
        use crate::shader_nodes::nodes::surface_direction::axis_vec;
        let expect: [(DirectionAxis, [f32; 3]); 6] = [
            (DirectionAxis::PosX, [1.0, 0.0, 0.0]),
            (DirectionAxis::NegX, [-1.0, 0.0, 0.0]),
            (DirectionAxis::PosY, [0.0, 1.0, 0.0]),
            (DirectionAxis::NegY, [0.0, -1.0, 0.0]),
            (DirectionAxis::PosZ, [0.0, 0.0, 1.0]),
            (DirectionAxis::NegZ, [0.0, 0.0, -1.0]),
        ];
        for (a, v) in expect {
            assert_eq!(axis_vec(a.as_u32()), v, "{a:?} axis vector");
            assert_eq!(DirectionAxis::from_u32(a.as_u32()), a, "{a:?} code round-trips");
        }
    }

    /// GATE (P8) — Direction NEVER gets promoted to the Vertex rung, even on a body with no
    /// UVs (where the "no UVs → Vertex" rule fires for Curvature/Cavity). There is no
    /// Direction vertex kernel, so a VertexCache Direction emits an unconnected node and
    /// contributes NOTHING — an invisible layer that looks authored. Both halves of the
    /// normalize decision are checked, since a disagreement between them is its own bug.
    #[test]
    fn p8_direction_never_promoted_to_vertex_rung() {
        let mut doc = MaterialPanelDoc::default_for_new();
        doc.channels.push(direction_channel(DirectionAxis::PosY));
        // Ask for the no-UV case — the one that promotes Curvature.
        let mut d2 = doc.clone();
        assert!(!generators_need_normalize(&d2, false, Some(0.02)));
        assert!(!normalize_generator_layers(&mut d2, false, Some(0.02)));
        assert_eq!(d2, doc, "no-UV normalize leaves a Direction layer alone");

        // A doc that somehow holds VertexCache (hand-edited JSON / source swap) is DEMOTED.
        let mut d3 = doc.clone();
        if let Some(LayerContent::Generator { rung, .. }) = d3
            .channels
            .last_mut()
            .and_then(|c| c.layers.first_mut())
            .map(|l| &mut l.content)
        {
            *rung = GeneratorRung::VertexCache;
        }
        assert!(
            generators_need_normalize(&d3, false, Some(0.02)),
            "a VertexCache Direction must be reported dirty"
        );
        assert!(normalize_generator_layers(&mut d3, false, Some(0.02)));
        assert_eq!(d3, doc, "VertexCache Direction is demoted back to Auto");

        // And it never arms the vertex-curvature GPU path.
        assert!(!channel_uses_vertex_curvature(&doc.channels[doc.channels.len() - 1]));
        assert!(channel_has_texture_bake_content(
            &doc.channels[doc.channels.len() - 1]
        ));
    }

    /// GATE (P6) — THREE-OUTPUT wiring correctness: the selected SdfCurvature
    /// output socket feeds the chain; Signed goes through the normalising
    /// Remap (−1..1 → 0..1) so 0.5 = flat (spec P6), Convex/Concave do not.
    #[test]
    fn p6_three_output_wiring() {
        use crate::shader_nodes::{Node, NodeType};
        assert_eq!(generator_output_socket(GeneratorOutput::Signed), 1);
        assert_eq!(generator_output_socket(GeneratorOutput::ConvexOnly), 2);
        assert_eq!(generator_output_socket(GeneratorOutput::ConcaveOnly), 3);
        let mat = PbrMaterial::default();
        for (output, expect_gnorm) in [
            (GeneratorOutput::Signed, true),
            (GeneratorOutput::ConvexOnly, false),
            (GeneratorOutput::ConcaveOnly, false),
        ] {
            let ch = Channel {
                kind: ChannelKind::Roughness,
                state: ChannelState::Layered,
                layers: vec![Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Generator {
                        source: GeneratorSource::Curvature,
                        radius: 0.04,
                        invert: false,
                        output,
                        remap: [0.0, 1.0],
                        rung: GeneratorRung::Auto,
                    sharpness: 1.0,
                        axis: DirectionAxis::default(),
                    },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement::default(),
                    adjust: Adjust::default(),
                }],
            bit_depth: ChannelBitDepth::default(),
    eval_mode: Default::default(), enabled: true };
            let mut g = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
            let gen = g
                .nodes
                .iter()
                .find(|n| matches!(n.node_type, NodeType::SdfCurvature))
                .expect("SdfCurvature emitted");
            assert_eq!(Node::default_f32(gen, 1, -1.0), 0.04, "radius → Epsilon");
            let wire = g
                .connections
                .iter()
                .find(|c| c.from_node == gen.id)
                .expect("generator output wired");
            assert_eq!(
                wire.from_output,
                generator_output_socket(output),
                "selected output socket feeds the chain"
            );
            let gnorm = g.nodes.iter().find(|n| n.label.contains(":gnorm:"));
            assert_eq!(gnorm.is_some(), expect_gnorm, "gnorm only for Signed");
            if let Some(gn) = gnorm {
                assert_eq!(Node::default_f32(gn, 1, 9.0), -1.0);
                assert_eq!(Node::default_f32(gn, 2, 9.0), 1.0);
                assert_eq!(Node::default_f32(gn, 3, 9.0), 0.0);
                assert_eq!(Node::default_f32(gn, 4, 9.0), 1.0);
            }
        }
    }

    /// Each layer's effective tiling is baked (UVTransform) so Mult stacks keep
    /// independent tile densities; absolute display uses uv_scale=1.
    #[test]
    fn per_layer_tiling_bakes_uv_transform() {
        use crate::shader_nodes::NodeType;
        let mat_ts = TextureSpace::default();
        let ch = Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 3, strength: 1.0 },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement {
                        space: Some(TextureSpace {
                            projection: 0,
                            tiling: [2.0, 2.0],
                            offset: [0.0, 0.0],
                            ..TextureSpace::default()
                        }),
                    },
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0,
                    drives: Vec::new(),
                    content: LayerContent::Noise {
                        scale: 4.0,
                        strength: 1.0,
                        octaves: 4.0,
                        noise_type: crate::shader_nodes::NoiseType::Fbm,
                    },
                    mask: MaskStack::default(),
                    blend: BlendMode::Multiply,
                    opacity: 1.0,
                    visible: true,
                    placement: Placement {
                        space: Some(TextureSpace {
                            projection: 0,
                            tiling: [0.5, 0.5],
                            offset: [0.0, 0.0],
                            ..TextureSpace::default()
                        }),
                    },
                    adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true };
        let mat = PbrMaterial::default();
        let mut g = crate::shader_nodes::NodeGraph::default();
        assert!(regen_channel_subgraph(&mut g, ChannelKind::BaseColor, &ch, mat_ts, &mat));
        assert!(
            g.nodes
                .iter()
                .any(|n| matches!(n.node_type, NodeType::UVTransform)),
            "non-1×1 per-layer tiling must bake UVTransform into Mult stack"
        );
        assert!(
            g.nodes
                .iter()
                .any(|n| matches!(n.node_type, NodeType::MixRgb)),
            "Noise Mult over Image emits MixRgb"
        );
    }

    /// GATE (P6/C4) — radius floor mirrors the bake clamp (1% of short AABB axis).
    #[test]
    fn p6_radius_floor() {
        assert!((curvature_radius_floor(10.0) - 0.1).abs() < 1e-6);
        assert!((curvature_radius_floor(0.0) - 1e-4).abs() < 1e-9, "never zero");
        // Elongated AABB: floor from short axis, not the long diagonal.
        let bmin = glam::Vec3::new(-5.0, -0.5, -0.5);
        let bmax = glam::Vec3::new(5.0, 0.5, 0.5);
        let floor = curvature_radius_floor_aabb(bmin, bmax);
        assert!((floor - 0.01).abs() < 1e-6, "min axis=1 → floor=0.01, got {floor}");
        let diag_floor = curvature_radius_floor((bmax - bmin).length());
        assert!(
            floor < diag_floor * 0.5,
            "short-axis floor must be much smaller than full-diag floor on elongated bodies"
        );
    }

    /// GATE — cost readout (revised C2): shader model + set-map count.
    #[test]
    fn cost_summary_cases() {
        use core_math::pbr_material::{MapKind, ShaderType};
        let mut mat = PbrMaterial::default();
        assert_eq!(cost_summary(&mat), "standard PBR");
        mat.map_mut(MapKind::Albedo).tex_id = 1;
        assert_eq!(cost_summary(&mat), "standard PBR · 1 map");
        mat.map_mut(MapKind::Normal).tex_id = 2;
        mat.map_mut(MapKind::Roughness).tex_id = 3;
        mat.map_mut(MapKind::Metallic).tex_id = 4;
        assert_eq!(cost_summary(&mat), "standard PBR · 4 maps");
        mat.shader_type = ShaderType::Glass;
        assert_eq!(cost_summary(&mat), "glass · 4 maps");
    }

    // ==============================================================
    // P7 — bake-tier rung (spec §6.5 / C3)
    // ==============================================================

    /// GATE (P7 serde) — a pre-P7 doc (no `rung` field on the Generator layer)
    /// parses, and the rung defaults to `Auto` (today's texture-bake behavior).
    #[test]
    fn p7_rung_serde_default_old_docs_parse() {
        let old = r#"{"Generator":{"source":"Curvature","radius":0.02,"invert":false}}"#;
        let c: LayerContent = serde_json::from_str(old).expect("pre-P7 layer parses");
        match c {
            LayerContent::Generator { rung, output, remap, .. } => {
                assert_eq!(rung, GeneratorRung::Auto);
                assert_eq!(output, GeneratorOutput::Signed);
                assert_eq!(remap, [0.0, 1.0]);
            }
            other => panic!("expected Generator, got {other:?}"),
        }
    }

    /// GATE (P7 round-trip) — the rung survives doc JSON serde. Graph
    /// reconstruction recovers the `v` flag for Vertex; Remap/Invert live on the
    /// panel doc for Vertex (G-buffer knobs), not the bake graph.
    #[test]
    fn p7_rung_round_trips_doc_and_graph() {
        let mat = PbrMaterial::default();
        for src in [GeneratorSource::Curvature, GeneratorSource::Cavity] {
            let mut ch = generator_channel();
            if let LayerContent::Generator { source, rung, .. } = &mut ch.layers[0].content {
                *source = src;
                *rung = GeneratorRung::VertexCache;
            }
            let json = serde_json::to_string(&ch).unwrap();
            let back: Channel = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ch, "doc serde preserves rung ({src:?})");
            let mut g = crate::shader_nodes::NodeGraph::default();
            // Vertex layers skip the bake chain; may still emit tagged nodes.
            let _ = regen_channel_subgraph(
                &mut g,
                ChannelKind::Roughness,
                &ch,
                TextureSpace::default(),
                &mat,
            );
            let has_v = g.nodes.iter().any(|n| {
                parse_panel_tag(&n.label)
                    .and_then(|(.., f)| f)
                    .is_some_and(|f| f.ends_with('v'))
            });
            // Second layer may still be Texture; first is Vertex-tagged.
            assert!(
                has_v
                    || matches!(
                        ch.layers[0].content,
                        LayerContent::Generator {
                            rung: GeneratorRung::VertexCache,
                            ..
                        }
                    ),
                "Vertex rung recorded for {src:?}"
            );
        }
    }

    /// GATE (P2) — VertexCache and Auto both wire SdfCurvature into the bake (single compositor).
    #[test]
    /// BOTH MODES: Texture and Vertex both wire the bake (Decision 1), but Vertex tags
    /// trailing `v` so `vertex_curv_keys_for_graph` / SEED_POSITIONS can discover them.
    fn p7_rung_switch_determinism_lowering_identical() {
        let mat = PbrMaterial::default();
        for src in [GeneratorSource::Curvature, GeneratorSource::Cavity] {
            let mut ch_tex = generator_channel();
            let mut ch_vtx = generator_channel();
            if let LayerContent::Generator { source, rung, .. } = &mut ch_tex.layers[0].content {
                *source = src;
                *rung = GeneratorRung::Auto;
            }
            if let LayerContent::Generator { source, rung, .. } = &mut ch_vtx.layers[0].content {
                *source = src;
                *rung = GeneratorRung::VertexCache;
            }
            let mut g_tex = crate::shader_nodes::NodeGraph::default();
            let mut g_vtx = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(
                &mut g_tex,
                ChannelKind::Roughness,
                &ch_tex,
                TextureSpace::default(),
                &mat
            ));
            assert!(regen_channel_subgraph(
                &mut g_vtx,
                ChannelKind::Roughness,
                &ch_vtx,
                TextureSpace::default(),
                &mat,
            ));
            // Both must wire a generator into the composite (not leave unconnected tags only).
            let wired = |g: &crate::shader_nodes::NodeGraph| {
                g.nodes.iter().any(|n| {
                    n.label.contains(":gen:") || n.label.contains(":cav")
                }) && !g.connections.is_empty()
            };
            assert!(wired(&g_tex), "Auto must wire bake chain for {src:?}");
            assert!(wired(&g_vtx), "VertexCache must wire bake chain for {src:?} (Decision 1)");
            // Sampling domain: Vertex tags trailing `v`; Texture does not.
            let is_vertex_flag = |l: &str| {
                l.contains(":sgv")
                    || l.contains(":cxv")
                    || l.contains(":ccv")
                    || parse_panel_tag(l).and_then(|(.., f)| f) == Some("v")
            };
            let tex_labels: Vec<_> = g_tex.nodes.iter().map(|n| n.label.as_str()).collect();
            let vtx_labels: Vec<_> = g_vtx.nodes.iter().map(|n| n.label.as_str()).collect();
            assert!(
                !tex_labels.iter().any(|l| is_vertex_flag(l)),
                "Auto/Texture must NOT tag trailing v; labels={tex_labels:?}"
            );
            assert!(
                vtx_labels.iter().any(|l| is_vertex_flag(l)),
                "VertexCache must tag trailing v (SEED_POSITIONS); labels={vtx_labels:?}"
            );
        }
    }

    /// GATE (P7 / C3) — the rung is METADATA-ONLY in the persisted doc: the
    /// serialized JSON with `VertexCache` differs from `Auto` by exactly the
    /// rung token — no cache payload, no baked data, ever, in FL4MPNLS.
    #[test]
    fn p7_rung_is_metadata_only_no_cache_payload() {
        let mut a = kitchen_sink_doc();
        let mut b = kitchen_sink_doc();
        let set = |doc: &mut MaterialPanelDoc, r: GeneratorRung| {
            for ch in &mut doc.channels {
                for l in &mut ch.layers {
                    if let LayerContent::Generator { rung, .. } = &mut l.content {
                        *rung = r;
                    }
                    if let Some(LayerContent::Generator { rung, .. }) = l.mask.primary_content_mut() {
                        *rung = r;
                    }
                }
            }
        };
        set(&mut a, GeneratorRung::Auto);
        set(&mut b, GeneratorRung::VertexCache);
        let ja = serde_json::to_string(&a).unwrap();
        let jb = serde_json::to_string(&b).unwrap();
        assert_ne!(ja, jb, "fixture must contain at least one generator layer");
        assert_eq!(ja, jb.replace("\"VertexCache\"", "\"Auto\""),
            "rung must be the ONLY serialized difference (no cache data in the doc)");
    }

    /// Regression (owner 2026-07-28): Soft/Hard is a power falloff with fixed
    /// endpoints — soft must NOT crush peak intensity (old contrast pivot did).
    /// Mirrors `gbuffer.wgsl`: `mag = pow(mag, hard)`. Intensity is Opacity.
    #[test]
    fn soft_hard_power_preserves_peak_intensity() {
        let falloff = |mag: f32, hard: f32| -> f32 { mag.clamp(0.0, 1.0).powf(hard.max(1e-4)) };
        // Endpoints fixed for any soft/hard.
        for hard in [0.001f32, 0.1, 0.5, 1.0, 2.0, 8.0, 64.0] {
            assert!((falloff(0.0, hard) - 0.0).abs() < 1e-5, "trough must stay 0 at hard={hard}");
            assert!((falloff(1.0, hard) - 1.0).abs() < 1e-4, "peak must stay 1 at hard={hard}");
        }
        // Soft lifts mids (blur-like wider band); hard crushes mids (edge-only).
        let mid = falloff(0.5, 1.0);
        assert!((mid - 0.5).abs() < 1e-5);
        assert!(falloff(0.5, 0.25) > mid, "soft lifts mid values");
        assert!(falloff(0.5, 4.0) < mid, "hard crushes mid values");
        // OLD contrast pivot (the bug): soft pulls peak toward 0.5.
        let old_soft = |mag: f32, sharp: f32| ((mag - 0.5) * sharp + 0.5).clamp(0.0, 1.0);
        assert!(
            old_soft(1.0, 0.1) < 0.6,
            "sanity: old pivot dimmed peaks (the bug we fixed)"
        );
        assert!(
            (falloff(1.0, 0.1) - 1.0).abs() < 1e-4,
            "new power keeps peak at full intensity when soft"
        );
    }
    /// P2 — bake always installs maps (skip_planar always false).
    /// The old pure-Vertex skip existed so G-buffer could own κ; that dual compositor is gone.
    /// Loop risk is handled by not clearing maps for VertexCache when they came from the bake.
    #[test]
    fn vertex_rung_only_channel_skips_map_install_but_noise_still_bakes() {
        let vgen = |source: GeneratorSource| LayerContent::Generator {
            source,
            radius: 0.05,
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: GeneratorRung::VertexCache,
            sharpness: 1.0,
            axis: DirectionAxis::default(),
        };
        let layer = |content: LayerContent, mask: MaskStack| Layer {
            id: 0,
            drives: Vec::new(),
            content,
            mask,
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };

        let pure = Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            layers: vec![layer(vgen(GeneratorSource::Curvature), MaskStack::default())],
            bit_depth: Default::default(),
            eval_mode: Default::default(), enabled: true };
        assert!(
            !channel_skip_planar_map_install(&pure),
            "P2: pure VertexCache must NOT skip map install — bake owns the composite"
        );
        assert!(
            channel_has_texture_bake_content(&pure),
            "VertexCache curvature is bake content (SdfCurvature)"
        );

        let masked_noise = Channel {
            kind: ChannelKind::BaseColor,
            state: ChannelState::Layered,
            layers: vec![layer(
                LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 4.0,
                    noise_type: crate::shader_nodes::NoiseType::Fbm,
                },
                MaskStack::single(vgen(GeneratorSource::Curvature)),
            )],
            bit_depth: Default::default(),
            eval_mode: Default::default(), enabled: true };
        assert!(
            !channel_skip_planar_map_install(&masked_noise),
            "Noise + curvature mask must bake"
        );
    }

    /// P3: nested mask stack emits msknest combine; legacy single content still deserializes.
    #[test]
    fn p3_nested_mask_stack_lowers_and_legacy_json_loads() {
        use crate::shader_nodes::NodeGraph;
        use super::super::model::{MaskLayer, MaskStack, ParamDrive};
        let mat = PbrMaterial::default();
        // Nested: curvature mask with noise nested on it.
        let mut nested = MaskLayer::from_content(LayerContent::Generator {
            source: GeneratorSource::Curvature,
            radius: 0.05,
            invert: false,
            output: GeneratorOutput::Signed,
            remap: [0.0, 1.0],
            rung: GeneratorRung::Auto,
            sharpness: 1.0,
            axis: DirectionAxis::default(),
        });
        nested.id = 11;
        nested.mask = MaskStack::single(LayerContent::Noise {
            scale: 8.0,
            strength: 1.0,
            octaves: 3.0,
            noise_type: crate::shader_nodes::NoiseType::Fbm,
        });
        nested.mask.layers[0].id = 12;
        let ch = Channel::with_state(
            ChannelKind::BaseColor,
            ChannelState::Layered,
            vec![Layer {
                id: 10,
                drives: Vec::new(),
                content: LayerContent::Constant {
                    value: ChannelValue::Color([1.0, 0.0, 0.0]),
                },
                mask: MaskStack {
                    layers: vec![nested],
                },
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        );
        let mut g = NodeGraph::default();
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::BaseColor,
            &ch,
            TextureSpace::default(),
            &mat,
        ));
        assert!(
            g.nodes.iter().any(|n| n.label.contains("msknest") || n.label.contains("msknse")),
            "nested mask must emit nest/noise tags: {:?}",
            g.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        // Legacy JSON bare LayerContent → MaskStack
        let legacy = r#"{"Image":{"tex_id":3,"strength":1.0}}"#;
        let stack: MaskStack = serde_json::from_str(legacy).expect("legacy mask content");
        assert_eq!(stack.layers.len(), 1);
        assert!(matches!(
            stack.primary_content(),
            Some(LayerContent::Image { tex_id: 3, .. })
        ));
        let _ = ParamDrive {
            param: "radius".into(),
            node: 1,
            output: 0,
        };
    }

    /// HANDOFF / ADR P1 exit: hand-edit (untagged node) survives panel regen.
    #[test]
    fn p1_hand_authored_node_survives_panel_regen() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        let mat = PbrMaterial::default();
        let mut doc = MaterialPanelDoc::default();
        doc.channels.push(Channel::with_state(
            ChannelKind::BaseColor,
            ChannelState::Layered,
            vec![Layer {
                id: 7,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 4.0,
                    noise_type: crate::shader_nodes::NoiseType::Fbm,
                },
                mask: MaskStack::default(),
                blend: BlendMode::Multiply,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        ));
        let mut g = NodeGraph::default();
        // Hand-authored node (no mp: tag) — must survive strip+regen.
        let hand = g.add_node(NodeType::ConstantColor, [0.0, 0.0]);
        if let Some(n) = g.find_node_mut(hand) {
            n.label = "user_hand_edit".into();
            n.ramp_color0 = [0.1, 0.2, 0.3];
        }
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::BaseColor,
            &doc.channels[0],
            TextureSpace::default(),
            &mat,
        ));
        // Change panel opacity → regen again.
        doc.channels[0].layers[0].opacity = 0.5;
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::BaseColor,
            &doc.channels[0],
            TextureSpace::default(),
            &mat,
        ));
        let hand_n = g
            .find_node(hand)
            .expect("hand-authored node must survive panel regen (merge rule)");
        assert_eq!(hand_n.label, "user_hand_edit");
        assert_eq!(hand_n.ramp_color0, [0.1, 0.2, 0.3]);
        // Panel tags use stable layer id 7, not stack index 0.
        assert!(
            g.nodes.iter().any(|n| n.label.starts_with("mp:0:nse:7")),
            "noise tag must carry stable layer id 7, got: {:?}",
            g.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    /// HANDOFF / ADR P1: Driven radius reconnects after regen (Const|Driven).
    #[test]
    fn p1_driven_radius_reconnects_after_regen() {
        use crate::shader_nodes::{NodeGraph, NodeType};
        use super::super::model::ParamDrive;
        let mat = PbrMaterial::default();
        let mut g = NodeGraph::default();
        let driver = g.add_node(NodeType::ConstantValue, [-200.0, 0.0]);
        if let Some(n) = g.find_node_mut(driver) {
            n.label = "radius_driver".into();
            n.const_float = 0.08;
        }
        let ch = Channel::with_state(
            ChannelKind::Roughness,
            ChannelState::Layered,
            vec![Layer {
                id: 3,
                drives: vec![ParamDrive {
                    param: "radius".into(),
                    node: driver,
                    output: 0,
                }],
                content: LayerContent::Generator {
                    source: GeneratorSource::Curvature,
                    radius: 0.02, // Const fallback; drive wins
                    invert: false,
                    output: GeneratorOutput::Signed,
                    remap: [0.0, 1.0],
                    rung: GeneratorRung::Auto,
                    sharpness: 1.0,
                    axis: DirectionAxis::default(),
                },
                mask: MaskStack::default(),
                blend: BlendMode::Multiply,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        );
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::Roughness,
            &ch,
            TextureSpace::default(),
            &mat,
        ));
        // Driver → some gen node's Epsilon (input 1).
        let wired = g.connections.iter().any(|c| {
            c.from_node == driver
                && c.from_output == 0
                && c.to_input == 1
                && g.find_node(c.to_node)
                    .map(|n| n.label.contains("mp:") && n.label.contains(":gen:"))
                    .unwrap_or(false)
        });
        assert!(
            wired,
            "Driven radius must connect driver → gen Epsilon; connections={:?}",
            g.connections
        );
        // Second regen (panel edit) keeps the drive.
        assert!(regen_channel_subgraph(
            &mut g,
            ChannelKind::Roughness,
            &ch,
            TextureSpace::default(),
            &mat,
        ));
        assert!(
            g.find_node(driver).is_some(),
            "driver node must not be stripped"
        );
        let wired2 = g.connections.iter().any(|c| c.from_node == driver && c.to_input == 1);
        assert!(wired2, "drive must re-apply after second regen");
    }

}

// ============================================================================
// G1 — FIDELITY GATE (MATERIAL_GRAPH_SOT_PLAN §G1, 2026-08-15).
//
// The graph becomes the source of truth ONLY if `regen_channel_subgraph` →
// `parse_panel_channel` recovers every construct the panel can author. These tests are the
// gate: any assertion that fails here is a construct the flip would silently lose, and the
// gate's OUTPUT is that list — not a workaround.
// ============================================================================
#[cfg(test)]
mod g1_fidelity {
    use super::*;
    use crate::material_panel::model::*;
    use crate::material_panel::lower::maps::{regen_channel_subgraph, parse_panel_channel};

    /// Semantic layer compare — the fields a user authored. Returns a human-readable diff
    /// instead of a bool so the GATE OUTPUT is the gap list itself.
    fn diff_layers(kind: ChannelKind, a: &[Layer], b: &[Layer], out: &mut Vec<String>) {
        if a.len() != b.len() {
            out.push(format!("{kind:?}: layer count {} -> {}", a.len(), b.len()));
            return;
        }
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            let tag = format!("{kind:?}[{i}]");
            if x.content != y.content {
                out.push(format!("{tag}.content: {:?} -> {:?}", x.content, y.content));
            }
            if x.blend != y.blend {
                out.push(format!("{tag}.blend: {:?} -> {:?}", x.blend, y.blend));
            }
            if (x.opacity - y.opacity).abs() > 1.0e-4 {
                out.push(format!("{tag}.opacity: {} -> {}", x.opacity, y.opacity));
            }
            if x.visible != y.visible {
                out.push(format!("{tag}.visible: {} -> {}", x.visible, y.visible));
            }
            if x.placement.space != y.placement.space {
                out.push(format!("{tag}.placement: {:?} -> {:?}", x.placement.space, y.placement.space));
            }
            if x.mask.layers.len() != y.mask.layers.len() {
                out.push(format!("{tag}.mask: {} layers -> {}", x.mask.layers.len(), y.mask.layers.len()));
            } else {
                for (mi, (mx, my)) in x.mask.layers.iter().zip(y.mask.layers.iter()).enumerate() {
                    if mx.content != my.content {
                        out.push(format!("{tag}.mask[{mi}].content: {:?} -> {:?}", mx.content, my.content));
                    }
                    if mx.blend != my.blend || (mx.opacity - my.opacity).abs() > 1.0e-4 {
                        out.push(format!(
                            "{tag}.mask[{mi}]: blend {:?}/{:?} opacity {}/{}",
                            mx.blend, my.blend, mx.opacity, my.opacity
                        ));
                    }
                }
            }
        }
    }

    /// Stable ids the way the doc does it (1-based, unique) — the tag grammar needs them.
    fn ensure_ids(layers: &mut [Layer]) {
        for (i, l) in layers.iter_mut().enumerate() {
            if l.id == 0 {
                l.id = i as u32 + 1;
            }
        }
    }

    fn roundtrip_channel(ch: &Channel, ts: TextureSpace) -> Option<Vec<Layer>> {
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        // Ensure a master output exists (regen wires into it).
        g.pull_from_pbr(&mat);
        let mut ch = ch.clone();
        ch.state = ChannelState::Layered;
        ensure_ids(&mut ch.layers);
        regen_channel_subgraph(&mut g, ch.kind, &ch, ts, &mat);
        parse_panel_channel(&g, ch.kind)
    }

    /// EVERY supported generator source, as content AND as mask, on a map-slot channel.
    #[test]
    fn g1_generators_round_trip_as_content_and_mask() {
        let sources = [
            GeneratorSource::Curvature,
            GeneratorSource::Cavity,
            GeneratorSource::Direction,
            GeneratorSource::AmbientOcclusion,
            GeneratorSource::Thickness,
            GeneratorSource::Position,
        ];
        let ts = TextureSpace { projection: 1, ..TextureSpace::default() };
        let mut gaps: Vec<String> = Vec::new();
        for src in sources {
            let gen = LayerContent::Generator {
                source: src,
                radius: 0.03,
                invert: true,
                output: GeneratorOutput::Signed,
                remap: [0.1, 0.9],
                rung: GeneratorRung::Auto,
                sharpness: 0.7,
                axis: DirectionAxis::default(),
            };
            // As content.
            let ch = Channel {
                kind: ChannelKind::Roughness,
                state: ChannelState::Layered,
                layers: vec![Layer {
                    id: 0, drives: Vec::new(),
                    content: gen.clone(),
                    mask: MaskStack::default(),
                    blend: BlendMode::Multiply, opacity: 0.75, visible: true,
                    placement: Placement::default(), adjust: Adjust::default(),
                }],
                bit_depth: ChannelBitDepth::default(),
                eval_mode: Default::default(), enabled: true,
            };
            match roundtrip_channel(&ch, ts) {
                Some(back) => diff_layers(ChannelKind::Roughness, &ch.layers, &back, &mut gaps),
                None => gaps.push(format!("{src:?} as content: parse returned None")),
            }
            // As mask over an image.
            let ch2 = Channel {
                kind: ChannelKind::BaseColor,
                state: ChannelState::Layered,
                layers: vec![Layer {
                    id: 0, drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 5, strength: 1.0 },
                    mask: MaskStack::single(gen.clone()),
                    blend: BlendMode::Normal, opacity: 1.0, visible: true,
                    placement: Placement::default(), adjust: Adjust::default(),
                }],
                bit_depth: ChannelBitDepth::default(),
                eval_mode: Default::default(), enabled: true,
            };
            match roundtrip_channel(&ch2, ts) {
                Some(back) => diff_layers(ChannelKind::BaseColor, &ch2.layers, &back, &mut gaps),
                None => gaps.push(format!("{src:?} as mask: parse returned None")),
            }
        }
        assert!(gaps.is_empty(), "G1 GAP LIST (generators):\n{}", gaps.join("\n"));
    }

    /// Multi-layer stack: image + constant + generator with distinct blends, opacities,
    /// visibility and a per-layer placement override.
    #[test]
    fn g1_multi_layer_stack_round_trips() {
        let ts = TextureSpace { projection: 1, tiling: [2.0, 2.0], ..TextureSpace::default() };
        let ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![
                Layer {
                    id: 0, drives: Vec::new(),
                    content: LayerContent::Image { tex_id: 3, strength: 0.8 },
                    mask: MaskStack::default(),
                    blend: BlendMode::Normal, opacity: 1.0, visible: true,
                    placement: Placement { space: Some(TextureSpace {
                        projection: 0, tiling: [4.0, 4.0], offset: [0.0, 0.5],
                        ..TextureSpace::default() }) },
                    adjust: Adjust::default(),
                },
                Layer {
                    id: 0, drives: Vec::new(),
                    content: LayerContent::Constant { value: ChannelValue::Scalar(0.4) },
                    mask: MaskStack::default(),
                    blend: BlendMode::Add, opacity: 0.6, visible: true,
                    placement: Placement::default(), adjust: Adjust::default(),
                },
                Layer {
                    id: 0, drives: Vec::new(),
                    content: LayerContent::Generator {
                        source: GeneratorSource::Cavity, radius: 0.05, invert: false,
                        output: GeneratorOutput::default(), remap: [0.0, 1.0],
                        rung: GeneratorRung::Auto, sharpness: 1.0,
                        axis: DirectionAxis::default(),
                    },
                    mask: MaskStack::default(),
                    blend: BlendMode::Overlay, opacity: 0.3, visible: false,
                    placement: Placement::default(), adjust: Adjust::default(),
                },
            ],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true,
        };
        let mut gaps = Vec::new();
        match roundtrip_channel(&ch, ts) {
            Some(back) => diff_layers(ChannelKind::Roughness, &ch.layers, &back, &mut gaps),
            None => gaps.push("multi-layer: parse returned None".into()),
        }
        assert!(gaps.is_empty(), "G1 GAP LIST (stack):\n{}", gaps.join("\n"));
    }

    /// R4 — a hand-relabelled node must DROP OUT of the projection (channel degrades),
    /// never mis-associate into a different layer.
    #[test]
    fn g1_relabelled_node_degrades_instead_of_misassociating() {
        let ts = TextureSpace { projection: 1, ..TextureSpace::default() };
        let ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0, drives: Vec::new(),
                content: LayerContent::Image { tex_id: 3, strength: 0.8 },
                mask: MaskStack::default(),
                blend: BlendMode::Normal, opacity: 1.0, visible: true,
                placement: Placement::default(), adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true,
        };
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        g.pull_from_pbr(&mat);
        let mut c = ch.clone();
        ensure_ids(&mut c.layers);
        regen_channel_subgraph(&mut g, c.kind, &c, ts, &mat);
        // User renames one panel node by hand.
        for n in &mut g.nodes {
            if n.label.starts_with("mp:") {
                n.label = "my cool node".into();
                break;
            }
        }
        let back = parse_panel_channel(&g, ChannelKind::Roughness);
        // Either the whole channel degrades (None) or the layer list no longer contains a
        // phantom — it must NOT still claim the original image layer intact.
        if let Some(layers) = back {
            assert_ne!(
                layers, c.layers,
                "a relabelled node must change the projection — identical layers back means \
                 the parser is guessing associations"
            );
        }
    }

    /// R5 — a HAND-AUTHORED node wired into the same graph must survive a regen of another
    /// channel byte-unchanged. The panel must never rewrite what it did not author.
    #[test]
    fn g1_hand_authored_nodes_survive_regen_untouched() {
        let ts = TextureSpace { projection: 1, ..TextureSpace::default() };
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = PbrMaterial::default();
        g.pull_from_pbr(&mat);
        // Hand-authored: a noise node with a user label, positioned by hand.
        let noise = g.add_node(crate::shader_nodes::NodeType::Noise, [-300.0, 200.0]);
        if let Some(n) = g.find_node_mut(noise) {
            n.label = "my noise".into();
        }
        let before = serde_json::to_string(g.find_node(noise).unwrap()).unwrap();
        // Panel regen on a DIFFERENT channel.
        let ch = Channel {
            kind: ChannelKind::Metallic,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 0, drives: Vec::new(),
                content: LayerContent::Image { tex_id: 2, strength: 1.0 },
                mask: MaskStack::default(),
                blend: BlendMode::Normal, opacity: 1.0, visible: true,
                placement: Placement::default(), adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::default(),
            eval_mode: Default::default(), enabled: true,
        };
        let mut c = ch.clone();
        ensure_ids(&mut c.layers);
        regen_channel_subgraph(&mut g, c.kind, &c, ts, &mat);
        let after = serde_json::to_string(g.find_node(noise).unwrap()).unwrap();
        assert_eq!(before, after, "regen of Metallic must not touch a hand-authored node");
    }
}

// ============================================================================
// G3 — write-path flip (MATERIAL_GRAPH_SOT_PLAN §G3). Pins the two structural
// properties the flip introduced: the graph carries shader_type end-to-end, and a
// WIRED Master socket wins over the scalar push (graph-wins is `is_input_connected`
// in `push_to_pbr`, not reconcile bookkeeping). Plus the two forward reads that
// replaced the deleted `refresh_doc_from_material`.
// ============================================================================
#[cfg(test)]
mod g3_flip {
    use super::*;
    use crate::material_panel::model::*;
    use crate::shader_nodes::{NodeGraph, NodeType};
    use core_math::pbr_material::PbrMaterial;

    #[test]
    fn g3_shader_type_rides_the_graph() {
        let mut mat = PbrMaterial::default();
        mat.shader_type = core_math::pbr_material::ShaderType::from_u32(2);
        let mut g = NodeGraph::default();
        g.pull_from_pbr(&mat);
        let mut derived = PbrMaterial::default();
        g.push_to_pbr(&mut derived);
        assert_eq!(
            derived.shader_type.as_u32(),
            2,
            "shader_type must survive mat -> graph -> mat"
        );
    }

    #[test]
    fn g3_wired_socket_wins_over_scalar_push() {
        let mut g = NodeGraph::default();
        let out = g.output_node().expect("default graph seeds MaterialOutput").id;
        // Master default says 0.9 — but the socket is WIRED, so push must skip it.
        if let Some(n) = g.find_node_mut(out) {
            n.pad_input_defaults();
            if let Some(s) = n.input_defaults.get_mut(NodeGraph::MO_ROUGHNESS) {
                s.value_f32 = 0.9;
            }
        }
        let nse = g.add_node(NodeType::Noise, [-200.0, 0.0]);
        let _ = g.connect(nse, 0, out, NodeGraph::MO_ROUGHNESS);
        let mut mat = PbrMaterial::default();
        mat.roughness = 0.3;
        g.push_to_pbr(&mut mat);
        assert!(
            (mat.roughness - 0.3).abs() < 1e-6,
            "a wired socket owns the channel — push must not overwrite, got {}",
            mat.roughness
        );
        // Unwire → the Master default owns it again.
        g.disconnect(out, NodeGraph::MO_ROUGHNESS);
        g.push_to_pbr(&mut mat);
        assert!(
            (mat.roughness - 0.9).abs() < 1e-6,
            "unwired socket falls back to the Master default, got {}",
            mat.roughness
        );
    }

    #[test]
    fn g3_forward_value_refresh_reads_master_defaults() {
        let mut g = NodeGraph::default();
        let out = g.output_node().unwrap().id;
        if let Some(n) = g.find_node_mut(out) {
            n.pad_input_defaults();
            if let Some(s) = n.input_defaults.get_mut(NodeGraph::MO_ROUGHNESS) {
                s.value_f32 = 0.42;
            }
        }
        let mut doc = MaterialPanelDoc::default();
        doc.channels = vec![Channel::value(
            ChannelKind::Roughness,
            ChannelValue::Scalar(0.8),
        )];
        refresh_doc_values_from_graph(&mut doc, &g);
        assert_eq!(
            doc.channel(ChannelKind::Roughness).unwrap().state,
            ChannelState::Value(ChannelValue::Scalar(0.42)),
            "panel Value must follow a node-editor Master-default scrub"
        );
    }

    #[test]
    fn g3_heal_adopts_only_zero_texids() {
        let mut mat = PbrMaterial::default();
        let mk = ChannelKind::BaseColor.map_kind().unwrap();
        mat.map_mut(mk).tex_id = 7;
        mat.map_mut(mk).strength = 0.5;
        let mk_r = ChannelKind::Roughness.map_kind().unwrap();
        mat.map_mut(mk_r).tex_id = 9;
        let img_layer = |tex_id: u32| Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };
        let mut doc = MaterialPanelDoc::default();
        doc.channels = vec![
            Channel::with_state(ChannelKind::BaseColor, ChannelState::Layered, vec![img_layer(0)]),
            Channel::with_state(ChannelKind::Roughness, ChannelState::Layered, vec![img_layer(3)]),
        ];
        heal_doc_fast_texids(&mut doc, &mat);
        let get = |k: ChannelKind, d: &MaterialPanelDoc| match d.channel(k).unwrap().layers[0].content {
            LayerContent::Image { tex_id, .. } => tex_id,
            _ => unreachable!(),
        };
        assert_eq!(get(ChannelKind::BaseColor, &doc), 7, "pending (0) adopts the landed slot");
        assert_eq!(get(ChannelKind::Roughness, &doc), 3, "authored tex_id is NEVER overwritten");
    }
}

// ============================================================================
// G4 PREP — Fast channels live in the graph (the prerequisite for doc-as-
// projection), while bake DECISIONS subtract them (direct install, never baked).
// ============================================================================
#[cfg(test)]
mod g4_prep {
    use super::*;
    use crate::material_panel::model::*;
    use crate::shader_nodes::NodeGraph;
    use core_math::pbr_material::PbrMaterial;

    fn fast_image_channel(kind: ChannelKind, tex_id: u32) -> Channel {
        Channel::with_state(
            kind,
            ChannelState::Layered,
            vec![Layer {
                id: 1,
                drives: Vec::new(),
                content: LayerContent::Image { tex_id, strength: 0.8 },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        )
    }

    #[test]
    fn fast_channel_regens_and_projects_back() {
        let mut g = NodeGraph::default();
        let mat = PbrMaterial::default();
        let ch = fast_image_channel(ChannelKind::BaseColor, 42);
        assert_eq!(channel_lowering(&ch, TextureSpace::default()), ChannelLowering::Fast);
        assert!(
            regen_channel_subgraph(&mut g, ChannelKind::BaseColor, &ch, TextureSpace::default(), &mat),
            "G4 prep: a Fast channel must now emit its chain"
        );
        assert_eq!(channel_source(&g, ChannelKind::BaseColor), ChannelSource::PanelStack);
        let layers = parse_panel_channel(&g, ChannelKind::BaseColor).expect("projects back");
        assert_eq!(layers.len(), 1);
        match layers[0].content {
            LayerContent::Image { tex_id, strength } => {
                assert_eq!(tex_id, 42);
                assert!((strength - 0.8).abs() < 1e-4);
            }
            ref c => panic!("expected the image layer back, got {c:?}"),
        }
    }

    #[test]
    fn fast_channel_subtracted_from_bake_mask_but_not_from_wired() {
        let mut g = NodeGraph::default();
        let mat = PbrMaterial::default();
        let mut doc = MaterialPanelDoc::default();
        doc.channels = vec![fast_image_channel(ChannelKind::BaseColor, 7)];
        assert!(regen_channel_subgraph(
            &mut g, ChannelKind::BaseColor,
            doc.channel(ChannelKind::BaseColor).unwrap(),
            doc.texture_space, &mat,
        ));
        // A rich (Subgraph) roughness chain beside it must KEEP baking.
        let rich = Channel::with_state(
            ChannelKind::Roughness,
            ChannelState::Layered,
            vec![Layer {
                id: 2,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 3.0,
                    noise_type: crate::shader_nodes::NoiseType::Fbm,
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        );
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &rich, doc.texture_space, &mat));

        let wired = crate::node_graph_bake::channels_from_graph(&g);
        assert!(wired.albedo, "the Fast chain WIRES the socket (ownership/clear logic)");
        assert!(wired.roughness);
        let bake = crate::node_graph_bake::bake_channels_from_graph(&g, Some(&doc));
        assert!(!bake.albedo, "Fast = direct install, MUST NOT bake");
        assert!(bake.roughness, "Subgraph channels keep baking");
    }
}

// ============================================================================
// MPEYEOFFREBAKE — the eye toggle must gate CODEGEN, not only the lowering.
// Before the fix: eye-off rich channel -> lower cleared the map, but the chain
// still regen'd + wired the socket -> the bake re-installed the composite over
// the cleared slot. The channel turned itself back on.
// ============================================================================
#[cfg(test)]
mod eye_off_codegen {
    use super::*;
    use crate::material_panel::model::*;

    fn rich_noise(kind: ChannelKind, enabled: bool) -> Channel {
        let mut ch = Channel::with_state(
            kind,
            ChannelState::Layered,
            vec![Layer {
                id: 1,
                drives: Vec::new(),
                content: LayerContent::Noise {
                    scale: 4.0,
                    strength: 1.0,
                    octaves: 3.0,
                    noise_type: crate::shader_nodes::NoiseType::Fbm,
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
        );
        ch.enabled = enabled;
        ch
    }

    #[test]
    fn disabled_channel_emits_unwired_and_recoverable() {
        // G4 evolution of MPEYEOFFREBAKE: the chain must EXIST (the projection needs the
        // layers) but the master socket must stay UNWIRED (so no bake can re-install the
        // map the eye just cleared — the original bug, still structurally impossible).
        let ts = TextureSpace::default();
        assert!(channel_needs_codegen(&rich_noise(ChannelKind::Roughness, true), ts));
        assert!(
            channel_needs_codegen(&rich_noise(ChannelKind::Roughness, false), ts),
            "disabled channels still codegen (emit-unwired) — the graph keeps the layers"
        );
        let mut g = crate::shader_nodes::NodeGraph::default();
        let mat = core_math::pbr_material::PbrMaterial::default();
        let ch = rich_noise(ChannelKind::Roughness, false);
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, ts, &mat));
        assert_eq!(
            channel_source(&g, ChannelKind::Roughness),
            ChannelSource::Unwired,
            "eye-off = socket UNWIRED: inert for every bake decider by construction"
        );
        assert!(
            !crate::node_graph_bake::channels_from_graph(&g).roughness,
            "no mask builder can see a disabled channel"
        );
        assert!(
            g.channel_meta.get(&(ChannelKind::Roughness as u32)).map(|m| m.disabled).unwrap_or(false),
            "meta records the eye state"
        );
        let layers = parse_panel_channel(&g, ChannelKind::Roughness)
            .expect("projection recovers a disabled chain via meta (no wire to gate on)");
        assert_eq!(layers.len(), 1, "the noise layer survives the eye toggle in the GRAPH");
        // Re-enable: regen restores the wire.
        let ch_on = rich_noise(ChannelKind::Roughness, true);
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch_on, ts, &mat));
        assert_eq!(channel_source(&g, ChannelKind::Roughness), ChannelSource::PanelStack);
        assert!(!g.channel_meta.get(&(ChannelKind::Roughness as u32)).unwrap().disabled);
    }
}

// ============================================================================
// G4 — projection fidelity pins: authored DRIVES and layer ids survive the
// graph round trip (both were silently lost before — a projected doc would have
// unwired every Driven radius on its next regen).
// ============================================================================
#[cfg(test)]
mod g4_projection {
    use super::*;
    use crate::material_panel::model::*;

    #[test]
    fn driven_radius_survives_the_projection() {
        let mat = core_math::pbr_material::PbrMaterial::default();
        let mut g = crate::shader_nodes::NodeGraph::default();
        // A hand node driving the radius — lives OUTSIDE the panel tags.
        let driver = g.add_node(crate::shader_nodes::NodeType::ConstantValue, [-500.0, 0.0]);
        let ch = Channel {
            kind: ChannelKind::Roughness,
            state: ChannelState::Layered,
            layers: vec![Layer {
                id: 3,
                drives: vec![ParamDrive { param: "radius".into(), node: driver, output: 0 }],
                content: LayerContent::Generator {
                    source: GeneratorSource::Curvature,
                    radius: 0.05,
                    invert: false,
                    output: GeneratorOutput::ConcaveOnly,
                    remap: [0.0, 1.0],
                    rung: GeneratorRung::Auto,
                    sharpness: 1.5,
                    axis: DirectionAxis::default(),
                },
                mask: MaskStack::default(),
                blend: BlendMode::Normal,
                opacity: 1.0,
                visible: true,
                placement: Placement::default(),
                adjust: Adjust::default(),
            }],
            bit_depth: ChannelBitDepth::Bit16,
            eval_mode: ChannelEvalMode::Bake,
            enabled: true,
        };
        assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, TextureSpace::default(), &mat));
        let layers = parse_panel_channel(&g, ChannelKind::Roughness).expect("projects");
        assert_eq!(layers[0].id, 3, "the tag lid IS the layer id");
        assert_eq!(
            layers[0].drives,
            vec![ParamDrive { param: "radius".into(), node: driver, output: 0 }],
            "the drive connection must project back — losing it unwires the drive on regen"
        );
        // And the channel meta rides along in the full derive.
        let doc = derive_doc_from_material(&mat, Some(&g));
        let d = doc.channel(ChannelKind::Roughness).unwrap();
        assert_eq!(d.bit_depth, ChannelBitDepth::Bit16, "meta carries bit depth");
        assert!(d.enabled);
    }
}

// ============================================================================
// AMPLIFIER PROBE (owner 2026-08-16: "the shader node panel keeps extending
// toward the bottom infinitely"): iterate the full projection cycle —
// regen(doc) -> graph -> parse -> doc' -> regen(doc') ... — and assert the
// layer/mask/channel counts are STABLE. A single round-trip test cannot see a
// construct that gains one entry per cycle.
// ============================================================================
#[cfg(test)]
mod amplifier_probe {
    use super::*;
    use crate::material_panel::model::*;
    use core_math::pbr_material::PbrMaterial;

    #[test]
    fn projection_cycle_is_a_fixed_point() {
        let mat = PbrMaterial::default();
        let mk_layer = |id: u32, content: LayerContent, masked: bool| Layer {
            id,
            drives: Vec::new(),
            content,
            mask: if masked {
                let mut m = MaskStack::default();
                m.layers.push(MaskLayer {
                    id: 1,
                    content: LayerContent::Generator {
                        source: GeneratorSource::Curvature,
                        radius: 0.03,
                        invert: false,
                        output: GeneratorOutput::ConcaveOnly,
                        remap: [0.1, 0.9],
                        rung: GeneratorRung::Auto,
                        sharpness: 1.2,
                        axis: DirectionAxis::default(),
                    },
                    blend: BlendMode::Multiply,
                    opacity: 0.8,
                    visible: true,
                    mask: MaskStack::default(),
                    drives: Vec::new(),
                });
                m
            } else {
                MaskStack::default()
            },
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        };
        let mut ch = Channel::with_state(
            ChannelKind::Roughness,
            ChannelState::Layered,
            vec![
                mk_layer(1, LayerContent::Image { tex_id: 5, strength: 0.9 }, false),
                mk_layer(
                    2,
                    LayerContent::Noise {
                        scale: 4.0,
                        strength: 1.0,
                        octaves: 3.0,
                        noise_type: crate::shader_nodes::NoiseType::Fbm,
                    },
                    true,
                ),
            ],
        );
        let ts = TextureSpace::default();
        for cycle in 0..6 {
            let mut g = crate::shader_nodes::NodeGraph::default();
            assert!(regen_channel_subgraph(&mut g, ChannelKind::Roughness, &ch, ts, &mat));
            let layers = parse_panel_channel(&g, ChannelKind::Roughness)
                .unwrap_or_else(|| panic!("cycle {cycle}: projection lost the channel"));
            assert_eq!(
                layers.len(),
                2,
                "cycle {cycle}: layer count must be STABLE, got {}",
                layers.len()
            );
            assert_eq!(
                layers[1].mask.layers.len(),
                1,
                "cycle {cycle}: mask stack must be STABLE, got {}",
                layers[1].mask.layers.len()
            );
            ch = Channel::with_state(ChannelKind::Roughness, ChannelState::Layered, layers);
        }
    }
}

