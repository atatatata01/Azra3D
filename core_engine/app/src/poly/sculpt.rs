//! PolySculpt runtime — clone-once working mesh, in-place display patch, spatial query.
//!
//! Plan: `docs/planning/poly/SCULPT_LAYER_PLAN.md` S0b. Deltas stay on the modifier;
//! this module owns the **working mesh** so a stroke does not deep-clone the remesh prefix.

use core_math::half_edge::{FaceId, HalfEdgeMesh, VertexId, NO_EDGE};
use core_math::voxel_sculpt::falloff_weight_preset;
use core_math::poly_mesh::NormalConfig;
use core_math::scene_graph::{NodeId, PolyModifier};
use glam::Vec3;
use std::collections::HashMap;
use std::sync::Arc;

use crate::AzraApp;

/// `sculpt.poly_edit` slot when Surface Sculpt writes the mesh itself (no Sculpt modifier).
const POLY_SCULPT_DIRECT: usize = usize::MAX;

/// Uniform-grid hash over working-mesh verts. Rebuilt on clone / rebase; updated on stroke.
pub(crate) struct VertGrid {
    cell: f32,
    buckets: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl VertGrid {
    fn empty(cell: f32) -> Self {
        Self {
            cell: cell.max(1e-4),
            buckets: HashMap::new(),
        }
    }

    fn key(&self, p: Vec3) -> (i32, i32, i32) {
        let s = 1.0 / self.cell;
        (
            (p.x * s).floor() as i32,
            (p.y * s).floor() as i32,
            (p.z * s).floor() as i32,
        )
    }

    pub(crate) fn rebuild(mesh: &HalfEdgeMesh, cell: f32) -> Self {
        let mut g = Self::empty(cell);
        for i in 0..mesh.vertices.len() as u32 {
            let vid = VertexId(i);
            if !mesh.is_vert_live(vid) {
                continue;
            }
            let k = g.key(mesh.vert(vid).position);
            g.buckets.entry(k).or_default().push(i);
        }
        g
    }

    /// PR-4 (sculpt_pick grid-DDA): read-only cell/bucket access for the ray walk.
    pub(crate) fn cell_size(&self) -> f32 {
        self.cell
    }
    pub(crate) fn key_of(&self, p: Vec3) -> (i32, i32, i32) {
        self.key(p)
    }
    pub(crate) fn bucket_at(&self, k: (i32, i32, i32)) -> Option<&[u32]> {
        self.buckets.get(&k).map(|v| v.as_slice())
    }

    pub(crate) fn query(&self, mesh: &HalfEdgeMesh, center: Vec3, radius: f32) -> Vec<u32> {
        let r = radius.max(0.0);
        let r2 = r * r;
        let reach = (r / self.cell).ceil() as i32;
        let c = self.key(center);
        let mut out = Vec::new();
        for dz in -reach..=reach {
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let Some(bucket) = self.buckets.get(&(c.0 + dx, c.1 + dy, c.2 + dz)) else {
                        continue;
                    };
                    for &i in bucket {
                        let vid = VertexId(i);
                        if !mesh.is_vert_live(vid) {
                            continue;
                        }
                        if (mesh.vert(vid).position - center).length_squared() <= r2 {
                            out.push(i);
                        }
                    }
                }
            }
        }
        out
    }

    pub(crate) fn move_vert(&mut self, id: u32, old: Vec3, new: Vec3) {
        let a = self.key(old);
        let b = self.key(new);
        if a == b {
            return;
        }
        if let Some(bucket) = self.buckets.get_mut(&a) {
            if let Some(i) = bucket.iter().position(|&x| x == id) {
                bucket.swap_remove(i);
            }
        }
        self.buckets.entry(b).or_default().push(id);
    }
}

/// Per-body clone-once working mesh (C1) + last raster bytes (C2).
/// Mirror moved verts' position+normal into the interleaved display corners.
///
/// ONE writer shared by every PolySculpt path (stamp + anchored Grab). Extracted rather than
/// copied: two display writers drifting apart is the bug class that produced this week's
/// stale-surface reports.
/// The ONE display writer (G2 single-writer rule): stamp, apply_dabs and Grab all call
/// this. PR-2: CSR lookup — O(moved corners); the per-stamp HashMap + full corner scan
/// are gone (G2 negative pins that they stay gone).
/// PR-7 (§7.7) normals policy — ONE place, three stroke callers. Honest tiers:
/// `S*2 >= V` and `S > CLIFF` take the full pass (a whole-mesh region is not a region;
/// a cliff-sized region pays superlinear collar cost — full is FASTER and honest);
/// seeds are NEVER subsampled (wrong normals). Returns the HUD `normals_mode`
/// (1 = region, 2 = full).
/// PR-9: THE stroke radius query — grid by default, PBVH under
/// `AZRA_POLYSCULPT_PBVH=1` (lazy build; equivalence pinned in sculpt_pbvh). One
/// helper so stamp and Grab can never disagree on the query.
pub(super) fn sculpt_radius_query(
    w: &mut PolySculptWorking,
    center: Vec3,
    radius: f32,
) -> Vec<u32> {
    if std::env::var_os("AZRA_POLYSCULPT_PBVH").is_some() {
        if w.pbvh.is_none() {
            w.pbvh = Some(super::sculpt_pbvh::SculptPbvh::build(&w.mesh));
        }
        if let Some(p) = &w.pbvh {
            return p.query(&w.mesh, center, radius);
        }
    }
    w.grid.query(&w.mesh, center, radius)
}

/// PR-9: post-move refit hook — expands the PBVH for moved verts; a saturated
/// escape budget rebuilds (the huge-Grab class).
pub(super) fn sculpt_refit_moved(w: &mut PolySculptWorking, moved: &[u32]) {
    if let Some(p) = &mut w.pbvh {
        if p.refit_moved(&w.mesh, moved) {
            w.pbvh = Some(super::sculpt_pbvh::SculptPbvh::build(&w.mesh));
        }
    }
}

pub(super) const NORMAL_SEED_SOFT: usize = 20_000;
pub(super) const NORMAL_SEED_CLIFF: usize = 80_000;
pub(super) fn sculpt_region_normals(mesh: &mut HalfEdgeMesh, seeds: &[VertexId]) -> u8 {
    if seeds.len() * 2 >= mesh.vertices.len() || seeds.len() > NORMAL_SEED_CLIFF {
        mesh.recompute_normals();
        return 2;
    }
    mesh.recompute_normals_region(seeds.iter().copied());
    1
}

pub(super) fn write_display_corners(w: &mut PolySculptWorking, ids: &[u32]) -> usize {
    let mut patched = 0usize;
    for &i in ids {
        let vid = VertexId(i);
        if !w.mesh.is_vert_live(vid) {
            continue;
        }
        if i as usize + 1 >= w.vert_off.len() {
            continue; // id beyond the CSR build (stroke topology is stable; fail soft)
        }
        let p = w.mesh.vert(vid).position;
        let n = w.mesh.vert(vid).normal;
        let (a, b) = (w.vert_off[i as usize] as usize, w.vert_off[i as usize + 1] as usize);
        for &ci in &w.vert_corners[a..b] {
            w.dirty_corners.push(ci);
            let ci = ci as usize;
            w.display_verts[ci][0] = p.x;
            w.display_verts[ci][1] = p.y;
            w.display_verts[ci][2] = p.z;
            w.display_verts[ci][3] = n.x;
            w.display_verts[ci][4] = n.y;
            w.display_verts[ci][5] = n.z;
            patched += 1;
        }
    }
    patched
}

pub(crate) struct PolySculptWorking {
    pub mesh: HalfEdgeMesh,
    pub gen: u64,
    pub prefix_ptr: usize,
    pub prefix_hash: u64,
    /// Extra ref on the remesh/base prefix so strokes cannot steal it via `try_unwrap`.
    pub prefix: Arc<HalfEdgeMesh>,
    pub display_verts: Vec<[f32; 8]>,
    pub corner_vert: Vec<u32>,
    /// PR-2 (§7.2) CSR: corners of vert `i` are `vert_corners[vert_off[i]..vert_off[i+1]]`,
    /// and `vert_faces[k]` is the live face owning the SAME slot `k` (shared offsets — a
    /// vert has exactly one corner per incident live-face ring). Built once per working
    /// mesh; topology is stable for the whole stroke. RUNTIME ONLY (K/FL4 red line).
    pub vert_off: Vec<u32>,
    pub vert_corners: Vec<u32>,
    pub vert_faces: Vec<u32>,
    /// PR-3: corners patched since the last flush — drained (sorted+deduped) into
    /// `MeshCache::patch_chunk_verts`, so upload bytes ∝ moved corners, not mesh.
    pub dirty_corners: Vec<u32>,
    /// PR-5/6 (§7.4/§7.5): PRE-STROKE positions of verts first-touched this episode.
    /// Drained by `commit_episode` into the sparse `UndoPayload::PolySculpt` and into the
    /// ONE document commit. Runtime only.
    pub first_touch: std::collections::HashMap<u32, Vec3>,
    /// STROKE-START normals for brush math (Blender's `orig_data.no` / `use_original`
    /// analog): with Accumulate OFF, Blender computes area normals/planes from the
    /// coordinates and normals the surface had when the stroke BEGAN — never from the
    /// surface the stroke is mid-way through deforming (calc_area_normal `use_original =
    /// !cache->accum`, v4.2). Sampling brush directions from the live result is a
    /// feedback loop: each dab tilts the normals the next dab aims along (owner
    /// 2026-08-23: "they are picking normal from their own result"). Frozen on first
    /// sight of a vert per stroke; STROKE-scoped (cleared with the deposit ledger) and
    /// cleared on dyntopo topo change (ids can be recycled mid-stroke). Runtime only.
    pub first_touch_normal: std::collections::HashMap<u32, Vec3>,
    /// Inverted-Smooth "Enhance Details" direction field (Blender sculpt_smooth.cc:
    /// `detail_directions[i] = neighbor_avg − co`, computed ONCE at the first enhance
    /// step of the stroke). Freezing the field is what makes enhance stable — a live
    /// negative Laplacian amplifies the noise it just created. STROKE-scoped; cleared on
    /// topo change (indexed by vert id). Runtime only.
    pub enhance_dirs: Option<Vec<Vec3>>,
    /// Accumulate-OFF deposit ledger (Blender BRUSH_ACCUMULATE unset — the default): signed
    /// deposit magnitude already applied THIS STROKE per vert, so overlapping/rescrubbed dabs
    /// top the vert up to the stroke's max fade instead of stacking (one offset layer per
    /// stroke). STROKE-scoped: cleared by `begin_poly_sculpt_episode`. Runtime only.
    pub deposit_applied: std::collections::HashMap<u32, f32>,
    /// PR-9 (P1, §8): vert PBVH — the flagged query (`AZRA_POLYSCULPT_PBVH=1`); the
    /// grid stays production until the tree is measured ≤ grid AND serves pick + draw
    /// (§8.2 / PR-10). Built lazily on first flagged query; refit-expanded per stamp.
    pub pbvh: Option<super::sculpt_pbvh::SculptPbvh>,
    pub grid: VertGrid,
    pub last_stamp_ms: f32,
    pub last_query_ov_ms: f32,
    pub last_query_grid_ms: f32,
}

impl PolySculptWorking {
    fn from_prefix(prefix: Arc<HalfEdgeMesh>, deltas: &[(u32, Vec3)], gen: u64, prefix_hash: u64) -> Self {
        let prefix_ptr = Arc::as_ptr(&prefix) as usize;
        let mut mesh = (*prefix).clone();
        apply_deltas(&mut mesh, deltas);
        let (display_verts, corner_vert, corner_face) = raster_with_corner_map(&mesh);
        let (vert_off, vert_corners, vert_faces) =
            build_vert_csr(mesh.vertices.len(), &corner_vert, &corner_face);
        let grid = VertGrid::rebuild(&mesh, grid_cell_for(&mesh));
        Self {
            mesh,
            gen,
            prefix_ptr,
            prefix_hash,
            prefix,
            display_verts,
            corner_vert,
            vert_off,
            vert_corners,
            vert_faces,
            dirty_corners: Vec::new(),
            first_touch: std::collections::HashMap::new(),
            first_touch_normal: std::collections::HashMap::new(),
            enhance_dirs: None,
            deposit_applied: std::collections::HashMap::new(),
            pbvh: None,
            grid,
            last_stamp_ms: 0.0,
            last_query_ov_ms: 0.0,
            last_query_grid_ms: 0.0,
        }
    }

    fn matches(&self, prefix_ptr: usize, prefix_hash: u64, gen: u64, prefix: &HalfEdgeMesh) -> bool {
        if self.prefix_hash != prefix_hash || self.gen != gen {
            return false;
        }
        if self.prefix_ptr == prefix_ptr {
            return true;
        }
        // `apply_poly_modifiers_cached` wraps the base in a fresh Arc on every start-from-0
        // walk (`Arc::new(base.clone())`). Same remesh/base content, new pointer — must
        // NOT rebase or the stroke that just wrote `gen` is orphaned.
        self.prefix.vertex_count() == prefix.vertex_count()
            && self.prefix.face_count() == prefix.face_count()
    }
}

fn apply_deltas(mesh: &mut HalfEdgeMesh, deltas: &[(u32, Vec3)]) {
    for &(i, d) in deltas {
        let vid = VertexId(i);
        if mesh.is_vert_live(vid) {
            mesh.vert_mut(vid).position += d;
        }
    }
}

pub(crate) fn grid_cell_for(mesh: &HalfEdgeMesh) -> f32 {
    // ~2% of the longest AABB axis so a typical stamp covers a handful of cells.
    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);
    let mut any = false;
    for i in 0..mesh.vertices.len() as u32 {
        let vid = VertexId(i);
        if !mesh.is_vert_live(vid) {
            continue;
        }
        let p = mesh.vert(vid).position;
        mn = mn.min(p);
        mx = mx.max(p);
        any = true;
    }
    let e = if any { (mx - mn).max_element().max(1e-3) } else { 1.0 };
    (e * 0.02).clamp(0.01, 1.0)
}

/// Same face order as [`core_math::poly_mesh::raster_data_ngon_with_normals`]: live faces,
/// sorted by material, skip rings < 3. Lets a stroke patch display bytes without a re-raster.
pub(crate) fn raster_with_corner_map(
    mesh: &HalfEdgeMesh,
) -> (Vec<[f32; 8]>, Vec<u32>, Vec<u32>) {
    let (verts, _, _, corner_vert, corner_face) = raster_full_with_corner_map(mesh);
    (verts, corner_vert, corner_face)
}

/// [`raster_with_corner_map`] that ALSO returns the triangle indices + material ranges —
/// the dyntopo display fast path uploads them straight to the mesh cache instead of
/// paying a second full raster through the stack resync.
pub(crate) fn raster_full_with_corner_map(
    mesh: &HalfEdgeMesh,
) -> (Vec<[f32; 8]>, Vec<u32>, Vec<(u32, u32, u32)>, Vec<u32>, Vec<u32>) {
    let cfg = NormalConfig::default();
    let (verts, indices, mat_ranges) =
        core_math::poly_mesh::raster_data_ngon_with_normals(mesh, &cfg);
    let mut face_order: Vec<u32> = (0..mesh.faces.len() as u32)
        .filter(|&i| mesh.face(FaceId(i)).half_edge != NO_EDGE)
        .collect();
    face_order.sort_by_key(|&i| mesh.face(FaceId(i)).material_id);
    let mut corner_vert = Vec::with_capacity(verts.len());
    let mut corner_face = Vec::with_capacity(verts.len());
    let mut fv = Vec::new();
    for fi in face_order {
        mesh.face_vertices_into(FaceId(fi), &mut fv);
        if fv.len() < 3 {
            continue;
        }
        for v in &fv {
            corner_vert.push(v.0);
            corner_face.push(fi);
        }
    }
    debug_assert_eq!(corner_vert.len(), verts.len());
    (verts, indices, mat_ranges, corner_vert, corner_face)
}

/// PR-2: invert corner→vert into the vert-keyed CSR (counting sort, one pass).
/// `vert_faces[k]` shares slot `k` with `vert_corners[k]` — same offsets by construction.
pub(crate) fn build_vert_csr(
    vert_count: usize,
    corner_vert: &[u32],
    corner_face: &[u32],
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let mut vert_off = vec![0u32; vert_count + 1];
    for &v in corner_vert {
        vert_off[v as usize + 1] += 1;
    }
    for i in 0..vert_count {
        vert_off[i + 1] += vert_off[i];
    }
    let mut cursor: Vec<u32> = vert_off[..vert_count].to_vec();
    let mut vert_corners = vec![0u32; corner_vert.len()];
    let mut vert_faces = vec![0u32; corner_vert.len()];
    for (ci, (&v, &f)) in corner_vert.iter().zip(corner_face).enumerate() {
        let slot = cursor[v as usize] as usize;
        vert_corners[slot] = ci as u32;
        vert_faces[slot] = f;
        cursor[v as usize] += 1;
    }
    (vert_off, vert_corners, vert_faces)
}

fn half_edge_soup(mesh: &HalfEdgeMesh) -> (Vec<Vec3>, Vec<u32>) {
    let verts: Vec<Vec3> = mesh.vertices.iter().map(|v| v.position).collect();
    let mut indices = Vec::new();
    for i in 0..mesh.faces.len() as u32 {
        let fid = FaceId(i);
        if mesh.face(fid).half_edge == NO_EDGE {
            continue;
        }
        let ring = mesh.face_vertices(fid);
        if ring.len() < 3 {
            continue;
        }
        let local: Vec<Vec3> = ring.iter().map(|&v| mesh.vert(v).position).collect();
        let n = mesh.compute_face_normal(fid);
        for tri in core_math::poly_mesh::earclip_polygon(&local, n) {
            indices.push(ring[tri[0] as usize].0);
            indices.push(ring[tri[1] as usize].0);
            indices.push(ring[tri[2] as usize].0);
        }
    }
    (verts, indices)
}

/// Re-splat stored deltas onto a new remesh/smooth prefix. Misses (beyond max_dist) are orphans.
pub(crate) fn rebase_deltas(
    old_prefix: &HalfEdgeMesh,
    old_deltas: &[(u32, Vec3)],
    new_prefix: &HalfEdgeMesh,
) -> (Vec<(u32, Vec3)>, usize) {
    let mut pts = Vec::with_capacity(old_deltas.len());
    let mut kept_src = Vec::with_capacity(old_deltas.len());
    for &(i, d) in old_deltas {
        let vid = VertexId(i);
        if !old_prefix.is_vert_live(vid) {
            continue;
        }
        pts.push(old_prefix.vert(vid).position + d);
        kept_src.push(d);
    }
    let (verts, indices) = half_edge_soup(new_prefix);
    let max_dist = {
        let mut e = 0.25f32;
        if !verts.is_empty() {
            let mut mn = verts[0];
            let mut mx = verts[0];
            for p in &verts {
                mn = mn.min(*p);
                mx = mx.max(*p);
            }
            e = (mx - mn).max_element().max(e) * 0.15;
        }
        e
    };
    let Some(proj) =
        io_bridge::mesh_to_sdf::project_points_to_mesh_filtered(&pts, &verts, &indices, max_dist, None)
    else {
        return (Vec::new(), old_deltas.len());
    };
    let mut orphans = old_deltas.len() - kept_src.len();
    let mut out: Vec<(u32, Vec3)> = Vec::new();
    for (k, p) in pts.iter().enumerate() {
        let q = proj[k];
        if (q - *p).length_squared() < 1e-12 {
            orphans += 1;
            continue;
        }
        let mut best = u32::MAX;
        let mut best_d = f32::MAX;
        for i in 0..new_prefix.vertices.len() as u32 {
            let vid = VertexId(i);
            if !new_prefix.is_vert_live(vid) {
                continue;
            }
            let d = (new_prefix.vert(vid).position - q).length_squared();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        if best == u32::MAX {
            orphans += 1;
            continue;
        }
        let base = new_prefix.vert(VertexId(best)).position;
        let delta = q - base;
        if let Some(e) = out.iter_mut().find(|(id, _)| *id == best) {
            e.1 += delta;
        } else {
            out.push((best, delta));
        }
    }
    let _ = kept_src;
    (out, orphans)
}

fn verts_in_radius_scan(mesh: &HalfEdgeMesh, center: Vec3, radius: f32) -> Vec<u32> {
    let r2 = radius.max(0.0) * radius.max(0.0);
    let mut out = Vec::new();
    for i in 0..mesh.vertices.len() as u32 {
        let vid = VertexId(i);
        if !mesh.is_vert_live(vid) {
            continue;
        }
        if (mesh.vert(vid).position - center).length_squared() <= r2 {
            out.push(i);
        }
    }
    out
}

impl AzraApp {
    /// Evaluate the Sculpt slot against the clone-once working mesh (C1).
    ///
    /// `prefix` is the mesh **before** this slot (Remesh / Smooth / base). Hits reuse the
    /// working mesh; a prefix or gen mismatch reclones. Never stores a Sculpt prefix.
    pub(crate) fn poly_sculpt_eval_working(
        &mut self,
        nid: NodeId,
        prefix: &Arc<HalfEdgeMesh>,
        prefix_hash: u64,
        m: &PolyModifier,
    ) -> HalfEdgeMesh {
        let prefix_ptr = Arc::as_ptr(prefix) as usize;
        // C1 OPEN-STROKE PIN (§7.4, K14): while a stroke is open on this body the live
        // working mesh IS the truth — gen mismatch is EXPECTED (PR-5 stopped bumping
        // `sculpt_delta_gen` per dab) and rebasing from stale `sculpt_deltas` here would
        // wipe the open stroke on any mid-stroke stack walk (flush fallback, reconcile,
        // prefix evict of another body).
        if self.poly_sculpt_stroke_pinned(nid) {
            if let Some(w) = self.poly_sculpt_working.get(&nid) {
                return w.mesh.clone();
            }
        }
        if let Some(w) = self.poly_sculpt_working.get(&nid) {
            if w.matches(prefix_ptr, prefix_hash, m.sculpt_delta_gen, prefix.as_ref()) {
                return w.mesh.clone();
            }
        }
        let mut deltas = m.sculpt_deltas.clone();
        let mut gen = m.sculpt_delta_gen;
        if let Some(w) = self.poly_sculpt_working.get(&nid) {
            if (w.prefix_ptr != prefix_ptr || w.prefix_hash != prefix_hash) && !deltas.is_empty() {
                let (rekeyed, orphans) = rebase_deltas(&w.prefix, &deltas, prefix.as_ref());
                if orphans > 0 {
                    self.toast(&format!("{orphans} PolySculpt deltas lost — remesh/smooth above this layer moved the surface"));
                }
                deltas = rekeyed;
                gen = gen.wrapping_add(1);
                if let Some(node) = self.scene.scene_graph.get_mut(nid) {
                    if let core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. } =
                        &mut node.payload
                    {
                        if let Some(slot) = modifiers.iter_mut().find(|mm| {
                            mm.enabled
                                && mm.mod_type == core_math::scene_graph::PolyModifierType::Sculpt
                        }) {
                            slot.sculpt_deltas = deltas.clone();
                            slot.sculpt_delta_gen = gen;
                        }
                    }
                }
            }
        }
        let w = PolySculptWorking::from_prefix(Arc::clone(prefix), &deltas, gen, prefix_hash);
        let out = w.mesh.clone();
        self.poly_sculpt_working.insert(nid, w);
        out
    }

    /// Stroke-sized delta edit on the working mesh. Patches display bytes in place (C2).
    /// Direct Surface Sculpt writes the body mesh; a Sculpt modifier (if Edit-armed) stores deltas.
    pub(crate) fn poly_sculpt_stamp(
        &mut self,
        nid: NodeId,
        center: Vec3,
        radius: f32,
        delta: Vec3,
    ) -> Vec<u32> {
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(core_math::scene_graph::NodePayload::PolygonalBody { .. }) => {}
            Some(_) => {
                self.toast("PolySculpt: select a polygonal body");
                return Vec::new();
            }
            None => {
                self.toast("PolySculpt: no poly body");
                return Vec::new();
            }
        }
        let slot = self.poly_sculpt_layer_slot(nid);
        self.ensure_poly_sculpt_working(nid);

        let t0 = std::time::Instant::now();
        let ids = {
            let Some(w) = self.poly_sculpt_working.get_mut(&nid) else {
                return Vec::new();
            };
            // P0-F (ZB_RESPONSIVE_SCULPT_PLAN §7.6, PR-1): the O(V) oracle scan is
            // DIAGNOSTIC ONLY — the grid is the production query. `AZRA_SCULPT_ORACLE=1`
            // re-enables it anywhere; debug builds keep it on small fixtures (≤50k verts)
            // so unit tests still cross-check grid vs scan. On a 323k remesh the scan was
            // a per-stamp O(V) tax on the hot path for an assert that never fires.
            let run_oracle = std::env::var_os("AZRA_SCULPT_ORACLE").is_some()
                || (cfg!(debug_assertions) && w.mesh.vertices.len() <= 50_000);
            if run_oracle {
                let tq = std::time::Instant::now();
                let mut ov = verts_in_radius_scan(&w.mesh, center, radius);
                w.last_query_ov_ms = tq.elapsed().as_secs_f32() * 1e3;
                let mut chk = w.grid.query(&w.mesh, center, radius);
                ov.sort_unstable();
                chk.sort_unstable();
                assert_eq!(ov, chk, "VertGrid query diverged from the O(V) oracle");
            }
            let tq = std::time::Instant::now();
            let ids = sculpt_radius_query(w, center, radius);
            w.last_query_grid_ms = tq.elapsed().as_secs_f32() * 1e3;
            ids
        };

        if ids.is_empty() {
            return Vec::new();
        }

        let Some(w) = self.poly_sculpt_working.get_mut(&nid) else {
            return Vec::new();
        };
        // PR-0 (ZB plan §11.2): split the stamp into the §19 metric bags. Timers are a
        // few ns; always-on (same contract as `last_stamp_ms`).
        let query_ms = w.last_query_grid_ms;
        let t_kernel = std::time::Instant::now();
        let mut seeds = Vec::with_capacity(ids.len());
        for &i in &ids {
            let vid = VertexId(i);
            if !w.mesh.is_vert_live(vid) {
                continue;
            }
            let old = w.mesh.vert(vid).position;
            w.first_touch.entry(i).or_insert(old);
            w.mesh.vert_mut(vid).position = old + delta;
            w.grid.move_vert(i, old, old + delta);
            seeds.push(vid);
        }
        let kernel_ms = t_kernel.elapsed().as_secs_f32() * 1e3;
        sculpt_refit_moved(w, &ids);
        let t_normals = std::time::Instant::now();
        let normals_mode = sculpt_region_normals(&mut w.mesh, &seeds);
        let normals_ms = t_normals.elapsed().as_secs_f32() * 1e3;
        let corners = write_display_corners(w, &ids);
        w.gen = w.gen.wrapping_add(1);
        w.last_stamp_ms = t0.elapsed().as_secs_f32() * 1e3;
        {
            let p = &mut self.sculpt.poly_profile;
            p.query_ms = query_ms;
            p.kernel_ms = kernel_ms;
            p.normals_ms = normals_ms;
            p.seed_count = seeds.len() as u32;
            p.corner_count = corners as u32;
            p.normals_mode = normals_mode;
            p.undo_bytes = (w.first_touch.len() * 16) as u64;
        }
        // PR-5 (§7.4): NO document write per dab. Direct mode's whole-mesh deep clone and
        // the layer mode's quadratic `sculpt_deltas.iter().find` are gone — the working
        // mesh is truth mid-stroke; `commit_episode` is the single document choke point.
        // The C1 open-stroke PIN (eval_working + mesh_sync) keeps display/eval coherent
        // while `w.gen` runs ahead of `m.sculpt_delta_gen`.
        let _ = slot;

        // GPU vert patch when a cache id is already allocated (headless tests skip this).
        self.poly_sculpt_flush_display(nid);
        ids
    }

    pub(crate) fn poly_sculpt_drop(&mut self, nid: NodeId) {
        self.poly_sculpt_working.remove(&nid);
    }

    pub(crate) fn poly_sculpt_radius(&self) -> f32 {
        if self.sculpt.poly_radius > 1e-4 {
            self.sculpt.poly_radius.clamp(0.01, 4.0)
        } else {
            0.15
        }
    }

    pub(crate) fn poly_sculpt_strength(&self) -> f32 {
        if self.sculpt.poly_strength > 1e-4 {
            self.sculpt.poly_strength.clamp(0.05, 2.0)
        } else {
            1.0
        }
    }

    pub(crate) fn poly_sculpt_enter_edit(
        &mut self,
        nid: core_math::scene_graph::NodeId,
        slot: usize,
    ) {
        if self.sculpt.voxel.is_some() {
            self.voxel_leave_edit();
        }
        self.arm_surface_sculpt_tools();
        self.select_single_unified(crate::SceneItemId::Node(nid));
        self.modeling_context = crate::ModelingContext::Polygonal;
        // Face/Edge/Vertex paint-select must not share LMB with the brush.
        self.poly_work_mode = crate::PolyWorkMode::Object;
        self.tool.work_mode = crate::WorkMode::Sculpt;
        if slot != POLY_SCULPT_DIRECT {
            self.ui.poly_mod_selected = Some((nid, slot));
        }
        self.sculpt.poly_edit = Some((nid, slot));
        if self.sculpt.poly_radius <= 1e-4 {
            self.sculpt.poly_radius = 0.15;
        }
        if self.sculpt.poly_strength <= 1e-4 {
            self.sculpt.poly_strength = 1.0;
        }
        if self.sculpt.poly_brush.is_none() {
            self.sculpt.poly_brush = Some(core_math::voxel_sculpt::VoxelBrush::Draw);
        }
        self.ui2.active_tool = "Sculpt".into();
        self.ensure_poly_sculpt_working(nid);
        // Direct Surface: bake live Remesh/Smooth into the body now so the remesh
        // surface is what you sculpt. Do not `poly_sync_mesh_cache` here — that
        // re-runs remesh, COWs the GPU cid, and MESH dirties `poly_sync_all_meshes`,
        // which is the "poly surface goes away" class.
        if slot == POLY_SCULPT_DIRECT {
            self.poly_sculpt_write_direct(nid);
        }
        let toast = if slot == POLY_SCULPT_DIRECT {
            "Surface sculpt: LMB stroke  ·  Z/X size  ·  Shift Smooth  ·  Ctrl invert  ·  writes the mesh"
        } else {
            "Surface sculpt (layer): LMB stroke  ·  Z/X size  ·  Shift Smooth  ·  Ctrl invert"
        };
        self.toast(toast);
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Add an enabled Sculpt slot if the body has none. Returns its stack index.
    pub(crate) fn ensure_poly_sculpt_slot(
        &mut self,
        nid: core_math::scene_graph::NodeId,
    ) -> Option<usize> {
        use core_math::scene_graph::{NodePayload, PolyModifier, PolyModifierType};
        let existing = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers
                .iter()
                .rposition(|m| m.enabled && m.mod_type == PolyModifierType::Sculpt),
            _ => None,
        });
        if existing.is_some() {
            return existing;
        }
        let n = self.scene.scene_graph.get_mut(nid)?;
        let NodePayload::PolygonalBody { modifiers, .. } = &mut n.payload else {
            return None;
        };
        modifiers.push(PolyModifier::new(PolyModifierType::Sculpt));
        Some(modifiers.len() - 1)
    }

    /// Sculpt-room LMB is the brush. Paint/box/region select must not start on that button.
    pub(crate) fn sculpt_brush_owns_lmb(&self) -> bool {
        self.tool.work_mode == crate::WorkMode::Sculpt
            && !self.tool.current.is_gizmo()
            && !self.tool.current.is_transform()
    }

    /// Drop G/R/S and any live transform so LMB is a sculpt stroke, not a gizmo drag.
    fn arm_surface_sculpt_tools(&mut self) {
        self.drop_grs_for_tool_switch();
        if self.tool.current.is_gizmo() || self.tool.current.is_transform() {
            let _ = self.cancel_operation();
        }
        if !self.tool.current.is_idle()
            && !self.tool.current.is_create()
        {
            self.tool.current = crate::ActiveTool::Idle;
        }
        self.poly_tool = crate::PolyTool::Idle;
    }

    /// 3D-Coat Surface: sculpt the selected poly mesh or switch a voxel to surface mode.
    pub(crate) fn begin_surface_sculpt(&mut self, nid: core_math::scene_graph::NodeId) {
        use core_math::scene_graph::NodePayload;
        match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(NodePayload::PolygonalBody { .. }) => {
                // Direct mesh sculpt — do not add a Sculpt modifier. The modifier is only
                // for the optional non-destructive layer (Edit on the modifier card).
                // Do not open a session-long undo episode here: one snapshot at enter
                // would make Ctrl+Z drop every stroke. Each LMB stroke begins its own.
                self.poly_sculpt_enter_edit(nid, POLY_SCULPT_DIRECT);
            }
            Some(NodePayload::VoxelBody { .. }) => {
                self.voxel_enter_edit(nid);
                self.arm_surface_sculpt_tools();
                self.tool.work_mode = crate::WorkMode::Sculpt;
                self.ui2.active_tool = "Sculpt".into();
                // Must go through the mode toggle — flipping the bool alone leaves GPU-resident
                // chunks without a CPU mesh, so Surface brushes have nothing to displace.
                if self.sculpt.voxel.as_ref().map_or(true, |v| !v.surface_mode) {
                    self.voxel_surface_set_mode(true);
                } else {
                    self.toast("Surface mode — brushes displace the mesh directly");
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                }
            }
            _ => {
                self.toast("Surface sculpt: select a polygon mesh or a voxel object");
            }
        }
    }

    /// Active / selected node → Surface sculpt. Used by the pie, palette, and outliner.
    pub(crate) fn begin_surface_sculpt_on_selection(&mut self) {
        if let Some(nid) = self.poly_active_body() {
            self.begin_surface_sculpt(nid);
            return;
        }
        if let Some(nid) = self.sel.usel.active_node() {
            self.begin_surface_sculpt(nid);
            return;
        }
        self.toast("Surface sculpt: select a mesh first");
    }

    pub(crate) fn poly_sculpt_flush_display(&mut self, nid: core_math::scene_graph::NodeId) {
        let t_up = std::time::Instant::now();
        // PR-3 (§7.3): drain the stroke's dirty corners into a scatter patch — bytes ∝
        // moved corners. The full-clone path below survives ONLY as the fallback for a
        // missing/count-changed baseline (genuine topology change / first frame).
        let cid_for_patch = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            core_math::scene_graph::NodePayload::PolygonalBody { mesh_cache_id, .. } => {
                *mesh_cache_id
            }
            _ => None,
        });
        if let (Some(cid), Some(w)) = (cid_for_patch, self.poly_sculpt_working.get_mut(&nid)) {
            // Empty dirty set + a valid baseline = NOTHING to upload. `patch_chunk_verts`
            // with zero patches returns true exactly when the baseline is intact, so a
            // redundant flush (double-call per stamp, idle frames) costs ~a hash lookup —
            // it used to take the full-clone fallback and re-upload 20.7 MB for nothing.
            if !w.display_verts.is_empty() {
                w.dirty_corners.sort_unstable();
                w.dirty_corners.dedup();
                let patches: Vec<(u32, [f32; 8])> = w
                    .dirty_corners
                    .iter()
                    .filter(|&&ci| (ci as usize) < w.display_verts.len())
                    .map(|&ci| (ci, w.display_verts[ci as usize]))
                    .collect();
                let queue_ok = self.gpu.is_some();
                let queue = self.gpu.as_ref().map(|g| g.queue.as_ref());
                let patched = self
                    .cache
                    .mesh_cache
                    .patch_chunk_verts(cid, &patches, queue);
                if !patched && std::env::var_os("AZRA_POLYSCULPT_PERF").is_some() {
                    eprintln!(
                        "[PolySculptPerf] patch REFUSED: cid={cid:?} display={} persistent={:?} raster={:?}",
                        self.poly_sculpt_working.get(&nid).map(|w| w.display_verts.len()).unwrap_or(0),
                        self.cache.mesh_cache.debug_persistent_len(cid),
                        self.cache.mesh_cache.debug_raster_len(cid),
                    );
                }
                if patched {
                    if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
                        w.dirty_corners.clear();
                    }
                    let _ = queue_ok;
                    self.sculpt.poly_profile.upload_ms =
                        t_up.elapsed().as_secs_f32() * 1e3;
                    self.sculpt.poly_profile.upload_bytes =
                        (patches.len() * std::mem::size_of::<[f32; 8]>()) as u64;
                    self.mark_dirty(crate::DirtyFlags::RENDER);
                    return;
                }
            }
        }
        let verts = match self.poly_sculpt_working.get_mut(&nid) {
            Some(w) if !w.display_verts.is_empty() => {
                w.dirty_corners.clear();
                w.display_verts.clone()
            }
            _ => {
                self.poly_sync_mesh_cache(nid);
                return;
            }
        };
        let upload_bytes = (verts.len() * std::mem::size_of::<[f32; 8]>()) as u64;
        let cid = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            core_math::scene_graph::NodePayload::PolygonalBody {
                mesh_cache_id, ..
            } => *mesh_cache_id,
            _ => None,
        });
        if let Some(cid) = cid {
            if self
                .cache
                .mesh_cache
                .can_update_verts_in_place(cid, verts.len())
                && self
                    .cache
                    .mesh_cache
                    .update_chunk_verts_in_place(cid, verts)
            {
                self.sculpt.poly_profile.upload_ms = t_up.elapsed().as_secs_f32() * 1e3;
                self.sculpt.poly_profile.upload_bytes = upload_bytes;
                self.mark_dirty(crate::DirtyFlags::RENDER);
                return;
            }
        }
        self.sculpt.poly_profile.upload_ms = t_up.elapsed().as_secs_f32() * 1e3;
        self.sculpt.poly_profile.upload_bytes = upload_bytes;
        // Do not `poly_sync_mesh_cache` here. That walk wraps the remesh/base prefix in a
        // new Arc, misses the working-mesh pointer, rebases, and can orphan the stroke
        // that just wrote these verts (headless tests + live remesh+sculpt).
        self.mark_dirty(crate::DirtyFlags::RENDER | crate::DirtyFlags::MESH);
    }

    /// Idle click-select / paint-select — not a sculpt brush and not a create tool.
    pub(crate) fn arm_default_selection_tool(&mut self) {
        self.set_mode(crate::ToolMode::Select);
        self.poly_tool = crate::PolyTool::Idle;
        if Self::active_tool_is_sculpt_brush(self.ui2.active_tool.as_str()) {
            self.ui2.active_tool.clear();
        }
    }

    fn active_tool_is_sculpt_brush(name: &str) -> bool {
        matches!(
            name,
            "Sculpt"
                | "Draw"
                | "Erase"
                | "Smooth"
                | "Flatten"
                | "Clay"
                | "ClayBuildup"
                | "Scrape"
                | "Crease"
                | "Fill"
                | "Inflate"
                | "Pinch"
                | "Move"
                | "Grab"
                | "Mask"
                | "Smudge"
                | "SnakeHook"
                | "Build"
        )
    }

    pub(crate) fn poly_sculpt_leave_edit(&mut self) {
        if self.sculpt.poly_edit.is_none() {
            return;
        }
        if self.sculpt.stroke_active {
            self.commit_episode();
            self.sculpt.stroke_active = false;
        }
        self.sdf_stroke_interpolator.reset();
        self.sculpt.poly_stroke_last = None;
        self.sculpt.poly_cursor_hit = None;
        self.sculpt.poly_edit = None;
        if self.tool.work_mode == crate::WorkMode::Sculpt && self.sculpt.voxel.is_none() {
            self.tool.work_mode = crate::WorkMode::Object;
        }
        self.arm_default_selection_tool();
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }

    /// Active polygonal body that Surface Sculpt can stroke. No Sculpt modifier required.
    pub(crate) fn poly_sculpt_target(&self) -> Option<NodeId> {
        self.poly_active_body()
    }

    fn poly_sculpt_is_direct(&self, nid: NodeId) -> bool {
        matches!(
            self.sculpt.poly_edit,
            Some((e, POLY_SCULPT_DIRECT)) if e == nid
        )
    }

    /// Enabled Sculpt-layer slot when that layer owns the stroke. `None` = write the mesh.
    fn poly_sculpt_layer_slot(&self, nid: NodeId) -> Option<usize> {
        use core_math::scene_graph::{NodePayload, PolyModifierType};
        if self.poly_sculpt_is_direct(nid) {
            return None;
        }
        if let Some((e, slot)) = self.sculpt.poly_edit {
            if e == nid && slot != POLY_SCULPT_DIRECT {
                return Some(slot);
            }
        }
        let node = self.scene.scene_graph.get(nid)?;
        match &node.payload {
            NodePayload::PolygonalBody { modifiers, .. } => modifiers
                .iter()
                .rposition(|m| m.enabled && m.mod_type == PolyModifierType::Sculpt),
            _ => None,
        }
    }

    /// Clone-once working mesh from the current evaluated surface (Remesh output or base).
    pub(crate) fn ensure_poly_sculpt_working(&mut self, nid: NodeId) {
        if self.poly_sculpt_working.contains_key(&nid) {
            return;
        }
        let empty = HashMap::new();
        let Some(base) = self.poly_node_mesh(nid) else {
            return;
        };
        let mods = match self.scene.scene_graph.get(nid).map(|n| &n.payload) {
            Some(core_math::scene_graph::NodePayload::PolygonalBody { modifiers, .. }) => {
                modifiers.clone()
            }
            _ => return,
        };
        let eval = self.apply_poly_modifiers_cached(nid, base.as_ref(), &mods, &empty, None);
        if self.poly_sculpt_working.contains_key(&nid) {
            return;
        }
        let w = PolySculptWorking::from_prefix(eval, &[], 0, 0);
        self.poly_sculpt_working.insert(nid, w);
    }

    /// Persist a direct (no-modifier) stroke into the polygonal body.
    /// Same-topology: copy positions. Topology-diverged working (enter-time remesh bake
    /// OR a Dyntopo stroke — commit_episode's topo branch): replace the authored mesh and
    /// turn Remesh/Smooth off so they do not rebuild under the brush.
    pub(crate) fn poly_sculpt_write_direct(&mut self, nid: NodeId) {
        use core_math::scene_graph::{NodePayload, PolyModifierType};
        let Some(src) = self.poly_sculpt_working.get(&nid).map(|w| w.mesh.clone()) else {
            return;
        };
        let mut disabled = 0usize;
        if let Some(node) = self.scene.scene_graph.get_mut(nid) {
            if let NodePayload::PolygonalBody {
                mesh, modifiers, ..
            } = &mut node.payload
            {
                let same = mesh.vertex_count() == src.vertex_count()
                    && mesh.face_count() == src.face_count();
                if same {
                    let m = std::sync::Arc::make_mut(mesh);
                    let n = src.vertices.len().min(m.vertices.len()) as u32;
                    for i in 0..n {
                        let vid = VertexId(i);
                        if src.is_vert_live(vid) && m.is_vert_live(vid) {
                            m.vert_mut(vid).position = src.vert(vid).position;
                            m.vert_mut(vid).normal = src.vert(vid).normal;
                        }
                    }
                } else {
                    *mesh = std::sync::Arc::new(src);
                    for m in modifiers.iter_mut() {
                        if m.enabled
                            && matches!(
                                m.mod_type,
                                PolyModifierType::Remesh | PolyModifierType::Smooth
                            )
                        {
                            m.enabled = false;
                            disabled += 1;
                        }
                    }
                }
            }
        }
        if disabled > 0 {
            if let Some(body) = self.poly_prefix_cache.remove(&nid) {
                self.poly_prefix_total_bytes = self
                    .poly_prefix_total_bytes
                    .saturating_sub(body.total_bytes);
            }
            self.bump_poly_edit_generation();
            self.toast(
                "Surface sculpt writes the mesh — Remesh/Smooth off so they do not rebuild under the brush",
            );
        }
    }

    /// Same dab record the voxel surface path builds from interpolator samples (C3).
    pub(crate) fn poly_sculpt_dabs_from_samples(
        pts: &[core_math::sculpt_layer::StrokeSample],
        radius: f32,
        strength: f32,
        stroke_tan: Vec3,
    ) -> Vec<crate::voxel_surface_sculpt::SurfaceDab> {
        pts.iter()
            .map(|pt| crate::voxel_surface_sculpt::SurfaceDab {
                pos: pt.position,
                dir: pt.normal.normalize_or_zero(),
                radius: (radius * pt.radius_scale).max(1e-4),
                strength: strength * pt.pressure,
                stroke_tan,
                tip_rotation: pt.tip_rotation,
            })
            .collect()
    }

    /// Apply Draw / Smooth (and S3 siblings) through `displace_verts_for_dabs`.
    pub(crate) fn poly_sculpt_apply_dabs(
        &mut self,
        nid: NodeId,
        dabs: &[crate::voxel_surface_sculpt::SurfaceDab],
        brush: core_math::voxel_sculpt::VoxelBrush,
        sgn: f32,
    ) -> usize {
        if dabs.is_empty() {
            return 0;
        }
        let hardness = self.poly_sculpt_hardness();
        let preset = self.sculpt.poly_tool.falloff_preset;
        // Accumulate-OFF is the poly default (Blender): the normal-deposit brushes build ONE
        // offset layer per stroke through the working's deposit ledger; the existing Brush
        // Settings "Accumulate" checkbox (previously dead in poly mode) restores stacking.
        // Plane brushes converge to their area plane and ignore the ledger by design.
        let layer_mode = !self.sculpt.poly_tool.accumulate
            && matches!(
                brush,
                core_math::voxel_sculpt::VoxelBrush::Draw
                    | core_math::voxel_sculpt::VoxelBrush::Erase
                    | core_math::voxel_sculpt::VoxelBrush::Inflate
            );
        self.ensure_poly_sculpt_working(nid);
        // Blender-tech Dyntopo: refine topology under the dabs BEFORE displacement so
        // this very stamp already sculpts the refined verts (Blender's order).
        if self.sculpt.dyn_topo {
            self.poly_dyntopo_refine_for_dabs(nid, dabs);
        }
        let Some(w) = self.poly_sculpt_working.get_mut(&nid) else {
            return 0;
        };
        let r_max = dabs.iter().map(|d| d.radius).fold(0.0f32, f32::max);
        // Plane-relative brushes (same set as the voxel dispatch, voxel_surface_sculpt.rs:617)
        // fit their area frame over Blender's `normal_radius_factor` sub-sphere, which is WIDER
        // than the displacement footprint — widen the vert query to match. The kernel still
        // displaces only inside `d.radius`, so the extra verts are frame samples, never moved.
        let plane_brush = matches!(
            brush,
            core_math::voxel_sculpt::VoxelBrush::Clay
                | core_math::voxel_sculpt::VoxelBrush::ClayBuildup
                | core_math::voxel_sculpt::VoxelBrush::Scrape
                | core_math::voxel_sculpt::VoxelBrush::Crease
        );
        // BLENDER-EXACT deposit direction (calc_sculpt_normal, sculpt.cc v3.6): Draw
        // displaces along the AREA normal of the brush footprint (the
        // normal_radius_factor sub-sphere), never the raw per-dab pick normal — a noisy
        // pick normal scatters deposit directions dab-to-dab, which is the
        // "strokes break the surface" class (owner 2026-08-19).
        let dir_from_area = matches!(
            brush,
            core_math::voxel_sculpt::VoxelBrush::Draw | core_math::voxel_sculpt::VoxelBrush::Erase
        );
        // Draw's normal disc is TIGHT — Blender DNA default `normal_radius_factor = 0.5`
        // (DNA_brush_defaults.h, v3.6; only the clay family overrides upward). Reusing the
        // plane brushes' 1.35 here made Draw's deposit direction average geometry far
        // outside the brush — the owner's "surface reacts weirdly to sculpting" (a stroke
        // near curvature leaned toward distant faces). Plane brushes keep their wider,
        // oracle-gated factor.
        const DRAW_NORMAL_RADIUS_FACTOR: f32 = 0.5;
        let query_factor = if plane_brush {
            crate::voxel_surface_sculpt::SURFACE_AREA_NORMAL_FACTOR
        } else {
            1.0 // Draw's 0.5 normal disc sits inside the displacement footprint
        };
        let mut id_set = std::collections::HashSet::new();
        for d in dabs {
            for i in sculpt_radius_query(w, d.pos, d.radius * query_factor) {
                id_set.insert(i);
            }
        }
        if id_set.is_empty() {
            return 0;
        }
        let mut ids: Vec<u32> = id_set.into_iter().collect();
        ids.sort_unstable();
        // BLENDER `use_original` (calc_area_normal, v4.2: `!ss.cache->accum` → sample
        // ORIGINAL data): with Accumulate OFF, every normal the brush math reads —
        // area-frame accumulation (Draw/Erase dir, Clay/Scrape/Crease planes) and
        // Inflate's per-vertex direction — is the STROKE-START normal, frozen on first
        // sight of the vert. The live normals are recomputed after every batch for
        // display (`sculpt_region_normals`), so reading them here fed each dab the
        // normals the PREVIOUS dab produced — a feedback loop that scatters deposit
        // directions along the stroke (owner 2026-08-23: "picking normal from their own
        // result"). Positions stay LIVE (displacement input; Blender also freezes the
        // area CENTER — documented divergence, revisit if plane brushes still drift).
        let use_original = !self.sculpt.poly_tool.accumulate;
        let mut packed: Vec<[f32; 8]> = Vec::with_capacity(ids.len());
        let mut old_pos: Vec<Vec3> = Vec::with_capacity(ids.len());
        for &i in &ids {
            let (pos, live_n) = {
                let v = w.mesh.vert(VertexId(i));
                (v.position, v.normal)
            };
            let n = if use_original {
                *w.first_touch_normal.entry(i).or_insert(live_n)
            } else {
                live_n
            };
            old_pos.push(pos);
            packed.push([pos.x, pos.y, pos.z, n.x, n.y, n.z, 0.0, 0.0]);
        }
        if matches!(brush, core_math::voxel_sculpt::VoxelBrush::Smooth) {
            if sgn < 0.0 {
                // BLENDER (sculpt_smooth.cc `do_smooth_brush` dispatcher, v4.2): inverted
                // Smooth is ENHANCE DETAILS, never a negative Laplacian step. The old
                // `mix * sgn` ran the Jacobi pass EXPANSIVELY — each pass amplified the
                // noise the previous one created (owner 2026-08-23: "smooth with
                // subtract mode gives weird result"). Enhance displaces along a
                // direction field FROZEN at its first step of the stroke
                // (`detail_directions[i] = neighbor_avg − co`), which is what makes the
                // inverse stable.
                if w
                    .enhance_dirs
                    .as_ref()
                    .map_or(true, |d| d.len() != w.mesh.vertices.len())
                {
                    w.enhance_dirs = Some(compute_enhance_dirs(&w.mesh));
                }
                let dirs = w.enhance_dirs.as_deref().unwrap_or(&[]);
                poly_enhance_details_one_ring(
                    &w.mesh, &ids, &old_pos, dabs, hardness, preset, dirs, &mut packed,
                );
            } else {
                // Mesh 1-ring Laplacian, NOT the voxel spatial-hash average. Averaging
                // every vert inside the brush pulls sparse remesh verts into dense
                // clusters and stretches triangles into poles.
                poly_smooth_one_ring(&w.mesh, &ids, &old_pos, dabs, hardness, preset, &mut packed);
            }
        } else {
            // Real area planes for the plane-relative brushes — the SAME pure fit the voxel
            // surface path uses (`accumulate_area_frames` + `resolve_area_frame`), fed from the
            // widened footprint packed above. The fail-closed contract carries over: `None`
            // (off-mesh probe, or trench walls whose normals cancel below
            // SURFACE_MIN_AREA_NORMAL) makes the kernel SKIP that dab rather than converge
            // verts onto a garbage plane. Before 2026-08-18 this passed all-None, and the same
            // gate made Clay / ClayBuildup / Scrape SILENT NO-OPS in poly sculpt
            // (POLYSCULPT-PLANELESS).
            let planes: Vec<Option<(Vec3, Vec3)>> = if plane_brush || dir_from_area {
                let probes: Vec<(Vec3, f32)> = dabs.iter().map(|d| (d.pos, d.radius)).collect();
                let refs: Vec<Vec3> = dabs.iter().map(|d| d.dir.normalize_or_zero()).collect();
                let mut acc: Vec<(Vec3, Vec3, u32)> =
                    vec![(Vec3::ZERO, Vec3::ZERO, 0); dabs.len()];
                crate::voxel_surface_sculpt::accumulate_area_frames(
                    &mut acc,
                    &packed,
                    &probes,
                    &refs,
                    if plane_brush {
                        crate::voxel_surface_sculpt::SURFACE_AREA_NORMAL_FACTOR
                    } else {
                        DRAW_NORMAL_RADIUS_FACTOR
                    },
                );
                acc.iter()
                    .map(|&(sp, sn, c)| crate::voxel_surface_sculpt::resolve_area_frame(sp, sn, c))
                    .collect()
            } else {
                dabs.iter().map(|_| None).collect()
            };
            // Override Draw/Erase deposit dirs with the resolved area normal; fail OPEN
            // to the pick normal when no stable frame exists (off-mesh probe) — a missed
            // frame must soften direction quality, never kill the dab.
            let dabs_eff: Vec<crate::voxel_surface_sculpt::SurfaceDab>;
            let dabs_kernel: &[crate::voxel_surface_sculpt::SurfaceDab] = if dir_from_area {
                dabs_eff = dabs
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let mut d = *d;
                        if let Some((_, n)) = planes[i] {
                            let n = n.normalize_or_zero();
                            if n.length_squared() > 1e-10 {
                                d.dir = n;
                            }
                        }
                        d
                    })
                    .collect();
                &dabs_eff
            } else {
                dabs
            };
            crate::voxel_surface_sculpt::displace_verts_for_dabs(
                &mut packed,
                dabs_kernel,
                brush,
                sgn,
                hardness,
                preset,
                &planes,
                None,
                (r_max * 0.35).max(1e-4),
                // Brush tip alpha (Photoshop/ZBrush brush shape) — poly stamps sample the
                // SAME CPU image the voxel paths use (disjoint field borrow vs `w`).
                self.sculpt.poly_tip.as_ref().map(|t| (t, self.sculpt.poly_tool.tip_contrast)),
                if layer_mode {
                    Some((ids.as_slice(), &mut w.deposit_applied))
                } else {
                    None
                },
            );
        }
        let mut seeds = Vec::new();
        let mut delta_add: Vec<(u32, Vec3)> = Vec::new();
        for (k, &i) in ids.iter().enumerate() {
            let np = Vec3::new(packed[k][0], packed[k][1], packed[k][2]);
            // FAIL-CLOSED FINITE GUARD: a non-finite position passes the delta filter below
            // (NaN comparisons are false), poisons the vert, AND parks it in VertGrid bucket
            // (0,0,0) forever (NaN as-i32 saturates to 0) — invisible to every later radius
            // query and ray pick: the PERMANENT dead-zone class. Skip the write; debug builds
            // assert so the producing brush is caught upstream, never papered over.
            if !np.is_finite() {
                debug_assert!(
                    np.is_finite(),
                    "poly sculpt dab produced non-finite position (brush {brush:?})"
                );
                continue;
            }
            let dlt = np - old_pos[k];
            if dlt.length_squared() < 1e-16 {
                continue;
            }
            let vid = VertexId(i);
            w.first_touch.entry(i).or_insert(old_pos[k]);
            w.mesh.vert_mut(vid).position = np;
            w.grid.move_vert(i, old_pos[k], np);
            seeds.push(vid);
            delta_add.push((i, dlt));
        }
        if seeds.is_empty() {
            return 0;
        }
        {
            let moved_ids_r: Vec<u32> = delta_add.iter().map(|(i, _)| *i).collect();
            sculpt_refit_moved(w, &moved_ids_r);
        }
        let t_normals = std::time::Instant::now();
        let normals_mode = sculpt_region_normals(&mut w.mesh, &seeds);
        self.sculpt.poly_profile.normals_ms = t_normals.elapsed().as_secs_f32() * 1e3;
        self.sculpt.poly_profile.normals_mode = normals_mode;
        // PR-2 / G2 single-writer: the forked corner scan this block used to carry is
        // deleted — same helper as stamp and Grab.
        let moved_ids: Vec<u32> = delta_add.iter().map(|(i, _)| *i).collect();
        write_display_corners(w, &moved_ids);
        w.gen = w.gen.wrapping_add(1);
        // PR-5 (§7.4): no per-dab document write — see poly_sculpt_stamp's tail.
        self.poly_sculpt_flush_display(nid);
        moved_ids.len()
    }

    /// Grab: translate verts in the stamp by `delta` (Move analog; not in displace_verts).
    ///
    /// LEGACY UNIFORM path — kept for callers that genuinely want a rigid translation of every
    /// vert in the radius. It is NOT what the Grab brush uses: `poly_sculpt_stamp` applies the
    /// same `delta` to every vert with no falloff, which moves a hard-edged disc. Blender's Grab
    /// is `poly_grab_begin` / `poly_grab_update` below.
    pub(crate) fn poly_sculpt_apply_grab(&mut self, nid: NodeId, center: Vec3, radius: f32, delta: Vec3) -> usize {
        self.poly_sculpt_stamp(nid, center, radius, delta).len()
    }

    /// ANCHOR a Grab stroke: freeze the affected verts, their pre-stroke positions and their
    /// falloff weights (Blender captures `orig_data` once per stroke for exactly this reason).
    ///
    /// `hit_local` is the surface point under the cursor at stroke start; `hit_world` anchors the
    /// drag plane. Returns false when nothing is in range, so the caller can skip the stroke.
    pub(crate) fn poly_grab_begin(
        &mut self,
        nid: NodeId,
        hit_local: Vec3,
        hit_world: Vec3,
        radius: f32,
    ) -> bool {
        self.ensure_poly_sculpt_working(nid);
        let hardness = self.poly_sculpt_hardness();
        // Symmetry (owner 2026-08-18: "move tool does not work with symmetry"): mirror the
        // anchor across each enabled LOCAL axis plane — the same octant expansion the stamp
        // path uses — and carry each instance's ±1 sign mask so its offset is mirrored too.
        let sym = self.sculpt.poly_symmetry;
        let mut insts: Vec<(Vec3, Vec3)> = vec![(hit_local, Vec3::ONE)];
        for axis in 0..3 {
            if !sym[axis] {
                continue;
            }
            let mirrored: Vec<(Vec3, Vec3)> = insts
                .iter()
                .map(|&(a, m)| {
                    let mut aa = a.to_array();
                    let mut mm = m.to_array();
                    aa[axis] = -aa[axis];
                    mm[axis] = -mm[axis];
                    (Vec3::from_array(aa), Vec3::from_array(mm))
                })
                .collect();
            insts.extend(mirrored);
        }
        let Some(w) = self.poly_sculpt_working.get_mut(&nid) else {
            return false;
        };
        let inv_r = 1.0 / radius.max(1e-6);
        // vert → (orig, Σ w·mask, Σw, max_w). Overlapping instances (verts near a mirror
        // plane) BLEND: weight = Σ(w·mask)·(max_w/Σw), so an on-plane vert loses its
        // cross-plane component (stays on the plane) instead of doubling or splitting,
        // while single-instance verts keep exactly the classic w·(1,1,1).
        let mut acc: std::collections::HashMap<u32, (Vec3, Vec3, f32, f32)> =
            std::collections::HashMap::new();
        for &(anchor, mask) in &insts {
            let ids = sculpt_radius_query(w, anchor, radius);
            for &i in &ids {
                let vid = VertexId(i);
                if !w.mesh.is_vert_live(vid) {
                    continue;
                }
                let p = w.mesh.vert(vid).position;
                // `e` = inside-ness: 1 at the anchor, 0 at the rim — the convention
                // `falloff_weight_preset` expects. Reusing the sculpt room's curve (with the
                // global hardness) is what makes Grab feel like the other brushes rather
                // than a second, subtly-different falloff.
                let e = 1.0 - (p - anchor).length() * inv_r;
                if e <= 0.0 {
                    continue;
                }
                let f = core_math::voxel_sculpt::falloff_weight(e.clamp(0.0, 1.0), hardness);
                if f <= 1.0e-4 {
                    continue;
                }
                let a = acc.entry(i).or_insert((p, Vec3::ZERO, 0.0, 0.0));
                a.1 += mask * f;
                a.2 += f;
                a.3 = a.3.max(f);
            }
        }
        if acc.is_empty() {
            return false;
        }
        // HashMap iteration is random — sort for a deterministic frozen set.
        let mut items: Vec<(u32, (Vec3, Vec3, f32, f32))> = acc.into_iter().collect();
        items.sort_unstable_by_key(|(i, _)| *i);
        let mut verts = Vec::with_capacity(items.len());
        let mut orig = Vec::with_capacity(items.len());
        let mut weight = Vec::with_capacity(items.len());
        for (i, (p, wm, wsum, wmax)) in items {
            verts.push(i);
            orig.push(p);
            weight.push(wm * (wmax / wsum.max(1e-6)));
        }
        // PR-6 (§7.5): Grab is a first-class document consumer — the frozen anchor set
        // IS the episode's first-touch set, seeded here so release commits it.
        if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
            for (v, o) in verts.iter().zip(orig.iter()) {
                w.first_touch.entry(*v).or_insert(*o);
            }
        }
        self.sculpt.poly_grab = Some(crate::sculpt_state::PolyGrabAnchor {
            nid,
            verts,
            orig,
            weight,
            anchor_world: hit_world,
        });
        true
    }

    /// Drive an anchored Grab from an ABSOLUTE world offset (current drag position minus the
    /// anchor). Idempotent by construction: every frame recomputes `orig + offset * weight` from
    /// the frozen originals, so re-sending the same offset is a no-op and returning the cursor to
    /// the anchor restores the surface exactly.
    pub(crate) fn poly_grab_update(&mut self, offset_world: Vec3) -> usize {
        let Some(anchor) = self.sculpt.poly_grab.take() else {
            return 0;
        };
        let nid = anchor.nid;
        let world = self.scene.scene_graph.world_transform(nid);
        // World → local direction (no translation): the offset is a displacement, not a point.
        let offset_local = world.inverse().transform_vector3(offset_world);
        let mut seeds: Vec<VertexId> = Vec::with_capacity(anchor.verts.len());
        if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
            for k in 0..anchor.verts.len() {
                let i = anchor.verts[k];
                let vid = VertexId(i);
                if !w.mesh.is_vert_live(vid) {
                    continue;
                }
                let old = w.mesh.vert(vid).position;
                let new = anchor.orig[k] + offset_local * anchor.weight[k];
                if (new - old).length_squared() > 1.0e-14 {
                    w.mesh.vert_mut(vid).position = new;
                    w.grid.move_vert(i, old, new);
                }
                seeds.push(vid);
            }
            let moved_ids: Vec<u32> = seeds.iter().map(|v| v.0).collect();
            sculpt_refit_moved(w, &moved_ids);
            let t_normals = std::time::Instant::now();
            let mode = sculpt_region_normals(&mut w.mesh, &seeds);
            self.sculpt.poly_profile.normals_ms = t_normals.elapsed().as_secs_f32() * 1e3;
            self.sculpt.poly_profile.normals_mode = mode;
        }
        let n = seeds.len();
        if let Some(w) = self.poly_sculpt_working.get_mut(&nid) {
            write_display_corners(w, &anchor.verts);
            w.gen = w.gen.wrapping_add(1);
        }
        self.poly_sculpt_flush_display(nid);
        self.sculpt.poly_grab = Some(anchor);
        n
    }

    /// Global sculpt hardness (owner 2026-08-14: falloff is a GLOBAL sculpt setting, not per
    /// brush). Single accessor so every brush reads the same curve.
    pub(crate) fn poly_sculpt_hardness(&self) -> f32 {
        self.sculpt.poly_hardness.clamp(0.0, 1.0)
    }

    /// Blender `paint_cursor` parity (owner 2026-08-19 "the brush circle still does not
    /// pick up the surface"): raycast the poly surface every HOVER frame and cache
    /// `(local pos, normal)` so `draw_brush_preview` draws the ring ON the mesh with the
    /// surface orientation — Blender's `SCULPT_cursor_geometry_info_update` does exactly
    /// this per mouse move. Uses the SAME budgeted pick as the stroke (GPU crop → DDA).
    pub(crate) fn poly_update_cursor_hit(&mut self, response: &egui::Response, rect: egui::Rect) {
        let Some(nid) = self
            .sculpt
            .poly_edit
            .map(|(n, _)| n)
            .or_else(|| self.poly_sculpt_target())
        else {
            self.sculpt.poly_cursor_hit = None;
            return;
        };
        let Some(mp) = response.hover_pos() else {
            self.sculpt.poly_cursor_hit = None;
            return;
        };
        if !rect.contains(mp) {
            self.sculpt.poly_cursor_hit = None;
            return;
        }
        let (cam, rd) = self.cam_ray(mp, rect);
        self.ensure_poly_sculpt_working(nid);
        self.sculpt.poly_cursor_hit = self
            .poly_sculpt_pick_hit(nid, cam, rd, Some((mp, rect)))
            .map(|h| (h.local, h.normal));
    }

    /// Ctrl+R Sculpt Remesh for a POLY body (owner 2026-08-19): one-shot voxel remesh of
    /// the AUTHORED mesh, installed in place (same NodeId) exactly like Apply-modifier —
    /// `poly_release_shared_display_arcs` + `poly_install_rebuilt_mesh` inside ONE
    /// `transaction`, so Ctrl+Z restores the pre-remesh mesh. Also the repair path for a
    /// sculpt-collapsed region: the remesh rebuilds clean, even topology over it.
    ///
    /// The sculpt working mesh is DROPPED afterward — remesh changes topology, and the
    /// working/grid/ledger all key on vert ids (the same-topology contract). The next
    /// stroke rebuilds a fresh working from the new mesh.
    /// Per-frame Frame Budget for dyntopo Euler ops (splits + face splits + collapses).
    /// Sized by the WORST op: a collapse still costs an O(mesh) half-edge scan
    /// (`merge_vertices_to_ex`, DYNTOPO_PLAN B2 localizes it) — 800 bounds a pathological
    /// all-collapse frame on the owner's 161k fixture to ~one frame's worth of scans,
    /// while a refine bigger than the budget simply continues on the next stamp
    /// (`budget_hit` is a pause, not a loss).
    const DYNTOPO_MAX_OPS_PER_FRAME: usize = 800;

    /// The dyntopo target edge length for a stroke whose largest dab radius is `r_max` —
    /// ONE formula shared by the refine driver and the symmetry plane-epsilon (the seam
    /// dedup must agree with the resolution dyntopo actually builds, or a dab can slip
    /// between "too close to the plane to mirror" and "far enough to re-refine").
    fn poly_dyntopo_detail_len(&self, r_max: f32) -> f32 {
        if self.sculpt.poly_dyntopo_brush_detail {
            let d = if self.sculpt.poly_dyntopo_detail <= 0.0 {
                12.0
            } else {
                self.sculpt.poly_dyntopo_detail.clamp(4.0, 48.0)
            };
            (r_max * 2.0 / d).max(1e-4)
        } else if self.sculpt.poly_dyntopo_const_detail > 0.0 {
            self.sculpt.poly_dyntopo_const_detail.clamp(0.002, 1.0)
        } else {
            0.05
        }
    }

    /// Blender-tech Dyntopo (DYNTOPO_PLAN Phase B, owner: "DYNTOPO NEEDS TO BE LIKE
    /// BLENDER TECH"): refine the WORKING mesh under this frame's dabs BEFORE the
    /// displacement kernel runs — local edge subdivide/collapse via
    /// `dyntopo_refine_region_seeded`, never a global remesh. When topology actually
    /// changed, the working's derived state (display raster, corner CSR, VertGrid)
    /// rebuilds from the mesh and the display re-derives on the next drain.
    /// Steady-state strokes over already-refined ground no-op in the kernel and cost
    /// O(brush region) here — the kernel's queues are seeded from `sculpt_radius_query`
    /// (Blender: queues built only from brush-intersecting PBVH nodes). Before the
    /// seeding landed this claim was FALSE: every dab paid an O(faces)+O(half-edges)
    /// whole-mesh scan even when nothing changed, ×≤64 dabs ×2–8 symmetry instances per
    /// frame — the owner's "slow remesh".
    fn poly_dyntopo_refine_for_dabs(
        &mut self,
        nid: NodeId,
        dabs: &[crate::voxel_surface_sculpt::SurfaceDab],
    ) {
        use core_math::half_edge::DyntopoParams;
        if dabs.is_empty() {
            return;
        }
        // Sculpt-LAYER strokes write position DELTAS into the modifier sidecar — a
        // topology change cannot ride that channel. Gate on the SAME predicate the
        // commit choke uses (`poly_sculpt_layer_slot`, which decides the episode's
        // slot) — gating on `poly_edit`'s slot alone could refine a stroke whose
        // episode still commits through the sparse layer path. Fail closed with a
        // once-per-stroke hint (same gate model as SnakeHook's).
        if self.poly_sculpt_layer_slot(nid).is_some()
            || matches!(self.sculpt.poly_edit, Some((_, s)) if s != POLY_SCULPT_DIRECT)
        {
            if !self.sculpt.stroke_active {
                self.toast("Dyntopo needs direct Surface sculpt (not a Sculpt-layer slot)");
            }
            return;
        }
        let r_max = dabs.iter().map(|d| d.radius).fold(0.0f32, f32::max);
        let detail = self.poly_dyntopo_detail_len(r_max);
        let collapse = self.sculpt.poly_dyntopo_collapse;
        let mut changed = false;
        let mut upload: Option<(Vec<[f32; 8]>, Vec<u32>, Vec<(u32, u32, u32)>)> = None;
        {
            let Some(w) = self.poly_sculpt_working.get_mut(&nid) else {
                return;
            };
            let mut budget = Self::DYNTOPO_MAX_OPS_PER_FRAME;
            let mut seeds: Vec<core_math::half_edge::VertexId> = Vec::new();
            for d in dabs {
                if budget == 0 {
                    break;
                }
                // Seed the kernel's candidate gather from the stroke's own radius query
                // (grid/PBVH — the same accel the displacement uses). The grid is only
                // rebuilt after this loop, so verts created by an EARLIER dab this frame
                // are absent from it; the kernel's coverage contract heals that through
                // the fans of their (indexed) neighbours plus next frame's fresh grid.
                seeds.clear();
                seeds.extend(
                    sculpt_radius_query(w, d.pos, d.radius)
                        .into_iter()
                        .map(core_math::half_edge::VertexId),
                );
                let res = w.mesh.dyntopo_refine_region_seeded(
                    &DyntopoParams {
                        center: d.pos,
                        radius: d.radius,
                        detail_len: detail,
                        max_ops: budget,
                        collapse,
                    },
                    Some(&seeds),
                );
                let ops = res.splits + res.collapses + res.triangulated;
                budget = budget.saturating_sub(ops);
                changed |= ops > 0;
            }
            if changed {
                let (indices, ranges) = Self::rebuild_working_derived(w);
                upload = Some((w.display_verts.clone(), indices, ranges));
            }
        }
        if let Some((verts, indices, mat_ranges)) = upload {
            // TEARING FIX + DISPLAY FAST PATH (owner: "tearing" then "remesh too slow for
            // the brush"): the GPU baseline must be re-laid out THIS frame — a deferred
            // resync let this stamp's corner patches scribble new-layout indices into the
            // old vertex buffer (the floating-shards tearing). The first fix ran
            // `poly_force_display_resync` here, which re-evaluated the stack, dropped the
            // prefix cache, bumped the edit generation AND re-rastered — a SECOND full
            // raster per topology frame, and the visible refine lag. Now the rebuild's OWN
            // raster uploads directly through the same count-change-safe choke the sync
            // path uses (`update_editable_mesh_ex`) — same corner layout as the working's
            // patch baseline by construction, half the cost, zero cache carnage. Commit
            // still runs the full resync.
            let cid = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
                core_math::scene_graph::NodePayload::PolygonalBody { mesh_cache_id, .. } => {
                    *mesh_cache_id
                }
                _ => None,
            });
            if let Some(cid) = cid {
                let tri_count = indices.len() as u32 / 3;
                let ranges = if mat_ranges.len() > 1 { Some(mat_ranges) } else { None };
                self.cache
                    .mesh_cache
                    .update_editable_mesh_ex(cid, (verts, indices), tri_count, ranges);
                // The stored sync key now describes content this upload replaced — drop it
                // so any later real sync re-uploads instead of early-outing on a stale key.
                self.cache.poly_mesh_gpu_key.remove(&cid);
                self.poly_lod_invalidate(nid);
            } else {
                // No display buffer yet — the established dirty-drain fallback.
                self.poly_display_dirty.insert(nid);
                self.mark_dirty(crate::DirtyFlags::MESH);
            }
            self.mark_dirty(crate::DirtyFlags::RENDER);
        }
    }

    /// Rebuild EVERY derived structure of the working after a topology change — the same
    /// set `from_prefix` builds. Never partial: a stale grid/CSR after topo change is the
    /// permanent-dead-zone class. Returns `(indices, material ranges)` of the SAME raster
    /// the display arrays came from, so the caller can upload it directly (dyntopo
    /// display fast path) without a second full raster.
    fn rebuild_working_derived(w: &mut PolySculptWorking) -> (Vec<u32>, Vec<(u32, u32, u32)>) {
        let (display_verts, indices, mat_ranges, corner_vert, corner_face) =
            raster_full_with_corner_map(&w.mesh);
        let (vert_off, vert_corners, vert_faces) =
            build_vert_csr(w.mesh.vertices.len(), &corner_vert, &corner_face);
        w.display_verts = display_verts;
        w.corner_vert = corner_vert;
        w.vert_off = vert_off;
        w.vert_corners = vert_corners;
        w.vert_faces = vert_faces;
        w.dirty_corners.clear();
        w.grid = VertGrid::rebuild(&w.mesh, grid_cell_for(&w.mesh));
        w.pbvh = None; // flag-gated; rebuilds lazily on next query
        // Vert ids can be RECYCLED by a collapse+split in the same stroke — a frozen
        // normal or enhance direction under a reused id would belong to a different
        // vertex. Surviving verts re-freeze at their next batch (bounded drift on
        // topo-change frames only).
        w.first_touch_normal.clear();
        w.enhance_dirs = None;
        w.gen = w.gen.wrapping_add(1);
        (indices, mat_ranges)
    }

    pub(crate) fn poly_sculpt_remesh_now(&mut self, nid: NodeId, res: u32) {
        if let Some((res, faces, ms)) = self.poly_sculpt_remesh_inner(nid, res) {
            self.toast(&format!(
                "Remeshed to {res}\u{B3} — {faces} faces in {ms:.0} ms (Ctrl+Z restores)"
            ));
        }
    }

    /// The remesh body shared by Ctrl+R and Dyntopo (quiet: failure toasts only —
    /// a per-stroke success toast would spam). Returns `(res, faces, ms)` on success.
    pub(crate) fn poly_sculpt_remesh_inner(
        &mut self,
        nid: NodeId,
        res: u32,
    ) -> Option<(u32, usize, f32)> {
        use core_math::scene_graph::NodePayload;
        // Flush any open stroke into the document first — the episode owns those deltas.
        self.commit_episode();
        self.sculpt.stroke_active = false;
        let Some(base) = self.scene.scene_graph.get(nid).and_then(|n| match &n.payload {
            NodePayload::PolygonalBody { mesh, .. } => Some((**mesh).clone()),
            _ => None,
        }) else {
            self.toast("Remesh: select a polygonal mesh");
            return None;
        };
        let res = res.clamp(16, 256);
        let t0 = std::time::Instant::now();
        let Some(new_mesh) =
            crate::poly::remesh::voxel_remesh_mesh(&base, res, true, 0.1, 1.0)
        else {
            self.toast("Remesh produced no surface — mesh unchanged");
            return None;
        };
        let faces = new_mesh.faces.len();
        self.transaction("Sculpt Remesh (Voxel)", |s| {
            s.poly_release_shared_display_arcs(nid);
            s.poly_install_rebuilt_mesh(nid, new_mesh, &[]);
            s.poly_sync_mesh_cache(nid);
            s.overlay_lines_fp = u64::MAX;
            s.mark_dirty(crate::DirtyFlags::SCENE);
            s.mark_dirty(crate::DirtyFlags::RENDER);
        });
        // Old-topology runtime state dies with the old mesh.
        self.poly_sculpt_drop(nid);
        Some((res, faces, t0.elapsed().as_secs_f32() * 1000.0))
    }

    /// Live PolySculpt stroke. Same interpolator / spacing / pressure as the voxel producer (C3).
    pub(crate) fn handle_poly_sculpt_input(
        &mut self,
        response: &egui::Response,
        rect: egui::Rect,
        shift: bool,
        ctrl: bool,
        alt: bool,
    ) {
        let nid = if let Some((nid, _)) = self.sculpt.poly_edit {
            nid
        } else if let Some(nid) = self.poly_active_body() {
            self.begin_surface_sculpt(nid);
            nid
        } else {
            return;
        };
        // Seed poly tool defaults (incl. Accumulate OFF) even when the params panel was
        // never opened — the stroke must see the same defaults the UI would show.
        self.sculpt.poly_ensure_defaults();
        // New stroke = new undo. `drag_started_by` is often missed (egui / tablet), so
        // key off `stroke_active` which is still false on the first dragged frame.
        // SELF-HEAL (owner 2026-08-19 dead-zone diagnosis): ALSO key on a fresh drag start —
        // if any exit path ate the release (`stroke_active` stuck true: body switch, work-mode
        // change, focus loss), the next gesture must still run `begin_poly_sculpt_episode`,
        // which commits the stale episode AND clears the `deposit_applied` ledger. A stale
        // ledger makes Draw/Erase/Inflate deposit exactly ZERO wherever the eaten stroke
        // already topped up — a silent per-region dead brush. Same discipline as the voxel
        // side's `surface_end_stroke` stroke-start heal.
        if response.drag_started_by(egui::PointerButton::Primary) || !self.sculpt.stroke_active {
            // PR-6 (§7.5): sparse first-touch episode — never a Full scene snapshot
            // (one COWed remesh retained per stroke was the memory cost).
            let slot = self.poly_sculpt_layer_slot(nid);
            self.begin_poly_sculpt_episode(nid, slot);
            self.sdf_stroke_interpolator.reset();
            self.sdf_stroke_interpolator.spacing =
                self.sculpt.poly_tool.stroke_spacing.clamp(0.03, 0.50);
            self.sculpt.poly_stroke_last = None;
            self.sculpt.poly_lazy_pos = None;
            self.sculpt.airbrush_last = None;
            // Fresh random stream per stroke for tip orientation / rotation dynamics
            // (poly twin of dyn_stroke_seed / dyn_dab_index).
            self.sculpt.poly_stroke_seed = self.sculpt.poly_stroke_seed.wrapping_add(0x9E37_79B9);
            self.sculpt.poly_dab_counter = 0;
            // Self-heal: any exit path that missed the release clears here, so a stroke can never
            // inherit the previous one's frozen anchor (same discipline as `surface_end_stroke`).
            self.sculpt.poly_grab = None;
        }
        let Some(mp_raw) = response.interact_pointer_pos() else {
            return;
        };
        // LazyMouse / Stabilize — same rope as voxel (`lazy * 70` screen px).
        let mp = {
            let lazy = if matches!(
                self.sculpt.poly_brush.unwrap_or(core_math::voxel_sculpt::VoxelBrush::Draw),
                core_math::voxel_sculpt::VoxelBrush::Move
            ) {
                0.0
            } else {
                self.sculpt.poly_tool.lazy
            };
            let radius_px = lazy * 70.0;
            if radius_px < 1.0 {
                self.sculpt.poly_lazy_pos = None;
                mp_raw
            } else {
                let cur = glam::Vec2::new(mp_raw.x, mp_raw.y);
                let lp0 = if !self.sculpt.stroke_active {
                    cur
                } else {
                    self.sculpt.poly_lazy_pos.unwrap_or(cur)
                };
                let d = cur - lp0;
                let dist = d.length();
                let lp = if dist > radius_px {
                    lp0 + d * (1.0 - radius_px / dist)
                } else {
                    lp0
                };
                self.sculpt.poly_lazy_pos = Some(lp);
                egui::pos2(lp.x, lp.y)
            }
        };
        let (cam, rd) = self.cam_ray(mp, rect);
        let world = self.scene.scene_graph.world_transform(nid);
        self.ensure_poly_sculpt_working(nid);
        let Some(w) = self.poly_sculpt_working.get(&nid) else {
            self.toast("Surface sculpt: no mesh to stroke");
            return;
        };
        // PR-4 (ZB plan §7.1): O(brush) pick — bounded grid-DDA today, 8×8 GPU crop
        // fronting it next (PR-4b). `ray_pick_mesh_face` is BANNED on the stroke path
        // (it was 523 ms p50 on the owner's 161k body — 98% of the stroke frame).
        let _ = w;
        let Some(hit) = self.poly_sculpt_pick_hit(nid, cam, rd, Some((mp, rect))) else {
            return;
        };
        let (hit_w, hit, nrm) = (hit.world, hit.local, hit.normal);
        let _ = hit_w;
        // Ring POSITION follows the stroke contact mid-drag; ORIENTATION latches to the
        // stroke's first contact normal — the voxel ring's "Mid-stroke Original" rule
        // (`draw_viewport_overlays.rs` `first_point_normal`), which poly lacked.
        // Re-orienting from the live nearest-vert normal every frame made the ring spin/
        // flicker while Crease dug its own valley under the cursor (owner 2026-08-23) —
        // the same sample-your-own-result loop as the brush normals. Blender sidesteps
        // the whole class by drawing a 2D view-space circle during strokes.
        let ring_n = if self.sculpt.stroke_active {
            self.sculpt.poly_cursor_hit.map(|(_, n)| n).unwrap_or(nrm)
        } else {
            nrm
        };
        self.sculpt.poly_cursor_hit = Some((hit, ring_n));
        let radius = self.poly_sculpt_radius();
        let strength = self.poly_sculpt_strength();
        let sample = core_math::sculpt_layer::StrokeSample::new(hit, nrm, 1.0);
        let mut pts = match self.sculpt.stroke_method.effective() {
            crate::SculptStrokeMethod::Dots | crate::SculptStrokeMethod::DragDot => {
                vec![sample.clone()]
            }
            crate::SculptStrokeMethod::Airbrush => {
                let mut pts = self.sdf_stroke_interpolator.push(sample.clone(), radius);
                if pts.is_empty() {
                    let now = std::time::Instant::now();
                    let due = self.sculpt.airbrush_last.map_or(true, |t| {
                        now.duration_since(t) >= self.sculpt.airbrush_interval()
                    });
                    if due {
                        self.sculpt.airbrush_last = Some(now);
                        pts = vec![sample.clone()];
                    }
                } else {
                    self.sculpt.airbrush_last = Some(std::time::Instant::now());
                }
                pts
            }
            _ => self.sdf_stroke_interpolator.push(sample.clone(), radius),
        };
        if pts.len() > 64 {
            pts = vec![sample];
        }
        let jitter = self.sculpt.poly_tool.dyn_jitter;
        if jitter > 1e-4 {
            for p in &mut pts {
                let seed = (p.position.x * 9176.0 + p.position.y * 413.0 + p.position.z * 127.0)
                    .to_bits()
                    .wrapping_mul(0x9E37_79B9);
                let u = ((seed >> 8) as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let v = ((seed >> 16) as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let bit = nrm.cross(Vec3::Y);
                let t1 = if bit.length_squared() < 1e-8 {
                    nrm.cross(Vec3::X).normalize_or_zero()
                } else {
                    bit.normalize_or_zero()
                };
                let t2 = nrm.cross(t1).normalize_or_zero();
                p.position += (t1 * u + t2 * v) * (jitter * radius);
            }
        }
        // Alpha Orientation + Random Rotation for the brush tip — the SAME resolver + dynamics
        // the voxel producer uses, so the poly Texture popover's Mapping/Angle/Random knobs
        // drive the stamp instead of being stored-but-dead. Before symmetry (mirrored dabs
        // clone the resolved rotation, matching how they clone position/normal).
        if !pts.is_empty() {
            let dab_start = self.sculpt.poly_dab_counter;
            let cam_up = self.viewport.camera.orbit * Vec3::Y;
            crate::input_sculpt::resolve_tip_rotations(
                &mut pts,
                self.sculpt.poly_tool.tip_orient,
                self.sculpt.poly_tool.tip_follow_stroke,
                self.sculpt.poly_tool.tip_angle,
                cam_up,
                self.sculpt.poly_stroke_seed,
                dab_start,
                self.sculpt.poly_stroke_last,
            );
            if self.sculpt.poly_tool.dyn_random_rotation > 1e-6 {
                let knobs = core_math::sculpt_layer::BrushDynamics {
                    random_rotation: self.sculpt.poly_tool.dyn_random_rotation,
                    ..Default::default()
                };
                core_math::sculpt_layer::apply_brush_dynamics(
                    &mut pts,
                    radius,
                    knobs,
                    crate::input_sculpt::brush_dynamics_seed_at(
                        self.sculpt.poly_stroke_seed,
                        dab_start,
                    ),
                );
            }
            self.sculpt.poly_dab_counter = dab_start.wrapping_add(pts.len() as u32);
        }
        // Stroke tangent from the PRIMARY samples, BEFORE the mirror expansion. This used
        // to be computed after it, as `last − first` of the expanded list — with symmetry
        // on, "last" was a MIRRORED point, so the tangent crossed the mirror plane instead
        // of following the stroke (wrong tip-follow / directional dab data every
        // symmetric stroke).
        let tan = if pts.len() >= 2 {
            (pts[pts.len() - 1].position - pts[0].position).normalize_or_zero()
        } else {
            Vec3::ZERO
        };
        // Local-axis mirror (same octant expansion as the voxel stamps'
        // `expand_voxel_symmetry`, input_sculpt/mod.rs), with its plane guard ported and
        // one ordering change:
        //  * PLANE DEDUP: a sample within half the resolution quantum of an enabled
        //    mirror plane already represents both sides — mirroring it deposits the dab
        //    TWICE on the seam (a ridge/groove along the plane). The voxel path has
        //    always skipped this; this path never did.
        //  * INTERLEAVED ORDER: each sample is immediately followed by its mirror images.
        //    The old order (all originals, then all mirrors) meant that when the dyntopo
        //    per-frame op budget drained mid-frame, ONLY the original side got refined —
        //    one side crisp, the mirror blobby (the dyntopo symmetry artifact). With
        //    interleaving, a budget cut lands at a sample boundary and both sides carry
        //    matching refinement; the tail resumes next frame (`budget_hit` is a pause).
        // DUPLICATION, FLAGGED NOT FIXED: three octant expansions now exist (voxel
        // samples, this, Grab anchors in `poly_grab_begin`) — candidates for one shared
        // helper; not merged here (bug-fix scope).
        if self.sculpt.poly_symmetry.iter().any(|&on| on) {
            // Resolution quantum: the edge length dyntopo builds when it is on, else the
            // working grid cell (≈ median edge), else 5% of the brush for a first frame
            // with no working yet.
            let quantum = if self.sculpt.dyn_topo {
                self.poly_dyntopo_detail_len(radius)
            } else {
                self.poly_sculpt_working
                    .get(&nid)
                    .map(|w| w.grid.cell_size())
                    .unwrap_or(radius * 0.05)
            };
            let plane_eps = 0.5 * quantum;
            let mut out = Vec::with_capacity(pts.len() * 2);
            for pt in pts.drain(..) {
                let mut inst = vec![(pt.position, pt.normal)];
                for axis in 0..3 {
                    if !self.sculpt.poly_symmetry[axis] {
                        continue;
                    }
                    let mirrored: Vec<_> = inst
                        .iter()
                        .filter(|(h, _)| h[axis].abs() > plane_eps)
                        .map(|&(h, n)| {
                            let mut ha = h.to_array();
                            let mut na = n.to_array();
                            ha[axis] = -ha[axis];
                            na[axis] = -na[axis];
                            (Vec3::from_array(ha), Vec3::from_array(na))
                        })
                        .collect();
                    inst.extend(mirrored);
                }
                for (h, n) in inst {
                    let mut q = pt.clone();
                    q.position = h;
                    q.normal = n;
                    out.push(q);
                }
            }
            pts = out;
        }
        let brush = if shift {
            core_math::voxel_sculpt::VoxelBrush::Smooth
        } else {
            self.sculpt
                .poly_brush
                .unwrap_or(core_math::voxel_sculpt::VoxelBrush::Draw)
        };
        if !crate::voxel_surface_sculpt::surface_brush_supported(brush)
            && !matches!(brush, core_math::voxel_sculpt::VoxelBrush::Move)
        {
            return;
        }
        if matches!(brush, core_math::voxel_sculpt::VoxelBrush::SnakeHook) {
            // No poly implementation yet: the tube lives in `voxel_surface_apply_snake_hook`
            // (voxel-chunk machinery) and the shared dab kernel has NO SnakeHook arm — without
            // this gate the stroke reached the kernel's `_ => {}` and silently no-opped
            // (POLYSCULPT-PLANELESS, 2026-08-18). Fail closed with a visible reason; both poly
            // brush pickers grey the button. Toast once per stroke (the caller flips
            // `stroke_active` after the first frame).
            if !self.sculpt.stroke_active {
                self.toast("Snake Hook is not in poly sculpt yet — use Voxel Surface mode");
            }
            return;
        }
        let carve_native = matches!(
            brush,
            core_math::voxel_sculpt::VoxelBrush::Erase
                | core_math::voxel_sculpt::VoxelBrush::Crease
                | core_math::voxel_sculpt::VoxelBrush::Scrape
        );
        let invert = if carve_native {
            self.sculpt.poly_tool.brush_add
        } else {
            !self.sculpt.poly_tool.brush_add
        };
        let mut sgn = if invert { -1.0 } else { 1.0 };
        // Ctrl (Blender) OR Alt (ZBrush, owner 2026-08-18) inverts the stroke. Alt+LMB drag
        // is free in sculpt (orbit is MMB; Alt only angle-snaps during an MMB orbit).
        if ctrl || alt {
            sgn = -sgn;
        }
        if matches!(brush, core_math::voxel_sculpt::VoxelBrush::Move) {
            // BLENDER GRAB. Anchor once, then drag in the VIEW PLANE through the anchor.
            //
            // What this replaced, and why each piece was wrong:
            //   * it re-raycast the mesh every frame — but the mesh is being deformed, so the ray
            //     landed somewhere new each frame, or missed and silently ended the stroke;
            //   * it re-queried the vertex set at the MOVING centre, so verts entered and left the
            //     brush mid-drag (taffy smear, not a grab);
            //   * it accumulated per-frame deltas instead of an absolute offset, so the result
            //     depended on frame timing and could not be undone by dragging back;
            //   * `poly_sculpt_stamp` applies a UNIFORM delta — no falloff — so the grabbed region
            //     moved as a rigid disc with a hard rim.
            if self.sculpt.poly_grab.is_none()
                && !self.poly_grab_begin(nid, hit, hit_w, radius)
            {
                return; // nothing in range — do not start a stroke that cannot move anything
            }
            // Drag plane: through the anchor, facing the camera. Intersect the CURRENT cursor ray
            // with it; the offset is that point minus the anchor. No mesh query is involved, which
            // is exactly why the grab keeps working once the surface has moved away.
            if let Some(anchor_world) = self.sculpt.poly_grab.as_ref().map(|g| g.anchor_world) {
                let n = -rd.normalize_or_zero(); // plane normal = toward the camera
                let denom = rd.dot(n);
                if denom.abs() > 1.0e-6 {
                    let t_plane = (anchor_world - cam).dot(n) / denom;
                    if t_plane > 0.0 {
                        let cur = cam + rd * t_plane;
                        self.poly_grab_update(cur - anchor_world);
                    }
                }
            }
            self.mark_dirty(crate::DirtyFlags::RENDER);
            return;
        }
        // Blender spacing-overlap attenuation (brush_strength, sculpt.cc v3.6: the
        // Draw-family multiplies `ups->overlap_factor`; Smooth/Grab do NOT). This is
        // what makes a pass deposit ≈ strength × radius at ANY spacing.
        let strength_eff = match brush {
            core_math::voxel_sculpt::VoxelBrush::Draw
            | core_math::voxel_sculpt::VoxelBrush::Erase
            | core_math::voxel_sculpt::VoxelBrush::Inflate => {
                strength
                    * core_math::voxel_sculpt::stroke_overlap_factor(
                        self.sculpt.poly_tool.stroke_spacing.clamp(0.03, 0.50),
                        self.poly_sculpt_hardness(),
                        self.sculpt.poly_tool.falloff_preset,
                    )
            }
            _ => strength,
        };
        let dabs = Self::poly_sculpt_dabs_from_samples(&pts, radius, strength_eff, tan);
        self.poly_sculpt_apply_dabs(nid, &dabs, brush, sgn);
        self.sculpt.poly_stroke_last = Some(hit);
        self.mark_dirty(crate::DirtyFlags::RENDER);
    }
}

/// One Jacobi step of 1-ring Laplacian Smooth on a polygonal surface.
///
/// Voxel Surface Smooth hashes nearby *positions* (uniform Surface Nets spacing). On a
/// remesh that mixes dense and coarse faces, that centroid yanks sparse verts into the
/// dense cluster and leaves pole-like stretched triangles.
fn poly_smooth_one_ring(
    mesh: &HalfEdgeMesh,
    ids: &[u32],
    old_pos: &[Vec3],
    dabs: &[crate::voxel_surface_sculpt::SurfaceDab],
    hardness: f32,
    preset: core_math::voxel_sculpt::FalloffPreset,
    packed: &mut [[f32; 8]],
) {
    let s = dabs
        .iter()
        .map(|d| d.strength)
        .fold(0.0f32, f32::max);
    if s <= 0.0 {
        return;
    }
    // POWER = ITERATIONS (owner 2026-08-19 "smooth strength is capped too low"): the
    // per-step travel clamp (0.45 × mean ring edge) is a STABILITY bound — raising it
    // overshoots the neighbor average and folds dense meshes. Strength instead scales the
    // number of clamped Jacobi passes per frame: s ≤ 0.5 → 1 pass (the old feel),
    // s 1.0 → 2, s 2.0 → 4 — up to ~2 mean-edges of travel per frame, stable every step.
    let iters = ((s * 2.0).round() as usize).clamp(1, 6);
    // Per-vert falloff weight from the PRE-stroke position (fixed across passes).
    let mut wgts: Vec<f32> = vec![0.0; ids.len()];
    for (k, _) in ids.iter().enumerate() {
        let p = old_pos[k];
        for d in dabs {
            let r = d.radius.max(1e-4);
            let d2 = (p - d.pos).length_squared();
            if d2 >= r * r {
                continue;
            }
            let t = d2.sqrt() / r;
            let fw = falloff_weight_preset(1.0 - t, hardness, preset);
            if fw > wgts[k] {
                wgts[k] = fw;
            }
        }
    }
    // Region position map: brush verts move between passes; neighbors OUTSIDE the brush
    // stay pinned at their document positions (natural boundary condition).
    let mut cur: std::collections::HashMap<u32, Vec3> =
        ids.iter().enumerate().map(|(k, &i)| (i, old_pos[k])).collect();
    let mut fan = Vec::new();
    let mut next: Vec<(u32, Vec3)> = Vec::with_capacity(ids.len());
    for _pass in 0..iters {
        next.clear();
        for (k, &i) in ids.iter().enumerate() {
            let wgt = wgts[k];
            if wgt <= 0.0 {
                continue;
            }
            let vid = VertexId(i);
            if !mesh.is_vert_live(vid) {
                continue;
            }
            let p = cur[&i];
            mesh.vertex_fan_into(vid, &mut fan);
            let mut sum = Vec3::ZERO;
            let mut cnt = 0u32;
            let mut ring_len = 0.0f32;
            for &he in &fan {
                if !mesh.is_edge_live(he) {
                    continue;
                }
                let nb = mesh.he(he).vertex;
                if !mesh.is_vert_live(nb) {
                    continue;
                }
                let q = cur
                    .get(&nb.0)
                    .copied()
                    .unwrap_or_else(|| mesh.vert(nb).position);
                sum += q;
                cnt += 1;
                ring_len += (q - p).length();
            }
            if cnt < 2 {
                continue;
            }
            let avg = sum / cnt as f32;
            // Full 1-ring Laplacian (flattens bumps). Do not drop the normal component —
            // that left spikes standing. Stretching is stopped by the per-step edge clamp.
            let mut delta = avg - p;
            let mean_edge = (ring_len / cnt as f32).max(1e-6);
            let max_step = mean_edge * 0.45;
            let dl = delta.length();
            if dl > max_step {
                delta *= max_step / dl;
            }
            // Per-pass mix stays ≤ 0.9 (Jacobi stability); the pass loop is the power.
            // Always CONTRACTIVE — Blender clamps forward smooth to [0,1]
            // (sculpt_smooth.cc `CLAMP(bstrength, 0.0f, 1.0f)`); the inverted brush is
            // Enhance Details (`poly_enhance_details_one_ring`), never `mix * sgn`.
            let mix = (wgt * s * 0.5).clamp(0.0, 0.9);
            next.push((i, p + delta * mix));
        }
        for &(i, np) in &next {
            cur.insert(i, np);
        }
    }
    for (k, &i) in ids.iter().enumerate() {
        if wgts[k] <= 0.0 {
            continue;
        }
        let np = cur[&i];
        packed[k][0] = np.x;
        packed[k][1] = np.y;
        packed[k][2] = np.z;
    }
}

/// Blender `enhance_details_brush` init (sculpt_smooth.cc v4.2, first-brush-step block):
/// `detail_directions[i] = neighbor_avg − co` for EVERY vertex, from the mesh as it stands
/// when the inverted-Smooth stroke first fires. One O(V·ring) pass per stroke. Verts with
/// fewer than 2 live neighbors get ZERO (nothing to enhance toward/away from).
fn compute_enhance_dirs(mesh: &HalfEdgeMesh) -> Vec<Vec3> {
    let mut dirs = vec![Vec3::ZERO; mesh.vertices.len()];
    let mut fan = Vec::new();
    for i in 0..mesh.vertices.len() as u32 {
        let vid = VertexId(i);
        if !mesh.is_vert_live(vid) {
            continue;
        }
        mesh.vertex_fan_into(vid, &mut fan);
        let mut sum = Vec3::ZERO;
        let mut cnt = 0u32;
        for &he in &fan {
            if !mesh.is_edge_live(he) {
                continue;
            }
            let nb = mesh.he(he).vertex;
            if mesh.is_vert_live(nb) {
                sum += mesh.vert(nb).position;
                cnt += 1;
            }
        }
        if cnt >= 2 {
            dirs[i as usize] = sum / cnt as f32 - mesh.vert(vid).position;
        }
    }
    dirs
}

/// Blender `do_enhance_details_brush_task` (sculpt_smooth.cc v4.2): the INVERTED Smooth.
/// `fade = bstrength(<0, clamped to [−1,1]) × falloff`, `disp = co + detail_directions[i]
/// × fade` — verts move AWAY from their stroke-start smoothing target along a direction
/// field frozen at the stroke's first enhance step (`dirs`), so the displacement cannot
/// feed back on itself the way a live negative Laplacian does. Single pass per batch
/// (Blender runs enhance once per step; only forward smooth iterates).
fn poly_enhance_details_one_ring(
    mesh: &HalfEdgeMesh,
    ids: &[u32],
    old_pos: &[Vec3],
    dabs: &[crate::voxel_surface_sculpt::SurfaceDab],
    hardness: f32,
    preset: core_math::voxel_sculpt::FalloffPreset,
    dirs: &[Vec3],
    packed: &mut [[f32; 8]],
) {
    let s = dabs.iter().map(|d| d.strength).fold(0.0f32, f32::max);
    if s <= 0.0 {
        return;
    }
    let b = s.min(1.0); // Blender CLAMP(bstrength, -1, 1) — magnitude side
    for (k, &i) in ids.iter().enumerate() {
        let vid = VertexId(i);
        if !mesh.is_vert_live(vid) {
            continue;
        }
        let Some(&dir) = dirs.get(i as usize) else { continue };
        if dir.length_squared() < 1e-16 || !dir.is_finite() {
            continue;
        }
        let p = old_pos[k];
        let mut wgt = 0.0f32;
        for d in dabs {
            let r = d.radius.max(1e-4);
            let d2 = (p - d.pos).length_squared();
            if d2 >= r * r {
                continue;
            }
            let t = d2.sqrt() / r;
            let fw = falloff_weight_preset(1.0 - t, hardness, preset);
            if fw > wgt {
                wgt = fw;
            }
        }
        if wgt <= 0.0 {
            continue;
        }
        let fade = -(b * wgt); // negative: away from the smoothing target
        let np = p + dir * fade;
        packed[k][0] = np.x;
        packed[k][1] = np.y;
        packed[k][2] = np.z;
    }
}

// ============================================================================
// GRAB — the two properties that make it feel like Blender's, pinned.
//
// Owner 2026-08-14: "all brushes feel weird, start with grab; it must work on a basic poly
// object like Blender". The old path re-raycast the mesh, re-queried the vertex set at the
// moving centre, accumulated per-frame deltas, and applied a UNIFORM offset. Each of those is
// separately observable, so each gets an assertion rather than a feel claim.
// ============================================================================
#[cfg(test)]
pub(super) mod grab_tests {
    use super::*;
    use core_math::scene_graph::NodePayload;

    /// Flat 8x8 grid in XZ at y=0 — enough verts that a mid-radius falloff is measurable.
    pub(crate) fn app_with_grid() -> (crate::AzraApp, NodeId) {
        let mut quads: Vec<[usize; 4]> = Vec::new();
        let mut pts: Vec<Vec3> = Vec::new();
        let n = 9;
        for j in 0..n {
            for i in 0..n {
                pts.push(Vec3::new(i as f32 * 0.25 - 1.0, 0.0, j as f32 * 0.25 - 1.0));
            }
        }
        for j in 0..n - 1 {
            for i in 0..n - 1 {
                let a = j * n + i;
                quads.push([a, a + 1, a + n + 1, a + n]);
            }
        }
        let mesh = HalfEdgeMesh::from_quads(&pts, &quads);
        let mut app = crate::AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "grid".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        (app, nid)
    }

    fn y_of(app: &crate::AzraApp, nid: NodeId, i: u32) -> f32 {
        app.poly_sculpt_working.get(&nid).unwrap().mesh.vert(VertexId(i)).position.y
    }

    /// Grab must FALL OFF: the vertex at the anchor moves (nearly) the full offset, one at the
    /// rim moves far less. The old `poly_sculpt_stamp` applied a uniform delta to every vert in
    /// range — a rigid disc with a hard edge, which is a large part of "feels weird".
    #[test]
    fn grab_falls_off_from_anchor() {
        let (mut app, nid) = app_with_grid();
        let anchor = Vec3::ZERO; // centre of the grid
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.6), "anchor must find verts");

        // Nearest vert to the anchor, and the farthest one still inside the radius.
        let (near, far) = {
            let w = app.poly_sculpt_working.get(&nid).unwrap();
            let g = app.sculpt.poly_grab.as_ref().unwrap();
            let mut near = (f32::MAX, 0u32);
            let mut far = (0.0f32, 0u32);
            for &i in &g.verts {
                let d = (w.mesh.vert(VertexId(i)).position - anchor).length();
                if d < near.0 { near = (d, i); }
                if d > far.0 { far = (d, i); }
            }
            (near.1, far.1)
        };

        app.poly_grab_update(Vec3::new(0.0, 1.0, 0.0));
        let dy_near = y_of(&app, nid, near);
        let dy_far = y_of(&app, nid, far);
        assert!(dy_near > 0.8, "anchor vert should take nearly the full offset, got {dy_near}");
        assert!(dy_far < dy_near * 0.6, "rim vert must fall off: near={dy_near} far={dy_far}");
        assert!(dy_far >= 0.0, "falloff must not invert");
    }

    /// Grab must be ABSOLUTE, not accumulated: sending the same offset twice changes nothing,
    /// and returning to zero restores the surface EXACTLY. The old path added `hit - prev` each
    /// frame, so identical input kept deforming and dragging back never restored.
    #[test]
    fn grab_is_absolute_and_reversible() {
        let (mut app, nid) = app_with_grid();
        let anchor = Vec3::ZERO;
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.6));
        let probe = app.sculpt.poly_grab.as_ref().unwrap().verts[0];
        let before = y_of(&app, nid, probe);

        app.poly_grab_update(Vec3::new(0.0, 1.0, 0.0));
        let once = y_of(&app, nid, probe);
        app.poly_grab_update(Vec3::new(0.0, 1.0, 0.0));
        let twice = y_of(&app, nid, probe);
        assert!(
            (once - twice).abs() < 1.0e-6,
            "re-sending the same offset must be idempotent: {once} vs {twice}"
        );

        app.poly_grab_update(Vec3::ZERO);
        let back = y_of(&app, nid, probe);
        assert!(
            (back - before).abs() < 1.0e-6,
            "dragging back to the anchor must restore exactly: {before} -> {back}"
        );
    }

    /// The affected set is FROZEN. Verts outside the radius at anchor time must never be
    /// recruited, however far the drag travels — the old code re-queried at the moving centre,
    /// which is what smeared the surface.
    #[test]
    fn grab_set_is_frozen_at_anchor() {
        let (mut app, nid) = app_with_grid();
        let anchor = Vec3::new(-1.0, 0.0, -1.0); // corner, so most of the grid is out of range
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.4));
        let grabbed: std::collections::HashSet<u32> =
            app.sculpt.poly_grab.as_ref().unwrap().verts.iter().copied().collect();

        // Drag far across the sheet: a re-querying grab would pick up the whole grid.
        app.poly_grab_update(Vec3::new(2.0, 0.0, 2.0));

        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let mut moved = 0usize;
        for i in 0..w.mesh.vertices.len() as u32 {
            if !w.mesh.is_vert_live(VertexId(i)) { continue; }
            let p = w.mesh.vert(VertexId(i)).position;
            let orig = Vec3::new(
                (i % 9) as f32 * 0.25 - 1.0, 0.0, (i / 9) as f32 * 0.25 - 1.0,
            );
            if (p - orig).length() > 1.0e-5 {
                moved += 1;
                assert!(grabbed.contains(&i), "vert {i} moved but was not in the frozen set");
            }
        }
        assert!(moved > 0, "the drag must actually move the frozen set");
    }
}

// ============================================================================
// PLANE BRUSHES IN POLY SCULPT — POLYSCULPT-PLANELESS (2026-08-18).
//
// `poly_sculpt_apply_dabs` used to pass `area_planes = None` for every dab; the shared
// kernel's fail-closed plane gate then made Clay / ClayBuildup / Scrape SILENT NO-OPS in
// poly sculpt (and SnakeHook fell into the kernel's `_ => {}`). These pin that the poly
// path now fits real area frames through the SAME pure functions the voxel path uses,
// and that the kernel-gap brush stays an explicit no-op at the apply layer (the input
// path gates it with a toast). The trench-cancel fail-closed behavior of the shared fit
// itself is pinned voxel-side (voxel_surface_sculpt tests) — not duplicated here.
// ============================================================================
#[cfg(test)]
mod plane_brush_tests {
    use super::grab_tests::app_with_grid;
    use super::*;
    use core_math::voxel_sculpt::VoxelBrush;

    pub(super) fn one_dab(pos: Vec3, dir: Vec3, radius: f32) -> Vec<crate::voxel_surface_sculpt::SurfaceDab> {
        vec![crate::voxel_surface_sculpt::SurfaceDab {
            pos,
            dir,
            radius,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }]
    }

    /// Enter the working mesh and make sure it carries real normals (the fixture's
    /// `from_quads` mesh starts without them; production meshes always have them — Draw
    /// and Inflate already depend on that).
    pub(super) fn app_with_working(app: &mut crate::AzraApp, nid: NodeId) -> Vec3 {
        app.ensure_poly_sculpt_working(nid);
        let w = app.poly_sculpt_working.get_mut(&nid).unwrap();
        w.mesh.recompute_normals();
        let n = w.mesh.vert(VertexId(40)).normal.normalize_or_zero();
        assert!(
            n.y.abs() > 0.99,
            "flat-grid fixture normal must be ±Y, got {n:?}"
        );
        n
    }

    /// Clay on a flat grid must DEPOSIT toward its proud plane (it moved nothing before
    /// the fix), bounded by the kernel's 0.5·r per-dab design cap.
    #[test]
    fn poly_clay_deposits_on_a_flat_grid() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        let r = 0.5f32;
        let before: Vec<Vec3> = {
            let w = app.poly_sculpt_working.get(&nid).unwrap();
            (0..81).map(|i| w.mesh.vert(VertexId(i)).position).collect()
        };
        let moved = app.poly_sculpt_apply_dabs(nid, &one_dab(Vec3::ZERO, n, r), VoxelBrush::Clay, 1.0);
        assert!(moved > 0, "Clay must move verts on a flat grid (was a silent no-op)");
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let center_d = w.mesh.vert(VertexId(40)).position - before[40];
        assert!(
            center_d.dot(n) > 1e-4,
            "center vert must build UP along the area normal, moved {center_d:?}"
        );
        for i in 0..81u32 {
            let d = (w.mesh.vert(VertexId(i)).position - before[i as usize]).length();
            assert!(
                d <= 0.5 * r + 1e-4,
                "vert {i} moved {d} — beyond the 0.5·r per-dab design cap"
            );
        }
    }

    /// Scrape is one-sided: it shaves the raised vert down toward the area plane and
    /// leaves material below the plane alone.
    #[test]
    fn poly_scrape_shaves_only_above_the_plane() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        let (raised_pos, flat_y) = {
            let w = app.poly_sculpt_working.get_mut(&nid).unwrap();
            let old = w.mesh.vert(VertexId(40)).position;
            let new = old + n * 0.1;
            w.mesh.vert_mut(VertexId(40)).position = new;
            w.grid.move_vert(40, old, new);
            (new, w.mesh.vert(VertexId(39)).position.dot(n))
        };
        let moved =
            app.poly_sculpt_apply_dabs(nid, &one_dab(raised_pos, n, 0.5), VoxelBrush::Scrape, 1.0);
        assert!(moved > 0, "Scrape must shave the raised vert (was a silent no-op)");
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let h_raised = w.mesh.vert(VertexId(40)).position.dot(n);
        assert!(
            h_raised < raised_pos.dot(n) - 1e-4,
            "raised vert must come DOWN toward the plane"
        );
        let h_flat = w.mesh.vert(VertexId(39)).position.dot(n);
        assert!(
            (h_flat - flat_y).abs() < 1e-5,
            "verts below the plane must not move (one-sided shave), drifted {}",
            h_flat - flat_y
        );
    }

    /// A dab whose footprint is off the mesh finds no verts → fail closed, no panic.
    #[test]
    fn poly_clay_fails_closed_off_mesh() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        let moved = app.poly_sculpt_apply_dabs(
            nid,
            &one_dab(Vec3::new(0.0, 5.0, 0.0), n, 0.4),
            VoxelBrush::Clay,
            1.0,
        );
        assert_eq!(moved, 0, "off-mesh Clay dab must be a clean no-op");
    }

    /// The kernel has no SnakeHook arm — the apply layer must stay a clean no-op (the
    /// input path gates it with a toast + both pickers grey it out). If someone ports
    /// Snake to poly, this pin flips and should be REPLACED by behavior tests.
    #[test]
    fn poly_snake_hook_apply_is_a_no_op() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        let moved =
            app.poly_sculpt_apply_dabs(nid, &one_dab(Vec3::ZERO, n, 0.5), VoxelBrush::SnakeHook, 1.0);
        assert_eq!(moved, 0, "SnakeHook has no poly kernel arm — must not half-work");
    }
}

// ============================================================================
// ACCUMULATE-OFF LAYER DEPOSIT (2026-08-18, feel gap #2 vs Blender).
//
// Blender's BRUSH_ACCUMULATE-unset default: one stroke deposits ONE offset layer
// measured from stroke-start positions — scribbling in place or overlapping dabs top a
// vertex up to the stroke's max fade and STOP, instead of stacking a tower (which is
// what AZRA always did = Blender's Accumulate-ON). Implemented as the per-stroke
// `deposit_applied` ledger threaded into the kernel's Draw/Erase/Inflate arms; the
// Brush Settings "Accumulate" checkbox (previously dead in poly mode) restores
// stacking. `begin_poly_sculpt_episode` = the stroke boundary that resets the layer.
// ============================================================================
#[cfg(test)]
mod layer_deposit_tests {
    use super::grab_tests::app_with_grid;
    use super::plane_brush_tests::{app_with_working, one_dab};
    use super::*;
    use core_math::voxel_sculpt::VoxelBrush;

    fn center_h(app: &crate::AzraApp, nid: NodeId, n: Vec3) -> f32 {
        app.poly_sculpt_working
            .get(&nid)
            .unwrap()
            .mesh
            .vert(VertexId(40))
            .position
            .dot(n)
    }

    /// A single dab at moderate strength — the kernel's Draw contract is BLENDER-EXACT
    /// since 2026-08-19: per-dab offset = `strength × radius × fade` (sculpt_brush_types.cc
    /// `SCULPT_do_draw_brush`), with the spacing-overlap attenuation applied UPSTREAM by
    /// the stroke producers. A raw strength-1.0 dab therefore displaces a full radius
    /// (exactly what Blender does at alpha 1 with spacing attenuation off) — these ledger
    /// tests use 0.2 so the vertex stays inside the brush sphere across dabs.
    fn soft_dab(pos: Vec3, n: Vec3, r: f32) -> Vec<crate::voxel_surface_sculpt::SurfaceDab> {
        let mut d = one_dab(pos, n, r);
        for dab in &mut d {
            dab.strength = 0.2;
        }
        d
    }

    /// Same-stroke re-application of the same dab must NOT stack: the ledger tops the
    /// vertex up to the stroke's max fade once, then holds.
    #[test]
    fn draw_layer_caps_within_a_stroke() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults(); // production seed: Accumulate OFF for poly
        let r = 0.5f32;
        let h0 = center_h(&app, nid, n);
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        let h1 = center_h(&app, nid, n);
        // Blender-exact: centre fade 1 → deposit = strength × radius = 0.2·r.
        assert!(
            (h1 - h0 - 0.2 * r).abs() < 0.02 * r,
            "first dab must lay strength·radius·fade (Blender draw), got {}",
            h1 - h0
        );
        for _ in 0..5 {
            app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        }
        let h2 = center_h(&app, nid, n);
        assert!(
            (h2 - h1).abs() < 1e-5,
            "rescrubbing the same spot in ONE stroke must not stack (drifted {})",
            h2 - h1
        );
    }

    /// A new stroke (episode) starts a new layer — the ledger is stroke-scoped.
    #[test]
    fn new_stroke_adds_a_second_layer() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults();
        let r = 0.5f32;
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        let h1 = center_h(&app, nid, n);
        app.begin_poly_sculpt_episode(nid, None); // stroke boundary = ledger reset
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        let h2 = center_h(&app, nid, n);
        assert!(
            h2 - h1 > 0.5 * h1,
            "a NEW stroke must deposit a fresh layer (h1 {h1}, h2 {h2})"
        );
    }

    /// The Accumulate checkbox restores classic per-dab stacking.
    #[test]
    fn accumulate_checkbox_restores_stacking() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults();
        app.sculpt.poly_tool.accumulate = true;
        let r = 0.5f32;
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        let h1 = center_h(&app, nid, n);
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        let h2 = center_h(&app, nid, n);
        assert!(
            h2 - h1 > 0.5 * h1,
            "Accumulate ON must stack per dab again (h1 {h1}, h2 {h2})"
        );
    }

    /// Inflate goes through its own kernel arm — the ledger must cap it too.
    #[test]
    fn inflate_layer_caps_within_a_stroke() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults();
        let r = 0.5f32;
        app.poly_sculpt_apply_dabs(nid, &one_dab(Vec3::ZERO, n, r), VoxelBrush::Inflate, 1.0);
        let h1 = center_h(&app, nid, n);
        app.poly_sculpt_apply_dabs(nid, &one_dab(Vec3::ZERO, n, r), VoxelBrush::Inflate, 1.0);
        let h2 = center_h(&app, nid, n);
        assert!(h1 > 1e-4, "Inflate must deposit on the first dab");
        assert!(
            (h2 - h1).abs() < 1e-5,
            "Inflate re-dab must not stack in one stroke (drifted {})",
            h2 - h1
        );
    }

    /// Blender `use_original` (calc_area_normal, `!accum`): the normals the brush math
    /// reads are FROZEN at the vert's first touch of the stroke — they must equal the
    /// pre-stroke mesh normals even after batches have displaced the surface and
    /// recomputed live normals. Reading live normals back is the feedback loop the owner
    /// diagnosed 2026-08-23 ("picking normal from their own result").
    #[test]
    fn brush_math_normals_freeze_at_stroke_start() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults(); // Accumulate OFF = use_original
        let pre: Vec<Vec3> = {
            let w = app.poly_sculpt_working.get(&nid).unwrap();
            (0..w.mesh.vertices.len() as u32)
                .map(|i| w.mesh.vert(VertexId(i)).normal)
                .collect()
        };
        let r = 0.5f32;
        for _ in 0..3 {
            app.poly_sculpt_apply_dabs(nid, &one_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        }
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        assert!(
            !w.first_touch_normal.is_empty(),
            "accumulate-off stroke must freeze brush-math normals"
        );
        for (&i, &frozen) in &w.first_touch_normal {
            assert_eq!(
                frozen, pre[i as usize],
                "frozen normal of vert {i} must be its PRE-stroke normal"
            );
        }
    }

    /// Blender inverted Smooth = Enhance Details over a STROKE-FROZEN direction field —
    /// growth per batch stays LINEAR (frozen dirs), never the compounding blow-up of a
    /// live inverse Laplacian, and the bump must actually get enhanced (taller).
    #[test]
    fn inverted_smooth_enhances_details_never_explodes() {
        let (mut app, nid) = app_with_grid();
        let n = app_with_working(&mut app, nid);
        app.sculpt.poly_ensure_defaults();
        let r = 0.5f32;
        // Raise a SOFT bump (0.2 — a full-strength dab would displace the apex exactly
        // one radius, OUTSIDE the brush sphere, same trap the ledger tests document),
        // then commit the stroke so enhance sees it as ground truth.
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Draw, 1.0);
        app.begin_poly_sculpt_episode(nid, None);
        let h0 = center_h(&app, nid, n);
        // Inverted smooth (subtract mode): sgn = -1.
        app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Smooth, -1.0);
        let h1 = center_h(&app, nid, n);
        let first_step = h1 - h0;
        assert!(
            first_step > 0.0,
            "enhance must move the bump apex AWAY from its neighbor average (grew {first_step})"
        );
        let mut prev = h1;
        let mut last_step = first_step;
        for _ in 0..9 {
            app.poly_sculpt_apply_dabs(nid, &soft_dab(Vec3::ZERO, n, r), VoxelBrush::Smooth, -1.0);
            let h = center_h(&app, nid, n);
            last_step = h - prev;
            prev = h;
            assert!(h.is_finite(), "enhance must never produce non-finite positions");
        }
        // Frozen dirs ⇒ per-batch increments stay ~constant. An expansive live inverse
        // Laplacian grows its increments batch over batch — that regression trips this.
        assert!(
            last_step <= first_step * 2.0 + 1e-6,
            "enhance increments must stay linear (first {first_step}, last {last_step})"
        );
    }
}

// ============================================================================
// SYMMETRIC GRAB (owner 2026-08-18: "move tool does not work with symmetrie").
//
// The anchored Grab bypassed the stamp path's octant mirror entirely. Fix: mirror the
// anchor across each enabled local axis at BEGIN, folding the instances into ONE per-axis
// signed weight per vert (see PolyGrabAnchor.weight), so the per-frame update rule is
// unchanged. Blend rule: mirrored verts move with the mirrored offset; a vert on the
// mirror plane loses its cross-plane component (stays welded to the plane).
// ============================================================================
#[cfg(test)]
mod grab_symmetry_tests {
    use super::grab_tests::app_with_grid;
    use super::*;

    /// A grab on the +X side with X-mirror on must move the −X side as its exact mirror.
    #[test]
    fn grab_mirrors_across_the_enabled_axis() {
        let (mut app, nid) = app_with_grid();
        app.sculpt.poly_symmetry = [true, false, false];
        let anchor = Vec3::new(0.5, 0.0, 0.0);
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.3), "grab must anchor");
        app.poly_grab_update(Vec3::new(0.2, 0.3, 0.0));
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let p_pos = w.mesh.vert(VertexId(42)).position; // was (0.5, 0, 0) — under the anchor
        let p_neg = w.mesh.vert(VertexId(38)).position; // was (-0.5, 0, 0) — the mirror vert
        assert!(
            (p_pos.x - 0.7).abs() < 1e-5 && (p_pos.y - 0.3).abs() < 1e-5,
            "anchor vert must get the full offset, got {p_pos:?}"
        );
        assert!(
            (p_neg.x + p_pos.x).abs() < 1e-6
                && (p_neg.y - p_pos.y).abs() < 1e-6
                && (p_neg.z - p_pos.z).abs() < 1e-6,
            "mirror side must be the exact X-mirror (pos {p_pos:?}, neg {p_neg:?})"
        );
    }

    /// Grabbing ON the mirror plane: the plane vert must stay on the plane (cross-plane
    /// component cancels) while the tangential pull applies exactly once (not doubled).
    #[test]
    fn on_plane_vert_stays_on_plane() {
        let (mut app, nid) = app_with_grid();
        app.sculpt.poly_symmetry = [true, false, false];
        assert!(app.poly_grab_begin(nid, Vec3::ZERO, Vec3::ZERO, 0.3), "grab must anchor");
        app.poly_grab_update(Vec3::new(0.2, 0.3, 0.0));
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let c = w.mesh.vert(VertexId(40)).position; // was the origin — on the mirror plane
        assert!(c.x.abs() < 1e-6, "on-plane vert must STAY on the plane, x = {}", c.x);
        assert!(
            (c.y - 0.3).abs() < 1e-5,
            "tangential pull must apply once (not doubled), y = {}",
            c.y
        );
    }

    /// Symmetry OFF must reproduce the classic scalar-falloff grab exactly.
    #[test]
    fn no_symmetry_matches_classic_grab() {
        let (mut app, nid) = app_with_grid();
        app.sculpt.poly_symmetry = [false, false, false];
        let anchor = Vec3::new(0.5, 0.0, 0.0);
        assert!(app.poly_grab_begin(nid, anchor, anchor, 0.3), "grab must anchor");
        app.poly_grab_update(Vec3::new(0.2, 0.3, 0.0));
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let p_pos = w.mesh.vert(VertexId(42)).position;
        let p_neg = w.mesh.vert(VertexId(38)).position;
        assert!(
            (p_pos.x - 0.7).abs() < 1e-5 && (p_pos.y - 0.3).abs() < 1e-5,
            "anchor vert must get the full offset, got {p_pos:?}"
        );
        assert_eq!(
            p_neg,
            Vec3::new(-0.5, 0.0, 0.0),
            "far side must not move without symmetry"
        );
    }
}

#[cfg(test)]
mod smooth_tests {
    use super::*;
    use core_math::half_edge::HalfEdgeId;
    use core_math::scene_graph::NodePayload;

    fn max_edge(m: &HalfEdgeMesh) -> f32 {
        let mut mx = 0.0f32;
        for i in 0..m.half_edges.len() as u32 {
            let h = HalfEdgeId(i);
            if !m.is_edge_live(h) {
                continue;
            }
            let twin = m.he(h).twin;
            if twin != core_math::half_edge::NO_TWIN && twin.0 < h.0 {
                continue;
            }
            let a = m.vert(m.he_source(h)).position;
            let b = m.vert(m.he(h).vertex).position;
            mx = mx.max((a - b).length());
        }
        mx
    }

    fn app_with(mesh: HalfEdgeMesh) -> (crate::AzraApp, NodeId) {
        let mut app = crate::AzraApp::new_headless();
        app.modeling_context = crate::ModelingContext::Polygonal;
        let root = app.scene.scene_graph.root();
        let nid = app.scene.scene_graph.add_child(
            root,
            NodePayload::PolygonalBody {
                name: "probe".into(),
                mesh: Arc::new(mesh),
                mesh_cache_id: None,
                material: core_math::primitives::PbrMaterial::default(),
                modifiers: Vec::new(),
                source: core_math::primitive_spec::MeshSource::Authored,
            },
        );
        app.select_single_unified(crate::SceneItemId::Node(nid));
        (app, nid)
    }

    fn cube() -> HalfEdgeMesh {
        let p = vec![
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let f = vec![
            vec![0, 1, 2, 3],
            vec![7, 6, 5, 4],
            vec![4, 5, 1, 0],
            vec![6, 7, 3, 2],
            vec![7, 4, 0, 3],
            vec![5, 6, 2, 1],
        ];
        HalfEdgeMesh::from_polygons(&p, &f)
    }

    #[test]
    fn smooth_relaxes_a_spike_without_leaving_the_plane() {
        let (mut app, nid) = grab_tests::app_with_grid();
        // Raise the centre vert. Smooth must pull it down, not slide the sheet.
        app.ensure_poly_sculpt_working(nid);
        let center_i = 4u32 * 9 + 4;
        {
            let w = app.poly_sculpt_working.get_mut(&nid).unwrap();
            w.mesh.vert_mut(VertexId(center_i)).position.y = 0.4;
        }
        let dab = crate::voxel_surface_sculpt::SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Y,
            radius: 0.8,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        };
        assert!(
            app.poly_sculpt_apply_dabs(
                nid,
                &[dab],
                core_math::voxel_sculpt::VoxelBrush::Smooth,
                1.0,
            ) > 0
        );
        let w = app.poly_sculpt_working.get(&nid).unwrap();
        let cy = w.mesh.vert(VertexId(center_i)).position.y;
        assert!(cy > 0.0 && cy < 0.4, "spike should relax toward the plane, got y={cy}");
        for i in 0..w.mesh.vertices.len() as u32 {
            if !w.mesh.is_vert_live(VertexId(i)) {
                continue;
            }
            let p = w.mesh.vert(VertexId(i)).position;
            let ox = (i % 9) as f32 * 0.25 - 1.0;
            let oz = (i / 9) as f32 * 0.25 - 1.0;
            assert!(
                (p.x - ox).abs() < 0.08 && (p.z - oz).abs() < 0.08,
                "tangential smooth must not slide vert {i} across the sheet: {p} vs ({ox},*,{oz})"
            );
        }
    }

    #[test]
    fn smooth_does_not_stretch_remesh_edges() {
        let base = cube();
        let remesh = crate::poly::remesh::voxel_remesh_mesh(&base, 16, false, 0.1, 0.0)
            .expect("remesh");
        let before = max_edge(&remesh);
        assert!(before > 0.0 && remesh.vertex_count() > 8);
        let (mut app, nid) = app_with(remesh);
        app.begin_surface_sculpt(nid);
        let dabs = [crate::voxel_surface_sculpt::SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Y,
            radius: 3.0,
            strength: 1.0,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        for _ in 0..8 {
            let _ = app.poly_sculpt_apply_dabs(
                nid,
                &dabs,
                core_math::voxel_sculpt::VoxelBrush::Smooth,
                1.0,
            );
        }
        let after = max_edge(&app.poly_sculpt_working.get(&nid).unwrap().mesh);
        assert!(
            after < before * 1.75,
            "Smooth must not stretch remesh edges into poles: max {before:.3} -> {after:.3}"
        );
    }
}

// ============================================================================
// DYNTOPO Phase B gate (DYNTOPO_PLAN.md) — a topology-changing stroke must land in the
// document, stay manifold, and undo/redo as ONE whole step.
// ============================================================================
#[cfg(test)]
mod dyntopo_stroke_tests {
    use super::*;
    use super::grab_tests::app_with_grid;
    use core_math::scene_graph::NodePayload;

    fn doc_counts(app: &crate::AzraApp, nid: NodeId) -> (usize, usize) {
        match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => (mesh.vertex_count(), mesh.face_count()),
            _ => unreachable!(),
        }
    }

    fn doc_vert_y(app: &crate::AzraApp, nid: NodeId, i: u32) -> f32 {
        match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => mesh.vert(VertexId(i)).position.y,
            _ => unreachable!(),
        }
    }

    /// One Draw stroke with Dyntopo ON: the brush region subdivides (doc gains verts),
    /// the mesh stays manifold, Ctrl+Z restores the EXACT pre-stroke topology + positions,
    /// redo restores the refined result — one undo step for the whole stroke.
    #[test]
    fn dyntopo_stroke_lands_with_whole_step_undo() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.sculpt.poly_ensure_defaults();
        app.sculpt.dyn_topo = true;
        app.sculpt.poly_dyntopo_brush_detail = false;
        app.sculpt.poly_dyntopo_const_detail = 0.08; // grid edges are 0.25 → must split
        app.sculpt.poly_dyntopo_collapse = true;
        let pre = doc_counts(&app, nid);
        let depth = app.history.undo_stack.len();

        app.begin_poly_sculpt_episode(nid, None);
        let dabs = [crate::voxel_surface_sculpt::SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Y,
            radius: 0.5,
            strength: 0.6,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        let moved = app.poly_sculpt_apply_dabs(
            nid,
            &dabs,
            core_math::voxel_sculpt::VoxelBrush::Draw,
            1.0,
        );
        assert!(moved > 0, "the dab must displace refined verts");
        {
            let w = app.poly_sculpt_working.get(&nid).unwrap();
            assert!(
                w.mesh.vertex_count() > pre.0,
                "dyntopo must refine the working under the brush"
            );
            w.mesh.is_manifold().expect("refined working must be manifold");
        }
        // Document untouched mid-stroke (working is truth).
        assert_eq!(doc_counts(&app, nid), pre, "no document write mid-stroke");

        app.commit_episode();
        let post = doc_counts(&app, nid);
        assert!(
            post.0 > pre.0 && post.1 > pre.1,
            "commit must land the refined topology in the document ({pre:?} → {post:?})"
        );
        assert_eq!(
            app.history.undo_stack.len(),
            depth + 1,
            "one stroke = ONE undo entry"
        );
        match &app.scene.scene_graph.get(nid).unwrap().payload {
            NodePayload::PolygonalBody { mesh, .. } => {
                mesh.is_manifold().expect("committed dyntopo mesh must be manifold")
            }
            _ => unreachable!(),
        }

        app.undo();
        assert_eq!(
            doc_counts(&app, nid),
            pre,
            "undo must restore the exact pre-stroke topology"
        );
        assert!(
            doc_vert_y(&app, nid, 40).abs() < 1e-6,
            "undo must restore pre-stroke positions too"
        );

        app.redo();
        assert_eq!(
            doc_counts(&app, nid),
            post,
            "redo must restore the refined topology"
        );
    }

    /// Dyntopo OFF: the same stroke keeps topology fixed and uses the sparse undo path —
    /// the Phase B wiring must not tax normal strokes.
    #[test]
    fn dyntopo_off_keeps_sparse_path() {
        let (mut app, nid) = app_with_grid();
        app.poly_sculpt_enter_edit(nid, usize::MAX);
        app.sculpt.dyn_topo = false;
        let pre = doc_counts(&app, nid);
        app.begin_poly_sculpt_episode(nid, None);
        let dabs = [crate::voxel_surface_sculpt::SurfaceDab {
            pos: Vec3::ZERO,
            dir: Vec3::Y,
            radius: 0.5,
            strength: 0.6,
            stroke_tan: Vec3::ZERO,
            tip_rotation: 0.0,
        }];
        app.poly_sculpt_apply_dabs(nid, &dabs, core_math::voxel_sculpt::VoxelBrush::Draw, 1.0);
        app.commit_episode();
        assert_eq!(doc_counts(&app, nid), pre, "no dyntopo → topology fixed");
        match &app.history.undo_stack.back().unwrap().payload {
            crate::UndoPayload::PolySculpt { orig, .. } => {
                assert!(!orig.is_empty(), "sparse stroke path must stay live")
            }
            _ => panic!("dyntopo-off stroke must stay a sparse PolySculpt entry"),
        }
    }
}
