//! Bake dispatch: GPU eligibility, spawn workers, AzraApp enqueue/schedule.
//!
//! Pure relocation from monolithic node_graph_bake — no logic change.
//! GPU-adjacent: whole functions only — bind-group / buffer order preserved.
//! Map pixel bake lives in sibling `bake_maps.rs`; poll/readback in `bake_poll.rs`.

use super::bake_maps::{
    bake_node_graph_maps_quality_with_mask_dirty_ch, bake_work_resolution,
    bake_work_resolution_for, channels_from_graph, curvature_keys_for_graph,
    graph_uses_image_texture, graph_uses_sdf_field, master_sockets_for_mask,
    topo_sort_for_mask,
};
use super::graph_eval::SdfFieldSources;
use super::m4_scoreboard::{
    gpu_bake_coverage_counts, gpu_bake_record_dispatch_fail, gpu_bake_record_ok,
    gpu_bake_record_reject,
};
use super::packing::{snap_work_res, BakeDirty, MapBakeRect};
use super::texture_cache::ImageMapSources;
use super::{
    BakeChannelMask, BakedNodeMaps, GraphBakeOwner, GraphBakeQuality, GraphBakeResult,
    InflightGpuBake, BAKE_WORK_RES,
};
use crate::material_textures::{self, DEFAULT_MAP_RES, MAP_RES};
use crate::shader_nodes::{NodeGraph, NodeType};
use core_math::pbr_material::MapKind;
use glam::Vec3;
use std::sync::mpsc;


/// A bake whose graph uses the SDF-field nodes, parked until the poll phase so the
/// `curvature_eval` kernel can run (Frame Ordering Contract — `enqueue_*` is reachable from
/// panel draws, where a GPU dispatch is illegal).
///
/// `poll_graph_curvature` dispatches one grid per key in `keys`, fills `sdf.curv_caches`, then
/// hands the whole thing to `spawn_graph_bake_quality` unchanged — so the worker path, the
/// seq/coalesce bookkeeping and the auto-apply Live→Final upgrade are all byte-identical to the
/// non-curvature path.
pub(crate) struct PendingCurvBake {
    pub owner: GraphBakeOwner,
    pub seq: u64,
    pub graph: NodeGraph,
    pub quality: GraphBakeQuality,
    pub images: ImageMapSources,
    pub sdf: SdfFieldSources,
    /// The bake work resolution — every curvature grid is computed at exactly this res so the
    /// pixel↔sample mapping in `sdf_curvature_masks`'s fast path is 1:1.
    pub res: u32,
    /// The distinct `CurvKey`s this job's graph uses (`curvature_keys_for_graph`) — one dispatch,
    /// one grid, per entry.
    pub keys: Vec<crate::gpu_curvature::CurvKey>,
    /// O4 dirty region + prior maps (moved with the job so parked curvature bakes stay partial).
    pub dirty: BakeDirty,
    pub prior: Option<BakedNodeMaps>,
    /// ADJUST_SLIDER_LAG F2: optional per-channel bake filter.
    pub channel_filter: Option<BakeChannelMask>,
}

/// A parked bake whose `curvature_eval` dispatches (ONE PER DISTINCT `CurvKey` in its graph) have
/// all been SUBMITTED, waiting on their async readbacks.
///
/// Stage 2 of the three-stage curvature path (park → submit → drain). The job payload rides
/// along untouched so that, once every readback has settled (or failed), `poll_graph_curvature`
/// can attach whichever grids succeeded and spawn the worker through the SAME
/// `spawn_graph_bake_quality` call the immediate path uses — the worker path stays byte-identical.
/// A per-key GPU failure drops ONLY that key (that node's samples fall back to the CPU path,
/// same as before P1 existed) — it never cancels the other keys' grids or the whole bake.
pub(crate) struct InFlightCurvBake {
    pub job: PendingCurvBake,
    /// One armed readback per distinct key this job's graph uses. Peeked non-blockingly each
    /// poll; a settled entry (`Some`) is read + unmapped and removed; `None` marks an already-
    /// finished slot so the vec's length still reflects "how many keys remain outstanding".
    pub flights: Vec<(crate::gpu_curvature::CurvKey, Option<crate::gpu_curvature::PendingCurvReadback>)>,
    /// Grids already resolved (settled OK) while others in `flights` are still pending.
    pub done: Vec<crate::gpu_curvature::CurvGrid>,
}

/// GPU-bake eligibility (GPU_MATERIAL_BAKE_PLAN.md P1).
///
/// `true` only when EVERY node in the graph is faithfully reproduced by
/// `NodeGraph::generate_wgsl`. This is a whitelist, not a blacklist: an unrecognised /
/// newly-added node type is rejected until someone verifies its WGSL matches the CPU arm.
/// Rejecting costs a slower bake; wrongly accepting silently ships a WRONG map.
///
/// **Why a gate is needed at all** — the WGSL codegen is NOT a faithful port of the CPU
/// evaluator, despite covering the same node types. Two families are deliberate placeholders
/// (`shader_nodes/graph.rs`, comments quoted verbatim):
///   • `ImageTexture` (`:1343`) emits `vec3(1.0,1.0,1.0)` — *"WGSL samples white placeholder"*.
///     A photo layer would bake pure white.
///   • `Curvature` / `Cavity` / `AmbientOcclusion` / `Slope` / `SdfCurvature` / `PrimSdfMask`
///     read a SYNTHETIC analytic height field (`sn_field_h`, `:589`) — *"Live WGSL has no scene
///     SDF — height-field laplacian is a preview only"*. They would bake a plausible-looking
///     pattern with no relationship to the actual surface.
///
/// Those are fine for the live shader preview they were written for, and unacceptable for the
/// authored map. P2/P3 of the plan replace them with real sampling; until then this gate is what
/// keeps the GPU path from silently producing wrong output (CLAUDE.md "No Hacks": a documented
/// guard that FAILS CLOSED, never a plausible-but-wrong result).
/// True when every node is on the GPU bake whitelist (WGSL faithful to CPU).
pub(crate) fn graph_is_gpu_bakeable(graph: &NodeGraph) -> bool {
    graph_gpu_bake_reject_reason(graph).is_none()
}

/// G2/G4 — node types that must **never** reach `create_shader_module` on the bake path.
/// Scene-dependent (`calc_ao` / `scene_sdf`) until a real bake-context port exists.
/// Fail closed (CPU path), not a validation crash.
pub(crate) const GPU_INELIGIBLE: &[NodeType] = &[
    NodeType::AmbientOcclusion,
    NodeType::SdfThickness,
];

pub(crate) fn node_type_gpu_bakeable(nt: &NodeType) -> bool {
    if GPU_INELIGIBLE.iter().any(|x| std::mem::discriminant(x) == std::mem::discriminant(nt)) {
        return false;
    }
    use crate::shader_nodes::NodeType as NT;
    matches!(
        nt,
        // Pure math / colour / constants — no external data, WGSL matches the CPU arm.
        NT::ConstantValue
            | NT::ConstantColor
            | NT::MathBinary
            | NT::MathUnary
            | NT::VectorMath
            | NT::Clamp
            | NT::MixFloat
            | NT::MixRgb
            | NT::Smoothstep
            | NT::Remap
            | NT::Invert
            | NT::ColorCorrect
            | NT::BrightnessContrast
            | NT::ColorRamp
            | NT::SeparateXyz
            | NT::CombineXyz
            // Procedural patterns — self-contained generators.
            | NT::Noise
            | NT::Voronoi
            | NT::GradientTexture
            | NT::WaveTexture
            | NT::MusgraveTexture
            | NT::BrickTexture
            | NT::Checker
            | NT::Gamma
            // G8 Frame: zero sockets, zero emission — can never join a chain, so a
            // decorative box must not force the whole material onto the CPU bake.
            // (Reroute is deliberately ABSENT: its WGSL float coercion would collapse
            // colours — one reroute anywhere = CPU bake, recorded trade-off.)
            | NT::Frame
            // Domain / projection — pure coordinate math.
            | NT::TexCoord
            | NT::ObjectInfo
            | NT::UVTransform
            | NT::Triplanar
            | NT::BoxMapping
            | NT::CylindricalMapping
            | NT::SphericalMapping
            | NT::PlanarMapping
            | NT::HeightToNormal
            // G1 unblocked integer-axis codegen; pure domain remaps (no scene SDF in live path).
            | NT::SdfPosition
            // The graph terminal.
            | NT::MaterialOutput
    )
    // DELIBERATELY EXCLUDED (placeholder or unverified in WGSL):
    //   ImageTexture, TextureSample — white placeholder / map sample
    //   Curvature, Cavity, Slope, SdfCurvature, PrimSdfMask — synthetic field
    //   SurfaceDirection — needs scene normal field
    //   AmbientOcclusion, SdfThickness — GPU_INELIGIBLE (G4 fail closed)
}

/// T2 — first node type that blocks GPU bake, for logging. `None` = fully bakeable.
/// Whole-graph check (full Final / no F2 filter). Prefer
/// [`graph_gpu_bake_reject_reason_for`] when a channel filter is known.
pub(crate) fn graph_gpu_bake_reject_reason(graph: &NodeGraph) -> Option<&'static str> {
    for n in &graph.nodes {
        if !node_type_gpu_bakeable(&n.node_type) {
            return Some(crate::shader_nodes::nodes::label(&n.node_type));
        }
    }
    None
}

/// M1 gate — whether the product GPU bake path may run (pure; unit-tested).
///
/// - `dirty_is_full`: O4 partial spatial dirty still forces CPU (no GPU tile composite).
/// - `force_cpu`: `AZRA_FORCE_CPU_BAKE=1` sabotage (must fail the <33 ms live gate).
/// - `gpu_bake_off`: `AZRA_GPU_BAKE=0` opt-out.
#[inline]
pub(crate) fn gpu_bake_path_eligible(
    dirty_is_full: bool,
    force_cpu: bool,
    gpu_bake_off: bool,
) -> bool {
    dirty_is_full && !force_cpu && !gpu_bake_off
}


/// M1.a — GPU eligibility scoped to the channels about to bake.
///
/// - `channel_filter == None` (full Final): every node in the graph must be whitelisted
///   (same as [`graph_gpu_bake_reject_reason`]).
/// - `channel_filter == Some(mask)` (F2 scrub): only nodes **reachable from those Master
///   sockets** are checked. An `ImageTexture` on Roughness no longer forces a Noise→Albedo
///   scale scrub onto the CPU path — the highest-frequency edit stays on GPU.
///
/// Fail-closed: if the filtered subgraph cannot be walked, treat as rejected (`"cycle/empty"`).
pub(crate) fn graph_gpu_bake_reject_reason_for(
    graph: &NodeGraph,
    channel_filter: Option<BakeChannelMask>,
) -> Option<&'static str> {
    let Some(ch) = channel_filter else {
        return graph_gpu_bake_reject_reason(graph);
    };
    // Empty mask → nothing to GPU-author; caller uses channel plan `None` — treat as
    // non-reject here so we don't double-count; plan gate owns the fallthrough.
    let sockets = master_sockets_for_mask(ch);
    if sockets.is_empty() {
        return None;
    }
    let Some(out) = graph.output_node() else {
        return Some("no MaterialOutput");
    };
    let Some(order) = topo_sort_for_mask(graph, out.id, ch) else {
        return Some("cycle/empty subgraph");
    };
    for nid in order {
        let Some(n) = graph.find_node(nid) else {
            continue;
        };
        if !node_type_gpu_bakeable(&n.node_type) {
            return Some(crate::shader_nodes::nodes::label(&n.node_type));
        }
    }
    None
}

/// M1.a — channel bitflags for a GPU material bake (1=albedo 2=rough 4=metal 8=emission).
///
/// Returns `None` when the CPU path must own this bake:
/// - no GPU-authorable channels after F2 filter intersection, or
/// - full bake (no filter) of a graph that also wires normal/bump/disp (half-author hazard), or
/// - filter that requests normal/bump/disp (GPU has no planes for them).
///
/// Pure: unit-tested without a device. Production `try_gpu_bake_graph` is a thin wrapper.
pub(crate) fn gpu_bake_channel_plan(
    graph: &NodeGraph,
    channel_filter: Option<BakeChannelMask>,
) -> Option<(u32, BakeChannelMask)> {
    let mut ch = channels_from_graph(graph);
    if let Some(f) = channel_filter {
        ch = ch.and(f);
    }
    let mut bits = 0u32;
    if ch.albedo {
        bits |= 1;
    }
    if ch.roughness {
        bits |= 2;
    }
    if ch.metallic {
        bits |= 4;
    }
    if ch.emission {
        bits |= 8;
    }
    if bits == 0 {
        return None;
    }
    // GPU bake has planes for albedo/rough/metal/emission ONLY. A graph wiring
    // normal/bump/disp — or sss_color — must fall to the CPU path (half-authoring
    // hazard: the GPU result would install some channels while silently dropping
    // the rest of the same bake).
    if channel_filter.is_none() {
        let full = channels_from_graph(graph);
        if full.normal || full.bump || full.displacement || full.sss_color {
            return None;
        }
    } else if ch.normal || ch.bump || ch.displacement || ch.sss_color {
        return None;
    }
    let baked = BakeChannelMask {
        albedo: ch.albedo,
        roughness: ch.roughness,
        metallic: ch.metallic,
        emission: ch.emission,
        normal: false,
        bump: false,
        displacement: false,
        sss_color: false,
    };
    Some((bits, baked))
}

/// Spawn a background bake. UI thread returns immediately.
pub(crate) fn spawn_graph_bake(
    tx: mpsc::Sender<GraphBakeResult>,
    owner: GraphBakeOwner,
    seq: u64,
    graph: NodeGraph,
) {
    spawn_graph_bake_quality(
        tx,
        owner,
        seq,
        graph,
        GraphBakeQuality::Final,
        ImageMapSources::default(),
        SdfFieldSources::default(),
    );
}

pub(crate) fn spawn_graph_bake_quality(
    tx: mpsc::Sender<GraphBakeResult>,
    owner: GraphBakeOwner,
    seq: u64,
    graph: NodeGraph,
    quality: GraphBakeQuality,
    images: ImageMapSources,
    sdf: SdfFieldSources,
) {
    spawn_graph_bake_quality_dirty(
        tx,
        owner,
        seq,
        graph,
        quality,
        images,
        sdf,
        BakeDirty::Full,
        None,
    );
}

/// O4: spawn with dirty-rect + optional prior maps (moved onto the worker thread).
pub(crate) fn spawn_graph_bake_quality_dirty(
    tx: mpsc::Sender<GraphBakeResult>,
    owner: GraphBakeOwner,
    seq: u64,
    graph: NodeGraph,
    quality: GraphBakeQuality,
    images: ImageMapSources,
    sdf: SdfFieldSources,
    dirty: BakeDirty,
    prior: Option<BakedNodeMaps>,
) {
    spawn_graph_bake_quality_dirty_ch(
        tx, owner, seq, graph, quality, images, sdf, dirty, prior, None,
    )
}

/// Like [`spawn_graph_bake_quality_dirty`] with an optional live channel filter (F2).
pub(crate) fn spawn_graph_bake_quality_dirty_ch(
    tx: mpsc::Sender<GraphBakeResult>,
    owner: GraphBakeOwner,
    seq: u64,
    graph: NodeGraph,
    quality: GraphBakeQuality,
    images: ImageMapSources,
    sdf: SdfFieldSources,
    dirty: BakeDirty,
    prior: Option<BakedNodeMaps>,
    channel_filter: Option<BakeChannelMask>,
) {
    let name = match quality {
        GraphBakeQuality::Live => "azra-graph-live",
        GraphBakeQuality::Final => "azra-graph-bake",
    };
    // Bake workers need a larger stack: dual-plane encode + deep node eval in debug
    // builds overflowed the default ~1–2 MB stack (STATUS_STACK_OVERFLOW after 1024²).
    // Rayon *row* workers inherit the global pool's stack (~2 MB) unless we install a
    // dedicated pool. ONE process-wide pool (OnceLock) — building a fresh
    // ThreadPoolBuilder per bake allocated 8MB×N threads every scrub tick and OOMed
    // / crashed the process under rapid Live rebakes.
    const BAKE_STACK: usize = 16 * 1024 * 1024;
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(BAKE_STACK)
        .spawn(move || {
            fn bake_row_pool() -> Option<&'static rayon::ThreadPool> {
                use std::sync::OnceLock;
                static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
                POOL.get_or_init(|| {
                    const RAYON_STACK: usize = 4 * 1024 * 1024;
                    // Cap row threads — stack reserve is per worker; 8×4MB is enough.
                    let n = rayon::current_num_threads().clamp(1, 8);
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(n)
                        .stack_size(RAYON_STACK)
                        .thread_name(|i| format!("azra-bake-row-{i}"))
                        .build()
                        .ok()
                })
                .as_ref()
            }
            let run = || {
                bake_node_graph_maps_quality_with_mask_dirty_ch(
                    &graph,
                    quality,
                    &images,
                    &sdf,
                    dirty,
                    prior.as_ref(),
                    channel_filter,
                )
            };
            let (maps, channels, work_res) = match bake_row_pool() {
                Some(pool) => pool.install(run),
                None => run(), // serial on the 16MB bake thread
            }
            .map(|(m, c, wr)| (Some(m), c, wr))
            .unwrap_or((None, BakeChannelMask::default(), 0));
            let _ = tx.send(GraphBakeResult {
                owner,
                seq,
                maps,
                channels,
                quality,
                work_res,
            });
        })
        .ok();
}

// ---------------------------------------------------------------------------
// AzraApp integration
// ---------------------------------------------------------------------------


impl crate::AzraApp {
    /// Request bake of a prim graph — **non-blocking** (background thread). Full quality.
    pub(crate) fn enqueue_node_graph_bake(&mut self, prim_uid: u32) {
        self.cache.graph_live_owner = None;
        self.cache.graph_live_dirty = false;
        self.enqueue_node_graph_bake_quality(prim_uid, GraphBakeQuality::Final);
    }

    pub(crate) fn enqueue_node_graph_bake_quality(
        &mut self,
        prim_uid: u32,
        quality: GraphBakeQuality,
    ) {
        // Prefer live editor graph when this prim is open.
        let graph = if let Some(crate::NodeGraphTarget::Prim(pi)) = self.ui.node_graph_target {
            if self.scene.primitives.get(pi).map(|p| p.prim_uid) == Some(prim_uid) {
                self.ui.node_graph.clone()
            } else {
                match self.ui.node_graphs.get(&prim_uid).cloned() {
                    Some(g) => g,
                    None => return,
                }
            }
        } else {
            match self.ui.node_graphs.get(&prim_uid).cloned() {
                Some(g) => g,
                None => return,
            }
        };
        self.ui.node_graphs.insert(prim_uid, graph.clone());
        // White tint + absolute Mult install: keep existing map tiling/projection
        // (do not force uv_scale=1 / Box — same Image+Noise tile jump as materials).
        if let Some(idx) = self.prim_index_by_uid(prim_uid) {
            self.write_prim(idx, |p| {
                p.material.albedo = Vec3::ONE;
                p.material.albedo_map.strength = 1.0;
                p.material.albedo_map.absolute = true;
                p.material.roughness_map.strength = 1.0;
                p.material.metallic_map.strength = 1.0;
            });
        }
        let images = if graph_uses_image_texture(&graph) {
            self.snapshot_prim_image_maps(prim_uid)
        } else {
            ImageMapSources::default()
        };
        let sdf = if graph_uses_sdf_field(&graph) {
            self.snapshot_sdf_field_for_prim(prim_uid)
        } else {
            SdfFieldSources::default()
        };
        let owner = GraphBakeOwner::Prim(prim_uid);
        let seq = self.cache.graph_bake_seq.wrapping_add(1);
        self.cache.graph_bake_seq = seq;
        self.cache.graph_bake_pending.insert(owner.key(), seq);
        // Coalesce gate for the live owner: Final from flush/auto-upgrade must also block
        // the next live kick until it lands (otherwise settle double-fires).
        self.cache.graph_live_in_flight = self.cache.graph_live_owner == Some(owner);
        // O4: dirty + prior (prim path same as materials).
        let dirty = self
            .cache
            .graph_bake_dirty
            .remove(&owner.key())
            .unwrap_or(BakeDirty::Full);
        let prior = self.cache.graph_bake_map_cache.get(&owner.key()).cloned();
        // Same GPU-curvature park/spawn decision as the material path.
        self.spawn_or_park_graph_bake(
            owner, seq, graph, quality, images, sdf, dirty, prior, None,
        );
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// CPU snapshot of a material's loaded map slots for ImageTexture bake sampling.
    pub(crate) fn snapshot_material_image_maps(
        &self,
        mid: core_math::primitives::MaterialId,
        graph: Option<&NodeGraph>,
    ) -> ImageMapSources {
        let mut out = ImageMapSources::default();
        let Some((_, mat)) = self.scene.scene_graph.material_by_id(mid) else {
            return out;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return out;
        };
        let mt = &gpu.material_textures;
        // Pinned per-node source slots (Node.image_tex_id ≠ 0 — panel P3 layers).
        if let Some(g) = graph {
            for n in &g.nodes {
                if matches!(n.node_type, NodeType::ImageTexture) && n.image_tex_id != 0 {
                    if let Ok(px) = mt.snapshot_slot(&gpu.device, &gpu.queue, n.image_tex_id) {
                        out.by_tex.entry(n.image_tex_id).or_insert(px);
                    }
                }
            }
        }
        for kind in MapKind::ALL {
            if let Ok(px) = mt.snapshot_slot(&gpu.device, &gpu.queue, mat.map(kind).tex_id) {
                out.maps.insert(kind.wire_tag(), px);
            }
        }
        out
    }

    /// Snapshot body prims for SDF mask / curvature bake (body-local space).
    pub(crate) fn snapshot_sdf_field_for_material(
        &self,
        mid: core_math::primitives::MaterialId,
    ) -> SdfFieldSources {
        let mut out = SdfFieldSources::default();
        // Prefer active body; fall back to all scene prims by uid.
        let body = self.scene.active_body_nid;
        // `body_primitives` returns `None` for a PolygonalBody, so `body_prims` staying empty is
        // exactly the "this body has no analytical field" signal `has_field()` reads. `by_uid`
        // below is scene-wide on purpose and must never imply one.
        if let Some(prims) = self.scene.scene_graph.body_primitives(body) {
            out.body_prims = prims.to_vec();
            for p in prims {
                out.by_uid.insert(p.prim_uid, p.clone());
            }
        }
        // Also index every prim in the scene so cross-body picks work.
        for (_nid, _i, p) in self.scene.scene_graph.all_primitives() {
            out.by_uid.entry(p.prim_uid).or_insert_with(|| p.clone());
        }
        // Ensure material-linked body (if any) is preferred when active body is empty.
        let _ = mid;
        out
    }

    pub(crate) fn snapshot_sdf_field_for_prim(&self, prim_uid: u32) -> SdfFieldSources {
        let mut out = self.snapshot_sdf_field_for_material(core_math::primitives::MaterialId(0));
        // If this prim lives on a body, prefer that body's prim list.
        if let Some(idx) = self.prim_index_by_uid(prim_uid) {
            if let Some(p) = self.scene.primitives.get(idx) {
                // Find owning body via scene graph if available.
                for (nid, _i, prim) in self.scene.scene_graph.all_primitives() {
                    if prim.prim_uid == p.prim_uid {
                        if let Some(prims) = self.scene.scene_graph.body_primitives(nid) {
                            out.body_prims = prims.to_vec();
                        }
                        break;
                    }
                }
            }
        }
        out
    }

    /// Prim embedded material map snapshot (same ImageTexture path).
    pub(crate) fn snapshot_prim_image_maps(&self, prim_uid: u32) -> ImageMapSources {
        let mut out = ImageMapSources::default();
        let Some(idx) = self.prim_index_by_uid(prim_uid) else {
            return out;
        };
        let Some(p) = self.scene.primitives.get(idx) else {
            return out;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return out;
        };
        let mt = &gpu.material_textures;
        let mat = &p.material;
        for kind in MapKind::ALL {
            if let Ok(px) = mt.snapshot_slot(&gpu.device, &gpu.queue, mat.map(kind).tex_id) {
                out.maps.insert(kind.wire_tag(), px);
            }
        }
        out
    }

    /// Clear procedural map slots when the graph is cleared (all bake channels → 0).
    pub(crate) fn clear_node_graph_maps(&mut self, prim_uid: u32) {
        if let Some(idx) = self.prim_index_by_uid(prim_uid) {
            self.write_prim(idx, |p| {
                for kind in MapKind::ALL {
                    p.material.map_mut(kind).tex_id = 0;
                }
            });
        }
        self.cache
            .graph_bake_pending
            .remove(&GraphBakeOwner::Prim(prim_uid).key());
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Schedule a **live** low-res rebake after node-editor edits.
    ///
    /// **Coalesce (not debounce-until-idle):** while the user drags knobs we mark dirty and
    /// keep one bake in flight. When it lands we immediately spawn the next with the latest
    /// graph — so the viewport updates *during* the drag, not only after they stop.
    pub(crate) fn schedule_live_material_graph_bake(
        &mut self,
        mid: core_math::primitives::MaterialId,
    ) {
        self.schedule_live_material_graph_bake_ch(mid, None);
    }

    /// Live bake restricted to `channel_filter` channels (ADJUST_SLIDER_LAG F2).
    /// Filters accumulate via OR while the same scrub coalesce window is open.
    pub(crate) fn schedule_live_material_graph_bake_ch(
        &mut self,
        mid: core_math::primitives::MaterialId,
        channel_filter: Option<BakeChannelMask>,
    ) {
        if let Some(m) = channel_filter {
            if m.any() {
                self.cache.graph_live_channel_filter = Some(
                    self.cache
                        .graph_live_channel_filter
                        .map(|p| p.or(m))
                        .unwrap_or(m),
                );
            }
        }
        self.cache.graph_live_owner = Some(GraphBakeOwner::Material(mid.0));
        if channel_filter.is_none() { self.cache.graph_live_channel_filter = None; }
        self.cache.graph_live_dirty = true;
        // When the node editor is open on this material AND it is the freshest
        // authoring surface (user scrubbing graph knobs), push editor → store.
        // Panel lower writes store first then syncs editor — do NOT reverse that
        // here or we bake an outdated open-editor copy over the panel stack.
        // (Editor-only edits: lower may not run; push editor store then.)
        // Heuristic: only copy editor → store if store is missing this material.
        if !self.ui.material_node_graphs.contains_key(&mid.0)
            && matches!(
                self.ui.node_graph_target,
                Some(crate::NodeGraphTarget::Material(m)) if m == mid
            )
        {
            self.ui
                .material_node_graphs
                .insert(mid.0, self.ui.node_graph.clone());
        }
    }

    /// Same live path for a prim override graph (Shift+A / prim open).
    pub(crate) fn schedule_live_prim_graph_bake(&mut self, prim_uid: u32) {
        self.cache.graph_live_owner = Some(GraphBakeOwner::Prim(prim_uid));
        self.cache.graph_live_dirty = true;
        if matches!(self.ui.node_graph_target, Some(crate::NodeGraphTarget::Prim(_))) {
            self.ui.node_graphs.insert(prim_uid, self.ui.node_graph.clone());
        }
    }

    /// Spawn one coalesced bake if dirty and nothing in flight. Called from poll + schedule.
    ///
    /// **While scrubbing** (`has_active_episode`): Live quality — interactive work-res cap so
    /// Fill/noise drags stay snappy; a settled Live auto-upgrades to Final only when capped
    /// below the panel Bake N² target.
    ///
    /// **When settled** (no episode): bake once at full target as Final — avoids the laggy
    /// Live@256 → Final@1024 double bake (owner log: 63 ms + 1085 ms then stack pressure).
    pub(crate) fn flush_live_graph_bake(&mut self) {
        if !self.cache.graph_live_dirty {
            return;
        }
        let Some(owner) = self.cache.graph_live_owner else {
            self.cache.graph_live_dirty = false;
            return;
        };
        if let GraphBakeOwner::Material(mid) = owner {
            if self.try_resident_material_preview(core_math::primitives::MaterialId(mid)) {
                return;
            }
        }
        if self.cache.graph_live_in_flight { return; }
        self.cache.graph_live_dirty = false;
        // Scrub → Live (capped). Click / settle path → Final at panel Bake N² once.
        let quality = if self.has_active_episode() {
            GraphBakeQuality::Live
        } else {
            GraphBakeQuality::Final
        };
        match owner {
            GraphBakeOwner::Material(mid) => {
                self.enqueue_material_node_graph_bake_quality(
                    core_math::primitives::MaterialId(mid),
                    quality,
                );
            }
            GraphBakeOwner::Prim(uid) => {
                self.enqueue_node_graph_bake_quality(uid, quality);
            }
        }
    }

    fn try_resident_material_preview(&mut self, mid: core_math::primitives::MaterialId) -> bool {
        use crate::gpu_material_bake::ResidentPreviewStatus;
        use crate::frame_scheduler::{WorkClass, WorkTicket};
        if std::env::var("AZRA_GPU_BAKE").as_deref() == Ok("0")
            || std::env::var("AZRA_FORCE_CPU_BAKE").as_deref() == Ok("1")
        { return false; }
        let Some(graph) = self.ui.material_node_graphs.get(&mid.0) else { return false; };
        if crate::gpu_material_bake::poly_curvature::has_curvature(graph) && self.scene.scene_graph.material_use_counts(mid).0 == 0 {
            self.cache.graph_live_dirty = false;
            return true;
        }
        if graph_gpu_bake_reject_reason_for(graph, self.cache.graph_live_channel_filter).is_some() {
            return false;
        }
        let mut channels = channels_from_graph(graph);
        if let Some(filter) = self.cache.graph_live_channel_filter { channels = channels.and(filter); }
        if channels.displacement || channels.sss_color { return false; }
        let panel_mask = crate::material_panel::lower::panel_generated_mask(graph);
        let (channels, conflicts) = super::bake_maps::gate_mask_by_panel_ownership(
            channels, self.ui.material_panel_docs.get(&mid.0), panel_mask,
        );
        if conflicts != 0 { self.ui.mp_bake_conflicts.insert(mid.0, conflicts); }
        let kinds = [MapKind::Albedo, MapKind::Roughness, MapKind::Metallic, MapKind::Emission, MapKind::Normal, MapKind::Bump];
        let enabled = [channels.albedo, channels.roughness, channels.metallic, channels.emission, channels.normal, channels.bump];
        let mut bits = 0;
        let mut precision = 0;
        for (index, kind) in kinds.into_iter().enumerate() {
            if !enabled[index] { continue; }
            bits |= 1 << index;
            let high_precision = self.ui.material_panel_docs.get(&mid.0)
                .and_then(|doc| doc.channel(crate::material_panel::model::ChannelKind::from_map_kind(kind)))
                .is_none_or(|channel| matches!(channel.bit_depth, crate::material_panel::model::ChannelBitDepth::Bit16));
            if high_precision { precision |= 1 << index; }
        }
        if bits == 0 { self.cache.graph_live_dirty = false; return true; }
        let ticket = WorkTicket { class: WorkClass::High, cost_hint_ms: 0.5, name: "resident_material_preview" };
        if !self.scheduler.can_run(&ticket) {
            self.scheduler.record_skipped(&ticket);
            return true;
        }
        let started = std::time::Instant::now();
        let code = graph.compile_wgsl();
        let Some(gpu) = self.gpu.as_mut() else { return false; };
        let status = crate::gpu_material_bake::submit_resident_preview(
            &gpu.device, &gpu.queue, &mut gpu.material_textures, &mut gpu.mat_bake_pipe_cache.resident,
            &code, mid.0, bits, precision,
        );
        self.scheduler.record(&ticket, started.elapsed().as_secs_f32() * 1000.0);
        match status {
            ResidentPreviewStatus::Warming => true,
            ResidentPreviewStatus::Failed(error) => {
                eprintln!("[ResidentMaterial] {error}");
                false
            }
            ResidentPreviewStatus::Submitted(slots) => {
                self.cache.graph_bake_pending.remove(&GraphBakeOwner::Material(mid.0).key());
                for (index, kind) in kinds.into_iter().enumerate() {
                    if slots[index] == 0 { continue; }
                    let high_precision = precision & (1 << index) != 0;
                    let raw_precision = high_precision && index == 5;
                    let already_bound = self.scene.scene_graph.material_by_id(mid)
                        .is_some_and(|(_, material)| {
                            let map = material.map(kind);
                            map.tex_id == slots[index] && map.high_precision == high_precision
                                && map.raw_precision == raw_precision
                                && (matches!(kind, MapKind::Bump) || map.strength == 1.0)
                        });
                    if !already_bound {
                        self.apply_map_slot_precision(crate::material_textures::MapTarget::Graph(mid), kind,
                            slots[index], 1.0, high_precision, false, raw_precision);
                        self.cache.prim_buf_stale = true;
                    }
                }
                self.cache.graph_live_dirty = false;
                self.cache.graph_live_in_flight = false;
                self.mark_dirty(crate::DirtyFlags::RENDER);
                self.render.pt_accumulated_frames = 0;
                self.render.pt_converged = false;
                true
            }
        }
    }

    /// Request bake of an outliner Material graph — **non-blocking** (background thread).
    /// Preset click must not stall the UI frame (was baking 1024² on the UI thread).
    ///
    /// **One material:** Properties Base Color and Master Material Albedo are the same values.
    /// Only force white albedo *tint* when Master Albedo is **wired** (color lives in the baked
    /// map). Unconnected Master sockets stay on `PbrMaterial` so UI and node never diverge.
    pub(crate) fn enqueue_material_node_graph_bake(
        &mut self,
        mid: core_math::primitives::MaterialId,
    ) {
        // Explicit Apply / presets → full quality; cancel live coalesce.
        self.cache.graph_live_owner = None;
        self.cache.graph_live_dirty = false;
        // Full re-bake all channels (not a single-channel scrub settle).
        self.cache.graph_live_channel_filter = None;
        self.enqueue_material_node_graph_bake_quality(mid, GraphBakeQuality::Final);
    }

    pub(crate) fn enqueue_material_node_graph_bake_quality(
        &mut self,
        mid: core_math::primitives::MaterialId,
        quality: GraphBakeQuality,
    ) {
        use crate::shader_nodes::NodeGraph;
        // Source of truth: `material_node_graphs` (panel lower + node editor both
        // write here before scheduling). Preferring open `node_graph` first baked a
        // STALE editor copy over panel Fill/Noise params — scale never updated until
        // 8/16 forced another path. Editor writes store before `schedule_live`.
        let graph = if let Some(g) = self.ui.material_node_graphs.get(&mid.0) {
            g.clone()
        } else if matches!(
            self.ui.node_graph_target,
            Some(crate::NodeGraphTarget::Material(m)) if m == mid
        ) {
            self.ui.node_graph.clone()
        } else {
            return;
        };
        if crate::gpu_material_bake::poly_curvature::has_curvature(&graph) && self.scene.scene_graph.material_use_counts(mid).0 == 0 {
            self.mark_dirty(crate::DirtyFlags::RENDER);
            return;
        }
        // full_ch = every map-driven Master socket (ownership / clear decisions).
        // bake_ch = optional live filter ∩ full_ch (ADJUST_SLIDER_LAG F2: scrub one channel).
        // G2: ONE mask builder. This block was a byte-duplicate of
        // `channels_from_graph` — the classic two-builder drift trap.
        // G4 prep: panel FAST channels are subtracted — they install directly, never bake.
        let full_ch = super::bake_maps::bake_channels_from_graph(
            &graph,
            self.ui.material_panel_docs.get(&mid.0),
        );
        let channel_filter = self.cache.graph_live_channel_filter;
        // Final settle: take filter so only soft Live channels re-finalize; clear for next scrub.
        if matches!(quality, GraphBakeQuality::Final) {
            self.cache.graph_live_channel_filter = None;
        }
        let ch = match channel_filter {
            Some(f) if f.any() => full_ch.and(f),
            _ => full_ch,
        };
        // Map clear / ownership uses full_ch — never wipe untouched channels mid-scrub.
        let ch_maps = full_ch;
        // Nothing map-driven → pure Master scalars; push already owns PbrMaterial. No map bake.
        // P2 ownership gate (spec C1): map slots the PANEL owns (hand-loaded
        // image layers) are never cleared by graph-side enqueues.
        let owned: Vec<core_math::pbr_material::MapKind> = core_math::pbr_material::MapKind::ALL
            .iter()
            .copied()
            .filter(|&k| self.mp_panel_owns_map(mid.0, k))
            .collect();
        // Pure Vertex-only channels: always clear map (no Noise to keep).
        let vertex_only: Vec<core_math::pbr_material::MapKind> = self
            .ui
            .material_panel_docs
            .get(&mid.0)
            .map(|doc| {
                doc.channels
                    .iter()
                    .filter(|c| crate::material_panel::lower::channel_skip_planar_map_install(c))
                    .filter_map(|c| c.kind.map_kind())
                    .collect()
            })
            .unwrap_or_default();
        // Early-out when the graph drives no maps at all (use full_ch, not the live filter).
        if !ch_maps.any() {
            if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
                for k in core_math::pbr_material::MapKind::ALL {
                    if !owned.contains(&k) || vertex_only.contains(&k) {
                        m.map_mut(k).tex_id = 0;
                    }
                }
            }
            self.cache.prim_buf_stale = true;
            self.outliner.outliner_dirty = true;
            self.mark_dirty(crate::DirtyFlags::RENDER);
            return;
        }
        if !ch.any() {
            // Live filter selected channels that aren't graph-driven — nothing to bake.
            return;
        }
        // Resolve before mut-borrow (vertex_mask_base_albedo needs &self).
        let mask_base = self.vertex_mask_base_albedo(mid);
        // Texture Space tiling/projection for Mult absolute display (before mut material borrow).
        let mult_tex_space = self
            .ui
            .material_panel_docs
            .get(&mid.0)
            .map(|d| (d.texture_space.tiling[0].max(1e-4), d.texture_space.projection & 1));
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            // Wired albedo → map carries color; white tint avoids double-multiply.
            // Unconnected → keep Master/PbrMaterial albedo (Properties = node) and drop map.
            // LIVE-LAYER GUARD (root cause of "no live noise", 2026-07-23 visdiff run 3):
            // while the GPU live-noise layer is engaged, apply_live owns albedo (color0,
            // black↔white grayscale preview). This enqueue runs in the SAME call chain right
            // after apply_live and used to clobber that black back to white → the live layer
            // mixed white→white = mathematically invisible (and PT showed noise in roughness
            // but never albedo — the owner's exact report). The white tint now happens at
            // MAP-APPLY time (apply_map_slot settle handoff), when the map actually lands.
            //
            // Ownership clears use ch_maps (full graph), never the live scrub filter.
            if vertex_only.contains(&core_math::pbr_material::MapKind::Albedo) {
                m.albedo_map.tex_id = 0;
                if !m.noise_pattern.enabled {
                    m.albedo = Vec3::ONE;
                }
            } else if ch_maps.albedo {
                // LIVE vs Mult-stack ownership (M0 / LIVE-MATERIAL-FEEL fence):
                //
                // `apply_live_gpu_noise_material` may have just enabled noise_pattern for a
                // simple Noise→Albedo graph (same call chain as this enqueue). Killing it
                // here and arming absolute Mult flags while tex_id is still 0 left Hybrid
                // showing solid white (no live, no map) — AZRA_VISDIFF A↔B1 mean|Δ|=0.
                //
                // While live noise is engaged, leave it alone. `apply_map_slot` kills live
                // and arms absolute when the bake actually lands (settle handoff).
                //
                // Mult/Overlay *panel* stacks disable noise_pattern before enqueue, so they
                // still take the absolute Mult arm below (gbuffer REPLACE would wipe Mult).
                if m.noise_pattern.enabled && m.noise_pattern.amount > 0.001 {
                    // Live path owns albedo/noise until map settle — do not touch.
                } else {
                    m.noise_pattern.enabled = false;
                    m.noise_pattern.amount = 0.0;
                    // Photoshop Vertex mask: keep bottom Constant as base tint so the gbuffer
                    // can mix(below, map, mask). White tint for absolute Mult (unused for color).
                    m.albedo = mask_base.unwrap_or(Vec3::ONE);
                    m.albedo_map.strength = 1.0;
                    // Mult stack bakes per-layer tiling into the composite (UVTransform on each
                    // Image/Noise). Display at scale 1 so layers keep independent tilings.
                    // Projection follows material Texture Space (Box/UV).
                    m.albedo_map.uv_scale = 1.0;
                    if let Some((_tiling, proj)) = mult_tex_space {
                        m.albedo_map.projection = proj;
                    }
                    m.albedo_map.absolute = true;
                }
            } else if !owned.contains(&core_math::pbr_material::MapKind::Albedo) {
                m.albedo_map.tex_id = 0;
            }
            use core_math::pbr_material::MapKind;
            if ch_maps.roughness {
                m.roughness_map.strength = MapKind::Roughness.default_strength();
            } else if !owned.contains(&MapKind::Roughness) {
                m.roughness_map.tex_id = 0;
            }
            if ch_maps.metallic {
                m.metallic_map.strength = MapKind::Metallic.default_strength();
            } else if !owned.contains(&MapKind::Metallic) {
                m.metallic_map.tex_id = 0;
            }
            if ch_maps.emission {
                m.emission_map.strength = MapKind::Emission.default_strength();
            } else if !owned.contains(&MapKind::Emission) {
                m.emission_map.tex_id = 0;
            }
            if ch_maps.normal {
                m.normal_map.strength = MapKind::Normal.default_strength();
            } else if !owned.contains(&MapKind::Normal) {
                m.normal_map.tex_id = 0;
            }
            if !ch_maps.bump && !owned.contains(&MapKind::Bump) {
                m.bump_map.tex_id = 0;
            }
            if ch_maps.displacement {
                m.displacement_map.strength = MapKind::Displacement.default_strength();
            } else if !owned.contains(&MapKind::Displacement) {
                m.displacement_map.tex_id = 0;
            }
            if ch_maps.sss_color {
                m.sss_color_map.strength = MapKind::SssColor.default_strength();
            } else if !owned.contains(&MapKind::SssColor) {
                m.sss_color_map.tex_id = 0;
            }
        }
        // Avoid cloning multi-MB map slots when the graph is pure procedural (Noise etc.).
        let images = if graph_uses_image_texture(&graph) {
            self.snapshot_material_image_maps(mid, Some(&graph))
        } else {
            ImageMapSources::default()
        };
        let sdf = if graph_uses_sdf_field(&graph) {
            let mut s = self.snapshot_sdf_field_for_material(mid);
            // POLY CURVATURE: an SDF body answers through `body_prims`; a polygonal one has no
            // field at all and answers through a measured mesh instead. Attaching it here — the
            // one place the bake's inputs are gathered — keeps the worker's view uniform: it
            // receives a `SdfFieldSources` and never asks what kind of body produced it.
            if !s.has_field() {
                s.poly_curv = self.snapshot_poly_curv_source(mid);
            }
            s
        } else {
            SdfFieldSources::default()
        };
        let owner = GraphBakeOwner::Material(mid.0);
        let seq = self.cache.graph_bake_seq.wrapping_add(1);
        self.cache.graph_bake_seq = seq;
        self.cache.graph_bake_pending.insert(owner.key(), seq);
        // Coalesce gate for the live owner: Final from flush/auto-upgrade must also block
        // the next live kick until it lands (otherwise settle double-fires).
        self.cache.graph_live_in_flight = self.cache.graph_live_owner == Some(owner);
        // Persist editor graph so disk/session match what we bake.
        self.ui.material_node_graphs.insert(mid.0, graph.clone());
        // P2 (spec C1): Apply / preset / live-bake graph installs also change
        // channel ownership — reconcile the panel doc's GraphDriven states.
        self.mp_reconcile_graph_ownership(mid);
        // O4: take pending dirty rect (default Full) + prior maps for composite.
        let dirty = self
            .cache
            .graph_bake_dirty
            .remove(&owner.key())
            .unwrap_or(BakeDirty::Full);
        let prior = self.cache.graph_bake_map_cache.get(&owner.key()).cloned();
        // GPU CURVATURE: a graph with SDF-field nodes parks for one poll so `curvature_eval` can
        // run in the legal dispatch window; everything else spawns immediately, as before.
        // channel_filter: Live F2 single-channel scrub; Final settle after Live may re-finalize
        // only the soft channels (filter already taken into `ch` above; pass the same mask).
        let bake_filter = if ch.albedo == full_ch.albedo
            && ch.roughness == full_ch.roughness
            && ch.metallic == full_ch.metallic
            && ch.emission == full_ch.emission
            && ch.normal == full_ch.normal
            && ch.bump == full_ch.bump
            && ch.displacement == full_ch.displacement
            && ch.sss_color == full_ch.sss_color
        {
            None
        } else {
            Some(ch)
        };
        self.spawn_or_park_graph_bake(
            owner, seq, graph, quality, images, sdf, dirty, prior, bake_filter,
        );
        self.cache.prim_buf_stale = true;
        self.outliner.outliner_dirty = true;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// O4: next bake for this material re-evaluates the full frame (default after generator edits).
    pub(crate) fn mark_material_bake_dirty_full(&mut self, mid: u32) {
        self.cache
            .graph_bake_dirty
            .insert(GraphBakeOwner::Material(mid).key(), BakeDirty::Full);
    }

    /// O4: next bake re-evaluates only `rect` (work-res pixels), composited onto the prior cache.
    /// Generator/noise param changes must use [`Self::mark_material_bake_dirty_full`] instead.
    /// Full already pending wins (never demotes Full → Rect).
    pub(crate) fn mark_material_bake_dirty_rect(&mut self, mid: u32, rect: MapBakeRect) {
        let key = GraphBakeOwner::Material(mid).key();
        match self.cache.graph_bake_dirty.get_mut(&key) {
            None => {
                self.cache
                    .graph_bake_dirty
                    .insert(key, BakeDirty::Rect(rect));
            }
            Some(BakeDirty::Full) => { /* full wins */ }
            Some(BakeDirty::Rect(existing)) => {
                *existing = existing.union(rect);
            }
        }
    }

    /// Spawn the bake worker now, or park it for one poll so the GPU curvature kernel can run
    /// first (`GPU_CURVATURE_KERNEL_PLAN.md` P1, consumer A).
    ///
    /// Parking is the ONLY way to keep the Frame Ordering Contract: `enqueue_*` is reachable
    /// from panel draws (`win_node_editor`, `material_panel`, `properties_material_node`), where
    /// creating a GPU buffer or encoding a dispatch is forbidden. `poll_graph_curvature` drains
    /// this the very next poll.
    ///
    /// A bake is parked ONLY when all three hold — otherwise it spawns immediately with today's
    /// exact behavior (and `curv_caches` empty, so the worker runs the CPU path unchanged):
    ///   1. the graph actually uses the SDF-field nodes (`graph_uses_sdf_field`),
    ///   2. the snapshot has a field to evaluate (`has_field`),
    ///   3. the graph's curvature nodes have no wired Position/knob socket and no non-unit
    ///      Cavity scale (the cases a precomputed grid can never represent, single- or
    ///      multi-key) — `curvature_keys_for_graph` returns the (possibly empty, possibly
    ///      multi-entry) set of distinct knob-sets otherwise.
    /// GPU bake bridge (GPU_MATERIAL_BAKE_PLAN.md P1). `None` = caller keeps the CPU path.
    ///
    /// Produces a `BakedNodeMaps` at exactly `MAP_RES`, because
    /// `pending_loads_for_material_selective` length-checks each channel against
    /// `MAP_RES*MAP_RES*4` and SILENTLY DROPS a mismatch — a subtly wrong size would look like
    /// "the bake did nothing" rather than an error.
    ///
    /// Channels this path cannot author faithfully (normal / bump / displacement — absent from
    /// the 5-field `MatResult`, see `material_bake.wgsl`) are left EMPTY, never zero-filled. An
    /// empty Vec correctly reads as "this path did not author it" and preserves whatever the CPU
    /// path installed; a zero-filled buffer would pass the length check and silently wipe a real
    /// map (plan §P1c).
    ///
    /// **Blocking.** `bake_material_maps_gpu_blocking` uses `Maintain::Wait`. Used for Live
    /// scrub (short res) and Final when the graph is GPU-bakeable (T1 dual-plane). Prefer
    /// short Live res mid-drag; Final is still far cheaper on GPU than the CPU path.
    ///
    /// `channel_filter` is F2 (ADJUST_SLIDER_LAG): when `Some`, only those Master sockets are
    /// dispatched. Unrequested planes stay empty → install skips them (prior maps stay put).
    /// Returns `(pending, channels_actually_baked)`.
    fn try_gpu_bake_graph(
        &mut self,
        graph: &NodeGraph,
        quality: GraphBakeQuality,
        channel_filter: Option<BakeChannelMask>,
    ) -> Option<(crate::gpu_material_bake::PendingMaterialBake, BakeChannelMask)> {
        let (bits, baked) = gpu_bake_channel_plan(graph, channel_filter)?;

        let gpu = self.gpu.as_mut()?;
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let node_wgsl = graph.compile_wgsl();
        // Live → interactive res; Final → full work res (same rule as CPU).
        let res = bake_work_resolution_for(graph, matches!(quality, GraphBakeQuality::Live));

        // SUBMIT ONLY — returns immediately, no `Maintain::Wait`. `poll_graph_bakes` drains it.
        let pending = crate::gpu_material_bake::submit_material_bake_gpu(
            &device,
            &queue,
            &node_wgsl,
            res,
            bits,
            Some(&mut gpu.mat_bake_pipe_cache),
        )?;
        Some((pending, baked))
    }


    fn spawn_or_park_graph_bake(
        &mut self,
        owner: GraphBakeOwner,
        seq: u64,
        graph: NodeGraph,
        quality: GraphBakeQuality,
        images: ImageMapSources,
        sdf: SdfFieldSources,
        dirty: BakeDirty,
        prior: Option<BakedNodeMaps>,
        channel_filter: Option<BakeChannelMask>,
    ) {
        // ── GPU BAKE ROUTE (P1 + T1 dual-plane Final) ──────────────────────────────────────
        // Default ON for Live **and** Final when `graph_is_gpu_bakeable`. T1 taught the GPU
        // path to write dual-plane 16-bit (rgba16float linear → CPU encode), so Bit16 Final
        // is no longer refused. ImageTexture / curvature / etc. stay on the CPU path.
        //
        // Opt-out: `AZRA_GPU_BAKE=0`. Force log spam: `AZRA_GPU_BAKE=1`.
        // Fail-closed: whitelist miss or dispatch fail → CPU path, never wrong maps.
        //
        // O4: GPU path is full-frame only (no tile composite yet). Partial *spatial* dirty
        // still falls through to the CPU regional path so paint/mask rects stay correct.
        //
        // F2 / M1.a: channel_filter (single-channel scrub) is GPU-eligible. The dispatch
        // bits are graph-wired ∩ filter; unrequested planes decode to empty Vec so install
        // skips them — same contract as CPU F2. Requiring `channel_filter.is_none()` forced
        // every slider tick onto CPU (the most common edit path).
        let gpu_bake_env = std::env::var("AZRA_GPU_BAKE").ok();
        let gpu_bake_off = gpu_bake_env.as_deref() == Some("0");
        // M1 sabotage gate: force the CPU oracle path so the <33 ms live gate must fail.
        // `AZRA_FORCE_CPU_BAKE=1` is an alias for `AZRA_GPU_BAKE=0` (plan PLAN_FOR_GROK_MATERIALS M1).
        let force_cpu = std::env::var("AZRA_FORCE_CPU_BAKE").as_deref() == Ok("1");
        let allow_gpu = gpu_bake_path_eligible(dirty.is_full(), force_cpu, gpu_bake_off);
        if force_cpu {
            eprintln!("[GpuBake] AZRA_FORCE_CPU_BAKE=1 — CPU path forced (M1 sabotage)");
        }
        if allow_gpu {
            // M1.a: F2 channel scope — only reject nodes reachable from scrubbed sockets.
            if let Some(reason) = graph_gpu_bake_reject_reason_for(&graph, channel_filter) {
                gpu_bake_record_reject(reason);
            } else if let Some((pending, baked_ch)) =
                self.try_gpu_bake_graph(&graph, quality, channel_filter)
            {
                gpu_bake_record_ok();
                // PARK the flight instead of blocking on it. `poll_graph_bakes` drains it and sends
                // the result through `graph_bake_tx` — the same channel the CPU worker path uses, so
                // the consumer side is unchanged.
                let work_res =
                    bake_work_resolution_for(&graph, matches!(quality, GraphBakeQuality::Live));
                if channel_filter.is_some()
                    || std::env::var("AZRA_GPU_BAKE").as_deref() == Ok("1")
                {
                    let (cov_ok, cov_rej, cov_fail) = gpu_bake_coverage_counts();
                    eprintln!(
                        "[GpuBake] F2/GPU dispatch bits a={} r={} m={} e={} @{}² | coverage ok={cov_ok} reject={cov_rej} fail={cov_fail}",
                        baked_ch.albedo as u8,
                        baked_ch.roughness as u8,
                        baked_ch.metallic as u8,
                        baked_ch.emission as u8,
                        work_res
                    );
                }
                self.cache.graph_bake_gpu_inflight.push(InflightGpuBake {
                    pending,
                    owner,
                    seq,
                    graph,
                    quality,
                    work_res,
                    channels: baked_ch,
                    submitted: std::time::Instant::now(),
                });
                return;
            } else {
                gpu_bake_record_dispatch_fail();
                let (cov_ok, cov_rej, cov_fail) = gpu_bake_coverage_counts();
                eprintln!(
                    "[GpuBake] CPU fallback — graph is whitelist-eligible but GPU dispatch failed \
                     (validate/adapter or no GPU-authorable channels in filter) \
                     | coverage ok={cov_ok} reject={cov_rej} fail={cov_fail}"
                );
            }
        }

        let keys = if graph_uses_sdf_field(&graph) && sdf.has_field() {
            curvature_keys_for_graph(&graph)
        } else {
            None
        };
        let Some(keys) = keys.filter(|k| !k.is_empty()) else {
            spawn_graph_bake_quality_dirty_ch(
                self.cache.graph_bake_tx.clone(),
                owner, seq, graph, quality, images, sdf, dirty, prior, channel_filter,
            );
            return;
        };
        // Same work-res the bake itself will use (`bake_node_graph_maps_quality_with_mask`), so
        // every grid is 1:1 with the pixel loop and the fast-path lookup is an exact index.
        let res = snap_work_res(if graph.bake_res == 0 { BAKE_WORK_RES } else { graph.bake_res });
        self.cache.graph_curv_pending.push(PendingCurvBake {
            owner, seq, graph, quality, images, sdf, res, keys, dirty, prior, channel_filter,
        });
    }

    /// Clear baked map slots on an outliner Material when its graph is cleared.
    pub(crate) fn clear_material_node_graph_maps(
        &mut self,
        mid: core_math::primitives::MaterialId,
    ) {
        if let Some(m) = self.scene.scene_graph.material_by_id_mut(mid) {
            for kind in MapKind::ALL {
                m.map_mut(kind).tex_id = 0;
            }
        }
        self.cache
            .graph_bake_pending
            .remove(&GraphBakeOwner::Material(mid.0).key());
        self.cache.prim_buf_stale = true;
        self.outliner.outliner_dirty = true;
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }
}

