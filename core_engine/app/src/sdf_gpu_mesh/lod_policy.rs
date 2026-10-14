//! O2b — the automatic per-body octree-LOD level policy (PURE logic; the poll-phase wiring lives
//! in `app_poll_gpu.rs::poll_sdf_lod_policy`).
//!
//! Signal: the projected size of ONE voxel on screen, in pixels. Perspective approximation
//! (matches `Mat4::perspective_rh` / `cam_ray`'s tan(fov/2) convention):
//!
//! `projected_px ≈ voxel_world * (viewport_h_px / 2) / (distance * tan(fov/2))`
//!
//! where `voxel_world = (gmax−gmin).min_element() / (res−1)` scaled by the body's max world scale
//! component, and `distance` = camera → body world center. Ortho uses the same formula with
//! `distance * tan_half` replaced by the ortho half-height (`camera.distance`). An approximation
//! is fine: the policy only needs a monotonic "how many pixels does one voxel cover" signal, and
//! the ±30% deadband absorbs its error.
//!
//! Distinct from `BodyVolumeCache::update_lod_resolutions` (adaptive BAKE resolution — re-bakes
//! the grid): this policy re-MESHES from the existing brick atlas at a coarser octree level, no
//! re-bake, no re-classify (O2a).

/// Level boundaries in projected-voxel pixels: `SPLIT[0]` separates L0/L1 (voxel ≥ 0.5 px = full
/// detail), `SPLIT[1]` separates L1/L2. Below `SPLIT[1]` a full-detail cell is far sub-pixel —
/// level 2 (8× fewer cells/axis... 4× per level in surface quads) is visually free.
pub(crate) const LOD_SPLIT_PX: [f32; 2] = [0.5, 0.25];

/// Hysteresis deadband: a boundary must be crossed by ±30% before the level switches. Coarsening
/// past boundary B needs `px < B·0.7`; refining back needs `px > B·1.3`. The bands are disjoint
/// (0.5·0.7 = 0.35 > 0.25·1.3 = 0.325), so the chooser can never oscillate for a static signal.
pub(crate) const LOD_DEADBAND: f32 = 0.30;

/// Minimum switch spacing per body (rate limit — at most one level change per body per second).
pub(crate) const LOD_MIN_SWITCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Hard level ceiling (the policy tiers are L0/L1/L2).
pub(crate) const LOD_MAX_LEVEL: u32 = 2;

/// Highest level this body may use: `(res−1) >> level` must stay ≥ 32 cells (below that the
/// coarse mesh collapses to mush), capped at [`LOD_MAX_LEVEL`].
pub(crate) fn lod_max_level_for_res(res: u32) -> u32 {
    let cells = res.saturating_sub(1);
    let mut l = 0;
    while l < LOD_MAX_LEVEL && (cells >> (l + 1)) >= 32 { l += 1; }
    l
}

/// The level chooser: projected-voxel pixels + the CURRENT level → the level the body should be
/// at. Pure and deterministic — the rate limit (1 switch/s) is the caller's, since it needs
/// wall-clock state. Steps through adjacent boundaries so a large signal jump can move more than
/// one level in one evaluation, but each boundary crossing individually respects the deadband.
pub(crate) fn choose_lod_level(projected_voxel_px: f32, current: u32, max_level: u32) -> u32 {
    let px = if projected_voxel_px.is_finite() { projected_voxel_px.max(0.0) } else {
        return 0; // degenerate signal (zero distance / zero viewport) → full detail, never coarsen
    };
    let mut lvl = current.min(max_level);
    loop {
        // Coarsen: the NEXT boundary must be undershot by the deadband.
        if lvl < max_level && (lvl as usize) < LOD_SPLIT_PX.len()
            && px < LOD_SPLIT_PX[lvl as usize] * (1.0 - LOD_DEADBAND) {
            lvl += 1;
            continue;
        }
        // Refine: the PREVIOUS boundary must be overshot by the deadband.
        if lvl > 0 && px > LOD_SPLIT_PX[(lvl - 1) as usize] * (1.0 + LOD_DEADBAND) {
            lvl -= 1;
            continue;
        }
        return lvl;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_chooser_boundaries_and_deadband() {
        // Fresh body (current 0): plain tiers apply only past the deadband.
        assert_eq!(choose_lod_level(2.0, 0, 2), 0, "big on screen = full detail");
        assert_eq!(choose_lod_level(0.5, 0, 2), 0, "exactly at the L0/L1 boundary: stay");
        assert_eq!(choose_lod_level(0.40, 0, 2), 0, "inside the deadband (0.35..0.5): no switch");
        assert_eq!(choose_lod_level(0.34, 0, 2), 1, "below 0.5*0.7: coarsen to L1");
        assert_eq!(choose_lod_level(0.20, 0, 2), 1, "0.175..0.325 from L0 → lands L1");
        assert_eq!(choose_lod_level(0.10, 0, 2), 2, "far sub-pixel: multi-step to L2");

        // Refining from L1 needs px > 0.5*1.3.
        assert_eq!(choose_lod_level(0.60, 1, 2), 1, "0.6 < 0.65: deadband holds L1");
        assert_eq!(choose_lod_level(0.66, 1, 2), 0, "past 0.65: refine to L0");
        // Coarsening from L1 needs px < 0.25*0.7.
        assert_eq!(choose_lod_level(0.20, 1, 2), 1, "0.2 > 0.175: deadband holds L1");
        assert_eq!(choose_lod_level(0.17, 1, 2), 2, "below 0.175: coarsen to L2");
        // Refining from L2 needs px > 0.25*1.3.
        assert_eq!(choose_lod_level(0.30, 2, 2), 2, "0.3 < 0.325: hold L2");
        assert_eq!(choose_lod_level(0.33, 2, 2), 1, "past 0.325: refine to L1");
        assert_eq!(choose_lod_level(5.0, 2, 2), 0, "zoom-in jump refines straight to L0");
    }

    #[test]
    fn lod_chooser_never_oscillates_for_a_static_signal() {
        // Fixed point: feeding the chooser its own output must be stable for any signal.
        for i in 0..2000 {
            let px = i as f32 * 0.001; // 0 .. 2.0 px
            for start in 0..=2u32 {
                let a = choose_lod_level(px, start, 2);
                assert_eq!(choose_lod_level(px, a, 2), a,
                    "oscillation at px={px} start={start}: {a} -> {}", choose_lod_level(px, a, 2));
            }
        }
    }

    #[test]
    fn lod_chooser_clamps_and_degenerates() {
        assert_eq!(choose_lod_level(0.01, 0, 0), 0, "max_level 0: never coarsen");
        assert_eq!(choose_lod_level(0.01, 2, 1), 1, "current above max clamps down");
        assert_eq!(choose_lod_level(f32::NAN, 2, 2), 0, "NaN signal → full detail");
        assert_eq!(choose_lod_level(f32::INFINITY, 2, 2), 0, "inf signal → full detail");
        // res clamp: (res-1)>>L must stay ≥ 32.
        assert_eq!(lod_max_level_for_res(65), 1, "64 cells: L1 (32) ok, L2 (16) not");
        assert_eq!(lod_max_level_for_res(64), 0, "63 cells: L1 = 31 < 32");
        assert_eq!(lod_max_level_for_res(129), 2, "128 cells: L2 = 32 ok");
        assert_eq!(lod_max_level_for_res(1025), 2, "capped at LOD_MAX_LEVEL");
        assert_eq!(lod_max_level_for_res(0), 0);
    }
}
