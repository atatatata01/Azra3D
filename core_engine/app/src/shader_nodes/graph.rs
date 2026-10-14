// shader_nodes/graph.rs
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use super::types::SocketType;
use super::ops::*;
use super::node_type::NodeType;
use super::node_data::{Node, NodeId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub from_node: NodeId,
    pub from_output: usize,  // output socket index
    pub to_node: NodeId,
    pub to_input: usize,     // input socket index
}

// ====================================================================
// Node Graph â€” container for nodes + connections
// ====================================================================
fn default_bake_res() -> u32 {
    256 // full-quality Apply bake; still ≤512 hard cap on CPU bake
}

/// Params for **instant GPU live preview** (fragment-shader FBM — no CPU bake).
/// Supported: Noise → Albedo, Noise → ColorRamp → Albedo, optional Noise → Roughness.
#[derive(Clone, Debug)]
pub struct LiveGpuNoise {
    pub scale: f32,
    pub octaves: u32,
    pub seed: f32,
    /// Base albedo at noise=0
    pub color0: [f32; 3],
    /// Albedo at noise=1
    pub color1: [f32; 3],
    pub roughness0: f32,
    pub roughness1: f32,
    pub drives_albedo: bool,
    pub drives_roughness: bool,
    /// Advanced blend curve (Linear / Smooth / Levels…).
    pub curve: NoiseBlendCurve,
    pub curve_k: f32,
    pub black: f32,
    pub white: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeGraph {
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
    pub next_id: NodeId,
    /// Cached WGSL code (regenerated on graph change)
    pub wgsl_cache: Option<String>,
    /// CPU map-bake working resolution (64..512). Set on Master Material node. Upsampled to MAP_RES.
    /// Live scrubbing and Apply use this same resolution — no downrez preview.
    #[serde(default = "default_bake_res")]
    pub bake_res: u32,
    /// G3 (MATERIAL_GRAPH_SOT_PLAN): shader model (`ShaderType as u32`). Lived only on the
    /// panel doc before — the one authored material property the graph could NOT express,
    /// which made graph-as-source-of-truth a lie for it. Carried by `pull_from_pbr` /
    /// `push_to_pbr`. FL4NODES graph JSON is name-tagged serde → `default` is additive-safe.
    #[serde(default)]
    pub shader_type: u32,
    /// G4: material-level texture space (tiling/offset/projection all maps inherit).
    /// Written from the doc on every regen; the projection reads it back.
    #[serde(default)]
    pub texture_space: crate::material_panel::model::TextureSpace,
    /// G4: [`GeneratorMode`] wire code (baked vs live generator sampling — a doc-level
    /// user choice, carried here so the doc can become a projection).
    #[serde(default)]
    pub generator_mode: u32,
    /// G4: per-channel authored state with NO node home — key = `ChannelKind as u32`.
    /// `disabled` marks the eye toggle: the channel's chain is EMITTED but its master
    /// socket left UNWIRED (inert for bake/eval BY CONSTRUCTION — no decider needs to
    /// consult this map), and the projection recovers the layers by tags + this flag.
    /// A BTreeMap so graph JSON stays deterministic (HashMap order would churn the
    /// no-op snapshot compare in `mp_lower_rich_channels`).
    #[serde(default)]
    pub channel_meta: std::collections::BTreeMap<u32, ChannelMeta>,
}

/// G4: authored per-channel state that has no node to live on (see `channel_meta`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChannelMeta {
    /// `ChannelBitDepth::Bit16` (8-bit is the default wire state).
    #[serde(default)]
    pub bit16: bool,
    /// `ChannelEvalMode::Live` (Bake is the default wire state).
    #[serde(default)]
    pub live: bool,
    /// Eye toggle OFF — chain emitted, master socket deliberately unwired.
    #[serde(default)]
    pub disabled: bool,
}

impl Default for NodeGraph {
    fn default() -> Self {
        // Minimal empty Master Material graph — user builds from scratch (or library fills it).
        let mut g = Self {
            nodes: vec![],
            connections: vec![],
            next_id: 0,
            wgsl_cache: None,
            bake_res: default_bake_res(),
            shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
        };
        let _out = g.add_node(NodeType::MaterialOutput, [400.0, 100.0]);
        g
    }
}

impl NodeGraph {
    /// Build a **unified** material graph from library PBR map channels: one Image Texture
    /// per map, wired into Master Material sockets. Same system as hand-authored graphs.
    /// Map pixels still live on the material's `TexMap` slots (ImageTexture samples them on bake).
    /// UNUSED since 2026-08-06, and deliberately so — do NOT re-wire this into the
    /// library drop path.
    ///
    /// It builds one `ImageTexture -> MO_*` wire per map kind. That makes every channel
    /// report `master_socket_driven`, and `derive_doc_from_material` gives the graph
    /// precedence over the material's map slot, so the panel showed `GraphDriven` badges
    /// with NO editable parameters and the only exit was the destructive "Revert to panel
    /// control". `reconcile_doc_with_graph` also re-promotes any untagged-but-driven
    /// channel on every bake, so reinterpreting these nodes in the derive is not a fix
    /// either — the wiring itself has to be absent.
    ///
    /// A library material is one plain image per channel, which the panel's Layered Fast
    /// path already represents exactly (and lowers to the same TexMap the renderer reads).
    /// `material_import` now calls `mp_mark_hand_loaded_map` instead. Kept only as
    /// reference for a future "explode a material into nodes" command, which would need
    /// `mp:`-tagged labels to coexist with the panel.
    pub fn from_library_pbr_maps(kinds: &[core_math::pbr_material::MapKind]) -> Self {
        use core_math::pbr_material::MapKind;
        let mut g = Self {
            nodes: vec![],
            connections: vec![],
            next_id: 0,
            wgsl_cache: None,
            bake_res: default_bake_res(),
            shader_type: 0,
            texture_space: Default::default(),
            generator_mode: 0,
            channel_meta: Default::default(),
        };
        let out = g.add_node(NodeType::MaterialOutput, [520.0, 40.0]);
        let mut y = 20.0f32;
        for &kind in kinds {
            let img = g.add_node(NodeType::ImageTexture, [40.0, y]);
            if let Some(n) = g.find_node_mut(img) {
                n.image_map_kind = kind.wire_tag();
                n.label = kind.label().to_string();
                // Match library import drop contract: Box / triplanar projection.
                n.projection = ProjectionStyle::Triplanar;
            }
            let mo = match kind {
                MapKind::Albedo => Self::MO_ALBEDO,
                MapKind::Roughness => Self::MO_ROUGHNESS,
                MapKind::Metallic => Self::MO_METALLIC,
                MapKind::Emission => Self::MO_EMISSION,
                MapKind::Normal => Self::MO_NORMAL,
                MapKind::Bump => Self::MO_HEIGHT,
                MapKind::Displacement => Self::MO_DISPLACEMENT,
                MapKind::SssColor => Self::MO_SSS_COLOR,
            };
            // Color out (0) → Master socket (Float/Color/Vec3 all compatible).
            let _ = g.connect(img, 0, out, mo);
            y += 140.0;
        }
        g
    }

    pub fn add_node(&mut self, node_type: NodeType, pos: [f32; 2]) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(Node::new(id, node_type, pos));
        self.wgsl_cache = None;
        id
    }

    /// G6: delete every node in `sel` except MaterialOutput (Blender `X` on a selection).
    /// Returns how many were removed.
    pub fn remove_nodes(&mut self, sel: &std::collections::HashSet<NodeId>) -> usize {
        let victims: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| sel.contains(&n.id) && n.node_type != NodeType::MaterialOutput)
            .map(|n| n.id)
            .collect();
        for id in &victims {
            self.remove_node(*id);
        }
        victims.len()
    }

    /// G6: duplicate every node in `sel` (Blender `Shift+D`), preserving RELATIVE offsets and
    /// re-creating the links BETWEEN duplicated nodes (links to unselected nodes are not
    /// carried — Blender's behavior). Returns old→new id mapping; MaterialOutput never
    /// duplicates.
    pub fn duplicate_nodes(
        &mut self,
        sel: &std::collections::HashSet<NodeId>,
        offset: [f32; 2],
    ) -> std::collections::HashMap<NodeId, NodeId> {
        let mut map = std::collections::HashMap::new();
        let originals: Vec<Node> = self
            .nodes
            .iter()
            .filter(|n| sel.contains(&n.id) && n.node_type != NodeType::MaterialOutput)
            .cloned()
            .collect();
        for src in &originals {
            let new_id = self.add_node(src.node_type.clone(), [
                src.pos[0] + offset[0],
                src.pos[1] + offset[1],
            ]);
            if let Some(dst) = self.find_node_mut(new_id) {
                let keep_id = dst.id;
                let keep_pos = dst.pos;
                *dst = src.clone();
                dst.id = keep_id;
                dst.pos = keep_pos;
            }
            map.insert(src.id, new_id);
        }
        // Internal links only: both endpoints duplicated.
        let internal: Vec<Connection> = self
            .connections
            .iter()
            .filter(|l| map.contains_key(&l.from_node) && map.contains_key(&l.to_node))
            .cloned()
            .collect();
        for l in internal {
            let _ = self.connect(map[&l.from_node], l.from_output, map[&l.to_node], l.to_input);
        }
        map
    }

    /// G6: toggle mute on the selection (Blender `M`). MaterialOutput never mutes.
    /// Returns the new state applied (majority-off → all on, else all off — Blender toggles
    /// per-node; a uniform toggle is more predictable for a set).
    pub fn toggle_mute(&mut self, sel: &std::collections::HashSet<NodeId>) -> bool {
        let any_unmuted = self
            .nodes
            .iter()
            .any(|n| sel.contains(&n.id) && !n.muted && n.node_type != NodeType::MaterialOutput);
        for n in &mut self.nodes {
            if sel.contains(&n.id) && n.node_type != NodeType::MaterialOutput {
                n.muted = any_unmuted;
            }
        }
        self.wgsl_cache = None;
        any_unmuted
    }

    /// G6: toggle collapse on the selection (Blender `H`). Display-only.
    pub fn toggle_collapse(&mut self, sel: &std::collections::HashSet<NodeId>) {
        let any_open = self.nodes.iter().any(|n| sel.contains(&n.id) && !n.collapsed);
        for n in &mut self.nodes {
            if sel.contains(&n.id) {
                n.collapsed = any_open;
            }
        }
    }

    pub fn remove_node(&mut self, id: NodeId) {
        self.connections.retain(|c| c.from_node != id && c.to_node != id);
        self.nodes.retain(|n| n.id != id);
        self.wgsl_cache = None;
    }

    pub fn connect(&mut self, from_node: NodeId, from_output: usize, to_node: NodeId, to_input: usize) -> bool {
        // Validate: no self-connections, types compatible
        if from_node == to_node { return false; }
        let from_n = self.nodes.iter().find(|n| n.id == from_node);
        let to_n = self.nodes.iter().find(|n| n.id == to_node);
        if from_n.is_none() || to_n.is_none() { return false; }
        let from_n = from_n.unwrap();
        let to_n = to_n.unwrap();
        let outputs = Node::output_defs(&from_n.node_type);
        let inputs = Node::input_defs(&to_n.node_type);
        if from_output >= outputs.len() || to_input >= inputs.len() { return false; }
        if !outputs[from_output].socket_type.compatible(&inputs[to_input].socket_type) { return false; }
        if self.topo_sort(from_node).is_none_or(|order| order.contains(&to_node)) { return false; }
        if matches!(to_n.node_type, NodeType::MaterialOutput)
            && to_input == Self::MO_NORMAL
            && outputs[from_output].socket_type == SocketType::Float
        {
            let pos = [from_n.pos[0] + 340.0, from_n.pos[1]];
            let converter = self.add_node(NodeType::HeightToNormal, pos);
            assert!(self.connect(from_node, from_output, converter, 0));
            assert!(self.connect(converter, 0, to_node, to_input));
            return true;
        }

        // Remove existing connection to this input (only one source per input)
        self.connections.retain(|c| !(c.to_node == to_node && c.to_input == to_input));
        self.connections.push(Connection { from_node, from_output, to_node, to_input });
        self.wgsl_cache = None;
        true
    }

    pub fn disconnect(&mut self, to_node: NodeId, to_input: usize) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| !(c.to_node == to_node && c.to_input == to_input));
        let changed = self.connections.len() != before;
        if changed {
            self.wgsl_cache = None;
        }
        changed
    }

    pub fn disconnect_output(&mut self, from_node: NodeId, from_output: usize) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|c| !(c.from_node == from_node && c.from_output == from_output));
        let changed = self.connections.len() != before;
        if changed {
            self.wgsl_cache = None;
        }
        changed
    }

    pub fn find_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn find_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Find the MaterialOutput node
    pub fn output_node(&self) -> Option<&Node> {
        self.nodes.iter().find(|n| n.node_type == NodeType::MaterialOutput)
    }

    pub fn output_node_mut(&mut self) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.node_type == NodeType::MaterialOutput)
    }

    pub fn is_input_connected(&self, node_id: NodeId, input_idx: usize) -> bool {
        self.connections
            .iter()
            .any(|c| c.to_node == node_id && c.to_input == input_idx)
    }

    /// Master Material socket indices (must match `nodes/material_output.rs`).
    pub const MO_ALBEDO: usize = 0;
    pub const MO_ROUGHNESS: usize = 1;
    pub const MO_METALLIC: usize = 2;
    pub const MO_OPACITY: usize = 3;
    pub const MO_EMISSION: usize = 4;
    pub const MO_AO: usize = 5;
    // Advanced (Properties Advanced parity)
    pub const MO_HEIGHT: usize = 6;
    pub const MO_NORMAL: usize = 7;
    pub const MO_CLEARCOAT: usize = 8;
    pub const MO_CLEARCOAT_ROUGH: usize = 9;
    pub const MO_IOR: usize = 10;
    pub const MO_SHEEN: usize = 11;
    pub const MO_EMISSION_INTENSITY: usize = 12;
    pub const MO_NORMAL_STRENGTH: usize = 13;
    pub const MO_SHEEN_TINT: usize = 14;
    pub const MO_ANISOTROPY: usize = 15;
    pub const MO_ANISOTROPY_ROT: usize = 16;
    pub const MO_SSS_RADIUS: usize = 17;
    pub const MO_SSS_COLOR: usize = 18;
    pub const MO_GLASS_TINT: usize = 19;
    pub const MO_DISPLACEMENT: usize = 20;
    pub const MO_BUMP_STRENGTH: usize = 21;

    pub(crate) fn spatial_bump_strength(&self) -> bool {
        self.master_socket_driven(Self::MO_HEIGHT) && self.master_socket_driven(Self::MO_BUMP_STRENGTH)
    }

    pub const MO_BASE_COUNT: usize = super::nodes::material_output::BASE_COUNT;
    pub const MO_ADVANCED_START: usize = super::nodes::material_output::ADVANCED_START;

    /// **One material:** copy `PbrMaterial` scalars into Master Material socket defaults.
    /// Properties panel and Master Material node are the same material — not two copies.
    /// Always writes defaults (including as disconnect fallback when a socket is wired).
    pub fn pull_from_pbr(&mut self, mat: &core_math::pbr_material::PbrMaterial) {
        self.shader_type = mat.shader_type.as_u32();
        let Some(n) = self.output_node_mut() else { return };
        n.pad_input_defaults();
        let d = &mut n.input_defaults;
        let set_f = |d: &mut [super::types::InputDefault], i: usize, v: f32| {
            if let Some(slot) = d.get_mut(i) {
                slot.value_f32 = v;
            }
        };
        let set_v = |d: &mut [super::types::InputDefault], i: usize, v: [f32; 3]| {
            if let Some(slot) = d.get_mut(i) {
                slot.value_vec3 = v;
            }
        };
        set_v(d, Self::MO_ALBEDO, [mat.albedo.x, mat.albedo.y, mat.albedo.z]);
        set_f(d, Self::MO_ROUGHNESS, mat.roughness);
        set_f(d, Self::MO_METALLIC, mat.metallic);
        set_f(d, Self::MO_OPACITY, mat.opacity);
        set_v(
            d,
            Self::MO_EMISSION,
            [
                mat.emission_color.x,
                mat.emission_color.y,
                mat.emission_color.z,
            ],
        );
        // AO is graph-only mask (no dedicated PbrMaterial field) — leave default.
        set_f(d, Self::MO_CLEARCOAT, mat.clearcoat);
        set_f(d, Self::MO_CLEARCOAT_ROUGH, mat.clearcoat_roughness);
        set_f(d, Self::MO_IOR, mat.ior);
        set_f(d, Self::MO_SHEEN, mat.sheen);
        set_f(d, Self::MO_EMISSION_INTENSITY, mat.emission_intensity);
        set_f(d, Self::MO_NORMAL_STRENGTH, mat.normal_strength);
        if mat.bump_map.tex_id != 0 {
            set_f(d, Self::MO_BUMP_STRENGTH, mat.bump_map.strength);
        }
        set_f(d, Self::MO_SHEEN_TINT, mat.sheen_tint);
        set_f(d, Self::MO_ANISOTROPY, mat.anisotropy);
        set_f(d, Self::MO_ANISOTROPY_ROT, mat.anisotropy_rotation);
        set_f(d, Self::MO_SSS_RADIUS, mat.sss_radius);
        set_v(
            d,
            Self::MO_SSS_COLOR,
            [mat.sss_color.x, mat.sss_color.y, mat.sss_color.z],
        );
        set_v(
            d,
            Self::MO_GLASS_TINT,
            [mat.glass_tint.x, mat.glass_tint.y, mat.glass_tint.z],
        );
        // Height / Normal / Displacement are map-driven in Hybrid; scalar defaults stay 0 / Z-up.
        self.wgsl_cache = None;
    }

    /// Push Master Material **unconnected** socket defaults into `PbrMaterial`.
    /// Wired sockets are driven by the graph at bake time — do not overwrite those channels.
    pub fn push_to_pbr(&self, mat: &mut core_math::pbr_material::PbrMaterial) {
        use glam::Vec3;
        mat.shader_type = core_math::pbr_material::ShaderType::from_u32(self.shader_type);
        let Some(n) = self.output_node() else { return };
        let id = n.id;
        let d = &n.input_defaults;
        let f = |i: usize| d.get(i).map(|v| v.value_f32);
        let v3 = |i: usize| d.get(i).map(|v| v.value_vec3);
        if !self.is_input_connected(id, Self::MO_ALBEDO) {
            if let Some(v) = v3(Self::MO_ALBEDO) {
                mat.albedo = Vec3::new(v[0], v[1], v[2]);
            }
        }
        if !self.is_input_connected(id, Self::MO_ROUGHNESS) {
            if let Some(v) = f(Self::MO_ROUGHNESS) {
                mat.roughness = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_METALLIC) {
            if let Some(v) = f(Self::MO_METALLIC) {
                mat.metallic = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_OPACITY) {
            if let Some(v) = f(Self::MO_OPACITY) {
                mat.opacity = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_EMISSION) {
            if let Some(v) = v3(Self::MO_EMISSION) {
                mat.emission_color = Vec3::new(v[0], v[1], v[2]);
            }
        }
        if !self.is_input_connected(id, Self::MO_CLEARCOAT) {
            if let Some(v) = f(Self::MO_CLEARCOAT) {
                mat.clearcoat = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_CLEARCOAT_ROUGH) {
            if let Some(v) = f(Self::MO_CLEARCOAT_ROUGH) {
                mat.clearcoat_roughness = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_IOR) {
            if let Some(v) = f(Self::MO_IOR) {
                mat.ior = v.clamp(1.0, 3.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_SHEEN) {
            if let Some(v) = f(Self::MO_SHEEN) {
                mat.sheen = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_EMISSION_INTENSITY) {
            if let Some(v) = f(Self::MO_EMISSION_INTENSITY) {
                mat.emission_intensity = v.max(0.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_NORMAL_STRENGTH) {
            if let Some(v) = f(Self::MO_NORMAL_STRENGTH) {
                mat.normal_strength = v.max(0.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_BUMP_STRENGTH) {
            mat.bump_map.strength = f(Self::MO_BUMP_STRENGTH)
                .unwrap_or(0.05).max(0.0);
        }
        if !self.is_input_connected(id, Self::MO_SHEEN_TINT) {
            if let Some(v) = f(Self::MO_SHEEN_TINT) {
                mat.sheen_tint = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_ANISOTROPY) {
            if let Some(v) = f(Self::MO_ANISOTROPY) {
                mat.anisotropy = v.clamp(-1.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_ANISOTROPY_ROT) {
            if let Some(v) = f(Self::MO_ANISOTROPY_ROT) {
                mat.anisotropy_rotation = v.clamp(0.0, 1.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_SSS_RADIUS) {
            if let Some(v) = f(Self::MO_SSS_RADIUS) {
                mat.sss_radius = v.max(0.0);
            }
        }
        if !self.is_input_connected(id, Self::MO_SSS_COLOR) {
            if let Some(v) = v3(Self::MO_SSS_COLOR) {
                mat.sss_color = Vec3::new(v[0], v[1], v[2]);
            }
        }
        if !self.is_input_connected(id, Self::MO_GLASS_TINT) {
            if let Some(v) = v3(Self::MO_GLASS_TINT) {
                mat.glass_tint = Vec3::new(v[0], v[1], v[2]);
            }
        }
    }

    /// Set of node ids reachable UPSTREAM from `start` (inclusive) by walking
    /// input connections. Used by [`prune_master_input`](Self::prune_master_input).
    fn upstream_closure(&self, start: NodeId) -> std::collections::HashSet<NodeId> {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            for c in &self.connections {
                if c.to_node == id {
                    stack.push(c.from_node);
                }
            }
        }
        seen
    }

    /// Material-panel P2 "Revert to panel control" (spec C1): disconnect a Master
    /// Material input and delete the nodes that were reachable ONLY through that
    /// wire. Nodes still feeding other Master sockets — and parked, unconnected
    /// nodes — are untouched. No-op when the socket is unwired.
    pub fn prune_master_input(&mut self, socket: usize) {
        let Some(out_id) = self.output_node().map(|n| n.id) else { return };
        let src = self
            .connections
            .iter()
            .find(|c| c.to_node == out_id && c.to_input == socket)
            .map(|c| c.from_node);
        let Some(src) = src else { return };
        self.disconnect(out_id, socket);
        // Keep = everything the output still reaches after the disconnect.
        let keep = self.upstream_closure(out_id);
        // Candidates = the orphaned wire's source and its upstream chain.
        let orphans: Vec<NodeId> = self
            .upstream_closure(src)
            .into_iter()
            .filter(|id| !keep.contains(id))
            .collect();
        for id in orphans {
            self.remove_node(id);
        }
    }

    /// True if Master Material input is driven by a wire (Properties UI should not fight it).
    pub fn master_socket_driven(&self, input_idx: usize) -> bool {
        self.output_node()
            .map(|n| self.is_input_connected(n.id, input_idx))
            .unwrap_or(false)
    }

    /// Direct Color → Master input is spatially uniform and belongs in the material scalar,
    /// not in a generated texture. Keep this narrow: non-trivial constant subgraphs still use
    /// the canonical evaluator until invariant folding is promoted to the material path.
    pub(crate) fn master_direct_constant_color(&self, input_idx: usize) -> Option<[f32; 3]> {
        let output = self.output_node()?;
        let (source_id, source_output) = self.connection_source(output.id, input_idx)?;
        let source = self.find_node(source_id)?;
        (source_output == 0 && matches!(source.node_type, NodeType::ConstantColor))
            .then_some(source.ramp_color0)
    }

    /// If the graph is simple enough for GPU live noise, return params.
    /// Complex graphs (math trees, multi-maps, normals…) return `None` → CPU bake path.
    pub fn extract_live_gpu_noise(&self) -> Option<LiveGpuNoise> {
        // Muted nodes bypass in the CPU evaluator; the live-GPU fast path has no mute concept,
        // so any muted node means "take the CPU path" (correct, merely slower) rather than
        // rendering a graph that ignores the mute (both-paths rule).
        if self.nodes.iter().any(|n| n.muted) {
            return None;
        }
        let out = self.output_node()?;
        // Anything beyond albedo/roughness needs full bake.
        let heavy = [
            Self::MO_METALLIC,
            Self::MO_EMISSION,
            Self::MO_EMISSION_INTENSITY,
            Self::MO_AO,
            Self::MO_HEIGHT,
            Self::MO_NORMAL,
            Self::MO_DISPLACEMENT,
        ];
        for i in heavy {
            if self.is_input_connected(out.id, i) {
                return None;
            }
        }
        // SDF mask / curvature need CPU bake with body field — not GPU noise live path.
        if self.nodes.iter().any(|n| {
            matches!(
                n.node_type,
                NodeType::PrimSdfMask | NodeType::SdfCurvature
            )
        }) {
            return None;
        }
        let albedo_on = self.is_input_connected(out.id, Self::MO_ALBEDO);
        let rough_on = self.is_input_connected(out.id, Self::MO_ROUGHNESS);
        if !albedo_on && !rough_on {
            return None;
        }

        let mut noise_id: Option<NodeId> = None;
        let mut color0 = [0.0f32, 0.0, 0.0];
        let mut color1 = [1.0f32, 1.0, 1.0];

        if albedo_on {
            let (from, from_out) = self.connection_source(out.id, Self::MO_ALBEDO)?;
            let src = self.find_node(from)?;
            match src.node_type {
                NodeType::Noise if from_out == 0 => {
                    noise_id = Some(from);
                    // Grayscale noise: black → white
                    color0 = [0.0, 0.0, 0.0];
                    color1 = [1.0, 1.0, 1.0];
                }
                NodeType::ColorRamp if from_out == 0 => {
                    // ColorRamp fac must come from Noise
                    let (nfrom, nout) = self.connection_source(from, 0)?;
                    let nnode = self.find_node(nfrom)?;
                    if !matches!(nnode.node_type, NodeType::Noise) || nout != 0 {
                        return None;
                    }
                    noise_id = Some(nfrom);
                    color0 = src.ramp_color0;
                    color1 = src.ramp_color1;
                }
                _ => return None,
            }
        }

        let mut roughness0 = 0.65f32;
        let mut roughness1 = 0.65f32;
        if rough_on {
            let (from, from_out) = self.connection_source(out.id, Self::MO_ROUGHNESS)?;
            let src = self.find_node(from)?;
            if !matches!(src.node_type, NodeType::Noise) || from_out != 0 {
                return None;
            }
            match noise_id {
                Some(id) if id == from => {}
                None => noise_id = Some(from),
                Some(_) => return None, // two different noises — bake
            }
            roughness0 = 0.0;
            roughness1 = 1.0;
        }

        let nid = noise_id?;
        let noise = self.find_node(nid)?;
        if !matches!(noise.node_type, NodeType::Noise) {
            return None;
        }
        // Only FBM/Value/Perlin-like for GPU live (simple fbm3). Exotic types → bake.
        match noise.noise_type {
            super::ops::NoiseType::Fbm
            | super::ops::NoiseType::Value
            | super::ops::NoiseType::Perlin
            | super::ops::NoiseType::Simplex
            | super::ops::NoiseType::Ridged
            | super::ops::NoiseType::Billow
            | super::ops::NoiseType::Musgrave => {}
            _ => return None,
        }

        let scale = {
            let s = Node::default_f32(noise, 1, 4.0);
            if s.abs() < 1e-6 {
                1e-6
            } else {
                s
            }
        };
        // Free-form octaves; GPU fbm loop safety cap only.
        let octaves = Node::default_f32(noise, 2, 4.0)
            .round()
            .max(1.0)
            .min(64.0) as u32;
        let seed = Node::default_f32(noise, 6, 0.0);

        if !albedo_on {
            // Roughness-only: keep base albedo from Master defaults
            if let Some(v) = out.input_defaults.get(Self::MO_ALBEDO) {
                color0 = v.value_vec3;
                color1 = v.value_vec3;
            }
        }
        if !rough_on {
            if let Some(v) = out.input_defaults.get(Self::MO_ROUGHNESS) {
                roughness0 = v.value_f32;
                roughness1 = v.value_f32;
            }
        }

        Some(LiveGpuNoise {
            scale,
            octaves,
            seed,
            color0,
            color1,
            roughness0,
            roughness1,
            drives_albedo: albedo_on,
            drives_roughness: rough_on,
            curve: noise.noise_curve,
            curve_k: noise.noise_curve_k,
            black: noise.noise_black,
            white: noise.noise_white,
        })
    }

    /// Source node/output for an input socket, if wired.
    pub(crate) fn connection_source(&self, to_node: NodeId, to_input: usize) -> Option<(NodeId, usize)> {
        self.connections
            .iter()
            .find(|c| c.to_node == to_node && c.to_input == to_input)
            .map(|c| (c.from_node, c.from_output))
    }

    // ================================================================
    // WGSL Code Generation
    // ================================================================

    /// Generate WGSL function body that evaluates this node graph.
    /// Returns a function `eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult`
    pub fn generate_wgsl(&mut self) -> String {
        if let Some(ref cached) = self.wgsl_cache {
            return cached.replace("/*graph_param*/", "");
        }
        let code = self.compile_wgsl();
        self.wgsl_cache = Some(code.clone());
        code.replace("/*graph_param*/", "")
    }

    pub(crate) fn compile_wgsl(&self) -> String {
        let output = match self.output_node() {
            Some(n) => n.id,
            None => return Self::fallback_wgsl(),
        };

        // Topological sort
        let order = match self.topo_sort(output) {
            Some(o) => o,
            None => return Self::fallback_wgsl(),
        };

        let mut lines = Vec::new();
        lines.push(crate::shader_nodes::nodes::curvature::CONTEXT_STUB.into());
        // UV rotate (degrees) + offset — per projection side.
        lines.push("fn sn_uv_xf(uv: vec2<f32>, rot_deg: f32, off: vec2<f32>) -> vec2<f32> {".into());
        lines.push("    let r = rot_deg * 0.01745329252; let c = cos(r); let s = sin(r);".into());
        lines.push("    return vec2(uv.x * c - uv.y * s, uv.x * s + uv.y * c) + off;".into());
        lines.push("}".into());
        // Shared analytic height field for surface analyzers (bake + live) — no scene SDF required.
        lines.push("fn sn_field_h(p: vec3<f32>) -> f32 {".into());
        lines.push("    let x = p.x; let y = p.y; let z = p.z;".into());
        lines.push("    return sin(x * 2.1) * 0.5 + cos(y * 1.7) * 0.35 + sin((x + y) * 3.3) * 0.15".into());
        lines.push("         + sin(z * 2.4 + x * 0.5) * 0.1;".into());
        lines.push("}".into());
        // True gradient Perlin (CPU bake parity) — not scalar-hash value noise.
        lines.push("fn sn_fade(t: f32) -> f32 { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0); }".into());
        lines.push("fn sn_grad3(h: f32) -> vec3<f32> {".into());
        lines.push("    let i = u32(floor(h * 12.0)) % 12u;".into());
        lines.push("    if (i == 0u) { return vec3(1.0, 1.0, 0.0); }".into());
        lines.push("    if (i == 1u) { return vec3(-1.0, 1.0, 0.0); }".into());
        lines.push("    if (i == 2u) { return vec3(1.0, -1.0, 0.0); }".into());
        lines.push("    if (i == 3u) { return vec3(-1.0, -1.0, 0.0); }".into());
        lines.push("    if (i == 4u) { return vec3(1.0, 0.0, 1.0); }".into());
        lines.push("    if (i == 5u) { return vec3(-1.0, 0.0, 1.0); }".into());
        lines.push("    if (i == 6u) { return vec3(1.0, 0.0, -1.0); }".into());
        lines.push("    if (i == 7u) { return vec3(-1.0, 0.0, -1.0); }".into());
        lines.push("    if (i == 8u) { return vec3(0.0, 1.0, 1.0); }".into());
        lines.push("    if (i == 9u) { return vec3(0.0, -1.0, 1.0); }".into());
        lines.push("    if (i == 10u) { return vec3(0.0, 1.0, -1.0); }".into());
        lines.push("    return vec3(0.0, -1.0, -1.0);".into());
        lines.push("}".into());
        lines.push("fn sn_wrap(i: f32, period: f32) -> f32 {".into());
        lines.push("    if (period <= 0.5) { return i; }".into());
        lines.push("    return i - floor(i / period) * period;".into()); // rem_euclid for ≥0 period
        lines.push("}".into());
        // period <= 0 → non-tileable. period >= 1 → seamless lattice wrap.
        lines.push("fn sn_perlin3(p: vec3<f32>, period: f32) -> f32 {".into());
        lines.push("    let i0 = floor(p); let f0 = fract(p);".into());
        lines.push("    let u = vec3(sn_fade(f0.x), sn_fade(f0.y), sn_fade(f0.z));".into());
        lines.push("    var ix0 = i0.x; var iy0 = i0.y; var iz0 = i0.z;".into());
        lines.push("    var ix1 = i0.x + 1.0; var iy1 = i0.y + 1.0; var iz1 = i0.z + 1.0;".into());
        lines.push("    if (period >= 1.0) {".into());
        lines.push("        ix0 = sn_wrap(ix0, period); iy0 = sn_wrap(iy0, period); iz0 = sn_wrap(iz0, period);".into());
        lines.push("        ix1 = sn_wrap(ix1, period); iy1 = sn_wrap(iy1, period); iz1 = sn_wrap(iz1, period);".into());
        lines.push("    }".into());
        lines.push("    let g000 = sn_grad3(hash31(vec3(ix0, iy0, iz0)));".into());
        lines.push("    let g100 = sn_grad3(hash31(vec3(ix1, iy0, iz0)));".into());
        lines.push("    let g010 = sn_grad3(hash31(vec3(ix0, iy1, iz0)));".into());
        lines.push("    let g110 = sn_grad3(hash31(vec3(ix1, iy1, iz0)));".into());
        lines.push("    let g001 = sn_grad3(hash31(vec3(ix0, iy0, iz1)));".into());
        lines.push("    let g101 = sn_grad3(hash31(vec3(ix1, iy0, iz1)));".into());
        lines.push("    let g011 = sn_grad3(hash31(vec3(ix0, iy1, iz1)));".into());
        lines.push("    let g111 = sn_grad3(hash31(vec3(ix1, iy1, iz1)));".into());
        lines.push("    let n000 = dot(g000, f0);".into());
        lines.push("    let n100 = dot(g100, f0 - vec3(1.0, 0.0, 0.0));".into());
        lines.push("    let n010 = dot(g010, f0 - vec3(0.0, 1.0, 0.0));".into());
        lines.push("    let n110 = dot(g110, f0 - vec3(1.0, 1.0, 0.0));".into());
        lines.push("    let n001 = dot(g001, f0 - vec3(0.0, 0.0, 1.0));".into());
        lines.push("    let n101 = dot(g101, f0 - vec3(1.0, 0.0, 1.0));".into());
        lines.push("    let n011 = dot(g011, f0 - vec3(0.0, 1.0, 1.0));".into());
        lines.push("    let n111 = dot(g111, f0 - vec3(1.0, 1.0, 1.0));".into());
        lines.push("    let x00 = mix(n000, n100, u.x); let x10 = mix(n010, n110, u.x);".into());
        lines.push("    let x01 = mix(n001, n101, u.x); let x11 = mix(n011, n111, u.x);".into());
        lines.push("    let y0 = mix(x00, x10, u.y); let y1 = mix(x01, x11, u.y);".into());
        lines.push("    return saturate(mix(y0, y1, u.z) * 0.5 + 0.5);".into());
        lines.push("}".into());
        lines.push("fn sn_fbm_perlin(p: vec3<f32>, oct: u32, lac: f32, gain: f32, period: f32) -> f32 {".into());
        lines.push("    var v = 0.0; var a = 0.5; var amp = 0.0; var pp = p; var freq = 1.0;".into());
        lines.push("    let g = clamp(gain, 0.15, 0.95); let L = max(lac, 1.01);".into());
        lines.push("    for (var oi = 0u; oi < min(oct, 6u); oi++) {".into());
        lines.push("        var per = period;".into());
        lines.push("        if (period >= 1.0) { per = max(round(period * freq), 1.0); }".into());
        lines.push("        let n = sn_perlin3(pp, per) * 2.0 - 1.0;".into());
        lines.push("        v += a * n; amp += a; pp *= L; freq *= L; a *= g;".into());
        lines.push("    }".into());
        lines.push("    return saturate(v / max(amp, 1e-4) * 0.5 + 0.5);".into());
        lines.push("}".into());
        for &node_id in &order {
            if matches!(self.find_node(node_id).unwrap().node_type, NodeType::HeightToNormal) {
                self.emit_input_sampler(node_id, 0, false, &mut lines);
            }
        }
        self.emit_input_sampler(output, Self::MO_HEIGHT, false, &mut lines);
        self.emit_input_sampler(output, Self::MO_NORMAL, true, &mut lines);
        if self.spatial_bump_strength() {
            self.emit_input_sampler(output, Self::MO_BUMP_STRENGTH, false, &mut lines);
            lines.push(include_str!("../../../graphics/src/material_precision.wgsl").into());
        }
        lines.push(format!("fn eval_node_height(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> f32 {{ return sn_input_{output}_{}(hit_prim, p, n); }}", Self::MO_HEIGHT));
        if self.spatial_bump_strength() {
            let height = Self::MO_HEIGHT;
            let normal = Self::MO_NORMAL;
            let strength = Self::MO_BUMP_STRENGTH;
            let epsilon = 2.0 / self.bake_res.max(16) as f32;
            lines.push(format!("fn eval_node_normal(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {{
                let e = /*graph_param*/{epsilon:.8};
                let base = normalize(sn_input_{output}_{normal}(hit_prim, p, n));
                let strength = clamp(sn_input_{output}_{strength}(hit_prim, p, n), 0.0, 4.0);
                let dx = (sn_input_{output}_{height}(hit_prim, p + vec3(e, 0.0, 0.0), n) - sn_input_{output}_{height}(hit_prim, p - vec3(e, 0.0, 0.0), n)) / (2.0 * e);
                let dy = (sn_input_{output}_{height}(hit_prim, p + vec3(0.0, e, 0.0), n) - sn_input_{output}_{height}(hit_prim, p - vec3(0.0, e, 0.0), n)) / (2.0 * e);
                return material_bump_normal(base, vec3(1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0), dx, dy, strength);
            }}"));
        } else {
            lines.push(format!("fn eval_node_normal(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {{ return sn_input_{output}_{}(hit_prim, p, n); }}", Self::MO_NORMAL));
        }
        lines.push("fn eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {".into());
        lines.push("    var result: MatResult;".into());

        // Generate variable for each node in topological order
        for &nid in &order {
            let node = self.find_node(nid).unwrap();
            self.gen_node_wgsl(node, &mut lines);
        }

        // Read from MaterialOutput's inputs (indices = MO_* constants).
        // Float↔Vec3/Color: both directions coerce at the use site (LOG_TRIAGE BUG 1 + BUG 3).
        // Float→Vec3: vec3(s). Vec3/Color→Float: Rec.709 luminance (SocketType::compatible promise).
        let albedo_var = self.input_var_name_vec3_coerced(output, Self::MO_ALBEDO, "vec3(0.66, 0.585, 0.53)");
        let rough_var = self.input_var_name(output, Self::MO_ROUGHNESS, "0.65");
        let metal_var = self.input_var_name(output, Self::MO_METALLIC, "0.0");
        let opacity_var = self.input_var_name(output, Self::MO_OPACITY, "1.0");
        let emission_var = self.input_var_name_vec3_coerced(output, Self::MO_EMISSION, "vec3(0.0, 0.0, 0.0)");
        let em_int_var = self.input_var_name(output, Self::MO_EMISSION_INTENSITY, "0.0");
        // Advanced sockets (Height/Normal/Clearcoat/…) push to PbrMaterial via push_to_pbr;
        // MatResult is still the 5-field lit surface until lighting consumes extra fields.

        lines.push(format!("    result.color = {};", albedo_var));
        lines.push(format!("    result.roughness = {};", rough_var));
        lines.push(format!("    result.metallic = {};", metal_var));
        lines.push(format!("    result.opacity = {};", opacity_var));
        // Match Properties: emission_color * emission_intensity
        lines.push(format!(
            "    result.emission = ({emission_var}) * ({em_int_var});"
        ));
        lines.push("    return result;".into());
        lines.push("}".into());

        lines.join("\n")
    }

    fn emit_input_sampler(&self, node_id: NodeId, input: usize, vector: bool, lines: &mut Vec<String>) {
        let result_type = if vector { "vec3<f32>" } else { "f32" };
        lines.push(format!("fn sn_input_{node_id}_{input}(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> {result_type} {{"));
        if let Some((source, _)) = self.connection_source(node_id, input) {
            for source_id in self.topo_sort(source).expect("validated acyclic shader graph") {
                self.gen_node_wgsl(self.find_node(source_id).unwrap(), lines);
            }
        }
        let result = if vector {
            let default = Node::default_vec3(self.find_node(node_id).unwrap(), input, [0.0, 0.0, 1.0]);
            self.input_var_name_vec3(node_id, input, &default)
        } else {
            let default = Node::default_f32(self.find_node(node_id).unwrap(), input, 0.0);
            self.input_var_name(node_id, input, &format!("/*graph_param*/{default:.4}"))
        };
        lines.push(format!("    return {result};"));
        lines.push("}".into());
    }

    /// Multiply a float result by the trailing **Mask** socket (default 1). Writes `out_var`.
    fn emit_mask_mul_f32(
        &self,
        id: NodeId,
        node: &Node,
        in_var: &str,
        out_var: &str,
        lines: &mut Vec<String>,
    ) {
        if let Some(mi) = Node::mask_input_index(&node.node_type) {
            let m = self.input_var_name(id, mi, &format!("/*graph_param*/{:.4}", Node::default_f32(node, mi, 1.0)));
            lines.push(format!("    let {out_var} = ({in_var}) * ({m});"));
        } else {
            lines.push(format!("    let {out_var} = {in_var};"));
        }
    }

    /// Multiply a vec3/color result by Mask (broadcast).
    fn emit_mask_mul_vec3(
        &self,
        id: NodeId,
        node: &Node,
        in_var: &str,
        out_var: &str,
        lines: &mut Vec<String>,
    ) {
        if let Some(mi) = Node::mask_input_index(&node.node_type) {
            let m = self.input_var_name(id, mi, &format!("/*graph_param*/{:.4}", Node::default_f32(node, mi, 1.0)));
            lines.push(format!("    let {out_var} = ({in_var}) * ({m});"));
        } else {
            lines.push(format!("    let {out_var} = {in_var};"));
        }
    }

    /// Emit sample-domain UV/pos for built-in projection.
    /// Writes `n{id}_dom` (vec3) for single-plane modes, or triplanar weights + 3 UVs.
    /// Applies per-side **position (UV offset)** and **rotation (degrees)** from `node`.
    /// Returns true if triplanar (caller must sample 3× and blend).
    fn emit_builtin_projection(
        &self,
        id: NodeId,
        node: &Node,
        pos_expr: &str,
        lines: &mut Vec<String>,
    ) -> bool {
        let style = node.projection;
        let ox = node.proj_off_x;
        let oy = node.proj_off_y;
        let oz = node.proj_off_z;
        let rx = node.proj_rot_x;
        let ry = node.proj_rot_y;
        let rz = node.proj_rot_z;
        // Single-plane modes use the Z-side transform as the active plane controls.
        let xf = |u: &str, v: &str, rot: f32, off: [f32; 2]| -> String {
            format!(
                "sn_uv_xf(vec2({u}, {v}), /*graph_param*/{rot:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4}))",
                off[0], off[1]
            )
        };
        match style {
            ProjectionStyle::None => {
                lines.push(format!("    let n{id}_dom = {pos_expr};"));
                false
            }
            ProjectionStyle::PlanarXY => {
                let t = xf(&format!("({pos_expr}).x"), &format!("({pos_expr}).y"), rz, oz);
                lines.push(format!("    let n{id}_dom = vec3(({t}).x, ({t}).y, 0.0);"));
                false
            }
            ProjectionStyle::PlanarXZ => {
                let t = xf(&format!("({pos_expr}).x"), &format!("({pos_expr}).z"), rz, oz);
                lines.push(format!("    let n{id}_dom = vec3(({t}).x, ({t}).y, 0.0);"));
                false
            }
            ProjectionStyle::PlanarYZ => {
                let t = xf(&format!("({pos_expr}).y"), &format!("({pos_expr}).z"), rz, oz);
                lines.push(format!("    let n{id}_dom = vec3(({t}).x, ({t}).y, 0.0);"));
                false
            }
            ProjectionStyle::Box => {
                // Dominant-axis pick, then apply that side's offset/rotation.
                lines.push(format!("    var n{id}_dom: vec3<f32>;"));
                lines.push(format!("    {{ let ap = abs({pos_expr}); let pe = {pos_expr};"));
                lines.push(format!(
                    "    if (ap.x >= ap.y && ap.x >= ap.z) {{ let t = sn_uv_xf(vec2(pe.y, pe.z), /*graph_param*/{rx:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})); n{id}_dom = vec3(t.x, t.y, 0.0); }}",
                    ox[0], ox[1]
                ));
                lines.push(format!(
                    "    else if (ap.y >= ap.z) {{ let t = sn_uv_xf(vec2(pe.x, pe.z), /*graph_param*/{ry:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})); n{id}_dom = vec3(t.x, t.y, 0.0); }}",
                    oy[0], oy[1]
                ));
                lines.push(format!(
                    "    else {{ let t = sn_uv_xf(vec2(pe.x, pe.y), /*graph_param*/{rz:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})); n{id}_dom = vec3(t.x, t.y, 0.0); }} }}",
                    oz[0], oz[1]
                ));
                false
            }
            ProjectionStyle::Cylindrical => {
                lines.push(format!("    var n{id}_dom: vec3<f32>;"));
                lines.push(format!("    {{ let a = atan2(({pos_expr}).z, ({pos_expr}).x); let h = ({pos_expr}).y;"));
                lines.push(format!(
                    "    let t = sn_uv_xf(vec2(a * 0.15915494, h), /*graph_param*/{rz:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})); n{id}_dom = vec3(t.x, t.y, 0.0); }}",
                    oz[0], oz[1]
                ));
                false
            }
            ProjectionStyle::Spherical => {
                lines.push(format!("    var n{id}_dom: vec3<f32>;"));
                lines.push(format!("    {{ let d = normalize({pos_expr});"));
                lines.push(format!(
                    "    let t = sn_uv_xf(vec2(atan2(d.z, d.x) * 0.15915494, acos(clamp(d.y, -1.0, 1.0)) * 0.31830988), /*graph_param*/{rz:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})); n{id}_dom = vec3(t.x, t.y, 0.0); }}",
                    oz[0], oz[1]
                ));
                false
            }
            ProjectionStyle::Triplanar => {
                // Blend weights from shading normal; 3 planar UVs with per-side pos/rot.
                lines.push(format!("    let n{id}_tw = pow(abs(n), vec3(4.0));"));
                lines.push(format!("    let n{id}_twN = n{id}_tw / max(dot(n{id}_tw, vec3(1.0)), 1e-4);"));
                lines.push(format!(
                    "    let n{id}_ux = vec3(sn_uv_xf(vec2(({pos_expr}).y, ({pos_expr}).z), /*graph_param*/{rx:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})), 0.0);",
                    ox[0], ox[1]
                ));
                lines.push(format!(
                    "    let n{id}_uy = vec3(sn_uv_xf(vec2(({pos_expr}).x, ({pos_expr}).z), /*graph_param*/{ry:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})), 0.0);",
                    oy[0], oy[1]
                ));
                lines.push(format!(
                    "    let n{id}_uz = vec3(sn_uv_xf(vec2(({pos_expr}).x, ({pos_expr}).y), /*graph_param*/{rz:.4}, vec2(/*graph_param*/{:.4}, /*graph_param*/{:.4})), 0.0);",
                    oz[0], oz[1]
                ));
                true
            }
        }
    }

    /// Emit Advanced blend curve reshape: `raw_expr` → `out_var` (float).
    fn emit_noise_blend_curve(
        &self,
        id: NodeId,
        node: &Node,
        raw_expr: &str,
        out_var: &str,
        lines: &mut Vec<String>,
    ) {
        let b = node.noise_black;
        let w = node.noise_white;
        let k = node.noise_curve_k.max(0.001);
        match node.noise_curve {
            NoiseBlendCurve::Linear => {
                // Still apply black/white levels remap when non-default.
                lines.push(format!(
                    "    let {out_var} = select({raw_expr}, saturate((({raw_expr}) - /*graph_param*/{b:.6}) / max(/*graph_param*/{w:.6} - /*graph_param*/{b:.6}, 1e-6)), abs(/*graph_param*/{b:.6}) > 1e-5 || abs(/*graph_param*/{w:.6} - 1.0) > 1e-5);"
                ));
            }
            NoiseBlendCurve::Smooth => {
                lines.push(format!(
                    "    let n{id}_x = saturate((({raw_expr}) - /*graph_param*/{b:.6}) / max(/*graph_param*/{w:.6} - /*graph_param*/{b:.6}, 1e-6));"
                ));
                lines.push(format!(
                    "    let {out_var} = n{id}_x * n{id}_x * (3.0 - 2.0 * n{id}_x);"
                ));
            }
            NoiseBlendCurve::Smoother => {
                lines.push(format!(
                    "    let n{id}_x = saturate((({raw_expr}) - {b:.6}) / max({w:.6} - {b:.6}, 1e-6));"
                ));
                lines.push(format!(
                    "    let {out_var} = n{id}_x * n{id}_x * n{id}_x * (n{id}_x * (n{id}_x * 6.0 - 15.0) + 10.0);"
                ));
            }
            NoiseBlendCurve::Power => {
                lines.push(format!(
                    "    let n{id}_x = saturate((({raw_expr}) - {b:.6}) / max({w:.6} - {b:.6}, 1e-6));"
                ));
                lines.push(format!("    let {out_var} = pow(n{id}_x, /*graph_param*/{k:.6});"));
            }
            NoiseBlendCurve::Levels => {
                lines.push(format!(
                    "    let n{id}_x = saturate((({raw_expr}) - {b:.6}) / max({w:.6} - {b:.6}, 1e-6));"
                ));
                lines.push(format!("    let {out_var} = pow(n{id}_x, 1.0 / /*graph_param*/{k:.6});"));
            }
        }
    }

    /// Emit one scalar noise sample into `n{id}_val{suffix}` at domain expression `dom` (vec3).
    ///
    /// `declare`: when true, introduce `var`/`let` for the result (single-plane path).
    /// When false, **assign** into a pre-declared outer `var` (G5 triplanar weight-guard:
    /// `var n_valx = 0.0; if (twN.x > 0.01) { … n_valx = …; }`).
    fn emit_noise_sample_wgsl(
        &self,
        id: NodeId,
        suffix: &str,
        dom: &str,
        node: &Node,
        scale: &str,
        oct: &str,
        lac: &str,
        gain: &str,
        seed: &str,
        dist: &str,
        lines: &mut Vec<String>,
    ) {
        self.emit_noise_sample_wgsl_ex(
            id, suffix, dom, node, scale, oct, lac, gain, seed, dist, lines, true,
        );
    }

    fn emit_noise_sample_wgsl_ex(
        &self,
        id: NodeId,
        suffix: &str,
        dom: &str,
        node: &Node,
        scale: &str,
        oct: &str,
        lac: &str,
        gain: &str,
        seed: &str,
        dist: &str,
        lines: &mut Vec<String>,
        declare: bool,
    ) {
        let v = format!("n{id}_val{suffix}");
        lines.push(format!(
            "    let n{id}_sp{suffix} = {dom} + {seed} * vec3(1.0, 0.6180339887, 1.7320508075);"
        ));
        // Tile: Scale = cell count; map domain [-1,1]→[0,period) so map edges match under Repeat.
        let tile = node.noise_tile;
        let period_expr = if tile {
            format!("max(round(max({scale}, 0.05)), 1.0)")
        } else {
            "0.0".into() // non-tileable
        };
        let p_expr = if tile {
            format!(
                "((n{id}_sp{suffix} * 0.5 + 0.5) - floor(n{id}_sp{suffix} * 0.5 + 0.5)) * {period_expr}"
            )
        } else {
            format!("n{id}_sp{suffix} * max({scale}, 0.05)")
        };
        lines.push(format!("    let n{id}_np{suffix} = {p_expr};"));
        lines.push(format!("    let n{id}_per{suffix} = {period_expr};"));
        let pp = format!("n{id}_np{suffix}");
        let per = format!("n{id}_per{suffix}");
        match node.noise_type {
            NoiseType::Value => {
                // Periodic value noise when Tile is on (period in lattice cells).
                if declare {
                    lines.push(format!("    var {v}: f32 = 0.0;"));
                } else {
                    lines.push(format!("    {v} = 0.0;"));
                }
                lines.push(format!("    {{ let bp = {pp}; let i0 = floor(bp); let f0 = fract(bp);"));
                lines.push(format!("    let uu = vec3(sn_fade(f0.x), sn_fade(f0.y), sn_fade(f0.z));"));
                lines.push(format!("    for (var dz = 0; dz <= 1; dz++) {{ for (var dy = 0; dy <= 1; dy++) {{ for (var dx = 0; dx <= 1; dx++) {{"));
                lines.push(format!("        var c = i0 + vec3(f32(dx), f32(dy), f32(dz));"));
                lines.push(format!("        if ({per} >= 1.0) {{ c = vec3(sn_wrap(c.x, {per}), sn_wrap(c.y, {per}), sn_wrap(c.z, {per})); }}"));
                lines.push(format!("        let h = hash31(c);"));
                lines.push(format!("        let wx = select(uu.x, 1.0 - uu.x, dx == 0);"));
                lines.push(format!("        let wy = select(uu.y, 1.0 - uu.y, dy == 0);"));
                lines.push(format!("        let wz = select(uu.z, 1.0 - uu.z, dz == 0);"));
                lines.push(format!("        {v} += h * wx * wy * wz;"));
                lines.push(format!("    }} }} }} }}"));
            }
            NoiseType::Fbm => {
                if declare {
                    lines.push(format!(
                        "    let {v} = sn_fbm_perlin({pp}, u32(clamp({oct}, 1.0, 64.0)), {lac}, {gain}, {per});"
                    ));
                } else {
                    lines.push(format!(
                        "    {v} = sn_fbm_perlin({pp}, u32(clamp({oct}, 1.0, 64.0)), {lac}, {gain}, {per});"
                    ));
                }
            }
            NoiseType::White => {
                if declare {
                    lines.push(format!("    var {v}: f32;"));
                }
                lines.push(format!("    {{ var c = floor({pp});"));
                lines.push(format!("    if ({per} >= 1.0) {{ c = vec3(sn_wrap(c.x, {per}), sn_wrap(c.y, {per}), sn_wrap(c.z, {per})); }}"));
                lines.push(format!("    {v} = hash31(c); }}"));
            }
            NoiseType::Cellular => {
                // Tileable Worley: wrap neighbor cell hashes so period edges match.
                if declare {
                    lines.push(format!("    var {v}: f32 = 8.0;"));
                } else {
                    lines.push(format!("    {v} = 8.0;"));
                }
                lines.push(format!("    {{ let bp = {pp}; let ip = floor(bp); let fp = fract(bp);"));
                lines.push(format!("    for (var dz = -1; dz <= 1; dz++) {{ for (var dy = -1; dy <= 1; dy++) {{ for (var dx = -1; dx <= 1; dx++) {{"));
                lines.push(format!("        let g = vec3(f32(dx), f32(dy), f32(dz));"));
                lines.push(format!("        var cell = ip + g;"));
                lines.push(format!("        if ({per} >= 1.0) {{ cell = vec3(sn_wrap(cell.x, {per}), sn_wrap(cell.y, {per}), sn_wrap(cell.z, {per})); }}"));
                lines.push(format!("        let o = vec3(hash31(cell), hash31(cell + vec3(17.0, 0.0, 0.0)), hash31(cell + vec3(0.0, 19.0, 0.0)));"));
                lines.push(format!("        let d = length(g + o - fp); {v} = min({v}, d);"));
                lines.push(format!("    }} }} }} {v} = saturate({v}); }}"));
            }
            NoiseType::Ridged => {
                if declare {
                    lines.push(format!("    var {v}: f32 = 0.0;"));
                } else {
                    lines.push(format!("    {v} = 0.0;"));
                }
                lines.push(format!("    {{ var pp = {pp}; var a = 0.5; var amp = 0.0; var freq = 1.0;"));
                lines.push(format!("    let g = clamp({gain}, 0.15, 0.95); let L = max({lac}, 1.01);"));
                lines.push(format!("    for (var oi = 0u; oi < u32(clamp({oct}, 1.0, 64.0)); oi++) {{"));
                lines.push(format!("        var per = {per}; if ({per} >= 1.0) {{ per = max(round({per} * freq), 1.0); }}"));
                lines.push(format!("        let n = sn_perlin3(pp, per) * 2.0 - 1.0; {v} += (1.0 - abs(n)) * a; amp += a;"));
                lines.push(format!("        pp *= L; freq *= L; a *= g; }} {v} = saturate({v} / max(amp, 1e-4)); }}"));
            }
            NoiseType::Billow => {
                if declare {
                    lines.push(format!("    var {v}: f32 = 0.0;"));
                } else {
                    lines.push(format!("    {v} = 0.0;"));
                }
                lines.push(format!("    {{ var pp = {pp}; var a = 0.5; var amp = 0.0; var freq = 1.0;"));
                lines.push(format!("    let g = clamp({gain}, 0.15, 0.95); let L = max({lac}, 1.01);"));
                lines.push(format!("    for (var oi = 0u; oi < u32(clamp({oct}, 1.0, 64.0)); oi++) {{"));
                lines.push(format!("        var per = {per}; if ({per} >= 1.0) {{ per = max(round({per} * freq), 1.0); }}"));
                lines.push(format!("        let n = sn_perlin3(pp, per) * 2.0 - 1.0; {v} += abs(n) * a; amp += a;"));
                lines.push(format!("        pp *= L; freq *= L; a *= g; }} {v} = saturate({v} / max(amp, 1e-4)); }}"));
            }
            NoiseType::DomainWarp => {
                lines.push(format!("    let n{id}_w{suffix} = sn_perlin3({pp}, {per}) * 2.0 - 1.0;"));
                if declare {
                    lines.push(format!(
                        "    let {v} = sn_perlin3({pp} + {dist} * vec3(n{id}_w{suffix}), {per});"
                    ));
                } else {
                    lines.push(format!(
                        "    {v} = sn_perlin3({pp} + {dist} * vec3(n{id}_w{suffix}), {per});"
                    ));
                }
            }
            NoiseType::Perlin => {
                if declare {
                    lines.push(format!("    let {v} = sn_perlin3({pp}, {per});"));
                } else {
                    lines.push(format!("    {v} = sn_perlin3({pp}, {per});"));
                }
            }
            NoiseType::Simplex => {
                if declare {
                    lines.push(format!("    let {v} = sn_perlin3({pp}, {per}); // tileable path uses Perlin"));
                } else {
                    lines.push(format!("    {v} = sn_perlin3({pp}, {per}); // tileable path uses Perlin"));
                }
            }
            NoiseType::Musgrave => {
                if declare {
                    lines.push(format!("    var {v}: f32 = 0.0;"));
                } else {
                    lines.push(format!("    {v} = 0.0;"));
                }
                lines.push(format!("    {{ var mp = {pp}; var freq = 1.0; var amp = 1.0; var as = 0.0;"));
                lines.push(format!("    let dim = max({gain}, 0.15); let L = max({lac}, 1.01);"));
                lines.push(format!("    for (var oi = 0u; oi < u32(clamp({oct}, 1.0, 64.0)); oi++) {{"));
                lines.push(format!("        var per = {per}; if ({per} >= 1.0) {{ per = max(round({per} * freq), 1.0); }}"));
                lines.push(format!("        let n = sn_perlin3(mp * freq, per) * 2.0 - 1.0; {v} += n * amp; as += amp;"));
                lines.push(format!("        amp *= pow(max(freq, 0.001), -dim); freq *= L;"));
                lines.push(format!("    }} {v} = saturate({v} / max(as, 1e-4) * 0.5 + 0.5); }}"));
            }
        }
    }

    /// G5: emit triplanar noise samples guarded by blend weights so near-zero axes
    /// skip the multi-octave FBM (bake domain `n=[0,0,1]` → only Z runs).
    fn emit_triplanar_noise_guarded(
        &self,
        id: NodeId,
        node: &Node,
        scale: &str,
        oct: &str,
        lac: &str,
        gain: &str,
        seed: &str,
        dist: &str,
        lines: &mut Vec<String>,
    ) {
        const AXES: [(&str, &str, &str); 3] = [
            ("x", "x", "ux"),
            ("y", "y", "uy"),
            ("z", "z", "uz"),
        ];
        for (suf, axis, uv) in AXES {
            lines.push(format!("    var n{id}_val{suf}: f32 = 0.0;"));
            lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
            self.emit_noise_sample_wgsl_ex(
                id,
                suf,
                &format!("n{id}_{uv}"),
                node,
                scale,
                oct,
                lac,
                gain,
                seed,
                dist,
                lines,
                false,
            );
            lines.push("    }".into());
        }
        lines.push(format!(
            "    let n{id}_raw = n{id}_valx * n{id}_twN.x + n{id}_valy * n{id}_twN.y + n{id}_valz * n{id}_twN.z;"
        ));
    }

    fn gen_node_wgsl(&self, node: &Node, lines: &mut Vec<String>) {
        let id = node.id;
        match &node.node_type {
            // G8: Reroute is NOT in `node_type_gpu_bakeable` — the bake path declines
            // and CPU-evaluates (typed-correct passthrough there). This arm is reachable
            // only from the debug Show-WGSL path; the float coercion here would collapse
            // colours, which is exactly why the GPU path declines instead of compiling it.
            NodeType::Reroute => {
                let v = self.input_var_name(id, 0, "0.0");
                lines.push(format!("    let n{id}_val = {v};"));
            }
            NodeType::Frame => {}
            NodeType::TexCoord => {
                lines.push(format!("    let n{id}_pos = p;"));
                lines.push(format!("    let n{id}_nrm = n;"));
            }
            NodeType::ObjectInfo => {
                lines.push(format!("    let n{id}_idx = f32(hit_prim);"));
            }
            NodeType::Noise => {
                // Baked-in projection (default Triplanar). Unconnected Offset → surface p + manual XYZ.
                let pos = self.generator_pos_expr(id, 0, node);
                let scale = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 5.0)));
                // Octaves is consumed as a float inside `clamp(oct, 1.0, 64.0)` then `u32(...)`.
                // Default MUST be a float literal (`3.0000`), not a bare integer (`3`) — clamp's
                // three args must share one type. Integer-consumed sockets use `input_var_name_u32`
                // instead (G1) — do NOT generalize `{:.4}` to those sites (that recurrence unblocked
                // 7 node types; see PLAN_FOR_GROK_NODE_CORRECTNESS G1).
                let oct = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 3.0)));
                let lac = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 2.0)));
                let gain = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 0.5)));
                let seed = self.input_var_name(id, 6, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 6, 0.0)));
                let dist = self.input_var_name(id, 5, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 5, 0.0)));
                let tri = self.emit_builtin_projection(id, node, &pos, lines);
                if tri {
                    // G5: skip FBM axes whose triplanar weight is ~0 (bake n=Z → only Z runs).
                    self.emit_triplanar_noise_guarded(
                        id, node, &scale, &oct, &lac, &gain, &seed, &dist, lines,
                    );
                } else {
                    self.emit_noise_sample_wgsl(id, "r", &format!("n{id}_dom"), node, &scale, &oct, &lac, &gain, &seed, &dist, lines);
                    lines.push(format!("    let n{id}_raw = n{id}_valr;"));
                }
                // Advanced blend curve (levels / smooth / power) on raw noise.
                self.emit_noise_blend_curve(id, node, &format!("n{id}_raw"), &format!("n{id}_shaped"), lines);
                self.emit_mask_mul_f32(id, node, &format!("n{id}_shaped"), &format!("n{id}_val"), lines);
            }
            NodeType::Voronoi => {
                let pos = self.generator_pos_expr(id, 0, node);
                let scale = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 5.0)));
                let rnd = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                let tri = self.emit_builtin_projection(id, node, &pos, lines);
                // `declare`: introduce vars (single-plane) vs assign into pre-declared outer vars
                // (G5 triplanar weight-guard: `var … = 0; if (twN.axis > 0.01) { … = 8.0; … }`).
                let emit_vor = |lines: &mut Vec<String>, suffix: &str, dom: String, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_dist{suffix}: f32 = 8.0; var n{id}_cell{suffix}: f32 = 0.0; var n{id}_ccol{suffix}: vec3<f32> = vec3(0.0);"));
                    } else {
                        lines.push(format!("    n{id}_dist{suffix} = 8.0; n{id}_cell{suffix} = 0.0; n{id}_ccol{suffix} = vec3(0.0);"));
                    }
                    lines.push(format!("    {{ let bp = ({dom}) * {scale}; let ip = floor(bp); let fp = fract(bp);"));
                    lines.push(format!("    for (var dz = -1; dz <= 1; dz++) {{ for (var dy = -1; dy <= 1; dy++) {{ for (var dx = -1; dx <= 1; dx++) {{"));
                    lines.push(format!("        let g = vec3(f32(dx), f32(dy), f32(dz));"));
                    lines.push(format!("        let h = vec3(hash31(ip + g), hash31(ip + g + vec3(19.0, 0.0, 0.0)), hash31(ip + g + vec3(0.0, 23.0, 0.0)));"));
                    lines.push(format!("        let o = mix(vec3(0.5), h, {rnd}); let d = length(g + o - fp);"));
                    lines.push(format!("        if (d < n{id}_dist{suffix}) {{ n{id}_dist{suffix} = d; n{id}_cell{suffix} = hash31(ip + g); n{id}_ccol{suffix} = h; }}"));
                    lines.push(format!("    }} }} }} n{id}_dist{suffix} = saturate(n{id}_dist{suffix}); }}"));
                };
                if tri {
                    // G5: weight-guard each Voronoi axis (same threshold as Noise).
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!(
                            "    var n{id}_dist{suf}: f32 = 0.0; var n{id}_cell{suf}: f32 = 0.0; var n{id}_ccol{suf}: vec3<f32> = vec3(0.0);"
                        ));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_vor(lines, suf, format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_d0 = n{id}_distx * n{id}_twN.x + n{id}_disty * n{id}_twN.y + n{id}_distz * n{id}_twN.z;"));
                    lines.push(format!("    let n{id}_c0 = n{id}_cellx * n{id}_twN.x + n{id}_celly * n{id}_twN.y + n{id}_cellz * n{id}_twN.z;"));
                    lines.push(format!("    let n{id}_k0 = n{id}_ccolx * n{id}_twN.x + n{id}_ccoly * n{id}_twN.y + n{id}_ccolz * n{id}_twN.z;"));
                } else {
                    // Suffix "s" (single-plane), NOT "" — the final `emit_mask_mul_*` calls below
                    // emit `let n{id}_dist` / `_cell` / `_ccol`, so an unsuffixed `var n{id}_dist`
                    // here is a redeclaration in the same scope: naga rejects the whole module with
                    // `redefinition of 'n{id}_dist'` and the GPU material bake silently falls back
                    // to the CPU path (7.6x at 256², 67x at 512²). The triplanar arm never hit this
                    // because its vars are `_distx/_disty/_distz`.
                    emit_vor(lines, "s", format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_d0 = n{id}_dists;"));
                    lines.push(format!("    let n{id}_c0 = n{id}_cells;"));
                    lines.push(format!("    let n{id}_k0 = n{id}_ccols;"));
                }
                self.emit_mask_mul_f32(id, node, &format!("n{id}_d0"), &format!("n{id}_dist"), lines);
                self.emit_mask_mul_f32(id, node, &format!("n{id}_c0"), &format!("n{id}_cell"), lines);
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_k0"), &format!("n{id}_ccol"), lines);
            }
            NodeType::MathBinary => {
                let a = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", node.input_defaults[0].value_f32));
                let b = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                let expr = node.math_binary_op.wgsl_expr_f32(&a, &b);
                lines.push(format!("    let n{id}_raw = {expr};"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::MathUnary => {
                let v = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", node.input_defaults[0].value_f32));
                let expr = node.math_unary_op.wgsl_expr(&v);
                lines.push(format!("    let n{id}_raw = {expr};"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::Remap => {
                let v = self.input_var_name(id, 0, "0.0");
                let from_min = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                let from_max = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                let to_min = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", node.input_defaults[3].value_f32));
                let to_max = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", node.input_defaults[4].value_f32));
                lines.push(format!("    let n{id}_raw = mix({to_min}, {to_max}, saturate(({v} - {from_min}) / max({from_max} - {from_min}, 0.0001)));"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::ColorRamp => {
                let fac = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", node.input_defaults[0].value_f32));
                let c0 = format!("vec3(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4})", node.ramp_color0[0], node.ramp_color0[1], node.ramp_color0[2]);
                let c1 = format!("vec3(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4})", node.ramp_color1[0], node.ramp_color1[1], node.ramp_color1[2]);
                let p0 = node.ramp_pos0;
                let p1 = node.ramp_pos1;
                lines.push(format!("    let n{id}_rawc = mix({c0}, {c1}, saturate(({fac} - /*graph_param*/{p0:.4}) / max(/*graph_param*/{p1:.4} - /*graph_param*/{p0:.4}, 0.0001)));"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
            }
            NodeType::MixRgb => {
                let fac = self.input_var_name(id, 0, &format!("{:.4}", node.input_defaults[0].value_f32));
                let c1 = self.input_var_name_vec3(id, 1, &node.input_defaults[1].value_vec3);
                let c2 = self.input_var_name_vec3(id, 2, &node.input_defaults[2].value_vec3);
                // Photoshop over: blended = op(base, layer); result = mix(base, blended, Factor).
                // Formulas must match `ops::mix_blend_channel` (CPU bake).
                use crate::shader_nodes::MixBlend;
                let blended = match node.mix_blend {
                    MixBlend::Mix => c2.clone(),
                    MixBlend::Multiply => format!("({c1} * {c2})"),
                    MixBlend::Screen => {
                        format!("(vec3(1.0) - (vec3(1.0) - {c1}) * (vec3(1.0) - {c2}))")
                    }
                    MixBlend::Add => format!("min({c1} + {c2}, vec3(1.0))"),
                    MixBlend::Overlay => format!(
                        "select(vec3(1.0) - 2.0 * (vec3(1.0) - {c1}) * (vec3(1.0) - {c2}), 2.0 * {c1} * {c2}, {c1} < vec3(0.5))"
                    ),
                    MixBlend::Darken => format!("min({c1}, {c2})"),
                    MixBlend::Lighten => format!("max({c1}, {c2})"),
                    MixBlend::ColorBurn => format!(
                        "select(vec3(0.0), clamp(vec3(1.0) - (vec3(1.0) - {c1}) / max({c2}, vec3(1e-5)), vec3(0.0), vec3(1.0)), {c2} > vec3(1e-5))"
                    ),
                    MixBlend::ColorDodge => format!(
                        "select(vec3(1.0), clamp({c1} / max(vec3(1.0) - {c2}, vec3(1e-5)), vec3(0.0), vec3(1.0)), {c2} < vec3(0.99999))"
                    ),
                    MixBlend::LinearBurn => format!("clamp({c1} + {c2} - vec3(1.0), vec3(0.0), vec3(1.0))"),
                    MixBlend::SoftLight => {
                        // Pegtop soft light (matches ops::mix_blend_channel SoftLight).
                        format!(
                            "(select(\
                                {c1} + (2.0 * {c2} - vec3(1.0)) * (select(sqrt({c1}), ((16.0 * {c1} - 12.0) * {c1} + 4.0) * {c1}, {c1} <= vec3(0.25)) - {c1}), \
                                {c1} - (vec3(1.0) - 2.0 * {c2}) * {c1} * (vec3(1.0) - {c1}), \
                                {c2} < vec3(0.5)))"
                        )
                    }
                    MixBlend::HardLight => format!(
                        "select(vec3(1.0) - 2.0 * (vec3(1.0) - {c1}) * (vec3(1.0) - {c2}), 2.0 * {c1} * {c2}, {c2} < vec3(0.5))"
                    ),
                    MixBlend::VividLight => format!(
                        "select(\
                            clamp({c1} / max(2.0 * (vec3(1.0) - {c2}), vec3(1e-5)), vec3(0.0), vec3(1.0)), \
                            clamp(vec3(1.0) - (vec3(1.0) - {c1}) / max(2.0 * {c2}, vec3(1e-5)), vec3(0.0), vec3(1.0)), \
                            {c2} < vec3(0.5))"
                    ),
                    MixBlend::LinearLight => {
                        format!("clamp({c1} + 2.0 * {c2} - vec3(1.0), vec3(0.0), vec3(1.0))")
                    }
                    MixBlend::PinLight => format!(
                        "select(max({c1}, 2.0 * {c2} - vec3(1.0)), min({c1}, 2.0 * {c2}), {c2} < vec3(0.5))"
                    ),
                    MixBlend::HardMix => {
                        format!("select(vec3(1.0), vec3(0.0), {c1} + {c2} < vec3(1.0))")
                    }
                    MixBlend::Difference => format!("abs({c1} - {c2})"),
                    MixBlend::Exclusion => format!("({c1} + {c2} - 2.0 * {c1} * {c2})"),
                    MixBlend::Subtract => format!("clamp({c1} - {c2}, vec3(0.0), vec3(1.0))"),
                    MixBlend::Divide => format!(
                        "select(vec3(1.0), clamp({c1} / max({c2}, vec3(1e-5)), vec3(0.0), vec3(1.0)), {c2} > vec3(1e-5))"
                    ),
                };
                lines.push(format!("    let n{id}_rawc = mix({c1}, {blended}, {fac});"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
            }
            NodeType::BrightnessContrast => {
                let col = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let bright = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                let contrast = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                lines.push(format!("    let n{id}_rawc = saturate(({col} - 0.5) * (1.0 + {contrast}) + 0.5 + {bright});"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
            }
            NodeType::SeparateXyz => {
                let v = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                lines.push(format!("    let n{id}_x = {v}.x;"));
                lines.push(format!("    let n{id}_y = {v}.y;"));
                lines.push(format!("    let n{id}_z = {v}.z;"));
            }
            NodeType::CombineXyz => {
                let x = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", node.input_defaults[0].value_f32));
                let y = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                let z = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                lines.push(format!("    let n{id}_vec = vec3({x}, {y}, {z});"));
            }
            // --- Texture projection nodes ---
            NodeType::Triplanar => {
                let pos = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let nrm = self.input_var_name_vec3(id, 1, &node.input_defaults[1].value_vec3);
                let sharp = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                let scale = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", node.input_defaults[3].value_f32));
                lines.push(format!("    let n{id}_w = pow(abs({nrm}), vec3({sharp}));"));
                lines.push(format!("    let n{id}_weights = n{id}_w / max(dot(n{id}_w, vec3(1.0)), 0.0001);"));
                lines.push(format!("    let n{id}_uvx = {pos}.yz * {scale};"));
                lines.push(format!("    let n{id}_uvy = {pos}.xz * {scale};"));
                lines.push(format!("    let n{id}_uvz = {pos}.xy * {scale};"));
            }
            NodeType::BoxMapping => {
                let pos = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let scale = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                // 6-face cube projection: pick face by dominant normal component
                lines.push(format!("    var n{id}_uv: vec3<f32>;"));
                lines.push(format!("    var n{id}_face: f32;"));
                lines.push(format!("    {{ let ap = abs({pos}); let s = {scale};"));
                lines.push(format!("    if (ap.x >= ap.y && ap.x >= ap.z) {{ n{id}_uv = vec3({pos}.y * s, {pos}.z * s, 0.0); n{id}_face = select(1.0, 0.0, {pos}.x > 0.0); }}"));
                lines.push(format!("    else if (ap.y >= ap.z) {{ n{id}_uv = vec3({pos}.x * s, {pos}.z * s, 0.0); n{id}_face = select(3.0, 2.0, {pos}.y > 0.0); }}"));
                lines.push(format!("    else {{ n{id}_uv = vec3({pos}.x * s, {pos}.y * s, 0.0); n{id}_face = select(5.0, 4.0, {pos}.z > 0.0); }} }}"));
            }
            NodeType::CylindricalMapping => {
                let pos = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let axis = self.input_var_name_u32(
                    id,
                    1,
                    Node::default_f32(node, 1, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                let scale = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                // Cylindrical: atan2 around axis + height along axis
                lines.push(format!("    var n{id}_uv: vec3<f32>;"));
                lines.push(format!("    {{ let ax = {axis}; let s = {scale};"));
                lines.push(format!("    var a: f32; var h: f32;"));
                lines.push(format!("    if (ax == 0u) {{ a = atan2({pos}.z, {pos}.y); h = {pos}.x; }}"));
                lines.push(format!("    else if (ax == 2u) {{ a = atan2({pos}.x, {pos}.y); h = {pos}.z; }}"));
                lines.push(format!("    else {{ a = atan2({pos}.z, {pos}.x); h = {pos}.y; }}"));
                lines.push(format!("    n{id}_uv = vec3(a * 0.15915494 * s, h * s, 0.0); }}"));
            }
            NodeType::SphericalMapping => {
                let pos = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let scale = self.input_var_name(id, 1, &format!("{:.4}", node.input_defaults[1].value_f32));
                lines.push(format!("    var n{id}_uv: vec3<f32>;"));
                lines.push(format!("    {{ let d = normalize({pos}); let s = {scale};"));
                lines.push(format!("    n{id}_uv = vec3(atan2(d.z, d.x) * 0.15915494 * s, acos(clamp(d.y, -1.0, 1.0)) * 0.31830988 * s, 0.0); }}"));
            }
            NodeType::PlanarMapping => {
                let pos = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let axis = self.input_var_name_u32(
                    id,
                    1,
                    Node::default_f32(node, 1, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                let scale = self.input_var_name(id, 2, &format!("{:.4}", node.input_defaults[2].value_f32));
                // 0=XY, 1=XZ, 2=YZ
                lines.push(format!("    var n{id}_uv: vec3<f32>;"));
                lines.push(format!("    {{ let ax = {axis}; let s = {scale};"));
                lines.push(format!("    if (ax == 0u) {{ n{id}_uv = vec3({pos}.x * s, {pos}.y * s, 0.0); }}"));
                lines.push(format!("    else if (ax == 1u) {{ n{id}_uv = vec3({pos}.x * s, {pos}.z * s, 0.0); }}"));
                lines.push(format!("    else {{ n{id}_uv = vec3({pos}.y * s, {pos}.z * s, 0.0); }} }}"));
            }
            NodeType::UVTransform => {
                let uv = self.input_var_name_vec3(id, 0, &node.input_defaults[0].value_vec3);
                let ox = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", node.input_defaults[1].value_f32));
                let oy = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", node.input_defaults[2].value_f32));
                let rot = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", node.input_defaults[3].value_f32));
                let sx = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", node.input_defaults[4].value_f32));
                let sy = self.input_var_name(id, 5, &format!("/*graph_param*/{:.4}", node.input_defaults[5].value_f32));
                lines.push(format!("    var n{id}_uv: vec3<f32>;"));
                lines.push(format!("    {{ let uv2 = vec2({uv}.x, {uv}.y);"));
                lines.push(format!("    let c = cos({rot}); let s = sin({rot});"));
                lines.push(format!("    let r = vec2(uv2.x * c - uv2.y * s, uv2.x * s + uv2.y * c);"));
                lines.push(format!("    n{id}_uv = vec3(r.x * {sx} + {ox}, r.y * {sy} + {oy}, 0.0); }}"));
            }
            // --- Texture pattern nodes (built-in projection; unconnected Offset = surface p) ---
            NodeType::TextureSample => {
                let uv_in = self.generator_pos_expr(id, 0, node);
                let pat = self.input_var_name_u32(
                    id,
                    1,
                    Node::default_f32(node, 1, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                let tri = self.emit_builtin_projection(id, node, &uv_in, lines);
                let emit_pat = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_col{suffix}: vec3<f32>;"));
                    }
                    lines.push(format!("    {{ let uv2 = vec2(({dom}).x, ({dom}).y); let pi = {pat};"));
                    lines.push(format!("    if (pi == 1u) {{ n{id}_col{suffix} = vec3(uv2.x, uv2.y, 0.0); }}"));
                    lines.push(format!("    else if (pi == 2u) {{ let bx = step(fract(uv2.x * 5.0), 0.9) * step(fract(uv2.y * 5.0 + step(0.5, fract(uv2.y * 2.5)) * 0.5), 0.9); n{id}_col{suffix} = vec3(bx); }}"));
                    lines.push(format!("    else {{ let ch = step(0.5, fract(uv2.x * 2.0)) + step(0.5, fract(uv2.y * 2.0)); n{id}_col{suffix} = vec3(f32(u32(ch) % 2u)); }} }}"));
                };
                if tri {
                    // G5: skip pattern eval on near-zero triplanar axes.
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!("    var n{id}_col{suf}: vec3<f32> = vec3(0.0);"));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_pat(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_rawc = n{id}_colx * n{id}_twN.x + n{id}_coly * n{id}_twN.y + n{id}_colz * n{id}_twN.z;"));
                } else {
                    emit_pat(lines, "p", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_rawc = n{id}_colp;"));
                }
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
                lines.push(format!("    let n{id}_alpha = 1.0;"));
            }
            NodeType::GradientTexture => {
                let uv_in = self.generator_pos_expr(id, 0, node);
                let mode = self.input_var_name_u32(
                    id,
                    1,
                    Node::default_f32(node, 1, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                let tri = self.emit_builtin_projection(id, node, &uv_in, lines);
                let emit_grad = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_val{suffix}: f32;"));
                    }
                    lines.push(format!("    {{ let uv2 = vec2(({dom}).x, ({dom}).y); let mi = {mode};"));
                    lines.push(format!("    if (mi == 1u) {{ n{id}_val{suffix} = length(uv2 - 0.5); }}"));
                    lines.push(format!("    else if (mi == 2u) {{ n{id}_val{suffix} = length(vec3(uv2, 0.0)); }}"));
                    lines.push(format!("    else {{ n{id}_val{suffix} = uv2.x; }}"));
                    lines.push(format!("    n{id}_val{suffix} = saturate(n{id}_val{suffix}); }}"));
                };
                if tri {
                    // G5: weight-guard triplanar gradient samples.
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!("    var n{id}_val{suf}: f32 = 0.0;"));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_grad(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_g0 = n{id}_valx * n{id}_twN.x + n{id}_valy * n{id}_twN.y + n{id}_valz * n{id}_twN.z;"));
                } else {
                    emit_grad(lines, "g", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_g0 = n{id}_valg;"));
                }
                self.emit_mask_mul_f32(id, node, &format!("n{id}_g0"), &format!("n{id}_val"), lines);
                lines.push(format!("    let n{id}_col = vec3(n{id}_val);"));
            }
            NodeType::WaveTexture => {
                let uv_in = self.generator_pos_expr(id, 0, node);
                let scale = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 5.0)));
                let dist = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.0)));
                let prof = self.input_var_name_u32(
                    id,
                    3,
                    Node::default_f32(node, 3, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                let tri = self.emit_builtin_projection(id, node, &uv_in, lines);
                let emit_wave = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_val{suffix}: f32;"));
                    }
                    lines.push(format!("    {{ let t = (({dom}).x + ({dom}).y) * {scale} + {dist} * sin(({dom}).x * 3.14159); let pi = {prof};"));
                    lines.push(format!("    if (pi == 1u) {{ n{id}_val{suffix} = fract(t); }}"));
                    lines.push(format!("    else if (pi == 2u) {{ n{id}_val{suffix} = abs(fract(t) * 2.0 - 1.0); }}"));
                    lines.push(format!("    else {{ n{id}_val{suffix} = sin(t * 6.28318) * 0.5 + 0.5; }} }}"));
                };
                if tri {
                    // G5: weight-guard triplanar wave samples.
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!("    var n{id}_val{suf}: f32 = 0.0;"));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_wave(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_w0 = n{id}_valx * n{id}_twN.x + n{id}_valy * n{id}_twN.y + n{id}_valz * n{id}_twN.z;"));
                } else {
                    emit_wave(lines, "w", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_w0 = n{id}_valw;"));
                }
                self.emit_mask_mul_f32(id, node, &format!("n{id}_w0"), &format!("n{id}_val"), lines);
                lines.push(format!("    let n{id}_col = vec3(n{id}_val);"));
            }
            NodeType::MusgraveTexture => {
                let pos = self.generator_pos_expr(id, 0, node);
                let scale = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 5.0)));
                let detail = self.input_var_name_u32(
                    id,
                    2,
                    Node::default_f32(node, 2, 3.0).round().clamp(1.0, 64.0) as u32,
                );
                let dim = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 2.0)));
                let lac = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 2.0)));
                let tri = self.emit_builtin_projection(id, node, &pos, lines);
                let emit_mus = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_val{suffix}: f32;"));
                    }
                    lines.push(format!("    {{ var mp = ({dom}) * {scale}; var freq = 1.0; var amp = 1.0; var sum = 0.0;"));
                    lines.push(format!("    for (var oi = 0u; oi < {detail}; oi++) {{"));
                    lines.push(format!("        sum += fbm_wgsl(mp * freq, 1u) * amp;"));
                    lines.push(format!("        amp *= pow(max(freq, 0.001), -{dim}); freq *= {lac};"));
                    lines.push(format!("    }} n{id}_val{suffix} = sum; }}"));
                };
                if tri {
                    // G5: multi-octave Musgrave is the expensive path — skip zero-weight axes.
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!("    var n{id}_val{suf}: f32 = 0.0;"));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_mus(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_m0 = n{id}_valx * n{id}_twN.x + n{id}_valy * n{id}_twN.y + n{id}_valz * n{id}_twN.z;"));
                } else {
                    emit_mus(lines, "m", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_m0 = n{id}_valm;"));
                }
                self.emit_mask_mul_f32(id, node, &format!("n{id}_m0"), &format!("n{id}_val"), lines);
            }
            NodeType::Checker => {
                let pos = self.generator_pos_expr(id, 0, node);
                let c1 = self.input_var_name_vec3(id, 1, &Node::default_vec3(node, 1, [0.8, 0.8, 0.8]));
                let c2 = self.input_var_name_vec3(id, 2, &Node::default_vec3(node, 2, [0.2, 0.2, 0.2]));
                let scale = self.input_var_name(id, 3, &format!("{:.4}", Node::default_f32(node, 3, 5.0)));
                let tri = self.emit_builtin_projection(id, node, &pos, lines);
                // Formula source of truth: eval_nodes.rs Checker arm — integer parity of
                // floor(domain * scale) summed over axes; & 1 matches Rust for negatives.
                let emit_chk = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_fac{suffix}: f32;"));
                    }
                    lines.push(format!("    {{ let cp = floor(({dom}) * {scale});"));
                    lines.push(format!(
                        "    n{id}_fac{suffix} = f32((i32(cp.x) + i32(cp.y) + i32(cp.z)) & 1); }}"
                    ));
                };
                if tri {
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!("    var n{id}_fac{suf}: f32 = 0.0;"));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_chk(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_f0 = n{id}_facx * n{id}_twN.x + n{id}_facy * n{id}_twN.y + n{id}_facz * n{id}_twN.z;"));
                } else {
                    emit_chk(lines, "c", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_f0 = n{id}_facc;"));
                }
                lines.push(format!("    let n{id}_fac = n{id}_f0;"));
                lines.push(format!("    let n{id}_col = mix({c1}, {c2}, n{id}_f0);"));
            }
            NodeType::Gamma => {
                let c = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [1.0, 1.0, 1.0]));
                let g = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 1.0)));
                // Must match eval_nodes.rs Gamma: pow(max(c,0), max(g,1e-4)) per channel.
                lines.push(format!(
                    "    let n{id}_col = pow(max({c}, vec3<f32>(0.0)), vec3<f32>(max({g}, 1e-4)));"
                ));
            }
            NodeType::BrickTexture => {
                let uv_in = self.generator_pos_expr(id, 0, node);
                let scale = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 5.0)));
                let mortar = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.02)));
                let row_off = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 0.5)));
                let tri = self.emit_builtin_projection(id, node, &uv_in, lines);
                let emit_brick = |lines: &mut Vec<String>, suffix: &str, dom: &str, declare: bool| {
                    if declare {
                        lines.push(format!("    var n{id}_col{suffix}: vec3<f32>; var n{id}_fac{suffix}: f32;"));
                    }
                    lines.push(format!("    {{ let bv = vec2(({dom}).x, ({dom}).y) * {scale};"));
                    lines.push(format!("    let row = floor(bv.y); let off = {row_off} * step(0.5, fract(row * 0.5));"));
                    lines.push(format!("    let bx = fract(bv.x + off); let by = fract(bv.y);"));
                    lines.push(format!("    let mx = step({mortar}, bx) * step(bx, 1.0 - {mortar});"));
                    lines.push(format!("    let my = step({mortar}, by) * step(by, 1.0 - {mortar});"));
                    lines.push(format!("    n{id}_fac{suffix} = mx * my; n{id}_col{suffix} = vec3(n{id}_fac{suffix}); }}"));
                };
                if tri {
                    // G5: weight-guard triplanar brick samples.
                    for (suf, axis, uv) in [("x", "x", "ux"), ("y", "y", "uy"), ("z", "z", "uz")] {
                        lines.push(format!(
                            "    var n{id}_col{suf}: vec3<f32> = vec3(0.0); var n{id}_fac{suf}: f32 = 0.0;"
                        ));
                        lines.push(format!("    if (n{id}_twN.{axis} > 0.01) {{"));
                        emit_brick(lines, suf, &format!("n{id}_{uv}"), false);
                        lines.push("    }".into());
                    }
                    lines.push(format!("    let n{id}_f0 = n{id}_facx * n{id}_twN.x + n{id}_facy * n{id}_twN.y + n{id}_facz * n{id}_twN.z;"));
                } else {
                    emit_brick(lines, "b", &format!("n{id}_dom"), true);
                    lines.push(format!("    let n{id}_f0 = n{id}_facb;"));
                }
                self.emit_mask_mul_f32(id, node, &format!("n{id}_f0"), &format!("n{id}_fac"), lines);
                lines.push(format!("    let n{id}_col = vec3(n{id}_fac);"));
            }
            NodeType::MaterialOutput => {
                // Handled separately in compile_wgsl
            }
            NodeType::ConstantColor => {
                // Solid color lives on ramp_color0 (param panel); default set in Node::new.
                lines.push(format!(
                    "    let n{id}_col = vec3(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4});",
                    node.ramp_color0[0], node.ramp_color0[1], node.ramp_color0[2]
                ));
            }
            NodeType::ImageTexture => {
                // Hybrid visibility is bake-to-maps; WGSL samples white placeholder.
                // Projection is still baked into the node (badge + param) for map domain parity.
                let uv_in = self.generator_pos_expr(id, 0, node);
                let _tri = self.emit_builtin_projection(id, node, &uv_in, lines);
                lines.push(format!("    let n{id}_rawc = vec3(1.0, 1.0, 1.0);"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
                lines.push(format!("    let n{id}_alpha = 1.0;"));
            }
            NodeType::ConstantValue => {
                lines.push(format!("    let n{id}_val = /*graph_param*/{:.4};", node.const_float));
            }
            NodeType::Invert => {
                let v = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [0.5, 0.5, 0.5]));
                // Normal-map Invert (panel tag mp:5:inv:…): flip XY slopes only, keep Z "out".
                // Full 1-rgb would flip the blue channel → decode z becomes negative and the
                // whole surface lighting inverts wrong (owner: "inverting normal should reverse
                // what goes out / what goes in"). Color/rough/etc. keep classic 1-x invert.
                let normal_map_invert = crate::material_panel::lower::parse_panel_tag(&node.label)
                    .map(|(k, role, _, _)| {
                        k == crate::material_panel::model::ChannelKind::Normal as u32
                            && role == "inv"
                    })
                    .unwrap_or(false);
                if normal_map_invert {
                    // encoded: n_ts = c*2-1; flip xy; re-encode. |n| preserved (z kept).
                    lines.push(format!(
                        "    let n{id}_rawc = vec3(1.0 - ({v}).x, 1.0 - ({v}).y, ({v}).z);"
                    ));
                } else {
                    lines.push(format!("    let n{id}_rawc = 1.0 - ({v});"));
                }
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
            }
            NodeType::ColorCorrect => {
                let col = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [0.5; 3]));
                let hue = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 0.0)));
                let sat = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                let val = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 1.0)));
                let gamma = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 1.0)));
                let exp = self.input_var_name(id, 5, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 5, 0.0)));
                let contrast = self.input_var_name(id, 6, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 6, 0.0)));
                // Lightweight HSV-ish correct (no full preamble dependency)
                lines.push(format!("    var n{id}_rawc: vec3<f32>;"));
                lines.push(format!("    {{ var c = {col} * exp2({exp});"));
                lines.push(format!("    c = (c - 0.5) * (1.0 + {contrast}) + 0.5;"));
                lines.push(format!("    let luma = dot(c, vec3(0.2126, 0.7152, 0.0722));"));
                lines.push(format!("    c = mix(vec3(luma), c, {sat}) * {val};"));
                lines.push(format!("    // hue rotate in YIQ-ish space"));
                lines.push(format!("    let ang = {hue} * 6.2831853; let ca = cos(ang); let sa = sin(ang);"));
                lines.push(format!("    let y = luma; let i = dot(c, vec3(0.596, -0.274, -0.322)); let q = dot(c, vec3(0.211, -0.523, 0.312));"));
                lines.push(format!("    let ir = i * ca - q * sa; let qr = i * sa + q * ca;"));
                lines.push(format!("    c = saturate(vec3(y) + vec3(0.956 * ir + 0.621 * qr, -0.272 * ir - 0.647 * qr, -1.106 * ir + 1.703 * qr));"));
                lines.push(format!("    n{id}_rawc = pow(max(c, vec3(0.0)), vec3(1.0 / max({gamma}, 0.001))); }}"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawc"), &format!("n{id}_col"), lines);
            }
            NodeType::Curvature => {
                // Analytic terrain height field (bake-safe) — Laplacian → curvature mask.
                // Live SDF path can later substitute calc_curvature when field is available.
                let pos = self.generator_pos_expr(id, 0, node);
                let eps = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 0.02)));
                let contrast = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                let strength = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 1.0)));
                lines.push(format!("    let n{id}_raw = saturate(sn_mesh_curvature({pos}, {eps}) * {contrast} * {strength});"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::Cavity => {
                let pos = self.generator_pos_expr(id, 0, node);
                let eps = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 0.02)));
                let scale = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                lines.push(format!("    var n{id}_raw: f32;"));
                lines.push(format!("    {{ let e = max({eps}, 1e-4); let p0 = {pos} * {scale};"));
                lines.push(format!("    let h = sn_field_h(p0);"));
                lines.push(format!("    let lap = (sn_field_h(p0 + vec3(e,0.0,0.0)) + sn_field_h(p0 - vec3(e,0.0,0.0))"));
                lines.push(format!("      + sn_field_h(p0 + vec3(0.0,e,0.0)) + sn_field_h(p0 - vec3(0.0,e,0.0)) - 4.0 * h) / (e * e);"));
                lines.push(format!("    n{id}_raw = saturate((-lap) * 0.2); }}")); // concave only
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::AmbientOcclusion => {
                // P5: live path uses host `calc_ao` (sdf_material.wgsl) — same IQ sample
                // loop as CPU `sdf_ambient_occlusion`. Geometric `n` is exact here
                // (eval_node_material receives the G-buffer / hit normal). GPU material
                // bake deliberately excludes this node (`graph_is_gpu_bakeable`) because
                // the bake shader has no scene_sdf; CPU bake is the map source of truth.
                let pos = self.generator_pos_expr(id, 0, node);
                let nrm = self.input_var_name_vec3(id, 1, &Node::default_vec3(node, 1, [0.0, 0.0, 1.0]));
                lines.push(format!("    var n{id}_raw: f32;"));
                lines.push(format!(
                    "    {{ let nn = normalize({nrm}); n{id}_raw = calc_ao({pos}, nn); }}"
                ));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::SurfaceDirection => {
                // EXACT in WGSL — unlike every other surface analyzer here. `eval_node_material`
                // receives the true per-pixel geometric normal `n` (deferred passes it the
                // G-buffer normal; the raster path its interpolated world normal), so the live
                // shader needs no synthetic height field and no approximation: it is literally
                // `dot(n, axis)`. The CPU bake arm reaches the SAME value the long way round
                // (project onto the isosurface, normalized SDF gradient) because ITS `n` is the
                // flat bake quad's constant — two routes, one definition.
                //
                // Grow/Band (sockets 2/3) are surface-projection knobs with no meaning when the
                // normal is already exact, so they are intentionally unread here. Contrast and
                // Strength shape the result identically to the CPU arm.
                let contrast = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 1.0)));
                let strength = self.input_var_name(id, 5, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 5, 1.0)));
                let a = crate::shader_nodes::nodes::surface_direction::axis_vec(node.sdf_curv_region);
                lines.push(format!("    var n{id}_raw: f32; var n{id}_dot: f32;"));
                lines.push(format!(
                    "    {{ let ax = normalize(vec3<f32>(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4}));",
                    a[0], a[1], a[2]
                ));
                lines.push(format!("    let d = clamp(dot(normalize(n), ax), -1.0, 1.0);"));
                lines.push(format!("    let t = d * 0.5 + 0.5;"));
                lines.push(format!(
                    "    n{id}_dot = d; n{id}_raw = saturate(saturate((t - 0.5) * max({contrast}, 0.001) + 0.5) * {strength}); }}"
                ));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::HeightToNormal => {
                let str = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 1.0)));
                let eps = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.02)));
                lines.push(format!("    var n{id}_rawv: vec3<f32>;"));
                lines.push(format!("    {{ let e = max({eps}, 1e-4); let s = {str};"));
                lines.push(format!("    let hx = sn_input_{id}_0(hit_prim, p + vec3(e, 0.0, 0.0), n) - sn_input_{id}_0(hit_prim, p - vec3(e, 0.0, 0.0), n);"));
                lines.push(format!("    let hy = sn_input_{id}_0(hit_prim, p + vec3(0.0, e, 0.0), n) - sn_input_{id}_0(hit_prim, p - vec3(0.0, e, 0.0), n);"));
                lines.push(format!("    n{id}_rawv = normalize(vec3(-hx * s, -hy * s, 2.0 * e)); }}"));
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawv"), &format!("n{id}_vec"), lines);
            }
            NodeType::Slope => {
                let nrm = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [0.0, 1.0, 0.0]));
                lines.push(format!(
                    "    let n{id}_raw = acos(clamp(({nrm}).y, -1.0, 1.0)) * 0.318309886; // / pi"
                ));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::PrimSdfMask => {
                // Deferred WGSL has no scene prim SDF — approximate with height field + grow/soft.
                // Full prim SDF is evaluated on the CPU map-bake path.
                let pos = self.generator_pos_expr(id, 0, node);
                let grow = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 0.0)));
                let soft = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.08)));
                let contrast = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 1.0)));
                let strength = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 1.0)));
                let inv = self.input_var_name(id, 5, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 5, 0.0)));
                lines.push(format!("    var n{id}_dist: f32; var n{id}_raw: f32; var n{id}_inside: f32;"));
                lines.push(format!("    {{ let d = sn_field_h({pos}) - {grow}; let s = max({soft}, 1e-5);"));
                lines.push(format!("    var m = saturate(0.5 - d / (2.0 * s));"));
                lines.push(format!("    if ({inv} > 0.5) {{ m = 1.0 - m; }}"));
                lines.push(format!("    m = saturate((m - 0.5) * max({contrast}, 0.001) + 0.5) * {strength};"));
                lines.push(format!("    n{id}_dist = d; n{id}_raw = m; n{id}_inside = select(0.0, 1.0, d < 0.0); }}"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::SdfCurvature => {
                // Live WGSL has no scene SDF — height-field laplacian is a preview only.
                // Full 3D surface mean-curvature is evaluated on the CPU map-bake path
                // (project UV → isosurface, Hessian κ). Prefer Apply / baked maps for edge wear.
                let pos = self.generator_pos_expr(id, 0, node);
                let eps = self.input_var_name(id, 1, &format!("{:.4}", Node::default_f32(node, 1, 0.02)));
                let grow = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.0)));
                let band = self.input_var_name(id, 3, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 3, 0.2)));
                let contrast = self.input_var_name(id, 4, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 4, 1.5)));
                let strength = self.input_var_name(id, 5, &format!("{:.4}", Node::default_f32(node, 5, 1.0)));
                let region = node.sdf_curv_region;
                lines.push(format!("    var n{id}_raw: f32; var n{id}_curv: f32; var n{id}_conv: f32; var n{id}_conc: f32;"));
                lines.push(format!("    {{ let e = max({eps}, 1e-4); var p0 = {pos};"));
                // One Newton step toward isosurface on the analytic field (preview only)
                lines.push(format!("    let h0 = sn_field_h(p0) - {grow};"));
                lines.push(format!("    let gx = (sn_field_h(p0+vec3(e,0.0,0.0))-sn_field_h(p0-vec3(e,0.0,0.0)))*(0.5/e);"));
                lines.push(format!("    let gy = (sn_field_h(p0+vec3(0.0,e,0.0))-sn_field_h(p0-vec3(0.0,e,0.0)))*(0.5/e);"));
                lines.push(format!("    let gz = (sn_field_h(p0+vec3(0.0,0.0,e))-sn_field_h(p0-vec3(0.0,0.0,e)))*(0.5/e);"));
                lines.push(format!("    let gl = max(length(vec3(gx,gy,gz)), 1e-5);"));
                lines.push(format!("    p0 = p0 - vec3(gx,gy,gz)/gl * h0;"));
                lines.push(format!("    let h = sn_field_h(p0);"));
                lines.push(format!("    let lap = (sn_field_h(p0+vec3(e,0.0,0.0))+sn_field_h(p0-vec3(e,0.0,0.0))"));
                lines.push(format!("      +sn_field_h(p0+vec3(0.0,e,0.0))+sn_field_h(p0-vec3(0.0,e,0.0))"));
                lines.push(format!("      +sn_field_h(p0+vec3(0.0,0.0,e))+sn_field_h(p0-vec3(0.0,0.0,e))-6.0*h)/(e*e);"));
                lines.push(format!("    let curv = clamp(lap * 0.15, -1.0, 1.0);"));
                lines.push(format!("    let near = saturate(1.0 - abs(sn_field_h(p0) - {grow}) / max({band}, 1e-4));"));
                lines.push(format!("    let c = max({contrast}, 0.001); let s = {strength} * near;"));
                lines.push(format!("    let cv = saturate(max(curv, 0.0) * c) * s;"));
                lines.push(format!("    let cc = saturate(max(-curv, 0.0) * c) * s;"));
                match region {
                    1 => lines.push(format!("    n{id}_raw = cv;")),
                    2 => lines.push(format!("    n{id}_raw = cc;")),
                    _ => lines.push(format!("    n{id}_raw = saturate(abs(curv) * c) * s;")),
                }
                lines.push(format!("    n{id}_curv = curv; n{id}_conv = cv; n{id}_conc = cc; }}"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::VectorMath => {
                let a = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [0.0; 3]));
                let b = self.input_var_name_vec3(id, 1, &Node::default_vec3(node, 1, [0.0; 3]));
                let sc = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                lines.push(format!("    var n{id}_rawv: vec3<f32> = vec3(0.0); var n{id}_raws: f32 = 0.0;"));
                match node.vector_math_op {
                    VectorMathOp::Add => lines.push(format!("    n{id}_rawv = {a} + {b};")),
                    VectorMathOp::Subtract => lines.push(format!("    n{id}_rawv = {a} - {b};")),
                    VectorMathOp::Multiply => lines.push(format!("    n{id}_rawv = {a} * {b};")),
                    VectorMathOp::Scale => lines.push(format!("    n{id}_rawv = {a} * {sc};")),
                    VectorMathOp::Dot => lines.push(format!("    n{id}_raws = dot({a}, {b}); n{id}_rawv = vec3(n{id}_raws);")),
                    VectorMathOp::Cross => lines.push(format!("    n{id}_rawv = cross({a}, {b});")),
                    VectorMathOp::Length => lines.push(format!("    n{id}_raws = length({a}); n{id}_rawv = vec3(n{id}_raws);")),
                    VectorMathOp::Normalize => lines.push(format!("    n{id}_rawv = normalize({a});")),
                    VectorMathOp::Reflect => lines.push(format!("    n{id}_rawv = reflect({a}, normalize({b}));")),
                    VectorMathOp::Distance => lines.push(format!("    n{id}_raws = distance({a}, {b}); n{id}_rawv = vec3(n{id}_raws);")),
                }
                self.emit_mask_mul_vec3(id, node, &format!("n{id}_rawv"), &format!("n{id}_vec"), lines);
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raws"), &format!("n{id}_val"), lines);
            }
            NodeType::Clamp => {
                let v = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 0, 0.5)));
                let mn = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 0.0)));
                let mx = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 1.0)));
                lines.push(format!("    let n{id}_raw = clamp({v}, {mn}, {mx});"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::MixFloat => {
                let a = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 0, 0.0)));
                let b = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 1.0)));
                let t = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.5)));
                lines.push(format!("    let n{id}_raw = mix({a}, {b}, {t});"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::Smoothstep => {
                let e0 = self.input_var_name(id, 0, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 0, 0.0)));
                let e1 = self.input_var_name(id, 1, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 1, 1.0)));
                let v = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.5)));
                lines.push(format!("    let n{id}_raw = smoothstep({e0}, {e1}, {v});"));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_res"), lines);
            }
            NodeType::SdfThickness => {
                // Live path: abs(scene_sdf(p - n * h)) style thickness, matching CPU.
                // Uses host scene_sdf / calc_ao environment (deferred has both).
                let pos = self.generator_pos_expr(id, 0, node);
                let nrm = self.input_var_name_vec3(id, 1, &Node::default_vec3(node, 1, [0.0, 0.0, 1.0]));
                let max_d = self.input_var_name(id, 2, &format!("/*graph_param*/{:.4}", Node::default_f32(node, 2, 0.25)));
                let contrast = self.input_var_name(id, 3, &format!("{:.4}", Node::default_f32(node, 3, 1.0)));
                let strength = self.input_var_name(id, 4, &format!("{:.4}", Node::default_f32(node, 4, 1.0)));
                lines.push(format!("    var n{id}_raw: f32;"));
                lines.push(format!(
                    "    {{ let nn = normalize({nrm}); let md = max({max_d}, 1e-4);"
                ));
                lines.push(format!(
                    "    var th = 0.0; var scale = 1.0; let samples = 5u;"
                ));
                lines.push(format!(
                    "    for (var i = 1u; i <= samples; i++) {{ let h = md * f32(i) / f32(samples);"
                ));
                lines.push(format!(
                    "      let d = abs(scene_sdf({pos} - nn * h)); th += max(0.0, h - d) * scale; scale *= 0.75; }}"
                ));
                lines.push(format!(
                    "    let t = saturate(1.0 - th / md); let c = max({contrast}, 0.001);"
                ));
                lines.push(format!(
                    "    n{id}_raw = saturate(((t - 0.5) * c + 0.5) * {strength}); }}"
                ));
                self.emit_mask_mul_f32(id, node, &format!("n{id}_raw"), &format!("n{id}_val"), lines);
            }
            NodeType::SdfPosition => {
                let pos = self.generator_pos_expr(id, 0, node);
                let axis = self.input_var_name_u32(
                    id,
                    1,
                    Node::default_f32(node, 1, 0.0).round().clamp(0.0, 16.0) as u32,
                );
                // Live: normalize world p with a soft heuristic (no AABB in live graph).
                // Bake CPU uses body AABB; live uses a gentle squash so values stay 0..1-ish.
                lines.push(format!("    var n{id}_col: vec3<f32>; var n{id}_x: f32; var n{id}_y: f32; var n{id}_z: f32;"));
                lines.push(format!(
                    "    {{ let p0 = {pos}; let nx = saturate(p0.x * 0.5 + 0.5); let ny = saturate(p0.y * 0.5 + 0.5); let nz = saturate(p0.z * 0.5 + 0.5);"
                ));
                lines.push(format!("    n{id}_x = nx; n{id}_y = ny; n{id}_z = nz;"));
                lines.push(format!("    let ax = {axis};"));
                lines.push(format!("    if (ax == 1u) {{ n{id}_col = vec3(nx); }}"));
                lines.push(format!("    else if (ax == 2u) {{ n{id}_col = vec3(ny); }}"));
                lines.push(format!("    else if (ax == 3u) {{ n{id}_col = vec3(nz); }}"));
                lines.push(format!("    else {{ n{id}_col = vec3(nx, ny, nz); }} }}"));
            }
        }
    }

    /// Like `input_var_name`, but for a target socket declared `Vec3`/`Color` (i.e. `vec3<f32>`
    /// in WGSL): if the connected source socket is `Float` (`f32`), wraps it in `vec3(...)` so
    /// the emitted assignment type-checks. `SocketType::compatible()` explicitly allows
    /// Float→Vec3/Color ("broadcast / luminance ... so Noise→albedo ... connections work like
    /// Blender") but `output_var` returns a bare scalar expression for a Float-output node — the
    /// broadcast has to happen at the USE site (the assignment into a vec3-typed field), because
    /// `output_var` itself doesn't know what the connection's destination type is.
    ///
    /// Found via the GPU material-bake P1 parity test: a bare `Noise → Albedo` connection (the
    /// canonical procedural material pattern) compiles WGSL that fails naga validation with
    /// `InvalidStoreTypes` — `result.color = n1_val;` where `n1_val: f32` and `result.color:
    /// vec3<f32>`. This was never caught before because nothing has compiled a REAL generated
    /// graph standalone; `shader_build.rs`'s own compile test uses a hand-written stub, and the
    /// only two current uses of `generate_wgsl()` output stash the string on disk/on a prim
    /// without ever calling `create_shader_module` on it (the deferred/analytical consumer this
    /// codegen originally targeted was deleted 2026-07-14).
    fn input_var_name_vec3_coerced(&self, node_id: NodeId, input_idx: usize, default: &str) -> String {
        if let Some(conn) = self.connections.iter().find(|c| c.to_node == node_id && c.to_input == input_idx) {
            let expr = self.output_var(conn.from_node, conn.from_output);
            let src_ty = self
                .find_node(conn.from_node)
                .map(|n| Node::output_defs(&n.node_type))
                .and_then(|outs| outs.get(conn.from_output).map(|s| s.socket_type))
                .unwrap_or(SocketType::Vec3);
            if src_ty == SocketType::Float {
                format!("vec3({expr})")
            } else {
                expr
            }
        } else {
            default.to_string()
        }
    }

    /// Integer-typed input (G1). Unconnected default → concrete `Nu` literal. Connected
    /// float socket → `u32(expr)`. Connected Vec3/Color → luminance then `u32(...)`.
    ///
    /// **Never** emit `u32({:.4})` with an abstract float default — WGSL refuses to
    /// auto-convert abstract floats to integers (7 node types shipped broken that way).
    /// Float-consumed sockets still use [`Self::input_var_name`] with `{:.4}` defaults.
    fn input_var_name_u32(&self, node_id: NodeId, input_idx: usize, default: u32) -> String {
        if let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.to_node == node_id && c.to_input == input_idx)
        {
            let expr = self.output_var(conn.from_node, conn.from_output);
            let src_ty = self
                .find_node(conn.from_node)
                .map(|n| Node::output_defs(&n.node_type))
                .and_then(|outs| outs.get(conn.from_output).map(|s| s.socket_type))
                .unwrap_or(SocketType::Float);
            match src_ty {
                SocketType::Vec3 | SocketType::Color => {
                    format!(
                        "u32(dot(({expr}), vec3(0.2126, 0.7152, 0.0722)))"
                    )
                }
                SocketType::Float => format!("u32({expr})"),
            }
        } else {
            format!("{default}u")
        }
    }

    /// Float input expression. **Always** coerces Vec3/Color sources to a scalar via Rec.709
    /// luminance so generated WGSL type-checks (LOG_TRIAGE BUG 3: ConstantColor → Roughness
    /// stored `n1_col` into `f32` → InvalidStoreTypes → silent CPU bake fallback).
    ///
    /// Matches CPU `Val::as_f` and `SocketType::compatible`'s "broadcast / luminance" promise.
    /// Do not reintroduce a raw path — the editor accepts Color→Float wires because compatible()
    /// says yes.
    fn input_var_name(&self, node_id: NodeId, input_idx: usize, default: &str) -> String {
        if let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.to_node == node_id && c.to_input == input_idx)
        {
            let expr = self.output_var(conn.from_node, conn.from_output);
            let src_ty = self
                .find_node(conn.from_node)
                .map(|n| Node::output_defs(&n.node_type))
                .and_then(|outs| outs.get(conn.from_output).map(|s| s.socket_type))
                .unwrap_or(SocketType::Float);
            match src_ty {
                SocketType::Vec3 | SocketType::Color => {
                    // Rec.709 luminance — same weights as `Val::as_f` on the CPU bake path.
                    format!("dot(({expr}), vec3(0.2126, 0.7152, 0.0722))")
                }
                SocketType::Float => expr,
            }
        } else {
            default.to_string()
        }
    }

    /// Sample position for generators with baked-in projection.
    /// Wired → connection. Unconnected → `p + Offset` (Offset defaults 0 = pure surface).
    fn generator_pos_expr(&self, node_id: NodeId, pos_idx: usize, node: &Node) -> String {
        if let Some(conn) = self
            .connections
            .iter()
            .find(|c| c.to_node == node_id && c.to_input == pos_idx)
        {
            self.output_var(conn.from_node, conn.from_output)
        } else {
            let o = Node::default_vec3(node, pos_idx, [0.0, 0.0, 0.0]);
            format!("(p + vec3(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4}))", o[0], o[1], o[2])
        }
    }

    /// Vec3/Color input expression. **Always** coerces Float sources to `vec3(s)` so
    /// generated WGSL type-checks (LOG_TRIAGE 2026-07-29: Noise → VectorMath Scale stored
    /// scalar into vec3 → InvalidStoreTypes → silent 10× CPU bake fallback).
    ///
    /// Do not reintroduce a "raw" path — `SocketType::compatible` allows Float→Vec3 and
    /// every call site that wants a vec3 must broadcast at the use site.
    fn input_var_name_vec3(&self, node_id: NodeId, input_idx: usize, default: &[f32; 3]) -> String {
        let def = format!(
            "vec3(/*graph_param*/{:.4}, /*graph_param*/{:.4}, /*graph_param*/{:.4})",
            default[0], default[1], default[2]
        );
        self.input_var_name_vec3_coerced(node_id, input_idx, &def)
    }

    /// Get the WGSL variable name for a node output socket
    fn output_var(&self, node_id: NodeId, output_idx: usize) -> String {
        let node = match self.find_node(node_id) {
            Some(n) => n,
            None => return "0.0".into(),
        };
        match &node.node_type {
            NodeType::Reroute => format!("n{node_id}_val"),
            NodeType::Frame => "0.0".into(),
            NodeType::TexCoord => match output_idx {
                0 => format!("n{node_id}_pos"),
                _ => format!("n{node_id}_nrm"),
            },
            NodeType::ObjectInfo => format!("n{node_id}_idx"),
            NodeType::Noise => format!("n{node_id}_val"),
            NodeType::Voronoi => match output_idx {
                0 => format!("n{node_id}_dist"),
                1 => format!("n{node_id}_cell"),
                _ => format!("n{node_id}_ccol"),
            },
            NodeType::MathBinary | NodeType::MathUnary | NodeType::Remap
            | NodeType::Clamp | NodeType::MixFloat | NodeType::Smoothstep => format!("n{node_id}_res"),
            NodeType::ColorRamp | NodeType::MixRgb | NodeType::BrightnessContrast
            | NodeType::ColorCorrect | NodeType::Invert => format!("n{node_id}_col"),
            NodeType::HeightToNormal => format!("n{node_id}_vec"),
            NodeType::SeparateXyz => match output_idx {
                0 => format!("n{node_id}_x"),
                1 => format!("n{node_id}_y"),
                _ => format!("n{node_id}_z"),
            },
            NodeType::CombineXyz => format!("n{node_id}_vec"),
            // Texture projection
            NodeType::Triplanar => match output_idx {
                0 => format!("n{node_id}_weights"),
                1 => format!("vec3(n{node_id}_uvx, 0.0)"),
                2 => format!("vec3(n{node_id}_uvy, 0.0)"),
                _ => format!("vec3(n{node_id}_uvz, 0.0)"),
            },
            NodeType::BoxMapping => match output_idx {
                0 => format!("n{node_id}_uv"),
                _ => format!("n{node_id}_face"),
            },
            NodeType::CylindricalMapping | NodeType::SphericalMapping
            | NodeType::PlanarMapping | NodeType::UVTransform => format!("n{node_id}_uv"),
            // Texture patterns
            NodeType::TextureSample => match output_idx {
                0 => format!("n{node_id}_col"),
                _ => format!("n{node_id}_alpha"),
            },
            NodeType::GradientTexture => match output_idx {
                0 => format!("n{node_id}_val"),
                _ => format!("n{node_id}_col"),
            },
            NodeType::WaveTexture => match output_idx {
                0 => format!("n{node_id}_val"),
                _ => format!("n{node_id}_col"),
            },
            NodeType::MusgraveTexture => format!("n{node_id}_val"),
            NodeType::BrickTexture => match output_idx {
                0 => format!("n{node_id}_col"),
                _ => format!("n{node_id}_fac"),
            },
            NodeType::Checker => match output_idx {
                0 => format!("n{node_id}_col"),
                _ => format!("n{node_id}_fac"),
            },
            NodeType::Gamma => format!("n{node_id}_col"),
            NodeType::MaterialOutput => "0.0".into(),
            NodeType::ConstantColor => format!("n{node_id}_col"),
            NodeType::ImageTexture => match output_idx {
                0 => format!("n{node_id}_col"),
                _ => format!("n{node_id}_alpha"),
            },
            NodeType::ConstantValue => format!("n{node_id}_val"),
            NodeType::Curvature
            | NodeType::Cavity
            | NodeType::AmbientOcclusion
            | NodeType::Slope
            | NodeType::PrimSdfMask
            | NodeType::SdfCurvature
            | NodeType::SdfThickness => format!("n{node_id}_val"),
            // Output 0 = shaped Mask (post `emit_mask_mul_f32`), output 1 = raw dot(n, axis).
            NodeType::SurfaceDirection => match output_idx {
                1 => format!("n{node_id}_dot"),
                _ => format!("n{node_id}_val"),
            },
            NodeType::SdfPosition => match output_idx {
                1 => format!("n{node_id}_x"),
                2 => format!("n{node_id}_y"),
                3 => format!("n{node_id}_z"),
                _ => format!("n{node_id}_col"),
            },
            NodeType::VectorMath => match output_idx {
                0 => format!("n{node_id}_vec"),
                _ => format!("n{node_id}_val"),
            },
        }
    }

    /// Topological sort from output node backwards (DFS post-order).
    /// `pub(crate)` so the PR-VIS bake path can evaluate graphs on the CPU without re-parsing WGSL.
    pub(crate) fn topo_sort(&self, output_id: NodeId) -> Option<Vec<NodeId>> {
        let mut visited = HashMap::new();
        let mut order = Vec::new();

        // Walk all nodes reachable from output
        fn visit(
            graph: &NodeGraph, nid: NodeId,
            visited: &mut HashMap<NodeId, u8>, order: &mut Vec<NodeId>,
        ) -> bool {
            match visited.get(&nid) {
                Some(1) => return false, // cycle detected
                Some(2) => return true,  // already processed
                _ => {}
            }
            visited.insert(nid, 1); // mark in-progress

            // Visit all nodes feeding into this one
            for conn in &graph.connections {
                if conn.to_node == nid {
                    if !visit(graph, conn.from_node, visited, order) {
                        return false;
                    }
                }
            }

            visited.insert(nid, 2); // mark done
            order.push(nid);
            true
        }

        if !visit(self, output_id, &mut visited, &mut order) {
            return None; // cycle
        }
        Some(order)
    }

    fn fallback_wgsl() -> String {
        "fn eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {
    return eval_material(hit_prim, p, n);
}".to_string()
    }

    /// Compile this graph to a per-prim helper function named `eval_node_mat_{prim_idx}`.
    /// Returns the function body WGSL. Used by multi-graph uber-function builder.
    pub fn compile_per_prim_wgsl(&self, prim_idx: usize) -> Option<String> {
        let output = self.output_node()?.id;
        let order = self.topo_sort(output)?;

        let mut lines = Vec::new();
        let fn_name = format!("eval_node_mat_{}", prim_idx);
        for &node_id in &order {
            if matches!(self.find_node(node_id)?.node_type, NodeType::HeightToNormal) {
                self.emit_input_sampler(node_id, 0, false, &mut lines);
            }
        }
        lines.push(format!("fn {}(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {{", fn_name));
        lines.push("    var result: MatResult;".into());

        for &nid in &order {
            let node = self.find_node(nid)?;
            self.gen_node_wgsl(node, &mut lines);
        }

        // Same Float->Vec3/Color coercion as `compile_wgsl` — see `input_var_name_vec3_coerced`.
        let albedo_var = self.input_var_name_vec3_coerced(output, 0, "vec3(0.66, 0.585, 0.53)");
        let rough_var = self.input_var_name(output, 1, "0.65");
        let metal_var = self.input_var_name(output, 2, "0.0");
        let opacity_var = self.input_var_name(output, 3, "1.0");
        let emission_var = self.input_var_name_vec3_coerced(output, 4, "vec3(0.0, 0.0, 0.0)");

        lines.push(format!("    result.color = {};", albedo_var));
        lines.push(format!("    result.roughness = {};", rough_var));
        lines.push(format!("    result.metallic = {};", metal_var));
        lines.push(format!("    result.opacity = {};", opacity_var));
        lines.push(format!("    result.emission = {};", emission_var));
        lines.push("    return result;".into());
        lines.push("}".into());
        Some(lines.join("\n").replace("sn_input_", &format!("sn_prim_{prim_idx}_input_")))
    }

    /// Build a multi-graph uber-function that dispatches by prim index.
    /// `graphs`: Vec of (prim_flat_index, &NodeGraph) for all prims with active node graphs.
    pub fn build_uber_material_fn(graphs: &[(usize, &NodeGraph)]) -> String {
        if graphs.is_empty() {
            return Self::fallback_wgsl();
        }

        // Compile all graphs once, collecting (prim_idx, wgsl_code) pairs
        let compiled: Vec<(usize, String)> = graphs.iter()
            .filter_map(|(idx, graph)| graph.compile_per_prim_wgsl(*idx).map(|code| (*idx, code)))
            .collect();

        if compiled.is_empty() {
            return Self::fallback_wgsl();
        }

        let mut wgsl = String::new();

        // Emit per-prim helper functions
        for (_, fn_code) in &compiled {
            wgsl.push_str(fn_code);
            wgsl.push('\n');
        }

        // Emit the dispatch function
        wgsl.push_str("fn eval_node_material(hit_prim: u32, p: vec3<f32>, n: vec3<f32>) -> MatResult {\n");
        for (i, (idx, _)) in compiled.iter().enumerate() {
            let kw = if i == 0 { "if" } else { "} else if" };
            wgsl.push_str(&format!("    {} (hit_prim == {}u) {{\n", kw, idx));
            wgsl.push_str(&format!("        return eval_node_mat_{}(hit_prim, p, n);\n", idx));
        }
        wgsl.push_str("    }\n");
        wgsl.push_str("    return eval_material(hit_prim, p, n);\n");
        wgsl.push_str("}\n");
        wgsl
    }
}

// ====================================================================
// Material Presets â€” ONE pipeline-proof graph (old library removed)
// ====================================================================
