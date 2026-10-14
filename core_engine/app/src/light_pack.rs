//! P3.1 Frame light packer — author → shade list → GPU buffer + link masks + vol bits.
//!
//! Design: `docs/planning/render/P3_CLUSTERED_LIGHT_CULL_DESIGN.md`
//!
//! - **Author space:** `LightData.id` (stable)
//! - **Shade space:** GPU slot `li` after packing (ephemeral, ≤ MAX_SHADE for PBR)
//! - Link masks are built **in shade order** so bit `li` always matches `light_data[li]`.
//! - **Phase C:** hysteresis (`ShadeHysteresis`) reduces top-K popping while orbiting.

use core_math::scene_graph::{LightData, LightLinkMode, LightType};
use glam::Vec3;
use std::collections::HashMap;

/// Deferred shade-list hard cap (Phase D: 32; RT dual packs still use first 8).
pub(crate) const MAX_SHADE_PBR: usize = 32;
/// RT dual-pack channel cap (pack0+pack1 RGBA) — first N of shade list.
pub(crate) const MAX_SHADE_RT: usize = 8;
/// Bitmask width for light linking (2×u32 per prim).
pub(crate) const MAX_LINK_LIGHTS: usize = 64;

/// Frames a shade-list member stays force-kept after selection (unless out of range).
pub(crate) const HOLD_FRAMES: u8 = 12;
/// Soft score boost for previous shade members (challenger must overcome this).
pub(crate) const STICKY_SCORE_MULT: f32 = 1.4;

/// Phase C — temporal stickiness for the PBR shade list (no orbit popping).
#[derive(Clone, Debug, Default)]
pub(crate) struct ShadeHysteresis {
    /// Author ids from last packed shade list (shade order).
    pub prev_ids: Vec<u32>,
    /// Remaining sticky frames per author id.
    pub hold: HashMap<u32, u8>,
}

impl ShadeHysteresis {
    pub fn clear(&mut self) {
        self.prev_ids.clear();
        self.hold.clear();
    }

    /// After a successful pack: refresh hold for shade members, decay the rest.
    fn commit(&mut self, shade_ids: &[u32]) {
        // Decay ids that dropped out.
        let shade_set: std::collections::HashSet<u32> = shade_ids.iter().copied().collect();
        self.hold.retain(|id, frames| {
            if shade_set.contains(id) {
                true
            } else {
                *frames = frames.saturating_sub(1);
                *frames > 0
            }
        });
        for &id in shade_ids {
            self.hold.insert(id, HOLD_FRAMES);
        }
        self.prev_ids = shade_ids.to_vec();
    }
}

/// One frame's GPU light payload (single source of truth for lights + linking + vol).
#[derive(Clone, Debug, Default)]
pub(crate) struct FrameLightGpu {
    /// `[count | light0..lightK-1]` — 25 floats per light.
    pub light_data: Vec<f32>,
    /// Shade-ordered (mode, flat_prim_indices) for `build_light_link_masks`.
    pub link_entries: Vec<(LightLinkMode, Vec<u32>)>,
    /// Bitfield: bit `li` = shade light `li` has volumetric enabled.
    pub vol_bits: u32,
    /// Author light ids in shade order.
    pub shade_ids: Vec<u32>,
    /// Visible lights before cull.
    pub visible_count: u32,
    /// Culled = visible - shade (PBR top-K).
    pub culled_count: u32,
    /// Phase D: cluster list buffer (u32). Empty/disabled → deferred global loop.
    pub cluster_data: Vec<u32>,
}

/// Input snapshot for one author light (visible only).
#[derive(Clone, Debug)]
struct AuthorLight {
    id: u32,
    data: LightData,
}

/// Build per-prim light link bitmask buffer.
/// `lights[li]` is shade-order; each target list is **flat prim indices** (already resolved).
/// Returns `Vec<u32>` of length `max(num_prims*2, 2)`. Bit N = 1 means shade light N affects prim.
pub(crate) fn build_light_link_masks(
    lights: &[(LightLinkMode, Vec<u32>)],
    num_prims: usize,
) -> Vec<u32> {
    let count = num_prims.max(1);
    let mut masks = vec![0xFFFF_FFFFu32; count * 2]; // default: all lights affect all prims

    for (li, (mode, targets)) in lights.iter().enumerate() {
        if li >= MAX_LINK_LIGHTS {
            break;
        }
        match mode {
            LightLinkMode::Default => {}
            LightLinkMode::Include => {
                let word_off = if li >= 32 { 1 } else { 0 };
                let bit = 1u32 << (li & 31);
                for pi in 0..count {
                    masks[pi * 2 + word_off] &= !bit;
                }
                for &pi in targets {
                    let pi = pi as usize;
                    if pi < count {
                        masks[pi * 2 + word_off] |= bit;
                    }
                }
            }
            LightLinkMode::Exclude => {
                let word_off = if li >= 32 { 1 } else { 0 };
                let bit = 1u32 << (li & 31);
                for &pi in targets {
                    let pi = pi as usize;
                    if pi < count {
                        masks[pi * 2 + word_off] &= !bit;
                    }
                }
            }
        }
    }
    masks
}

/// Resolve author `link_targets` (prim_uid preferred; flat index accepted as fallback)
/// into flat prim indices for the mask builder.
pub(crate) fn resolve_link_targets(
    targets: &[u32],
    uid_to_flat: &HashMap<u32, usize>,
    num_prims: usize,
) -> Vec<u32> {
    let mut out = Vec::with_capacity(targets.len());
    for &t in targets {
        if let Some(&flat) = uid_to_flat.get(&t) {
            out.push(flat as u32);
        } else if (t as usize) < num_prims {
            // Legacy: stored as flat index
            out.push(t);
        }
        // else: missing prim — skip
    }
    out
}

/// Score a light for top-K selection (higher = keep). Negative = out of range.
fn score_light(l: &LightData, cam_pos: Vec3, cam_target: Vec3, force_ids: &[u32]) -> f32 {
    if force_ids.contains(&l.id) {
        return 1.0e9;
    }
    match l.light_type {
        LightType::Directional => {
            // Prefer brighter suns; always high so hard-rule can pick the best.
            1.0e6
                + l.intensity
                    * (0.3
                        + 0.7
                            * (l.color[0] * 0.2126 + l.color[1] * 0.7152 + l.color[2] * 0.0722))
        }
        LightType::Point | LightType::Spot => {
            let pos = l.position;
            let dist = (pos - cam_pos).length().max(0.05);
            // Cull candidates beyond range (still may force-include later)
            if dist > l.range * 1.15 && l.range > 0.01 {
                return -1.0;
            }
            let lum = l.intensity
                * (l.color[0] * 0.2126 + l.color[1] * 0.7152 + l.color[2] * 0.0722).max(0.01);
            let mut s = lum / (1.0 + dist * dist);
            // Prefer lights near look-at
            let d_tgt = (pos - cam_target).length();
            s += 0.15 * lum / (1.0 + d_tgt * d_tgt);
            if l.volumetric.enabled {
                s *= 1.1;
            }
            if l.rt_shadow.enabled {
                s *= 1.05;
            }
            if matches!(l.light_type, LightType::Spot) {
                // Mild bonus if target roughly in front of spot
                let dir = glam::Vec3::from_array(l.direction());
                let to_t = (cam_target - pos).normalize_or_zero();
                let cos_a = dir.dot(to_t);
                if cos_a > l.spot_outer_deg.to_radians().cos() {
                    s *= 1.2;
                }
            }
            s
        }
    }
}

/// Put best Directional at shade index 0 when present (PBR hard rule / sun slot).
fn promote_directional_to_front(authors: &[AuthorLight], order: &mut Vec<usize>) {
    if let Some(pos) = order
        .iter()
        .position(|&i| matches!(authors[i].data.light_type, LightType::Directional))
    {
        if pos != 0 {
            order.swap(0, pos);
        }
    }
}

/// Stabilize shade order: sun @0, then previous frame order for survivors, then newcomers by score.
fn stabilize_order(
    authors: &[AuthorLight],
    chosen: &[usize],
    scores: &[f32],
    prev_ids: &[u32],
) -> Vec<usize> {
    let chosen_set: std::collections::HashSet<usize> = chosen.iter().copied().collect();
    let mut out: Vec<usize> = Vec::with_capacity(chosen.len());

    // Prefer previous order among survivors (deterministic, reduces RT pack channel flicker).
    for &pid in prev_ids {
        if let Some(ai) = authors.iter().position(|a| a.id == pid) {
            if chosen_set.contains(&ai) && !out.contains(&ai) {
                out.push(ai);
            }
        }
    }
    // Newcomers by score desc, then lower id.
    let mut rest: Vec<usize> = chosen
        .iter()
        .copied()
        .filter(|i| !out.contains(i))
        .collect();
    rest.sort_by(|a, b| {
        scores[*b]
            .partial_cmp(&scores[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| authors[*a].id.cmp(&authors[*b].id))
    });
    out.extend(rest);
    promote_directional_to_front(authors, &mut out);
    out
}

/// Select up to `max_shade` author lights. `full_list` = no cull (PathTrace / Phase A order).
///
/// When culling (PBR/Lit): hard-rule best Directional → shade index 0, then force-ids,
/// then hold/sticky prev members, then top-K by score.
/// When `full_list`: preserve author/visible order (no reorder).
fn select_shade_list(
    authors: &[AuthorLight],
    cam_pos: Vec3,
    cam_target: Vec3,
    force_ids: &[u32],
    max_shade: usize,
    full_list: bool,
    hyst: Option<&ShadeHysteresis>,
) -> Vec<usize> {
    if authors.is_empty() {
        return Vec::new();
    }
    // PathTrace / Phase A: keep visible author order (no sun reorder).
    if full_list {
        return (0..authors.len()).collect();
    }
    // Cull path, all fit: still promote directional to shade index 0.
    if authors.len() <= max_shade {
        let mut order: Vec<usize> = (0..authors.len()).collect();
        promote_directional_to_front(authors, &mut order);
        return order;
    }

    let prev_ids: &[u32] = hyst.map(|h| h.prev_ids.as_slice()).unwrap_or(&[]);
    let hold_map = hyst.map(|h| &h.hold);

    // Base scores
    let mut scores: Vec<f32> = authors
        .iter()
        .map(|a| score_light(&a.data, cam_pos, cam_target, force_ids))
        .collect();

    // Soft hysteresis: boost previous / held shade members still in range.
    // Hold adds a second boost so members stay preferred for ~HOLD_FRAMES without
    // hard-locking the budget (a clearly better light can still displace).
    if !prev_ids.is_empty() || hold_map.is_some() {
        for (i, a) in authors.iter().enumerate() {
            if scores[i] < 0.0 {
                continue;
            }
            let mut mult = 1.0f32;
            if prev_ids.contains(&a.id) {
                mult *= STICKY_SCORE_MULT;
            }
            if let Some(hold) = hold_map {
                if hold.get(&a.id).copied().unwrap_or(0) > 0 {
                    mult *= 1.15;
                }
            }
            scores[i] *= mult;
        }
    }

    // Hard rule: best Directional first if any
    let mut chosen: Vec<usize> = Vec::with_capacity(max_shade);
    if let Some((di, _)) = scores
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            **s >= 0.0 && matches!(authors[*i].data.light_type, LightType::Directional)
        })
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
    {
        chosen.push(di);
    }

    // Force-includes next (selected / non-Default link)
    for (i, a) in authors.iter().enumerate() {
        if chosen.len() >= max_shade {
            break;
        }
        if force_ids.contains(&a.id) && !chosen.contains(&i) {
            chosen.push(i);
        }
    }

    // Fill by score (exclude already chosen; skip score < 0 unless needed to fill)
    let mut ranked: Vec<usize> = (0..authors.len()).collect();
    ranked.sort_by(|a, b| {
        scores[*b]
            .partial_cmp(&scores[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| authors[*a].id.cmp(&authors[*b].id))
    });
    for i in ranked {
        if chosen.len() >= max_shade {
            break;
        }
        if chosen.contains(&i) {
            continue;
        }
        if scores[i] < 0.0 && !chosen.is_empty() {
            continue; // out of range — only if we have something else
        }
        chosen.push(i);
    }

    // If still empty (all negative), take closest by dist
    if chosen.is_empty() {
        let mut by_dist: Vec<(usize, f32)> = authors
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let d = match a.data.light_type {
                    LightType::Directional => 0.0,
                    _ => (a.data.position - cam_pos).length(),
                };
                (i, d)
            })
            .collect();
        by_dist.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (i, _) in by_dist.into_iter().take(max_shade) {
            chosen.push(i);
        }
    }

    stabilize_order(authors, &chosen, &scores, prev_ids)
}

/// Build the frame light GPU blob.
///
/// * `cull` — if true, top-K by score (PBR/Lit). If false, all visible (PathTrace / offline).
/// * `force_ids` — always include these author light ids (selected, etc.).
/// * `uid_to_flat` — prim_uid → flat index for link target resolve.
/// * `hyst` — Phase C sticky state; updated on cull packs, cleared when `!cull`.
/// * `hdr_ambient_tint` — SH sky average × brightness when env is loaded (for From HDR ambient).
pub(crate) fn build_frame_lights(
    visible_lights: &[&LightData],
    cam_pos: Vec3,
    cam_target: Vec3,
    cull: bool,
    force_ids: &[u32],
    uid_to_flat: &HashMap<u32, usize>,
    num_prims: usize,
    hyst: Option<&mut ShadeHysteresis>,
    cluster_params: Option<&crate::light_cluster::ClusterParams>,
    hdr_ambient_tint: Option<[f32; 3]>,
) -> FrameLightGpu {
    let authors: Vec<AuthorLight> = visible_lights
        .iter()
        .map(|l| AuthorLight {
            id: l.id,
            data: (*l).clone(),
        })
        .collect();
    let visible_count = authors.len() as u32;

    let full_list = !cull;
    let max_shade = if cull {
        MAX_SHADE_PBR
    } else {
        authors.len().max(1).min(256)
    };

    // Snapshot for selection (don't hold mut borrow across select).
    let hyst_snap = hyst.as_ref().map(|h| ShadeHysteresis {
        prev_ids: h.prev_ids.clone(),
        hold: h.hold.clone(),
    });

    let order = select_shade_list(
        &authors,
        cam_pos,
        cam_target,
        force_ids,
        max_shade,
        full_list,
        hyst_snap.as_ref(),
    );
    let culled_count = visible_count.saturating_sub(order.len() as u32);

    let mut light_data = Vec::with_capacity(1 + order.len() * 25);
    light_data.push(f32::from_bits(order.len() as u32));
    let mut link_entries = Vec::with_capacity(order.len());
    let mut shade_ids = Vec::with_capacity(order.len());
    let mut vol_bits = 0u32;

    for (li, &ai) in order.iter().enumerate() {
        let a = &authors[ai];
        let mut packed = a.data.serialize_to_gpu();
        // HDR-driven ambient tint (directional): slots 22..24.
        if a.data.ambient_from_hdr {
            if let Some(tint) = hdr_ambient_tint {
                packed[22] = tint[0];
                packed[23] = tint[1];
                packed[24] = tint[2];
            }
        } else {
            let t = a.data.ambient_color;
            packed[22] = t[0];
            packed[23] = t[1];
            packed[24] = t[2];
        }
        light_data.extend_from_slice(&packed);
        shade_ids.push(a.id);
        let flat_targets = resolve_link_targets(&a.data.link_targets, uid_to_flat, num_prims);
        link_entries.push((a.data.link_mode.clone(), flat_targets));
        if a.data.volumetric.enabled && li < 32 {
            vol_bits |= 1u32 << li;
        }
    }
    if light_data.len() < 2 {
        light_data.resize(2, 0.0);
    }

    if let Some(h) = hyst {
        if cull {
            h.commit(&shade_ids);
        } else {
            h.clear();
        }
    }

    // Phase D: CPU cluster assignment (PBR/Lit only; disabled header for PathTrace).
    let cluster_data = if cull {
        if let Some(cp) = cluster_params {
            let mut clights = Vec::with_capacity(order.len());
            for (li, &ai) in order.iter().enumerate() {
                let a = &authors[ai];
                clights.push(crate::light_cluster::ClusterLight {
                    shade_li: li as u32,
                    is_directional: matches!(a.data.light_type, LightType::Directional),
                    position: a.data.position,
                    // Preserve the <=0 "infinite range" sentinel — the shader treats it
                    // as unbounded inverse-square, and the cluster now inserts such
                    // lights everywhere (clamping to 0.05 here culled them to one tile).
                    range: a.data.range,
                });
            }
            crate::light_cluster::build_cluster_lists(cp, &clights)
        } else {
            crate::light_cluster::empty_cluster_data()
        }
    } else {
        crate::light_cluster::empty_cluster_data()
    };

    FrameLightGpu {
        light_data,
        link_entries,
        vol_bits,
        shade_ids,
        visible_count,
        culled_count,
        cluster_data,
    }
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn point(id: u32, pos: Vec3, intensity: f32) -> LightData {
        let mut l = LightData::new_point(id, format!("P{id}"), pos);
        l.intensity = intensity;
        l
    }

    fn pack(
        refs: &[&LightData],
        cam: Vec3,
        cull: bool,
        force: &[u32],
        hyst: Option<&mut ShadeHysteresis>,
    ) -> FrameLightGpu {
        let map = HashMap::new();
        build_frame_lights(refs, cam, cam, cull, force, &map, 0, hyst, None, None)
    }

    #[test]
    fn phase_a_visible_order_no_cull_matches_count() {
        let a = point(1, Vec3::new(0.0, 2.0, 0.0), 1.0);
        let b = point(2, Vec3::new(10.0, 2.0, 0.0), 1.0);
        let vis = vec![&a, &b];
        let f = pack(&vis, Vec3::ZERO, false, &[], None);
        assert_eq!(f.visible_count, 2);
        assert_eq!(f.culled_count, 0);
        assert_eq!(f.shade_ids, vec![1, 2]);
        assert_eq!(f32::to_bits(f.light_data[0]), 2);
    }

    #[test]
    fn phase_b_top_k_keeps_nearest() {
        let near = point(1, Vec3::new(1.0, 0.0, 0.0), 2.0);
        let far = point(2, Vec3::new(100.0, 0.0, 0.0), 2.0);
        let mid = point(3, Vec3::new(5.0, 0.0, 0.0), 2.0);
        let fillers: Vec<LightData> = (10..50)
            .map(|i| point(i, Vec3::new(50.0 + i as f32, 0.0, 0.0), 0.5))
            .collect();
        let mut refs: Vec<&LightData> = vec![&near, &far, &mid];
        for f in &fillers {
            refs.push(f);
        }
        let f = pack(&refs, Vec3::ZERO, true, &[], None);
        assert!(f.shade_ids.len() <= MAX_SHADE_PBR);
        assert!(f.shade_ids.contains(&1), "near light must be packed");
        assert_eq!(f.visible_count, 43);
        assert!(f.culled_count >= 5);
    }

    #[test]
    fn sun_forced_to_shade_index_zero() {
        let sun = {
            let mut l = LightData::default();
            l.id = 1;
            l.light_type = LightType::Directional;
            l.intensity = 1.0;
            l
        };
        let p = point(2, Vec3::new(0.5, 0.0, 0.0), 10.0);
        let vis = vec![&p, &sun]; // point first in author order
        let f = pack(&vis, Vec3::ZERO, true, &[], None);
        assert_eq!(f.shade_ids[0], 1, "directional must be shade index 0");
    }

    #[test]
    fn force_include_selected_far_light() {
        let far = point(99, Vec3::new(200.0, 0.0, 0.0), 0.1);
        let near: Vec<LightData> = (1..40)
            .map(|i| point(i, Vec3::new(i as f32 * 0.5, 0.0, 0.0), 2.0))
            .collect();
        let mut refs: Vec<&LightData> = near.iter().collect();
        refs.push(&far);
        let f = pack(&refs, Vec3::ZERO, true, &[99], None);
        assert!(f.shade_ids.contains(&99), "force id must be in shade list");
    }

    #[test]
    fn link_include_remapped_to_shade_bits() {
        let mut a = point(1, Vec3::ZERO, 1.0);
        a.link_mode = LightLinkMode::Default;
        let mut b = point(2, Vec3::new(1.0, 0.0, 0.0), 1.0);
        b.link_mode = LightLinkMode::Include;
        b.link_targets = vec![100]; // prim_uid 100
        let mut uid_map = HashMap::new();
        uid_map.insert(100u32, 0usize);
        let vis = vec![&a, &b];
        let f = build_frame_lights(&vis, Vec3::ZERO, Vec3::ZERO, false, &[], &uid_map, 2, None, None, None);
        let masks = build_light_link_masks(&f.link_entries, 2);
        let bit1 = 1u32 << 1;
        assert_ne!(masks[0] & bit1, 0, "prim 0 should get light 1");
        assert_eq!(masks[2] & bit1, 0, "prim 1 should not get light 1");
        assert_ne!(masks[0] & 1, 0);
        assert_ne!(masks[2] & 1, 0);
    }

    #[test]
    fn resolve_uid_and_legacy_flat() {
        let mut map = HashMap::new();
        map.insert(50u32, 3usize);
        let t = resolve_link_targets(&[50, 1, 999], &map, 10);
        assert_eq!(t, vec![3, 1]);
    }

    #[test]
    fn vol_bits_follow_shade_order() {
        let mut a = point(1, Vec3::new(0.0, 1.0, 0.0), 1.0);
        a.volumetric.enabled = false;
        let mut b = point(2, Vec3::new(0.0, 1.0, 1.0), 1.0);
        b.volumetric.enabled = true;
        let vis = vec![&a, &b];
        let f = pack(&vis, Vec3::ZERO, false, &[], None);
        assert_eq!(f.vol_bits & 1, 0);
        assert_ne!(f.vol_bits & 2, 0);
    }

    /// Phase C: borderline swap — sticky keeps previous shade set across a tiny camera nudge.
    #[test]
    fn phase_c_hysteresis_keeps_prev_on_borderline_swap() {
        // 9 equal-ish lights on a ring; top-K is unstable without sticky.
        let lights: Vec<LightData> = (1..=40)
            .map(|i| {
                let ang = (i as f32) * std::f32::consts::TAU / 40.0;
                point(
                    i,
                    Vec3::new(ang.cos() * 4.0, 0.0, ang.sin() * 4.0),
                    2.0,
                )
            })
            .collect();
        let refs: Vec<&LightData> = lights.iter().collect();
        let mut hyst = ShadeHysteresis::default();
        let cam0 = Vec3::new(0.05, 0.0, 0.0);
        let f0 = pack(&refs, cam0, true, &[], Some(&mut hyst));
        assert_eq!(f0.shade_ids.len(), MAX_SHADE_PBR);
        let set0: std::collections::HashSet<u32> = f0.shade_ids.iter().copied().collect();

        // Tiny camera move that would reorder pure scores.
        let cam1 = Vec3::new(-0.05, 0.0, 0.0);
        let f1 = pack(&refs, cam1, true, &[], Some(&mut hyst));
        let set1: std::collections::HashSet<u32> = f1.shade_ids.iter().copied().collect();
        assert_eq!(
            set0, set1,
            "sticky/hold must keep the same shade set across a tiny orbit nudge: {:?} vs {:?}",
            set0, set1
        );
    }

    /// Phase C: out-of-range light is dropped even with remaining hold.
    #[test]
    fn phase_c_hold_drops_out_of_range() {
        let mut near = point(1, Vec3::new(1.0, 0.0, 0.0), 2.0);
        near.range = 5.0;
        let mut sticky_far = point(2, Vec3::new(2.0, 0.0, 0.0), 2.0);
        sticky_far.range = 5.0;
        let fillers: Vec<LightData> = (10..50)
            .map(|i| {
                let mut l = point(i, Vec3::new(i as f32 * 0.3, 0.0, 0.0), 2.0);
                l.range = 50.0;
                l
            })
            .collect();
        let mut hyst = ShadeHysteresis::default();
        // Frame 0: both near lights in range at cam origin.
        let mut refs: Vec<&LightData> = vec![&near, &sticky_far];
        for f in &fillers {
            refs.push(f);
        }
        let f0 = pack(&refs, Vec3::ZERO, true, &[], Some(&mut hyst));
        assert!(f0.shade_ids.contains(&2));
        assert!(hyst.hold.get(&2).copied().unwrap_or(0) > 0);

        // Frame 1: move sticky_far way out of its range; hold must not keep it.
        sticky_far.position = Vec3::new(200.0, 0.0, 0.0);
        let mut refs2: Vec<&LightData> = vec![&near, &sticky_far];
        for f in &fillers {
            refs2.push(f);
        }
        let f1 = pack(&refs2, Vec3::ZERO, true, &[], Some(&mut hyst));
        assert!(
            !f1.shade_ids.contains(&2),
            "out-of-range light must drop despite hold"
        );
    }

    /// Phase C: a clearly better light enters once score overcomes sticky mult + hold.
    #[test]
    fn phase_c_hold_expires_allows_new_entrant() {
        // MAX_SHADE near lights fill the budget; challenger starts out-of-range.
        let sticky: Vec<LightData> = (1..=MAX_SHADE_PBR as u32)
            .map(|i| {
                let mut l = point(i, Vec3::new(0.15 + (i as f32) * 0.04, 0.0, 0.0), 5.0);
                l.range = 80.0;
                l
            })
            .collect();
        let mut challenger = point(99, Vec3::new(100.0, 0.0, 0.0), 0.01);
        challenger.range = 2.0; // OOR from origin → score -1
        let mut hyst = ShadeHysteresis::default();
        {
            let mut refs: Vec<&LightData> = sticky.iter().collect();
            refs.push(&challenger);
            let f0 = pack(&refs, Vec3::ZERO, true, &[], Some(&mut hyst));
            assert!(
                !f0.shade_ids.contains(&99),
                "far OOR challenger must start culled: {:?}",
                f0.shade_ids
            );
            assert_eq!(f0.shade_ids.len(), MAX_SHADE_PBR);
        }

        // Teleport + boost next to camera — must beat sticky.
        challenger.intensity = 500.0;
        challenger.position = Vec3::new(0.05, 0.0, 0.0);
        challenger.range = 80.0;
        let mut entered = false;
        for _ in 0..(HOLD_FRAMES as usize + 4) {
            let mut refs: Vec<&LightData> = sticky.iter().collect();
            refs.push(&challenger);
            let f = pack(&refs, Vec3::ZERO, true, &[], Some(&mut hyst));
            if f.shade_ids.contains(&99) {
                entered = true;
                break;
            }
        }
        assert!(entered, "strong challenger must eventually enter shade list");
    }

    #[test]
    fn phase_c_full_list_clears_hysteresis() {
        let a = point(1, Vec3::new(0.0, 1.0, 0.0), 1.0);
        let b = point(2, Vec3::new(1.0, 1.0, 0.0), 1.0);
        let mut hyst = ShadeHysteresis::default();
        let _ = pack(&[&a, &b], Vec3::ZERO, true, &[], Some(&mut hyst));
        assert!(!hyst.prev_ids.is_empty());
        let _ = pack(&[&a, &b], Vec3::ZERO, false, &[], Some(&mut hyst));
        assert!(hyst.prev_ids.is_empty());
        assert!(hyst.hold.is_empty());
    }
}
