//! Per-map pixel writers. All read the shared [`UvGeoBuffer`].

use super::raster::UvGeoBuffer;
use super::source::PolyCurvSource;
use super::MeshMapKind;
use crate::bake_diag::BakeNotes;
use glam::Vec3;

/// Curvature → RGBA8, grey-centred: 0.5 flat, >0.5 convex, <0.5 concave.
///
/// G0: encodes `UvGeoBuffer.kappa` (the shared raster). Peak-normalised so a gentle body and a
/// hard-surface one both come out readable.
pub(crate) fn bake_curvature_pixels(buf: &UvGeoBuffer, notes: &mut BakeNotes) -> Option<Vec<u8>> {
    let n = (buf.res * buf.res) as usize;
    let mut peak = 0.0f32;
    for i in 0..n {
        if buf.covered(i) {
            peak = peak.max(buf.kappa[i].abs());
        }
    }
    const FLAT_EPS: f32 = 1.0e-3;
    let gain = if peak > FLAT_EPS { 0.5 / peak } else { 0.0 };
    if gain == 0.0 {
        notes.push("Bake Mesh Maps: this mesh is flat — curvature map is uniform grey");
    }
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let signed = (buf.kappa[i] * gain).clamp(-0.5, 0.5);
        let v = ((signed + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
        px[i * 4] = v;
        px[i * 4 + 1] = v;
        px[i * 4 + 2] = v;
        px[i * 4 + 3] = 255;
    }
    Some(px)
}

pub(crate) fn occlusion_scene(
    src: &PolyCurvSource,
    high: Option<&super::project::HighTarget>,
    notes: &mut BakeNotes,
) -> Option<(Vec<[f32; 3]>, Vec<u32>, core_math::bvh::TriBlas)> {
    use core_math::bvh::build_tri_blas;
    if let Some(h) = high {
        return Some((h.positions.clone(), h.indices.clone(), h.blas.clone()));
    }
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(src.rings.positions.len());
    for p in &src.rings.positions {
        positions.push([p.x, p.y, p.z]);
    }
    let mut indices: Vec<u32> = Vec::new();
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..src.rings.face_count() {
        let lo = src.rings.face_start[fi] as usize;
        let hi = src.rings.face_start[fi + 1] as usize;
        let ring = &src.rings.face_ring[lo..hi];
        if ring.len() < 3 {
            continue;
        }
        pts.clear();
        pts.extend(ring.iter().map(|&v| src.rings.positions[v as usize]));
        let fnorm = core_math::half_edge::newell_normal(&pts).normalize_or_zero();
        for t in core_math::poly_mesh::earclip_polygon(&pts, fnorm) {
            indices.push(ring[t[0] as usize]);
            indices.push(ring[t[1] as usize]);
            indices.push(ring[t[2] as usize]);
        }
    }
    if indices.is_empty() {
        notes.push("Bake Mesh Maps: no triangles to trace against");
        return None;
    }
    let blas = build_tri_blas(&positions, &indices);
    if blas.is_empty() {
        notes.push("Bake Mesh Maps: could not build a ray-tracing structure");
        return None;
    }
    Some((positions, indices, blas))
}

/// AO (`inward = false`) / Thickness (`inward = true`) by ray casting the mesh against itself.
pub(crate) fn bake_occlusion_pixels(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    inward: bool,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<Vec<u8>> {
    use core_math::bvh::blas_any_hit;

    let (positions, indices, blas) = occlusion_scene(src, high, notes)?;

    let (lo, hi) = buf.bounds;
    let diag = (hi - lo).length().max(1.0e-4);
    let bias = diag * 1.0e-3;
    let ray_len = if inward { diag * 0.5 } else { diag * 0.75 };
    let rays = rays.clamp(8, 512);

    use rayon::prelude::*;
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    px.par_chunks_mut(4).enumerate().for_each(|(i, out)| {
        if !buf.covered(i) {
            return;
        }
        let nrm = if inward { -buf.high_normal[i] } else { buf.high_normal[i] };
        if nrm == Vec3::ZERO {
            return;
        }
        let origin = buf.high_position[i] + nrm * bias;
        let up = if nrm.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
        let t1 = nrm.cross(up).normalize_or_zero();
        let t2 = nrm.cross(t1);
        let mut open = 0u32;
        for r in 0..rays {
            let (sin_t, phi) = super::quality::hemisphere_sample(r, rays, i);
            let cos_t = (1.0 - sin_t * sin_t).max(0.0).sqrt();
            let d = t1 * (sin_t * phi.cos()) + t2 * (sin_t * phi.sin()) + nrm * cos_t;
            if !blas_any_hit(
                &blas,
                &positions,
                &indices,
                [origin.x, origin.y, origin.z],
                [d.x, d.y, d.z],
                ray_len,
            ) {
                open += 1;
            }
        }
        let f = open as f32 / rays as f32;
        let v = if inward { 1.0 - f } else { f };
        let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        out[0] = b;
        out[1] = b;
        out[2] = b;
        out[3] = 255;
    });
    Some(px)
}

/// Bent normal: average unoccluded hemisphere direction, encoded in the **low** MikkT frame.
pub(crate) fn bake_bent_normal_pixels(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<Vec<u8>> {
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    bake_bent_into(src, buf, rays, notes, high, |i, ts| {
        let rgb = core_math::mikkt::encode_tangent_normal(ts);
        px[i * 4] = rgb[0];
        px[i * 4 + 1] = rgb[1];
        px[i * 4 + 2] = rgb[2];
        px[i * 4 + 3] = 255;
    })?;
    Some(px)
}

/// 16-bit dual-plane bent normal (G7).
pub(crate) fn bake_bent_normal_pixels_16(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
) -> Option<(Vec<u8>, Vec<u8>)> {
    use crate::material_textures::encode_normal_rgba16_planes;
    let n = (buf.res * buf.res) as usize;
    let mut hi_px = vec![0u8; n * 4];
    let mut lo_px = vec![0u8; n * 4];
    bake_bent_into(src, buf, rays, notes, high, |i, ts| {
        let (h, l) = encode_normal_rgba16_planes([ts.x, ts.y, ts.z]);
        hi_px[i * 4..i * 4 + 4].copy_from_slice(&h);
        lo_px[i * 4..i * 4 + 4].copy_from_slice(&l);
    })?;
    Some((hi_px, lo_px))
}

fn bake_bent_into(
    src: &PolyCurvSource,
    buf: &UvGeoBuffer,
    rays: usize,
    notes: &mut BakeNotes,
    high: Option<&super::project::HighTarget>,
    mut write: impl FnMut(usize, Vec3),
) -> Option<()> {
    use core_math::bvh::blas_any_hit;
    use core_math::mikkt::world_to_tangent;
    let (positions, indices, blas) = occlusion_scene(src, high, notes)?;
    let (lo, hi) = buf.bounds;
    let diag = (hi - lo).length().max(1.0e-4);
    let bias = diag * 1.0e-3;
    let ray_len = diag * 0.75;
    let rays = rays.clamp(8, 512);
    let n = (buf.res * buf.res) as usize;
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let nrm = buf.high_normal[i];
        let t = buf.low_tangent[i];
        let ln = buf.normal[i];
        if nrm == Vec3::ZERO || t == Vec3::ZERO || ln == Vec3::ZERO {
            continue;
        }
        let origin = buf.high_position[i] + nrm * bias;
        let up = if nrm.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
        let t1 = nrm.cross(up).normalize_or_zero();
        let t2 = nrm.cross(t1);
        let mut acc = Vec3::ZERO;
        let mut open = 0u32;
        for r in 0..rays {
            let (sin_t, phi) = super::quality::hemisphere_sample(r, rays, i);
            let cos_t = (1.0 - sin_t * sin_t).max(0.0).sqrt();
            let d = t1 * (sin_t * phi.cos()) + t2 * (sin_t * phi.sin()) + nrm * cos_t;
            if !blas_any_hit(
                &blas,
                &positions,
                &indices,
                [origin.x, origin.y, origin.z],
                [d.x, d.y, d.z],
                ray_len,
            ) {
                acc += d;
                open += 1;
            }
        }
        let world = if open == 0 {
            nrm
        } else {
            acc.normalize_or_zero()
        };
        let ts = world_to_tangent(world, t, buf.low_bitangent[i], ln);
        write(i, ts);
    }
    Some(())
}

pub(crate) fn bake_vector_pixels(buf: &UvGeoBuffer, kind: MeshMapKind) -> Option<Vec<u8>> {
    let (lo, hi) = buf.bounds;
    let ext = (hi - lo).max(Vec3::splat(1.0e-6));
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let v = match kind {
            MeshMapKind::Position => (buf.high_position[i] - lo) / ext,
            MeshMapKind::ObjectNormal => buf.high_normal[i] * 0.5 + Vec3::splat(0.5),
            _ => continue,
        };
        for c in 0..3 {
            px[i * 4 + c] = (v[c].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        px[i * 4 + 3] = 255;
    }
    Some(px)
}

/// 16-bit dual-plane Position / ObjectNormal (G7). Linear 0..1 per channel.
pub(crate) fn bake_vector_pixels_16(
    buf: &UvGeoBuffer,
    kind: MeshMapKind,
) -> Option<(Vec<u8>, Vec<u8>)> {
    use crate::material_textures::encode_data_rgb_rgba16_planes;
    let (lo, hi) = buf.bounds;
    let ext = (hi - lo).max(Vec3::splat(1.0e-6));
    let n = (buf.res * buf.res) as usize;
    let mut hi_px = vec![0u8; n * 4];
    let mut lo_px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let v = match kind {
            MeshMapKind::Position => (buf.high_position[i] - lo) / ext,
            MeshMapKind::ObjectNormal => buf.high_normal[i] * 0.5 + Vec3::splat(0.5),
            _ => continue,
        };
        let (h, l) = encode_data_rgb_rgba16_planes([v.x, v.y, v.z]);
        hi_px[i * 4..i * 4 + 4].copy_from_slice(&h);
        lo_px[i * 4..i * 4 + 4].copy_from_slice(&l);
    }
    Some((hi_px, lo_px))
}

/// High-poly normal in the **low** MikkT frame. OpenGL +Y, `n*0.5+0.5`.
pub(crate) fn bake_tangent_normal_pixels(buf: &UvGeoBuffer) -> Option<Vec<u8>> {
    use core_math::mikkt::{encode_tangent_normal, world_to_tangent};
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let t = buf.low_tangent[i];
        let b = buf.low_bitangent[i];
        let ln = buf.normal[i];
        if t == Vec3::ZERO || ln == Vec3::ZERO {
            continue;
        }
        let ts = world_to_tangent(buf.high_normal[i], t, b, ln);
        let rgb = encode_tangent_normal(ts);
        px[i * 4] = rgb[0];
        px[i * 4 + 1] = rgb[1];
        px[i * 4 + 2] = rgb[2];
        px[i * 4 + 3] = 255;
    }
    Some(px)
}

/// 16-bit dual-plane tangent normal (G7).
pub(crate) fn bake_tangent_normal_pixels_16(buf: &UvGeoBuffer) -> Option<(Vec<u8>, Vec<u8>)> {
    use crate::material_textures::encode_normal_rgba16_planes;
    use core_math::mikkt::world_to_tangent;
    let n = (buf.res * buf.res) as usize;
    let mut hi_px = vec![0u8; n * 4];
    let mut lo_px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) {
            continue;
        }
        let t = buf.low_tangent[i];
        let b = buf.low_bitangent[i];
        let ln = buf.normal[i];
        if t == Vec3::ZERO || ln == Vec3::ZERO {
            continue;
        }
        let ts = world_to_tangent(buf.high_normal[i], t, b, ln);
        let (h, l) = encode_normal_rgba16_planes([ts.x, ts.y, ts.z]);
        hi_px[i * 4..i * 4 + 4].copy_from_slice(&h);
        lo_px[i * 4..i * 4 + 4].copy_from_slice(&l);
    }
    Some((hi_px, lo_px))
}

pub(crate) fn bake_id_pixels(
    face_groups: &[u16],
    buf: &UvGeoBuffer,
    notes: &mut BakeNotes,
) -> Option<Vec<u8>> {
    use core_math::poly_mesh::PolyGroupMeta;
    let has_high = buf.high_group.iter().any(|g| *g != 0);
    if face_groups.is_empty() && !has_high {
        notes.push("Bake Mesh Maps: ID skipped — no polygroups on this body");
        return None;
    }
    let n = (buf.res * buf.res) as usize;
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        if !buf.covered(i) || buf.face[i] == u32::MAX {
            continue;
        }
        let gid = if !buf.high_group.is_empty() {
            buf.high_group[i]
        } else {
            face_groups.get(buf.face[i] as usize).copied().unwrap_or(0)
        };
        let c = PolyGroupMeta::color_for_id(gid);
        px[i * 4] = c[0];
        px[i * 4 + 1] = c[1];
        px[i * 4 + 2] = c[2];
        px[i * 4 + 3] = 255;
    }
    Some(px)
}
