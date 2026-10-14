//! Texture map sources + pending map-load packing for baked channels.
//!
//! Pure relocation from monolithic node_graph_bake — no logic change.

use super::{BakeChannelMask, BakedNodeMaps};
use crate::material_textures::{self, map_side_len, MapTarget, PendingMapLoad};
use core_math::pbr_material::MapKind;
use std::collections::HashMap;

/// CPU snapshot of material map slots for ImageTexture bake sampling (wire_tag → MAP_RES RGBA).
/// Library + Load Image share the same slots as Properties — one texture system.
#[derive(Clone, Default)]
pub(crate) struct ImageMapSources {
    pub maps: HashMap<u32, Vec<u8>>,
    /// Pinned SOURCE images keyed by texture-array slot (`Node.image_tex_id`).
    /// Panel-generated layer stacks sample these so re-bakes never read back the
    /// composite that was installed into the channel's map slot (P3).
    pub by_tex: HashMap<u32, Vec<u8>>,
}

impl ImageMapSources {
    pub(super) fn get(&self, wire_tag: u32) -> Option<&[u8]> {
        self.maps
            .get(&wire_tag)
            .map(|v| v.as_slice())
            .filter(|v| map_side_len(v).is_some())
    }
}

/// sRGB byte → linear (Photoshop Mult / Mix color ops must run in linear light).
#[inline]
pub(super) fn srgb_u8_to_linear(b: u8) -> f32 {
    let s = b as f32 / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Bilinear sample square RGBA at UV (Repeat wrap) → RGB 0..1.
/// Side length is inferred from the buffer (O3: maps may be 1K/2K/4K).
/// `as_srgb_color`: true for albedo/emission file bytes (hardware sRGB array) → linear;
/// false for data maps already stored as pre-encoded linear-via-sRGB-trick.
pub(super) fn sample_map_rgb(pixels: &[u8], u: f32, v: f32, as_srgb_color: bool) -> [f32; 3] {
    let Some(map_res) = map_side_len(pixels) else {
        return [1.0, 1.0, 1.0];
    };
    let res = map_res as f32;
    let uf = u.rem_euclid(1.0) * res - 0.5;
    let vf = v.rem_euclid(1.0) * res - 0.5;
    let x0 = uf.floor() as i32;
    let y0 = vf.floor() as i32;
    let fx = uf - x0 as f32;
    let fy = vf - y0 as f32;
    let wrap = |i: i32| -> u32 {
        let r = map_res as i32;
        ((i % r) + r) as u32 % map_res
    };
    let pix = |x: i32, y: i32| -> [f32; 3] {
        let i = ((wrap(y) * map_res + wrap(x)) * 4) as usize;
        if as_srgb_color {
            [
                srgb_u8_to_linear(pixels[i]),
                srgb_u8_to_linear(pixels[i + 1]),
                srgb_u8_to_linear(pixels[i + 2]),
            ]
        } else {
            // Data map: stored bytes are pre-encoded so GPU sRGB decode ≈ linear raw.
            // Recover approximate linear with the inverse LUT path used by thumbs.
            let lut = material_textures::srgb_to_linear_byte_lut();
            [
                lut[pixels[i] as usize] as f32 / 255.0,
                lut[pixels[i + 1] as usize] as f32 / 255.0,
                lut[pixels[i + 2] as usize] as f32 / 255.0,
            ]
        }
    };
    let c00 = pix(x0, y0);
    let c10 = pix(x0 + 1, y0);
    let c01 = pix(x0, y0 + 1);
    let c11 = pix(x0 + 1, y0 + 1);
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        let a = c00[i] + (c10[i] - c00[i]) * fx;
        let b = c01[i] + (c11[i] - c01[i]) * fx;
        out[i] = a + (b - a) * fy;
    }
    out
}

/// Build PendingMapLoad entries for a prim's embedded material (MapTarget::Prim).
/// `body_nid` is unused by apply_map_slot today but kept for API symmetry.
pub(crate) fn pending_loads_for_prim(
    body_nid: core_math::scene_graph::NodeId,
    prim_uid: u32,
    maps: BakedNodeMaps,
) -> Vec<PendingMapLoad> {
    pending_loads(MapTarget::Prim(body_nid, prim_uid), prim_uid, "prim", maps)
}

/// Build PendingMapLoad entries for an outliner graph Material (`MapTarget::Graph`).
pub(crate) fn pending_loads_for_material(
    mid: core_math::primitives::MaterialId,
    maps: BakedNodeMaps,
) -> Vec<PendingMapLoad> {
    pending_loads_for_material_selective(
        mid,
        maps,
        BakeChannelMask {
            albedo: true,
            roughness: true,
            metallic: true,
            emission: true,
            normal: true,
            bump: true,
            displacement: true,
            sss_color: true,
        },
    )
}

/// Only upload channels whose Master Material sockets are wired. Unconnected channels stay
/// as Properties / Master scalars (one material — no solid bake map fighting the base color).
/// Empty pixel buffers (live-unbaked channels) are **skipped** so prior Apply maps stay put.
pub(crate) fn pending_loads_for_material_selective(
    mid: core_math::primitives::MaterialId,
    maps: BakedNodeMaps,
    ch: BakeChannelMask,
) -> Vec<PendingMapLoad> {
    let target = MapTarget::Graph(mid);
    let id = mid.0;
    // O3: accept any legal square (1024 / 2048 / 4096 …). Size mismatch used to silently
    // drop the channel — keep that soft-fail for garbage, not for higher bake ceilings.
    let mut out = Vec::with_capacity(7);
    // Read BEFORE the closure — the pushes move the pixel buffers out of `maps`.
    let normal_is_dx = maps.normal_is_dx;
    let mut push = |on: bool, kind: MapKind, pixels: Vec<u8>, pixels_lo: Vec<u8>, name: String| {
        if on && map_side_len(&pixels).is_some() {
            // Dual-plane lo present → 16-bit path; empty lo → 8-bit single plane.
            let high_precision = !pixels_lo.is_empty() && pixels_lo.len() == pixels.len();
            out.push(PendingMapLoad {
                target,
                kind,
                pixels,
                strength: kind.default_strength(),
                name,
                high_precision,
                pixels_lo,
                // Only a SYNTHESIZED (height-derived) normal is DirectX — bake_maps builds
                // it as [-hx, -hy, 1] with y down-image. An authored map passed through by
                // the bake keeps its own convention (GL for every bundled library material),
                // so stamping DX here re-inverted it on every re-bake.
                green_dx: kind == MapKind::Normal && normal_is_dx,
            });
        }
    };
    push(
        ch.albedo,
        MapKind::Albedo,
        maps.albedo,
        maps.albedo_lo,
        format!("proc_mat_{id}_albedo"),
    );
    push(
        ch.roughness,
        MapKind::Roughness,
        maps.roughness,
        maps.roughness_lo,
        format!("proc_mat_{id}_rough"),
    );
    push(
        ch.metallic,
        MapKind::Metallic,
        maps.metallic,
        maps.metallic_lo,
        format!("proc_mat_{id}_metal"),
    );
    push(
        ch.emission,
        MapKind::Emission,
        maps.emission,
        maps.emission_lo,
        format!("proc_mat_{id}_emission"),
    );
    push(
        ch.normal,
        MapKind::Normal,
        maps.normal,
        maps.normal_lo,
        format!("proc_mat_{id}_normal"),
    );
    push(
        ch.bump,
        MapKind::Bump,
        maps.bump,
        maps.bump_lo,
        format!("proc_mat_{id}_bump"),
    );
    push(
        ch.displacement,
        MapKind::Displacement,
        maps.displacement,
        maps.displacement_lo,
        format!("proc_mat_{id}_disp"),
    );
    push(
        ch.sss_color,
        MapKind::SssColor,
        maps.sss_color,
        maps.sss_color_lo,
        format!("proc_mat_{id}_ssscol"),
    );
    out
}

pub(super) fn pending_loads(
    target: MapTarget,
    id: u32,
    tag: &str,
    maps: BakedNodeMaps,
) -> Vec<PendingMapLoad> {
    // MapTarget is Copy (Graph/Prim/BodyMaterial). Skip empty (unbaked) channels.
    // O3: any legal square installs (array grows / upsamples in upload_map).
    let mut out = Vec::with_capacity(7);
    let normal_is_dx = maps.normal_is_dx;
    let mut push = |kind: MapKind, pixels: Vec<u8>, pixels_lo: Vec<u8>, name: String| {
        if map_side_len(&pixels).is_some() {
            let high_precision = !pixels_lo.is_empty() && pixels_lo.len() == pixels.len();
            out.push(PendingMapLoad {
                target,
                kind,
                pixels,
                strength: kind.default_strength(),
                name,
                high_precision,
                pixels_lo,
                // Same rule as pending_loads_for_material_selective: DX only when the bake
                // SYNTHESIZED the normal from height; authored maps keep their convention.
                green_dx: kind == MapKind::Normal && normal_is_dx,
            });
        }
    };
    push(
        MapKind::Albedo,
        maps.albedo,
        maps.albedo_lo,
        format!("proc_{tag}_{id}_albedo"),
    );
    push(
        MapKind::Roughness,
        maps.roughness,
        maps.roughness_lo,
        format!("proc_{tag}_{id}_rough"),
    );
    push(
        MapKind::Metallic,
        maps.metallic,
        maps.metallic_lo,
        format!("proc_{tag}_{id}_metal"),
    );
    push(
        MapKind::Emission,
        maps.emission,
        maps.emission_lo,
        format!("proc_{tag}_{id}_emission"),
    );
    push(
        MapKind::Normal,
        maps.normal,
        maps.normal_lo,
        format!("proc_{tag}_{id}_normal"),
    );
    push(
        MapKind::Bump,
        maps.bump,
        maps.bump_lo,
        format!("proc_{tag}_{id}_bump"),
    );
    push(
        MapKind::Displacement,
        maps.displacement,
        maps.displacement_lo,
        format!("proc_{tag}_{id}_disp"),
    );
    push(
        MapKind::SssColor,
        maps.sss_color,
        maps.sss_color_lo,
        format!("proc_{tag}_{id}_ssscol"),
    );
    out
}

