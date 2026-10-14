// scene_state.rs — Scene-level state extracted from FourthLayerApp.
// Contains the scene graph (source of truth), flat prim cache,
// material library, and body/scope tracking.
// Lights are accessed directly through the scene graph — no flat Vec.

use core_math::primitives::SdfPrimitive;
use core_math::scene_graph::{NodeId, SceneGraph, LightData};
use std::collections::HashMap;

use crate::BodyPivotMode;
use graphics::LightConfig;

/// Scene-level state: graph, flat prim cache, materials, body tracking.
pub(crate) struct SceneState {
    // Scene graph — source of truth. `primitives` is a derived flat cache rebuilt via sync_flat_cache().
    pub(crate) scene_graph: SceneGraph,
    pub(crate) primitives: Vec<SdfPrimitive>,
    pub(crate) next_prim_id: u32,
    // Maps flat primitives[i] → (body_node_id, index_within_body). Rebuilt by sync_flat_cache().
    pub(crate) prim_body_map: Vec<(NodeId, usize)>,
    // O(1) lookup caches — rebuilt by sync_flat_cache().
    pub(crate) prim_uid_to_index: HashMap<u32, usize>,
    // Cached NodeId of the default SdfBody node (always exists after init/load).
    pub(crate) sdf_body_nid: NodeId,
    /// Which SdfBody the artist is currently editing.
    /// Defaults to sdf_body_nid (root body). Set by enter/exit scope.
    pub(crate) active_body_nid: NodeId,
    /// How body origins are computed when prim membership changes.
    pub(crate) body_pivot_mode: BodyPivotMode,
    // Light ID counter — used to assign unique IDs when creating lights.
    pub(crate) next_light_id: u32,
    // Isolate mode
    pub(crate) isolated_body_nid: Option<NodeId>,
    /// General-purpose naming counter for new scene-graph nodes (Body, Group, Poly, etc.)
    pub(crate) next_node_name_id: u32,
    /// Legacy GPU light_config — synced from the first scene light (moved from FourthLayerApp — Phase 1).
    pub(crate) light_config: LightConfig,
}

impl SceneState {
    pub(crate) fn new() -> Self {
        Self {
            scene_graph: SceneGraph::new(),
            primitives: Vec::new(),
            next_prim_id: 1,
            prim_body_map: Vec::new(),
            prim_uid_to_index: HashMap::new(),
            sdf_body_nid: NodeId(0),
            active_body_nid: NodeId(0),
            body_pivot_mode: BodyPivotMode::default(),
            next_light_id: 2,
            isolated_body_nid: None,
            next_node_name_id: 1,
            light_config: LightConfig::default(),
        }
    }

    // ---- Base-prim check (delegates to scene_graph — chain order, not Vec order) ----

    /// True iff flat prim `flat_idx` is its body's BASE operand (first slot of the body's
    /// resolved `child_order` — see `SceneGraph::body_base_slot`). Replaces every flat-index
    /// `i == 0` / `idx > 0` base check: reorder permutes ONLY `child_order`, the flat Vec
    /// never moves, so index-0 checks diverge from the field after any reorder (green-dot /
    /// X-does-nothing bug). Returns false when the body's first chain slot is a sub-body
    /// (the sub-body result seeds — no prim is base).
    pub(crate) fn is_base_prim_flat(&self, flat_idx: usize) -> bool {
        self.prim_body_map.get(flat_idx)
            .is_some_and(|&(nid, local)| self.scene_graph.is_base_prim(nid, local))
    }

    // ---- Light graph accessors (delegate to scene_graph) ----

    /// Get a light by its stable id. Returns None if not found.
    pub(crate) fn light_by_id(&self, id: u32) -> Option<&LightData> {
        self.scene_graph.light_by_id(id).map(|(_, data)| data)
    }

    /// Get a mutable light by its stable id. Returns None if not found.
    pub(crate) fn light_by_id_mut(&mut self, id: u32) -> Option<&mut LightData> {
        self.scene_graph.light_by_id_mut(id).map(|(_, data)| data)
    }

    /// Number of lights in the scene.
    pub(crate) fn light_count(&self) -> usize {
        self.scene_graph.light_count()
    }

    /// Bitfield of **visible** lights with volumetric enabled (graph order).
    /// Prefer `FrameLightGpu.vol_bits` after P3.1 pack (shade order). This remains for
    /// cheap any-vol checks before packing.
    pub(crate) fn volumetric_light_bitfield(&self) -> u32 {
        let mut bits = 0u32;
        for (i, light) in self.scene_graph.lights()
            .map(|(_, d)| d)
            .filter(|l| l.visible)
            .enumerate()
        {
            if i >= 32 { break; }
            if light.volumetric.enabled {
                bits |= 1u32 << i;
            }
        }
        bits
    }

    /// True if any VISIBLE light has volumetric scattering ("god rays") enabled. Gates the
    /// fog/volumetric pass + its composite so volumetric lights render even with global fog OFF.
    pub(crate) fn any_volumetric_light(&self) -> bool {
        self.scene_graph.lights().any(|(_, l)| l.visible && l.volumetric.enabled)
    }

    /// True if any VISIBLE, RT-enabled light carries its OWN soft radius (> 0). Gates the
    /// RT-shadow PENUMBRA chain (soft disc + denoise + temporal) so a per-light radius produces a
    /// soft penumbra INDEPENDENTLY of the GLOBAL `rt_shadow_softness` slider — otherwise a scene with
    /// the global slider at 0 but one soft light would render that light hard (RT_SHADOWS_DESIGN
    /// Stage 4).
    ///
    /// 2026-08-07: DIRECTIONAL lights count too. The old exclusion ("the sun stays on CSM")
    /// predates the 2026-07-28 sun RT opt-in — with it, a sun-only scene whose sun had a
    /// Source Radius silently collapsed to the single hard center ray, ignoring the radius
    /// the user authored.
    pub(crate) fn any_rt_soft_light(&self) -> bool {
        self.scene_graph.lights().any(|(_, l)| {
            l.visible && l.rt_shadow.enabled && l.rt_shadow.radius > 0.0
        })
    }

    /// Count VISIBLE, RT-enabled point/spot lights. The RT visibility buffer packs at most 4 lights
    /// (RGBA channel = light index); when more than 4 RT point/spot lights exist, the ones past the
    /// cap fall back to the legacy `soft_shadow` (texture-array packing is a Stage-4 follow-up). Used
    /// only to emit a one-time heads-up (RT_SHADOWS_DESIGN Stage 4). Directional (sun) stays on CSM.
    pub(crate) fn count_rt_shadow_pointspot_lights(&self) -> usize {
        self.scene_graph.lights().filter(|(_, l)| {
            l.visible
                && l.rt_shadow.enabled
                && l.light_type != core_math::scene_graph::LightType::Directional
        }).count()
    }

    /// Collect all light IDs (stable, unordered).
    pub(crate) fn light_ids(&self) -> Vec<u32> {
        self.scene_graph.lights().map(|(_, d)| d.id).collect()
    }

    /// Find the graph NodeId for a light by its stable id.
    pub(crate) fn light_nid(&self, id: u32) -> Option<NodeId> {
        self.scene_graph.light_by_id(id).map(|(nid, _)| nid)
    }
}
