//! **Bake Mesh Maps** — bake a polygonal body's UV textures.
//!
//! Plan: `docs/planning/render/MESH_MAP_BAKER_PLAN.md` (v2).
//!
//! * **SelfMesh** (default): the post-modifier **display mesh** is rasterised into UV space.
//!   That is the owner's original "high rez = modifier stack" path.
//! * **HighPoly**: a separate scene body is projected onto that raster (G2). High samples
//!   are **added** to [`UvGeoBuffer`]; the low position/normal stay put (G3 needs them).
//!
//! Curvature is LOW-geo (κ of the rasterised display mesh). Position / Object-Normal / ID /
//! AO / Thickness read the projected high sample (which equals the low sample on self-bake).
//!
//! ## Frame Ordering
//!
//! `MaterialTextures::upload_map` creates GPU resources, so the bake cannot run in a menu
//! callback. The menu only sets [`PendingMeshMapBake`]; [`AzraApp::poll_mesh_map_bakes`] does
//! the work.

mod cage_preview;
pub(crate) mod gpu_rays;
mod image_panel;
mod maps;
mod poll;
mod project;
mod quality;
mod raster;
mod registry;
mod settings;
mod source;
pub(crate) mod udim;
mod ui;
mod uv_underlay;

pub(crate) use raster::UvGeoBuffer;
pub(crate) use registry::{
    reconnect_baked_images, BakedImage, BakedImageRegistry,
};
pub(crate) use cage_preview::{BakeCagePreview, BakeCagePreviewCache};
pub(crate) use settings::{BakeSettings, BakeSource};
pub(crate) use source::{PolyCurvSource, PolySampleSpace};
pub(crate) use uv_underlay::{
    UvUnderlayCache, UvUnderlayResolved, UvUnderlaySource,
};

use crate::bake_diag::BakeNotes;
use core_math::scene_graph::NodeId;

/// Which maps a bake should produce. Extending this is the way new mesh maps are added.
/// Bits are persisted UI state — append-only, never renumber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MeshMapKind {
    /// Signed surface curvature, convex-positive. Grey 0.5 = flat.
    Curvature,
    /// Ambient occlusion: 1 = open, 0 = fully enclosed.
    AmbientOcclusion,
    /// Thickness: solid behind the surface (AO fired along the inward normal).
    Thickness,
    /// Object-space position, normalised to the mesh AABB (RGB = XYZ in 0..1).
    Position,
    /// Object-space surface normal, `n * 0.5 + 0.5`. Bit 1<<4 — was labelled
    /// "WorldNormal"; the name was a correctness trap once high-poly lives in other nodes.
    ObjectNormal,
    /// One flat colour per polygroup.
    MaterialId,
    /// High-poly normal in the **low-poly** MikkT tangent frame. Bit 1<<6 — append-only.
    TangentNormal,
    /// Average unoccluded hemisphere direction in the low MikkT frame. Bit 1<<7.
    BentNormal,
}

impl MeshMapKind {
    pub fn label(self) -> &'static str {
        match self {
            MeshMapKind::Curvature => "Curvature",
            MeshMapKind::AmbientOcclusion => "AO",
            MeshMapKind::Thickness => "Thickness",
            MeshMapKind::Position => "Position",
            MeshMapKind::ObjectNormal => "ObjectNormal",
            MeshMapKind::MaterialId => "ID",
            MeshMapKind::TangentNormal => "TangentNormal",
            MeshMapKind::BentNormal => "BentNormal",
        }
    }

    /// Full "bake all" set, cheapest first so early results appear sooner.
    pub const ALL: &'static [MeshMapKind] = &[
        MeshMapKind::Position,
        MeshMapKind::ObjectNormal,
        MeshMapKind::TangentNormal,
        MeshMapKind::Curvature,
        MeshMapKind::MaterialId,
        MeshMapKind::BentNormal,
        MeshMapKind::AmbientOcclusion,
        MeshMapKind::Thickness,
    ];

    pub fn bit(self) -> u32 {
        match self {
            MeshMapKind::Curvature => 1 << 0,
            MeshMapKind::AmbientOcclusion => 1 << 1,
            MeshMapKind::Thickness => 1 << 2,
            MeshMapKind::Position => 1 << 3,
            MeshMapKind::ObjectNormal => 1 << 4,
            MeshMapKind::MaterialId => 1 << 5,
            MeshMapKind::TangentNormal => 1 << 6,
            MeshMapKind::BentNormal => 1 << 7,
        }
    }

    /// Inverse of [`bit`]. Unknown bits (future kinds) return `None`.
    pub fn from_bit(bit: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.bit() == bit)
    }

    pub fn hint(self) -> &'static str {
        match self {
            MeshMapKind::Curvature => {
                "Signed surface curvature. Grey = flat, white = convex edges, black = concave."
            }
            MeshMapKind::AmbientOcclusion => {
                "How enclosed each point is. Ray cast — the slowest map."
            }
            MeshMapKind::Thickness => {
                "How much solid lies behind the surface. Ray cast."
            }
            MeshMapKind::Position => "Object-space position, normalised to the mesh bounds.",
            MeshMapKind::ObjectNormal => {
                "Object-space surface normal (n·0.5+0.5). Not world-space."
            }
            MeshMapKind::MaterialId => {
                "One flat colour per polygroup, for masking by part. Needs polygroups."
            }
            MeshMapKind::TangentNormal => {
                "High-poly normal in the low-poly tangent frame. Engine-ready normal map."
            }
            MeshMapKind::BentNormal => {
                "Average unoccluded lighting direction. Tangent-space; tilts away from blockers."
            }
        }
    }

    pub fn needs_rays(self) -> bool {
        matches!(
            self,
            MeshMapKind::AmbientOcclusion
                | MeshMapKind::Thickness
                | MeshMapKind::BentNormal
        )
    }

    /// Vector maps that band in RGBA8. G7 writes these as 16-bit.
    pub fn wants_16(self) -> bool {
        matches!(
            self,
            MeshMapKind::Position
                | MeshMapKind::ObjectNormal
                | MeshMapKind::TangentNormal
                | MeshMapKind::BentNormal
        )
    }
}

/// Everything a bake needs, OWNED — so it can cross to a worker thread.
pub(crate) struct MeshMapJob {
    pub body: NodeId,
    pub src: PolyCurvSource,
    /// Polygroup id per RingMesh face index. Empty = no polygroups (ID map skipped).
    pub face_groups: Vec<u16>,
    pub settings: BakeSettings,
    /// High-poly soup in low-local space. `None` = self-bake.
    pub high: Option<project::HighTarget>,
    /// Optional mesh cage in low-local space. `None` = numeric Max Front/Rear only.
    pub cage: Option<project::CageTarget>,
}

pub(crate) struct BakedMapPixels {
    pub kind: MeshMapKind,
    pub hi: Vec<u8>,
    /// Dual-plane lo bytes when `BakeSettings.bit16` and [`MeshMapKind::wants_16`].
    pub lo: Option<Vec<u8>>,
    /// Mari UDIM index. `1001` when UDIM is off.
    pub udim: u32,
}

pub(crate) struct MeshMapResult {
    pub body: NodeId,
    pub pixels: Vec<BakedMapPixels>,
    pub notes: BakeNotes,
    pub ms: f32,
}

fn ray_map(
    src: &PolyCurvSource,
    geo: &UvGeoBuffer,
    inward: bool,
    rays: usize,
    gpu: bool,
    notes: &mut BakeNotes,
    high: Option<&project::HighTarget>,
) -> Option<Vec<u8>> {
    if gpu {
        if let Some(px) = gpu_rays::bake_occlusion(src, geo, inward, rays, notes, high) {
            notes.push("Bake Mesh Maps: GPU rays");
            return Some(px);
        }
        notes.push("Bake Mesh Maps: GPU rays unavailable — CPU");
    }
    maps::bake_occlusion_pixels(src, geo, inward, rays, notes, high)
}

fn bent_map(
    src: &PolyCurvSource,
    geo: &UvGeoBuffer,
    rays: usize,
    gpu: bool,
    notes: &mut BakeNotes,
    high: Option<&project::HighTarget>,
) -> Option<Vec<u8>> {
    if gpu {
        if let Some(px) = gpu_rays::bake_bent(src, geo, rays, notes, high) {
            notes.push("Bake Mesh Maps: GPU rays (bent)");
            return Some(px);
        }
        notes.push("Bake Mesh Maps: GPU rays unavailable — CPU");
    }
    maps::bake_bent_normal_pixels(src, geo, rays, notes, high)
}

fn bent_map_16(
    src: &PolyCurvSource,
    geo: &UvGeoBuffer,
    rays: usize,
    gpu: bool,
    notes: &mut BakeNotes,
    high: Option<&project::HighTarget>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if gpu {
        if let Some(px) = gpu_rays::bake_bent_16(src, geo, rays, notes, high) {
            notes.push("Bake Mesh Maps: GPU rays (bent 16)");
            return Some(px);
        }
        notes.push("Bake Mesh Maps: GPU rays unavailable — CPU");
    }
    maps::bake_bent_normal_pixels_16(src, geo, rays, notes, high)
}

impl MeshMapJob {
    /// CPU raster + optional GPU rays. No app state, no toasts.
    pub fn run(self) -> MeshMapResult {
        let t0 = std::time::Instant::now();
        let mut notes = BakeNotes::default();
        let mut pixels: Vec<BakedMapPixels> = Vec::new();
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f32() * 1000.0;

        let settings = self.settings.clamp();
        let ss = quality::effective_ss(settings.res_global, settings.supersample);
        let hi_res = settings.res_global * ss;
        let tiles: Vec<(u32, glam::Vec2, bool)> = if settings.udim {
            let found = udim::occupied_udim_tiles(&self.src);
            if found.is_empty() {
                notes.push("Bake Mesh Maps: UDIM on but no tiles in 0..10 — baking 1001");
                vec![(udim::UDIM_1001, glam::Vec2::ZERO, false)]
            } else {
                notes.push(format!("Bake Mesh Maps: UDIM {} tile(s)", found.len()));
                found
                    .into_iter()
                    .map(|(idx, tu, tv)| (idx, glam::Vec2::new(tu as f32, tv as f32), false))
                    .collect()
            }
        } else {
            vec![(udim::UDIM_1001, glam::Vec2::ZERO, true)]
        };
        for (tile_udim, origin, wrap) in tiles {
        // ONE rasterisation shared by every map — including curvature (G0).
        // G6: optionally at res×SS, then box-filter back to res_global.
        let mut geo = raster::rasterize_uv_geo_in(
            &self.src,
            hi_res,
            settings.curv_radius,
            origin,
            wrap,
        );
        let covered = (0..(geo.res * geo.res) as usize)
            .filter(|i| geo.covered(*i))
            .count();
        if covered == 0 {
            notes.push(format!(
                "Bake Mesh Maps: UDIM {tile_udim} covered no texels"
            ));
            continue;
        }
        project::apply_projection(
            &mut geo,
            self.high.as_ref(),
            settings.max_front,
            settings.max_rear,
            &mut notes,
            self.cage.as_ref(),
        );
        if self.high.is_none() {
            for i in 0..(geo.res * geo.res) as usize {
                if !geo.covered(i) || geo.face[i] == u32::MAX {
                    continue;
                }
                geo.high_group[i] = self
                    .face_groups
                    .get(geo.face[i] as usize)
                    .copied()
                    .unwrap_or(0);
            }
        }
        // Dilate at working res so `padding_px` survives the SS box filter as output texels.
        quality::dilate_uv_geo(&mut geo, settings.padding_px.saturating_mul(ss));
        if ss > 1 {
            geo = quality::box_downsample_uv_geo(geo, ss);
        }
        for kind in &settings.maps() {
            let bit16 = settings.bit16 && kind.wants_16();
            let baked = match kind {
                MeshMapKind::Curvature => maps::bake_curvature_pixels(&geo, &mut notes)
                    .map(|hi| BakedMapPixels {
                        kind: *kind,
                        hi,
                        lo: None,
                        udim: tile_udim,
                    }),
                MeshMapKind::AmbientOcclusion => ray_map(
                    &self.src,
                    &geo,
                    false,
                    settings.ao_rays as usize,
                    settings.gpu_rays,
                    &mut notes,
                    self.high.as_ref(),
                )
                .map(|hi| BakedMapPixels {
                    kind: *kind,
                    hi,
                    lo: None,
                    udim: tile_udim,
                }),
                MeshMapKind::Thickness => ray_map(
                    &self.src,
                    &geo,
                    true,
                    settings.ao_rays as usize,
                    settings.gpu_rays,
                    &mut notes,
                    self.high.as_ref(),
                )
                .map(|hi| BakedMapPixels {
                    kind: *kind,
                    hi,
                    lo: None,
                    udim: tile_udim,
                }),
                MeshMapKind::Position | MeshMapKind::ObjectNormal => {
                    if bit16 {
                        maps::bake_vector_pixels_16(&geo, *kind).map(|(hi, lo)| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: Some(lo),
                            udim: tile_udim,
                        })
                    } else {
                        maps::bake_vector_pixels(&geo, *kind).map(|hi| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: None,
                            udim: tile_udim,
                        })
                    }
                }
                MeshMapKind::TangentNormal => {
                    if bit16 {
                        maps::bake_tangent_normal_pixels_16(&geo).map(|(hi, lo)| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: Some(lo),
                            udim: tile_udim,
                        })
                    } else {
                        maps::bake_tangent_normal_pixels(&geo).map(|hi| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: None,
                            udim: tile_udim,
                        })
                    }
                }
                MeshMapKind::MaterialId => {
                    maps::bake_id_pixels(&self.face_groups, &geo, &mut notes).map(|hi| {
                        BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: None,
                            udim: tile_udim,
                        }
                    })
                }
                MeshMapKind::BentNormal => {
                    if bit16 {
                        bent_map_16(
                            &self.src,
                            &geo,
                            settings.ao_rays as usize,
                            settings.gpu_rays,
                            &mut notes,
                            self.high.as_ref(),
                        )
                        .map(|(hi, lo)| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: Some(lo),
                            udim: tile_udim,
                        })
                    } else {
                        bent_map(
                            &self.src,
                            &geo,
                            settings.ao_rays as usize,
                            settings.gpu_rays,
                            &mut notes,
                            self.high.as_ref(),
                        )
                        .map(|hi| BakedMapPixels {
                            kind: *kind,
                            hi,
                            lo: None,
                            udim: tile_udim,
                        })
                    }
                }
            };
            if let Some(b) = baked {
                pixels.push(b);
            }
        }
        } // tiles
        if pixels.is_empty() {
            notes.push("Bake Mesh Maps: the UV layout covered no texels - check the unwrap");
        }
        MeshMapResult {
            body: self.body,
            pixels,
            notes,
            ms: ms(t0),
        }
    }
}

/// Queued request, drained in the poll phase.
#[derive(Clone, Debug)]
pub(crate) struct PendingMeshMapBake {
    pub body: NodeId,
    pub settings: BakeSettings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use core_math::mesh_curvature::RingMesh;

    fn unit_quad_src() -> PolyCurvSource {
        // One XY quad, UV = 0..1. Corners have non-zero κ only after we fold a crease;
        // a flat quad is an honest grey curvature map (G0 peak-normalise → flat).
        PolyCurvSource {
            rings: RingMesh {
                positions: vec![
                    glam::Vec3::new(0.0, 0.0, 0.0),
                    glam::Vec3::new(1.0, 0.0, 0.0),
                    glam::Vec3::new(1.0, 1.0, 0.0),
                    glam::Vec3::new(0.0, 1.0, 0.0),
                ],
                face_start: vec![0, 4],
                face_ring: vec![0, 1, 2, 3],
            },
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            space: PolySampleSpace {
                projection: 1,
                tiling: 1.0,
                blend: 4.0,
                rot: [0, 0, 0],
            },
        }
    }

    /// GATE (G0) — curvature, position and coverage come from ONE raster.
    #[test]
    fn curvature_and_position_agree_on_coverage() {
        let src = unit_quad_src();
        let geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        let n = (geo.res * geo.res) as usize;
        let covered = (0..n).filter(|&i| geo.covered(i)).count();
        assert!(covered > 16, "quad must cover a real island, got {covered}");

        let mut notes = BakeNotes::default();
        let curv = maps::bake_curvature_pixels(&geo, &mut notes).expect("curvature");
        let pos = maps::bake_vector_pixels(&geo, MeshMapKind::Position).expect("position");
        assert_eq!(curv.len(), n * 4);
        assert_eq!(pos.len(), n * 4);

        for i in 0..n {
            let c_a = curv[i * 4 + 3];
            let p_a = pos[i * 4 + 3];
            if geo.covered(i) {
                assert_eq!(c_a, 255, "covered texel {i} must write curvature");
                assert_eq!(p_a, 255, "covered texel {i} must write position");
            } else {
                assert_eq!(c_a, 0, "uncovered texel {i} must stay empty in curvature");
                assert_eq!(p_a, 0, "uncovered texel {i} must stay empty in position");
            }
        }
    }

    #[test]
    fn mesh_map_kind_bits_are_stable() {
        assert_eq!(MeshMapKind::Curvature.bit(), 1 << 0);
        assert_eq!(MeshMapKind::AmbientOcclusion.bit(), 1 << 1);
        assert_eq!(MeshMapKind::Thickness.bit(), 1 << 2);
        assert_eq!(MeshMapKind::Position.bit(), 1 << 3);
        assert_eq!(MeshMapKind::ObjectNormal.bit(), 1 << 4);
        assert_eq!(MeshMapKind::MaterialId.bit(), 1 << 5);
        assert_eq!(MeshMapKind::TangentNormal.bit(), 1 << 6);
        assert_eq!(MeshMapKind::BentNormal.bit(), 1 << 7);
        assert_eq!(
            MeshMapKind::from_bit(1 << 7),
            Some(MeshMapKind::BentNormal)
        );
        assert!(MeshMapKind::from_bit(1 << 20).is_none());
    }

    /// GATE (G1) — the job honors `BakeSettings.res_global` (slot pixel count).
    #[test]
    fn job_honors_settings_resolution() {
        let mut settings = BakeSettings::default();
        settings.res_global = 64;
        settings.enabled_maps = MeshMapKind::Position.bit();
        let done = MeshMapJob {
            body: NodeId(1),
            src: unit_quad_src(),
            face_groups: vec![],
            settings,
            high: None,
            cage: None,
        }
        .run();
        assert_eq!(done.pixels.len(), 1, "only Position was enabled");
        assert_eq!(done.pixels[0].kind, MeshMapKind::Position);
        assert_eq!(done.pixels[0].hi.len(), 64 * 64 * 4);
        assert!(done.pixels[0].lo.is_none());
    }

    /// GATE (G7) — 16-bit Position writes a real lo plane; u16 quantization is finer than u8.
    #[test]
    fn bit16_position_has_lo_plane_and_finer_quant() {
        let src = unit_quad_src();
        let mut geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        crate::mesh_map_bake::project::apply_projection(
            &mut geo,
            None,
            0.0,
            0.0,
            &mut BakeNotes::default(),
            None,
        );
        let (hi, lo) = maps::bake_vector_pixels_16(&geo, MeshMapKind::Position).unwrap();
        assert_eq!(hi.len(), lo.len());
        let lo_energy: u32 = lo.iter().step_by(4).map(|b| *b as u32).sum();
        assert!(lo_energy > 0, "lo plane must carry low bytes, not sit empty");
        let v = 0.5f32 + 0.4 / 255.0;
        let e8 = ((v * 255.0).round() / 255.0 - v).abs();
        let e16 = ((v * 65535.0).round() / 65535.0 - v).abs();
        assert!(e16 < e8, "u16 step must beat u8 (e8={e8} e16={e16})");
    }

    #[test]
    fn job_bit16_emits_lo_plane() {
        let mut settings = BakeSettings::default();
        settings.res_global = 64;
        settings.enabled_maps = MeshMapKind::Position.bit();
        settings.bit16 = true;
        let done = MeshMapJob {
            body: NodeId(1),
            src: unit_quad_src(),
            face_groups: vec![],
            settings,
            high: None,
            cage: None,
        }
        .run();
        assert_eq!(done.pixels[0].hi.len(), 64 * 64 * 4);
        assert_eq!(
            done.pixels[0].lo.as_ref().map(|v| v.len()),
            Some(64 * 64 * 4)
        );
    }

    fn two_udim_src() -> PolyCurvSource {
        PolyCurvSource {
            rings: RingMesh {
                positions: vec![
                    glam::Vec3::new(0.0, 0.0, 0.0),
                    glam::Vec3::new(1.0, 0.0, 0.0),
                    glam::Vec3::new(1.0, 1.0, 0.0),
                    glam::Vec3::new(0.0, 1.0, 0.0),
                    glam::Vec3::new(2.0, 0.0, 1.0),
                    glam::Vec3::new(3.0, 0.0, 1.0),
                    glam::Vec3::new(3.0, 1.0, 1.0),
                    glam::Vec3::new(2.0, 1.0, 1.0),
                ],
                face_start: vec![0, 4, 8],
                face_ring: vec![0, 1, 2, 3, 4, 5, 6, 7],
            },
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(2.0, 0.0),
                Vec2::new(2.0, 1.0),
                Vec2::new(1.0, 1.0),
            ],
            space: PolySampleSpace {
                projection: 1,
                tiling: 1.0,
                blend: 4.0,
                rot: [0, 0, 0],
            },
        }
    }

    fn pos_z_mean(px: &[u8]) -> f32 {
        let mut s = 0u32;
        let mut n = 0u32;
        for c in px.chunks(4) {
            if c[3] == 0 {
                continue;
            }
            s += c[2] as u32;
            n += 1;
        }
        s as f32 / n.max(1) as f32
    }

    /// GATE (G7 UDIM) — two UV islands become two Position maps; Z does not mix.
    #[test]
    fn job_udim_splits_tiles() {
        let mut settings = BakeSettings::default();
        settings.res_global = 64;
        settings.enabled_maps = MeshMapKind::Position.bit();
        settings.udim = true;
        settings.padding_px = 0;
        let done = MeshMapJob {
            body: NodeId(1),
            src: two_udim_src(),
            face_groups: vec![],
            settings,
            high: None,
            cage: None,
        }
        .run();
        assert_eq!(done.pixels.len(), 2, "1001 + 1002");
        let a = done.pixels.iter().find(|p| p.udim == 1001).unwrap();
        let b = done.pixels.iter().find(|p| p.udim == 1002).unwrap();
        let z1 = pos_z_mean(&a.hi);
        let z2 = pos_z_mean(&b.hi);
        assert!(z1 < 80.0, "tile 1001 is z=0, mean Z byte={z1}");
        assert!(z2 > 180.0, "tile 1002 is z=1, mean Z byte={z2}");
    }

    /// GATE (G7 UDIM negative) — off = one wrapped map.
    #[test]
    fn job_without_udim_is_single_map() {
        let mut settings = BakeSettings::default();
        settings.res_global = 64;
        settings.enabled_maps = MeshMapKind::Position.bit();
        settings.udim = false;
        let done = MeshMapJob {
            body: NodeId(1),
            src: two_udim_src(),
            face_groups: vec![],
            settings,
            high: None,
            cage: None,
        }
        .run();
        assert_eq!(done.pixels.len(), 1);
        assert_eq!(done.pixels[0].udim, 1001);
    }
}
