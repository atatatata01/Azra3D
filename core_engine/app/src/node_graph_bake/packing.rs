//! Map packing: dirty rects, tile grid, work/install resolution, bilinear upsample.
//!
//! Pure relocation from monolithic node_graph_bake — no logic change.

use super::{BakeChannelMask, BakedNodeMaps};
use crate::material_textures::DEFAULT_MAP_RES;

/// Tile edge for O4 dirty-rect material bakes (matches common 4K = 16×16 tiles).
pub(crate) const MAP_BAKE_TILE: u32 = 256;

/// Margin (pixels) expanded around a dirty rect so height→normal Sobel (and any
/// 3-tap spatial filter) never bands at the rect edge. Plan §O4 tile seams.
pub(crate) const MAP_BAKE_FILTER_MARGIN: u32 = 1;

/// Pixel AABB in bake space: inclusive min, exclusive max (`x0..x1`, `y0..y1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MapBakeRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl MapBakeRect {
    pub(crate) fn full(res: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: res,
            y1: res,
        }
    }

    pub(crate) fn clamp(self, res: u32) -> Self {
        Self {
            x0: self.x0.min(res),
            y0: self.y0.min(res),
            x1: self.x1.min(res).max(self.x0.min(res)),
            y1: self.y1.min(res).max(self.y0.min(res)),
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    pub(crate) fn is_full(self, res: u32) -> bool {
        self.x0 == 0 && self.y0 == 0 && self.x1 == res && self.y1 == res
    }

    pub(crate) fn width(self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub(crate) fn height(self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    pub(crate) fn pixel_count(self) -> u64 {
        self.width() as u64 * self.height() as u64
    }

    /// Expand by `m` pixels on each side, clamped to `[0, res)`.
    pub(crate) fn expand(self, m: u32, res: u32) -> Self {
        Self {
            x0: self.x0.saturating_sub(m),
            y0: self.y0.saturating_sub(m),
            x1: (self.x1 + m).min(res),
            y1: (self.y1 + m).min(res),
        }
        .clamp(res)
    }

    pub(crate) fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// What must be re-evaluated for the next material map bake (O4).
///
/// Conservative default is [`BakeDirty::Full`]. Generator / noise param edits must
/// stay Full (not spatially local). Mask/paint (or an explicit rect API) may use
/// [`BakeDirty::Rect`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BakeDirty {
    Full,
    /// Pixel rect in the **work-resolution** bake grid (before install upsample).
    Rect(MapBakeRect),
}

impl Default for BakeDirty {
    fn default() -> Self {
        BakeDirty::Full
    }
}

impl BakeDirty {
    /// Resolve to a work-space rect, expanding for spatial filters when partial.
    pub(crate) fn resolve_eval_rect(&self, work_res: u32) -> MapBakeRect {
        match self {
            BakeDirty::Full => MapBakeRect::full(work_res),
            BakeDirty::Rect(r) => r
                .clamp(work_res)
                .expand(MAP_BAKE_FILTER_MARGIN, work_res),
        }
    }

    pub(crate) fn is_full(&self) -> bool {
        matches!(self, BakeDirty::Full)
    }
}

/// Fixed tile grid covering `res` (last tiles may be smaller). Used for dirty union.
pub(crate) fn map_bake_tiles(res: u32) -> Vec<MapBakeRect> {
    let res = res.max(1);
    let t = MAP_BAKE_TILE.max(1);
    let mut out = Vec::new();
    let mut y = 0u32;
    while y < res {
        let y1 = (y + t).min(res);
        let mut x = 0u32;
        while x < res {
            let x1 = (x + t).min(res);
            out.push(MapBakeRect {
                x0: x,
                y0: y,
                x1,
                y1,
            });
            x = x1;
        }
        y = y1;
    }
    out
}

/// Union of tiles that intersect `rect` (for multi-stroke dirty accumulation).
pub(crate) fn tiles_covering_rect(rect: MapBakeRect, res: u32) -> MapBakeRect {
    let rect = rect.clamp(res);
    if rect.is_empty() {
        return rect;
    }
    let t = MAP_BAKE_TILE.max(1);
    let x0 = (rect.x0 / t) * t;
    let y0 = (rect.y0 / t) * t;
    let x1 = ((rect.x1 + t - 1) / t * t).min(res);
    let y1 = ((rect.y1 + t - 1) / t * t).min(res);
    MapBakeRect { x0, y0, x1, y1 }.clamp(res)
}

/// Snap bake work resolution to UI steps (panel Bake target + Master Material node).
/// M4 / D3: snaps through 2K ([`PRODUCT_MAP_CEILING`]); 4K only when
/// [`product_map_ceiling`] returns 4096 (`AZRA_ALLOW_4K_MAPS=1`).
pub(crate) fn snap_work_res(raw: u32) -> u32 {
    let ceil = crate::material_textures::product_map_ceiling();
    match raw.clamp(64, ceil) {
        0..=96 => 64,
        97..=192 => 128,
        193..=384 => 256,
        385..=768 => 512,
        769..=1536 => 1024,
        1537..=3072 => 2048.min(ceil),
        _ => ceil,
    }
}

/// Final install resolution for a bake target (O3 + M4).
///
/// Low targets (128–512) still soft-upsample to [`DEFAULT_MAP_RES`] (legacy 1024 install).
/// Targets at or above 1024 install at that size up to the **product ceiling** (2K default).
pub(crate) fn final_install_res(bake_target: u32) -> u32 {
    let t = snap_work_res(if bake_target == 0 {
        DEFAULT_MAP_RES
    } else {
        bake_target
    });
    t.max(DEFAULT_MAP_RES)
        .min(crate::material_textures::product_map_ceiling())
}

/// Side length of a baked channel buffer (`len/4` is square pixels), or 0 if empty.
pub(super) fn actual_bake_res(m: &BakedNodeMaps, ch: BakeChannelMask) -> u32 {
    let n = if ch.albedo && !m.albedo.is_empty() {
        m.albedo.len() / 4
    } else if ch.roughness && !m.roughness.is_empty() {
        m.roughness.len() / 4
    } else if ch.metallic && !m.metallic.is_empty() {
        m.metallic.len() / 4
    } else if ch.emission && !m.emission.is_empty() {
        m.emission.len() / 4
    } else if ch.normal && !m.normal.is_empty() {
        m.normal.len() / 4
    } else if ch.sss_color && !m.sss_color.is_empty() {
        m.sss_color.len() / 4
    } else {
        0
    };
    if n == 0 {
        return 0;
    }
    // Exact square side (bake always writes res²).
    let side = (n as f64).sqrt() as u32;
    if (side as usize) * (side as usize) == n {
        side
    } else {
        0
    }
}

/// Bilinear upsample for baked channels only. Unbaked stay empty.
pub(super) fn upsample_maps_selective(
    m: BakedNodeMaps,
    src_res: u32,
    dst_res: u32,
    ch: BakeChannelMask,
) -> BakedNodeMaps {
    if src_res == dst_res {
        return m;
    }
    let up = |src: Vec<u8>, on: bool| -> Vec<u8> {
        if !on || src.is_empty() {
            Vec::new()
        } else {
            upsample_bilinear_rgba(&src, src_res, dst_res)
        }
    };
    BakedNodeMaps {
        // Resampling never changes the convention — carry the source's provenance.
        normal_is_dx: m.normal_is_dx,
        albedo: up(m.albedo, ch.albedo),
        roughness: up(m.roughness, ch.roughness),
        metallic: up(m.metallic, ch.metallic),
        emission: up(m.emission, ch.emission),
        normal: up(m.normal, ch.normal),
        bump: up(m.bump, ch.bump),
        displacement: up(m.displacement, ch.displacement),
        sss_color: up(m.sss_color, ch.sss_color),
        albedo_lo: up(m.albedo_lo, ch.albedo),
        roughness_lo: up(m.roughness_lo, ch.roughness),
        metallic_lo: up(m.metallic_lo, ch.metallic),
        emission_lo: up(m.emission_lo, ch.emission),
        normal_lo: up(m.normal_lo, ch.normal),
        bump_lo: up(m.bump_lo, ch.bump),
        displacement_lo: up(m.displacement_lo, ch.displacement),
        sss_color_lo: up(m.sss_color_lo, ch.sss_color),
    }
}

pub(super) fn upsample_bilinear_rgba(src: &[u8], src_res: u32, dst_res: u32) -> Vec<u8> {
    let sr = src_res as f32;
    let mut out = vec![0u8; (dst_res * dst_res * 4) as usize];
    let sample = |x: i32, y: i32, c: usize| -> f32 {
        let xx = x.clamp(0, src_res as i32 - 1) as u32;
        let yy = y.clamp(0, src_res as i32 - 1) as u32;
        src[((yy * src_res + xx) * 4 + c as u32) as usize] as f32
    };
    for y in 0..dst_res {
        let fy = (y as f32 + 0.5) * sr / dst_res as f32 - 0.5;
        let y0 = fy.floor() as i32;
        let ty = fy - y0 as f32;
        for x in 0..dst_res {
            let fx = (x as f32 + 0.5) * sr / dst_res as f32 - 0.5;
            let x0 = fx.floor() as i32;
            let tx = fx - x0 as f32;
            let di = ((y * dst_res + x) * 4) as usize;
            for c in 0..4 {
                let v00 = sample(x0, y0, c);
                let v10 = sample(x0 + 1, y0, c);
                let v01 = sample(x0, y0 + 1, c);
                let v11 = sample(x0 + 1, y0 + 1, c);
                let v0 = v00 + (v10 - v00) * tx;
                let v1 = v01 + (v11 - v01) * tx;
                out[di + c] = (v0 + (v1 - v0) * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

