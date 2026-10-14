//! P2 — the panel becomes the AUTHOR (MATERIAL_PANEL_SPEC.md §4-P2, C1).
//!
//! App-side commit/mutation layer for the Material panel. Every user-visible
//! edit goes through `transaction_snapshot_only()` (one-shot) or an episode
//! (`begin_episode` on drag start; `auto_commit_episodes()` commits on release)
//! — the exact write-back pattern of the v1 material editor
//! (`properties_material_node.rs:357-378`), including its dirty propagation:
//! `prim_buf_stale + RENDER + outliner_dirty` (materials re-upload, never rebake
//! geometry).
//!
//! Authority (C1): the doc is authored intent; committing a doc lowers it onto
//! the `PbrMaterial` graph node (`lower_to_material`) and mirrors scalars into
//! the material's Master node (`sync_pbr_into_material_graph` = the panel→graph
//! codegen for Value channels: Master socket defaults). `GraphDriven` channels
//! are NEVER written by any path here (`lower_to_material` skips them).
//!
//! Known undo-model limitation (pre-existing, same as v1 "Remove material shader
//! graph"): undo snapshots capture primitives + scene graph, not the `ui.*`
//! stores — Ctrl+Z restores the `PbrMaterial`; the doc follows it at read time
//! via `refresh_doc_values_from_graph` + `heal_doc_fast_texids` (G3 forward reads), so the panel stays truthful.

use core_math::pbr_material::{MapKind, PbrMaterial};
use core_math::primitives::MaterialId;

use super::lower::{
    channel_value_of, derive_doc_from_material, kind_socket, kind_sockets,
    reconcile_doc_with_graph,
};
use super::model::*;
use crate::{AzraApp, DirtyFlags};

/// Pending confirmation for destructive panel actions (spec C1: removing a
/// Layered/GraphDriven channel or reverting a GraphDriven one destroys content
/// → explicit confirmation; Value channels need none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MpConfirm {
    RemoveChannel { mid: u32, kind: ChannelKind },
    RevertChannel { mid: u32, kind: ChannelKind },
    /// P0-4 conflict resolution "let the shader tree own it" — discards the
    /// panel's layer stack for the channel, so it confirms like the others.
    YieldChannelToGraph { mid: u32, kind: ChannelKind },
}

/// M1.b — true when every channel is a plain Value (or GraphDriven badge) with **no**
/// layer stack. Such edits only write `PbrMaterial` factors / Master socket defaults;
/// a map bake is waste (spatially constant). Layered / image / noise stacks need bake.
pub(crate) fn doc_is_scalar_uniform_only(doc: &MaterialPanelDoc) -> bool {
    doc.channels.iter().all(|c| {
        c.layers.is_empty()
            && matches!(
                c.state,
                ChannelState::Value(_) | ChannelState::GraphDriven { .. }
            )
    })
}

/// BakeChannelMask slot ↔ panel channel for the bake-install ownership gate.
/// Order matches `node_graph_bake::BakeChannelMask` fields.
pub(crate) const MASK_KINDS: [ChannelKind; 8] = [
    ChannelKind::BaseColor,
    ChannelKind::Roughness,
    ChannelKind::Metallic,
    ChannelKind::Emission,
    ChannelKind::Normal,
    ChannelKind::Bump,
    ChannelKind::Displacement,
    ChannelKind::SssColor,
];

/// True when the PANEL owns this channel's content (hand-loaded image layer):
/// a graph bake must not install into — or clear — its `TexMap` slot.
/// `GraphDriven`/`Value`/absent doc → graph behavior unchanged.
pub(crate) fn doc_owns_channel(doc: Option<&MaterialPanelDoc>, kind: ChannelKind) -> bool {
    doc.and_then(|d| d.channel(kind))
        .map(|c| matches!(c.state, ChannelState::Layered))
        .unwrap_or(false)
}

impl AzraApp {
    /// The doc the panel shows this frame: stored authored doc (refreshed from
    /// the live material so undo/redo track — see module header), else derived
    /// fresh. Pure read — nothing is inserted into the store here.
    pub(crate) fn mp_current_doc(&mut self, mid: MaterialId, mat: &PbrMaterial) -> MaterialPanelDoc {
        // ── G4: THE DOC IS A PROJECTION OF THE GRAPH ─────────────────────────────
        // When the material has a graph, the panel reads `derive_doc_from_material`
        // (parse_panel_channel + channel_meta + graph-level fields) — memoized on
        // `material_generation`, the trusted invalidation signal (bumped by every
        // material/graph commit and explicitly on undo; NEVER a UI mutation counter,
        // the 2026-08-13 trap). The stored doc remains as (a) migration input for
        // materials without graphs and (b) the async-upload tex_id heal source.
        if self.ui.material_node_graphs.contains_key(&mid.0) {
            let gen = self.scene.scene_graph.material_generation();
            if let Some((g0, doc)) = self.ui.mp_doc_memo.get(&mid.0) {
                if *g0 == gen {
                    return doc.clone();
                }
            }
            let g = self.ui.material_node_graphs.get(&mid.0);
            let mut d = derive_doc_from_material(mat, g);
            // USER-ADDED CHANNELS SURVIVE THE PROJECTION (owner 2026-08-19 "j'ajoute un
            // channel genre SSS color, je ne vois pas le channel apparaître"): the derive
            // keeps only channels that are non-default / mapped / MAIN (`in_use`,
            // lower/actions.rs) — but "the user added it" is INTENT, recorded only in the
            // stored doc, and a freshly added map-less channel (SSS color, IOR, sheen…)
            // is default-valued BY CONSTRUCTION, so the projection silently deleted it on
            // the very next frame. Merge intent back: PRESENCE (+ enabled) from the
            // stored doc, VALUE re-read from the live material — never a stale value.
            // Removal still works: mp_remove_channel commits the stored doc without the
            // channel, so nothing resurrects it here.
            if let Some(stored) = self.ui.material_panel_docs.get(&mid.0) {
                let mut extra: Vec<Channel> = Vec::new();
                for ch in &stored.channels {
                    if d.channel(ch.kind).is_none() {
                        let mut c = Channel::value(ch.kind, channel_value_of(ch.kind, mat));
                        c.enabled = ch.enabled;
                        extra.push(c);
                    }
                }
                d.channels.extend(extra);
            }
            // Async-upload heal: a pending image decode lands in the TexMap before
            // the doc/graph learn its id — adopt-if-zero, never overwrite (G3).
            super::lower::heal_doc_fast_texids(&mut d, mat);
            self.ui.mp_doc_memo.insert(mid.0, (gen, d.clone()));
            return d;
        }
        // No graph yet: the stored doc (or a fresh derive) is still the source.
        match self.ui.material_panel_docs.get(&mid.0) {
            Some(d) => {
                let mut d = d.clone();
                super::lower::heal_doc_fast_texids(&mut d, mat);
                d
            }
            None => derive_doc_from_material(mat, None),
        }
    }

    /// Commit an edited doc: undo entry, persist the doc (FL4MPNLS on first
    /// edit), lower onto the material, mirror scalars into the Master node,
    /// propagate dirty. `drag` = the edit came from a drag start this frame
    /// (episode: ONE undo per drag, committed by `auto_commit_episodes()`).
    pub(crate) fn mp_commit_doc_edit(
        &mut self,
        mid: MaterialId,
        mut doc: MaterialPanelDoc,
        desc: &str,
        drag: bool,
    ) {
        // Owner rule (no UVs → Vertex) + C4 radius floor — applied on every commit
        // so Texture-rung gift-wrap cannot stick on SDF bodies after an edit.
        let (has_uvs, radius_floor) = self.mp_active_body_gen_hints();
        let _ = super::lower::normalize_generator_layers(&mut doc, has_uvs, radius_floor);
        // v1 write-back undo pattern (properties_material_node.rs:358-363).
        if !self.has_active_episode() {
            if drag {
                self.begin_episode(desc);
            } else {
                self.transaction_snapshot_only(desc);
            }
        }
        // M1.b — scalar fast path: Value-only docs (no layer stacks / map content) only
        // write PbrMaterial uniforms + Master socket defaults. Bake is for
        // spatially-varying results. Roughness/metal/tint scrub = RENDER dirty only.
        let scalar_only = doc_is_scalar_uniform_only(&doc);
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            super::lower::lower_to_material(&doc, m);
        }
        // Doc persists from the FIRST edit (FL4MPNLS picks it up on save).
        self.ui.material_panel_docs.insert(mid.0, doc);
        // Panel = Master Material node (one material): Value-channel codegen is
        // the Master socket defaults — same seam the v1 editor uses.
        self.sync_pbr_into_material_graph(mid);
        // G3 WRITE FLIP: the material is DERIVED back from the graph, making the
        // graph the LAST writer of shader_type and every unwired scalar. A socket
        // wired in the node editor now wins STRUCTURALLY (`push_to_pbr` skips
        // connected sockets) instead of by reconcile bookkeeping. TexMap slots are
        // derived outputs (bake installs / Fast image lowering) and are not touched
        // here; the Fast single-image install stays doc-side until G4 puts Fast
        // channels in the graph (`regen` emits Subgraph shapes only today).
        if let (Some(g), Some(m)) = (
            self.ui.material_node_graphs.get(&mid.0),
            self.scene.scene_graph.material_by_id_mut(mid),
        ) {
            g.push_to_pbr(m);
        }
        if scalar_only {
            // No mp_lower_rich / eval-mode bake — uniforms already on the material.
            self.cache.prim_buf_stale = true;
            // M1.c: during a drag episode, keep PT accum; raster shows the factor.
            // Restart PT only when not mid-scrub (release / one-shot).
            if !self.has_active_episode() {
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
            }
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.temporal_frame_valid = false;
            }
            self.mark_dirty(DirtyFlags::RENDER);
            self.outliner.outliner_dirty = true;
            return;
        }
        // P3: rich Layered channels expand to panel-generated node subgraphs;
        // the auto-apply bake composites them into the channel's TexMap slot.
        self.mp_lower_rich_channels(mid);
        // Vertex-rung generators showcase via the per-vertex GPU buffer — drop any
        // stale planar bake map so the gift-wrap cross cannot fight the vertex tint.
        self.mp_clear_vertex_rung_texture_maps(mid);
        // Live vs Bake is a **user choice** — apply after lower so bake/noise agree.
        self.mp_apply_eval_modes(mid);
        // v1 dirty propagation (properties_material_node.rs:375-377).
        self.cache.prim_buf_stale = true;
        // Material scalars (roughness/metal) land in the raster G-buffer + deferred.
        // Invalidate temporal history so a static camera cannot keep a pre-edit HDR frame
        // (owner: "roughness only after PathTrace round-trip" — PT forced continuous dirty).
        // M1.c: mid-episode map scrub keeps PT accum (raster preview); settle restarts.
        if !self.has_active_episode() {
            self.render.pt_accumulated_frames = 0;
            self.render.pt_converged = false;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.temporal_frame_valid = false;
        }
        self.mark_dirty(DirtyFlags::RENDER);
        self.outliner.outliner_dirty = true;
    }

    /// Active-body UV presence + C4 radius floor — same signals as the panel's `GenCtx`.
    pub(crate) fn mp_active_body_gen_hints(&self) -> (bool, Option<f32>) {
        let nid = self.scene.active_body_nid;
        let has_uvs = self
            .cache
            .poly_custom_data
            .get(&nid)
            .map_or(false, |cd| !cd.uv.is_empty());
        let radius_floor = self
            .scene
            .scene_graph
            .body_primitives(nid)
            .filter(|pr| !pr.is_empty())
            .map(|pr| {
                let mut src = crate::node_graph_bake::SdfFieldSources::default();
                src.body_prims = pr.to_vec();
                let (bmin, bmax) = src.body_aabb(0);
                super::lower::curvature_radius_floor_aabb(bmin, bmax)
            });
        (has_uvs, radius_floor)
    }

    /// Silent auto-migrate: force Vertex rung + radius floor when the body has no
    /// UVs, re-lower so node labels get the `v` flag, and clear gift-wrap maps.
    /// No undo entry (repairs broken authored state; user never opted into Texture
    /// as a meaningful choice on SDF). Returns true when something changed.
    pub(crate) fn mp_ensure_vertex_curvature_path(&mut self, mid: MaterialId) -> bool {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
            return false;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        let (has_uvs, radius_floor) = self.mp_active_body_gen_hints();
        let normalized = super::lower::normalize_generator_layers(&mut doc, has_uvs, radius_floor);
        let stale_map = doc.channels.iter().any(|c| {
            super::lower::channel_skip_planar_map_install(c)
                && c.kind
                    .map_kind()
                    .map(|mk| mat.map(mk).is_set())
                    .unwrap_or(false)
        });
        if !normalized && !stale_map {
            return false;
        }
        if normalized {
            self.ui.material_panel_docs.insert(mid.0, doc);
        } else {
            // Stale planar map under pure-Vertex doc — keep/store so clear + re-lower run.
            self.ui.material_panel_docs.entry(mid.0).or_insert(doc);
        }
        // Always re-lower when migrating: node labels need the trailing `v` flag for
        // `vertex_curv_keys_for_graph` / `poll_vertex_curvature`. A no-op lower is cheap.
        self.mp_lower_rich_channels(mid);
        self.mp_clear_vertex_rung_texture_maps(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        true
    }

    /// P2: no-op. Formerly cleared maps for pure Vertex-rung channels so G-buffer could
    /// own κ; bake is now the single compositor and those maps must stay installed.
    pub(crate) fn mp_clear_vertex_rung_texture_maps(&mut self, mid: MaterialId) {
        let Some(doc) = self.ui.material_panel_docs.get(&mid.0) else {
            return;
        };
        let clear: Vec<(ChannelKind, MapKind)> = doc
            .channels
            .iter()
            .filter(|c| super::lower::channel_skip_planar_map_install(c))
            .filter_map(|c| c.kind.map_kind().map(|mk| (c.kind, mk)))
            .collect();
        if clear.is_empty() {
            return;
        }
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            for (kind, mk) in clear {
                {
                    let map = m.map_mut(mk);
                    map.tex_id = 0;
                    map.strength = 0.0;
                    map.absolute = false;
                    map.high_precision = false;
                }
                if kind == ChannelKind::BaseColor {
                    // Pure white paint: G-buffer Normal/Mult composites black↔white by vertex κ.
                    m.albedo = glam::Vec3::ONE;
                    // Live GPU noise REPLACE would fight the vertex composite — kill it when
                    // this channel is pure-Vertex (no texture bake content).
                    m.noise_pattern.enabled = false;
                    m.noise_pattern.amount = 0.0;
                }
            }
        }
    }

    /// "+ Add channel": append the kind, seeded with the engine's CURRENT value
    /// for that channel (so the panel and render agree immediately). One-shot.
    /// Resolve the layer index a library-dropped texture should land on, CREATING an Image layer
    /// when the channel has none (owner 2026-07-28: drag an image onto a channel -> image layer).
    ///
    /// Reuses the TOPMOST existing Image layer so dropping again onto the same channel REPLACES
    /// the image instead of stacking near-identical layers — which is what "drop a texture here"
    /// means to a user, and matches how the Load… button behaves.
    ///
    /// A `Value` channel is promoted to `Layered`, otherwise the new layer would sit in a state
    /// that does not render layers and the drop would appear to do nothing.
    pub(crate) fn mp_ensure_image_layer_for_drop(
        &mut self,
        mid: MaterialId,
        kind: ChannelKind,
    ) -> usize {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
            return 0;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == kind) else { return 0 };
        ch.state = ChannelState::Layered;
        // Topmost image layer wins (layers are bottom-up, so scan from the end).
        if let Some(i) = ch
            .layers
            .iter()
            .rposition(|l| matches!(l.content, LayerContent::Image { .. }))
        {
            // No doc commit needed — nothing changed except possibly `state`, and the pending
            // load will commit when the decoded slot lands.
            let idx = i;
            self.ui.material_panel_docs.insert(mid.0, doc);
            return idx;
        }
        ch.layers.push(Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 0, strength: 1.0 },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement::default(),
            adjust: Adjust::default(),
        });
        let idx = ch.layers.len() - 1;
        self.ui.material_panel_docs.insert(mid.0, doc);
        idx
    }

    /// Copy ONE layer from a channel into another EXISTING channel's stack (owner 2026-07-28:
    /// "if I need a curvature in metalness I can duplicate it from base color").
    ///
    /// Unlike `mp_duplicate_channel_to`, the destination must already exist — this appends a
    /// layer to a stack rather than creating a channel — and the source keeps its layer.
    /// The layer lands on TOP of the destination stack, which is where a newly added layer goes
    /// and therefore where the user expects to find it.
    pub(crate) fn mp_copy_layer_to_channel(
        &mut self,
        mid: MaterialId,
        from: ChannelKind,
        li: usize,
        to: ChannelKind,
    ) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        if to.map_kind().is_none() {
            return; // cannot bake there — refuse rather than silently no-op
        }
        let Some(layer) = doc.channel(from).and_then(|c| c.layers.get(li)).cloned() else {
            return;
        };
        let Some(dst) = doc.channels.iter_mut().find(|c| c.kind == to) else { return };
        // A Value channel has no stack to append to; promote it so the copy is visible rather
        // than being dropped into a state that does not render layers.
        dst.state = ChannelState::Layered;
        dst.layers.push(layer);
        self.mp_commit_doc_edit(mid, doc, "Duplicate layer to channel", false);
    }

    /// Copy a channel's whole layer stack into another, currently-absent kind
    /// (owner 2026-07-28: "duplicate channel"). The source is left untouched.
    ///
    /// Refuses when the target already exists: `MaterialPanelDoc::channel()` resolves by
    /// `.find()` (FIRST match wins), so a second channel of the same kind would be invisible to
    /// every lookup and would fight the first for the single TexMap slot that kind owns. The menu
    /// already filters those out; this is the belt-and-braces guard for any other caller.
    pub(crate) fn mp_duplicate_channel_to(
        &mut self,
        mid: MaterialId,
        from: ChannelKind,
        to: ChannelKind,
    ) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        if doc.channel(to).is_some() || to.map_kind().is_none() {
            return;
        }
        let Some(src) = doc.channel(from).cloned() else { return };
        // The layer stack is kind-agnostic (content + mask + blend + opacity + placement), so it
        // transfers verbatim. Only `kind` changes; `bit_depth` follows the TARGET's convention
        // rather than the source's, since 16-bit is a Displacement/Bump concern, not a property
        // of the layers being copied.
        let mut dup = src.clone();
        dup.kind = to;
        dup.bit_depth = ChannelBitDepth::default_for(to);
        doc.channels.push(dup);
        self.mp_commit_doc_edit(mid, doc, "Duplicate material channel", false);
    }

    /// Retarget a channel to another kind, MOVING its stack (owner: "change channel type").
    /// Same guards as duplicate; the source channel becomes the target rather than being copied.
    pub(crate) fn mp_retype_channel(
        &mut self,
        mid: MaterialId,
        from: ChannelKind,
        to: ChannelKind,
    ) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        if doc.channel(to).is_some() || to.map_kind().is_none() {
            return;
        }
        let Some(c) = doc.channels.iter_mut().find(|c| c.kind == from) else { return };
        c.kind = to;
        c.bit_depth = ChannelBitDepth::default_for(to);
        self.mp_commit_doc_edit(mid, doc, "Change material channel type", false);
    }

    pub(crate) fn mp_add_channel(&mut self, mid: MaterialId, kind: ChannelKind) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        if doc.channel(kind).is_some() {
            return;
        }
        doc.channels.push(Channel::value(kind, channel_value_of(kind, &mat)));
        self.mp_commit_doc_edit(mid, doc, "Add material channel", false);
    }

    /// Remove a channel after any needed confirmation (view gates Layered /
    /// GraphDriven through `MpConfirm`). Layered → clears the map slot;
    /// GraphDriven → prunes the channel's subgraph (same surgery as revert).
    pub(crate) fn mp_remove_channel_confirmed(&mut self, mid: MaterialId, kind: ChannelKind) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        let Some(pos) = doc.channels.iter().position(|c| c.kind == kind) else { return };
        let state = doc.channels[pos].state;
        doc.channels.remove(pos);
        self.transaction_snapshot_only("Remove material channel");
        if matches!(state, ChannelState::GraphDriven { .. }) {
            self.mp_prune_channel_subgraph(mid, kind);
        }
        if matches!(state, ChannelState::Layered | ChannelState::GraphDriven { .. }) {
            if let Some(mk) = kind.map_kind() {
                if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                    m.map_mut(mk).tex_id = 0;
                }
            }
        }
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            super::lower::lower_to_material(&doc, m);
        }
        self.ui.material_panel_docs.insert(mid.0, doc);
        self.sync_pbr_into_material_graph(mid);
        // P3: drop the removed channel's panel-generated subgraph, if any.
        self.mp_lower_rich_channels(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        self.outliner.outliner_dirty = true;
    }

    /// "Revert to panel control" (spec C1, confirmed): discard the channel's
    /// hand-authored subgraph (disconnect its Master sockets + delete nodes
    /// reachable only through them), clear its baked map slot, and return the
    /// doc state to `Value` from the live material.
    pub(crate) fn mp_revert_channel_confirmed(&mut self, mid: MaterialId, kind: ChannelKind) {
        self.transaction_snapshot_only("Revert channel to panel control");
        self.mp_prune_channel_subgraph(mid, kind);
        // The graph no longer owns the slot — drop its baked map.
        if let Some(mk) = kind.map_kind() {
            if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                let map = m.map_mut(mk);
                map.tex_id = 0;
                map.strength = 1.0;
            }
        }
        // Doc: back to a live Value slider seeded from the material.
        if let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone()) {
            let mut doc = self
                .ui
                .material_panel_docs
                .get(&mid.0)
                .cloned()
                .unwrap_or_else(|| {
                    derive_doc_from_material(&mat, self.ui.material_node_graphs.get(&mid.0))
                });
            if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == kind) {
                ch.state = ChannelState::Value(channel_value_of(kind, &mat));
                ch.layers.clear();
            }
            self.ui.material_panel_docs.insert(mid.0, doc);
        }
        self.ui.mp_bake_conflicts.remove(&mid.0);
        // Channels the graph STILL drives keep their bake path warm.
        self.enqueue_material_node_graph_bake(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        self.outliner.outliner_dirty = true;
        self.toast("Channel reverted to panel control");
    }

    /// Apply `Channel.bit_depth` to the live `TexMap.high_precision` flag for any map channel.
    /// Available in **both** Live and Bake modes. Bit16 → dual-plane reconstruct; Bit8 → hi only.
    /// Always re-bakes so the user sees the precision change (not a no-op flag flip).
    pub(crate) fn mp_apply_channel_bit_depth(&mut self, mid: MaterialId, kind: ChannelKind) {
        let Some(mk) = kind.map_kind() else { return };
        let bit16 = self
            .ui
            .material_panel_docs
            .get(&mid.0)
            .and_then(|d| d.channel(kind))
            .map(|c| c.bit_depth == ChannelBitDepth::Bit16)
            .unwrap_or(true);
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            let map = m.map_mut(mk);
            if map.is_set() {
                map.high_precision = bit16;
            }
        }
        // Force map re-bake at current Bake target so 8↔16 is visible (dual-plane install).
        self.enqueue_material_node_graph_bake(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// Apply user Live/Bake choices after a doc commit.
    ///
    /// **Map bake pacing (owner Fill-color lag):** panel stack edits (Fill color, Mult noise,
    /// etc.) always schedule a **Live** (fast, coalesced) map bake — never a hard Final enqueue
    /// here. Final auto-upgrades when Live lands and scrubbing has settled
    /// (`poll_graph_bakes`). Forcing Final on every color tick cancelled Live coalesce and made
    /// even Bake 256² feel laggy.
    ///
    /// Channel **Live** mode: try GPU noise first; if the stack is not live-capable, same Live
    /// map bake path as Bake mode.
    pub(crate) fn mp_apply_eval_modes(&mut self, mid: MaterialId) {
        let Some(doc) = self.ui.material_panel_docs.get(&mid.0).cloned() else {
            return;
        };
        let albedo_live = doc
            .channel(ChannelKind::BaseColor)
            .map(|c| c.eval_mode == ChannelEvalMode::Live)
            .unwrap_or(false);
        // Any map-slot channel with a layer stack (or GraphDriven) needs the map bake path.
        // Includes Roughness Mult noise even when Base Color is Live-only GPU noise.
        let any_map_bake = doc.channels.iter().any(|c| {
            c.kind.map_kind().is_some()
                && (c.eval_mode == ChannelEvalMode::Bake || !c.layers.is_empty())
                && (!c.layers.is_empty()
                    || matches!(c.state, ChannelState::Layered | ChannelState::GraphDriven { .. }))
        });

        let mut need_map_bake = any_map_bake;
        if albedo_live {
            // Prefer same-frame GPU noise when the graph is simple Noise→Albedo.
            // If that succeeds but other channels (Roughness Mult, etc.) still need maps,
            // still schedule a map bake — GPU live only owns albedo.
            if self.apply_live_gpu_noise_material(mid) {
                // Live GPU handled albedo; still bake if roughness/metal/… have stacks.
                need_map_bake = doc.channels.iter().any(|c| {
                    c.kind != ChannelKind::BaseColor
                        && c.kind.map_kind().is_some()
                        && (!c.layers.is_empty()
                            || matches!(
                                c.state,
                                ChannelState::Layered | ChannelState::GraphDriven { .. }
                            ))
                });
            }
        }

        if need_map_bake {
            // Map bake owns non-live channels — kill albedo live REPLACE so Mult maps show.
            if !albedo_live {
                if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                    m.noise_pattern.enabled = false;
                    m.noise_pattern.amount = 0.0;
                }
            }
            // Live quality while scrubbing / after each edit; Final on settle (auto-upgrade).
            self.schedule_live_material_graph_bake(mid);
        }
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
    }

    /// P0-4 resolution A — "Let the shader tree own it". The conflict exists
    /// because `gate_mask_by_panel_ownership` skips a bake install into a channel
    /// whose doc state is `Layered` (a hand-loaded map). Dropping that layer
    /// stack back to a plain `Value` makes `doc_owns_channel` false, so the gate
    /// stops firing and the NEXT bake installs normally.
    ///
    /// No new ownership semantics: this only moves the doc to a state the gate
    /// already understands, then re-enqueues the existing bake.
    pub(crate) fn mp_conflict_yield_to_graph(&mut self, mid: MaterialId, kind: ChannelKind) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        // Destructive (drops the layer stack) → its own undo entry.
        self.transaction_snapshot_only("Let shader tree drive channel");
        let mut doc = self.mp_current_doc(mid, &mat);
        if let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == kind) {
            ch.state = ChannelState::Value(channel_value_of(kind, &mat));
            ch.layers.clear();
        }
        // Clear the panel's map slot: the bake is about to install its own, and a
        // stale hand-loaded id would show until it lands.
        if let Some(mk) = kind.map_kind() {
            if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                m.map_mut(mk).tex_id = 0;
            }
        }
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            super::lower::lower_to_material(&doc, m);
        }
        self.ui.material_panel_docs.insert(mid.0, doc);
        self.sync_pbr_into_material_graph(mid);
        // Drop this channel's conflict bit; the re-bake recomputes the rest.
        self.mp_clear_conflict_bit(mid, kind);
        self.mp_lower_rich_channels(mid);
        self.enqueue_material_node_graph_bake(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        self.outliner.outliner_dirty = true;
        self.toast("Shader tree now drives this channel");
    }

    /// P0-4 resolution B — "Keep my map". The gate ALREADY skipped the bake, so
    /// the panel's map is what renders; the only thing to do is stop nagging.
    /// Non-destructive → no undo entry (nothing about the scene changes; this
    /// clears a transient UI advisory bit, same class as `mp_bake_conflicts`
    /// being recomputed on the next bake).
    pub(crate) fn mp_conflict_keep_panel(&mut self, mid: MaterialId, kind: ChannelKind) {
        self.mp_clear_conflict_bit(mid, kind);
        self.toast("Keeping the panel's map for this channel");
    }

    /// Clear ONE channel's bit in the material's conflict mask, dropping the
    /// entry entirely when no conflicts remain (matches what the bake writes).
    fn mp_clear_conflict_bit(&mut self, mid: MaterialId, kind: ChannelKind) {
        if let Some(mask) = self.ui.mp_bake_conflicts.get_mut(&mid.0) {
            *mask &= !(1u32 << (kind as u32));
            if *mask == 0 {
                self.ui.mp_bake_conflicts.remove(&mid.0);
            }
        }
    }

    /// Shared graph surgery: prune every Master socket the channel owns from the
    /// stored graph AND the open editor's working copy (one material, two views).
    pub(crate) fn mp_prune_channel_subgraph(&mut self, mid: MaterialId, kind: ChannelKind) {
        if let Some(g) = self.ui.material_node_graphs.get_mut(&mid.0) {
            for &sk in kind_sockets(kind) {
                g.prune_master_input(sk);
            }
        }
        if matches!(self.ui.node_graph_target, Some(crate::NodeGraphTarget::Material(m)) if m == mid)
        {
            for &sk in kind_sockets(kind) {
                self.ui.node_graph.prune_master_input(sk);
            }
        }
    }

    /// P8 — THE ONE framing entry point. Open (or focus) the shader tree bound to
    /// this material, then frame + select the relevant node. Every panel→graph
    /// navigation (header button, connection dots, "Edit in shader tree" context
    /// menu) routes through here so there is never an unframed open and never a
    /// second window.
    ///
    /// Target resolution:
    /// - `None` (header / master) → the MaterialOutput (Master) node.
    /// - `Some(kind)` → the node feeding that channel's PRIMARY master socket
    ///   (`kind_socket`): for a GraphDriven channel this is the head of its
    ///   subgraph; for a panel-generated channel (mp:* tagged) it is that
    ///   subgraph's head (the node wired into master); for a plain-value channel
    ///   (nothing wired) it degrades to the Master node so the user still lands on
    ///   the socket. Reuses the same socket→source trace `prune_master_input`
    ///   keys off (Connection.to_node == master && to_input == kind_socket).
    pub(crate) fn mp_frame_shader_node(
        &mut self,
        mid: MaterialId,
        channel: Option<ChannelKind>,
    ) {
        // Single-window: this seeds/persists the graph, sets the target, and shows
        // the editor. If it is already open on this material it simply re-seeds
        // (idempotent) — never a second window (there is one node_graph + one
        // show_node_editor bool).
        self.open_material_shader_graph(mid);

        // Resolve on the now-live graph (open_material_shader_graph copied the
        // stored/seeded graph into self.ui.node_graph).
        let g = &self.ui.node_graph;
        let master_id = g.output_node().map(|n| n.id);
        let target_node = match channel {
            None => master_id,
            Some(kind) => {
                let sock = kind_socket(kind);
                let src = master_id.and_then(|mid_node| {
                    g.connections
                        .iter()
                        .find(|c| c.to_node == mid_node && c.to_input == sock)
                        .map(|c| c.from_node)
                });
                // Plain-value channel (unwired socket) → land on the Master node.
                src.or(master_id)
            }
        };
        self.ui.node_drag_state.frame_request = target_node;
    }

    /// PROMOTION CHOKE POINT companion — reconcile the stored doc's per-channel
    /// ownership with the material's graph. Called wherever the material graph
    /// changes: `commit_material_node_graph_edit` (hand edits in the node
    /// editor), `enqueue_material_node_graph_bake_quality` (Apply / presets /
    /// live bakes). Absent docs are left absent (they derive live and already
    /// reflect the graph); a doc missing from the store costs nothing here.
    pub(crate) fn mp_reconcile_graph_ownership(&mut self, mid: MaterialId) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        let graph = self.ui.material_node_graphs.get(&mid.0);
        if let Some(doc) = self.ui.material_panel_docs.get(&mid.0) {
            let mut d = doc.clone();
            if reconcile_doc_with_graph(&mut d, &mat, graph) {
                self.ui.material_panel_docs.insert(mid.0, d);
            }
        }
    }

    /// True when the PANEL owns this material channel's map slot (hand-loaded
    /// image layer) — bake installs and unwired-socket map clearing must skip it.
    pub(crate) fn mp_panel_owns_map(&self, mid: u32, mk: MapKind) -> bool {
        doc_owns_channel(
            self.ui.material_panel_docs.get(&mid),
            ChannelKind::from_map_kind(mk),
        )
    }

    /// A HAND-LOADED image landed for a Graph material (file dialog → poll).
    /// Record panel ownership: ensure a stored doc and mark the channel Layered
    /// with a single image layer (tex_id syncs from the TexMap once the upload
    /// applies — `heal_doc_fast_texids`). Without this record the bake
    /// ownership gate has nothing to defend.
    pub(crate) fn mp_mark_hand_loaded_map(
        &mut self,
        mid: MaterialId,
        kind: MapKind,
        strength: f32,
    ) {
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        let mut doc = self.mp_current_doc(mid, &mat);
        let ckind = ChannelKind::from_map_kind(kind);
        let ts = doc.texture_space;
        // P3: a per-LAYER load is in flight for this channel → the slot id is
        // routed into that layer by `mp_apply_layer_load` (apply_map_slot);
        // replacing the stack here would destroy the other layers.
        if matches!(self.ui.mp_pending_layer_load, Some((m, k, _, _)) if m == mid.0 && k == ckind)
        {
            return;
        }
        // DIAGNOSTIC (`AZRA_MASK_WHY=1`) — owner 2026-07-29: "Add mask -> Image still appears as
        // an image layer".
        //
        // Reaching HERE means the guard above did NOT fire, so this pushes a brand-new Image
        // LAYER. For a mask load that is the bug: the mask menu sets `mp_pending_layer_load` with
        // is_mask=true, and `poll_file_dialogs` (app_poll.rs:88) runs BEFORE `apply_map_slot`
        // (:151), so this function sees the pending entry first and should bail. If it does not,
        // the pending entry is missing or names a different channel by the time the picked file
        // comes back — and this line silently converts an intended mask into a layer.
        //
        // Logs the pending state so the mismatch is named rather than inferred.
        if std::env::var("AZRA_MASK_WHY").as_deref() == Ok("1") {
            eprintln!(
                "[MaskWhy] mp_mark_hand_loaded_map PUSHING a new Image layer for {:?}/{:?} —                  pending={:?} (a mask load should have bailed at the guard above)",
                mid, ckind, self.ui.mp_pending_layer_load);
        }
        let layer = Layer {
            id: 0,
            drives: Vec::new(),
            content: LayerContent::Image { tex_id: 0, strength },
            mask: MaskStack::default(),
            blend: BlendMode::Normal,
            opacity: 1.0,
            visible: true,
            placement: Placement { space: Some(ts) },
            adjust: Adjust::default(),
        };
        match doc.channels.iter_mut().find(|c| c.kind == ckind) {
            Some(ch) => {
                // A GraphDriven channel keeps graph ownership — the user loaded
                // a map that the graph's next bake would overwrite; don't lie.
                if matches!(ch.state, ChannelState::GraphDriven { .. }) {
                    return;
                }
                ch.state = ChannelState::Layered;
                ch.layers = vec![layer];
            }
            None => doc.channels.push(Channel::with_state(
                ckind,
                ChannelState::Layered,
                vec![layer],
            )),
        }
        // Store WITHOUT lowering: the pending upload writes the TexMap itself
        // (apply_map_slot); lowering now would zero the still-unassigned tex_id.
        self.ui.material_panel_docs.insert(mid.0, doc);
    }

    /// P3 — expand every RICH Layered channel of the stored doc into its
    /// panel-generated node subgraph (deterministic codegen,
    /// `lower::regen_channel_subgraph`), strip subgraphs for channels that are
    /// no longer rich, and enqueue the auto-apply bake when anything is wired.
    /// Mirrors into the open node editor's working copy (one material, two
    /// views — same pattern as `mp_prune_channel_subgraph`).
    pub(crate) fn mp_lower_rich_channels(&mut self, mid: MaterialId) {
        self.mp_lower_rich_channels_inner(mid, true);
    }

    /// G4 LOAD MIGRATION (R2): sync doc → graph WITHOUT enqueueing bakes. Spawning
    /// N full bakes on load was a measured freeze risk — the load path itself bakes
    /// at most ONE material (project_load.rs) and FL4MATTX usually restores pixels.
    pub(crate) fn mp_sync_doc_to_graph(&mut self, mid: MaterialId) {
        self.mp_lower_rich_channels_inner(mid, false);
    }

    fn mp_lower_rich_channels_inner(&mut self, mid: MaterialId, bake: bool) {
        use super::lower::{
            channel_uses_vertex_curvature, panel_generated_mask, regen_channel_subgraph,
            strip_channel_subgraph,
        };
        let Some(mut doc) = self.ui.material_panel_docs.get(&mid.0).cloned() else { return };
        // P1: stable layer ids before tagging (mp:kind:role:id).
        doc.ensure_layer_ids();
        self.ui.material_panel_docs.insert(mid.0, doc.clone());
        let Some(mat) = self.scene.scene_graph.material_by_id(mid).map(|(_, m)| m.clone())
        else {
            return;
        };
        // Need subgraph codegen when:
        //   • Layered channel lowers to Subgraph (noise/images/generators), OR
        //   • Any channel still carries Vertex-rung layers (state may be GraphDriven
        //     after earlier ownership reconcile — still need `…:sgv` tags for poll).
        // ONE testable decider (layers.rs) — includes the eye-toggle gate
        // (MPEYEOFFREBAKE): disabled channels strip, so no bake can re-install them.
        let needs_codegen =
            |c: &Channel| -> bool { super::lower::channel_needs_codegen(c, doc.texture_space) };
        let any_rich = doc.channels.iter().any(needs_codegen);
        let had_graph = self.ui.material_node_graphs.contains_key(&mid.0);
        if !any_rich && !had_graph {
            // Still remember bake target for when layers appear later.
            return;
        }
        let mut g = self
            .ui
            .material_node_graphs
            .get(&mid.0)
            .cloned()
            .unwrap_or_default();
        // Snapshot for the no-op check (Node has no PartialEq — JSON compare;
        // panel graphs are small).
        let orig_snap = serde_json::to_string(&(&g.nodes, &g.connections)).unwrap_or_default();
        let before = panel_generated_mask(&g);
        let mut after = 0u32;
        for ch in &doc.channels {
            if needs_codegen(ch) {
                if regen_channel_subgraph(&mut g, ch.kind, ch, doc.texture_space, &mat) {
                    after |= 1u32 << (ch.kind as u32);
                }
            } else {
                strip_channel_subgraph(&mut g, ch.kind);
            }
        }
        // Kinds absent from the doc entirely (removed channels) lose their nodes.
        for &kind in super::lower::ALL_KINDS.iter() {
            if doc.channel(kind).is_none() {
                strip_channel_subgraph(&mut g, kind);
            }
        }
        // Always write the regenerated graph (params live on node input_defaults).
        // Skipping store on "snap equal" left bakes reading a stale store / open editor
        // graph — noise Scale/Fill never reached the worker until 8/16 forced a Final.
        let new_snap = serde_json::to_string(&(&g.nodes, &g.connections)).unwrap_or_default();
        let mutated = new_snap != orig_snap;
        let vertex_keys_missing = any_rich
            && doc.channels.iter().any(channel_uses_vertex_curvature)
            && crate::node_graph_bake::vertex_curv_keys_for_graph(&g).is_empty();
        let want_res = super::model::snap_panel_bake_res(doc.bake_res);
        let res_changed = self
            .ui
            .material_node_graphs
            .get(&mid.0)
            .map(|og| og.bake_res != want_res)
            .unwrap_or(true);
        g.bake_res = want_res;
        // G4: doc-level authored fields ride the graph (the projection reads them back).
        g.texture_space = doc.texture_space;
        g.generator_mode = doc.generator_mode;
        let _ = any_rich;
        // Always persist panel codegen (Fill/Noise params) into the store.
        self.ui.material_node_graphs.insert(mid.0, g.clone());
        if matches!(self.ui.node_graph_target, Some(crate::NodeGraphTarget::Material(m)) if m == mid)
        {
            // Keep the open editor in sync so editor-preferred knobs match the panel.
            self.ui.node_graph = g;
        }
        // Texture bake when Master sockets are driven. Pure-vertex-only: no map bake.
        let needs_map_bake = self
            .ui
            .material_node_graphs
            .get(&mid.0)
            .map(|g| crate::node_graph_bake::bake_channels_from_graph(g, Some(&doc)))
            .map(|m| m.any())
            .unwrap_or(false);
        let _ = (after, before, mutated, vertex_keys_missing);
        if bake && needs_map_bake {
            // Live bake for all panel stack param edits (scale, fill, opacity…).
            // Final only for explicit Bake N² change when not mid-scrub.
            if res_changed && !self.has_active_episode() {
                self.enqueue_material_node_graph_bake(mid);
            } else {
                self.schedule_live_material_graph_bake(mid);
            }
        }
        // Vertex-rung channels must never keep a planar map (gift-wrap).
        self.mp_clear_vertex_rung_texture_maps(mid);
    }

    /// P3 — a per-layer "Load…" completed: the decoded image now owns texture
    /// array `slot`. Route the id into the pending layer, then re-lower + bake.
    /// Called from `apply_map_slot` (MapTarget::Graph) right after the TexMap
    /// write. Returns true when the load was claimed by a layer.
    pub(crate) fn mp_apply_layer_load(&mut self, mid: MaterialId, kind: MapKind, slot: u32) -> bool {
        let ckind = ChannelKind::from_map_kind(kind);
        let Some((pm, pk, li, is_mask)) = self.ui.mp_pending_layer_load else { return false };
        if pm != mid.0 || pk != ckind {
            return false;
        }
        self.ui.mp_pending_layer_load = None;
        let Some(doc) = self.ui.material_panel_docs.get_mut(&mid.0) else { return false };
        let Some(ch) = doc.channels.iter_mut().find(|c| c.kind == ckind) else { return false };
        let Some(layer) = ch.layers.get_mut(li) else { return false };
        if is_mask {
            // Route the decoded slot into the layer's MASK image.
            if let Some(LayerContent::Image { tex_id, .. }) = layer.mask.primary_content_mut() {
                *tex_id = slot;
            } else {
                return false;
            }
        } else if let LayerContent::Image { tex_id, .. } = &mut layer.content {
            *tex_id = slot;
        } else {
            return false;
        }
        self.mp_lower_rich_channels(mid);
        self.cache.prim_buf_stale = true;
        self.mark_dirty(DirtyFlags::RENDER);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material_panel::model::{
        Adjust, BlendMode, Channel, ChannelKind, ChannelState, ChannelValue, Layer, LayerContent,
        MaskStack, MaterialPanelDoc, Placement,
    };

    fn empty_doc() -> MaterialPanelDoc {
        MaterialPanelDoc::default()
    }

    #[test]
    fn m1b_value_only_doc_is_scalar_uniform() {
        let mut doc = empty_doc();
        doc.channels = vec![
            Channel::value(ChannelKind::Roughness, ChannelValue::Scalar(0.4)),
            Channel::value(ChannelKind::Metallic, ChannelValue::Scalar(0.1)),
        ];
        assert!(
            doc_is_scalar_uniform_only(&doc),
            "Value-only roughness/metal must take the no-bake scalar path"
        );
    }

    #[test]
    fn m1b_layered_noise_is_not_scalar_uniform() {
        let mut doc = empty_doc();
        doc.channels = vec![Channel::with_state(
            ChannelKind::BaseColor,
            ChannelState::Layered,
            vec![Layer {
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
                placement: Placement { space: None },
                adjust: Adjust::default(),
            }],
        )];
        assert!(
            !doc_is_scalar_uniform_only(&doc),
            "noise layer stack must bake (not scalar fast path)"
        );
    }

    #[test]
    fn m1b_graph_driven_empty_layers_is_scalar_uniform() {
        let mut doc = empty_doc();
        doc.channels = vec![Channel::with_state(
            ChannelKind::BaseColor,
            ChannelState::GraphDriven { node: 0 },
            vec![],
        )];
        assert!(
            doc_is_scalar_uniform_only(&doc),
            "GraphDriven with no layers is not a panel bake"
        );
    }
}

// ============================================================================
// Owner 2026-08-19: "j'ajoute un channel (SSS color) et le channel n'apparaît pas."
// ============================================================================
#[cfg(test)]
mod added_channel_survival_tests {
    use crate::material_panel::model::ChannelKind;

    /// On a GRAPH material, `mp_current_doc` re-derives the doc every generation and the
    /// `in_use` projection (lower/actions.rs) drops any default-valued, map-less channel
    /// — which a freshly ADDED channel is by construction. The stored-doc intent merge
    /// must keep it alive.
    #[test]
    fn added_default_valued_channel_survives_graph_projection() {
        let mut app = crate::AzraApp::new_headless();
        let mid = app
            .scene
            .scene_graph
            .add_material("m", core_math::pbr_material::PbrMaterial::default());
        // Simulate an opened shader graph (the projection path trigger).
        let mut g = crate::shader_nodes::NodeGraph::default();
        if let Some((_, mat)) = app.scene.scene_graph.material_by_id(mid) {
            g.pull_from_pbr(mat);
        }
        app.ui.material_node_graphs.insert(mid.0, g);

        app.mp_add_channel(mid, ChannelKind::SssColor);

        let mat = app
            .scene
            .scene_graph
            .material_by_id(mid)
            .map(|(_, m)| m.clone())
            .unwrap();
        let doc = app.mp_current_doc(mid, &mat);
        assert!(
            doc.channel(ChannelKind::SssColor).is_some(),
            "added SSS color channel must survive the graph projection"
        );
        // Removal must still work — nothing may resurrect from the stored doc.
        let mut doc2 = doc.clone();
        doc2.channels.retain(|c| c.kind != ChannelKind::SssColor);
        app.mp_commit_doc_edit(mid, doc2, "remove", false);
        let doc3 = app.mp_current_doc(mid, &mat);
        assert!(
            doc3.channel(ChannelKind::SssColor).is_none(),
            "removing the channel must stick (no resurrection from the stored doc)"
        );
    }
}
