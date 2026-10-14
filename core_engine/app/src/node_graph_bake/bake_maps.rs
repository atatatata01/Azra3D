//! Map bake free functions: work-res policy, channel masks, pixel/region bake loop.
//!
//! Pure relocation from bake_dispatch — no logic change.
//! Rect-region + dirty-row parallel path + curvature key discovery live here.

use super::graph_eval::{
    compute_invariant_outs, def_f, eval_graph_sample_cached, is_input_connected,
    prepare_vertex_splat_caches, SdfFieldSources,
};
use super::packing::{
    actual_bake_res, final_install_res, snap_work_res, upsample_maps_selective,
    BakeDirty, MapBakeRect,
};
use super::texture_cache::ImageMapSources;
use super::{
    BakeChannelMask, BakedNodeMaps, GraphBakeQuality, BAKE_MAX_OCTAVES, BAKE_WORK_RES,
    MAX_CURV_KEYS_PER_BAKE,
};
use crate::material_textures::{self, DEFAULT_MAP_RES, MAP_RES, MAX_MAP_RES};
use crate::shader_nodes::{NodeGraph, NodeType};


pub(super) fn bake_work_resolution(graph: &NodeGraph) -> u32 {
    let requested = if graph.bake_res == 0 {
        if graph_uses_image_texture(graph) {
            MAP_RES
        } else {
            BAKE_WORK_RES
        }
    } else {
        graph.bake_res
    };
    snap_work_res(requested)
}

/// Preview resolution cap while scrubbing (P0c / ADJUST_SLIDER_LAG F1).
///
/// 256² is 1/16th the pixels of a 1024² bake — soft after upsample, but legible while dragging
/// contrast/sat/fill. Applies to **procedural and image** graphs during Live scrub; settled
/// Live auto-upgrades to Final (full target) so sharpness is restored on release.
pub(super) const BAKE_INTERACTIVE_RES: u32 = 256;

/// Work resolution for a bake, accounting for an in-progress interaction.
///
/// `interacting` is true for [`GraphBakeQuality::Live`] (slider scrub / gizmo drag). Cap
/// **all** graphs — including ImageTexture stacks — while scrubbing. A permanently soft photo
/// would look broken, but Live is always followed by Final on settle (`flush_live_graph_bake`
/// uses Final when no episode, and Live results with `work_res < target` auto-enqueue Final).
/// That settle path is what protects sharpness; paying full 1024²×N channels on every contrast
/// tick was the 1–2 s lag (ADJUST_SLIDER_LAG_2026-07-30).
pub(super) fn bake_work_resolution_for(graph: &NodeGraph, interacting: bool) -> u32 {
    let full = bake_work_resolution(graph);
    if interacting && full > BAKE_INTERACTIVE_RES {
        BAKE_INTERACTIVE_RES
    } else {
        full
    }
}

pub(super) fn graph_uses_image_texture(graph: &NodeGraph) -> bool {
    graph
        .nodes
        .iter()
        .any(|n| matches!(n.node_type, NodeType::ImageTexture))
}

pub(super) fn graph_uses_sdf_field(graph: &NodeGraph) -> bool {
    graph.nodes.iter().any(|n| {
        matches!(
            n.node_type,
            NodeType::PrimSdfMask
                | NodeType::SdfCurvature
                | NodeType::Curvature
                | NodeType::Cavity
                | NodeType::SurfaceDirection
                | NodeType::AmbientOcclusion
                | NodeType::SdfThickness
                | NodeType::SdfPosition
        )
    })
}

/// Bake at quality tier, then upsample to `MAP_RES`.
pub(crate) fn bake_node_graph_maps(graph: &NodeGraph) -> Option<BakedNodeMaps> {
    bake_node_graph_maps_quality(
        graph,
        GraphBakeQuality::Final,
        &ImageMapSources::default(),
        &SdfFieldSources::default(),
    )
}

pub(crate) fn bake_node_graph_maps_quality(
    graph: &NodeGraph,
    quality: GraphBakeQuality,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
) -> Option<BakedNodeMaps> {
    bake_node_graph_maps_quality_with_mask(graph, quality, images, sdf).map(|(m, _, _)| m)
}

/// Like [`bake_node_graph_maps_quality`] but also returns the channel mask that was baked
/// and the **work resolution before upsample** (for Live→Final skip).
/// Live = interactive work res (procedural only); Final = full work res. Both upsample to
/// [`final_install_res`] for the graph's bake target (O3: 1024 / 2048 / 4096).
pub(crate) fn bake_node_graph_maps_quality_with_mask(
    graph: &NodeGraph,
    quality: GraphBakeQuality,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
) -> Option<(BakedNodeMaps, BakeChannelMask, u32)> {
    bake_node_graph_maps_quality_with_mask_dirty(
        graph,
        quality,
        images,
        sdf,
        BakeDirty::Full,
        None,
    )
}

/// O4: same as [`bake_node_graph_maps_quality_with_mask`], with dirty-rect + prior maps.
///
/// When `dirty` is a partial rect and `prior_work` holds a previous bake at the same work
/// resolution, only the dirty region is re-evaluated and composited. Height→normal (Sobel)
/// without a height cache forces Full (margin alone is not enough without prior heights).
pub(crate) fn bake_node_graph_maps_quality_with_mask_dirty(
    graph: &NodeGraph,
    quality: GraphBakeQuality,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    dirty: BakeDirty,
    prior_work: Option<&BakedNodeMaps>,
) -> Option<(BakedNodeMaps, BakeChannelMask, u32)> {
    bake_node_graph_maps_quality_with_mask_dirty_ch(
        graph,
        quality,
        images,
        sdf,
        dirty,
        prior_work,
        None,
    )
}

/// Like [`bake_node_graph_maps_quality_with_mask_dirty`], with an optional channel filter
/// (ADJUST_SLIDER_LAG F2). `channel_filter` restricts which Master sockets are evaluated and
/// written; other map slots stay empty so install skips them (prior maps stay put).
pub(crate) fn bake_node_graph_maps_quality_with_mask_dirty_ch(
    graph: &NodeGraph,
    quality: GraphBakeQuality,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    dirty: BakeDirty,
    prior_work: Option<&BakedNodeMaps>,
    channel_filter: Option<BakeChannelMask>,
) -> Option<(BakedNodeMaps, BakeChannelMask, u32)> {
    // P0c: Live caps work res so panel/node scrub stays responsive; Final is full quality.
    // Settled Live auto-upgrades to Final only when Live was capped below the target.
    let work = bake_work_resolution_for(graph, matches!(quality, GraphBakeQuality::Live));
    let install = final_install_res(if graph.bake_res == 0 {
        // Image Mult default ceiling; pure procedural default work (still installs at DEFAULT).
        if graph_uses_image_texture(graph) {
            DEFAULT_MAP_RES
        } else {
            BAKE_WORK_RES
        }
    } else {
        graph.bake_res
    });
    let full_ch = channels_from_graph(graph);
    let ch = match channel_filter {
        Some(f) if f.any() => full_ch.and(f),
        _ => full_ch,
    };
    if !ch.any() {
        return None;
    }
    // Full octaves always — Noise node Octaves socket is the only control (1..12).
    let max_oct = BAKE_MAX_OCTAVES;

    // O4: resolve region; force Full when partial would be incorrect or useless.
    let mut dirty = dirty;
    let need_height_sobel = ch.normal
        && graph.master_socket_driven(crate::shader_nodes::NodeGraph::MO_HEIGHT)
        && !graph.master_socket_driven(crate::shader_nodes::NodeGraph::MO_NORMAL);
    if need_height_sobel && !dirty.is_full() {
        // No retained height_f in prior maps — Sobel needs neighbours. Full until we cache height.
        dirty = BakeDirty::Full;
    }
    // Prior only useful when channel buffers match work res.
    let prior_ok = prior_work.is_some_and(|p| {
        let side = actual_bake_res(p, ch);
        side == work && work > 0
    });
    if !prior_ok && !dirty.is_full() {
        // No base to composite into → must full-eval (zeros outside would install as black holes).
        dirty = BakeDirty::Full;
    }
    let region = dirty.resolve_eval_rect(work);
    let base = if dirty.is_full() || !prior_ok {
        None
    } else {
        prior_work
    };

    if !region.is_full(work) {
        let pct = region.pixel_count() as f64 * 100.0 / ((work as u64 * work as u64).max(1) as f64);
        eprintln!(
            "[BakeO4] dirty {}×{} @{},{} ({pct:.1}% of {work}²)",
            region.width(),
            region.height(),
            region.x0,
            region.y0
        );
    }

    bake_node_graph_maps_res_region(graph, work, ch, images, sdf, max_oct, region, base).map(
        |m| {
            // Upsample based on the buffer that was *actually* written — never trust `work`
            // alone (a past .min(512) clamp made work=1024 while maps were 512², then skipped
            // upsample and failed upload_map size check → Mult never reached the viewport).
            let actual = actual_bake_res(&m, ch);
            let work_res = if actual > 0 { actual } else { work };
            let maps = if actual >= install || actual == 0 {
                m
            } else {
                upsample_maps_selective(m, actual, install, ch)
            };
            (maps, ch, work_res)
        },
    )
}

pub(crate) fn channels_from_graph(graph: &NodeGraph) -> BakeChannelMask {
    use crate::shader_nodes::NodeGraph as NG;
    BakeChannelMask {
        albedo: (graph.master_socket_driven(NG::MO_ALBEDO)
            && graph.master_direct_constant_color(NG::MO_ALBEDO).is_none())
            || graph.master_socket_driven(NG::MO_AO),
        roughness: graph.master_socket_driven(NG::MO_ROUGHNESS),
        metallic: graph.master_socket_driven(NG::MO_METALLIC),
        emission: graph.master_socket_driven(NG::MO_EMISSION)
            || graph.master_socket_driven(NG::MO_EMISSION_INTENSITY),
        normal: graph.master_socket_driven(NG::MO_NORMAL) || graph.spatial_bump_strength(),
        bump: graph.master_socket_driven(NG::MO_HEIGHT) && !graph.spatial_bump_strength(),
        displacement: graph.master_socket_driven(NG::MO_DISPLACEMENT),
        sss_color: graph.master_socket_driven(NG::MO_SSS_COLOR),
    }
}

/// G4 prep: the BAKE mask = wired channels MINUS panel FAST channels. A Fast
/// channel (single plain image) installs its TexMap directly and must never
/// bake — baking would replace the native-resolution tint-Mult install with a
/// res-limited absolute composite (visible change + wasted work). Every bake
/// DECISION goes through this; `channels_from_graph` stays the raw wired-mask
/// for ownership/clear logic (a Fast install must keep its map slot).
pub(crate) fn bake_channels_from_graph(
    graph: &NodeGraph,
    doc: Option<&crate::material_panel::MaterialPanelDoc>,
) -> BakeChannelMask {
    use crate::material_panel::edit::MASK_KINDS;
    use crate::material_panel::lower::panel_fast_channel;
    let mut mask = channels_from_graph(graph);
    let ts = doc.map(|d| d.texture_space).unwrap_or_default();
    let slots: [&mut bool; 8] = [
        &mut mask.albedo,
        &mut mask.roughness,
        &mut mask.metallic,
        &mut mask.emission,
        &mut mask.normal,
        &mut mask.bump,
        &mut mask.displacement,
        &mut mask.sss_color,
    ];
    for (on, kind) in slots.into_iter().zip(MASK_KINDS) {
        if *on && panel_fast_channel(graph, kind, ts) {
            *on = false;
        }
    }
    mask
}

/// Master Material sockets needed for a bake mask (ADJUST_SLIDER_LAG F2 eval prune).
pub(super) fn master_sockets_for_mask(ch: BakeChannelMask) -> Vec<usize> {
    use crate::shader_nodes::NodeGraph as NG;
    let mut s = Vec::with_capacity(8);
    if ch.albedo {
        s.push(NG::MO_ALBEDO);
        s.push(NG::MO_AO);
    }
    if ch.roughness {
        s.push(NG::MO_ROUGHNESS);
    }
    if ch.metallic {
        s.push(NG::MO_METALLIC);
    }
    if ch.emission {
        s.push(NG::MO_EMISSION);
        s.push(NG::MO_EMISSION_INTENSITY);
    }
    if ch.normal {
        s.push(NG::MO_NORMAL);
        s.push(NG::MO_HEIGHT);
        s.push(NG::MO_BUMP_STRENGTH);
    }
    if ch.bump {
        s.push(NG::MO_HEIGHT);
    }
    if ch.displacement {
        s.push(NG::MO_DISPLACEMENT);
    }
    if ch.sss_color {
        s.push(NG::MO_SSS_COLOR);
    }
    s
}

/// Topo order that only walks Master sockets in `ch` (plus all internal edges of those
/// subgraphs). Avoids re-evaluating ImageTexture/noise on untouched channels during a
/// single-channel contrast scrub (ADJUST_SLIDER_LAG F2).
pub(super) fn topo_sort_for_mask(
    graph: &NodeGraph,
    output_id: crate::shader_nodes::NodeId,
    ch: BakeChannelMask,
) -> Option<Vec<crate::shader_nodes::NodeId>> {
    use crate::shader_nodes::NodeId;
    use std::collections::HashMap;
    let sockets = master_sockets_for_mask(ch);
    if sockets.is_empty() {
        return None;
    }
    let mut visited: HashMap<NodeId, u8> = HashMap::new();
    let mut order: Vec<NodeId> = Vec::new();

    fn visit(
        graph: &NodeGraph,
        nid: NodeId,
        at_output: bool,
        sockets: &[usize],
        visited: &mut HashMap<NodeId, u8>,
        order: &mut Vec<NodeId>,
    ) -> bool {
        match visited.get(&nid) {
            Some(1) => return false, // cycle
            Some(2) => return true,
            _ => {}
        }
        visited.insert(nid, 1);
        for conn in &graph.connections {
            if conn.to_node != nid {
                continue;
            }
            // At the MaterialOutput, only follow sockets we intend to bake.
            if at_output && !sockets.contains(&conn.to_input) {
                continue;
            }
            if !visit(graph, conn.from_node, false, sockets, visited, order) {
                return false;
            }
        }
        visited.insert(nid, 2);
        order.push(nid);
        true
    }

    if !visit(graph, output_id, true, &sockets, &mut visited, &mut order) {
        return None;
    }
    Some(order)
}

/// P2 bake-install ownership gate (spec C1, pure fn): zero out mask channels
/// whose panel doc state is `Layered` (panel-owned hand-loaded image) and return
/// the conflict bitmask (`ChannelKind as u32` bits) for the panel badge.
/// `None` doc → mask unchanged, no conflicts (legacy behavior).
/// `panel_generated` (P3): bitmask (`ChannelKind as u32` bits) of channels whose
/// master-socket subgraph was GENERATED BY THE PANEL (`panel_generated_mask`).
/// Those installs are the panel's own layer-stack composite landing in the slot
/// — allowed through the gate, no conflict. Only a HAND-authored graph baking
/// into a panel-owned slot is gated off.
pub(crate) fn gate_mask_by_panel_ownership(
    mut mask: BakeChannelMask,
    doc: Option<&crate::material_panel::MaterialPanelDoc>,
    panel_generated: u32,
) -> (BakeChannelMask, u32) {
    use crate::material_panel::edit::{doc_owns_channel, MASK_KINDS};
    use crate::material_panel::lower::channel_skip_planar_map_install;
    let mut conflicts = 0u32;
    let slots: [&mut bool; 8] = [
        &mut mask.albedo,
        &mut mask.roughness,
        &mut mask.metallic,
        &mut mask.emission,
        &mut mask.normal,
        &mut mask.bump,
        &mut mask.displacement,
        &mut mask.sss_color,
    ];
    for (on, kind) in slots.into_iter().zip(MASK_KINDS) {
        let bit = 1u32 << (kind as u32);
        if !*on {
            continue;
        }
        // Pure Vertex-only: no planar install. Mixed Noise+Vertex: allow noise bake.
        if let Some(ch) = doc.and_then(|d| d.channel(kind)) {
            if channel_skip_planar_map_install(ch) {
                *on = false;
                continue;
            }
        }
        if doc_owns_channel(doc, kind) && panel_generated & bit == 0 {
            *on = false;
            conflicts |= bit;
        }
    }
    (mask, conflicts)
}

/// Raw bake at `res²` (no upsample). Background thread only — never call on UI thread.
/// Skips writing channels that aren't wired (still evaluates the graph once per pixel).
/// `max_noise_octaves` caps FBM/Ridged cost (Noise node Octaves socket, hard max 12).
///
/// Cap is [`MAX_MAP_RES`] (O3): Image Mult stacks request full install res so base×noise is
/// evaluated at install resolution. A hard `.min(512)` left Mult buffers at 512² while the
/// quality path thought work==install and skipped upsample → `upload_map` rejected wrong size
/// → Mult never installed (pure map or pure live-noise REPLACE).
pub(crate) fn bake_node_graph_maps_res(
    graph: &NodeGraph,
    res: u32,
    ch: BakeChannelMask,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
) -> Option<BakedNodeMaps> {
    bake_node_graph_maps_res_region(
        graph,
        res,
        ch,
        images,
        sdf,
        max_noise_octaves,
        MapBakeRect::full(res.max(8).min(MAX_MAP_RES)),
        None,
    )
}

/// O4: bake only `region` (pixel AABB at `res`), optionally compositing into `base`.
///
/// * `region` must already include any filter margin ([`MAP_BAKE_FILTER_MARGIN`]).
/// * When `base` is `Some` and matches `res` for every wired channel, non-region pixels
///   are kept from `base` (dirty-rect update). When `None`, outside the region stays zero.
/// * Full-frame `region` is bit-identical to [`bake_node_graph_maps_res`].
pub(crate) fn bake_node_graph_maps_res_region(
    graph: &NodeGraph,
    res: u32,
    ch: BakeChannelMask,
    images: &ImageMapSources,
    sdf: &SdfFieldSources,
    max_noise_octaves: u32,
    region: MapBakeRect,
    base: Option<&BakedNodeMaps>,
) -> Option<BakedNodeMaps> {
    use crate::shader_nodes::NodeGraph as NG;
    let out = graph.output_node()?;
    // F2: only walk subgraphs feeding requested channels — full topo re-evals every
    // ImageTexture on every channel even when the user scrubbed contrast on Base Color alone.
    let order = topo_sort_for_mask(graph, out.id, ch)
        .or_else(|| graph.topo_sort(out.id))?;
    let out_id = out.id;
    let res = res.max(8).min(MAX_MAP_RES);
    let region = region.clamp(res);
    if region.is_empty() {
        // Nothing to do — return base clone or empty channel shells.
        return Some(base.cloned().unwrap_or(BakedNodeMaps {
            normal_is_dx: false, // stub: nothing synthesized
            albedo: Vec::new(),
            roughness: Vec::new(),
            metallic: Vec::new(),
            emission: Vec::new(),
            normal: Vec::new(),
            bump: Vec::new(),
            displacement: Vec::new(),
            sss_color: Vec::new(),
            albedo_lo: Vec::new(),
            roughness_lo: Vec::new(),
            metallic_lo: Vec::new(),
            emission_lo: Vec::new(),
            normal_lo: Vec::new(),
            bump_lo: Vec::new(),
            displacement_lo: Vec::new(),
            sss_color_lo: Vec::new(),
        }));
    }
    let n = (res * res) as usize;
    let expected = n * 4;
    let take_or_alloc = |on: bool, prior: &[u8]| -> Vec<u8> {
        if !on {
            return Vec::new();
        }
        if prior.len() == expected {
            prior.to_vec()
        } else {
            vec![0u8; expected]
        }
    };

    // BOTH MODES Phase 2: pre-build surface→map splat grids for Vertex-tagged curv nodes.
    // Clone so the caller's SdfFieldSources stays unchanged; workers may run in parallel.
    let mut sdf_owned;
    // POLY CURVATURE (P5): a polygonal body's grids are measured from its mesh, not projected
    // from a field, so this is the counterpart branch rather than an extra step inside the other.
    // Both produce ordinary `CurvGrid`s keyed by `CurvKey`; nothing downstream distinguishes them.
    let needs_poly = sdf.poly_curv.as_ref().is_some_and(|p| !p.is_empty());
    let sdf = if needs_poly {
        sdf_owned = sdf.clone();
        // Worker thread: reasons cannot toast from here, so they accumulate and are surfaced by
        // the poll phase when the result lands (see `bake_diag`).
        let mut notes = crate::bake_diag::BakeNotes::default();
        crate::poly_curvature::prepare_poly_curv_caches(graph, &mut sdf_owned, res, &mut notes);
        if !notes.is_empty() {
            crate::bake_diag::stash_worker_notes(notes);
        }
        &sdf_owned
    } else if !vertex_curv_keys_for_graph(graph).is_empty() && sdf.has_field() {
        sdf_owned = sdf.clone();
        prepare_vertex_splat_caches(graph, &mut sdf_owned, res);
        &sdf_owned
    } else {
        sdf
    };

    // ── CURVATURE VALUE PROBE (temporary, owner-facing 2026-07-28) ─────────────────────────
    // The owner reports curvature reads as a flat binary (white with a few black pockets), and
    // at Signed/radius 0.400 as pure white — while the shaping MATH predicts a strong gradient.
    // Reasoning about the pipeline has been wrong twice now, so this records what the bake
    // ACTUALLY produced instead of what it should produce. Written to a FILE because
    // Run_UIv2.bat does not capture stdout.
    let curv_probe_active = std::env::var("AZRA_CURV_PROBE").as_deref() == Ok("1");

    // ── SLOW-BAKE PROBE (temporary, owner-facing) ───────────────────────────────────────────
    // The owner reports "adding a generator takes ~45 s" recurring after the vertex-rung landed.
    // The 45 s signature is the CPU curvature fallback (~500 analytical evals/pixel). This
    // records, per bake, which curvature path each key took and how long the whole bake ran, to
    // a FILE (the console scrolls away and is not captured by Run_UIv2.bat). Remove once the
    // cause is identified — it is diagnostics, not a fix.
    let probe_t0 = std::time::Instant::now();
    let probe_keys: Vec<String> = curvature_keys_for_graph(graph)
        .map(|ks| ks.iter().map(|k| {
            let hit = sdf.curv_caches.iter().any(|g| g.key.matches(k) && g.res > 0);
            format!("eps={:.4} region={} prim={} -> {}", k.eps, k.region, k.prim_uid,
                    if hit { "GPU-grid" } else { "CPU-FALLBACK(slow)" })
        }).collect())
        .unwrap_or_else(|| vec!["INELIGIBLE-or-OVER-KEY-CAP -> CPU-FALLBACK(slow)".to_string()]);
    let need_height = ch.bump || (ch.normal && graph.master_socket_driven(NG::MO_HEIGHT));
    let normal_wired = graph.master_socket_driven(NG::MO_NORMAL) || graph.spatial_bump_strength();
    let height_wired = graph.master_socket_driven(NG::MO_HEIGHT);

    // Seed from prior when sizes match (O4 composite); else allocate zeros.
    // Each channel is dual-plane 16-bit (hi + lo); Bit8 installs use hi only.
    let (bp, empty) = match base {
        Some(b) => (b, false),
        None => (
            &BakedNodeMaps {
                normal_is_dx: false, // stub: nothing synthesized
                albedo: Vec::new(),
                roughness: Vec::new(),
                metallic: Vec::new(),
                emission: Vec::new(),
                normal: Vec::new(),
                bump: Vec::new(),
                displacement: Vec::new(),
                sss_color: Vec::new(),
                albedo_lo: Vec::new(),
                roughness_lo: Vec::new(),
                metallic_lo: Vec::new(),
                emission_lo: Vec::new(),
                normal_lo: Vec::new(),
                bump_lo: Vec::new(),
                displacement_lo: Vec::new(),
                sss_color_lo: Vec::new(),
            },
            true,
        ),
    };
    let _ = empty;
    let mut albedo = take_or_alloc(ch.albedo, &bp.albedo);
    let mut albedo_lo = take_or_alloc(ch.albedo, &bp.albedo_lo);
    let mut roughness = take_or_alloc(ch.roughness, &bp.roughness);
    let mut roughness_lo = take_or_alloc(ch.roughness, &bp.roughness_lo);
    let mut metallic = take_or_alloc(ch.metallic, &bp.metallic);
    let mut metallic_lo = take_or_alloc(ch.metallic, &bp.metallic_lo);
    let mut emission = take_or_alloc(ch.emission, &bp.emission);
    let mut emission_lo = take_or_alloc(ch.emission, &bp.emission_lo);
    let mut normal = take_or_alloc(ch.normal, &bp.normal);
    let mut normal_lo = take_or_alloc(ch.normal, &bp.normal_lo);
    let mut bump = take_or_alloc(ch.bump, &bp.bump);
    let mut bump_lo = take_or_alloc(ch.bump, &bp.bump_lo);
    let mut displacement = take_or_alloc(ch.displacement, &bp.displacement);
    let mut displacement_lo = take_or_alloc(ch.displacement, &bp.displacement_lo);
    let mut sss_color = take_or_alloc(ch.sss_color, &bp.sss_color);
    let mut sss_color_lo = take_or_alloc(ch.sss_color, &bp.sss_color_lo);
    let mut height_f = if need_height {
        vec![0.0f32; n]
    } else {
        Vec::new()
    };

    // ── P0a + O4: PARALLEL OVER DIRTY ROWS ──────────────────────────────────────────────────
    // Full-frame: every row. Partial: only `region.y0..region.y1`, and within each row only
    // `region.x0..region.x1`. Outside the region, prior pixels (if any) are left untouched.
    //
    // Row-parallel is safe by construction:
    //   • every output buffer is split with `par_chunks_mut(row_stride)`, so a row owns a DISJOINT
    //     `&mut` slice — no aliasing, no locks, no atomics;
    //   • every input (`graph`, `order`, `images`, `sdf`) is a SHARED reference to plain data;
    //   • `eval_graph_sample` is pure — pixel order does not affect any result.
    // The height→normal Sobel pass BELOW stays serial and also region-limited (with margin
    // already baked into `region` by the caller).
    {
        use rayon::prelude::*;
        let row = res as usize;
        let row4 = row * 4;
        let empty_rows = |v: &mut Vec<u8>| v.is_empty();
        let (a_on, r_on, m_on, e_on, b_on, d_on, n_on, h_on, s_on) = (
            !empty_rows(&mut albedo),
            !empty_rows(&mut roughness),
            !empty_rows(&mut metallic),
            !empty_rows(&mut emission),
            !empty_rows(&mut bump),
            !empty_rows(&mut displacement),
            !normal.is_empty(),
            !height_f.is_empty(),
            !empty_rows(&mut sss_color),
        );
        // Full-row chunks (all y), then we only process y in region.
        let a_rows: Vec<&mut [u8]> = if a_on { albedo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let a_lo_rows: Vec<&mut [u8]> = if a_on { albedo_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let r_rows: Vec<&mut [u8]> = if r_on { roughness.par_chunks_mut(row4).collect() } else { Vec::new() };
        let r_lo_rows: Vec<&mut [u8]> = if r_on { roughness_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let m_rows: Vec<&mut [u8]> = if m_on { metallic.par_chunks_mut(row4).collect() } else { Vec::new() };
        let m_lo_rows: Vec<&mut [u8]> = if m_on { metallic_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let e_rows: Vec<&mut [u8]> = if e_on { emission.par_chunks_mut(row4).collect() } else { Vec::new() };
        let e_lo_rows: Vec<&mut [u8]> = if e_on { emission_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let b_rows: Vec<&mut [u8]> = if b_on { bump.par_chunks_mut(row4).collect() } else { Vec::new() };
        let b_lo_rows: Vec<&mut [u8]> = if b_on { bump_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let d_rows: Vec<&mut [u8]> = if d_on { displacement.par_chunks_mut(row4).collect() } else { Vec::new() };
        let d_lo_rows: Vec<&mut [u8]> = if d_on { displacement_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let n_rows: Vec<&mut [u8]> = if n_on { normal.par_chunks_mut(row4).collect() } else { Vec::new() };
        let n_lo_rows: Vec<&mut [u8]> = if n_on { normal_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let s_rows: Vec<&mut [u8]> = if s_on { sss_color.par_chunks_mut(row4).collect() } else { Vec::new() };
        let s_lo_rows: Vec<&mut [u8]> = if s_on { sss_color_lo.par_chunks_mut(row4).collect() } else { Vec::new() };
        let h_rows: Vec<&mut [f32]> = if h_on { height_f.par_chunks_mut(row).collect() } else { Vec::new() };

        struct RowOut<'a> {
            a: Option<&'a mut [u8]>, a_lo: Option<&'a mut [u8]>,
            r: Option<&'a mut [u8]>, r_lo: Option<&'a mut [u8]>,
            m: Option<&'a mut [u8]>, m_lo: Option<&'a mut [u8]>,
            e: Option<&'a mut [u8]>, e_lo: Option<&'a mut [u8]>,
            b: Option<&'a mut [u8]>, b_lo: Option<&'a mut [u8]>,
            d: Option<&'a mut [u8]>, d_lo: Option<&'a mut [u8]>,
            n: Option<&'a mut [u8]>, n_lo: Option<&'a mut [u8]>,
            s: Option<&'a mut [u8]>, s_lo: Option<&'a mut [u8]>,
            h: Option<&'a mut [f32]>,
        }
        let mut rows: Vec<RowOut> = Vec::with_capacity(res as usize);
        let mut ai = a_rows.into_iter(); let mut ail = a_lo_rows.into_iter();
        let mut ri = r_rows.into_iter(); let mut ril = r_lo_rows.into_iter();
        let mut mi = m_rows.into_iter(); let mut mil = m_lo_rows.into_iter();
        let mut ei = e_rows.into_iter(); let mut eil = e_lo_rows.into_iter();
        let mut bi = b_rows.into_iter(); let mut bil = b_lo_rows.into_iter();
        let mut di = d_rows.into_iter(); let mut dil = d_lo_rows.into_iter();
        let mut ni = n_rows.into_iter(); let mut nil = n_lo_rows.into_iter();
        let mut si = s_rows.into_iter(); let mut sil = s_lo_rows.into_iter();
        let mut hi = h_rows.into_iter();
        for _ in 0..res {
            rows.push(RowOut {
                a: ai.next(), a_lo: ail.next(),
                r: ri.next(), r_lo: ril.next(),
                m: mi.next(), m_lo: mil.next(),
                e: ei.next(), e_lo: eil.next(),
                b: bi.next(), b_lo: bil.next(),
                d: di.next(), d_lo: dil.next(),
                n: ni.next(), n_lo: nil.next(),
                s: si.next(), s_lo: sil.next(),
                h: hi.next(),
            });
        }

        // T3: hoist per-pixel-invariant nodes (constants / pure math) once per bake.
        let (invariant_outs, varying_ids) =
            compute_invariant_outs(graph, &order, images, sdf, max_noise_octaves);
        let inv_ref = (&invariant_outs, varying_ids.as_slice());

        let rx0 = region.x0;
        let rx1 = region.x1;
        let ry0 = region.y0 as usize;
        let ry1 = region.y1 as usize;

        rows.into_par_iter().enumerate().for_each(|(y, mut out_row)| {
            // O4: skip rows outside the dirty rect entirely.
            if y < ry0 || y >= ry1 {
                return;
            }
            let y = y as u32;
            for x in rx0..rx1 {
                let u = (x as f32 + 0.5) / res as f32;
                let v = (y as f32 + 0.5) / res as f32;
                let p = [u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0];
                let nrm = [0.0, 0.0, 1.0];
                let s = eval_graph_sample_cached(
                    graph,
                    &order,
                    Some(inv_ref),
                    out_id,
                    p,
                    nrm,
                    0.0,
                    images,
                    sdf,
                    max_noise_octaves,
                );
                let i = (x * 4) as usize; // row-local byte offset
                let pi = x as usize;      // row-local pixel offset
                // Dual-plane 16-bit for every channel (hi + lo). Bit8 install uses hi only.
                if out_row.a.is_some() {
                    let ao = s.ao.clamp(0.0, 1.0);
                    let al = [
                        (s.albedo[0] * ao).clamp(0.0, 1.0),
                        (s.albedo[1] * ao).clamp(0.0, 1.0),
                        (s.albedo[2] * ao).clamp(0.0, 1.0),
                    ];
                    let (h, l) = material_textures::encode_srgb_color_rgba16_planes(al);
                    if let Some(dst) = out_row.a.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.a_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.r.is_some() {
                    let (h, l) = material_textures::encode_data_scalar_rgba16_planes(s.roughness);
                    if let Some(dst) = out_row.r.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.r_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.m.is_some() {
                    let (h, l) = material_textures::encode_data_scalar_rgba16_planes(s.metallic);
                    if let Some(dst) = out_row.m.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.m_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.e.is_some() {
                    let (h, l) = material_textures::encode_srgb_color_rgba16_planes([
                        s.emission[0],
                        s.emission[1],
                        s.emission[2],
                    ]);
                    if let Some(dst) = out_row.e.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.e_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.b.is_some() {
                    let (h, l) = material_textures::encode_data_scalar_rgba16_planes(s.height);
                    if let Some(dst) = out_row.b.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.b_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.d.is_some() {
                    let (h, l) =
                        material_textures::encode_data_scalar_rgba16_planes(s.displacement);
                    if let Some(dst) = out_row.d.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.d_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if normal_wired && out_row.n.is_some() {
                    let (h, l) = material_textures::encode_normal_rgba16_planes(s.normal);
                    if let Some(dst) = out_row.n.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.n_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if out_row.s.is_some() {
                    // SSS color — sRGB color plane, same encode as emission.
                    let (h, l) = material_textures::encode_srgb_color_rgba16_planes([
                        s.sss_color[0],
                        s.sss_color[1],
                        s.sss_color[2],
                    ]);
                    if let Some(dst) = out_row.s.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&h);
                    }
                    if let Some(dst) = out_row.s_lo.as_deref_mut() {
                        dst[i..i + 4].copy_from_slice(&l);
                    }
                }
                if let Some(dst) = out_row.h.as_deref_mut() {
                    dst[pi] = s.height;
                }
            }
        });
    }


    // Height → Normal: finite-difference the baked height buffer into a proper
    // tangent-space normal (OpenGL 0.5+0.5·n) only when no authored Normal is wired.
    let normal_from_image = {
        use crate::shader_nodes::NodeGraph as NG;
        use crate::shader_nodes::NodeType;
        graph
            .output_node()
            .and_then(|out| {
                graph
                    .connections
                    .iter()
                    .find(|c| c.to_node == out.id && c.to_input == NG::MO_NORMAL)
                    .and_then(|c| graph.find_node(c.from_node))
                    .map(|n| matches!(n.node_type, NodeType::ImageTexture))
            })
            .unwrap_or(false)
    };
    // The ONLY branch that produces DirectX-convention green. `normal_from_image`
    // (an authored map wired to MO_NORMAL, e.g. every library material) skips it and
    // passes the source pixels through unchanged, so those keep their own convention.
    let normal_is_dx = height_wired && ch.normal && !normal_wired && !height_f.is_empty() && !normal_from_image;
    if normal_is_dx {
        let strength = 4.0f32;
        // O4: only recompute normals inside the eval region (already margin-expanded).
        for y in region.y0..region.y1 {
            for x in region.x0..region.x1 {
                let pi = (y * res + x) as usize;
                let xm = if x > 0 { x - 1 } else { x };
                let xp = if x + 1 < res { x + 1 } else { x };
                let ym = if y > 0 { y - 1 } else { y };
                let yp = if y + 1 < res { y + 1 } else { y };
                let hx = height_f[(y * res + xp) as usize] - height_f[(y * res + xm) as usize];
                let hy = height_f[(yp * res + x) as usize] - height_f[(ym * res + x) as usize];
                let nvec = [-hx * strength, -hy * strength, 1.0];
                let i = pi * 4;
                let (h, l) = material_textures::encode_normal_rgba16_planes(nvec);
                normal[i..i + 4].copy_from_slice(&h);
                if !normal_lo.is_empty() {
                    normal_lo[i..i + 4].copy_from_slice(&l);
                }
            }
        }
    }

    // ── SLOW-BAKE PROBE (temporary) — see the matching block at the top of this function.
    {
        let ms = probe_t0.elapsed().as_secs_f64() * 1000.0;
        let mut line = format!(
            "[bake] res={res} region={}x{}@{},{} {:.0} ms",
            region.width(),
            region.height(),
            region.x0,
            region.y0,
            ms
        );
        if ms > 1500.0 { line.push_str("  <<< SLOW"); }
        // CURVATURE VALUE PROBE: what did the bake ACTUALLY write? A histogram distinguishes
        // "flat binary" (all 0 / all 255) from "smooth ramp" (populated midtones) from "blank"
        // (a single value) — three very different bugs that look similar on screen.
        if curv_probe_active {
            for (name, buf) in [("albedo", &albedo), ("roughness", &roughness)] {
                if buf.is_empty() { continue; }
                let mut mn = 255u8; let mut mx = 0u8; let mut sum = 0u64;
                let mut buckets = [0u32; 8];
                let n = buf.len() / 4;
                for px in buf.chunks_exact(4) {
                    let v = px[0];
                    mn = mn.min(v); mx = mx.max(v); sum += v as u64;
                    buckets[(v as usize * 8 / 256).min(7)] += 1;
                }
                let pct = |b: u32| (b as f64 * 100.0 / n.max(1) as f64);
                line.push_str(&format!(
                    "
         {name}: min={mn} max={mx} avg={:.1} | 0-31:{:.0}% .. 224-255:{:.0}% | mid(64-191)={:.0}%",
                    sum as f64 / n.max(1) as f64,
                    pct(buckets[0]), pct(buckets[7]),
                    pct(buckets[2]) + pct(buckets[3]) + pct(buckets[4]) + pct(buckets[5])
                ));
            }
        }
        for k in &probe_keys { line.push_str(&format!("\n         curv {k}")); }
        eprintln!("{line}");
        // Append to a stable path so the owner can hand over one file (console is not captured).
        let p = std::env::temp_dir().join("azra_bake_probe.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }

    Some(BakedNodeMaps {
        // Set ONLY by the height->normal synthesis above; an authored map passed
        // through keeps its own green convention.
        normal_is_dx,
        albedo,
        roughness,
        metallic,
        emission,
        normal,
        bump,
        displacement,
        sss_color,
        albedo_lo,
        roughness_lo,
        metallic_lo,
        emission_lo,
        normal_lo,
        bump_lo,
        displacement_lo,
        sss_color_lo,
    })
}

/// All DISTINCT curvature-node knob sets found in a graph (2026-07-27 ÔÇö was `curvature_key_for_
/// graph`, singular: a material with a Generator layer on more than one channel ÔÇö e.g. Roughness
/// via Curvature AND Bump via Cavity at a different radius, an entirely ordinary recipe ÔÇö has
/// two `SdfCurvature`/`Cavity` nodes with DIFFERENT knobs. The old single-key function returned
/// `None` the instant it saw a second disagreeing key, which fell EVERY sample of the ENTIRE
/// bake back to ~500 analytical evals/pixel ÔÇö the owner's ~45s bug. One grid per DISTINCT key
/// fixes this: two Generator layers now cost two dispatches, not zero.
///
/// Returns `None` only when a node is ineligible outright (wired Position/knob socket, or a
/// non-unit Cavity scale) ÔÇö those genuinely cannot be precomputed at all, on either the single-
/// or multi-key design, and must keep taking the CPU path for that one node.
///
/// Capped at `MAX_CURV_KEYS_PER_BAKE` distinct keys (Frame Budget Rule): a graph with more
/// distinct curvature knob-sets than that returns `None` rather than dispatching unboundedly
/// many grids in one poll ÔÇö an extreme case in practice, and it degrades to the CPU path exactly
/// as before P1 existed, never a crash or a dropped node.
pub(crate) fn curvature_keys_for_graph(
    graph: &NodeGraph,
) -> Option<Vec<crate::gpu_curvature::CurvKey>> {
    let mut found: Vec<crate::gpu_curvature::CurvKey> = Vec::new();
    for node in &graph.nodes {
        // Which sockets this node type reads, and the key it implies. Defaults and `.max()`
        // floors are copied VERBATIM from the matching `eval_node` arms (`NodeType::Curvature`
        // :1580, `SdfCurvature` :1604, `Cavity` :1646) — if those defaults ever change, the fast
        // path must stop claiming the grid, which is why the sample site re-checks the key.
        let (knob_sockets, key) = match node.node_type {
            NodeType::Curvature => (
                &[1usize, 2, 3][..],
                crate::gpu_curvature::CurvKey {
                    eps: def_f(node, 1, 0.02).max(1e-4),
                    prim_uid: 0,
                    region: 0,
                    grow: 0.0,
                    band: 0.2,
                    contrast: def_f(node, 2, 1.0).max(0.001),
                    strength: def_f(node, 3, 1.0),
                },
            ),
            NodeType::SdfCurvature => (
                &[1usize, 2, 3, 4, 5][..],
                crate::gpu_curvature::CurvKey {
                    eps: def_f(node, 1, 0.03).max(1e-4),
                    prim_uid: node.sdf_prim_uid,
                    region: node.sdf_curv_region,
                    grow: def_f(node, 2, 0.0),
                    band: def_f(node, 3, 0.15).max(1e-4),
                    contrast: def_f(node, 4, 1.0).max(0.001),
                    strength: def_f(node, 5, 1.0),
                },
            ),
            NodeType::Cavity => (
                &[1usize, 2][..],
                crate::gpu_curvature::CurvKey {
                    eps: def_f(node, 1, 0.02).max(1e-4),
                    prim_uid: 0,
                    region: 2,
                    grow: 0.0,
                    band: 0.2,
                    contrast: 1.0,
                    strength: 1.0,
                },
            ),
            _ => continue,
        };
        // A curvature node whose POSITION input (socket 0) is wired is not a function of the
        // pixel (plan §3.3) — CPU path in P1, so it must not claim a grid.
        if is_input_connected(graph, node.id, 0) {
            return None;
        }
        // A wired KNOB socket is data-dependent per pixel, so a precomputed grid cannot
        // represent it. `def_f` would silently read the stale literal underneath the wire.
        if knob_sockets.iter().any(|&s| is_input_connected(graph, node.id, s)) {
            return None;
        }
        // Cavity also SCALES its position before evaluating (`pos * scale`, :1654), which breaks
        // the pixel↔sample identity unless scale == 1.
        if matches!(node.node_type, NodeType::Cavity)
            && (def_f(node, 2, 1.0).max(0.01) - 1.0).abs() > f32::EPSILON
        {
            return None;
        }
        if !found.iter().any(|k| k.matches(&key)) {
            found.push(key);
        }
    }
    if found.len() > MAX_CURV_KEYS_PER_BAKE {
        return None;
    }
    Some(found)
}

/// The subset of `curvature_keys_for_graph`'s keys whose node is tagged `GeneratorRung::VertexCache`
/// (GPU_CURVATURE_KERNEL_PLAN.md P2/P3 — the panel's "Vertex" rung, `material_panel/lower.rs:685-
/// 699`/`:1078-1129`: a trailing `v` on the panel-tag flag). REUSE: the eligibility rules (no wired
/// Position/knob socket, unit Cavity scale) are identical to the texture rung's, so this walks the
/// SAME node list and asks the SAME per-node eligibility question as `curvature_keys_for_graph` —
/// it does not re-derive them, it filters by an ADDITIONAL label check.
///
/// A node whose knobs make it ineligible for a GPU key at all (wired Position, etc.) is silently
/// excluded here too — the vertex rung has no CPU fallback path of its own to hand it to (unlike
/// the texture rung), so an ineligible VertexCache-tagged node simply carries no per-vertex data
/// (the consumer treats "no matching `CurvVertexSlot`" as "nothing to show for this layer" — no
/// crash, no stale reuse, per `CurvKey::matches`).
///
/// Each key is paired with its BUFFER CHANNEL (1=signed curv, 2=convex mask, 3=concave mask — the
/// kernel's native `output_grid` layout `material_panel/lower.rs` already tags onto the node label
/// as `sg`/`cx`/`cc` for the panel's `GeneratorOutput` selector; Cavity has no selector and is
/// always the concave channel, matching its CPU eval at `:2014` which takes only that tuple slot).
/// This is what lets the G-buffer fragment shader honour the layer's own output choice instead of
/// hardcoding signed curvature for every Generator layer.
pub(crate) fn vertex_curv_keys_for_graph(
    graph: &NodeGraph,
) -> Vec<(crate::gpu_curvature::CurvKey, u32)> {
    let Some(all_keys) = curvature_keys_for_graph(graph) else { return Vec::new(); };
    let mut out: Vec<(crate::gpu_curvature::CurvKey, u32)> = Vec::new();
    for node in &graph.nodes {
        if !matches!(node.node_type, NodeType::SdfCurvature | NodeType::Cavity | NodeType::Curvature) {
            continue;
        }
        let flag = crate::material_panel::lower::parse_panel_tag(&node.label).and_then(|(.., f)| f);
        let Some(flag) = flag else { continue; };
        if !flag.ends_with('v') {
            continue;
        }
        // Re-derive this node's key with the SAME expressions `curvature_keys_for_graph` uses,
        // then confirm it actually survived that function's eligibility gates (present in
        // `all_keys`) before claiming it — this avoids a second, possibly-drifting inline copy of
        // the ineligibility rules while still only reusing keys the GPU path can legally serve.
        let (key, out_channel) = match node.node_type {
            NodeType::Curvature => (crate::gpu_curvature::CurvKey {
                eps: def_f(node, 1, 0.02).max(1e-4), prim_uid: 0, region: 0,
                grow: 0.0, band: 0.2, contrast: def_f(node, 2, 1.0).max(0.001), strength: def_f(node, 3, 1.0),
            }, 1u32),
            NodeType::SdfCurvature => {
                // flag is one of "sgv"/"cxv"/"ccv" (see lower.rs:705-710) — strip the trailing 'v'
                // to recover the panel's GeneratorOutput selector.
                let channel = match flag.trim_end_matches('v') {
                    "cx" => 2u32,
                    "cc" => 3u32,
                    _ => 1u32, // "sg" (Signed) and any unrecognised tag default to signed curv
                };
                (crate::gpu_curvature::CurvKey {
                    eps: def_f(node, 1, 0.03).max(1e-4), prim_uid: node.sdf_prim_uid, region: node.sdf_curv_region,
                    grow: def_f(node, 2, 0.0), band: def_f(node, 3, 0.15).max(1e-4),
                    contrast: def_f(node, 4, 1.0).max(0.001), strength: def_f(node, 5, 1.0),
                }, channel)
            }
            NodeType::Cavity => (crate::gpu_curvature::CurvKey {
                eps: def_f(node, 1, 0.02).max(1e-4), prim_uid: 0, region: 2,
                grow: 0.0, band: 0.2, contrast: 1.0, strength: 1.0,
            }, 3u32), // Cavity is always concave (CPU eval at :2014 keeps only that channel)
            _ => continue,
        };
        if all_keys.iter().any(|k| k.matches(&key)) && !out.iter().any(|(k, _)| k.matches(&key)) {
            out.push((key, out_channel));
        }
    }
    out
}

