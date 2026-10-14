// voxel_sculpt/ops.rs — brush application: elastic grab / warp preview, stamp plans,
// masks, paint colors, fill_from_sdf, relax/smooth, symmetrize, snapshot/undo deltas.
// Split from voxel_sculpt.rs (pure relocation).

use glam::Vec3;
use super::*;

impl VoxelGrid {
    /// LEVEL-SET / DENSITY WARP PREVIEW (Move + Smudge).
    /// Single (anchor, drag) — see [`elastic_grab_preview_multi`] for symmetry.
    pub fn elastic_grab_preview(&mut self, grab: &mut ElasticGrab, drag: Vec3, rigid: bool) {
        let a = grab.anchor;
        self.elastic_grab_preview_multi(grab, &[(a, drag)], rigid);
    }

    /// Expand primary grab by X/Y/Z mirror planes (local origin). Same convention as stamp symmetry.
    pub fn grab_symmetry_instances(anchor: Vec3, drag: Vec3, symmetry: [bool; 3]) -> Vec<(Vec3, Vec3)> {
        let mut out = vec![(anchor, drag)];
        for axis in 0..3 {
            if !symmetry[axis] { continue; }
            let mirrored: Vec<_> = out.iter().map(|&(a, d)| {
                let (mut aa, mut dd) = (a.to_array(), d.to_array());
                aa[axis] = -aa[axis];
                dd[axis] = -dd[axis];
                (Vec3::from_array(aa), Vec3::from_array(dd))
            }).collect();
            out.extend(mirrored);
        }
        out.truncate(8);
        out
    }

    /// Move voxelize: density semi-Lagrangian warp (no mesh→SDF). Soft-round tip + mid-plane-safe
    /// blend matching the live GPU mesh grab. `instances` = `(anchor, drag, outer_r)`.
    ///
    /// **Perf:** multi-instance (symmetry) always warps **per side** on tight AABBs then merges —
    /// never one fat joint AABB spanning both mirrors (owner logs: 200–700 ms / 300k–750k cells).
    /// Worker should pass density-only via `from_density_field`.
    pub fn grab_density_displace_writes(
        &mut self,
        instances: &[(Vec3, Vec3, f32)],
    ) -> (Vec<(u32, f32, f32)>, [u32; 6]) {
        if instances.is_empty() {
            return (Vec::new(), [0, 0, 0, 0, 0, 0]);
        }
        // Always multi → independent per-instance (1D inverse each). Joint full-AABB was the cost bomb
        // with X symmetry (cells≈res²·brush_height). Mid-plane cells merge by stronger |Δ|.
        if instances.len() > 1 {
            return self.grab_density_displace_multi(instances);
        }
        self.grab_density_displace_joint(instances)
    }

    /// Lightweight density-only grid for Move bake workers (skips colors/mask/dirty clone cost).
    pub fn from_density_field(density: DensityStorage, res: u32, bounds: f32, iso: f32) -> Self {
        VoxelGrid {
            density,
            res,
            bounds,
            iso,
            dirty: None,
            colors: Vec::new(),
            mask: Vec::new(),
            mask_sparse: std::collections::HashMap::new(),
            show_mask: true,
        }
    }

    /// True when every pair of instance capsules is separated (no joint mid-plane region).
    pub(super) fn grab_instances_disjoint(&self, instances: &[(Vec3, Vec3, f32)]) -> bool {
        let vs = self.voxel_size();
        let pad = 2.0 * vs;
        for i in 0..instances.len() {
            for j in (i + 1)..instances.len() {
                let (a0, d0, r0) = instances[i];
                let (a1, d1, r1) = instances[j];
                // Capsules overlap if any source/dest spheres are within r0+r1+pad.
                for c0 in [a0, a0 + d0] {
                    for c1 in [a1, a1 + d1] {
                        let lim = r0 + r1 + pad;
                        if (c0 - c1).length_squared() < lim * lim {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Multi-instance / symmetry: independent 1D warp per instance on its own tight AABB, then merge.
    /// Avoids joint AABB over both mirrors (was 300k–750k cells @ 192³ with inst=2).
    fn grab_density_displace_multi(
        &mut self,
        instances: &[(Vec3, Vec3, f32)],
    ) -> (Vec<(u32, f32, f32)>, [u32; 6]) {
        let mut all: Vec<(u32, f32, f32)> = Vec::new();
        let mut ucx = u32::MAX;
        let mut ucy = u32::MAX;
        let mut ucz = u32::MAX;
        let mut umx = 0u32;
        let mut umy = 0u32;
        let mut umz = 0u32;
        let mut any = false;
        let mut max_ratio = 0.0f32;
        // Parallel per-instance warps (each samples pristine density; no cross-talk until merge).
        {
            use rayon::prelude::*;
            let results: Vec<_> = instances
                .par_iter()
                .map(|&inst| {
                    let ratio = inst.1.length() / inst.2.max(1e-6);
                    let (writes, dirty) = self.grab_warp_single_readonly(inst);
                    (writes, dirty, ratio)
                })
                .collect();
            for (writes, dirty, ratio) in results {
                max_ratio = max_ratio.max(ratio);
                if dirty[0] <= dirty[3] {
                    any = true;
                    ucx = ucx.min(dirty[0]); ucy = ucy.min(dirty[1]); ucz = ucz.min(dirty[2]);
                    umx = umx.max(dirty[3]); umy = umy.max(dirty[4]); umz = umz.max(dirty[5]);
                }
                all.extend(writes);
            }
        }
        if !any || all.is_empty() {
            return (Vec::new(), [0, 0, 0, 0, 0, 0]);
        }
        // Merge by index: mid-plane / near-touching capsules → keep stronger |Δ| / more solid.
        {
            use std::collections::HashMap;
            let mut merged: HashMap<u32, (f32, f32)> = HashMap::with_capacity(all.len());
            for (idx, pre, post) in all.drain(..) {
                merged.entry(idx).and_modify(|e| {
                    let old_d = (e.1 - e.0).abs();
                    let new_d = (post - pre).abs();
                    if new_d > old_d || (new_d >= old_d * 0.9 && post > e.1) {
                        *e = (pre, post);
                    }
                }).or_insert((pre, post));
            }
            all = merged.into_iter().map(|(idx, (pre, post))| (idx, pre, post)).collect();
        }
        for &(idx, _pre, post) in &all {
            self.density.set_idx(idx as usize, post);
        }
        let aabb = [ucx, ucy, ucz, umx, umy, umz];
        // Light post only. Skip if union AABB is huge (high-res symmetry) — polish was O(AABB)
        // and turned 400ms warps into multi-second (owner 424³ logs).
        let union_cells = if ucx <= umx {
            (umx - ucx + 1) as u64 * (umy - ucy + 1) as u64 * (umz - ucz + 1) as u64
        } else {
            0
        };
        let allow_post = union_cells > 0 && union_cells < 1_500_000 && self.res < 320;
        let (crack, relax_n, relax_a, iso_n) = if !allow_post {
            (0u32, 0u32, 0.0f32, 0u32)
        } else if max_ratio >= 1.50 {
            (1u32, 2u32, 0.45f32, 1u32)
        } else if max_ratio >= 0.85 {
            (1u32, 1u32, 0.40f32, 0u32)
        } else {
            (0u32, 0u32, 0.0f32, 0u32)
        };
        if crack > 0 { self.grab_fill_density_cracks(aabb, crack); }
        if relax_n > 0 { self.relax_region(aabb, relax_n, relax_a, true, None); }
        if iso_n > 0 { self.grab_smooth_iso_band_tiled(aabb, iso_n, 0.50, None); }
        if crack > 0 || relax_n > 0 || iso_n > 0 {
            let mut out_w = Vec::with_capacity(all.len());
            for (idx, pre, _) in all {
                let post = self.density.get_idx(idx as usize);
                if (pre - post).abs() > 1e-6 {
                    out_w.push((idx, pre, post));
                }
            }
            self.mark_dirty([ucx, ucy, ucz], [umx, umy, umz]);
            return (out_w, aabb);
        }
        self.mark_dirty([ucx, ucy, ucz], [umx, umy, umz]);
        (all, aabb)
    }

    /// Single-instance warp, read-only sample of `self.density` (does not mutate).
    /// Returns `(idx, pre, post)` for cells that would change + dirty AABB.
    ///
    /// **High-res path:** iterate only cells near the capsule (sphere-swept segment), NOT the
    /// bounding box. At 424³ a long pull's AABB was 3–8M empty cells (owner: 1.3–2.7s warp);
    /// sphere-sweep stays O(volume of brush × path length) ≈ tens–hundreds of k cells.
    fn grab_warp_single_readonly(
        &self,
        inst: (Vec3, Vec3, f32),
    ) -> (Vec<(u32, f32, f32)>, [u32; 6]) {
        let (a, d, r) = inst;
        let vs = self.voxel_size().max(1e-8);
        let pad = r + 2.5 * vs;
        let drag_ratio = d.length() / r.max(1e-6);
        // High res: skip heavy polish (dual/5-tap dominate when cell counts grow with res).
        let hi_res = self.res >= 256;
        let do_splat = drag_ratio >= 1.0 && !hi_res;
        let use_5tap = drag_ratio >= 1.0 && !hi_res;
        let dual_smooth = if hi_res {
            0u32
        } else if drag_ratio >= 1.50 {
            2
        } else if drag_ratio >= 0.85 {
            1
        } else {
            0
        };

        let cells = self.grab_collect_capsule_cells(a, d, pad);
        if cells.is_empty() {
            return (Vec::new(), [0, 0, 0, 0, 0, 0]);
        }
        // Gate polish by actual candidate count (owner: 192³ first stroke 374ms with 5-tap+dual).
        let n_cand = cells.len();
        let use_5tap = use_5tap && n_cand < 48_000;
        let dual_smooth = if n_cand > 80_000 { 0 } else { dual_smooth };

        let res = self.res;
        let bounds = self.bounds;
        let density = &self.density;
        // Parallel over candidate cells only.
        let posts: Vec<(u32, u32, u32, f32)> = {
            use rayon::prelude::*;
            cells
                .par_iter()
                .map(|&(x, y, z)| {
                    let p = grid_cell_world(x, y, z, res, bounds);
                    let s = Self::grab_inverse_1d(p, a, d, r);
                    let dens = if use_5tap {
                        Self::sample_static_smooth(res, bounds, density, s, vs * 0.35)
                    } else {
                        Self::sample_static(res, bounds, density, s)
                    }
                    .clamp(0.0, 1.0);
                    (x, y, z, dens)
                })
                .collect()
        };

        // Optional local dual smooth on a tight dual buffer only if small enough.
        let mut post_map: std::collections::HashMap<u32, f32> =
            std::collections::HashMap::with_capacity(posts.len());
        let mut cx = u32::MAX;
        let mut cy = u32::MAX;
        let mut cz = u32::MAX;
        let mut mx = 0u32;
        let mut my = 0u32;
        let mut mz = 0u32;
        for &(x, y, z, dens) in &posts {
            cx = cx.min(x); cy = cy.min(y); cz = cz.min(z);
            mx = mx.max(x); my = my.max(y); mz = mz.max(z);
            post_map.insert(self.idx(x, y, z) as u32, dens);
        }

        if dual_smooth > 0 && posts.len() < 400_000 {
            // Build dense dual only for the tight write AABB of candidates (still << fat capsule AABB).
            let span_x = (mx - cx + 1) as usize;
            let span_y = (my - cy + 1) as usize;
            let span_z = (mz - cz + 1) as usize;
            let plane = span_x * span_y;
            let mut out = vec![f32::NAN; plane * span_z];
            for &(x, y, z, dens) in &posts {
                let lx = (x - cx) as usize;
                let ly = (y - cy) as usize;
                let lz = (z - cz) as usize;
                out[lx + ly * span_x + lz * plane] = dens;
            }
            Self::smooth_dual_buffer_nan(&mut out, span_x, span_y, span_z, dual_smooth, 0.45, self.iso);
            post_map.clear();
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let lx = (x - cx) as usize;
                        let ly = (y - cy) as usize;
                        let lz = (z - cz) as usize;
                        let v = out[lx + ly * span_x + lz * plane];
                        if v.is_nan() { continue; }
                        post_map.insert(self.idx(x, y, z) as u32, v);
                    }
                }
            }
        }

        if do_splat {
            let iso = self.iso;
            let src_r2 = (r + 2.0 * vs).powi(2);
            // Only source-sphere candidates (subset of capsule cells).
            for &(x, y, z) in &cells {
                let p = grid_cell_world(x, y, z, res, bounds);
                if (p - a).length_squared() > src_r2 { continue; }
                let dens = self.density.get_idx(self.idx(x, y, z));
                if dens < iso { continue; }
                let w = grab_soft_round_falloff((p - a).length(), r);
                if w <= 1e-6 { continue; }
                let dest = p + d * w;
                // Nearest cell splat into post_map.
                let s = (res - 1) as f32;
                let g = ((dest + Vec3::splat(bounds)) / (2.0 * bounds).max(1e-6)) * s;
                let ix = g.x.round() as i32;
                let iy = g.y.round() as i32;
                let iz = g.z.round() as i32;
                if ix < 0 || iy < 0 || iz < 0 { continue; }
                let (ux, uy, uz) = (ix as u32, iy as u32, iz as u32);
                if ux >= res || uy >= res || uz >= res { continue; }
                let idx = self.idx(ux, uy, uz) as u32;
                let e = post_map.entry(idx).or_insert(0.0);
                *e = (*e).max(dens);
                cx = cx.min(ux); cy = cy.min(uy); cz = cz.min(uz);
                mx = mx.max(ux); my = my.max(uy); mz = mz.max(uz);
            }
        }

        let mut writes = Vec::with_capacity(post_map.len());
        for (idx, post) in post_map {
            let pre = self.density.get_idx(idx as usize);
            if (pre - post).abs() < 1e-6 { continue; }
            writes.push((idx, pre, post));
        }
        if writes.is_empty() {
            return (Vec::new(), [0, 0, 0, 0, 0, 0]);
        }
        (writes, [cx, cy, cz, mx, my, mz])
    }

    /// Cells near the capsule of radius `pad` around segment a→a+d.
    ///
    /// Cost is O(steps × r²) with circle slices (not O(steps × r³) full cubes). Step spacing is
    /// ~0.6·pad so spheres overlap without voxel-stepping the whole drag (owner: 374ms first
    /// stroke at 192³ was collect thrash with r_cells up to 96 and steps=256).
    fn grab_collect_capsule_cells(&self, a: Vec3, d: Vec3, pad: f32) -> Vec<(u32, u32, u32)> {
        let vs = self.voxel_size().max(1e-8);
        let res = self.res as i32;
        // Cap brush radius in cells — beyond this we still cover via denser stepping of smaller shells.
        let r_cells = ((pad / vs).ceil() as i32).clamp(1, 40);
        let r2 = r_cells * r_cells;
        let len = d.length();
        // Overlapping spheres along the path (step < 2·pad) — not one sphere per voxel.
        let step_world = (pad * 0.60).max(vs);
        let steps = if len < 1e-8 {
            0
        } else {
            ((len / step_world).ceil() as i32).clamp(1, 48)
        };
        let bounds = self.bounds;
        let s_lat = (self.res - 1).max(1) as f32;
        let world_to_cell = |p: Vec3| -> (i32, i32, i32) {
            let g = ((p + Vec3::splat(bounds)) / (2.0 * bounds).max(1e-6)) * s_lat;
            (g.x.round() as i32, g.y.round() as i32, g.z.round() as i32)
        };
        // Parallel per-step collect → sort/dedup (faster than HashSet for dense spheres).
        use rayon::prelude::*;
        let mut all: Vec<(u32, u32, u32)> = (0..=steps)
            .into_par_iter()
            .flat_map_iter(|si| {
                let t = if steps == 0 { 0.0 } else { si as f32 / steps as f32 };
                let c = a + d * t;
                let (cx, cy, cz) = world_to_cell(c);
                let mut local = Vec::with_capacity((4 * r_cells * r_cells) as usize);
                for dz in -r_cells..=r_cells {
                    let rxy2 = r2 - dz * dz;
                    if rxy2 < 0 { continue; }
                    // Integer circle in xy: for each dy, dx ∈ [-rx, rx].
                    let rxy = (rxy2 as f32).sqrt() as i32;
                    for dy in -rxy..=rxy {
                        let rx2 = rxy2 - dy * dy;
                        if rx2 < 0 { continue; }
                        let rx = (rx2 as f32).sqrt() as i32;
                        for dx in -rx..=rx {
                            let x = cx + dx;
                            let y = cy + dy;
                            let z = cz + dz;
                            if x < 1 || y < 1 || z < 1 || x >= res - 1 || y >= res - 1 || z >= res - 1 {
                                continue;
                            }
                            local.push((x as u32, y as u32, z as u32));
                        }
                    }
                }
                local
            })
            .collect();
        all.par_sort_unstable();
        all.dedup();
        all
    }

    /// Joint multi-instance warp (overlapping capsules / single instance).
    fn grab_density_displace_joint(
        &mut self,
        instances: &[(Vec3, Vec3, f32)],
    ) -> (Vec<(u32, f32, f32)>, [u32; 6]) {
        let vs = self.voxel_size().max(1e-8);
        let mut max_r = 0.0f32;
        let mut max_drag = 0.0f32;
        let mut cx = u32::MAX;
        let mut cy = u32::MAX;
        let mut cz = u32::MAX;
        let mut mx = 0u32;
        let mut my = 0u32;
        let mut mz = 0u32;
        let mut any = false;
        for &(a, d, r) in instances {
            max_r = max_r.max(r);
            max_drag = max_drag.max(d.length());
            let halo = r + 2.5 * vs;
            for center in [a, a + d] {
                if let Some(bb) = self.brush_aabb(center, halo) {
                    any = true;
                    cx = cx.min(bb[0]); cy = cy.min(bb[1]); cz = cz.min(bb[2]);
                    mx = mx.max(bb[3]); my = my.max(bb[4]); mz = mz.max(bb[5]);
                }
            }
        }
        if !any {
            return (Vec::new(), [0, 0, 0, 0, 0, 0]);
        }
        let pad_r = max_r + 2.5 * vs;
        let r2 = pad_r.max(1e-6).powi(2);
        let pairs: Vec<(Vec3, Vec3)> = instances.iter().map(|&(a, d, _)| (a, d)).collect();
        let drag_ratio = max_drag / max_r.max(1e-6);
        // Lean tiers (single-instance / rare joint fallback). Heavy polish only on large pulls.
        let (inv_iters, damp, do_splat, dual_smooth, crack_iters, relax_iters, relax_amt, iso_smooth) =
            if drag_ratio < 0.50 {
                (4u32, 1.0f32, false, 0u32, 0u32, 0u32, 0.0f32, 0u32)
            } else if drag_ratio < 1.20 {
                (6u32, 0.55f32, drag_ratio > 1.0, 1u32, 1u32, 2u32, 0.42f32, 0u32)
            } else {
                (10u32, 0.42f32, true, 2u32, 1u32, 3u32, 0.48f32, 1u32)
            };
        let need_post = crack_iters > 0 || relax_iters > 0 || iso_smooth > 0 || dual_smooth > 0;
        let conv2 = (0.02 * vs).powi(2);

        let n_inst = instances.len();
        let total_u = |s: Vec3| -> Vec3 {
            match n_inst {
                1 => {
                    let (a, d, r) = instances[0];
                    let w = grab_soft_round_falloff((s - a).length(), r);
                    if w <= 1e-6 { Vec3::ZERO } else { d * w }
                }
                2 => {
                    let (a0, d0, r0) = instances[0];
                    let (a1, d1, r1) = instances[1];
                    let w0 = grab_soft_round_falloff((s - a0).length(), r0);
                    let w1 = grab_soft_round_falloff((s - a1).length(), r1);
                    let sum_w = w0 + w1;
                    if sum_w <= 1e-5 { return Vec3::ZERO; }
                    let max_w = w0.max(w1);
                    (d0 * w0 + d1 * w1) * (max_w / sum_w)
                }
                _ => {
                    let mut sum_w = 0.0f32;
                    let mut sum_wd = Vec3::ZERO;
                    let mut max_w = 0.0f32;
                    for &(a, d, r) in instances {
                        let w = grab_soft_round_falloff((s - a).length(), r);
                        if w <= 1e-6 { continue; }
                        sum_w += w;
                        sum_wd += d * w;
                        max_w = max_w.max(w);
                    }
                    if sum_w > 1e-5 { (sum_wd / sum_w) * max_w } else { Vec3::ZERO }
                }
            }
        };

        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;
        let span_z = (mz - cz + 1) as usize;
        let plane = span_x * span_y;
        let n_cells = plane * span_z;

        // Pristine only when post-process can change cells beyond the warp dual-buffer.
        let pristine = if need_post {
            let mut p = vec![0.0f32; n_cells];
            let mut k = 0usize;
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        p[k] = self.density.get_idx(self.idx(x, y, z));
                        k += 1;
                    }
                }
            }
            Some(p)
        } else {
            None
        };

        let mut out = vec![0.0f32; n_cells];
        let res = self.res;
        let bounds = self.bounds;
        let single = n_inst == 1;
        // NOTE: do NOT seed multi-instance inverse with primary drag only — that tears the mirror
        // side on X/Y/Z symmetry (owner: one side nice, other pour). Live gbuffer uses max_w blend;
        // bake must seed with total_u(p) or per-cell dominant-instance 1D.
        {
            use rayon::prelude::*;
            let density = &self.density;
            let inst0 = instances[0];
            let inst_slice = instances;
            out.par_chunks_mut(plane).enumerate().for_each(|(zi, plane_out)| {
                let z = cz + zi as u32;
                let mut k = 0usize;
                for y in cy..=my {
                    for x in cx..=mx {
                        let p = grid_cell_world(x, y, z, res, bounds);
                        if !Self::grab_any_capsule(p, &pairs, r2) {
                            plane_out[k] = f32::NAN;
                            k += 1;
                            continue;
                        }
                        let s = if single {
                            Self::grab_inverse_1d(p, inst0.0, inst0.1, inst0.2)
                        } else {
                            // Dominant-instance 1D when clearly on one side of mid-plane (symmetric
                            // for mirrors). Mid-plane blend zone: Picard seeded by total_u(p).
                            let mut best_i = 0usize;
                            let mut best_w = 0.0f32;
                            let mut second_w = 0.0f32;
                            for (i, &(a, _, r)) in inst_slice.iter().enumerate() {
                                let w = grab_soft_round_falloff((p - a).length(), r);
                                if w > best_w {
                                    second_w = best_w;
                                    best_w = w;
                                    best_i = i;
                                } else if w > second_w {
                                    second_w = w;
                                }
                            }
                            if best_w > 1e-5 && best_w >= second_w * 2.0 {
                                let (a, d, r) = inst_slice[best_i];
                                Self::grab_inverse_1d(p, a, d, r)
                            } else {
                                // Symmetric seed: full mid-plane displacement at p (not primary d).
                                let mut s = p - total_u(p);
                                for _ in 0..inv_iters {
                                    let s_tgt = p - total_u(s);
                                    let delta = s_tgt - s;
                                    if delta.length_squared() < conv2 { s = s_tgt; break; }
                                    s = s + delta * damp;
                                }
                                s
                            }
                        };
                        // Large pulls: 5-tap average at preimage kills SL staircasing / sugar noise.
                        let dens = if drag_ratio >= 0.85 {
                            Self::sample_static_smooth(res, bounds, density, s, vs * 0.35)
                        } else {
                            Self::sample_static(res, bounds, density, s)
                        };
                        plane_out[k] = dens.clamp(0.0, 1.0);
                        k += 1;
                    }
                }
            });
        }

        // Dual-buffer Laplacian first (kills SL sugar), then splat reseals solid core melted by smooth.
        if dual_smooth > 0 {
            Self::smooth_dual_buffer_nan(
                &mut out, span_x, span_y, (mz - cz + 1) as usize, dual_smooth, 0.45, self.iso,
            );
        }
        if do_splat {
            let iso = self.iso;
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let dens = self.density.get_idx(self.idx(x, y, z));
                        if dens < iso { continue; }
                        let p = grid_cell_world(x, y, z, res, bounds);
                        let mut in_src = false;
                        for &(a, _, r) in instances {
                            if (p - a).length_squared() <= (r + 2.0 * vs).powi(2) {
                                in_src = true;
                                break;
                            }
                        }
                        if !in_src { continue; }
                        let u = total_u(p);
                        if u.length_squared() < 1e-12 { continue; }
                        Self::splat_density_nearest(
                            &mut out, cx, cy, cz, mx, my, mz, span_x, span_y,
                            res, bounds, p + u, dens,
                        );
                    }
                }
            }
        }

        // Apply dual buffer.
        let mut writes = Vec::with_capacity(n_cells / 8);
        let mut any_write = false;
        let mut k = 0usize;
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let v = out[k];
                    k += 1;
                    if v.is_nan() { continue; }
                    let idx = self.idx(x, y, z);
                    let pre = self.density.get_idx(idx);
                    if (pre - v).abs() < 1e-6 { continue; }
                    self.density.set_idx(idx, v);
                    any_write = true;
                    if !need_post {
                        writes.push((idx as u32, pre, v));
                    }
                }
            }
        }
        if !any_write {
            return (Vec::new(), [cx, cy, cz, mx, my, mz]);
        }

        let aabb = [cx, cy, cz, mx, my, mz];
        if crack_iters > 0 {
            self.grab_fill_density_cracks(aabb, crack_iters);
        }
        if relax_iters > 0 {
            self.relax_region(aabb, relax_iters, relax_amt, true, None);
        }
        // Extra iso-band polish: only cells near the surface (won't melt solid cores).
        if iso_smooth > 0 {
            self.grab_smooth_iso_band_tiled(aabb, iso_smooth, 0.55, None);
        }

        if need_post {
            let pristine = pristine.expect("pristine when need_post");
            writes.clear();
            k = 0;
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let pre = pristine[k];
                        k += 1;
                        let idx = self.idx(x, y, z);
                        let post = self.density.get_idx(idx);
                        if (pre - post).abs() < 1e-6 { continue; }
                        writes.push((idx as u32, pre, post));
                    }
                }
            }
        }
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
        (writes, aabb)
    }

    /// Trilinear sample + average of 6 face-neighbors at ±`half` — softens preimage noise on long pulls.
    #[inline]
    fn sample_static_smooth(
        res: u32, bounds: f32, density: &DensityStorage, s: Vec3, half: f32,
    ) -> f32 {
        let c = Self::sample_static(res, bounds, density, s);
        if half < 1e-8 { return c; }
        let h = half;
        let n = Self::sample_static(res, bounds, density, s + Vec3::new(h, 0.0, 0.0))
            + Self::sample_static(res, bounds, density, s + Vec3::new(-h, 0.0, 0.0))
            + Self::sample_static(res, bounds, density, s + Vec3::new(0.0, h, 0.0))
            + Self::sample_static(res, bounds, density, s + Vec3::new(0.0, -h, 0.0))
            + Self::sample_static(res, bounds, density, s + Vec3::new(0.0, 0.0, h))
            + Self::sample_static(res, bounds, density, s + Vec3::new(0.0, 0.0, -h));
        // Center heavily weighted so solid cores don't wash under iso.
        (c * 4.0 + n) / 10.0
    }

    /// 6-neighbor average of dual-buffer cells (NaN = outside capsule). Preserves iso side.
    fn smooth_dual_buffer_nan(
        out: &mut [f32], span_x: usize, span_y: usize, span_z: usize,
        iters: u32, amount: f32, iso: f32,
    ) {
        if iters == 0 || span_x == 0 || span_y == 0 || span_z == 0 { return; }
        let amount = amount.clamp(0.0, 1.0);
        let plane = span_x * span_y;
        let mut snap = out.to_vec();
        let offs: [(i32, i32, i32); 6] = [
            (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1),
        ];
        for _ in 0..iters {
            snap.copy_from_slice(out);
            for z in 0..span_z {
                for y in 0..span_y {
                    for x in 0..span_x {
                        let i = x + y * span_x + z * plane;
                        let cur = snap[i];
                        if cur.is_nan() { continue; }
                        let mut sum = 0.0f32;
                        let mut n = 0u32;
                        for (dx, dy, dz) in offs {
                            let nx = x as i32 + dx;
                            let ny = y as i32 + dy;
                            let nz = z as i32 + dz;
                            if nx < 0 || ny < 0 || nz < 0
                                || nx as usize >= span_x || ny as usize >= span_y || nz as usize >= span_z
                            {
                                continue;
                            }
                            let v = snap[nx as usize + ny as usize * span_x + nz as usize * plane];
                            if v.is_nan() { continue; }
                            sum += v;
                            n += 1;
                        }
                        if n == 0 { continue; }
                        let avg = sum / n as f32;
                        let mut nv = (cur + (avg - cur) * amount).clamp(0.0, 1.0);
                        if cur > iso && nv <= iso { nv = iso + 1e-4; }
                        else if cur < iso && nv >= iso { nv = iso - 1e-4; }
                        out[i] = nv;
                    }
                }
            }
        }
    }

    /// **Coat continuous-clay polish** — Laplacian only in the iso shell so stamp terraces melt
    /// without dissolving solid interior. Public for stroke-release / remesh polish (3D-Coat feel).
    /// `amount` 0..1, `iters` 1..4 typical. Records first-touch pre-values to `rec` when given.
    pub fn polish_iso_band(
        &mut self,
        aabb: [u32; 6],
        iters: u32,
        amount: f32,
        rec: Option<&mut StrokeRecorder>,
    ) {
        // Tile so peak alloc stays O(TILE³) — never allocate the full stroke AABB
        // (was ~512 MiB @512³ / ~4 GiB @1024³ → OOM / UI freeze on pen-up).
        self.grab_smooth_iso_band_tiled(aabb, iters, amount, rec);
        self.mark_dirty([aabb[0], aabb[1], aabb[2]], [aabb[3], aabb[4], aabb[5]]);
    }

    /// Smooth only the iso band (|d−iso| < band) so thin pulls get a clean silhouette without
    /// dissolving solid interior (preserve_iso-style clamp per cell).
    /// Processes 32³ tiles so peak memory stays bounded (P0 pen-up OOM fix).
    fn grab_smooth_iso_band_tiled(
        &mut self,
        aabb: [u32; 6],
        iters: u32,
        amount: f32,
        mut rec: Option<&mut StrokeRecorder>,
    ) {
        const TILE: u32 = 32;
        let [cx, cy, cz, mx, my, mz] = aabb;
        if cx > mx || cy > my || cz > mz || iters == 0 { return; }
        let iso = self.iso;
        let band = 0.38f32;
        let amount = amount.clamp(0.0, 1.0);
        let offs: [(i32, i32, i32); 6] = [
            (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1),
        ];
        // Max tile volume = 32³ = 32768 floats ≈ 128 KiB snapshot.
        let mut snap = vec![0.0f32; (TILE as usize).pow(3)];
        let mut z0 = cz;
        while z0 <= mz {
            let z1 = (z0 + TILE - 1).min(mz);
            let mut y0 = cy;
            while y0 <= my {
                let y1 = (y0 + TILE - 1).min(my);
                let mut x0 = cx;
                while x0 <= mx {
                    let x1 = (x0 + TILE - 1).min(mx);
                    if self.density.is_sparse() {
                        if let Some(0.0) = self.density.region_const([
                            x0 as i32, y0 as i32, z0 as i32,
                            x1 as i32, y1 as i32, z1 as i32,
                        ]) {
                            x0 = x0.saturating_add(TILE);
                            if x0 == 0 { break; }
                            continue;
                        }
                    }
                    let span_x = (x1 - x0 + 1) as usize;
                    let span_y = (y1 - y0 + 1) as usize;
                    let span_z = (z1 - z0 + 1) as usize;
                    let need = span_x * span_y * span_z;
                    if snap.len() < need { snap.resize(need, 0.0); }
                    for _ in 0..iters {
                        let mut k = 0usize;
                        for z in z0..=z1 {
                            for y in y0..=y1 {
                                for x in x0..=x1 {
                                    snap[k] = self.density.get_idx(self.idx(x, y, z));
                                    k += 1;
                                }
                            }
                        }
                        for z in z0..=z1 {
                            for y in y0..=y1 {
                                for x in x0..=x1 {
                                    let lx = (x - x0) as usize;
                                    let ly = (y - y0) as usize;
                                    let lz = (z - z0) as usize;
                                    let cur = snap[lx + ly * span_x + lz * span_x * span_y];
                                    if (cur - iso).abs() > band { continue; }
                                    let mut sum = 0.0f32;
                                    let mut n = 0u32;
                                    for (dx, dy, dz) in offs {
                                        let nx = lx as i32 + dx;
                                        let ny = ly as i32 + dy;
                                        let nz = lz as i32 + dz;
                                        let nv = if nx < 0 || ny < 0 || nz < 0
                                            || nx as usize >= span_x || ny as usize >= span_y || nz as usize >= span_z
                                        {
                                            // Halo: sample live grid outside tile.
                                            let gx = x as i32 + dx;
                                            let gy = y as i32 + dy;
                                            let gz = z as i32 + dz;
                                            if gx < 0 || gy < 0 || gz < 0
                                                || gx as u32 >= self.res || gy as u32 >= self.res || gz as u32 >= self.res
                                            {
                                                cur
                                            } else {
                                                self.density.get_idx(self.idx(gx as u32, gy as u32, gz as u32))
                                            }
                                        } else {
                                            snap[nx as usize + ny as usize * span_x + nz as usize * span_x * span_y]
                                        };
                                        sum += nv;
                                        n += 1;
                                    }
                                    let mut nv = (cur + (sum / n as f32 - cur) * amount).clamp(0.0, 1.0);
                                    if cur > iso && nv <= iso { nv = iso + 1e-4; }
                                    else if cur < iso && nv >= iso { nv = iso - 1e-4; }
                                    if (nv - cur).abs() < 1e-7 { continue; }
                                    let i = self.idx(x, y, z);
                                    if let Some(r) = rec.as_deref_mut() { r.record(i as u32, cur); }
                                    self.density.set_idx(i, nv);
                                }
                            }
                        }
                    }
                    x0 = x0.saturating_add(TILE);
                    if x0 == 0 { break; }
                }
                y0 = y0.saturating_add(TILE);
                if y0 == 0 { break; }
            }
            z0 = z0.saturating_add(TILE);
            if z0 == 0 { break; }
        }
    }

    /// Max-splat into the single nearest dual-buffer cell (less blocky than 8-corner deposit).
    fn splat_density_nearest(
        out: &mut [f32],
        cx: u32, cy: u32, cz: u32,
        mx: u32, my: u32, mz: u32,
        span_x: usize, span_y: usize,
        res: u32, bounds: f32,
        p: Vec3, val: f32,
    ) {
        if val <= 1e-6 { return; }
        let s = (res - 1) as f32;
        let g = ((p + Vec3::splat(bounds)) / (2.0 * bounds).max(1e-6)) * s;
        let ix = g.x.round() as i32;
        let iy = g.y.round() as i32;
        let iz = g.z.round() as i32;
        if ix < cx as i32 || iy < cy as i32 || iz < cz as i32 { return; }
        if ix > mx as i32 || iy > my as i32 || iz > mz as i32 { return; }
        let lx = (ix as u32 - cx) as usize;
        let ly = (iy as u32 - cy) as usize;
        let lz = (iz as u32 - cz) as usize;
        let idx = lx + ly * span_x + lz * span_x * span_y;
        let cur = if out[idx].is_nan() { 0.0 } else { out[idx] };
        out[idx] = cur.max(val);
    }

    /// 1D inverse for a single grab instance: find `t ∈ [0,1]` s.t. `t ≈ w(|p − t·d − a|)`,
    /// then `s = p − t·d`. Bisection (~12 steps) — much cheaper and more stable than 3D Picard
    /// when only one instance is active.
    #[inline]
    fn grab_inverse_1d(p: Vec3, a: Vec3, d: Vec3, r: f32) -> Vec3 {
        let d2 = d.length_squared();
        if d2 < 1e-14 {
            return p;
        }
        // r(t) = t − w(|p − a − t·d|); r(0)≤0, r(1)≥0 typically → bisection.
        let mut lo = 0.0f32;
        let mut hi = 1.0f32;
        for _ in 0..12 {
            let mid = 0.5 * (lo + hi);
            let s = p - d * mid;
            let w = grab_soft_round_falloff((s - a).length(), r);
            if mid > w { hi = mid; } else { lo = mid; }
        }
        let t = 0.5 * (lo + hi);
        p - d * t
    }

    /// Close single-cell density holes inside a Move dirty region (exterior cell with ≥5 solid
    /// 6-neighbours → fill; interior with ≥5 empty → open). Stops thin surface tears after large
    /// semi-Lagrangian pulls without a mesh→SDF round-trip.
    fn grab_fill_density_cracks(&mut self, aabb: [u32; 6], iters: u32) {
        let [cx, cy, cz, mx, my, mz] = aabb;
        if cx > mx || cy > my || cz > mz || iters == 0 { return; }
        let iso = self.iso;
        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;
        let span_z = (mz - cz + 1) as usize;
        let mut snap = vec![0.0f32; span_x * span_y * span_z];
        let offs: [(i32, i32, i32); 6] = [
            (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1),
        ];
        for _ in 0..iters {
            let mut k = 0usize;
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        snap[k] = self.density.get_idx(self.idx(x, y, z));
                        k += 1;
                    }
                }
            }
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let lx = (x - cx) as usize;
                        let ly = (y - cy) as usize;
                        let lz = (z - cz) as usize;
                        let cur = snap[lx + ly * span_x + lz * span_x * span_y];
                        let mut n_in = 0u32;
                        let mut n_out = 0u32;
                        let mut sum_in = 0.0f32;
                        let mut sum_out = 0.0f32;
                        for (dx, dy, dz) in offs {
                            let nx = lx as i32 + dx;
                            let ny = ly as i32 + dy;
                            let nz = lz as i32 + dz;
                            let nv = if nx < 0 || ny < 0 || nz < 0
                                || nx as usize >= span_x || ny as usize >= span_y || nz as usize >= span_z
                            {
                                cur
                            } else {
                                snap[nx as usize + ny as usize * span_x + nz as usize * span_x * span_y]
                            };
                            if nv > iso {
                                n_in += 1;
                                sum_in += nv;
                            } else {
                                n_out += 1;
                                sum_out += nv;
                            }
                        }
                        let idx = self.idx(x, y, z);
                        // Hole in solid: exterior with mostly solid neighbours → fill.
                        if cur < iso && n_in >= 5 {
                            let fill = (sum_in / n_in as f32).max(iso + 0.05).clamp(0.0, 1.0);
                            self.density.set_idx(idx, fill);
                        // Speck tear: interior with mostly empty neighbours → open (thin sheet break).
                        } else if cur > iso && n_out >= 5 {
                            let open = (sum_out / n_out as f32).min(iso - 0.05).clamp(0.0, 1.0);
                            self.density.set_idx(idx, open);
                        }
                    }
                }
            }
        }
    }

    /// Semi-Lagrangian inverse warp from stroke-start snapshot for one or more (anchor, drag) pairs
    /// (primary + symmetry mirrors). `u(x) = Σ Δ_i · w_i(x)`, `φ₁(x) = φ₀(x − u)`.
    ///
    /// **Move (`rigid=true`) — grab and pull:** weight is a sphere plateau around the ORIGINAL
    /// anchor only. The inner core translates as a coherent block (full Δ); only a thin rim blends.
    /// Capsule distance was used earlier so long pulls would not "stop", but that made every cell
    /// along the path drag with full weight → smear / taffy / thin. Large |Δ| is handled instead by
    /// inverse-map init `s₀ = p − Δ` plus more fixed-point iters (still sphere weight at anchor).
    ///
    /// **Smudge (`rigid=false`) — intentional soft smear:** capsule falloff along the drag segment.
    ///
    /// Dirty AABB is still the capsule (source ∪ destination) so the landing region is written.
    /// Perf (192³ live): no mid-drag redistance; parallel z-planes; dual-buffer write.
    /// Full redistance is on release only.
    pub fn elastic_grab_preview_multi(
        &mut self,
        grab: &mut ElasticGrab,
        instances: &[(Vec3, Vec3)],
        rigid: bool,
    ) {
        if !grab.active || instances.is_empty() { return; }
        let radius = grab.radius.max(1e-6);
        let hardness = grab.hardness;
        let vs = self.voxel_size();
        let halo = radius + 2.0 * vs;
        let inv_r = 1.0 / radius;
        let r2 = radius * radius;

        // Capsule AABB = union over instances of sphere(a) ∪ sphere(a+d), + halo.
        // Write coverage only — falloff weight for Move is still sphere-at-anchor.
        let mut cx = u32::MAX;
        let mut cy = u32::MAX;
        let mut cz = u32::MAX;
        let mut mx = 0u32;
        let mut my = 0u32;
        let mut mz = 0u32;
        let mut any = false;
        let mut max_drag = 0.0f32;
        for &(a, d) in instances {
            max_drag = max_drag.max(d.length());
            for center in [a, a + d] {
                if let Some(bb) = self.brush_aabb(center, halo) {
                    any = true;
                    cx = cx.min(bb[0]); cy = cy.min(bb[1]); cz = cz.min(bb[2]);
                    mx = mx.max(bb[3]); my = my.max(bb[4]); mz = mz.max(bb[5]);
                }
            }
        }
        if !any { return; }

        // 1) RESTORE only voxels already captured (usually << full AABB after first frames).
        for (&i, &pre) in grab.pristine.iter() {
            let iu = i as usize;
            if iu < self.density.len() { self.density.set_idx(iu, pre); }
        }

        // 2) First-touch pristine — cells that can be inside any capsule (source + landing).
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let p = self.cell_world_pos(x, y, z);
                    if !Self::grab_any_capsule(p, instances, r2) { continue; }
                    let i = self.idx(x, y, z) as u32;
                    grab.pristine.entry(i).or_insert_with(|| self.density.get_idx(i as usize));
                }
            }
        }

        // Weight at source sample `s` for instance (a, d).
        // Move:  sphere plateau at ORIGINAL anchor → coherent block translate (grab & pull).
        // Smudge: capsule along a→a+d → soft path smear (intentional).
        let weight = |s: Vec3, a0: Vec3, d: Vec3| -> f32 {
            if rigid {
                let t = ((s - a0).length() * inv_r).clamp(0.0, 1.0);
                grab_plateau_falloff(t, hardness)
            } else {
                let ab2 = d.length_squared();
                let dist = if ab2 > 1e-12 {
                    let t = ((s - a0).dot(d) / ab2).clamp(0.0, 1.0);
                    (s - (a0 + d * t)).length()
                } else {
                    (s - a0).length()
                };
                let t = (dist * inv_r).clamp(0.0, 1.0);
                if t >= 1.0 { return 0.0; }
                let e = 1.0 - t;
                e * e * (3.0 - 2.0 * e)
            }
        };

        // u(x) = Σ Δ_i · w_i(x) over primary + symmetry instances.
        let total_u = |s: Vec3| -> Vec3 {
            let mut u = Vec3::ZERO;
            for &(a, d) in instances {
                let w = weight(s, a, d);
                if w > 1e-5 { u += d * w; }
            }
            u
        };

        // Full-drag guess for inverse init (critical when |Δ| ≳ R: starting at s=p freezes because
        // sphere weight at the landing tip is 0). Sum of instance drags is a safe overestimate.
        let d_guess = {
            let mut t = Vec3::ZERO;
            for &(_, d) in instances { t += d; }
            t
        };
        // More iters for long pulls so fixed-point inverse converges under sphere weight.
        // Large |Δ|/R needs more Picard steps or the landing core samples empty space (under-iso tear).
        let inv_iters = (3u32 + (max_drag * inv_r * 1.5).ceil() as u32).clamp(3, 16);

        // Live: inverse-map iters, parallel z-planes.
        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;
        let span_z = (mz - cz + 1) as usize;
        let plane = span_x * span_y;
        let mut out = vec![0.0f32; span_x * span_y * span_z];
        let res = self.res;
        let bounds = self.bounds;

        {
            use rayon::prelude::*;
            out.par_chunks_mut(plane).enumerate().for_each(|(zi, plane_out)| {
                let z = cz + zi as u32;
                let mut k = 0usize;
                for y in cy..=my {
                    for x in cx..=mx {
                        let p = grid_cell_world(x, y, z, res, bounds);
                        if !Self::grab_any_capsule(p, instances, r2) {
                            plane_out[k] = f32::NAN;
                            k += 1;
                            continue;
                        }
                        // Inverse: find source s with s + u(s) ≈ p. Init at p−Δ (full drag guess)
                        // so long pulls still sample the grabbed block, not empty space at the tip.
                        let mut s = p - d_guess;
                        for _ in 0..inv_iters {
                            s = p - total_u(s);
                        }
                        plane_out[k] = Self::sample_static(res, bounds, &self.density, s).clamp(0.0, 1.0);
                        k += 1;
                    }
                }
            });
        }

        // Commit (serial — density writes).
        let mut k = 0usize;
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let v = out[k];
                    k += 1;
                    if v.is_nan() { continue; }
                    self.density.set_idx(self.idx(x, y, z), v);
                }
            }
        }
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
    }

    /// True if `p` is inside any instance capsule of radius R (sphere at a or a+d, or the segment).
    #[inline]
    fn grab_any_capsule(p: Vec3, instances: &[(Vec3, Vec3)], r2: f32) -> bool {
        for &(a, d) in instances {
            if (p - a).length_squared() <= r2 { return true; }
            let b = a + d;
            if (p - b).length_squared() <= r2 { return true; }
            // Segment distance².
            let ab = d;
            let ap = p - a;
            let ab2 = ab.length_squared();
            if ab2 > 1e-12 {
                let t = (ap.dot(ab) / ab2).clamp(0.0, 1.0);
                if (ap - ab * t).length_squared() <= r2 { return true; }
            }
        }
        false
    }

    /// Trilinear sample of density storage at world `p` without `&mut self` (for rayon).
    #[inline]
    pub(super) fn sample_static(res: u32, bounds: f32, density: &DensityStorage, p: Vec3) -> f32 {
        let s = (res - 1) as f32;
        let g = ((p + Vec3::splat(bounds)) / (2.0 * bounds).max(1e-6)) * s;
        let x0 = g.x.floor() as i32;
        let y0 = g.y.floor() as i32;
        let z0 = g.z.floor() as i32;
        let fx = g.x - x0 as f32;
        let fy = g.y - y0 as f32;
        let fz = g.z - z0 as f32;
        let at = |x: i32, y: i32, z: i32| density.at_i(x, y, z);
        let c000 = at(x0, y0, z0);
        let c100 = at(x0 + 1, y0, z0);
        let c010 = at(x0, y0 + 1, z0);
        let c110 = at(x0 + 1, y0 + 1, z0);
        let c001 = at(x0, y0, z0 + 1);
        let c101 = at(x0 + 1, y0, z0 + 1);
        let c011 = at(x0, y0 + 1, z0 + 1);
        let c111 = at(x0 + 1, y0 + 1, z0 + 1);
        let c00 = c000 + (c100 - c000) * fx;
        let c10 = c010 + (c110 - c010) * fx;
        let c01 = c001 + (c101 - c001) * fx;
        let c11 = c011 + (c111 - c011) * fx;
        let c0 = c00 + (c10 - c00) * fy;
        let c1 = c01 + (c11 - c01) * fy;
        c0 + (c1 - c0) * fz
    }

    /// Full local redistance after a Move release: crack-fill + relax so large pulls
    /// do not leave destination density under iso (empty core tear).
    pub fn grab_redistance_region(&mut self, aabb: [u32; 6]) {
        self.grab_fill_density_cracks(aabb, 3);
        self.relax_region(aabb, 3, 0.45, true, None);
        // Second crack pass after relax can re-open thin sheets — one more mild fill.
        self.grab_fill_density_cracks(aabb, 1);
    }

    /// MESH→SDF RE-VOXELIZE WRITE (MOVE tool Phase 2). Write the density field over the inclusive cell
    /// AABB `[gx0,gy0,gz0 .. gx1,gy1,gz1]` from an externally-computed signed distance field `sdf`
    /// (one value per cell, in VoxelGrid sub-region order `x + y*nx + z*nx*ny`, as produced by
    /// `io_bridge::region_mesh_to_sdf`). Records each changed voxel's PRE value so the caller commits ONE
    /// sculpt-undo delta. Only the AABB voxels are touched, so regions the grab never swept never drift.
    ///
    /// SDF → density convention: the sculpt field is density ∈ [0,1] with iso 0.5, inside = high; the SDF
    /// is negative inside. `density = clamp(0.5 − sdf / W, 0, 1)`, `W` = the iso band width (≈ 2·voxel
    /// size) so the surface (sdf = 0) maps to density 0.5 and the band spans ~1-2 voxels. Marks the
    /// written AABB dirty for the caller's re-mesh.
    ///
    /// Returns the undo delta `(flat_index, pre_density)` for every voxel whose value actually changed.
    /// SIGN RECONCILIATION for a region mesh→SDF bake (fuzz-caught 2026-07-23, seed 5296).
    ///
    /// `region_mesh_to_sdf`'s sign is the nearest-TRIANGLE pseudo-normal test — locally fine in
    /// the narrow band, but for cells FAR from the surface the closest feature is a silhouette
    /// edge/vertex, and folded or self-intersecting stroke geometry (a pinched Snake Hook tube
    /// collapses triangles through its axis, inverting their normals) makes that test flip.
    /// `write_region_from_sdf` rewrites EVERY region cell from the sign, so ONE misclassified
    /// far cell becomes fully-solid garbage — the whole-slab / random-blob class (measured:
    /// 13 793 out-of-bounds verts after one symmetric hook + revoxelize).
    ///
    /// Fix (topological, normal-free where normals are unreliable): the sign of every cell
    /// OUTSIDE the ±`w` band is re-derived by 6-connected flood fill seeded from TRUSTED state —
    /// the pre-bake density of the cells just outside the region (unchanged by this bake), and
    /// the sealed grid border (always empty). The band (|d| ≤ w, at least 2 cells thick on each
    /// side of the true surface since w = 2·voxel_size ≥ 2 cell spacings) blocks the flood, so a
    /// label can never leak across the surface; the band itself keeps the geometric sign (there
    /// the closest feature is a face interior at ≤2 voxels — reliable). Far cells the flood
    /// cannot reach (pockets fully enclosed by band) keep their original sign.
    /// O(region cells) — noise next to the SDF computation itself.
    pub fn reconcile_region_sdf_signs(&self, aabb: [u32; 6], sdf: &mut [f32], w: f32) -> u32 {
        let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
        let nx = (gx1 - gx0 + 1) as usize;
        let ny = (gy1 - gy0 + 1) as usize;
        let nz = (gz1 - gz0 + 1) as usize;
        if sdf.len() != nx * ny * nz { return 0; }
        let band = w.max(1e-6);
        const UNKNOWN: u8 = 0;
        const OUTSIDE: u8 = 1;
        const INSIDE: u8 = 2;
        let mut label = vec![UNKNOWN; sdf.len()];
        let li = |x: usize, y: usize, z: usize| x + y * nx + z * nx * ny;
        let mut queue: std::collections::VecDeque<(usize, usize, usize)> = std::collections::VecDeque::new();
        // Seed every non-band region-boundary cell from its trusted out-of-region neighbours.
        for z in 0..nz { for y in 0..ny { for x in 0..nx {
            if x != 0 && x != nx - 1 && y != 0 && y != ny - 1 && z != 0 && z != nz - 1 { continue; }
            let i = li(x, y, z);
            if sdf[i].abs() <= band { continue; }
            let (gx, gy, gz) = (gx0 + x as u32, gy0 + y as u32, gz0 + z as u32);
            let mut vote_out = 0u32;
            let mut vote_in = 0u32;
            for (dx, dy, dz) in [(-1i64, 0i64, 0i64), (1, 0, 0), (0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1)] {
                let (ngx, ngy, ngz) = (gx as i64 + dx, gy as i64 + dy, gz as i64 + dz);
                // Only neighbours OUTSIDE the region carry trusted (pre-bake) state.
                let in_region = ngx >= gx0 as i64 && ngx <= gx1 as i64
                    && ngy >= gy0 as i64 && ngy <= gy1 as i64
                    && ngz >= gz0 as i64 && ngz <= gz1 as i64;
                if in_region { continue; }
                if ngx < 0 || ngy < 0 || ngz < 0
                    || ngx >= self.res as i64 || ngy >= self.res as i64 || ngz >= self.res as i64 {
                    vote_out += 1; // beyond the sealed grid border = empty
                    continue;
                }
                let fi = self.flat_index(ngx as u32, ngy as u32, ngz as u32);
                if fi < self.density.len() {
                    if self.density.get_idx(fi) >= self.iso { vote_in += 1; } else { vote_out += 1; }
                }
            }
            if vote_out == 0 && vote_in == 0 { continue; }
            label[i] = if vote_in > vote_out { INSIDE } else { OUTSIDE };
            queue.push_back((x, y, z));
        }}}
        // 6-connected flood through non-band cells.
        while let Some((x, y, z)) = queue.pop_front() {
            let l = label[li(x, y, z)];
            for (dx, dy, dz) in [(-1i64, 0i64, 0i64), (1, 0, 0), (0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1)] {
                let (nxx, nyy, nzz) = (x as i64 + dx, y as i64 + dy, z as i64 + dz);
                if nxx < 0 || nyy < 0 || nzz < 0
                    || nxx >= nx as i64 || nyy >= ny as i64 || nzz >= nz as i64 { continue; }
                let (ux, uy, uz) = (nxx as usize, nyy as usize, nzz as usize);
                let j = li(ux, uy, uz);
                if label[j] != UNKNOWN { continue; }
                if sdf[j].abs() <= band { continue; } // band blocks the flood
                label[j] = l;
                queue.push_back((ux, uy, uz));
            }
        }
        // Rewrite far-cell signs that disagree with their flood label.
        let mut flipped = 0u32;
        for i in 0..sdf.len() {
            let want_neg = match label[i] {
                INSIDE => true,
                OUTSIDE => false,
                _ => continue,
            };
            if (sdf[i] < 0.0) != want_neg {
                sdf[i] = -sdf[i];
                flipped += 1;
            }
        }
        flipped
    }

    pub fn write_region_from_sdf(&mut self, aabb: [u32; 6], sdf: &[f32], w: f32) -> Vec<(u32, f32)> {
        self.write_region_from_sdf_ex(aabb, sdf, w, true)
    }

    /// [`Self::write_region_from_sdf`] with the undo delta made optional.
    ///
    /// A FULL-grid write changes nearly every cell, so the delta is one `(u32, f32)` per
    /// voxel — ~134 MB of allocation at 256³. The poly Remesh modifier discards it (its
    /// grid is transient and its undo is the modifier stack's, not a sculpt delta), so
    /// building it was pure cost. Sculpt callers keep `want_delta = true`.
    pub fn write_region_from_sdf_ex(
        &mut self,
        aabb: [u32; 6],
        sdf: &[f32],
        w: f32,
        want_delta: bool,
    ) -> Vec<(u32, f32)> {
        let [gx0, gy0, gz0, gx1, gy1, gz1] = aabb;
        let nx = (gx1 - gx0 + 1) as usize;
        let ny = (gy1 - gy0 + 1) as usize;
        let nz = (gz1 - gz0 + 1) as usize;
        let mut delta: Vec<(u32, f32)> = Vec::new();
        if sdf.len() != nx * ny * nz { return delta; }
        let inv_w = 1.0 / w.max(1e-6);

        // DENSE FAST PATH (2026-08-08): the serial triple loop cost 45ms on a 256³ remesh
        // write. A Dense grid is one flat buffer, so `par_chunks_mut` over whole grid
        // z-planes gives every worker a disjoint slice. Deltas are collected per plane and
        // concatenated in gz order, so the returned Vec is IDENTICAL (same entries, same
        // order) to the serial form — undo replay is untouched. Sparse keeps the serial
        // path below (brick writes cannot be split this way).
        if matches!(self.density, DensityStorage::Dense { .. }) {
            use rayon::prelude::*;
            let res = self.res as usize;
            let plane = res * res;
            let per: Vec<Vec<(u32, f32)>> = self
                .density
                .as_dense_slice_mut()
                .par_chunks_mut(plane)
                .enumerate()
                .map(|(gz, buf)| {
                    let mut d_local: Vec<(u32, f32)> = Vec::new();
                    if gz < gz0 as usize || gz > gz1 as usize {
                        return d_local;
                    }
                    let z = gz - gz0 as usize;
                    for y in 0..ny {
                        let gy = gy0 as usize + y;
                        for x in 0..nx {
                            let d = sdf[x + y * nx + z * nx * ny];
                            let new_density = (0.5 - d * inv_w).clamp(0.0, 1.0);
                            let gx = gx0 as usize + x;
                            let local = gx + gy * res;
                            if local >= buf.len() { continue; }
                            let pre = buf[local];
                            if (pre - new_density).abs() > 1e-6 {
                                if want_delta {
                                    d_local.push(((gz * plane + local) as u32, pre));
                                }
                                buf[local] = new_density;
                            }
                        }
                    }
                    d_local
                })
                .collect();
            for p in per {
                delta.extend(p);
            }
            self.mark_dirty([gx0, gy0, gz0], [gx1, gy1, gz1]);
            return delta;
        }

        for z in 0..nz { for y in 0..ny { for x in 0..nx {
            let d = sdf[x + y * nx + z * nx * ny];
            let new_density = (0.5 - d * inv_w).clamp(0.0, 1.0);
            let gx = gx0 + x as u32;
            let gy = gy0 + y as u32;
            let gz = gz0 + z as u32;
            let fi = self.flat_index(gx, gy, gz);
            if fi >= self.density.len() { continue; }
            let pre = self.density.get_idx(fi);
            // Only record + write voxels that actually change (idempotent re-bake of unchanged cells is
            // a no-op, and recording them would bloat the undo delta with zero-effect entries).
            if (pre - new_density).abs() > 1e-6 {
                if want_delta {
                    delta.push((fi as u32, pre));
                }
                self.density.set_idx(fi, new_density);
            }
        }}}
        self.mark_dirty([gx0, gy0, gz0], [gx1, gy1, gz1]);
        delta
    }
}

impl VoxelGrid {
    // ------------------------------------------------------------------
    // Brushes — add/remove material with a spherical falloff.
    // ------------------------------------------------------------------

    /// Voxel-space AABB of a brush (clamped to the grid), or None if fully outside.
    pub fn brush_aabb(&self, center: Vec3, radius: f32) -> Option<[u32; 6]> {
        let vs = self.voxel_size();
        let g_min = self.world_to_grid(center - Vec3::splat(radius + vs));
        let g_max = self.world_to_grid(center + Vec3::splat(radius + vs));
        // SEALED BORDER: clamp writes to [1, res-2], leaving the outermost density layer empty (0).
        // The Surface-Nets mesher only emits a face where density CROSSES the iso level; at the grid
        // edge there is no cell beyond it, so geometry written right up to the boundary has no "air"
        // to cross into → the surface stays OPEN there (a hollow, un-capped end — the "hole" when a
        // shape grows into a grid face). Keeping the border empty guarantees a watertight cap.
        let lo = 1.0;
        let hi = (self.res as f32 - 2.0).max(1.0);
        let cx = g_min.x.floor().clamp(lo, hi) as u32;
        let cy = g_min.y.floor().clamp(lo, hi) as u32;
        let cz = g_min.z.floor().clamp(lo, hi) as u32;
        let mx = g_max.x.ceil().clamp(lo, hi) as u32;
        let my = g_max.y.ceil().clamp(lo, hi) as u32;
        let mz = g_max.z.ceil().clamp(lo, hi) as u32;
        if cx > mx || cy > my || cz > mz { return None; }
        Some([cx, cy, cz, mx, my, mz])
    }

    /// Apply one brush stamp. `strength` ∈ [0,1] is the per-stamp amount.
    /// `invert` (Ctrl) flips the brush direction generically so add/subtract works for every brush.
    /// `hardness` ∈ [0,1] is the continuous radial falloff knob (0 = soft round shoulder, 1 = sharp
    /// pointed core); every brush scales by `falloff_weight(1-t, hardness)`.
    /// `drag_delta` is the world-space cursor movement for THIS stamp (in voxel-LOCAL space, the same
    /// frame `center` is in) — non-zero only for the Move/Grab brush, which grabs the field along it;
    /// every other brush ignores it.
    ///
    /// PHASE 0 (BRUSH_SYSTEM_ARCHITECTURE.md): this is now a thin shim over the `StampPlan` POD
    /// path. It compiles the active brush into a `BrushRecipe` (`from_builtin`), folds it into a
    /// flat `StampPlan` (`fold_recipe`), and runs `apply_stamp_plan` — which reproduces the EXACT
    /// math the old per-`match brush` loop did. `apply_brush_legacy` (below) is the byte-for-byte
    /// reference the golden parity test compares against. The `normal`/`strength` passed in are
    /// already the producer's resolved (Direction- and pressure-applied) values, so the recipe is
    /// built with neutral Direction/pressure and the plan packs those resolved scalars verbatim.
    pub fn apply_brush(&mut self, brush: VoxelBrush, center: Vec3, normal: Vec3, radius: f32, strength: f32, invert: bool, hardness: f32, drag_delta: Vec3, rec: Option<&mut StrokeRecorder>) {
        use crate::brush::{self, Direction};
        let builtin = brush.to_builtin();
        // Direction/pressure are ALREADY resolved into `normal`/`strength` by the producer, so the
        // recipe uses neutral Surface/None — the plan packs the resolved values verbatim (bit-identity).
        // Falloff preset Smooth (0) + Blender hardness — matches `apply_brush_legacy` /
        // `falloff_weight` so stamp-plan parity stays bit-identical. App producer passes
        // the live tool preset via `from_builtin_ex` / fold instead.
        let recipe = brush::BrushRecipe::from_builtin_ex(
            builtin, radius, strength, hardness, 0, Direction::SurfaceNormal,
            0, false, false, false, [false; 3], 0.25, 0.0, true,
        );
        let hit = brush::SurfaceHit { center, normal, radius, strength };
        // Parity with apply_brush_legacy:
        // - Clay: plane_offset 0.10 (legacy hardcoded proud plane)
        // - Flatten/Fill/Scrape: plane through contact (0)
        // - pinch width/amount identity; flatten amt 0.5
        // App producer passes live tool knobs instead of these parity defaults.
        let mods = brush::StrokeMods {
            invert,
            drag_delta,
            plane_offset: if matches!(brush, VoxelBrush::Clay) { 0.10 } else { 0.0 },
            plane_bias: 0.0,
            flatten_amount: 0.5,
            pinch_intensity: 1.0,
            pinch_width: 1.0,
            pinch_amount: 1.0,
            ..brush::StrokeMods::default()
        };
        let plan = brush::fold_recipe(&recipe, &hit, &mods);
        self.apply_stamp_plan(&plan, rec);
    }

    /// LEGACY reference apply (the byte-for-byte source of truth the golden parity test compares
    /// the `StampPlan` path against). This is the pre-Phase-0 `apply_brush` body, UNCHANGED. Kept
    /// during the migration window (deleted in Phase 4 when the enum retires). Never call this on
    /// the live path — `apply_brush` routes through `apply_stamp_plan`.
    pub fn apply_brush_legacy(&mut self, brush: VoxelBrush, center: Vec3, normal: Vec3, radius: f32, strength: f32, invert: bool, hardness: f32, drag_delta: Vec3, mut rec: Option<&mut StrokeRecorder>) {
        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return };
        let inv_r = 1.0 / radius.max(1e-6);
        // Smooth relaxes toward the pre-stamp neighbourhood; Pinch + Move gather from it (semi-Lagrangian,
        // order-independent). All need the pre-stamp snapshot. (Inflate reads its gradient from the
        // LIVE grid instead — it tolerates the mild order dependence, and skipping this O(box)
        // snapshot pass + alloc roughly halves its cost: the snapshot was the big-radius freeze.)
        let snapshot: Option<Vec<f32>> = if matches!(brush, VoxelBrush::Smooth | VoxelBrush::Pinch | VoxelBrush::Inflate | VoxelBrush::Move | VoxelBrush::Smudge) {
            let cap = ((mx - cx + 1) as usize) * ((my - cy + 1) as usize) * ((mz - cz + 1) as usize);
            let mut s = Vec::with_capacity(cap);
            for z in cz..=mz { for y in cy..=my { for x in cx..=mx { s.push(self.density.get_idx(self.idx(x, y, z))); } } }
            Some(s)
        } else { None };
        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;

        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let p = self.cell_world_pos(x, y, z);
                    let dist = (p - center).length();
                    let t = dist * inv_r;
                    if t >= 1.0 { continue; }
                    // Radial brush weight (legacy path = Blend hardness curve for parity tests).
                    let fall = falloff_weight(1.0 - t, hardness);
                    let w = fall * strength;
                    // Skip negligible-effect voxels. Move/Grab displaces by the PURE falloff `fall`
                    // (independent of strength — see the Move arm), so it gates on `fall`, not `w`;
                    // otherwise a low-strength grab would skip most of its footprint and undershoot.
                    let effect = if matches!(brush, VoxelBrush::Move | VoxelBrush::Smudge) { fall } else { w };
                    if effect < 1e-5 { continue; }
                    let i = self.idx(x, y, z);
                    let cur = self.density.get_idx(i);
                    // `invert` (Ctrl) flips the brush's direction generally, so it works
                    // for every brush (Draw↔subtract, Flatten→inverse). Smooth has no
                    // distinct inverse so it ignores it.
                    let sgn = if invert { -1.0 } else { 1.0 };
                    // 6-neighbour pre-stamp values [+x,-x,+y,-y,+z,-z] (clamped at AABB edge), for
                    // Smooth's Laplacian average. Skipped for every other brush.
                    let neigh: Option<[f32; 6]> = if matches!(brush, VoxelBrush::Smooth) {
                        snapshot.as_ref().map(|s| {
                            let lx = (x - cx) as usize; let ly = (y - cy) as usize; let lz = (z - cz) as usize;
                            let sz = (mz - cz + 1) as usize;
                            [ sub_at(s, span_x, span_y, sz, lx, ly, lz, 1, 0, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, -1, 0, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 1, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, -1, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, 1),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, -1) ]
                        })
                    } else { None };
                    let new = match brush {
                        // Draw: soft dual approach solid/empty (Add and Remove are duals).
                        // Legacy matches Accu-ON stronger approach (stamp path Accu-off uses t=wk only).
                        VoxelBrush::Draw => {
                            let target = if sgn >= 0.0 { 1.0 } else { 0.0 };
                            let t = (w * 1.35).clamp(0.0, 1.0);
                            cur + (target - cur) * t
                        },
                        VoxelBrush::Erase => cur - sgn * w,
                        VoxelBrush::Smooth => {
                            // Relax toward the 6-neighbour average (Laplacian).
                            // sgn (Ctrl / Remove) reverses → push away from average.
                            let n = neigh.unwrap();
                            let avg = (n[0]+n[1]+n[2]+n[3]+n[4]+n[5]) / 6.0;
                            cur + sgn * (avg - cur) * w
                        }
                        VoxelBrush::Flatten => {
                            let plane = (p - center).dot(normal); // >0 above plane
                            // sgn (Ctrl) flips the push direction → real "inverse flatten",
                            // keeping the invert invariant true for every brush.
                            cur - sgn * plane.signum() * w * 0.5
                        }
                        VoxelBrush::Clay => {
                            // Even buildup: deposit toward a plane set slightly PROUD of the
                            // contact point (offset outward along the normal). Voxels below the
                            // offset plane get filled; voxels above get a light shave — so cavities
                            // catch up to ridges and the surface thickens evenly (not lumpy like
                            // Draw). sgn (Ctrl) flips it to subtractive clay.
                            let off = radius * 0.10;
                            let plane = (p - center).dot(normal) - off; // <=0 → below offset plane
                            let dep = if plane <= 0.0 { w } else { -w * 0.5 };
                            cur + sgn * dep
                        }
                        VoxelBrush::Scrape => {
                            // One-directional Flatten — only remove material on the cut side of the
                            // contact plane (planar hard-surface trim). sgn (Ctrl) flips which side
                            // is cut, so it stays a pure subtract on whichever half the user wants.
                            let side = (p - center).dot(normal) * sgn; // >0 = cut side
                            if side > 0.0 { cur - w } else { cur }
                        }
                        VoxelBrush::Crease => {
                            // Sharp, narrow groove: CUBE the radial falloff so the cut concentrates
                            // on the centerline with high-contrast walls (Dam_Standard). `fall` is the
                            // user-chosen radial weight (same curve as `w`); cubing it keeps the groove
                            // profile relative to the active falloff. sgn (Ctrl) → raised ridge.
                            cur - sgn * (fall * fall * fall) * strength
                        }
                        VoxelBrush::Fill => {
                            // Inverse Scrape — only DEPOSIT on the low side of the contact plane
                            // (fills pits / dents / AO traps, never builds above the surface).
                            // sgn (Ctrl) flips which side is filled.
                            let side = (p - center).dot(normal) * sgn; // <0 = fill side (below plane)
                            if side < 0.0 { cur + w } else { cur }
                        }
                        VoxelBrush::ClayBuildup => {
                            // ZBrush Clay Buildup: VOLUME BY LAYER along surface normal.
                            // Plane sits proud of the contact along `normal` by layer thickness;
                            // density packs up to that plane (flat strip top) then stops.
                            // Layer height scales with strength so each stroke is a readable slab.
                            // Must match apply_stamp_plan converge arm (parity): rate = 1-(1-t)^2
                            // with t = fall·strength (legacy rate_mul=2). Old `(t*2).clamp(0,1)`
                            // saturated at the core for any t≥0.5 so Build Rate felt clamp-dead.
                            let layer = radius * (0.14 + 0.22 * strength.clamp(0.0, 1.0)); // ~0.14–0.36·r
                            let off = layer;
                            let vs = self.voxel_size();
                            let band = vs * 1.5; // flat-top iso band
                            let plane = (p - center).dot(normal) - off;
                            let target = (0.5 - plane / band).clamp(0.0, 1.0);
                            let d0 = target - cur;
                            let d = if sgn >= 0.0 { d0.max(0.0) } else { d0.min(0.0) };
                            let t = (fall * strength).clamp(0.0, 1.0);
                            let rate = 1.0 - (1.0 - t).powf(2.0);
                            // Clay Strips slab normal-band (BLENDER_BRUSH_SPEC §3a) — mirror of the
                            // stamp_plan converge arm (golden-parity). z = depth surface→proud-plane, with
                            // z→1 at the CONTACT surface (cursor) and z→0 at the proud plane. Blender's
                            // clay_strips (clay_strips_ref.json: broad, flat-topped deposit peaked away from
                            // the plane edge) tapers the slab toward the proud plane but stays STRONG at the
                            // contact face — a pure z·(1−z) parabola is 0 exactly at the cursor (z clamps to
                            // 1 there) and deposited nothing where you sculpt (regression bc9ff30). Use a
                            // smoothstep plateau: ~1 across z∈[0.6,1] (the working depth at/under the cursor),
                            // softly tapering to 0 by the proud plane (z=0). Guard div-by-zero → passthrough.
                            let z_fac = if off > 1e-6 {
                                let z = ((off - plane) / off).clamp(0.0, 1.0);
                                let s = (z / 0.6).clamp(0.0, 1.0);
                                s * s * (3.0 - 2.0 * s)
                            } else { 1.0 };
                            cur + d * rate * z_fac
                        }
                        VoxelBrush::Inflate => {
                            // Puff volume: add density in proportion to the local gradient magnitude,
                            // which peaks AT the surface — so the shell thickens and the iso surface
                            // moves outward along its normal, while solid interior and empty exterior
                            // (flat field, ~0 gradient) stay put. sgn (Ctrl) → deflate. The gradient is a
                            // central difference read from the PRE-STAMP SNAPSHOT (via sub_at, AABB-edge
                            // clamped) — NOT the live grid. This matches the GPU brush (voxel_brush.wgsl),
                            // which reads its gradient from the frozen snap_buf, so with >1 stamp/frame
                            // every stamp sees the SAME frozen field on both paths → CPU and GPU/atlas
                            // Inflate stay bit-for-bit aligned (op-parity). The snapshot pass is the same
                            // O(box) cost Smooth/Pinch already pay, covered by the brush-cost subsample.
                            let s = snapshot.as_ref().unwrap();
                            let lx = (x - cx) as usize; let ly = (y - cy) as usize; let lz = (z - cz) as usize;
                            let sz = (mz - cz + 1) as usize;
                            let gx = sub_at(s, span_x, span_y, sz, lx, ly, lz, 1, 0, 0) - sub_at(s, span_x, span_y, sz, lx, ly, lz, -1, 0, 0);
                            let gy = sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 1, 0) - sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, -1, 0);
                            let gz = sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, 1) - sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, -1);
                            let gmag = (gx*gx + gy*gy + gz*gz).sqrt().min(1.0);
                            cur + sgn * w * gmag
                        }
                        VoxelBrush::Pinch => {
                            // Sharpen by pulling material tangentially toward the dab axis (the line
                            // through `center` along `normal`): each voxel ADOPTS the density from
                            // slightly farther out in the tangent plane, so the solid region contracts
                            // inward → crisp edges/creases. Semi-Lagrangian gather from the pre-stamp
                            // snapshot keeps it deterministic (order-independent). sgn (Ctrl) reverses
                            // the reach → push apart (bulge). Legacy path: reach scale = 1.0 (parity).
                            let s = snapshot.as_ref().unwrap();
                            let rel = p - center;
                            let tang = rel - normal * rel.dot(normal); // tangential component
                            let tlen = tang.length();
                            if tlen < 1e-6 { cur } else {
                                // Legacy: reach=1, width=1 → fall_w=fall.
                                let off = fall * radius * 0.65 * sgn;
                                let gp = p + (tang / tlen) * off;
                                let g = self.world_to_grid(gp);
                                let sz = (mz - cz + 1) as usize;
                                let sampled = sample_sub(s, span_x, span_y, sz,
                                    g.x - cx as f32, g.y - cy as f32, g.z - cz as f32).unwrap_or(cur);
                                let blend = (fall * strength).clamp(0.0, 1.0);
                                cur + (sampled - cur) * blend
                            }
                        }
                        VoxelBrush::Move | VoxelBrush::Smudge => {
                            // GRAB (Blender Grab / ZBrush Move / 3D-Coat): translate the grabbed region
                            // COHERENTLY toward the cursor. Each voxel ADOPTS the pre-stamp density from
                            // `p - drag_delta * fall` — i.e. it pulls in the material that sat `drag_delta·fall`
                            // upstream, so the whole region SHIFTS by ~`drag_delta` at the centre and tapers
                            // smoothly to 0 at the rim (a coherent grab, not a smear). The displacement weight
                            // is the PURE radial falloff `fall` — NOT `w = fall·strength` — so the grabbed
                            // surface follows the pointer by the FULL drag at the centre regardless of the
                            // strength slider (strength governs deposit brushes, not how far a grab carries;
                            // matches every other DCC). Semi-Lagrangian GATHER from the frozen snapshot keeps
                            // it order-independent + deterministic (every voxel reads the same pre-stamp
                            // field). `invert` (sgn) reverses the pull. NO trailing relax (it is excluded
                            // below): a relax pass would diffuse the moved material — that was the smear. A
                            // clean trilinear resample of full density values needs no redistancing.
                            let s = snapshot.as_ref().unwrap();
                            let src = p - drag_delta * (fall * sgn); // upstream sample point (voxel-local space)
                            let g = self.world_to_grid(src);
                            let sz = (mz - cz + 1) as usize;
                            // Trilinear gather from the box-local snapshot; out-of-box falls back to `cur`
                            // (clamped behaviour at the stamp edge, same convention as Pinch).
                            sample_sub(s, span_x, span_y, sz,
                                g.x - cx as f32, g.y - cy as f32, g.z - cz as f32).unwrap_or(cur)
                        }
                        // Snake Hook is SURFACE-mode only (vertex tube). It has NO density stamp and is
                        // greyed in Voxel mode, so it never reaches this legacy density path — fail
                        // closed to a no-op (leave the field unchanged) rather than fake a stamp.
                        VoxelBrush::SnakeHook => cur,
                    };
                    // Additive buildup control — golden-ref mirror of the `apply_stamp_plan`
                    // deposit cap (see there for the 0.5·strength derivation). Pure deposit
                    // brushes only (Add/Subtract/PlaneFill families).
                    let new = if matches!(brush, VoxelBrush::Draw | VoxelBrush::Erase
                        | VoxelBrush::Crease | VoxelBrush::Clay | VoxelBrush::Fill
                        | VoxelBrush::ClayBuildup) {
                        let cap = strength.clamp(0.0, 1.0) * 0.5;
                        cur + (new - cur).clamp(-cap, cap)
                    } else { new };
                    // Capture the pre-stroke value for undo BEFORE writing — only for voxels the
                    // brush actually modifies (we already skipped t>=1 / w<1e-5 above), so the undo
                    // delta is the true changed set, not the whole bounding box.
                    if let Some(r) = rec.as_deref_mut() { r.record(i as u32, cur); }
                    self.density.set_idx(i, new.clamp(0.0, 1.0));
                }
            }
        }
        // Pinch moves samples toward the dab axis and can leave faint resampling steps — one mild relax
        // pass over the stamp region restores a smooth field before the next stamp gathers from it (the
        // density-grid equivalent of the spec's §3 redistancing). The relax records its own pre-values
        // to `rec`, so it stays part of the stroke's single undo step. Move/Grab is EXCLUDED: it is a
        // coherent translation of full density values, so it needs no redistancing, and a relax would
        // diffuse the grabbed material (that was the smear — the whole point of the grab is to move the
        // surface cleanly, not blur it).
        if matches!(brush, VoxelBrush::Pinch) {
            self.relax_region([cx, cy, cz, mx, my, mz], 1, 0.12, true, rec.as_deref_mut());
        }
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
    }

    /// Consume a flat `StampPlan` POD. Single-op plans stay bit-identical to legacy; multi-op
    /// (`op_count` 2..4) runs ops sequentially per voxel (same as GPU `voxel_brush.wgsl`) so stack
    /// mods like Flatten+Gather compose. Production path; `apply_brush_legacy` is the golden ref.
    pub fn apply_stamp_plan(&mut self, plan: &crate::brush::StampPlan, rec: Option<&mut StrokeRecorder>) {
        self.apply_stamp_plan_with_tip(plan, None, rec);
    }

    /// Like [`apply_stamp_plan`], but multiplies radial falloff by an optional greyscale brush-tip
    /// alpha (sampled in the stamp plane). Tips force the CPU path when loaded (no GPU tip texture yet).
    pub fn apply_stamp_plan_with_tip(
        &mut self,
        plan: &crate::brush::StampPlan,
        tip: Option<&BrushTipAlpha>,
        mut rec: Option<&mut StrokeRecorder>,
    ) {
        use crate::brush::{eval, AdvectSrc, Operator, PlaneSide, MAX_OPS};
        let center = Vec3::new(plan.center[0], plan.center[1], plan.center[2]);
        let radius = plan.center[3];
        let normal = Vec3::new(plan.plane_n[0], plan.plane_n[1], plan.plane_n[2]);
        let strength = plan.plane_n[3];
        let drag_delta = Vec3::new(plan.drag[0], plan.drag[1], plan.drag[2]);
        let sgn = plan.sgn; // +1.0 normally, -1.0 when inverted (Ctrl)
        let hardness = plan.kernel_hardness;
        let (tip_t0, tip_b0) = brush_tip_basis(normal);
        // Tip rotation from plan._pad1 (f32 bits) — brush dynamics Random Rotation.
        let tip_rot = f32::from_bits(plan._pad1);
        let (tip_t, tip_b) = if tip_rot.abs() > 1e-6 {
            let c = tip_rot.cos();
            let s = tip_rot.sin();
            (tip_t0 * c + tip_b0 * s, -tip_t0 * s + tip_b0 * c)
        } else {
            (tip_t0, tip_b0)
        };
        let falloff_preset = FalloffPreset::from_u32(plan._pad0);
        let exp = plan.kernel_exp;

        let n_ops = (plan.op_count as usize).clamp(1, MAX_OPS);
        // Classify stack: Move only as primary; Pinch if any Tangential Advect (for trailing relax).
        let mut any_pinch = false;
        let mut any_move = false;
        let mut needs_snapshot = false;
        let mut any_relax = false;
        // Pure DEPOSIT stack (Draw/Erase/Crease/Clay/Fill/ClayBuildup — Add/Subtract/PlaneFill
        // only): eligible for the per-stamp buildup cap below. Gather/relax/plane-push ops are
        // resampling or target-seeking (self-limiting) and keep their character uncapped.
        let mut deposit_only = true;
        for oi in 0..n_ops {
            let pop = plan.op_list[oi];
            let op = eval::op_from_code(pop.op_code);
            let advect = eval::unpack_advect(pop.flags);
            let is_move = matches!(op, Operator::Advect) && matches!(advect, AdvectSrc::DragVector);
            let is_pinch = matches!(op, Operator::Advect) && !is_move;
            if is_move { any_move = true; }
            if is_pinch { any_pinch = true; }
            if matches!(op, Operator::Relax) { any_relax = true; }
            if matches!(op, Operator::Relax | Operator::Inflate) || is_pinch || is_move {
                needs_snapshot = true;
            }
            if !matches!(op, Operator::Add | Operator::Subtract | Operator::PlaneFill) {
                deposit_only = false;
            }
        }
        // Move as sole primary: strength-independent gate (legacy). Stacked non-Move uses `w`.
        let is_move = any_move && n_ops == 1;

        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return };
        let inv_r = 1.0 / radius.max(1e-6);
        let vs = self.voxel_size(); // Clay-1 converge: plane→iso transition band (one voxel)
        let snapshot: Option<Vec<f32>> = if needs_snapshot {
            let cap = ((mx - cx + 1) as usize) * ((my - cy + 1) as usize) * ((mz - cz + 1) as usize);
            let mut s = Vec::with_capacity(cap);
            for z in cz..=mz { for y in cy..=my { for x in cx..=mx { s.push(self.density.get_idx(self.idx(x, y, z))); } } }
            Some(s)
        } else { None };
        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;

        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let p = self.cell_world_pos(x, y, z);
                    let dist = (p - center).length();
                    let t = dist * inv_r;
                    if t >= 1.0 { continue; }
                    let mut fall = falloff_weight_preset(1.0 - t, hardness, falloff_preset);
                    if let Some(tip_map) = tip {
                        let rel = p - center;
                        let u = rel.dot(tip_t) * inv_r;
                        let v = rel.dot(tip_b) * inv_r;
                        // Tip contrast rides plan.drag[3] (f32; 0 → treat as 1.0 identity).
                        let tip_c = plan.drag[3];
                        let c = if tip_c > 1e-4 { tip_c } else { 1.0 };
                        fall *= tip_map.sample_contrast(u, v, c);
                    }
                    let i = self.idx(x, y, z);
                    fall *= 1.0 - self.mask_factor_at(i);
                    let w = fall * strength;
                    let effect = if is_move { fall } else { w };
                    if effect < 1e-5 { continue; }
                    let wk = if exp == 2.0 {
                        (fall * fall) * strength
                    } else if exp == 3.0 {
                        (fall * fall * fall) * strength
                    } else if exp == 1.0 {
                        w
                    } else {
                        fall.powf(exp) * strength
                    };
                    let cur0 = self.density.get_idx(i);

                    let neigh: Option<[f32; 6]> = if any_relax {
                        snapshot.as_ref().map(|s| {
                            let lx = (x - cx) as usize; let ly = (y - cy) as usize; let lz = (z - cz) as usize;
                            let sz = (mz - cz + 1) as usize;
                            [ sub_at(s, span_x, span_y, sz, lx, ly, lz, 1, 0, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, -1, 0, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 1, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, -1, 0),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, 1),
                              sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, -1) ]
                        })
                    } else { None };

                    // Sequential op stack (GPU parity): each op feeds the next.
                    let mut d = cur0;
                    for oi in 0..n_ops {
                        let pop = plan.op_list[oi];
                        let op = eval::op_from_code(pop.op_code);
                        let side = eval::unpack_side(pop.flags);
                        let advect = eval::unpack_advect(pop.flags);
                        let shave = eval::unpack_shave(pop.flags);
                        let sgn_on_side = eval::unpack_sgn_on_side(pop.flags);
                        let converge = eval::unpack_converge(pop.flags);
                        let plane_offset = pop.plane_offset;
                        let op_amount = pop.amount.max(0.0);
                        // 0 packed → treat as 1.0 (legacy / off). Do not floor at 0.05 (that forced a live cap).
                        let height_limit_ui = {
                            let h = crate::brush::plan::flags::unpack_height_limit(pop.flags);
                            if h < 1e-4 { 1.0 } else { h.clamp(0.0, 1.0) }
                        };
                        let density_cap = crate::brush::plan::flags::density_cap_from_height_limit(
                            height_limit_ui, self.iso,
                        );
                        let vol_pres = crate::brush::plan::flags::unpack_vol_pres(pop.flags).clamp(0.0, 1.0);
                        let use_max = matches!(eval::unpack_blend(pop.flags), crate::brush::OpBlend::Max);
                        let op_is_move = matches!(op, Operator::Advect) && matches!(advect, AdvectSrc::DragVector);
                        let cur = d;
                        // Cap only limits growth — never min() solid down to the ceiling (that killed clay).
                        let apply_cap = |v: f32| -> f32 {
                            crate::brush::plan::flags::apply_density_cap(cur, v, density_cap)
                        };
                        d = match op {
                            // Draw: ALWAYS soft dual approach (Add→solid, Remove→empty) so polarity
                            // feels the same — pure `cur±wk` made Add balloon while Remove dug cleanly.
                            // Accu OFF: rate = wk. Accu ON: stronger rate (1.35×) still dual, no pile.
                            Operator::Add => {
                                let target = if sgn >= 0.0 { 1.0 } else { 0.0 };
                                let rate = if use_max { 1.0 } else { 1.35 };
                                let t = (wk * rate).clamp(0.0, 1.0);
                                cur + (target - cur) * t
                            }
                            Operator::Subtract => {
                                // Crease Depth (amount) + Crease Width (plane_offset reshape falloff).
                                // Erase: defaults width=1 depth=1 → legacy wk.
                                let depth = if op_amount > 1e-6 { op_amount } else { 1.0 };
                                let width = if plane_offset > 1e-6 { plane_offset.clamp(0.1, 4.0) } else { 1.0 };
                                let fall_c = if (width - 1.0).abs() < 1e-4 {
                                    fall
                                } else {
                                    fall.powf(1.0 / width)
                                };
                                let wk_c = if exp == 3.0 {
                                    fall_c * fall_c * fall_c * strength * depth
                                } else if exp == 2.0 {
                                    fall_c * fall_c * strength * depth
                                } else if exp == 1.0 {
                                    fall_c * strength * depth
                                } else {
                                    fall_c.powf(exp) * strength * depth
                                };
                                cur - sgn * wk_c
                            }
                            Operator::Relax => {
                                let n = neigh.unwrap_or([cur; 6]);
                                let avg = (n[0]+n[1]+n[2]+n[3]+n[4]+n[5]) / 6.0;
                                // sgn (Ctrl / Remove) reverses Smooth → push away from average (sharpen-ish).
                                cur + sgn * (avg - cur) * w
                            }
                            Operator::PlanePush => {
                                // Flatten Amount (op_amount) scales push; Plane Offset+Bias in plane_offset×r.
                                // Volume Preservation softens deep cores. Height Clamp caps density.
                                // Push weight uses fall * flat_amt * strength so Flatten Amount is not a
                                // weak twin of Brush Strength alone (both still matter via product).
                                let flat_amt = if op_amount > 1e-6 { op_amount } else { 0.5 };
                                let plane = (p - center).dot(normal) - radius * plane_offset;
                                let solid = (cur - 0.5).abs() * 2.0; // 0 at iso, 1 at 0/1
                                let vp = 1.0 - vol_pres * solid.clamp(0.0, 1.0);
                                // fall * flat_amt * strength: flat_amt independent scale from strength
                                let push = fall * flat_amt * strength * vp;
                                let mut nxt = cur - sgn * plane.signum() * push;
                                apply_cap(nxt)
                            }
                            Operator::PlaneFill => {
                                let off = radius * plane_offset;
                                // Height Limit: density ceiling above iso (UI 1 = off).
                                if sgn_on_side {
                                    // Fill: side test respects Plane Offset (was ignored → slider dead on Fill).
                                    let s = ((p - center).dot(normal) - off) * sgn;
                                    if s < 0.0 {
                                        let nxt = cur + wk;
                                        apply_cap(if use_max { cur.max(nxt) } else { nxt })
                                    } else { cur }
                                } else if converge {
                                    // Build Rate = op_amount (legacy 2.0); Height Limit caps target.
                                    // Rate uses 1-(1-t)^rate so values >1 still matter at the core
                                    // (old ` (wk*rate).clamp(0,1) ` saturated to 1 for any rate≥1/wk).
                                    let band = vs * 1.5;
                                    let plane = (p - center).dot(normal) - off;
                                    let mut target = (0.5 - plane / band).clamp(0.0, 1.0);
                                    // Build target may sit under the ceiling; never force target below iso.
                                    if let Some(c) = density_cap {
                                        target = target.min(c).max(self.iso + 1e-4);
                                    }
                                    let d0 = target - cur;
                                    let dd = if sgn >= 0.0 { d0.max(0.0) } else { d0.min(0.0) };
                                    let rate_mul = if op_amount > 1e-6 { op_amount } else { 2.0 };
                                    let t = wk.clamp(0.0, 1.0);
                                    let rate = if rate_mul <= 1e-6 {
                                        0.0
                                    } else {
                                        1.0 - (1.0 - t).powf(rate_mul.clamp(0.15, 8.0))
                                    };
                                    // Clay Strips slab normal-band (BLENDER_BRUSH_SPEC §3a): weight the
                                    // deposit along the normal, z = depth surface→plane ∈[0,1] with z→1 at the
                                    // CONTACT surface (cursor) and z→0 at the proud plane. Blender's clay_strips
                                    // (clay_strips_ref.json: broad flat-topped deposit) tapers the slab toward
                                    // the proud plane yet deposits STRONGLY at the contact face. A pure z·(1−z)
                                    // parabola is 0 exactly at the cursor (z clamps to 1) → built nothing where
                                    // you sculpt (regression bc9ff30). smoothstep plateau: ~1 across z∈[0.6,1]
                                    // (working depth under the cursor), soft taper to 0 by the proud plane.
                                    // off>0 always for ClayBuildup (converge implies a proud plane); guard
                                    // div-by-zero → passthrough for a degenerate zero offset.
                                    let z_fac = if off > 1e-6 {
                                        let z = ((off - plane) / off).clamp(0.0, 1.0);
                                        let s = (z / 0.6).clamp(0.0, 1.0);
                                        s * s * (3.0 - 2.0 * s)
                                    } else { 1.0 };
                                    let nxt = cur + dd * rate * z_fac;
                                    let out = if use_max { cur.max(nxt) } else { nxt };
                                    apply_cap(out)
                                } else {
                                    // Clay: plane offset + optional shave; Height Limit caps growth only.
                                    let plane = (p - center).dot(normal) - off;
                                    if plane <= 0.0 {
                                        let nxt = cur + sgn * wk;
                                        apply_cap(if use_max { cur.max(nxt) } else { nxt })
                                    } else if shave {
                                        cur + sgn * (-wk * 0.5)
                                    } else {
                                        cur
                                    }
                                }
                            }
                            Operator::PlaneClip => {
                                // Trim: Plane Angle shifts cut threshold; Cut Depth multiplies subtract;
                                // Fill Holes (converge bit) deposits on keep side.
                                let cut_depth = if op_amount > 1e-6 { op_amount } else { 1.0 };
                                let angle = plane_offset; // already ×radius fraction + bias
                                let s = (p - center).dot(normal) * sgn - radius * angle;
                                let cut = match side {
                                    PlaneSide::Above => s > 0.0,
                                    PlaneSide::Below => s < 0.0,
                                    PlaneSide::Both => true,
                                };
                                let fill_holes = converge; // repurposed CONVERGE bit for Trim
                                if cut {
                                    cur - w * cut_depth
                                } else if fill_holes {
                                    cur + w * cut_depth * 0.35
                                } else {
                                    cur
                                }
                            }
                            Operator::Inflate => {
                                // Inflate Speed (amount) scales delta independently of Brush Strength
                                // (both matter: fall * strength * speed). Direction + Thickness Clamp + Vol Pres.
                                let s = snapshot.as_ref().unwrap();
                                let lx = (x - cx) as usize; let ly = (y - cy) as usize; let lz = (z - cz) as usize;
                                let sz = (mz - cz + 1) as usize;
                                let gx = sub_at(s, span_x, span_y, sz, lx, ly, lz, 1, 0, 0) - sub_at(s, span_x, span_y, sz, lx, ly, lz, -1, 0, 0);
                                let gy = sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 1, 0) - sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, -1, 0);
                                let gz = sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, 1) - sub_at(s, span_x, span_y, sz, lx, ly, lz, 0, 0, -1);
                                let gmag = (gx*gx + gy*gy + gz*gz).sqrt().min(1.0);
                                let speed = if op_amount > 1e-6 { op_amount } else { 1.0 };
                                let dir_mode = crate::brush::plan::flags::unpack_inflate_dir(pop.flags);
                                let factor = match dir_mode {
                                    1 => {
                                        let glen = (gx*gx + gy*gy + gz*gz).sqrt().max(1e-6);
                                        let align = (gx * normal.x + gy * normal.y + gz * normal.z).abs() / glen;
                                        gmag * (0.35 + 0.65 * align)
                                    }
                                    2 => gmag * (0.25 + 0.75 * fall),
                                    _ => gmag, // Gradient
                                };
                                let solid = (cur - 0.5).abs() * 2.0;
                                let vp = 1.0 - vol_pres * solid.clamp(0.0, 1.0);
                                // Same structure as Flatten: fall * strength * speed (not w*speed only)
                                let mut nxt = cur + sgn * fall * strength * speed * factor * vp;
                                let tclamp = if plane_offset > 1e-6 { plane_offset.clamp(0.05, 1.0) } else { 1.0 };
                                if tclamp < 0.999 {
                                    let lo = (0.5 - 0.5 * tclamp).max(0.0);
                                    let hi = (0.5 + 0.5 * tclamp).min(1.0);
                                    nxt = nxt.clamp(lo, hi);
                                }
                                nxt
                            }
                            Operator::Advect => {
                                let s = snapshot.as_ref().unwrap();
                                if op_is_move {
                                    let src = p - drag_delta * (fall * sgn);
                                    let g = self.world_to_grid(src);
                                    let sz = (mz - cz + 1) as usize;
                                    sample_sub(s, span_x, span_y, sz,
                                        g.x - cx as f32, g.y - cy as f32, g.z - cz as f32).unwrap_or(cur)
                                } else {
                                    // Pinch Strength (op_amount) = REACH — how far along tangent we sample.
                                    // Pinch Width (plane_offset) = FOOTPRINT shape — reshapes radial falloff
                                    //   (<1 tighter core, >1 broader shoulders). NOT a second reach mult
                                    //   (that made Width feel identical/dead vs Strength).
                                    // Pinch Amount (height_limit×2) + Brush Strength = blend weight.
                                    // STROKE-AXIS PINCH (BLENDER_BRUSH_SPEC §2): pull toward the stroke
                                    // AXIS LINE (through `center` along the stroke tangent), not the dab
                                    // point. Basis stroke_xz = [tangent, normal]; the gather direction is
                                    // the BITANGENT (in-plane, ⊥ to the tangent) so material along the
                                    // stroke line stays put and only the cross-stroke component is squeezed
                                    // → a crease that runs ALONG the stroke. `drag_delta` carries the unit
                                    // stroke tangent (app side); ZERO (first frame / no motion) falls back
                                    // to the axisymmetric radial gather so a single dab still pinches.
                                    // Shared stroke-axis basis (BLENDER_BRUSH_SPEC §2) — ONE impl in
                                    // `pinch_axis_pull`, also used by the Surface-mode Pinch/Crease dab.
                                    // `drag_delta` carries the unit stroke tangent here.
                                    let pull_vec = crate::voxel_sculpt::pinch_axis_pull(p - center, normal, drag_delta);
                                    let tlen = pull_vec.length();
                                    if tlen < 1e-6 { cur } else {
                                        let tang = pull_vec;
                                        let width = if plane_offset > 1e-6 { plane_offset.clamp(0.15, 4.0) } else { 1.0 };
                                        let pamt = {
                                            let h = crate::brush::plan::flags::unpack_height_limit(pop.flags);
                                            if h < 1e-4 { 1.0 } else { (h * 2.0).clamp(0.0, 2.0) }
                                        };
                                        let reach = if op_amount > 1e-6 { op_amount } else { 1.0 };
                                        // Width reshapes fall: pow(fall, 1/width) — narrower peak when width < 1.
                                        let fall_w = if (width - 1.0).abs() < 1e-3 {
                                            fall
                                        } else {
                                            fall.powf((1.0 / width).clamp(0.2, 5.0))
                                        };
                                        // Reach scales sample distance (strength-free).
                                        let off = fall_w * radius * 0.65 * reach * sgn;
                                        let gp = p + (tang / tlen) * off;
                                        let g = self.world_to_grid(gp);
                                        let sz = (mz - cz + 1) as usize;
                                        let sampled = sample_sub(s, span_x, span_y, sz,
                                            g.x - cx as f32, g.y - cy as f32, g.z - cz as f32).unwrap_or(cur);
                                        let blend = (fall_w * strength * pamt).clamp(0.0, 1.0);
                                        cur + (sampled - cur) * blend
                                    }
                                }
                            }
                        };
                    }
                    // Additive buildup control (Blender/ZBrush layering feel): pure deposit stacks
                    // cap the per-stamp density change so repeated strokes build in controlled
                    // increments instead of saturating to clamp(0,1) within a pass. The iso
                    // transition band is ~1.5 voxels wide (the ClayBuildup `band = vs * 1.5`
                    // target ramp), so a full 0→1 density swing ≈ 1.5 voxels of surface motion;
                    // 0.5·strength therefore limits one stamp to ≤ ~0.75 voxel of iso motion at
                    // strength 1 (≈ 0.4 voxel at the default 0.5) — visible per pass, never a
                    // one-stamp slam to solid. MUST stay bit-identical to `voxel_brush.wgsl`.
                    if deposit_only {
                        let cap = strength.clamp(0.0, 1.0) * 0.5;
                        d = cur0 + (d - cur0).clamp(-cap, cap);
                    }
                    if let Some(r) = rec.as_deref_mut() { r.record(i as u32, cur0); }
                    self.density.set_idx(i, d.clamp(0.0, 1.0));
                }
            }
        }
        // Pinch trailing relax (any stack gather) — Move excluded.
        // Mild only: 0.3 was wiping the pinch so Strength/reach changes looked like no-ops.
        if any_pinch && !is_move {
            self.relax_region([cx, cy, cz, mx, my, mz], 1, 0.12, true, rec.as_deref_mut());
        }
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
    }

    /// PAINT CHOKEPOINT — the ONLY sanctioned way paint code readies the `colors` buffer for
    /// writing. Every `self.colors[..] = …` write site MUST be preceded (in the same function)
    /// by a `true` return from this helper; direct lazy-alloc (`if colors.len() != n { … }`)
    /// at call sites is forbidden — that pattern let write paths silently trust the
    /// `colors.len() == res³` invariant while only reads were checked.
    ///
    /// Structural invariant enforced here (`colors_in_sync()` afterwards is guaranteed `true`
    /// when this returns `true`):
    /// - **Sparse density** → returns `false` (vertex-paint on the ≥256³ / atlas path is
    ///   deferred by design — callers no-op). Any stale dense buffer left behind by an in-place
    ///   dense→sparse density swap is dropped here so it can never be misread later.
    /// - **Dense density** → (re)allocates `colors` to exactly `density.len()` (`res³`).
    ///   `colors` non-empty but the wrong length means a resample / density-replace path left a
    ///   stale buffer whose flat layout no longer matches the grid — the stale paint is
    ///   meaningless under the new layout, so it is dropped (zeroed), never partially reused
    ///   (partial reuse = silent mis-colour). Resize-on-first-paint is the intended lazy-alloc
    ///   semantics (an unpainted sculpt pays nothing).
    fn ensure_paint_colors(&mut self) -> bool {
        if self.density.is_sparse() {
            if !self.colors.is_empty() {
                // Stale dense paint carried across a dense→sparse conversion: layout is dead.
                self.colors = Vec::new();
            }
            return false;
        }
        let n = self.density.len();
        debug_assert_eq!(
            n,
            (self.res as usize).pow(3),
            "dense DensityStorage len diverged from res³ — grid construction bug"
        );
        if self.colors.len() != n {
            // Empty = first paint (normal lazy alloc). Non-empty wrong-length = stale buffer
            // from a density swap/resample — drop it, a mismatched flat layout would mis-colour.
            self.colors = vec![0u32; n];
        }
        true
    }

    /// True when the paint `colors` buffer is a valid dense parallel of the density buffer —
    /// the ONE predicate read-modify paths (undo restore, symmetrize) use before touching
    /// `colors` without allocating. Sparse grids never carry paint → always `false` there.
    #[inline]
    pub fn colors_in_sync(&self) -> bool {
        !self.density.is_sparse() && self.colors.len() == self.density.len()
    }

    /// PAINT: stamp a per-voxel `color` over the brush sphere (Blender-style sculpt-vertex-paint). The
    /// caller invokes this ALONGSIDE `apply_brush` when the Paint-Color toggle is on, so any brush
    /// (Draw / Clay / Smooth / …) also deposits colour on the voxels it touches. Single responsibility:
    /// it only writes the parallel `colors` buffer (never density). DENSE-ONLY — a no-op on a sparse
    /// grid (vertex-paint on the ≥256³ / atlas path is deferred). Lazily allocates `colors` on first
    /// use (`res³` words) so an unpainted sculpt never pays the memory. Marks the same region dirty so
    /// the live CPU re-mesh picks up the new colour this frame. `color` is the packed `0x_FF_RR_GG_BB`
    /// word (see `pack_voxel_color`); the sphere falloff matches the density brush footprint.
    pub fn deposit_color_sphere(&mut self, center: Vec3, radius: f32, color: u32) {
        if !self.ensure_paint_colors() { return; } // sparse → paint deferred (see chokepoint doc)
        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return };
        let inv_r = 1.0 / radius.max(1e-6);
        for z in cz..=mz { for y in cy..=my { for x in cx..=mx {
            let p = self.cell_world_pos(x, y, z);
            if (p - center).length() * inv_r >= 1.0 { continue; } // outside the sphere
            let ci = self.idx(x, y, z);
            self.colors[ci] = color; // flag byte (0xFF) marks it painted
        }}}
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
    }

    /// How masked a voxel is (0 = free, 1 = fully protected). Empty mask → 0.
    #[inline]
    pub fn mask_factor_at(&self, idx: usize) -> f32 {
        if self.density.is_sparse() {
            return self.mask_sparse.get(&(idx as u32)).copied().unwrap_or(0) as f32 / 255.0;
        }
        if self.mask.is_empty() || idx >= self.mask.len() { return 0.0; }
        self.mask[idx] as f32 / 255.0
    }

    /// True when any mask protection exists (forces box GPU / CPU brush so mask is respected).
    #[inline]
    pub fn has_mask(&self) -> bool {
        if self.density.is_sparse() {
            return !self.mask_sparse.is_empty();
        }
        !self.mask.is_empty() && self.mask.iter().any(|&m| m != 0)
    }

    /// Box-local mask factors (0..1) for a cell AABB, same layout as `density.subvolume`.
    /// Used by the GPU box brush path so protect-mask multiplies falloff on device.
    pub fn mask_subvolume(&self, aabb: [u32; 6]) -> Vec<f32> {
        let [x0, y0, z0, x1, y1, z1] = aabb;
        let dx = (x1 - x0 + 1) as usize;
        let dy = (y1 - y0 + 1) as usize;
        let dz = (z1 - z0 + 1) as usize;
        let mut out = vec![0.0f32; dx * dy * dz];
        if !self.has_mask() { return out; }
        let mut i = 0usize;
        for z in z0..=z1 {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    out[i] = self.mask_factor_at(self.idx(x, y, z));
                    i += 1;
                }
            }
        }
        out
    }

    /// Clear the entire mask (frees memory). Marks the whole grid dirty so the
    /// viewport remesh drops the mask tint.
    pub fn clear_mask(&mut self) {
        let had = self.has_mask();
        self.mask.clear();
        self.mask_sparse.clear();
        if !had { return; }
        let r = self.res.saturating_sub(1);
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Invert mask protection (255 - m). Dense: every cell in the buffer. Sparse: every stored
    /// non-zero cell (HashMap keys only — free solid cells stay free). Marks whole grid dirty.
    pub fn invert_mask(&mut self) {
        if !self.has_mask() { return; }
        if self.density.is_sparse() {
            let keys: Vec<u32> = self.mask_sparse.keys().copied().collect();
            for k in keys {
                let cur = self.mask_sparse.get(&k).copied().unwrap_or(0);
                let inv = 255u8.wrapping_sub(cur);
                if inv == 0 { self.mask_sparse.remove(&k); }
                else { self.mask_sparse.insert(k, inv); }
            }
        } else {
            for m in &mut self.mask {
                *m = 255u8.wrapping_sub(*m);
            }
        }
        let r = self.res.saturating_sub(1);
        self.mark_dirty([0, 0, 0], [r, r, r]);
    }

    /// Toggle mask viewport tint. Remeshes only when a mask is present (tint actually changes).
    pub fn set_show_mask(&mut self, on: bool) {
        if self.show_mask == on { return; }
        self.show_mask = on;
        if self.has_mask() {
            let r = self.res.saturating_sub(1);
            self.mark_dirty([0, 0, 0], [r, r, r]);
        }
    }

    /// True when the mesher should pack mask darkening into vertex colours.
    #[inline]
    pub fn mask_visible(&self) -> bool {
        self.show_mask && self.has_mask()
    }

    /// Paint mask inside the brush sphere. `value` 0 = clear protection, 255 = full protect.
    /// `hardness` uses the Smooth falloff for soft mask edges. Works on dense (Vec) and sparse
    /// (HashMap of non-zero cells) so ≥256³ can protect without a full res³ allocation.
    pub fn deposit_mask_sphere(&mut self, center: Vec3, radius: f32, value: u8, hardness: f32) {
        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return };
        let sparse = self.density.is_sparse();
        if !sparse {
            let n = (self.res as usize).pow(3);
            if self.mask.len() != n { self.mask = vec![0u8; n]; }
        }
        let inv_r = 1.0 / radius.max(1e-6);
        let target = value as f32;
        for z in cz..=mz { for y in cy..=my { for x in cx..=mx {
            let p = self.cell_world_pos(x, y, z);
            let t = (p - center).length() * inv_r;
            if t >= 1.0 { continue; }
            let w = falloff_weight_preset(1.0 - t, hardness, FalloffPreset::Smooth);
            if w < 1e-4 { continue; }
            let ci = self.idx(x, y, z);
            let cur = if sparse {
                self.mask_sparse.get(&(ci as u32)).copied().unwrap_or(0) as f32
            } else {
                self.mask[ci] as f32
            };
            // Lerp toward target by falloff weight (paint up or clear down).
            let next = (cur + (target - cur) * w).clamp(0.0, 255.0) as u8;
            if sparse {
                if next == 0 { self.mask_sparse.remove(&(ci as u32)); }
                else { self.mask_sparse.insert(ci as u32, next); }
            } else {
                self.mask[ci] = next;
            }
        }}}
        self.mark_dirty([cx, cy, cz], [mx, my, mz]);
    }

    /// Auto Mask: write protect-mask from local density curvature inside the brush sphere.
    ///
    /// Uses a 6-neighbour density Laplacian as a cheap feature proxy (same family as body field
    /// smooth). Only the iso band is considered so solid cores / empty space stay free.
    ///
    /// **Soft overwrite** (not permanent max-lock): within the brush falloff, protection lerps
    /// toward the current feature response. Re-stroking with lower intensity / different features
    /// can **release** locks; pure `max` used to make Auto Mask feel permanent after first pass.
    /// Dense + sparse (HashMap) paths mirror [`Self::deposit_mask_sphere`].
    ///
    /// Call **before** the density stamp so `mask_factor_at` gates the same dab.
    pub fn deposit_auto_mask_sphere(
        &mut self,
        center: Vec3,
        radius: f32,
        hardness: f32,
        mode: AutoMaskMode,
        intensity: f32,
        invert: bool,
    ) {
        if !mode.is_on() { return; }
        let intensity = intensity.clamp(0.0, 1.0);
        if intensity < 1e-4 { return; }
        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return };
        let sparse = self.density.is_sparse();
        if !sparse {
            let n = (self.res as usize).pow(3);
            if self.mask.len() != n { self.mask = vec![0u8; n]; }
        }
        let inv_r = 1.0 / radius.max(1e-6);
        let iso = self.iso;
        // Surface shell in density units — only feature-mask near the iso surface.
        let band = 0.38f32;
        // Laplacian scale: density ∈ [0,1] → 6-neighbour lap roughly [-6,6]; compress to ~[0,1].
        let lap_scale = 1.8f32;
        let mut any = false;
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let p = self.cell_world_pos(x, y, z);
                    let t = (p - center).length() * inv_r;
                    if t >= 1.0 { continue; }
                    let fall = falloff_weight_preset(1.0 - t, hardness, FalloffPreset::Smooth);
                    if fall < 1e-4 { continue; }
                    let d = self.density.get_idx(self.idx(x, y, z));
                    if (d - iso).abs() > band { continue; }
                    let xi = x as i32;
                    let yi = y as i32;
                    let zi = z as i32;
                    // OOB reads as 0 (empty) via at_i — correct for surface feature detection.
                    let sum_n = self.density.at_i(xi - 1, yi, zi)
                        + self.density.at_i(xi + 1, yi, zi)
                        + self.density.at_i(xi, yi - 1, zi)
                        + self.density.at_i(xi, yi + 1, zi)
                        + self.density.at_i(xi, yi, zi - 1)
                        + self.density.at_i(xi, yi, zi + 1);
                    // lap > 0 → local solid peak/ridge; lap < 0 → cavity / dent.
                    let lap = (d * 6.0 - sum_n) / 6.0;
                    let h = (lap * lap_scale).clamp(-1.0, 1.0);
                    let mut feature = match mode {
                        AutoMaskMode::Off => 0.0,
                        AutoMaskMode::Curvature => h.abs(),
                        AutoMaskMode::Peak => h.max(0.0),
                        AutoMaskMode::Cavity => (-h).max(0.0),
                        AutoMaskMode::Flat => (1.0 - h.abs()).max(0.0),
                    };
                    // Soft knee so mild features don't fully lock the surface.
                    feature = (feature * feature * (3.0 - 2.0 * feature)).clamp(0.0, 1.0);
                    if invert { feature = 1.0 - feature; }
                    let w = (fall * intensity * feature).clamp(0.0, 1.0);
                    // How hard this dab rewrites protect (0 at rim → intensity at centre).
                    // Non-feature cells (w≈0) fade existing locks so Auto Mask can be “turned down”.
                    let rewrite = (fall * intensity).clamp(0.0, 1.0);
                    if rewrite < 1e-4 { continue; }
                    let target = (w * 255.0).clamp(0.0, 255.0);
                    let ci = self.idx(x, y, z);
                    let cur = if sparse {
                        self.mask_sparse.get(&(ci as u32)).copied().unwrap_or(0) as f32
                    } else {
                        self.mask[ci] as f32
                    };
                    let next_f = cur * (1.0 - rewrite) + target * rewrite;
                    let next = next_f.clamp(0.0, 255.0) as u8;
                    if (next as f32 - cur).abs() < 0.5 { continue; }
                    any = true;
                    if sparse {
                        if next == 0 { self.mask_sparse.remove(&(ci as u32)); }
                        else { self.mask_sparse.insert(ci as u32, next); }
                    } else {
                        self.mask[ci] = next;
                    }
                }
            }
        }
        if any {
            self.mark_dirty([cx, cy, cz], [mx, my, mz]);
        }
    }

    /// FILL ALL: paint EVERY solid voxel (density ≥ `iso`) of this grid with `color`, in ONE step. The
    /// panel's "Fill All" button calls this with the current brush paint colour so the whole sculpt takes
    /// the colour at once (no stroke needed). Single responsibility: only writes the parallel `colors`
    /// buffer (never density). DENSE-ONLY — a no-op (returns an empty delta) on a sparse grid, mirroring
    /// `deposit_color_sphere`; vertex-paint on the ≥256³ / atlas path is deferred. Lazily allocates
    /// `colors` on first use (`res³` words). There is NO masking yet — "solid" means above the iso
    /// surface; a future masked-region fill would intersect this with a selection mask.
    ///
    /// Records each changed voxel's pre-fill colour word into `(idx, pre_color)` and RETURNS that delta so
    /// the caller can register ONE entry on the sculpt colour-undo path (sculpt undo is its own path — NOT
    /// `transaction()`), letting Ctrl+Z revert the whole fill in a single step. Replaying the returned
    /// delta through `restore_color_delta` restores the prior colours exactly. Marks the touched AABB dirty
    /// so the live CPU re-mesh picks up the new colour this frame.
    pub fn fill_all_color(&mut self, color: u32) -> Vec<(u32, u32)> {
        if !self.ensure_paint_colors() { return Vec::new(); } // sparse → paint deferred (see chokepoint doc)
        let n = self.colors.len(); // == res³, guaranteed by the chokepoint
        let iso = self.iso;
        let res = self.res;
        let mut delta: Vec<(u32, u32)> = Vec::new();
        let (mut min_v, mut max_v) = ([u32::MAX; 3], [0u32; 3]);
        for i in 0..n {
            if self.density.get_idx(i) < iso { continue; } // empty voxel — leave its colour untouched
            let prev = self.colors[i];
            if prev == color { continue; } // already this colour — nothing to record/change
            delta.push((i as u32, prev));
            self.colors[i] = color; // flag byte (0xFF) marks it painted
            let (x, y, z) = (i as u32 % res, (i as u32 / res) % res, i as u32 / (res * res));
            min_v = [min_v[0].min(x), min_v[1].min(y), min_v[2].min(z)];
            max_v = [max_v[0].max(x), max_v[1].max(y), max_v[2].max(z)];
        }
        if !delta.is_empty() { self.mark_dirty(min_v, max_v); }
        delta
    }

    /// Write back an `(index, color)` delta and return the INVERSE (the colour words overwritten), so the
    /// same call replayed on the inverse reproduces the prior colours — symmetric colour undo/redo for
    /// `fill_all_color`. Marks the touched AABB dirty so the live mesh re-extracts. DENSE-ONLY; a no-op on
    /// a sparse grid (the colour buffer never allocates there). Used by the sculpt colour-undo path.
    pub fn restore_color_delta(&mut self, delta: &[(u32, u32)]) -> Vec<(u32, u32)> {
        if delta.is_empty() { return Vec::new(); }
        if !self.ensure_paint_colors() { return Vec::new(); } // sparse → no paint buffer (see chokepoint doc)
        let res = self.res;
        let mut inverse = Vec::with_capacity(delta.len());
        let (mut min_v, mut max_v) = ([u32::MAX; 3], [0u32; 3]);
        for &(idx, col) in delta {
            let iu = idx as usize;
            if iu >= self.colors.len() { continue; }
            inverse.push((idx, self.colors[iu]));
            self.colors[iu] = col;
            let (x, y, z) = (idx % res, (idx / res) % res, idx / (res * res));
            min_v = [min_v[0].min(x), min_v[1].min(y), min_v[2].min(z)];
            max_v = [max_v[0].max(x), max_v[1].max(y), max_v[2].max(z)];
        }
        if !inverse.is_empty() { self.mark_dirty(min_v, max_v); }
        inverse
    }

    /// Per-voxel paint colour at a flat index, or `None` if unpainted / no colour buffer. Used by the
    /// chunk mesher to pick the vertex colour. (A vertex straddles 8 corner voxels; the mesher samples
    /// the anchor corner — good enough for a smooth painted surface.)
    #[inline]
    pub fn color_idx(&self, i: usize) -> Option<u32> {
        match self.colors.get(i) {
            Some(&c) if (c & 0xFF00_0000) != 0 => Some(c),
            _ => None,
        }
    }

    /// Stamp an external signed-distance field into this density grid — the SDF→Voxel boolean stamp
    /// (the SDF-modifier-into-sculpt path; see the Display Invariant "SDF enters as a boolean modifier
    /// that stamps the voxel density field, never as a second renderer").
    ///
    /// `sdf` is evaluated PER VOXEL with that voxel's grid-local position (`cell_world_pos`, the same
    /// space brushes use) and returns the source field's signed distance there (negative inside, 0 on the
    /// surface) — the caller's closure does any grid-local→world / body-transform composition (so this
    /// routine never inlines SDF eval; callers use the `FieldEvaluator`). The distance is converted to a
    /// density amount with the SAME anti-aliased convention the base primitives use (`fill_sphere` /
    /// `fill_box`): `smoothstep01(-d/aa)` — 1 well inside the solid, 0 well outside, a ~1-voxel soft
    /// boundary so Surface Nets stays smooth. That stamp density is then combined into the field:
    /// - `additive == true`  → UNION: `new = max(cur, stamp)` (deposit the SDF solid into the material).
    /// - `additive == false` → CARVE: `new = min(cur, 1 - stamp)` (subtract the SDF solid from material).
    ///
    /// `bounds` optionally restricts the sweep to an AABB `[min_xyz, max_xyz]` expressed in the SAME space
    /// `sdf` receives (the source body's bounds mapped into grid-local), so a small stamp doesn't pay for
    /// a full `res³` scan; `None` sweeps the whole grid.
    /// Writes record each modified voxel's pre-stamp density to `rec` (first-touch dedup) so the stamp is
    /// a single entry on the voxel/sculpt undo path — NOT the SDF `transaction()` (sculpt undo is its own
    /// path). Marks the touched AABB dirty (so the live mesh/atlas resync) and returns it, or `None` if
    /// nothing was written (field entirely outside the grid / the optional bounds).
    pub fn fill_from_sdf<F: Fn(Vec3) -> f32>(
        &mut self,
        sdf: F,
        additive: bool,
        bounds: Option<[Vec3; 2]>,
        mut rec: Option<&mut StrokeRecorder>,
    ) -> Option<[u32; 6]> {
        let aa = self.voxel_size();
        let inv_aa = 1.0 / aa.max(1e-6);
        // SEALED BORDER: like brush writes, keep the outermost density layer empty so Surface Nets caps
        // the surface watertight where it meets a grid face (see `brush_aabb`). Sweep [1, res-2].
        let lo = 1u32;
        let hi = self.res.saturating_sub(2).max(1);
        // Optional grid-local bounds → voxel-space sweep window (clamped to the sealed border). Pad by
        // one voxel each side so the anti-aliased boundary is fully captured.
        let (sx, sy, sz, ex, ey, ez) = match bounds {
            Some([wmin, wmax]) => {
                let gmin = self.world_to_grid(wmin - Vec3::splat(aa));
                let gmax = self.world_to_grid(wmax + Vec3::splat(aa));
                let cl = |f: f32| -> u32 { f.clamp(lo as f32, hi as f32) as u32 };
                let (a, b) = (cl(gmin.x.floor()), cl(gmax.x.ceil()));
                let (c, d) = (cl(gmin.y.floor()), cl(gmax.y.ceil()));
                let (e, f) = (cl(gmin.z.floor()), cl(gmax.z.ceil()));
                if a > b || c > d || e > f { return None; }
                (a, c, e, b, d, f)
            }
            None => (lo, lo, lo, hi, hi, hi),
        };

        let mut touched = false;
        let (mut tmin, mut tmax) = ([u32::MAX; 3], [0u32; 3]);
        for z in sz..=ez {
            for y in sy..=ey {
                for x in sx..=ex {
                    let p = self.cell_world_pos(x, y, z);
                    let d = sdf(p);
                    // Anti-aliased solidity of the source field at this voxel (matches fill_sphere/box).
                    let stamp = smoothstep01(-d * inv_aa); // 1 inside, 0 outside, ~1-voxel soft edge
                    if stamp < 1e-5 { continue; } // field doesn't reach this voxel → leave it untouched
                    let i = self.idx(x, y, z);
                    let cur = self.density.get_idx(i);
                    let new = if additive {
                        cur.max(stamp)             // UNION: add the SDF solid
                    } else {
                        cur.min(1.0 - stamp)       // CARVE: subtract the SDF solid
                    };
                    if (new - cur).abs() < 1e-6 { continue; } // no net change → don't dirty/record it
                    if let Some(r) = rec.as_deref_mut() { r.record(i as u32, cur); }
                    self.density.set_idx(i, new.clamp(0.0, 1.0));
                    touched = true;
                    tmin = [tmin[0].min(x), tmin[1].min(y), tmin[2].min(z)];
                    tmax = [tmax[0].max(x), tmax[1].max(y), tmax[2].max(z)];
                }
            }
        }
        if !touched { return None; }
        self.mark_dirty(tmin, tmax);
        Some([tmin[0], tmin[1], tmin[2], tmax[0], tmax[1], tmax[2]])
    }

    /// Smooth-relax a voxel AABB toward local 6-neighbour averages for `iters` passes (each blends
    /// every voxel toward its neighbour mean by `amount` ∈ [0,1]). This is the density-grid analogue
    /// of SDF "redistancing": advection brushes (Pinch / Move / Snake-Hook) move samples and leave
    /// resampling noise, and this restores a smooth field BEFORE the next brush-plane / normal /
    /// curvature estimate reads it. Each pass reads a pre-pass snapshot, so it is order-independent.
    /// Pre-pass values are recorded to `rec` (first-touch dedup) so the relax is part of stroke undo.
    /// `preserve_iso`: when true, the relax may NOT push a voxel across `self.iso`. This protects a thin
    /// Pinched crease — plain averaging erodes a ~1-cell wall's peak below iso each pass until the surface
    /// dissolves (a white crack that grows mid-stroke). Flanks (away from iso) still smooth.
    pub fn relax_region(&mut self, aabb: [u32; 6], iters: u32, amount: f32, preserve_iso: bool, mut rec: Option<&mut StrokeRecorder>) {
        const WRITE_EPSILON: f32 = 1.0e-7;

        let [cx, cy, cz, mx, my, mz] = aabb;
        if cx > mx || cy > my || cz > mz { return; }
        let amount = amount.clamp(0.0, 1.0);
        let iso = self.iso;
        let span_x = (mx - cx + 1) as usize;
        let span_y = (my - cy + 1) as usize;
        let span_z = (mz - cz + 1) as usize;
        let mut s = vec![0.0f32; span_x * span_y * span_z];
        let mut touched = false;
        for _ in 0..iters {
            let mut k = 0;
            for z in cz..=mz { for y in cy..=my { for x in cx..=mx { s[k] = self.density.get_idx(self.idx(x, y, z)); k += 1; } } }
            for z in cz..=mz {
                for y in cy..=my {
                    for x in cx..=mx {
                        let lx = (x - cx) as usize; let ly = (y - cy) as usize; let lz = (z - cz) as usize;
                        let cur = s[lx + ly * span_x + lz * span_x * span_y];
                        let avg = (sub_at(&s, span_x, span_y, span_z, lx, ly, lz, 1, 0, 0)
                                 + sub_at(&s, span_x, span_y, span_z, lx, ly, lz, -1, 0, 0)
                                 + sub_at(&s, span_x, span_y, span_z, lx, ly, lz, 0, 1, 0)
                                 + sub_at(&s, span_x, span_y, span_z, lx, ly, lz, 0, -1, 0)
                                 + sub_at(&s, span_x, span_y, span_z, lx, ly, lz, 0, 0, 1)
                                 + sub_at(&s, span_x, span_y, span_z, lx, ly, lz, 0, 0, -1)) / 6.0;
                        let mut nv = (cur + (avg - cur) * amount).clamp(0.0, 1.0);
                        if preserve_iso {
                            // A surface voxel may not cross iso: that erosion is what dissolves a thin
                            // Pinched crease into a crack. Flanks (cur far from iso) are untouched here.
                            if cur > iso && nv <= iso { nv = iso + 1e-4; }
                            else if cur < iso && nv >= iso { nv = iso - 1e-4; }
                        }
                        if (nv - cur).abs() <= WRITE_EPSILON { continue; }
                        let i = self.idx(x, y, z);
                        if let Some(r) = rec.as_deref_mut() { r.record(i as u32, cur); }
                        self.density.set_idx(i, nv);
                        touched = true;
                    }
                }
            }
        }
        if touched {
            self.mark_dirty([cx, cy, cz], [mx, my, mz]);
        }
    }

    /// SMOOTH ALL: relax the ENTIRE grid toward its local 6-neighbour average for `iters` passes — a
    /// global density smooth (the "Smooth All" panel button). The density-grid analogue of the Smooth
    /// brush applied everywhere at once. Reuses the same Laplacian stencil as `relax_region` / the Smooth
    /// brush (`sub_at`), so the operator lives in ONE place. Each pass reads a fresh whole-grid snapshot,
    /// so it is order-independent. Records ONLY voxels whose value actually changed (across the whole
    /// op, first-touch dedup) into `rec` so the caller can register ONE sculpt-undo step (sculpt undo is
    /// its OWN path — NOT transaction()). Returns the number of voxels changed (0 = nothing to smooth →
    /// caller skips the undo push). Marks the full grid dirty so the live re-mesh picks it up. Works on
    /// dense AND sparse grids (sparse iterates res³ but stays a one-shot button cost, never per-frame).
    pub fn smooth_all(&mut self, iters: u32, amount: f32, rec: Option<&mut StrokeRecorder>) -> usize {
        let res = self.res as usize;
        if res == 0 || iters == 0 { return 0; }
        let amount = amount.clamp(0.0, 1.0);
        let n = res * res * res;
        let mut changed = 0usize;
        let mut rec = rec;
        let mut snap = vec![0.0f32; n];
        for _ in 0..iters {
            for i in 0..n { snap[i] = self.density.get_idx(i); }
            for z in 0..res {
                for y in 0..res {
                    for x in 0..res {
                        let cur = snap[x + y * res + z * res * res];
                        let avg = (sub_at(&snap, res, res, res, x, y, z, 1, 0, 0)
                                 + sub_at(&snap, res, res, res, x, y, z, -1, 0, 0)
                                 + sub_at(&snap, res, res, res, x, y, z, 0, 1, 0)
                                 + sub_at(&snap, res, res, res, x, y, z, 0, -1, 0)
                                 + sub_at(&snap, res, res, res, x, y, z, 0, 0, 1)
                                 + sub_at(&snap, res, res, res, x, y, z, 0, 0, -1)) / 6.0;
                        let nv = (cur + (avg - cur) * amount).clamp(0.0, 1.0);
                        if nv != cur {
                            let i = x + y * res + z * res * res;
                            // First-touch dedup in the recorder keeps the PRE-smooth value across passes,
                            // so one undo step reverts the whole Smooth All exactly. Count first touches.
                            if let Some(r) = rec.as_deref_mut() {
                                let before = r.len();
                                r.record(i as u32, cur);
                                if r.len() != before { changed += 1; }
                            } else { changed += 1; }
                            self.density.set_idx(i, nv);
                        }
                    }
                }
            }
        }
        if changed > 0 {
            let last = (res - 1) as u32;
            self.mark_dirty([0, 0, 0], [last, last, last]);
        }
        changed
    }

    /// One-shot SYMMETRIZE across a local axis (0=X, 1=Y, 2=Z): mirror one half of the density grid
    /// onto the other, making the sculpt symmetric NOW. This is the deliberate, whole-object op — NOT
    /// the live per-stroke symmetry toggles (which only mirror future stamps). `positive_to_negative =
    /// true` KEEPS the +axis half (higher voxel indices = the +bounds side) and overwrites the −axis
    /// half with its mirror; `false` reverses. Reads the (disjoint) SOURCE half and writes only the DEST
    /// half, so no read ever sees a just-written voxel. Density only — paint `colors` are NOT mirrored
    /// (a documented v1 boundary; the color-undo lane is separate). Records each overwritten voxel's PRE
    /// value into `rec` with first-touch dedup, so the whole op is ONE reversible sculpt-undo delta,
    /// exactly like `smooth_all`. Marks the whole grid dirty. Returns the number of voxels changed
    /// (0 when already symmetric → the caller can no-op).
    pub fn symmetrize(&mut self, axis: usize, positive_to_negative: bool,
                      rec: Option<&mut StrokeRecorder>) -> usize {
        let res = self.res as usize;
        if res == 0 || axis > 2 { return 0; }
        let half = res / 2;
        let mut rec = rec;
        let mut changed = 0usize;
        for z in 0..res {
            for y in 0..res {
                for x in 0..res {
                    // Coord along `axis`; overwrite the DEST half only (the master half + the odd-res
                    // centre voxel, which mirrors to itself, are left untouched).
                    let a = match axis { 0 => x, 1 => y, _ => z };
                    let on_dest = if positive_to_negative { a < half } else { a >= res - half };
                    if !on_dest { continue; }
                    let m = res - 1 - a; // mirror index along the axis (source is in the master half)
                    let (sx, sy, sz) = match axis { 0 => (m, y, z), 1 => (x, m, z), _ => (x, y, m) };
                    let di = x + y * res + z * res * res;
                    let si = sx + sy * res + sz * res * res;
                    let sval = self.density.get_idx(si);
                    let cur = self.density.get_idx(di);
                    if sval != cur {
                        // First-touch dedup keeps the PRE-symmetrize value so one undo reverts the op.
                        if let Some(r) = rec.as_deref_mut() {
                            let before = r.len();
                            r.record(di as u32, cur);
                            if r.len() != before { changed += 1; }
                        } else { changed += 1; }
                        self.density.set_idx(di, sval);
                    }
                }
            }
        }
        if changed > 0 {
            let last = (res - 1) as u32;
            self.mark_dirty([0, 0, 0], [last, last, last]);
        }
        changed
    }

    // ------------------------------------------------------------------
    // Undo/redo — density deltas over a brush footprint.
    // ------------------------------------------------------------------

    /// Snapshot (flat_index, density) for every voxel a brush at (center, radius) can
    /// touch. Captured BEFORE applying the brush; the caller dedups across drag frames.
    pub fn snapshot_brush(&self, center: Vec3, radius: f32) -> Vec<(u32, f32)> {
        let Some([cx, cy, cz, mx, my, mz]) = self.brush_aabb(center, radius) else { return Vec::new() };
        let mut out = Vec::new();
        for z in cz..=mz {
            for y in cy..=my {
                for x in cx..=mx {
                    let i = self.idx(x, y, z) as u32;
                    out.push((i, self.density.get_idx(i as usize)));
                }
            }
        }
        out
    }

    /// Write back an (index, density) delta and return the INVERSE (the values that were
    /// overwritten), so the same call replayed on the inverse reproduces the prior state
    /// — symmetric undo/redo. Marks the touched AABB dirty.
    pub fn restore_delta(&mut self, delta: &[(u32, f32)]) -> Vec<(u32, f32)> {
        if delta.is_empty() { return Vec::new(); }
        let res = self.res;
        let mut min_v = [u32::MAX; 3];
        let mut max_v = [0u32; 3];
        let mut inverse = Vec::with_capacity(delta.len());
        let iso = self.iso;
        let has_colors = self.colors_in_sync(); // shared predicate — never a raw len compare
        for &(idx, val) in delta {
            let iu = idx as usize;
            if iu >= self.density.len() { continue; }
            inverse.push((idx, self.density.get_idx(iu)));
            self.density.set_idx(iu, val);
            // PAINT undo: a voxel restored to empty (below iso) loses its paint flag, so material an
            // undone stroke removed never carries stale colour back. (Paint on voxels that remain solid
            // is not reverted in this first version — the paint-undo delta is a documented follow-up.)
            if has_colors && val < iso { self.colors[iu] = 0; }
            let x = idx % res;
            let y = (idx / res) % res;
            let z = idx / (res * res);
            min_v = [min_v[0].min(x), min_v[1].min(y), min_v[2].min(z)];
            max_v = [max_v[0].max(x), max_v[1].max(y), max_v[2].max(z)];
        }
        self.mark_dirty(min_v, max_v);
        inverse
    }
}
