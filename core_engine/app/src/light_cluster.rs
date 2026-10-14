//! P3.1 Phase D — CPU clustered light lists for deferred shading.
//!
//! Shade indices (0..shade_count-1) are assigned to screen-space tiles × log-depth
//! slices. Deferred samples only the lights in the pixel's cluster.
//!
//! Layout of `cluster_data` (u32 array):
//! ```text
//! [0] tiles_x  [1] tiles_y  [2] z_slices  [3] tile_px
//! [4] max_per  [5] enabled (1/0)  [6] screen_w  [7] screen_h
//! [8] near_bits  [9] far_bits   (f32 bitcast — must match GPU z_slice)
//! then for each cluster (ty * tiles_x * z + tx * z + tz):
//!   [count, li0, li1, …]  length = 1 + max_per
//! ```
//! Directional lights are inserted into every cluster (cheap, usually 1).
//! RT still uses shade indices 0..7 only (unchanged dual packs).

use glam::Vec3;

/// Screen tile size in pixels.
pub(crate) const CLUSTER_TILE_PX: u32 = 32;
/// Log-depth slices.
pub(crate) const CLUSTER_Z_SLICES: u32 = 8;
/// Max shade lights stored per cluster (budget for local lights + sun).
pub(crate) const MAX_LIGHTS_PER_CLUSTER: u32 = 12;
/// Header words before cluster bodies.
pub(crate) const CLUSTER_HEADER_U32: usize = 10;

/// Camera + resolution inputs for cluster assignment.
#[derive(Clone, Debug)]
pub(crate) struct ClusterParams {
    pub screen_w: u32,
    pub screen_h: u32,
    pub cam_pos: Vec3,
    pub cam_forward: Vec3,
    pub cam_right: Vec3,
    pub cam_up: Vec3,
    pub tan_half_fov: f32,
    pub near: f32,
    pub far: f32,
    /// When false, emit disabled header only (shader falls back to global loop).
    pub enabled: bool,
}

/// One packed light used for assignment (shade-order index + influence).
#[derive(Clone, Debug)]
pub(crate) struct ClusterLight {
    pub shade_li: u32,
    pub is_directional: bool,
    pub position: Vec3,
    pub range: f32,
}

/// Disabled / empty cluster buffer (header only, enabled=0).
pub(crate) fn empty_cluster_data() -> Vec<u32> {
    vec![
        1, 1, 1, CLUSTER_TILE_PX, // tiles_x/y/z, tile
        MAX_LIGHTS_PER_CLUSTER, 0, // max_per, enabled=0
        1, 1,                     // screen
        f32::to_bits(0.1), f32::to_bits(200.0), // near/far
    ]
}

fn z_slice(view_z: f32, near: f32, far: f32, slices: u32) -> u32 {
    let z = view_z.clamp(near, far);
    let denom = (far / near).ln().max(1e-6);
    let t = ((z / near).ln() / denom).clamp(0.0, 0.9999);
    (t * slices as f32) as u32
}

/// Build the GPU cluster list buffer for the current shade lights.
pub(crate) fn build_cluster_lists(params: &ClusterParams, lights: &[ClusterLight]) -> Vec<u32> {
    if !params.enabled || params.screen_w < 1 || params.screen_h < 1 {
        return empty_cluster_data();
    }

    let tile = CLUSTER_TILE_PX.max(8);
    let tiles_x = (params.screen_w + tile - 1) / tile;
    let tiles_y = (params.screen_h + tile - 1) / tile;
    let z_slices = CLUSTER_Z_SLICES.max(1);
    let max_per = MAX_LIGHTS_PER_CLUSTER as usize;
    let n_clusters = (tiles_x * tiles_y * z_slices) as usize;
    let stride = 1 + max_per;

    let aspect = params.screen_w as f32 / params.screen_h.max(1) as f32;
    let tan_h = params.tan_half_fov.max(0.01);
    let near = params.near.max(0.01);
    let far = params.far.max(near + 1.0);

    let mut data = vec![0u32; CLUSTER_HEADER_U32 + n_clusters * stride];
    data[0] = tiles_x;
    data[1] = tiles_y;
    data[2] = z_slices;
    data[3] = tile;
    data[4] = max_per as u32;
    data[5] = 1; // enabled
    data[6] = params.screen_w;
    data[7] = params.screen_h;
    data[8] = f32::to_bits(near);
    data[9] = f32::to_bits(far);
    let fwd = params.cam_forward.normalize_or_zero();
    let right = params.cam_right.normalize_or_zero();
    let up = params.cam_up.normalize_or_zero();
    if fwd.length_squared() < 1e-8 {
        return empty_cluster_data();
    }

    let push = |data: &mut [u32], ci: usize, li: u32| {
        let base = CLUSTER_HEADER_U32 + ci * stride;
        let count = data[base] as usize;
        if count >= max_per {
            return;
        }
        // Avoid duplicates
        for k in 0..count {
            if data[base + 1 + k] == li {
                return;
            }
        }
        data[base + 1 + count] = li;
        data[base] = (count + 1) as u32;
    };

    for light in lights {
        // range <= 0 = "infinite" to the shader (unbounded inverse-square,
        // sdf_lighting point_atten) — clamping it to 5cm here culled the light to
        // ~one tile so it vanished everywhere else while top-K scoring kept it.
        // Infinite lights go everywhere, exactly like directional.
        if light.is_directional || light.range <= 0.01 {
            for ci in 0..n_clusters {
                push(&mut data, ci, light.shade_li);
            }
            continue;
        }

        let rel = light.position - params.cam_pos;
        let view_z = rel.dot(fwd);
        if view_z < -light.range {
            continue; // fully behind camera
        }
        let z_center = view_z.max(near * 0.5);
        let range = light.range.max(0.05);

        // Screen footprint (conservative). The radius MUST be projected at the
        // sphere's NEAREST depth, not its center depth (owner "weird rectangular
        // clipping" 2026-08-07): dividing by the center z underestimates the
        // footprint for any light near the camera or near the geometry it lights,
        // so edge tiles never evaluated the light and the lit pool was clipped
        // along tile boundaries — invisible under the old dim artistic falloff,
        // obvious under physical inverse-square. A camera inside the light sphere
        // has an unbounded footprint → all tiles for its z band.
        let x = rel.dot(right);
        let y = rel.dot(up);
        let z_proj = z_center.max(near);
        let ndc_x = x / (z_proj * aspect * tan_h);
        let ndc_y = y / (z_proj * tan_h);
        let z_nearest = z_proj - range;
        if z_nearest <= near {
            // Camera inside (or nearly inside) the light sphere: full-screen reach.
            let z0 = z_slice((z_center - range).max(near), near, far, z_slices);
            let z1 = z_slice(z_center + range, near, far, z_slices);
            for ty in 0..tiles_y {
                for tx in 0..tiles_x {
                    for tz in z0..=z1.min(z_slices - 1) {
                        let ci = (ty * tiles_x * z_slices + tx * z_slices + tz) as usize;
                        push(&mut data, ci, light.shade_li);
                    }
                }
            }
            continue;
        }
        let px_r = (range / z_nearest) * (0.5 * params.screen_h as f32 / tan_h);
        let cx = (ndc_x * 0.5 + 0.5) * params.screen_w as f32;
        let cy = (1.0 - (ndc_y * 0.5 + 0.5)) * params.screen_h as f32; // y-down screen

        let min_sx = (cx - px_r).floor().max(0.0) as i32;
        let max_sx = (cx + px_r).ceil().min(params.screen_w as f32 - 1.0) as i32;
        let min_sy = (cy - px_r).floor().max(0.0) as i32;
        let max_sy = (cy + px_r).ceil().min(params.screen_h as f32 - 1.0) as i32;
        if min_sx > max_sx || min_sy > max_sy {
            // Off-screen but may still affect edges — skip
            continue;
        }

        let tx0 = (min_sx as u32) / tile;
        let tx1 = (max_sx as u32) / tile;
        let ty0 = (min_sy as u32) / tile;
        let ty1 = (max_sy as u32) / tile;

        let z0 = z_slice((z_center - range).max(near), near, far, z_slices);
        let z1 = z_slice(z_center + range, near, far, z_slices);

        for ty in ty0..=ty1.min(tiles_y - 1) {
            for tx in tx0..=tx1.min(tiles_x - 1) {
                for tz in z0..=z1.min(z_slices - 1) {
                    let ci = (ty * tiles_x * z_slices + tx * z_slices + tz) as usize;
                    push(&mut data, ci, light.shade_li);
                }
            }
        }
    }

    data
}

/// Cluster index for a pixel + view depth (mirrors GPU shader).
pub(crate) fn cluster_index_for_pixel(
    data: &[u32],
    px: u32,
    py: u32,
    view_z: f32,
    near: f32,
    far: f32,
) -> Option<usize> {
    if data.len() < CLUSTER_HEADER_U32 || data[5] == 0 {
        return None;
    }
    let tiles_x = data[0];
    let tiles_y = data[1];
    let z_slices = data[2];
    let tile = data[3].max(1);
    let max_per = data[4] as usize;
    let stride = 1 + max_per;
    let tx = (px / tile).min(tiles_x.saturating_sub(1));
    let ty = (py / tile).min(tiles_y.saturating_sub(1));
    let tz = z_slice(view_z, near, far, z_slices).min(z_slices.saturating_sub(1));
    let ci = (ty * tiles_x * z_slices + tx * z_slices + tz) as usize;
    let base = CLUSTER_HEADER_U32 + ci * stride;
    if base + stride > data.len() {
        return None;
    }
    Some(ci)
}

pub(crate) fn cluster_lights_at(data: &[u32], ci: usize) -> &[u32] {
    if data.len() < CLUSTER_HEADER_U32 || data[5] == 0 {
        return &[];
    }
    let max_per = data[4] as usize;
    let stride = 1 + max_per;
    let base = CLUSTER_HEADER_U32 + ci * stride;
    if base >= data.len() {
        return &[];
    }
    let count = (data[base] as usize).min(max_per);
    let end = (base + 1 + count).min(data.len());
    &data[base + 1..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(w: u32, h: u32) -> ClusterParams {
        ClusterParams {
            screen_w: w,
            screen_h: h,
            cam_pos: Vec3::new(0.0, 0.0, 5.0),
            cam_forward: Vec3::new(0.0, 0.0, -1.0),
            cam_right: Vec3::new(1.0, 0.0, 0.0),
            cam_up: Vec3::new(0.0, 1.0, 0.0),
            tan_half_fov: (60.0f32.to_radians() * 0.5).tan(),
            near: 0.1,
            far: 200.0,
            enabled: true,
        }
    }

    #[test]
    fn disabled_header_only() {
        let mut p = params(128, 128);
        p.enabled = false;
        let d = build_cluster_lists(&p, &[]);
        assert_eq!(d[5], 0);
    }

    #[test]
    fn directional_in_every_cluster() {
        let p = params(64, 64);
        let lights = vec![ClusterLight {
            shade_li: 0,
            is_directional: true,
            position: Vec3::ZERO,
            range: 0.0,
        }];
        let d = build_cluster_lists(&p, &lights);
        assert_eq!(d[5], 1);
        let tiles_x = d[0];
        let tiles_y = d[1];
        let z = d[2];
        let n = (tiles_x * tiles_y * z) as usize;
        for ci in 0..n {
            let ls = cluster_lights_at(&d, ci);
            assert!(ls.contains(&0), "cluster {ci} missing sun");
        }
    }

    #[test]
    fn local_light_only_near_its_tile() {
        let p = params(256, 256);
        // Light at camera-forward center, close
        let lights = vec![
            ClusterLight {
                shade_li: 0,
                is_directional: true,
                position: Vec3::ZERO,
                range: 0.0,
            },
            ClusterLight {
                shade_li: 1,
                is_directional: false,
                position: Vec3::new(0.0, 0.0, 0.0), // world origin; cam at z=5 looking -Z
                range: 2.0,
            },
        ];
        let d = build_cluster_lists(&p, &lights);
        // Center pixel should include light 1
        let ci = cluster_index_for_pixel(&d, 128, 128, 5.0, 0.1, 200.0).unwrap();
        let center = cluster_lights_at(&d, ci);
        assert!(center.contains(&1), "center cluster should hold local light: {:?}", center);

        // Far corner tile may or may not — with small range often not
        let ci_corner = cluster_index_for_pixel(&d, 0, 0, 5.0, 0.1, 200.0).unwrap();
        let corner = cluster_lights_at(&d, ci_corner);
        // Sun always; local may be absent if footprint small
        assert!(corner.contains(&0));
    }

    #[test]
    fn respects_max_per_cluster() {
        let p = params(64, 64);
        let mut lights = vec![ClusterLight {
            shade_li: 0,
            is_directional: true,
            position: Vec3::ZERO,
            range: 0.0,
        }];
        // Many overlapping locals at same spot
        for i in 1..20 {
            lights.push(ClusterLight {
                shade_li: i,
                is_directional: false,
                position: Vec3::new(0.0, 0.0, 0.0),
                range: 50.0,
            });
        }
        let d = build_cluster_lists(&p, &lights);
        let ci = cluster_index_for_pixel(&d, 32, 32, 5.0, 0.1, 200.0).unwrap();
        let ls = cluster_lights_at(&d, ci);
        assert!(ls.len() <= MAX_LIGHTS_PER_CLUSTER as usize);
        assert!(ls.contains(&0));
    }
}
