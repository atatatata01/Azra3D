use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};

/// Maximum number of Move-grab displacement INSTANCES the G-buffer vertex shader sums per frame
/// (`CameraUniforms.grab_anchors`/`grab_drags` length, and the `array<vec4,N>` size in `gbuffer.wgsl`).
///
/// One ACTIVE grab contributes up to 8 (its symmetry octants); the rest are persisted "surface mode"
/// (deferred-voxelize) moves accumulated since the last voxelize. 32 lets the user stack several
/// completed moves on top of a live one before a flush is forced (the accumulation cap → auto-voxelize).
/// MUST equal the WGSL `array<vec4<f32>, 32>` size in `gbuffer.wgsl::vs_main`.
pub const MAX_GRAB_INSTANCES: usize = 32;

/// Camera uniform buffer for the rasterization pipeline. 1280 bytes.
///
/// Built from the same orbit camera parameters used by the SDF compute pipeline,
/// ensuring both rendering paths see an identical camera.
///
/// The trailing `grab_*` + `wire_color` fields drive Move-tool vertex displacement and the
/// GPU DC-wire pass (see `gbuffer.wgsl`). They are APPENDED after the camera proper so
/// consumers that read only the camera prefix (picking / fog) stay valid — a uniform buffer
/// may be larger than a shader's declared struct.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniforms {
    /// Combined view-projection matrix (column-major)
    pub view_proj: [[f32; 4]; 4],
    /// View matrix only (column-major)
    pub view: [[f32; 4]; 4],
    /// Projection matrix only (column-major)
    pub proj: [[f32; 4]; 4],
    /// Camera world position (xyz), w = 0 for perspective, cam_dist for ortho
    pub cam_pos: [f32; 4],
    /// x = near plane, y = far plane
    pub near_far: [f32; 2],
    pub _pad: [f32; 2],
    /// Move-grab vertex displacement. z = active grab-INSTANCE COUNT (as f32, 0 = no grab → the G-buffer
    /// vertex shader skips displacement entirely); x/y/w unused (radii are now PER-INSTANCE, packed below).
    /// Falloff per instance = smoothstep(outer, inner, dist-to-anchor): 1 inside inner_r, 0 beyond outer_r.
    pub grab_radii: [f32; 4],
    /// Per-INSTANCE WORLD-space grab anchor: xyz = anchor, **w = this instance's inner_r** (world). Up to
    /// `MAX_GRAB_INSTANCES` instances = the active grab's symmetry octants (1/2/4/8) PLUS every accumulated
    /// deferred-voxelize ("surface mode") move. The vertex shader sums each instance's displacement.
    /// PER-INSTANCE radii (vs one global pair) let moves made at different brush sizes compose correctly.
    pub grab_anchors: [[f32; 4]; MAX_GRAB_INSTANCES],
    /// Per-INSTANCE WORLD-space drag: xyz = the displacement applied at that instance's core, **w = this
    /// instance's outer_r** (world). Falloff = smoothstep(outer_r=drags[i].w, inner_r=anchors[i].w, dist).
    pub grab_drags: [[f32; 4]; MAX_GRAB_INSTANCES],
    /// GPU DC-wire edge color (rgb) + opacity (a). Used by `fs_gbuffer_wire`.
    pub wire_color: [f32; 4],
    /// Per-pass mode flags. x = 1.0 → this raster pass is an auto reflection-probe
    /// capture frame (`probe_bake.rs`): `fs_gbuffer` then writes
    /// `PROBE_BACKFACE_MARK` into the body_index target for back-facing fragments so
    /// the capture can measure whether the probe eye sits inside solid geometry.
    /// y/z/w unused. Main-view passes leave it zero (body_index untouched).
    pub frame_flags: [f32; 4],
}

/// Near / far clip distances for the orbit camera, scaled with `distance` so zooming in
/// never puts the look-at target (or nearby geometry) behind the near plane.
///
/// Owner 2026-08-10: orthographic zoom uses `distance` as both camera offset AND ortho
/// half-height. A fixed `near = 0.1` clipped solid geometry as soon as `distance` dropped
/// toward that value — the classic "ortho clip when zoomed close" bug.
///
/// * **Ortho:** near stays a tiny fraction of distance (always ≪ focus depth ≈ distance);
///   far is a wide slab (cheap in ortho).
/// * **Perspective:** keep a usable fixed floor but still shrink near when the camera is
///   very close so macro inspection does not cut the surface.
#[inline]
pub fn orbit_clip_planes(distance: f32, ortho: bool) -> (f32, f32) {
    let d = distance.max(1e-4);
    if ortho {
        // Target sits at view-depth ≈ d. Keep near well below that, far far beyond.
        let near = (d * 0.001).clamp(1e-5, d * 0.05);
        let far = (d * 500.0).max(1_000.0);
        (near, far)
    } else {
        // Perspective: fixed 0.1 is fine at normal distances; when d < ~0.4, pull near in.
        let near = 0.1f32.min(d * 0.25).max(1e-3);
        let far = (d * 20.0).max(500.0);
        (near, far)
    }
}

impl CameraUniforms {
    /// Build camera matrices from orbit camera parameters.
    ///
    /// This matches the camera model in `RenderConfig::to_gpu_uniforms()`:
    /// - `orbit`: quaternion defining camera orientation
    /// - `dist`: distance from camera to target
    /// - `target`: world-space look-at point
    /// - `aspect`: viewport width / height
    /// - `fov_y_deg`: vertical field of view in degrees
    /// - `near` / `far`: clip planes
    /// - `ortho`: if true, use orthographic projection
    pub fn from_orbit(
        orbit: Quat,
        dist: f32,
        target: Vec3,
        aspect: f32,
        fov_y_deg: f32,
        near: f32,
        far: f32,
        ortho: bool,
    ) -> Self {
        // Camera position: orbit quaternion applied to (0, 0, dist) behind target
        let cam_pos = target + orbit * Vec3::new(0.0, 0.0, dist);
        let forward = (orbit * Vec3::NEG_Z).normalize();
        let up = (orbit * Vec3::Y).normalize();

        let view = Mat4::look_at_rh(cam_pos, cam_pos + forward, up);

        let proj = if ortho {
            // half_h = dist to match SDF compute shader + CPU world_to_screen
            // (both use cam_dist as the ortho half-height, not cam_dist*0.5)
            let half_h = dist;
            let half_w = half_h * aspect;
            Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, near, far)
        } else {
            Mat4::perspective_rh(fov_y_deg.to_radians(), aspect, near, far)
        };

        let view_proj = proj * view;

        Self {
            view_proj: view_proj.to_cols_array_2d(),
            view: view.to_cols_array_2d(),
            proj: proj.to_cols_array_2d(),
            cam_pos: [cam_pos.x, cam_pos.y, cam_pos.z, if ortho { dist } else { 0.0 }],
            near_far: [near, far],
            _pad: [0.0; 2],
            // No grab by default (count in grab_radii.z = 0); the raster pass fills these when a Move-grab is active.
            grab_radii: [0.0; 4],
            grab_anchors: [[0.0; 4]; MAX_GRAB_INSTANCES],
            grab_drags: [[0.0; 4]; MAX_GRAB_INSTANCES],
            // Default black wire at full opacity (matches wire_overlay defaults).
            wire_color: [0.0, 0.0, 0.0, 1.0],
            frame_flags: [0.0; 4],
        }
    }

    /// Returns the camera world position as Vec3.
    pub fn position(&self) -> Vec3 {
        Vec3::new(self.cam_pos[0], self.cam_pos[1], self.cam_pos[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_uniforms_size() {
        // 224 (camera) + 16 (grab_radii) + 32*16 (anchors) + 32*16 (drags) + 16 (wire_color)
        // + 16 (frame_flags) = 1296.
        assert_eq!(
            std::mem::size_of::<CameraUniforms>(),
            224 + 16 + MAX_GRAB_INSTANCES * 16 * 2 + 16 + 16
        );
        assert_eq!(std::mem::size_of::<CameraUniforms>(), 1296);
    }

    #[test]
    fn perspective_camera_basics() {
        let cam = CameraUniforms::from_orbit(
            Quat::IDENTITY,
            5.0,
            Vec3::ZERO,
            16.0 / 9.0,
            45.0,
            0.1,
            100.0,
            false,
        );
        // Camera should be at (0, 0, 5) looking at origin
        let pos = cam.position();
        assert!((pos.z - 5.0).abs() < 0.001);
        assert!(pos.x.abs() < 0.001);
        assert!(pos.y.abs() < 0.001);
    }

    /// Ortho zoom-close must keep the focus depth (`distance`) strictly inside [near, far].
    #[test]
    fn ortho_clip_planes_never_eat_the_focus_at_any_zoom() {
        for d in [0.001f32, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 50.0, 500.0] {
            let (near, far) = orbit_clip_planes(d, true);
            assert!(
                near > 0.0 && near < d,
                "ortho near must be in front of focus: d={d} near={near}"
            );
            assert!(
                far > d,
                "ortho far must be past focus: d={d} far={far}"
            );
            // Room for geometry that extends toward the camera past the focus point.
            assert!(
                near < d * 0.5,
                "ortho near too close to focus (will clip near-side faces): d={d} near={near}"
            );
        }
    }

    #[test]
    fn perspective_clip_planes_shrink_near_when_zoomed_in() {
        let (n_far, _) = orbit_clip_planes(10.0, false);
        let (n_close, _) = orbit_clip_planes(0.05, false);
        assert!((n_far - 0.1).abs() < 1e-5, "normal perspective near stays 0.1");
        assert!(n_close < 0.1, "close perspective near must drop below 0.1, got {n_close}");
        assert!(n_close < 0.05, "close near must stay in front of focus at d=0.05");
    }
}
