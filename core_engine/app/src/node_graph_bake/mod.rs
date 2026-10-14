//! PR-VIS: bake a procedural `NodeGraph` to material map pixels so Hybrid SDF (DC mesh →
//! `gbuffer.wgsl` triplanar) can **show** the graph. Deferred `eval_node_material` is invisible
//! on raster pixels — maps are the visibility path (PROCEDURAL_TEXTURING_DESIGN).
//!
//! **v1 engine:** CPU graph eval on a **background thread** at `graph.bake_res` (64–512),
//! bilinear-upsampled to `MAP_RES` (1024) — **except** when the graph samples an ImageTexture
//! (Mult/Overlay stacks over a photo map): those bake at full `MAP_RES` so Mult cannot
//! destroy base sharpness via 256→1024 upsample. Live scrub and Apply use the same quality.
//! **No Rayon** — UI only clones the graph + spawns the job.
//!
//! **Domain:** planar UV sample `p = (u*2-1, v*2-1, 0)` for most generators.
//! **SdfCurvature** remaps that UV into the body AABB and projects onto the 3D
//! isosurface (triplanar seeds) so masks are surface curvature — not a plane silhouette.

use eframe::wgpu;
use crate::material_textures::{
    self, map_side_len, MapTarget, PendingMapLoad, DEFAULT_MAP_RES, MAP_RES, MAX_MAP_RES,
};
use crate::shader_nodes::{
    apply_noise_blend_curve, MathBinaryOp, MathUnaryOp, Node, NodeGraph, NodeId, NodeType,
    NoiseType, ProjectionStyle, SocketType, VectorMathOp,
};
use core_math::pbr_material::MapKind;
use glam::Vec3;
use std::collections::HashMap;
use std::sync::mpsc;

// Map pixel bake + channel/topo + curvature-key discovery (rect-region math).
mod bake_maps;
pub(crate) use bake_maps::{
    bake_node_graph_maps, bake_node_graph_maps_quality,
    bake_node_graph_maps_quality_with_mask, bake_node_graph_maps_quality_with_mask_dirty,
    bake_node_graph_maps_quality_with_mask_dirty_ch, bake_node_graph_maps_res,
    bake_node_graph_maps_res_region, bake_channels_from_graph, channels_from_graph,
    curvature_keys_for_graph,
    gate_mask_by_panel_ownership, vertex_curv_keys_for_graph,
};

// GPU eligibility, spawn workers, AzraApp enqueue/schedule.
mod bake_dispatch;
pub(crate) use bake_dispatch::{
    gpu_bake_channel_plan, gpu_bake_path_eligible,
    graph_gpu_bake_reject_reason, graph_gpu_bake_reject_reason_for, graph_is_gpu_bakeable,
    spawn_graph_bake, spawn_graph_bake_quality, spawn_graph_bake_quality_dirty,
    spawn_graph_bake_quality_dirty_ch, GPU_INELIGIBLE, InFlightCurvBake, PendingCurvBake,
};

// Poll/readback/curvature drain — AzraApp methods pure-relocated from bake_dispatch.
// Module loaded so its `impl AzraApp` attaches (no re-exports).
mod bake_poll;

mod graph_eval;
pub(crate) use graph_eval::{
    build_vertex_splat_curv_grid, prepare_vertex_splat_caches, sdf_ambient_occlusion,
    sdf_curvature_masks, sdf_curvature_masks_ex, sdf_direction_mask, sdf_position_01,
    sdf_thickness, shape_curvature_channels, tangent_basis, render_noise_preview_rgba,
    SdfFieldSources, NO_FIELD_D,
};
use graph_eval::{
    compute_invariant_outs, def_f, def_v, eval_graph_sample, eval_graph_sample_cached,
    is_input_connected,
};

mod texture_cache;
pub(crate) use texture_cache::{
    pending_loads_for_material, pending_loads_for_material_selective,
    pending_loads_for_prim, ImageMapSources,
};
use texture_cache::{sample_map_rgb, srgb_u8_to_linear};

mod packing;
pub(crate) use packing::{
    final_install_res, map_bake_tiles, snap_work_res, tiles_covering_rect,
    BakeDirty, MapBakeRect, MAP_BAKE_FILTER_MARGIN, MAP_BAKE_TILE,
};
use packing::{actual_bake_res, upsample_maps_selective};

mod m4_scoreboard;
pub(crate) use m4_scoreboard::gpu_bake_coverage_counts;
#[cfg(test)]
pub(crate) use m4_scoreboard::gpu_bake_coverage_reset_for_test;
// pub(super) record helpers — visible to sibling modules and this parent
use m4_scoreboard::{
    gpu_bake_record_dispatch_fail, gpu_bake_record_ok, gpu_bake_record_reject,
};

#[cfg(test)]
use bake_maps::{
    bake_work_resolution, bake_work_resolution_for, graph_uses_image_texture, graph_uses_sdf_field,
    topo_sort_for_mask, BAKE_INTERACTIVE_RES,
};
pub(crate) use bake_dispatch::node_type_gpu_bakeable;
#[cfg(test)]
use graph_eval::eval_noise;


/// Default working bake resolution when graph has no explicit `bake_res`.
pub(crate) const BAKE_WORK_RES: u32 = 256;

/// Safety ceiling for FBM loops (UI is free-form; only prevents runaway cost).
pub(crate) const BAKE_MAX_OCTAVES: u32 = 64;

/// Cap on distinct `CurvKey`s a single bake will dispatch grids for (Frame Budget Rule — bounds
/// the per-poll GPU dispatch count same as `MAX_CURV_READBACKS_PER_FRAME` bounds the drain). A
/// real material's curvature/cavity node count is small (single digits); this is generous
/// headroom, not a soft-real limit a normal material would ever hit.
pub(crate) const MAX_CURV_KEYS_PER_BAKE: usize = 8;


/// Bake quality tier.
///
/// **Live ≠ Final resolution (P0c / ADJUST_SLIDER_LAG).** Live caps work res
/// (`BAKE_INTERACTIVE_RES`) for **all** graphs while scrubbing so panel sliders stay
/// responsive — including ImageTexture stacks. Final always uses the full work res.
/// A settled Live result auto-upgrades to Final (or flush uses Final when no episode)
/// so the soft preview is never permanent. Image graphs used to skip the cap because a
/// soft *settled* photo looks broken; that reason does not apply to a live scrub that
/// is always followed by a full-res re-bake on release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GraphBakeQuality {
    /// Knob scrub / graph edit — coalesced worker; interactive work res while scrubbing.
    Live,
    /// Explicit Apply / settle upgrade — full work resolution.
    Final,
}

/// Baked channel buffers ready to enqueue as map loads (install-res² after upsample).
/// Unwired channels stay empty so poll does not wipe prior maps.
#[derive(Clone)]
pub(crate) struct BakedNodeMaps {
    pub albedo: Vec<u8>,
    pub roughness: Vec<u8>,
    pub metallic: Vec<u8>,
    pub emission: Vec<u8>,
    pub normal: Vec<u8>,
    pub bump: Vec<u8>,
    pub displacement: Vec<u8>,
    /// SSS color (sRGB color plane, like emission). CPU bake only — the GPU bake has
    /// no sss plane and `gpu_bake_channel_plan` rejects sss-wired graphs to CPU.
    pub sss_color: Vec<u8>,
    /// Dual-plane low bytes for 16-bit (same length as the hi plane, or empty if 8-bit legacy).
    pub albedo_lo: Vec<u8>,
    pub roughness_lo: Vec<u8>,
    pub metallic_lo: Vec<u8>,
    pub emission_lo: Vec<u8>,
    pub normal_lo: Vec<u8>,
    pub bump_lo: Vec<u8>,
    pub displacement_lo: Vec<u8>,
    pub sss_color_lo: Vec<u8>,
    /// True when `normal` was SYNTHESIZED from height by `bake_maps` (which builds
    /// `[-hx, -hy, 1]` with y running down-image = DirectX green by construction).
    /// False when the bake passed an AUTHORED normal map through untouched — those
    /// keep whatever convention the source used, which for every bundled library
    /// material is OpenGL.
    ///
    /// This exists because `texture_cache` used to stamp `green_dx: kind ==
    /// MapKind::Normal` unconditionally, so re-baking a library material (a panel
    /// edit, Apply, a tiling scrub) re-flagged its GL map as DX and inverted its
    /// normals again — undoing the shader-side fix every time.
    pub normal_is_dx: bool,
}

/// Which Master sockets should install maps (wired only — one material rule).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BakeChannelMask {
    pub albedo: bool,
    pub roughness: bool,
    pub metallic: bool,
    pub emission: bool,
    pub normal: bool,
    pub bump: bool,
    pub displacement: bool,
    pub sss_color: bool,
}

impl BakeChannelMask {
    pub(crate) fn any(self) -> bool {
        self.albedo
            || self.roughness
            || self.metallic
            || self.emission
            || self.normal
            || self.bump
            || self.displacement
            || self.sss_color
    }

    /// Union — accumulate channels scrubbed during one live coalesce window.
    pub(crate) fn or(self, o: Self) -> Self {
        Self {
            albedo: self.albedo || o.albedo,
            roughness: self.roughness || o.roughness,
            metallic: self.metallic || o.metallic,
            emission: self.emission || o.emission,
            normal: self.normal || o.normal,
            bump: self.bump || o.bump,
            displacement: self.displacement || o.displacement,
            sss_color: self.sss_color || o.sss_color,
        }
    }

    /// Intersection — bake only channels that are both requested and graph-driven.
    pub(crate) fn and(self, o: Self) -> Self {
        Self {
            albedo: self.albedo && o.albedo,
            roughness: self.roughness && o.roughness,
            metallic: self.metallic && o.metallic,
            emission: self.emission && o.emission,
            normal: self.normal && o.normal,
            bump: self.bump && o.bump,
            displacement: self.displacement && o.displacement,
            sss_color: self.sss_color && o.sss_color,
        }
    }

    /// Single panel channel → bake mask (ADJUST_SLIDER_LAG F2).
    pub(crate) fn from_channel_kind(k: crate::material_panel::model::ChannelKind) -> Self {
        use crate::material_panel::model::ChannelKind;
        match k {
            ChannelKind::BaseColor => Self {
                albedo: true,
                ..Default::default()
            },
            ChannelKind::Roughness => Self {
                roughness: true,
                ..Default::default()
            },
            ChannelKind::Metallic => Self {
                metallic: true,
                ..Default::default()
            },
            ChannelKind::Emission => Self {
                emission: true,
                ..Default::default()
            },
            ChannelKind::Normal => Self {
                normal: true,
                ..Default::default()
            },
            ChannelKind::Bump => Self {
                bump: true,
                normal: true, // height→normal may co-bake
                ..Default::default()
            },
            ChannelKind::Displacement => Self {
                displacement: true,
                ..Default::default()
            },
            ChannelKind::SssColor => Self {
                sss_color: true,
                ..Default::default()
            },
            _ => Self::default(),
        }
    }
}

/// Completed background bake (drained in poll).
/// A GPU material bake that has been SUBMITTED but not yet read back.
///
/// Exists so `spawn_or_park_graph_bake` — which runs on the frame thread — can return immediately
/// instead of calling `device.poll(Maintain::Wait)` inline (pillar review 2026-07-30; the same freeze
/// class `gpu_curvature.rs` documents and `gpu_material_bake.rs`'s header forbids). `poll_graph_bakes`
/// peeks these each frame and converts settled ones into an ordinary `GraphBakeResult`.
///
/// The `graph` is carried because the finish half needs it for channel/resolution decisions — the
/// same values the blocking version read from its local scope.
pub(crate) struct InflightGpuBake {
    pub pending: crate::gpu_material_bake::PendingMaterialBake,
    pub owner: GraphBakeOwner,
    pub seq: u64,
    pub graph: NodeGraph,
    pub quality: GraphBakeQuality,
    pub work_res: u32,
    /// Channels actually requested of the GPU dispatch (graph-wired ∩ F2 filter).
    /// Install intersects this so unauthored slots are not wiped by a partial scrub bake.
    pub channels: BakeChannelMask,
    pub submitted: std::time::Instant,
}

pub(crate) struct GraphBakeResult {
    pub owner: GraphBakeOwner,
    /// Sequence token — stale results are dropped if a newer bake was requested for the same owner.
    pub seq: u64,
    pub maps: Option<BakedNodeMaps>,
    /// Channels that were actually evaluated/upsampled.
    pub channels: BakeChannelMask,
    /// Quality this result was baked at — a settled Live result auto-upgrades to Final
    /// (owner 2026-07-23: connecting a socket must fully apply, no Apply button required).
    pub quality: GraphBakeQuality,
    /// Work resolution actually used for this bake (before upsample to MAP_RES).
    /// Used to skip redundant Live→Final when already at the panel Bake N² target.
    pub work_res: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GraphBakeOwner {
    Material(u32),
    Prim(u32),
}

impl GraphBakeOwner {
    pub(crate) fn key(self) -> (u8, u32) {
        match self {
            GraphBakeOwner::Material(id) => (0, id),
            GraphBakeOwner::Prim(id) => (1, id),
        }
    }
}

/// One material sample at a body-local position (Master Material sockets).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SampledMat {
    pub albedo: [f32; 3],
    pub roughness: f32,
    pub metallic: f32,
    /// Emission color × intensity (already multiplied).
    pub emission: [f32; 3],
    pub height: f32,
    pub normal: [f32; 3],
    pub ao: f32,
    pub displacement: f32,
    /// Subsurface color (MO_SSS_COLOR socket).
    pub sss_color: [f32; 3],
}



#[cfg(test)]
include!("tests.rs");
