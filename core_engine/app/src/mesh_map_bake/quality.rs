//! G6 quality: island dilation, output supersample, blue-noise AO rays.

use super::raster::UvGeoBuffer;
use glam::Vec3;

/// Hard ceiling for the SS working raster (D8: one shared layer_res; 4K×4 is not a CPU raster).
pub(crate) const SS_HI_RES_MAX: u32 = 4096;

/// Effective SS so `res_global * ss` never exceeds [`SS_HI_RES_MAX`].
pub(crate) fn effective_ss(res_global: u32, supersample: u32) -> u32 {
    let mut ss = match supersample {
        2 | 4 => supersample,
        _ => 1,
    };
    while ss > 1 && res_global.saturating_mul(ss) > SS_HI_RES_MAX {
        ss /= 2;
    }
    ss.max(1)
}

/// Bleed covered texels outward `passes` times (Writer::dilate, now on the shared buffer).
///
/// Each pass averages 8-connected already-covered neighbours into empty cells, then marks
/// those cells covered for the *next* pass only — one pass cannot flood the whole map.
/// UV wrap (`rem_euclid`) matches tiled unwraps.
pub(crate) fn dilate_uv_geo(buf: &mut UvGeoBuffer, passes: u32) {
    if passes == 0 {
        return;
    }
    let res = buf.res as i64;
    let n = (res * res) as usize;
    for _ in 0..passes {
        let cov: Vec<bool> = (0..n).map(|i| buf.covered(i)).collect();
        let pos = buf.position.clone();
        let nrm = buf.normal.clone();
        let kap = buf.kappa.clone();
        let tan = buf.low_tangent.clone();
        let bit = buf.low_bitangent.clone();
        let face = buf.face.clone();
        let hp = buf.high_position.clone();
        let hn = buf.high_normal.clone();
        let hf = buf.high_face.clone();
        let hg = buf.high_group.clone();
        let miss = buf.missed.clone();
        let mut grew = false;
        for y in 0..res {
            for x in 0..res {
                let i = (y * res + x) as usize;
                if cov[i] {
                    continue;
                }
                let mut cnt = 0.0f32;
                let mut ap = Vec3::ZERO;
                let mut an = Vec3::ZERO;
                let mut ak = 0.0f32;
                let mut at = Vec3::ZERO;
                let mut ab = Vec3::ZERO;
                let mut ahp = Vec3::ZERO;
                let mut ahn = Vec3::ZERO;
                let mut ff = u32::MAX;
                let mut hff = u32::MAX;
                let mut gg = 0u16;
                let mut mm = 0u8;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = (x + dx).rem_euclid(res);
                        let ny = (y + dy).rem_euclid(res);
                        let j = (ny * res + nx) as usize;
                        if !cov[j] {
                            continue;
                        }
                        ap += pos[j];
                        an += nrm[j];
                        ak += kap[j];
                        at += tan[j];
                        ab += bit[j];
                        ahp += hp[j];
                        ahn += hn[j];
                        ff = face[j];
                        hff = hf[j];
                        gg = hg[j];
                        mm = miss[j];
                        cnt += 1.0;
                    }
                }
                if cnt > 0.0 {
                    let inv = 1.0 / cnt;
                    buf.position[i] = ap * inv;
                    buf.normal[i] = (an * inv).normalize_or_zero();
                    buf.kappa[i] = ak * inv;
                    let t = (at * inv).normalize_or_zero();
                    let tan = core_math::mikkt::orthogonalize_tangent(buf.normal[i], t);
                    let sign = core_math::mikkt::tangent_handedness(buf.normal[i], tan, ab * inv);
                    buf.low_tangent[i] = tan;
                    buf.low_bitangent[i] =
                        core_math::mikkt::reconstruct_bitangent(buf.normal[i], tan, sign);
                    buf.high_position[i] = ahp * inv;
                    buf.high_normal[i] = (ahn * inv).normalize_or_zero();
                    buf.face[i] = ff;
                    buf.high_face[i] = hff;
                    buf.high_group[i] = gg;
                    buf.missed[i] = mm;
                    buf.coverage[i] = 1.0e-6;
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
}

/// Box-filter an `ss`× working raster down to `res/ss`. Coverage-weighted so empty
/// hi-res texels do not pull the island toward black (that is dilation's job).
pub(crate) fn box_downsample_uv_geo(buf: UvGeoBuffer, ss: u32) -> UvGeoBuffer {
    if ss <= 1 {
        return buf;
    }
    let hi = buf.res;
    let lo = hi / ss;
    if lo < 8 {
        return buf;
    }
    let ss = ss as usize;
    let hi_i = hi as usize;
    let lo_i = lo as usize;
    let n = lo_i * lo_i;
    let mut out = UvGeoBuffer {
        res: lo,
        position: vec![Vec3::ZERO; n],
        normal: vec![Vec3::ZERO; n],
        kappa: vec![0.0; n],
        coverage: vec![0.0; n],
        face: vec![u32::MAX; n],
        bounds: buf.bounds,
        high_position: vec![Vec3::ZERO; n],
        high_normal: vec![Vec3::ZERO; n],
        high_face: vec![u32::MAX; n],
        high_group: vec![0; n],
        missed: vec![0; n],
        miss_count: 0,
        low_tangent: vec![Vec3::ZERO; n],
        low_bitangent: vec![Vec3::ZERO; n],
    };
    for ly in 0..lo_i {
        for lx in 0..lo_i {
            let o = ly * lo_i + lx;
            let mut w = 0.0f32;
            let mut ap = Vec3::ZERO;
            let mut an = Vec3::ZERO;
            let mut ak = 0.0f32;
            let mut at = Vec3::ZERO;
            let mut ab = Vec3::ZERO;
            let mut ahp = Vec3::ZERO;
            let mut ahn = Vec3::ZERO;
            let mut ff = u32::MAX;
            let mut hff = u32::MAX;
            let mut gg = 0u16;
            let mut mm_acc = 0u32;
            let mut mm_n = 0u32;
            let mut best_c = 0.0f32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let i = (ly * ss + sy) * hi_i + (lx * ss + sx);
                    if !buf.covered(i) {
                        continue;
                    }
                    let c = buf.coverage[i];
                    ap += buf.position[i] * c;
                    an += buf.normal[i] * c;
                    ak += buf.kappa[i] * c;
                    at += buf.low_tangent[i] * c;
                    ab += buf.low_bitangent[i] * c;
                    ahp += buf.high_position[i] * c;
                    ahn += buf.high_normal[i] * c;
                    if c >= best_c {
                        best_c = c;
                        ff = buf.face[i];
                        hff = buf.high_face[i];
                        gg = buf.high_group[i];
                    }
                    mm_acc += buf.missed[i] as u32;
                    mm_n += 1;
                    w += c;
                }
            }
            if w <= 1.0e-8 {
                continue;
            }
            let inv = 1.0 / w;
            out.position[o] = ap * inv;
            out.normal[o] = (an * inv).normalize_or_zero();
            out.kappa[o] = ak * inv;
            let t = at * inv;
            let tan = core_math::mikkt::orthogonalize_tangent(out.normal[o], t);
            let sign = core_math::mikkt::tangent_handedness(out.normal[o], tan, ab * inv);
            out.low_tangent[o] = tan;
            out.low_bitangent[o] =
                core_math::mikkt::reconstruct_bitangent(out.normal[o], tan, sign);
            out.high_position[o] = ahp * inv;
            out.high_normal[o] = (ahn * inv).normalize_or_zero();
            out.face[o] = ff;
            out.high_face[o] = hff;
            out.high_group[o] = gg;
            out.missed[o] = if mm_n > 0 && mm_acc * 2 >= mm_n { 1 } else { 0 };
            out.coverage[o] = (w / (ss * ss) as f32).clamp(1.0e-6, 1.0);
        }
    }
    out.miss_count = out.missed.iter().map(|m| *m as u32).sum();
    out
}

/// Cosine-weighted hemisphere sample `ray_i` of `rays`, Cranley–Patterson rotated by `texel`.
/// Returns `(sin_theta, phi)` in the local +Z frame — same contract as the old golden spiral.
pub(crate) fn hemisphere_sample(ray_i: usize, rays: usize, texel: usize) -> (f32, f32) {
    let (u0, v0) = r2(ray_i as u32);
    let (du, dv) = hash2(texel as u32);
    let u = frac(u0 + du);
    let v = frac(v0 + dv);
    let sin_t = u.sqrt();
    let phi = v * std::f32::consts::TAU;
    let _ = rays;
    (sin_t, phi)
}

/// R2 (plastic) low-discrepancy pair — blue-noise-like on the unit square.
fn r2(i: u32) -> (f32, f32) {
    const A1: f32 = 0.754_877_7;
    const A2: f32 = 0.569_840_3;
    (frac(0.5 + A1 * i as f32), frac(0.5 + A2 * i as f32))
}

fn hash2(i: u32) -> (f32, f32) {
    let h = i.wrapping_mul(0x9E37_79B9);
    let x = (h >> 8) as f32 / 16_777_216.0;
    let y = (h.wrapping_mul(0x85EB_CA6B) >> 8) as f32 / 16_777_216.0;
    (x, y)
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}

/// Average RGB of a square RGBA8 map by 2× (one mip). Uncovered (a=0) counts as black —
/// that is the fringe the dilation test exists to kill.
pub(crate) fn mip_box_rgba(px: &[u8], res: u32) -> (Vec<u8>, u32) {
    let res = res as usize;
    let lo = res / 2;
    let mut out = vec![0u8; lo * lo * 4];
    for y in 0..lo {
        for x in 0..lo {
            let mut acc = [0u32; 4];
            for sy in 0..2 {
                for sx in 0..2 {
                    let i = ((y * 2 + sy) * res + (x * 2 + sx)) * 4;
                    for c in 0..4 {
                        acc[c] += px[i + c] as u32;
                    }
                }
            }
            let o = (y * lo + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / 4) as u8;
            }
        }
    }
    (out, lo as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_map_bake::source::{PolyCurvSource, PolySampleSpace};
    use crate::mesh_map_bake::{maps, raster, BakeSettings, MeshMapJob, MeshMapKind};
    use core_math::mesh_curvature::RingMesh;
    use core_math::scene_graph::NodeId;
    use glam::Vec2;

    fn inset_quad() -> PolyCurvSource {
        PolyCurvSource {
            rings: RingMesh {
                positions: vec![
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(1.0, 1.0, 0.0),
                    Vec3::new(0.0, 1.0, 0.0),
                ],
                face_start: vec![0, 4],
                face_ring: vec![0, 1, 2, 3],
            },
            corner_uv: vec![
                Vec2::new(0.25, 0.25),
                Vec2::new(0.75, 0.25),
                Vec2::new(0.75, 0.75),
                Vec2::new(0.25, 0.75),
            ],
            space: PolySampleSpace::default(),
        }
    }

    fn coverage_rgba(buf: &UvGeoBuffer) -> Vec<u8> {
        let n = (buf.res * buf.res) as usize;
        let mut px = vec![0u8; n * 4];
        for i in 0..n {
            if buf.covered(i) {
                px[i * 4] = 255;
                px[i * 4 + 1] = 255;
                px[i * 4 + 2] = 255;
                px[i * 4 + 3] = 255;
            }
        }
        px
    }

    /// GATE (G6 seam) — 2-mip downsample of a padded island has no black fringe.
    #[test]
    fn dilate_kills_seam_fringe_after_two_mips() {
        let src = inset_quad();
        let mut geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        let raw = coverage_rgba(&geo);
        dilate_uv_geo(&mut geo, 4);
        let dil = coverage_rgba(&geo);
        let (raw1, r1) = mip_box_rgba(&raw, 32);
        let (raw2, _) = mip_box_rgba(&raw1, r1);
        let (dil1, d1) = mip_box_rgba(&dil, 32);
        let (dil2, lo) = mip_box_rgba(&dil1, d1);
        // Island is the centre of 0..1 → centre of the 8² mip.
        let c = (lo as usize / 2) * lo as usize + (lo as usize / 2);
        let raw_v = raw2[c * 4] as i32;
        let dil_v = dil2[c * 4] as i32;
        assert!(
            dil_v >= 200,
            "dilated 2-mip centre should stay white, got {dil_v}"
        );
        assert!(
            dil_v >= raw_v,
            "dilation must not darken the island (raw={raw_v} dil={dil_v})"
        );
        // A pixel that sat on the island *edge* (quarter way) is where fringe shows.
        // 2-mip texel just outside the 0.25 UV island (block y=4..7 on a 32²).
        let e = (lo as usize / 4).saturating_sub(1) * lo as usize + (lo as usize / 2);
        assert!(
            dil2[e * 4] as i32 > raw2[e * 4] as i32,
            "edge texel after 2 mips: raw={} dil={}",
            raw2[e * 4],
            dil2[e * 4]
        );
    }

    #[test]
    fn dilate_zero_passes_is_noop() {
        let src = inset_quad();
        let mut geo = raster::rasterize_uv_geo(&src, 16, 0.0);
        let before = geo.coverage.clone();
        dilate_uv_geo(&mut geo, 0);
        assert_eq!(geo.coverage, before);
    }

    #[test]
    fn effective_ss_caps_hi_res() {
        assert_eq!(effective_ss(1024, 4), 4);
        assert_eq!(effective_ss(4096, 4), 1);
        assert_eq!(effective_ss(2048, 4), 2);
        assert_eq!(effective_ss(64, 3), 1);
    }

    /// GATE (G6 SS) — job output is res_global², not (res×SS)².
    #[test]
    fn job_supersample_keeps_output_resolution() {
        let mut settings = BakeSettings::default();
        settings.res_global = 64;
        settings.supersample = 2;
        settings.enabled_maps = MeshMapKind::Position.bit();
        settings.padding_px = 0;
        let src = inset_quad();
        // Full-UV so raster covers something at both resolutions.
        let mut src = src;
        src.corner_uv = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let done = MeshMapJob {
            body: NodeId(1),
            src,
            face_groups: vec![],
            settings,
            high: None,
            cage: None,
        }
        .run();
        assert_eq!(done.pixels.len(), 1);
        assert_eq!(done.pixels[0].hi.len(), 64 * 64 * 4);
    }

    fn l_mesh() -> PolyCurvSource {
        // Floor XY + wall XZ sharing the X axis — some rays hit, some miss.
        PolyCurvSource {
            rings: RingMesh {
                positions: vec![
                    Vec3::new(0.0, 0.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(1.0, 1.0, 0.0),
                    Vec3::new(0.0, 1.0, 0.0),
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(0.0, 0.0, 1.0),
                ],
                face_start: vec![0, 4, 8],
                face_ring: vec![0, 1, 2, 3, 0, 1, 4, 5],
            },
            corner_uv: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(0.5, 0.5),
                Vec2::new(0.0, 0.5),
                Vec2::new(0.5, 0.5),
                Vec2::new(1.0, 0.5),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.5, 1.0),
            ],
            space: PolySampleSpace::default(),
        }
    }

    fn ao_neighbor_var(px: &[u8], res: u32) -> f32 {
        let res = res as usize;
        let mut acc = 0.0f32;
        let mut n = 0u32;
        for y in 0..res.saturating_sub(1) {
            for x in 0..res.saturating_sub(1) {
                let i = y * res + x;
                if px[i * 4 + 3] == 0 {
                    continue;
                }
                let a = px[i * 4] as f32;
                let r = i + 1;
                let d = i + res;
                if px[r * 4 + 3] > 0 {
                    let t = a - px[r * 4] as f32;
                    acc += t * t;
                    n += 1;
                }
                if px[d * 4 + 3] > 0 {
                    let t = a - px[d * 4] as f32;
                    acc += t * t;
                    n += 1;
                }
            }
        }
        if n == 0 {
            0.0
        } else {
            acc / n as f32
        }
    }

    /// GATE (G6 AO) — more rays reduce spatial variance on a self-shadowing L.
    #[test]
    fn ao_variance_drops_with_raycount() {
        let src = l_mesh();
        let mut geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        crate::mesh_map_bake::project::apply_projection(
            &mut geo,
            None,
            0.0,
            0.0,
            &mut crate::bake_diag::BakeNotes::default(),
            None,
        );
        let mut notes = crate::bake_diag::BakeNotes::default();
        let lo = maps::bake_occlusion_pixels(&src, &geo, false, 8, &mut notes, None).unwrap();
        let hi = maps::bake_occlusion_pixels(&src, &geo, false, 64, &mut notes, None).unwrap();
        let v8 = ao_neighbor_var(&lo, 32);
        let v64 = ao_neighbor_var(&hi, 32);
        assert!(
            v64 <= v8 * 1.05,
            "64 rays should be no noisier than 8 (v8={v8} v64={v64})"
        );
        // The L must actually occlude — otherwise this is a vacuous all-white map.
        let mean64 = hi
            .chunks(4)
            .filter(|c| c[3] == 255)
            .map(|c| c[0] as f32)
            .sum::<f32>()
            / hi.chunks(4).filter(|c| c[3] == 255).count().max(1) as f32;
        assert!(mean64 < 250.0, "L-mesh AO should not be fully open, mean={mean64}");
    }

    /// GATE (G7 GPU rays) — same L-mesh, GPU AO matches CPU within 8-bit noise.
    /// Ignored: first-device init can stall a headless adapter. Naga parse is the CI gate.
    /// `cargo test -p app gpu_ao_matches_cpu -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gpu_ao_matches_cpu_on_l_mesh() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::mesh_map_bake::gpu_rays::try_device().is_some());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(8)) {
            Ok(true) => {}
            _ => {
                eprintln!("skip gpu_ao_matches_cpu_on_l_mesh: no wgpu adapter (or timeout)");
                return;
            }
        }
        let src = l_mesh();
        let mut geo = raster::rasterize_uv_geo(&src, 16, 0.0);
        crate::mesh_map_bake::project::apply_projection(
            &mut geo,
            None,
            0.0,
            0.0,
            &mut crate::bake_diag::BakeNotes::default(),
            None,
        );
        let mut notes = crate::bake_diag::BakeNotes::default();
        let cpu = maps::bake_occlusion_pixels(&src, &geo, false, 16, &mut notes, None).unwrap();
        let gpu = crate::mesh_map_bake::gpu_rays::bake_occlusion(
            &src, &geo, false, 16, &mut notes, None,
        )
        .expect("GPU bake after try_device");
        assert_eq!(cpu.len(), gpu.len());
        let mut abs = 0.0f32;
        let mut n = 0u32;
        let mut mx = 0u32;
        for i in 0..cpu.len() / 4 {
            if cpu[i * 4 + 3] == 0 && gpu[i * 4 + 3] == 0 {
                continue;
            }
            let d = (cpu[i * 4] as i32 - gpu[i * 4] as i32).unsigned_abs();
            abs += d as f32;
            mx = mx.max(d);
            n += 1;
        }
        let mean = abs / n.max(1) as f32;
        assert!(
            mean < 4.0 && mx < 16,
            "GPU AO should match CPU (mean={mean} max={mx})"
        );
    }

    /// GATE (G7 bent) — open plane: bent ≈ geometric normal → TS (0,0,1) → Z byte high.
    #[test]
    fn bent_open_plane_is_flat_blue() {
        let mut src = inset_quad();
        src.corner_uv = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let mut geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        crate::mesh_map_bake::project::apply_projection(
            &mut geo,
            None,
            0.0,
            0.0,
            &mut crate::bake_diag::BakeNotes::default(),
            None,
        );
        let mut notes = crate::bake_diag::BakeNotes::default();
        let px = maps::bake_bent_normal_pixels(&src, &geo, 32, &mut notes, None).unwrap();
        let mut z_sum = 0u32;
        let mut n = 0u32;
        for i in 0..geo.position.len() {
            if px[i * 4 + 3] == 0 {
                continue;
            }
            z_sum += px[i * 4 + 2] as u32;
            n += 1;
        }
        assert!(n > 16);
        let z_mean = z_sum as f32 / n as f32;
        assert!(z_mean > 200.0, "open-plane bent Z should be ~255, got {z_mean}");
    }

    /// GATE (G7 bent) — L-mesh floor near the wall tilts +Y (G channel > 128).
    #[test]
    fn bent_l_mesh_tilts_away_from_wall() {
        let src = l_mesh();
        let mut geo = raster::rasterize_uv_geo(&src, 32, 0.0);
        crate::mesh_map_bake::project::apply_projection(
            &mut geo,
            None,
            0.0,
            0.0,
            &mut crate::bake_diag::BakeNotes::default(),
            None,
        );
        let mut notes = crate::bake_diag::BakeNotes::default();
        let px = maps::bake_bent_normal_pixels(&src, &geo, 48, &mut notes, None).unwrap();
        // Floor island is UV 0..0.5. Sample near the shared edge (low V).
        let res = geo.res as usize;
        let mut g_sum = 0i32;
        let mut n = 0i32;
        for y in 0..res / 4 {
            for x in 0..res / 2 {
                let i = y * res + x;
                if px[i * 4 + 3] == 0 {
                    continue;
                }
                g_sum += px[i * 4 + 1] as i32;
                n += 1;
            }
        }
        assert!(n > 4, "floor-near-wall must cover texels");
        let g_mean = g_sum as f32 / n as f32;
        assert!(
            g_mean > 132.0,
            "bent should tilt +Y away from the wall (G>{g_mean} expected >132)"
        );
    }

    #[test]
    fn hemisphere_samples_are_unit_disk() {
        for i in 0..32 {
            let (sin_t, phi) = hemisphere_sample(i, 32, 7);
            assert!((0.0..=1.0).contains(&sin_t), "sin_t={sin_t}");
            assert!(phi.is_finite());
        }
    }
}
