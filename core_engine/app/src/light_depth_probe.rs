// light_depth_probe.rs — depth-test the light gizmos against the raster depth buffer.
//
// VIEWPORT_PERF_2026-08-12 item A. `draw_light_gizmos` needs one bit per light: is the icon behind
// the surface the user can see? It used to answer that with `light_world_occluded` →
// `raycast_scene_surface` — a full-scene CPU raycast, per light, PER FRAME. Measured at **31 ms of
// a 33 ms frame** with `Lights: 1` on a 323k-tri body: the cost scaled with TRIANGLES, not lights,
// which is exactly why the HUD's light count read as innocent.
//
// The raster pass already computes this for every pixel. So sample it: one 1×1 depth copy per
// light, read back asynchronously, compared next frame. O(lights), independent of scene size.
//
// FRAME ORDERING (CLAUDE.md): every GPU resource mutation here — staging buffer creation, the
// copy encode, the submit — happens in `poll_background_tasks`, never in a panel draw or an input
// handler. `draw_light_gizmos` only READS `light_occluded`.
//
// FAIL-OPEN: a light with no verdict draws. A stray icon is recoverable; an invisible light is a
// tool the user cannot find. Every early-out below leaves the map untouched for that reason.

use glam::Vec3;
use render::depth_probe::ProbePoint;

use crate::AzraApp;

impl AzraApp {
    /// Convert a `Depth32Float` NDC depth (wgpu 0..1, `CompareFunction::Less`, cleared to 1.0)
    /// into linear view-space distance, so the comparison and its bias are in world units.
    ///
    /// Background (never written, depth 1.0) maps to `far`, which is behind everything — so an
    /// icon over empty space is never reported occluded, with no special case.
    pub(crate) fn ndc_depth_to_view(ndc_z: f32, near: f32, far: f32, ortho: bool) -> f32 {
        if ortho {
            near + ndc_z * (far - near)
        } else {
            let denom = far - ndc_z * (far - near);
            if denom.abs() < 1e-9 { far } else { (near * far) / denom }
        }
    }

    /// Poll phase: pair the in-flight readback with the lights it was issued for, then issue the
    /// next one. Single-flight — the previous result is consumed (and the staging buffer unmapped)
    /// before anything is submitted, which is what keeps this off the picking.rs "buffer is still
    /// mapped" crash path.
    pub(crate) fn poll_light_depth_probe(&mut self) {
        // Nothing to test: drop stale verdicts so a deleted light cannot leave a ghost bit behind.
        if self.scene.scene_graph.lights().next().is_none() {
            if !self.light_occluded.is_empty() {
                self.light_occluded.clear();
            }
            return;
        }

        // Camera must match the one the raster pass rendered the depth buffer with: same clip
        // planes, same aspect (taken from the DEPTH texture, which `ensure_depth` sizes to the
        // G-buffer, not to the egui rect). TAA jitter is sub-pixel and irrelevant to a 1-texel
        // probe of an icon-sized marker.
        let Some(gpu) = self.gpu.as_ref() else { return };
        let (dw, dh) = gpu.raster.depth_size;
        if dw == 0 || dh == 0 || gpu.raster.depth_texture.is_none() {
            return;
        }
        let ortho = self.render.ortho;
        let (near, far) = render::camera::orbit_clip_planes(self.viewport.camera.distance, ortho);
        let camera = render::camera::CameraUniforms::from_orbit(
            self.viewport.camera.orbit,
            self.viewport.camera.distance,
            self.viewport.camera.target,
            dw as f32 / dh.max(1) as f32,
            self.render.fov_deg,
            near,
            far,
            ortho,
        );
        let vp = glam::Mat4::from_cols_array_2d(&camera.view_proj);

        // Project every visible light; keep only those with a texel to sample.
        let mut points: Vec<ProbePoint> = Vec::new();
        let mut issued: Vec<(u32, f32)> = Vec::new();
        for (_nid, light) in self.scene.scene_graph.lights() {
            if !light.visible {
                continue;
            }
            let p: Vec3 = light.gizmo_position();
            let clip = vp * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            if clip.w <= 0.0 {
                continue; // behind the eye
            }
            let ndc = clip.truncate() / clip.w;
            if !(-1.0..=1.0).contains(&ndc.x)
                || !(-1.0..=1.0).contains(&ndc.y)
                || !(0.0..=1.0).contains(&ndc.z)
            {
                continue; // off-screen or outside the clip range — nothing to compare against
            }
            // NDC y is up, texture y is down.
            let tx = ((ndc.x * 0.5 + 0.5) * dw as f32) as u32;
            let ty = ((0.5 - ndc.y * 0.5) * dh as f32) as u32;
            if tx >= dw || ty >= dh {
                continue;
            }
            points.push(ProbePoint { x: tx, y: ty });
            issued.push((light.id, Self::ndc_depth_to_view(ndc.z, near, far, ortho)));
        }

        let device = gpu.device.clone();
        let queue = gpu.queue.clone();

        // 1. Consume the readback issued LAST poll. Its ids and their own depths were stored then;
        //    pairing is by index, which is why `encode` must arm every point it is handed.
        let prev = std::mem::take(&mut self.light_probe_inflight);
        let taken = {
            let Some(gpu) = self.gpu.as_mut() else { return };
            gpu.light_depth_probe.try_take(&device)
        };
        let Some(depths) = taken else {
            // Still in flight — put the ids back so the result can still be paired next poll.
            self.light_probe_inflight = prev;
            return;
        };
        for (i, (id, light_z)) in prev.iter().enumerate() {
            let Some(&scene_ndc) = depths.get(i) else { break };
            let scene_z = Self::ndc_depth_to_view(scene_ndc, near, far, ortho);
            // Same tolerance the CPU ray used: an icon sitting just in front of a surface still
            // draws; only a surface CLEARLY nearer than the icon hides it.
            let bias = (light_z * 0.002).max(0.04);
            self.light_occluded.insert(*id, scene_z + bias < *light_z);
        }
        // A light that left the frustum keeps no verdict: it is not on screen, and a stale bit
        // would decide its visibility the instant it comes back.
        self.light_occluded.retain(|id, _| issued.iter().any(|(i, _)| i == id));

        // 2. Issue the next probe.
        if points.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.as_mut() else { return };
        // Disjoint field borrows: the probe needs `&mut`, the depth texture only `&`.
        let probe = &mut gpu.light_depth_probe;
        let Some(depth_tex) = gpu.raster.depth_texture.as_ref() else { return };
        let mut enc = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
            label: Some("light_depth_probe"),
        });
        let armed = probe.encode(&device, &mut enc, depth_tex, dw, dh, &points);
        if armed == 0 {
            return;
        }
        queue.submit(Some(enc.finish()));
        probe.start_readback();
        issued.truncate(armed);
        self.light_probe_inflight = issued;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The depth convention is the whole fix: get it inverted (reverse-Z, or near/far swapped) and
    /// every light hides exactly when it should show. Round-trip a real point through the SAME
    /// camera the raster pass builds, rather than asserting against a hand-copied formula.
    fn roundtrip(ortho: bool) {
        let dist = 7.5f32;
        let (near, far) = render::camera::orbit_clip_planes(dist, ortho);
        let cam = render::camera::CameraUniforms::from_orbit(
            glam::Quat::IDENTITY, dist, Vec3::ZERO, 16.0 / 9.0, 60.0, near, far, ortho,
        );
        let vp = glam::Mat4::from_cols_array_2d(&cam.view_proj);
        // Points straight ahead of the camera at known view depths.
        for &z in &[near * 2.0, dist * 0.5, dist, dist * 1.5] {
            let world = Vec3::new(0.0, 0.0, dist - z); // camera sits at +Z looking down -Z
            let clip = vp * glam::Vec4::new(world.x, world.y, world.z, 1.0);
            assert!(clip.w > 0.0, "test point behind the eye (ortho={ortho}, z={z})");
            let ndc_z = clip.z / clip.w;
            let back = AzraApp::ndc_depth_to_view(ndc_z, near, far, ortho);
            assert!(
                (back - z).abs() <= z * 1e-3 + 1e-4,
                "ortho={ortho}: view depth {z} → ndc {ndc_z} → {back}"
            );
        }
    }

    #[test]
    fn ndc_depth_round_trips_perspective() {
        roundtrip(false);
    }

    #[test]
    fn ndc_depth_round_trips_ortho() {
        roundtrip(true);
    }

    /// Cleared depth (nothing drawn) must read as the far plane, i.e. behind every marker —
    /// that is what makes "icon over empty space" need no special case.
    #[test]
    fn cleared_depth_is_the_far_plane() {
        for ortho in [false, true] {
            let (near, far) = render::camera::orbit_clip_planes(10.0, ortho);
            let d = AzraApp::ndc_depth_to_view(1.0, near, far, ortho);
            assert!((d - far).abs() <= far * 1e-3, "ortho={ortho}: {d} vs far {far}");
        }
    }
}
