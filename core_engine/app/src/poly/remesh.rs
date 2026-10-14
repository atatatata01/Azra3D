//! Voxel remesh for the poly modifier stack.
//!
//! Pipeline (MODIFIER_STACK_PIPELINE.md — poly → voxel → Surface Nets):
//!   1. Triangulate the input half-edge mesh (`to_raster_data` ear-clip path)
//!   2. Fit a tight cubic voxel lattice around the AABB (+ padding)
//!   3. Sample a signed distance field via `io_bridge::region_mesh_to_sdf`
//!   4. Map SDF → density with the sculpt convention (`0.5 − sdf/W`)
//!   5. Extract with `VoxelGrid::surface_nets` (the live sculpt remesher)
//!   6. Rebuild a `HalfEdgeMesh` via `from_mesh` (quad topology preserved)
//!
//! Intermediate voxel field is never rendered — Display Invariant holds.
//! Fail closed: empty / degenerate input or a failed sample returns `None`
//! and the stack keeps the previous mesh.

use core_math::half_edge::{FaceId, HalfEdgeMesh, VertexId, NO_EDGE};
use core_math::mesh_boolean::{boundary_half_edge_count, mesh_boolean, BoolOp};
use core_math::voxel_sculpt::VoxelGrid;
use glam::Vec3;
use std::collections::{HashMap, HashSet};

/// Hard resolution clamp for stack evaluation cost (Frame Budget Rule).
/// Raised 128 → 256 (owner 2026-08-08) then 256 → 512 (owner 2026-08-15, "remove 256
/// limitation"). 128³ is ~74ms live; 256³ is a few hundred ms; 512³ is a one-shot on
/// slider release (see [`REMESH_LIVE_SCRUB_MAX`]), not a per-notch freeze. Remesh uses
/// a transient dense `VoxelGrid` (`new_empty_dense`) even above `SPARSE_RES_THRESHOLD`
/// — brick-indirected extract is slower and the grid is freed at end of eval. The UI
/// slider range derives from these constants.
pub const REMESH_RES_MIN: u32 = 8;
pub const REMESH_RES_MAX: u32 = 512;

/// Above this resolution the panel defers re-evaluation to slider RELEASE while actively
/// dragging (a per-notch eval in the top range is 200–400ms — a frozen drag). At or below
/// it every notch re-evaluates live (≤ ~70ms). Display always shows committed geometry —
/// this defers evaluation, it never shows a draft mesh at a different resolution.
pub const REMESH_LIVE_SCRUB_MAX: u32 = 128;

/// Residual threshold for parent-first 2³ collapse. Matches the isolated S2 win
/// (S2+S4 24.5 → 15.3 ms @256). `0` is uniform QEF (bit-identical).
pub const REMESH_COLLAPSE_EPS: f32 = 1e-4;

/// Live remesh console (owner models, session tails). One line per stack eval.
/// Override with `AZRA_REMESH_CONSOLE`; default is the remesh-live worktree log.
pub fn remesh_console_log(line: &str) {
    let path = std::env::var("AZRA_REMESH_CONSOLE").unwrap_or_else(|_| {
        r"F:\PROJECT\FourthLAyerAi\.claude\worktrees\remesh-live-incr\remesh_console.log"
            .to_string()
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// Remesh a half-edge mesh through the voxel Surface-Nets path.
///
/// * `resolution` — grid cells along each axis (clamped to [`REMESH_RES_MIN`]..=[`REMESH_RES_MAX`]).
/// * `smooth` — Surface Nets relaxation + wide-stencil normals (sculpt release path).
/// * `padding` — fraction of AABB half-extent added as margin (a ≥2-cell air border is
///   enforced structurally regardless, so the sign flood's border contract always holds).
///
/// Semantics (band sampler, 2026-08-08 — industry voxel-remesh behavior):
/// * **Sealed internal voids FILL solid** (Blender/3D-Coat semantic). The previous full
///   sampler kept sealed voids as air and remeshed the inner shell.
/// * **Open meshes remesh as a thin crust** (~2-cell shell around the sheet, Blender's
///   mesh-to-volume behavior). The previous sampler solidified the pseudo-normal back
///   half-space — a different arbitrary result; closed input is the supported contract.
///
/// Returns `None` on empty/degenerate input or if extraction yields no surface.
/// * `project` — post-extract surface reprojection strength, 0..1 (0 = raw voxel output).
/// * `union_islands` — when true, fuse every mesh island into one body before voxelising
///   (boolean Union for closed solids, soup merge otherwise).
pub fn voxel_remesh_mesh(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    project: f32,
) -> Option<HalfEdgeMesh> {
    voxel_remesh_mesh_ex(mesh, resolution, smooth, padding, project, false)
}

/// Like [`voxel_remesh_mesh`] with the **Union islands** toggle.
pub fn voxel_remesh_mesh_ex(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    project: f32,
    union_islands: bool,
) -> Option<HalfEdgeMesh> {
    let owned;
    let input = if union_islands {
        owned = union_mesh_islands(mesh);
        &owned
    } else {
        mesh
    };
    let (mut out, _grid, _center, _enclosed, _projected, _clps) =
        voxel_remesh_full(input, resolution, smooth, padding, 1.0, project, false, false)?;
    transfer_face_materials(input, &mut out);
    Some(out)
}

/// Like [`voxel_remesh_mesh`] with **Union islands** and optional 2³ collapse.
pub fn voxel_remesh_mesh_collapse(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    project: f32,
    collapse: bool,
) -> Option<HalfEdgeMesh> {
    let (mut out, _grid, _center, _enclosed, _projected, _clps) =
        voxel_remesh_full(mesh, resolution, smooth, padding, 1.0, project, false, collapse)?;
    transfer_face_materials(mesh, &mut out);
    Some(out)
}

/// MATERIAL_PRESERVATION.md applied to the voxel remesh (owner 2026-08-19 "quand je
/// remesh, je perds mon matériau"): the extraction builds every face from the grid, so
/// per-face `material_id`s cannot index-map — transfer them SPATIALLY instead.
/// * Single-material input (the overwhelmingly common case): stamp that id on every
///   output face — O(F), exact.
/// * Multi-material input: each output face takes the material of the NEAREST input
///   face centroid (uniform grid hash on the input centroids, expected O(1) per query,
///   expanding-ring search so isolated islands still resolve).
/// Doing nothing repaints the whole body with the default material — the exact defect
/// class MATERIAL_PRESERVATION.md §1 closed for the Euler ops in 2026-08-07.
pub fn transfer_face_materials(src: &HalfEdgeMesh, out: &mut HalfEdgeMesh) {
    // Gather SURFACE SAMPLES of live source faces: centroid + every corner vert, each
    // tagged with the face's material. Corner samples matter on COARSE faces — a big
    // quad's centroid sits far from most of its surface, and centroid-only matching
    // painted wall faces with a neighbour's material near edges.
    let mut centroids: Vec<(Vec3, u32)> = Vec::new();
    let mut distinct: Option<u32> = None;
    let mut multi = false;
    for fi in 0..src.faces.len() as u32 {
        let f = FaceId(fi);
        if !src.is_face_live(f) {
            continue;
        }
        let vs = src.face_vertices(f);
        if vs.is_empty() {
            continue;
        }
        let c = vs.iter().map(|&v| src.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        let m = src.face(f).material_id;
        centroids.push((c, m));
        for &v in &vs {
            centroids.push((src.vert(v).position, m));
        }
        match distinct {
            None => distinct = Some(m),
            Some(d) if d != m => multi = true,
            _ => {}
        }
    }
    let Some(single) = distinct else { return }; // empty source — nothing to transfer
    if !multi {
        for fi in 0..out.faces.len() as u32 {
            let f = FaceId(fi);
            if out.is_face_live(f) {
                out.face_mut(f).material_id = single;
            }
        }
        return;
    }
    // Multi-material: uniform grid over the input centroids.
    let mut mn = Vec3::splat(f32::INFINITY);
    let mut mx = Vec3::splat(f32::NEG_INFINITY);
    for (c, _) in &centroids {
        mn = mn.min(*c);
        mx = mx.max(*c);
    }
    let extent = (mx - mn).max_element().max(1e-6);
    // ~cell per a few faces: extent / cbrt(n) keeps buckets small on dense meshes.
    let cell = (extent / (centroids.len() as f32).cbrt().max(1.0)).max(1e-4);
    let key = |p: Vec3| -> (i32, i32, i32) {
        (
            (p.x / cell).floor() as i32,
            (p.y / cell).floor() as i32,
            (p.z / cell).floor() as i32,
        )
    };
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
    for (i, (c, _)) in centroids.iter().enumerate() {
        grid.entry(key(*c)).or_default().push(i as u32);
    }
    for fi in 0..out.faces.len() as u32 {
        let f = FaceId(fi);
        if !out.is_face_live(f) {
            continue;
        }
        let vs = out.face_vertices(f);
        if vs.is_empty() {
            continue;
        }
        let c = vs.iter().map(|&v| out.vert(v).position).sum::<Vec3>() / vs.len() as f32;
        let (kx, ky, kz) = key(c);
        // Expanding ring search — ring 1 almost always hits; bounded for safety.
        let mut best: Option<(f32, u32)> = None;
        for ring in 1..=8i32 {
            for dx in -ring..=ring {
                for dy in -ring..=ring {
                    for dz in -ring..=ring {
                        if dx.abs() != ring && dy.abs() != ring && dz.abs() != ring && ring != 1 {
                            continue; // shell only after ring 1
                        }
                        if let Some(ids) = grid.get(&(kx + dx, ky + dy, kz + dz)) {
                            for &i in ids {
                                let (sc, m) = centroids[i as usize];
                                let d2 = (sc - c).length_squared();
                                if best.map_or(true, |(bd, _)| d2 < bd) {
                                    best = Some((d2, m));
                                }
                            }
                        }
                    }
                }
            }
            if best.is_some() && ring >= 2 {
                break; // one extra shell past the first hit = correct nearest
            }
        }
        if let Some((_, m)) = best {
            out.face_mut(f).material_id = m;
        }
    }
}

/// Partition live faces into connected islands (shared-vertex connectivity).
fn mesh_face_islands(mesh: &HalfEdgeMesh) -> Vec<HashSet<u32>> {
    // vert → face adjacency
    let mut vert_faces: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut all_faces: Vec<u32> = Vec::new();
    for fi in 0..mesh.faces.len() as u32 {
        if !mesh.is_face_live(FaceId(fi)) {
            continue;
        }
        all_faces.push(fi);
        for v in mesh.face_vertices(FaceId(fi)) {
            vert_faces.entry(v.0).or_default().push(fi);
        }
    }
    all_faces.sort_unstable();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut out: Vec<HashSet<u32>> = Vec::new();
    for &seed in &all_faces {
        if !seen.insert(seed) {
            continue;
        }
        let mut island = HashSet::new();
        let mut stack = vec![seed];
        island.insert(seed);
        while let Some(f) = stack.pop() {
            for v in mesh.face_vertices(FaceId(f)) {
                if let Some(nbrs) = vert_faces.get(&v.0) {
                    for &nf in nbrs {
                        if seen.insert(nf) {
                            island.insert(nf);
                            stack.push(nf);
                        }
                    }
                }
            }
        }
        out.push(island);
    }
    out
}

/// Extract one face set as a standalone half-edge mesh (positions + rings).
fn extract_face_island(mesh: &HalfEdgeMesh, faces: &HashSet<u32>) -> Option<HalfEdgeMesh> {
    let mut positions: Vec<Vec3> = Vec::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    let mut remap: HashMap<u32, usize> = HashMap::new();
    let mut ordered: Vec<u32> = faces.iter().copied().collect();
    ordered.sort_unstable();
    for fi in ordered {
        if mesh.face(FaceId(fi)).half_edge == NO_EDGE {
            continue;
        }
        let ring: Vec<usize> = mesh
            .face_vertices(FaceId(fi))
            .iter()
            .map(|v| {
                *remap.entry(v.0).or_insert_with(|| {
                    positions.push(mesh.vert(*v).position);
                    positions.len() - 1
                })
            })
            .collect();
        if ring.len() >= 3 {
            rings.push(ring);
        }
    }
    if rings.is_empty() {
        return None;
    }
    let out = HalfEdgeMesh::from_polygons(&positions, &rings);
    if out.face_count() == 0 {
        None
    } else {
        Some(out)
    }
}

/// Fuse every connected face island into one mesh before remesh.
///
/// * **Closed** islands → fold with solid **Boolean Union** (overlapping shells become one solid).
/// * Open islands or a refused union → soup **merge** (single lattice, multi-component OK).
///
/// One island → returns a clone of the input (cheap path for the common case).
pub fn union_mesh_islands(mesh: &HalfEdgeMesh) -> HalfEdgeMesh {
    let islands = mesh_face_islands(mesh);
    if islands.len() <= 1 {
        return mesh.clone();
    }
    let mut parts: Vec<HalfEdgeMesh> = Vec::with_capacity(islands.len());
    for isl in &islands {
        if let Some(p) = extract_face_island(mesh, isl) {
            parts.push(p);
        }
    }
    if parts.is_empty() {
        return mesh.clone();
    }
    if parts.len() == 1 {
        return parts.pop().unwrap();
    }
    // Largest first — reduces intermediate empty results when a small piece misses a big one.
    parts.sort_by_key(|m| std::cmp::Reverse(m.face_count()));
    let mut acc = parts.remove(0);
    for p in parts {
        let both_closed =
            boundary_half_edge_count(&acc) == 0 && boundary_half_edge_count(&p) == 0;
        if both_closed {
            if let Some(u) = mesh_boolean(&acc, &p, BoolOp::Union) {
                if u.face_count() > 0
                    && u.validate().is_ok()
                    && boundary_half_edge_count(&u) == 0
                {
                    acc = u;
                    continue;
                }
            }
        }
        // Open meshes or refused union: concatenate soups so one lattice still covers all.
        acc = crate::poly::boolean::merge_mesh_list(&[&acc, &p]);
    }
    acc
}

/// Cubic lattice fit for a mesh AABB: centered, user padding, and a STRUCTURAL border
/// guarantee independent of the padding (which may be 0): the band sampler's sign flood
/// seeds at the lattice border, so the border must be AIR. Enforce ≥2 cells of margin on
/// the max axis: half − 2·cell ≥ max_ext with cell = 2·half/(res−1) ⇒
/// half ≥ max_ext / (1 − 4/(res−1)). (res ≥ 8 keeps the denominator positive.) Without
/// this, a solid touching the border walls off its own openings — the flood would seal a
/// cup whose rim lies on the border plane. ONE implementation — the stateless path and
/// the incremental path must fit identically or the exact-lattice reuse test breaks.
fn region_air_count(grid: &VoxelGrid, region: [u32; 6]) -> usize {
    let iso = grid.iso;
    let mut n = 0usize;
    for z in region[2]..=region[5] {
        for y in region[1]..=region[4] {
            for x in region[0]..=region[3] {
                if grid.density.get_idx(grid.flat_index(x, y, z)) < iso {
                    n += 1;
                }
            }
        }
    }
    n
}

fn remesh_lattice(mn: Vec3, mx: Vec3, res: u32, padding: f32) -> (Vec3, f32) {
    let center = (mn + mx) * 0.5;
    let pad = padding.clamp(0.0, 1.0);
    let max_ext = ((mx - mn) * 0.5).max_element().max(1e-3);
    let half = max_ext * (1.0 + pad);
    let half = half.max(max_ext / (1.0 - 4.0 / (res as f32 - 1.0)));
    (center, half)
}

/// How S3 (reproject onto the source) runs on this extract.
///
/// The incremental path must NOT leave clean-region verts at their raw QEF
/// positions — that would flash lattice stair-steps outside the edit. Clean verts
/// keep the previous projected positions; only the dirty box is re-projected.
/// Vertex-count mismatch or a project-slider change falls back to a full project
/// (fail closed, still live).
enum ProjectScope<'a> {
    /// Project every extracted vertex onto the whole source (stateless / first frame).
    Full,
    /// Nothing moved and the project slider is unchanged: keep last projected verts.
    ReuseAll { prev: &'a [Vec3] },
    /// Re-project verts inside `mn..=mx` (already dilated); copy `prev` elsewhere.
    Region { prev: &'a [Vec3], mn: Vec3, mx: Vec3 },
}

/// Source triangles whose face AABB overlaps `[mn, mx]`. Ear-clips only those
/// faces — the incremental band sample and regional project do not need the rest
/// of a dense cage. `None` if nothing overlaps (caller fails closed).
fn source_tris_overlapping(
    mesh: &HalfEdgeMesh,
    mn: Vec3,
    mx: Vec3,
) -> Option<(Vec<Vec3>, Vec<u32>)> {
    let live_faces = mesh.face_count();
    let mut verts: Vec<Vec3> = Vec::with_capacity(live_faces * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(live_faces * 6);
    let mut face_verts: Vec<VertexId> = Vec::with_capacity(8);
    let mut pts: Vec<Vec3> = Vec::with_capacity(8);
    for fi in 0..mesh.faces.len() as u32 {
        if !mesh.is_face_live(FaceId(fi)) {
            continue;
        }
        mesh.face_vertices_into(FaceId(fi), &mut face_verts);
        if face_verts.len() < 3 {
            continue;
        }
        pts.clear();
        let mut fmn = Vec3::splat(f32::INFINITY);
        let mut fmx = Vec3::splat(f32::NEG_INFINITY);
        for &vid in &face_verts {
            let p = mesh.vert(vid).position;
            pts.push(p);
            fmn = fmn.min(p);
            fmx = fmx.max(p);
        }
        if fmx.x < mn.x || fmn.x > mx.x
            || fmx.y < mn.y || fmn.y > mx.y
            || fmx.z < mn.z || fmn.z > mx.z
        {
            continue;
        }
        let face_n = mesh.compute_face_normal(FaceId(fi));
        let base = verts.len() as u32;
        verts.extend_from_slice(&pts);
        for t in core_math::poly_mesh::earclip_polygon(&pts, face_n) {
            let (a, b, c) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            if (b - a).cross(c - a).length_squared() <= 1e-20 {
                continue;
            }
            indices.push(base + t[0]);
            indices.push(base + t[1]);
            indices.push(base + t[2]);
        }
    }
    if indices.len() < 3 {
        None
    } else {
        Some((verts, indices))
    }
}

fn vert_in_box(p: Vec3, mn: Vec3, mx: Vec3) -> bool {
    p.x >= mn.x && p.x <= mx.x && p.y >= mn.y && p.y <= mx.y && p.z >= mn.z && p.z <= mx.z
}

/// Surface-Nets extract + half-edge rebuild from a written grid (the shared tail of the
/// stateless and incremental paths). `None` if extraction yields no surface.
fn extract_half_edge(
    grid: &VoxelGrid,
    center: Vec3,
    smooth: bool,
    src: &HalfEdgeMesh,
    project: f32,
    // REGIONAL EXTRACT (Gate 1b). `Some` only on the incremental path: reuse the retained Phase A
    // slabs outside `dirty` instead of re-solving every surface cell. `None` keeps the stateless
    // path byte-for-byte on the original code path.
    mut cache: Option<(&mut core_math::voxel_sculpt::SurfaceNetsCache, core_math::voxel_sculpt::DirtyCells)>,
    scope: ProjectScope<'_>,
    out_projected: Option<&mut Vec<Vec3>>,
    // A: last projected positions per cz slab. None on the stateless path.
    proj_slabs: Option<&mut Vec<Vec<Vec3>>>,
    // B: episode drag uses from_mesh_draft (no twin-link). Release uses from_mesh.
    episode_active: bool,
    collapse: bool,
    mut collapse_cache: Option<(
        &mut core_math::voxel_sculpt::OctreeCollapseCache,
        core_math::voxel_sculpt::DirtyCells,
    )>,
) -> Option<HalfEdgeMesh> {
    // Collapse remumbers verts vs uniform cz slabs, so the uniform extract
    // cache cannot be reused. Parent-even-z slabs are stable — reuse those.
    let mut extracted = if collapse {
        if let Some((cc, dirty)) = collapse_cache.as_mut() {
            grid.surface_nets_octree_cached(smooth, REMESH_COLLAPSE_EPS, cc, *dirty)
        } else {
            grid.surface_nets_octree(smooth, REMESH_COLLAPSE_EPS)
        }
    } else if let Some((c, dirty)) = cache.as_mut() {
        grid.surface_nets_placed_cached(
            smooth,
            core_math::voxel_sculpt::CellPlacement::Qef,
            c,
            *dirty,
        )
    } else {
        grid.surface_nets_placed(smooth, core_math::voxel_sculpt::CellPlacement::Qef)
    };
    if extracted.quads.is_empty() || extracted.vertices.is_empty() {
        return None;
    }
    // Restore original local-space origin.
    for v in &mut extracted.vertices {
        *v += center;
    }
    // SURFACE REPROJECTION. Voxelising quantises the surface to the lattice; snapping back to the
    // source triangles undoes that quantisation without needing a finer lattice (memory makes
    // 512³ a 537 MB grid, so resolution is not the lever). Bounded by `max_dist` so it only ever
    // undoes quantisation, never the remesh itself — see `project_points_to_mesh`.
    let k = project.clamp(0.0, 1.0);
    let max_dist = grid.voxel_size() * PROJECT_MAX_CELLS;
    // A: when the Phase A cache is live, project by cz slab (stable cell key), not compact
    // vertex index. A fast drag that flips a cell remumbers the extract; reused slabs still
    // keep last projected positions. Collapse uses parent-then-leftover slabs the same way.
    let used_slab_s3 = if collapse {
        if let Some((cc, _)) = collapse_cache.as_mut() {
            if cc.primed && !matches!(scope, ProjectScope::ReuseAll { .. }) {
                let full = matches!(scope, ProjectScope::Full)
                    || (cc.parent_recompute_lo == 0
                        && cc.parent_recompute_hi >= cc.parent_slab_count()
                        && cc.leftover_recompute_lo == 0
                        && cc.leftover_recompute_hi >= cc.leftover_slab_count());
                let clip = if full {
                    None
                } else {
                    match scope {
                        ProjectScope::Region { mn, mx, .. } => {
                            let pad = Vec3::splat(max_dist);
                            Some((mn - pad, mx + pad))
                        }
                        _ => None,
                    }
                };
                apply_collapse_slab_project(&mut extracted, src, k, max_dist, cc, clip, full);
                true
            } else {
                false
            }
        } else {
            false
        }
    } else if let (Some((sn, _)), Some(ps)) = (cache.as_ref(), proj_slabs) {
        if sn.primed && !matches!(scope, ProjectScope::ReuseAll { .. }) {
            // Clip source tris only when a SUBSET of slabs recomputed. A full-slab
            // recompute (cold cache) must see every source triangle or far verts
            // snap to the wrong surface (incremental_engages_on_a_closed_solid).
            let counts_n = sn.slab_vert_counts().len();
            let full_recompute = sn.recompute_lo == 0 && sn.recompute_hi >= counts_n;
            let clip = if full_recompute {
                None
            } else {
                match scope {
                    ProjectScope::Region { mn, mx, .. } => {
                        let pad = Vec3::splat(max_dist);
                        Some((mn - pad, mx + pad))
                    }
                    _ => None,
                }
            };
            apply_slab_project(&mut extracted, src, k, max_dist, sn, ps, clip);
            true
        } else {
            false
        }
    } else {
        false
    };
    if !used_slab_s3 {
        match scope {
            ProjectScope::ReuseAll { prev } if prev.len() == extracted.vertices.len() => {
                extracted.vertices.copy_from_slice(prev);
            }
            ProjectScope::Region { prev, mn, mx }
                if prev.len() == extracted.vertices.len() && k > 1e-4 =>
            {
                let pad = Vec3::splat(max_dist);
                let lo = mn - pad;
                let hi = mx + pad;
                let mut dirty_idx: Vec<usize> = Vec::new();
                let mut dirty_pts: Vec<Vec3> = Vec::new();
                for i in 0..extracted.vertices.len() {
                    let v = extracted.vertices[i];
                    if vert_in_box(v, lo, hi) {
                        dirty_idx.push(i);
                        dirty_pts.push(v);
                    } else {
                        extracted.vertices[i] = prev[i];
                    }
                }
                if !dirty_pts.is_empty() {
                    if let Some((sv, indices)) = source_tris_overlapping(src, lo, hi) {
                        if let Some(snapped) = io_bridge::mesh_to_sdf::project_points_to_mesh_filtered(
                            &dirty_pts,
                            &sv,
                            &indices,
                            max_dist,
                            Some((lo, hi)),
                        ) {
                            for (di, s) in dirty_idx.iter().zip(snapped.iter()) {
                                extracted.vertices[*di] = extracted.vertices[*di].lerp(*s, k);
                            }
                        }
                    }
                }
            }
            _ if k > 1e-4 => {
                let (raster, indices) = src.to_raster_data();
                if !raster.is_empty() && indices.len() >= 3 {
                    let sv: Vec<Vec3> = raster.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
                    if let Some(snapped) = io_bridge::mesh_to_sdf::project_points_to_mesh(
                        &extracted.vertices,
                        &sv,
                        &indices,
                        max_dist,
                    ) {
                        for (v, s) in extracted.vertices.iter_mut().zip(snapped.iter()) {
                            *v = v.lerp(*s, k);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(out_p) = out_projected {
        out_p.clear();
        out_p.extend_from_slice(&extracted.vertices);
    }
    // B: twin-link is ~60% of from_mesh and display does not read twins. Rebuild a
    // real manifold on episode commit (`episode_active == false`).
    let out = if episode_active {
        HalfEdgeMesh::from_mesh_draft(&extracted)
    } else {
        HalfEdgeMesh::from_mesh(&extracted)
    };
    if out.face_count() == 0 {
        return None;
    }
    Some(out)
}

/// Project only verts from recomputed cz slabs; copy last projected positions into reused slabs.
fn apply_slab_project(
    extracted: &mut core_math::mesh::Mesh,
    src: &HalfEdgeMesh,
    k: f32,
    max_dist: f32,
    sn: &core_math::voxel_sculpt::SurfaceNetsCache,
    proj_slabs: &mut Vec<Vec<Vec3>>,
    clip: Option<(Vec3, Vec3)>,
) {
    let counts = sn.slab_vert_counts();
    if proj_slabs.len() != counts.len() {
        *proj_slabs = vec![Vec::new(); counts.len()];
    }
    let rlo = sn.recompute_lo;
    let rhi = sn.recompute_hi.min(counts.len());
    let mut dirty_idx: Vec<usize> = Vec::new();
    let mut dirty_pts: Vec<Vec3> = Vec::new();
    let mut vi = 0usize;
    for (cz, &n) in counts.iter().enumerate() {
        let reuse = cz < rlo || cz >= rhi;
        if reuse && proj_slabs[cz].len() == n && n > 0 {
            extracted.vertices[vi..vi + n].copy_from_slice(&proj_slabs[cz]);
        } else if k > 1e-4 && n > 0 {
            for j in 0..n {
                dirty_idx.push(vi + j);
                dirty_pts.push(extracted.vertices[vi + j]);
            }
        }
        vi += n;
    }
    if !dirty_pts.is_empty() {
        let soup = if let Some((mn, mx)) = clip {
            source_tris_overlapping(src, mn, mx)
        } else {
            let (raster, indices) = src.to_raster_data();
            if raster.is_empty() || indices.len() < 3 {
                None
            } else {
                Some((
                    raster.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect(),
                    indices,
                ))
            }
        };
        if let Some((sv, indices)) = soup {
            if let Some(snapped) = io_bridge::mesh_to_sdf::project_points_to_mesh_filtered(
                &dirty_pts,
                &sv,
                &indices,
                max_dist,
                clip,
            ) {
                for (di, s) in dirty_idx.iter().zip(snapped.iter()) {
                    extracted.vertices[*di] = extracted.vertices[*di].lerp(*s, k);
                }
            }
        }
    }
    vi = 0;
    for (cz, &n) in counts.iter().enumerate() {
        if cz >= rlo && cz < rhi {
            proj_slabs[cz] = extracted.vertices[vi..vi + n].to_vec();
        }
        vi += n;
    }
}

/// Project dirty parent/leftover collapse slabs; copy last snaps into reused slabs.
/// Emission order matches extract: all parent even-z slabs, then leftover cz.
fn apply_collapse_slab_project(
    extracted: &mut core_math::mesh::Mesh,
    src: &HalfEdgeMesh,
    k: f32,
    max_dist: f32,
    cc: &mut core_math::voxel_sculpt::OctreeCollapseCache,
    clip: Option<(Vec3, Vec3)>,
    force_all: bool,
) {
    let pc = cc.parent_vert_counts();
    let lc = cc.leftover_vert_counts();
    if cc.proj_parent.len() != pc.len() {
        cc.proj_parent = vec![Vec::new(); pc.len()];
    }
    if cc.proj_leftover.len() != lc.len() {
        cc.proj_leftover = vec![Vec::new(); lc.len()];
    }
    let p_lo = if force_all { 0 } else { cc.parent_recompute_lo };
    let p_hi = if force_all {
        pc.len()
    } else {
        cc.parent_recompute_hi.min(pc.len())
    };
    let l_lo = if force_all { 0 } else { cc.leftover_recompute_lo };
    let l_hi = if force_all {
        lc.len()
    } else {
        cc.leftover_recompute_hi.min(lc.len())
    };
    let mut dirty_idx: Vec<usize> = Vec::new();
    let mut dirty_pts: Vec<Vec3> = Vec::new();
    let mut vi = 0usize;
    for (i, &n) in pc.iter().enumerate() {
        let reuse = i < p_lo || i >= p_hi;
        if reuse && cc.proj_parent[i].len() == n && n > 0 {
            extracted.vertices[vi..vi + n].copy_from_slice(&cc.proj_parent[i]);
        } else if k > 1e-4 && n > 0 {
            for j in 0..n {
                dirty_idx.push(vi + j);
                dirty_pts.push(extracted.vertices[vi + j]);
            }
        }
        vi += n;
    }
    for (i, &n) in lc.iter().enumerate() {
        let reuse = i < l_lo || i >= l_hi;
        if reuse && cc.proj_leftover[i].len() == n && n > 0 {
            extracted.vertices[vi..vi + n].copy_from_slice(&cc.proj_leftover[i]);
        } else if k > 1e-4 && n > 0 {
            for j in 0..n {
                dirty_idx.push(vi + j);
                dirty_pts.push(extracted.vertices[vi + j]);
            }
        }
        vi += n;
    }
    debug_assert_eq!(vi, extracted.vertices.len());
    cc.last_project_dirty_verts = dirty_pts.len();
    if !dirty_pts.is_empty() {
        let soup = if let Some((mn, mx)) = clip {
            source_tris_overlapping(src, mn, mx)
        } else {
            let (raster, indices) = src.to_raster_data();
            if raster.is_empty() || indices.len() < 3 {
                None
            } else {
                Some((
                    raster.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect(),
                    indices,
                ))
            }
        };
        if let Some((sv, indices)) = soup {
            if let Some(snapped) = io_bridge::mesh_to_sdf::project_points_to_mesh_filtered(
                &dirty_pts,
                &sv,
                &indices,
                max_dist,
                clip,
            ) {
                for (di, s) in dirty_idx.iter().zip(snapped.iter()) {
                    extracted.vertices[*di] = extracted.vertices[*di].lerp(*s, k);
                }
            }
        }
    }
    vi = 0;
    for (i, &n) in pc.iter().enumerate() {
        if i >= p_lo && i < p_hi {
            cc.proj_parent[i] = extracted.vertices[vi..vi + n].to_vec();
        }
        vi += n;
    }
    for (i, &n) in lc.iter().enumerate() {
        if i >= l_lo && i < l_hi {
            cc.proj_leftover[i] = extracted.vertices[vi..vi + n].to_vec();
        }
        vi += n;
    }
}

/// Reprojection travel limit, in cells. Sized just above the cell diagonal (√3 ≈ 1.73) so every
/// vertex displaced purely by lattice quantisation can reach its source surface, while anything
/// further — a sealed void's wall, a bridged gap — stays put.
const PROJECT_MAX_CELLS: f32 = 1.8;

/// Full stateless remesh, also returning the written grid + lattice center so the
/// incremental path can retain them as its cache. `lattice_headroom` scales the fitted
/// half-extent (1.0 = canonical fit): the incremental path passes 1.25 when filling its
/// cache mid-episode, because the canonical structural fit leaves EXACTLY the 2-cell air
/// margin — zero headroom for a drag to grow the mesh — while an inflated lattice gives
/// the latch ~25% growth room. Headroom > 1.0 is by definition `diverged` (a fresh
/// stateless eval would fit tighter) and is reconciled at episode commit.
fn voxel_remesh_full(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    lattice_headroom: f32,
    project: f32,
    // B: first-frame / fallback mid-drag uses from_mesh_draft; commit uses from_mesh.
    episode_active: bool,
    collapse: bool,
) -> Option<(
    HalfEdgeMesh,
    VoxelGrid,
    Vec3,
    usize,
    Vec<Vec3>,
    core_math::voxel_sculpt::OctreeCollapseCache,
)> {
    REMESH_FULL_EVALS.with(|c| c.set(c.get().saturating_add(1)));
    if mesh.face_count() == 0 {
        return None;
    }
    let res = resolution.clamp(REMESH_RES_MIN, REMESH_RES_MAX);
    let (raster, indices) = mesh.to_raster_data();
    if raster.is_empty() || indices.len() < 3 {
        return None;
    }
    let verts: Vec<Vec3> = raster
        .iter()
        .map(|v| Vec3::new(v[0], v[1], v[2]))
        .collect();

    let mut mn = Vec3::splat(f32::INFINITY);
    let mut mx = Vec3::splat(f32::NEG_INFINITY);
    for v in &verts {
        mn = mn.min(*v);
        mx = mx.max(*v);
    }
    if !mn.is_finite() || !mx.is_finite() {
        return None;
    }
    let (center, half) = remesh_lattice(mn, mx, res, padding);
    let half = half * lattice_headroom.max(1.0);

    // Lattice is shape-local and centered: p_i = -half + i * cell (region_mesh_to_sdf contract).
    let local: Vec<Vec3> = verts.iter().map(|v| *v - center).collect();
    // Dense storage always: this grid is transient (one voxelize→extract round trip), and
    // at ≥256 `new_empty` would cross SPARSE_RES_THRESHOLD — brick-indirected reads slow the
    // extract ~2× for zero benefit on a grid freed at end of eval (512³ dense ≈ 536 MB).
    let mut grid = VoxelGrid::new_empty_dense(res, half);
    // Same SDF→density band as the sculpt MOVE re-voxelize (`write_region_from_sdf` docs).
    let w = 2.0 * grid.voxel_size();
    // NARROW-BAND sampler with band = w: `write_region_from_sdf` saturates at |d| ≥ w/2, so
    // exact distances beyond w carry zero information — the band path skips them (exact in
    // band, flood-classified sign outside; golden-gated bit-identical densities in
    // io_bridge/tests/band_sdf_golden.rs). This is the live-edit hot path: full sampling was
    // O(res³ × tris) — 1.2–5s at 128³ — vs ~O(surface) here.
    let (dist, sign_stats) = io_bridge::mesh_to_sdf::region_mesh_to_sdf_band_stats(
        &local,
        &indices,
        half,
        res,
        [0, 0, 0, res - 1, res - 1, res - 1],
        w,
    )?;
    if dist.len() != (res as usize).pow(3) {
        return None;
    }
    // want_delta=false: this grid is transient and undo belongs to the modifier stack, so
    // the per-voxel delta (~134 MB at 256³) is pure waste.
    grid.write_region_from_sdf_ex([0, 0, 0, res - 1, res - 1, res - 1], &dist, w, false);

    let mut projected = Vec::new();
    let mut collapse_extract = core_math::voxel_sculpt::OctreeCollapseCache::default();
    let out = extract_half_edge(
        &grid,
        center,
        smooth,
        mesh,
        project,
        None,
        ProjectScope::Full,
        Some(&mut projected),
        None,
        episode_active,
        collapse,
        if collapse {
            Some((
                &mut collapse_extract,
                core_math::voxel_sculpt::DirtyCells::All,
            ))
        } else {
            None
        },
    )?;
    // 4th element is the SEALED-POCKET count only — NOT `far_solid`, which for any closed solid is
    // its whole interior. Gating the incremental lockout on the combined count is REMESHSOLIDNOINCR:
    // it fired on the first eval of every solid mesh and never released (see `BandSignStats`).
    Some((out, grid, center, sign_stats.sealed_pocket, projected, collapse_extract))
}

/// V1 INCREMENTAL remesh state — retained between stack evaluations for ONE (body, slot).
/// Runtime-only (never serialized; PolyMesh-wrapper rule). Holds the written voxel grid
/// (~8 MB @128, ~67 MB @256 dense) plus the previous INPUT vertex snapshot for diffing.
/// Lifecycle: created on any remesh eval through the cached display walk; replaced when a
/// different (body, slot) remeshes; dropped at episode commit (`commit_episode` reconcile),
/// with a one-shot stateless resync when the latched lattice diverged.
pub struct RemeshIncrCache {
    pub nid: u64,
    pub slot: usize,
    pub res: u32,
    pub smooth: bool,
    pub padding_bits: u32,
    pub collapse: bool,
    /// Latched lattice center (half lives in `grid.bounds`).
    pub center: Vec3,
    /// Previous input positions, raw vertex-slot order (free slots ride along unchanged).
    pub prev_positions: Vec<Vec3>,
    /// (half-edge slots, face slots) — with the vertex-slot count, a cheap same-topology proxy.
    pub prev_counts: (usize, usize),
    pub grid: VoxelGrid,
    /// Latched lattice differs from a fresh stateless fit — display must reconcile with
    /// one stateless eval when the interaction episode commits.
    pub diverged: bool,
    /// The field contains a SEALED VOID that was filled solid. Void filling is one-way
    /// (`seal_enclosed_air`), so a later edit that RE-OPENS that void can never be
    /// reproduced by a local region splice — such a field must always take the full
    /// stateless path. Fail-closed: set by the full path when the sampler reports fills,
    /// and by the incremental path whenever its post-splice whole-field seal fills anything.
    pub had_enclosed_void: bool,
    /// Diagnostics / tests: whether the last eval took the incremental path.
    pub last_eval_incremental: bool,
    /// Retained Phase A (per-cell vertex) output for the REGIONAL extract — Gate 1b. Runtime-only,
    /// like the rest of this struct. Lives here and nowhere else on purpose: every `break 'incr`
    /// arm falls through to `voxel_remesh_full`, which REPLACES this whole struct, so parameter
    /// changes (res / smooth / padding / body / topology / sealed-void) invalidate the cache
    /// structurally rather than needing their own checks.
    pub extract: core_math::voxel_sculpt::SurfaceNetsCache,
    /// Last projected output positions (local/world, same space as `extract_half_edge` after
    /// `+ center`). Lets the next incremental frame skip S3 on verts outside the dirty box
    /// without flashing raw QEF stair-steps on the rest of the surface.
    pub prev_projected: Vec<Vec3>,
    /// Bits of the last `project` slider. A change forces a full re-project (the dirty-box
    /// splice would otherwise keep the old strength on clean verts and freeze the slider).
    pub project_bits: u32,
    /// Last projected positions per cz slab (A: slab-keyed S3). Same length as
    /// `extract.slabs`; survives compact-index reshuffles when a dirty slab flips a cell.
    pub proj_slabs: Vec<Vec<Vec3>>,
    /// Parent-first 2³ collapse extract cache. Used only when `collapse` is on.
    pub collapse_extract: core_math::voxel_sculpt::OctreeCollapseCache,
    /// Drag skipped `seal_enclosed_air`; commit must run it once.
    pub seal_deferred: bool,
}

/// Live-console skip reason for the last incremental miss (`""` if the last eval was incr).
thread_local! {
    static LAST_INCR_SKIP: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

pub fn last_incr_skip() -> &'static str {
    LAST_INCR_SKIP.with(|c| c.get())
}

fn incr_skip(why: &'static str) {
    LAST_INCR_SKIP.with(|c| c.set(why));
}

thread_local! {
    static LAST_INCR_SEALED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn last_incr_sealed() -> bool {
    LAST_INCR_SEALED.with(|c| c.get())
}

fn incr_sealed(ran: bool) {
    LAST_INCR_SEALED.with(|c| c.set(ran));
}

thread_local! {
    static REMESH_FULL_EVALS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub fn remesh_full_eval_count() -> u32 {
    REMESH_FULL_EVALS.with(|c| c.get())
}

pub fn reset_remesh_full_eval_count() {
    REMESH_FULL_EVALS.with(|c| c.set(0));
}

/// Was 0.5: a whole-body grab dirties ~100% of the lattice and fell through to a full
/// remesh every frame (~60 ms @180) plus a lattice rebuild. Incremental on a large
/// dirty region still keeps the lattice and can use from_mesh_draft — same voxelize,
/// no 1.25/1.0 pop, no extra commit remesh.
const REMESH_INCR_MAX_DIRTY_FRAC: f32 = 1.01;

/// Incremental voxel remesh: same output contract as [`voxel_remesh_mesh`], but reuses the
/// retained grid when only vertex POSITIONS of the input changed — resampling the SDF only
/// in the dirty region (seeded-border band sampler) instead of the whole lattice.
///
/// Lattice policy (the correctness core):
/// * If the fresh stateless fit is BIT-IDENTICAL to the latched lattice (edits that keep
///   the input AABB), the incremental result is bit-identical to a stateless eval —
///   golden-gated by `incremental_interior_nudge_is_bit_identical`.
/// * If the AABB changed, the latched lattice is reused ONLY during an interaction episode
///   (`episode_active`, the InteractionRenderPolicy pattern: latch during the drag), and
///   the cache is marked `diverged` — the episode commit reconciles with one stateless
///   eval so saved/reloaded scenes never see a latched lattice.
/// * Anything else (topology change, param change, other body, dirty region > 50% of the
///   lattice, mesh outgrew the latch) falls back to the full stateless path and refreshes
///   the cache.
#[allow(clippy::too_many_arguments)]
pub fn voxel_remesh_mesh_incremental(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    project: f32,
    nid: u64,
    slot: usize,
    episode_active: bool,
    cache: &mut Option<RemeshIncrCache>,
) -> Option<HalfEdgeMesh> {
    voxel_remesh_mesh_incremental_ex(
        mesh,
        resolution,
        smooth,
        padding,
        project,
        false,
        false,
        nid,
        slot,
        episode_active,
        false,
        false,
        cache,
    )
}

/// Incremental remesh with the **Union islands** pre-pass (same contract as
/// [`voxel_remesh_mesh_ex`]).
#[allow(clippy::too_many_arguments)]
pub fn voxel_remesh_mesh_incremental_ex(
    mesh: &HalfEdgeMesh,
    resolution: u32,
    smooth: bool,
    padding: f32,
    project: f32,
    union_islands: bool,
    collapse: bool,
    nid: u64,
    slot: usize,
    episode_active: bool,
    skip_relax_drag: bool,
    skip_seal_drag: bool,
    cache: &mut Option<RemeshIncrCache>,
) -> Option<HalfEdgeMesh> {
    use core_math::half_edge::FaceId;
    // Island union changes topology; run it once up front so the cache keys on the fused mesh.
    let owned;
    let mesh = if union_islands {
        owned = union_mesh_islands(mesh);
        &owned
    } else {
        mesh
    };
    let res = resolution.clamp(REMESH_RES_MIN, REMESH_RES_MAX);
    // Authored `smooth` stays the cache key so mouse-up does not skip=key.
    // Drag extract omits relax when the skip-relax toggle is on.
    let extract_smooth = smooth && !(episode_active && skip_relax_drag);
    incr_skip("");
    incr_sealed(false);

    'incr: {
        let Some(c) = cache.as_mut() else {
            incr_skip("nocache");
            break 'incr;
        };
        if c.nid != nid
            || c.slot != slot
            || c.res != res
            || c.smooth != smooth
            || c.padding_bits != padding.to_bits()
            || c.collapse != collapse
        {
            incr_skip("key");
            break 'incr;
        }
        // Topology (extrude / subdivide) is a different skip from res/params: a deformed
        // solid's lining noise must not acquire the void lock on an extrude (session
        // 2026-08-13: skip=key after 12→36 verts latched skip=void for the rest of the grab).
        if c.prev_positions.len() != mesh.vertices.len()
            || c.prev_counts != (mesh.half_edges.len(), mesh.faces.len())
        {
            incr_skip("topo");
            break 'incr;
        }
        if c.had_enclosed_void {
            incr_skip("void");
            break 'incr;
        }

        // One pass over live faces: mesh AABB (live corners only — free vertex slots hold
        // stale positions) + dirty AABB = union of old+new positions of every vertex of
        // every face touching a moved vertex (a moved TRIANGLE's whole old+new extent
        // matters, not just the moved vertex's).
        let changed: Vec<bool> = mesh
            .vertices
            .iter()
            .zip(&c.prev_positions)
            .map(|(v, p)| v.position != *p)
            .collect();
        let mut mesh_mn = Vec3::splat(f32::INFINITY);
        let mut mesh_mx = Vec3::splat(f32::NEG_INFINITY);
        let mut d_mn = Vec3::splat(f32::INFINITY);
        let mut d_mx = Vec3::splat(f32::NEG_INFINITY);
        let mut any_changed = false;
        for fi in 0..mesh.faces.len() as u32 {
            let fid = FaceId(fi);
            if !mesh.is_face_live(fid) {
                continue;
            }
            let vs = mesh.face_vertices(fid);
            let mut touched = false;
            for &vid in &vs {
                let p = mesh.vert(vid).position;
                mesh_mn = mesh_mn.min(p);
                mesh_mx = mesh_mx.max(p);
                touched |= changed[vid.0 as usize];
            }
            if touched {
                any_changed = true;
                for &vid in &vs {
                    let pn = mesh.vert(vid).position;
                    let po = c.prev_positions[vid.0 as usize];
                    d_mn = d_mn.min(pn).min(po);
                    d_mx = d_mx.max(pn).max(po);
                }
            }
        }
        if !mesh_mn.is_finite() || !mesh_mx.is_finite() {
            incr_skip("aabb");
            break 'incr;
        }

        // Lattice decision.
        let half = c.grid.bounds;
        let cell = c.grid.voxel_size();
        let (fit_center, fit_half) = remesh_lattice(mesh_mn, mesh_mx, res, padding);
        let exact = fit_center == c.center && fit_half == half;
        if !exact {
            // Session 2026-08-13: refusing the latch on mouse-up (`!episode_active`)
            // forced a full remesh every release (`skip=lattice`, 140–190 ms) even when
            // the mesh still fit the 1.25 lattice the drag was already using. Reuse the
            // latch whenever the 2-cell air margin holds — episode or not.
            let reach = (mesh_mx - c.center)
                .abs()
                .max((mesh_mn - c.center).abs())
                .max_element();
            if reach > half - 2.0 * cell {
                incr_skip(if episode_active { "grow" } else { "lattice" });
                break 'incr;
            }
        }

        // Grid-point box rewritten this frame. Declared out here because `region` is scoped to
        // the `any_changed` block: when NOTHING moved the grid is provably byte-identical, so the
        // extract can reuse every cached slab (a signature-only re-eval — e.g. a `project` change
        // — still re-extracts, but from unchanged data).
        let mut dirty = core_math::voxel_sculpt::DirtyCells::None;
        let mut dirty_world: Option<(Vec3, Vec3)> = None;
        if any_changed {
            // Dirty region cells: local coords l = p − center; index i = (l + half)/cell.
            let w = 2.0 * cell;
            let dil = w + cell;
            let lmn = d_mn - c.center;
            let lmx = d_mx - c.center;
            let lo = |l: f32| ((((l - dil) + half) / cell).floor() as i64).clamp(0, res as i64 - 1) as u32;
            let hi = |l: f32| ((((l + dil) + half) / cell).ceil() as i64).clamp(0, res as i64 - 1) as u32;
            let region = [lo(lmn.x), lo(lmn.y), lo(lmn.z), hi(lmx.x), hi(lmx.y), hi(lmx.z)];
            let vol = (region[3] - region[0] + 1) as u64
                * (region[4] - region[1] + 1) as u64
                * (region[5] - region[2] + 1) as u64;
            if (vol as f32) > (res as u64).pow(3) as f32 * REMESH_INCR_MAX_DIRTY_FRAC {
                incr_skip("dirty");
                break 'incr;
            }

            let world_lo = Vec3::new(
                c.center.x - half + region[0] as f32 * cell,
                c.center.y - half + region[1] as f32 * cell,
                c.center.z - half + region[2] as f32 * cell,
            );
            let world_hi = Vec3::new(
                c.center.x - half + region[3] as f32 * cell,
                c.center.y - half + region[4] as f32 * cell,
                c.center.z - half + region[5] as f32 * cell,
            );
            let band_pad = Vec3::splat(w);
            let clip_lo = world_lo - band_pad;
            let clip_hi = world_hi + band_pad;
            dirty_world = Some((clip_lo, clip_hi));

            // Seeded resample of the dirty region. Only source faces that overlap the
            // dilated region can change a cell in it — skip the rest of a dense cage.
            let Some((sv, indices)) = source_tris_overlapping(mesh, clip_lo, clip_hi) else {
                incr_skip("clip");
                break 'incr;
            };
            let local: Vec<Vec3> = sv.iter().map(|v| *v - c.center).collect();
            let grid_ref = &c.grid;
            let iso = grid_ref.iso;
            let Some(sdf) = io_bridge::mesh_to_sdf::region_mesh_to_sdf_band_seeded(
                &local,
                &indices,
                half,
                res,
                region,
                w,
                &|gx, gy, gz| grid_ref.density.get_idx(grid_ref.flat_index(gx, gy, gz)) < iso,
            ) else {
                incr_skip("sdf");
                break 'incr;
            };
            let air_before = region_air_count(&c.grid, region);
            c.grid.write_region_from_sdf_ex(region, &sdf, w, false);
            dirty = core_math::voxel_sculpt::DirtyCells::Points {
                lo: [region[0], region[1], region[2]],
                hi: [region[3], region[4], region[5]],
            };
            // ENCLOSURE RECONCILE: a new sealed void requires filling air.
            // Skip-seal-while-dragging defers the flood to mouse-up.
            let air_after = region_air_count(&c.grid, region);
            if episode_active && skip_seal_drag {
                c.seal_deferred = true;
            } else if air_after < air_before {
                incr_sealed(true);
                if c.grid.seal_enclosed_air() > 0 {
                    c.had_enclosed_void = true;
                    // The seal writes ARBITRARY flat indices and reports only a count, so nothing
                    // bounds where the field changed — the whole extract must be redone THIS frame.
                    // `had_enclosed_void` guards the NEXT frame, not this one.
                    dirty = core_math::voxel_sculpt::DirtyCells::All;
                }
                c.seal_deferred = false;
            }
        } else if !episode_active && c.seal_deferred {
            incr_sealed(true);
            if c.grid.seal_enclosed_air() > 0 {
                c.had_enclosed_void = true;
                dirty = core_math::voxel_sculpt::DirtyCells::All;
            }
            c.seal_deferred = false;
        }
        // (No position change: keep the grid — a signature-only change still re-extracts.)

        let prev_proj = std::mem::take(&mut c.prev_projected);
        let project_changed = c.project_bits != project.to_bits();
        let scope = if project_changed || prev_proj.is_empty() {
            ProjectScope::Full
        } else if !any_changed {
            ProjectScope::ReuseAll { prev: &prev_proj }
        } else if matches!(dirty, core_math::voxel_sculpt::DirtyCells::All) {
            ProjectScope::Full
        } else if let Some((mn, mx)) = dirty_world {
            ProjectScope::Region { prev: &prev_proj, mn, mx }
        } else {
            ProjectScope::Full
        };
        let mut projected = Vec::new();
        let extract_cache = if collapse {
            None
        } else {
            Some((&mut c.extract, dirty))
        };
        let proj_slabs = if collapse {
            None
        } else {
            Some(&mut c.proj_slabs)
        };
        let collapse_cache = if collapse {
            Some((&mut c.collapse_extract, dirty))
        } else {
            None
        };
        let Some(out) = extract_half_edge(
            &c.grid,
            c.center,
            extract_smooth,
            mesh,
            project,
            extract_cache,
            scope,
            Some(&mut projected),
            proj_slabs,
            episode_active,
            collapse,
            collapse_cache,
        ) else {
            incr_skip("extract");
            break 'incr;
        };
        c.prev_projected = projected;
        c.project_bits = project.to_bits();
        c.prev_positions.clear();
        c.prev_positions.extend(mesh.vertices.iter().map(|v| v.position));
        c.diverged = c.diverged || !exact;
        c.last_eval_incremental = true;
        let mut out = out;
        transfer_face_materials(mesh, &mut out);
        return Some(out);
    }

    // FULL path — refresh the cache. Mid-episode inflate so an AABB-growing grab can
    // latch on later frames. The hitch we measured was dropping this cache on commit
    // (next grab starts `nocache` and remeshes twice), not the inflate itself.
    // 1.5 on a mid-episode grow so the next pull is less likely to skip=grow again.
    // Cold / commit fills stay 1.0 unless this is an episode grow (cache already live).
    let headroom = if episode_active { 1.5 } else { 1.0 };
    let (out, grid, center, sealed_pockets, projected, collapse_extract) =
        voxel_remesh_full(mesh, res, extract_smooth, padding, headroom, project, episode_active, collapse)?;
    // `sealed_pocket` is in-band facing/flood disagreement. A pulled CUBE lights it
    // (lining noise) even though there is no cavity — that was `skip=void` after every
    // grow/lattice remesh (session 2026-08-13). Do not ACQUIRE the lock from an AABB
    // refit; keep it only if we already had it. Cold / topology / param fills still
    // lock when the sampler reports a real pocket (hollow fixture).
    let skip = last_incr_skip();
    let prev_void = cache.as_ref().map_or(false, |c| c.had_enclosed_void);
    // grow / lattice / topo: same body, lining noise is not a new cavity.
    // nocache / key (res, params): cold or param fill — lock if the sampler reports a pocket.
    let keep_prev = skip == "grow" || skip == "lattice" || skip == "topo";
    let had_enclosed_void = if sealed_pockets == 0 {
        false
    } else if keep_prev {
        prev_void
    } else {
        true
    };
    *cache = Some(RemeshIncrCache {
        nid,
        slot,
        res,
        smooth,
        padding_bits: padding.to_bits(),
        collapse,
        center,
        prev_positions: mesh.vertices.iter().map(|v| v.position).collect(),
        prev_counts: (mesh.half_edges.len(), mesh.faces.len()),
        grid,
        diverged: headroom > 1.0,
        had_enclosed_void,
        last_eval_incremental: false,
        extract: Default::default(),
        prev_projected: projected,
        project_bits: project.to_bits(),
        proj_slabs: Vec::new(),
        collapse_extract,
        seal_deferred: false,
    });
    let mut out = out;
    transfer_face_materials(mesh, &mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn remesh_cube_produces_surface() {
        let cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let out = voxel_remesh_mesh(&cube, 24, true, 0.1, 1.0).expect("remesh cube");
        assert!(out.face_count() > 0, "expected remeshed faces");
    }

    /// MATERIAL_PRESERVATION.md: a remesh must NEVER repaint the body with the default
    /// material (owner 2026-08-19 "quand je remesh, je perds mon matériau").
    #[test]
    fn remesh_keeps_single_material() {
        let mut cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for fi in 0..cube.faces.len() as u32 {
            let f = FaceId(fi);
            if cube.is_face_live(f) {
                cube.face_mut(f).material_id = 7;
            }
        }
        let out = voxel_remesh_mesh(&cube, 24, true, 0.1, 1.0).expect("remesh cube");
        for fi in 0..out.faces.len() as u32 {
            let f = FaceId(fi);
            if out.is_face_live(f) {
                assert_eq!(out.face(f).material_id, 7, "remesh dropped the material");
            }
        }
    }

    /// Multi-material: each output face takes the NEAREST input face's material — the
    /// top of a two-material cube must come back with the top's id, the bottom with the
    /// bottom's, and no face may fall back to the default.
    #[test]
    fn remesh_transfers_multi_material_by_region() {
        let mut cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        for fi in 0..cube.faces.len() as u32 {
            let f = FaceId(fi);
            if !cube.is_face_live(f) {
                continue;
            }
            let cz: Vec3 = cube
                .face_vertices(f)
                .iter()
                .map(|&v| cube.vert(v).position)
                .sum::<Vec3>()
                / cube.face_vertices(f).len() as f32;
            cube.face_mut(f).material_id = if cz.y > 0.25 { 5 } else { 3 };
        }
        let out = voxel_remesh_mesh(&cube, 24, true, 0.1, 1.0).expect("remesh cube");
        let (mut top5, mut bot3, mut wrong) = (0usize, 0usize, 0usize);
        for fi in 0..out.faces.len() as u32 {
            let f = FaceId(fi);
            if !out.is_face_live(f) {
                continue;
            }
            let vs = out.face_vertices(f);
            let c: Vec3 =
                vs.iter().map(|&v| out.vert(v).position).sum::<Vec3>() / vs.len() as f32;
            let m = out.face(f).material_id;
            // CLEAR interior of each region only — faces near cube edges are legitimately
            // nearest to a SIDE face's surface, so their material is geometry-dependent.
            let interior = c.x.abs() < 0.25 && c.z.abs() < 0.25;
            if c.y > 0.4 && interior {
                if m == 5 { top5 += 1 } else { wrong += 1 }
            } else if c.y < -0.4 && interior {
                if m == 3 { bot3 += 1 } else { wrong += 1 }
            }
            assert!(m == 5 || m == 3, "face fell back to default material ({m})");
        }
        assert!(top5 > 0 && bot3 > 0, "both regions must be sampled (top {top5} bot {bot3})");
        assert_eq!(wrong, 0, "clear-region faces must take the nearest material");
    }

    #[test]
    fn remesh_collapse_is_coarser_than_uniform() {
        let sphere = HalfEdgeMesh::new_uv_sphere(0.5, 24, 16);
        let uniform = voxel_remesh_mesh(&sphere, 48, false, 0.1, 0.0).expect("uniform");
        let collapsed =
            voxel_remesh_mesh_collapse(&sphere, 48, false, 0.1, 0.0, true).expect("collapse");
        assert!(
            collapsed.face_count() < uniform.face_count(),
            "collapse must drop faces ({} vs {})",
            collapsed.face_count(),
            uniform.face_count()
        );
        assert!(collapsed.validate().is_ok(), "collapsed remesh must validate");
    }

    #[test]
    fn remesh_new_modifier_defaults_draft_res_on() {
        let m = core_math::scene_graph::PolyModifier::new(
            core_math::scene_graph::PolyModifierType::Remesh,
        );
        assert!(m.remesh_drop_res_drag, "new Remesh defaults draft-res-on-drag");
        let old = core_math::scene_graph::PolyModifier::default();
        assert!(
            !old.remesh_drop_res_drag,
            "loaded/default modifiers stay off until the FL4PRMDR tail says otherwise"
        );
    }

    #[test]
    fn remesh_draft_res_then_confirm_is_skip_key() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 64, false, 0.1, 0.0, false, true, 3, 0, false, false, false, &mut cache,
        )
        .expect("idle authored");
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 32, false, 0.1, 0.0, false, true, 3, 0, true, false, false, &mut cache,
        )
        .expect("draft res");
        assert_eq!(last_incr_skip(), "key", "drop authored→draft must miss the incr key");
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 64, false, 0.1, 0.0, false, true, 3, 0, false, false, false, &mut cache,
        )
        .expect("confirm authored");
        assert_eq!(last_incr_skip(), "key", "confirm draft→authored must miss the incr key");
    }

    #[test]
    fn remesh_collapse_toggle_busts_incr_cache() {
        let mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 32, false, 0.1, 0.0, false, false, 3, 0, false, false, false, &mut cache,
        )
        .expect("uniform fill");
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 32, false, 0.1, 0.0, false, true, 3, 0, false, false, false, &mut cache,
        )
        .expect("collapse fill");
        assert_eq!(last_incr_skip(), "key", "collapse toggle must miss the incr key");
        assert!(
            cache.as_ref().unwrap().collapse,
            "cache must store the new collapse flag"
        );
    }

    #[test]
    fn remesh_collapse_incremental_reuses_parent_slabs() {
        let mut mesh = HalfEdgeMesh::new_uv_sphere(0.5, 24, 16);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 64, false, 0.1, 0.0, false, true, 3, 0, true, false, false, &mut cache,
        )
        .expect("collapse fill");
        let vi = (0..mesh.vertices.len())
            .max_by(|&a, &b| {
                mesh.vertices[a]
                    .position
                    .x
                    .total_cmp(&mesh.vertices[b].position.x)
            })
            .unwrap();
        mesh.vertices[vi].position.x += 0.02;
        let out = voxel_remesh_mesh_incremental_ex(
            &mesh, 64, false, 0.1, 0.0, false, true, 3, 0, true, false, false, &mut cache,
        )
        .expect("collapse incr");
        assert!(out.face_count() > 0);
        let c = cache.as_ref().unwrap();
        assert!(c.last_eval_incremental, "nudge must stay incremental");
        assert!(
            c.collapse_extract.primed,
            "collapse extract cache must be live"
        );
        assert!(
            c.collapse_extract.recomputed_parent_slabs < 32,
            "local nudge must not recompute every parent slab ({})",
            c.collapse_extract.recomputed_parent_slabs
        );
    }

    #[test]
    fn remesh_collapse_incremental_projects_dirty_slabs_only() {
        let mut mesh = HalfEdgeMesh::new_uv_sphere(0.5, 24, 16);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 128, false, 0.1, 1.0, false, true, 3, 0, true, false, false, &mut cache,
        )
        .expect("collapse fill");
        let vi = (0..mesh.vertices.len())
            .max_by(|&a, &b| {
                mesh.vertices[a]
                    .position
                    .x
                    .total_cmp(&mesh.vertices[b].position.x)
            })
            .unwrap();
        mesh.vertices[vi].position.x += 0.02;
        let out = voxel_remesh_mesh_incremental_ex(
            &mesh, 128, false, 0.1, 1.0, false, true, 3, 0, true, false, false, &mut cache,
        )
        .expect("collapse incr project");
        let c = cache.as_ref().unwrap();
        let dirty = c.collapse_extract.last_project_dirty_verts;
        let total = out.vertex_count();
        assert!(c.last_eval_incremental);
        assert!(
            dirty > 0 && dirty < total,
            "collapse S3 must snap a dirty subset ({dirty} / {total})"
        );
    }

    #[test]
    fn remesh_skip_seal_drag_defers_flood() {
        let mut mesh = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 32, false, 0.1, 0.0, false, false, 3, 0, true, false, true, &mut cache,
        )
        .expect("fill");
        let vi = 0usize;
        mesh.vertices[vi].position.x += 0.05;
        let _ = voxel_remesh_mesh_incremental_ex(
            &mesh, 32, false, 0.1, 0.0, false, false, 3, 0, true, false, true, &mut cache,
        )
        .expect("drag skip-seal");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "nudge must stay incremental"
        );
        assert!(
            !last_incr_sealed(),
            "skip-seal-while-dragging must not flood"
        );
        assert!(
            cache.as_ref().unwrap().seal_deferred,
            "commit must still owe a seal"
        );
    }

    #[test]
    fn remesh_empty_is_none() {
        let empty = HalfEdgeMesh::new();
        assert!(voxel_remesh_mesh(&empty, 16, false, 0.1, 1.0).is_none());
    }

    /// V1 GATE — an AABB-preserving vertex nudge through the incremental path must produce
    /// a mesh BIT-IDENTICAL to a fresh stateless eval (same lattice fit ⇒ the seeded
    /// dirty-region splice is provably the same field ⇒ same extract ⇒ same half-edge).
    #[test]
    fn incremental_interior_nudge_is_bit_identical() {
        let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let mut cache = None;
        let _a = voxel_remesh_mesh_incremental(&mesh, 32, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("first eval");
        assert!(
            !cache.as_ref().unwrap().last_eval_incremental,
            "first eval is the stateless cache fill"
        );

        // Nudge a strictly-interior vertex along a non-degenerate axis: AABB unchanged.
        let (mn, mx) = mesh.bounding_box();
        let interior = (0..mesh.vertices.len())
            .find(|&i| {
                let p = mesh.vertices[i].position;
                [(p.x, mn.x, mx.x), (p.y, mn.y, mx.y), (p.z, mn.z, mx.z)]
                    .iter()
                    .all(|&(v, lo, hi)| hi - lo < 1e-6 || (v > lo + 0.3 && v < hi - 0.3))
            })
            .expect("plane has an interior vertex");
        // Move along the first non-degenerate axis.
        let axis = if mx.x - mn.x > 1e-6 { 0 } else { 1 };
        match axis {
            0 => mesh.vertices[interior].position.x += 0.05,
            _ => mesh.vertices[interior].position.y += 0.05,
        }
        let (mn2, mx2) = mesh.bounding_box();
        assert_eq!((mn, mx), (mn2, mx2), "fixture must keep the AABB (else the test is void)");

        let inc = voxel_remesh_mesh_incremental(&mesh, 32, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("incremental eval");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "AABB-preserving nudge must take the incremental path"
        );
        assert!(!cache.as_ref().unwrap().diverged, "same lattice fit ⇒ not diverged");

        let reference = voxel_remesh_mesh(&mesh, 32, true, 0.1, 1.0).expect("stateless reference");
        assert_eq!(inc.vertex_count(), reference.vertex_count(), "vertex count");
        assert_eq!(inc.face_count(), reference.face_count(), "face count");
        for i in 0..inc.vertices.len() {
            assert_eq!(
                inc.vertices[i].position, reference.vertices[i].position,
                "vertex {i} position diverged"
            );
        }

        // Second incremental frame uses the regional S3 splice (prev_projected is live).
        // Must stay bit-identical to a stateless eval — otherwise the live surface would
        // drift from the committed remesh on release.
        match axis {
            0 => mesh.vertices[interior].position.x += 0.04,
            _ => mesh.vertices[interior].position.y += 0.04,
        }
        let inc2 = voxel_remesh_mesh_incremental(&mesh, 32, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("second incremental eval");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "second AABB-preserving nudge must stay incremental"
        );
        let reference2 = voxel_remesh_mesh(&mesh, 32, true, 0.1, 1.0).expect("stateless 2");
        assert_eq!(inc2.vertex_count(), reference2.vertex_count(), "vertex count after splice");
        assert_eq!(inc2.face_count(), reference2.face_count(), "face count after splice");
        for i in 0..inc2.vertices.len() {
            assert_eq!(
                inc2.vertices[i].position, reference2.vertices[i].position,
                "vertex {i} position diverged on regional project splice"
            );
        }
    }

    /// V1 REGRESSION GATE (review finding, 2026-08-08) — an edit that changes AIR
    /// CONNECTIVITY must still match a stateless eval. Sign in the far field is a GLOBAL
    /// property, so a purely local dirty-region splice cannot see a cavity being sealed:
    /// far cells outside the region would keep their stale air sign and Surface Nets would
    /// extract a phantom inner shell. Guarded by the post-splice whole-field
    /// `seal_enclosed_air` + the one-way `had_enclosed_void` lockout.
    ///
    /// Session 2026-08-13: pulling a CUBE vertex at remesh 256 set `had_enclosed_void`
    /// (`skip=void`) and every later frame paid a full remesh. A cube has no cavity —
    /// `band_finalize_topological` counts in-band facing/flood disagreements as
    /// `sealed_pocket`, which a deformed solid produces as lining noise.
    #[test]
    fn pulled_cube_does_not_latch_void_lockout() {
        let mut cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut cache = None;
        // 128 is enough to light lining noise; 256 is the session repro (too slow as a
        // default unit if we also pull a sphere). One 128 grow + commit is the gate.
        const RES: u32 = 128;
        let _ = voxel_remesh_mesh_incremental(&cube, RES, true, 0.1, 1.0, 51, 0, false, &mut cache)
            .expect("fill");
        assert!(
            !cache.as_ref().unwrap().had_enclosed_void,
            "pristine cube must not lock"
        );

        let vi = (0..cube.vertices.len())
            .max_by(|&a, &b| {
                cube.vertices[a]
                    .position
                    .x
                    .total_cmp(&cube.vertices[b].position.x)
            })
            .unwrap();
        cube.vertices[vi].position.x += 0.45;

        let _ = voxel_remesh_mesh_incremental(&cube, RES, true, 0.1, 1.0, 51, 0, true, &mut cache)
            .expect("grow eval");
        let sealed = {
            let (_, _, _, n, _, _) =
                voxel_remesh_full(&cube, RES, true, 0.1, 1.25, 1.0, true, false).expect("count");
            n
        };
        eprintln!(
            "[VOIDLOCK] pulled cube @{RES} sealed_pocket={sealed} skip={} had_void={} incr={}",
            last_incr_skip(),
            cache.as_ref().unwrap().had_enclosed_void,
            cache.as_ref().unwrap().last_eval_incremental
        );
        assert!(
            !cache.as_ref().unwrap().had_enclosed_void,
            "pulled cube sealed_pocket={sealed} must not latch had_enclosed_void"
        );

        // Episode commit (AABB changed, no episode) — must stay incremental if it fits.
        let _ = voxel_remesh_mesh_incremental(&cube, RES, true, 0.1, 1.0, 51, 0, false, &mut cache)
            .expect("commit refit");
        assert!(
            !cache.as_ref().unwrap().had_enclosed_void,
            "lattice refit after a cube pull must not latch void (skip={})",
            last_incr_skip()
        );
        assert!(
            cache.as_ref().unwrap().last_eval_incremental
                || last_incr_skip() == "grow"
                || last_incr_skip() == "lattice",
            "commit after pull is incremental or a true outgrow, not a void lock (skip={})",
            last_incr_skip()
        );

        cube.vertices[vi].position.x += 0.02;
        let _ = voxel_remesh_mesh_incremental(&cube, RES, true, 0.1, 1.0, 51, 0, true, &mut cache)
            .expect("post-grow");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "post-commit cube pull must be incremental (skip={} void={})",
            last_incr_skip(),
            cache.as_ref().unwrap().had_enclosed_void
        );

        // Session repro resolution. One grow + commit; ~150 ms each.
        let mut cube256 = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let mut cache256 = None;
        let _ = voxel_remesh_mesh_incremental(
            &cube256, 256, true, 0.1, 1.0, 52, 0, false, &mut cache256,
        )
        .expect("fill 256");
        cube256.vertices[vi].position.x += 0.45;
        let _ = voxel_remesh_mesh_incremental(
            &cube256, 256, true, 0.1, 1.0, 52, 0, true, &mut cache256,
        )
        .expect("grow 256");
        let sealed256 = {
            let (_, _, _, n, _, _) =
                voxel_remesh_full(&cube256, 256, true, 0.1, 1.25, 1.0, true, false).expect("count 256");
            n
        };
        eprintln!(
            "[VOIDLOCK] pulled cube @256 sealed_pocket={sealed256} skip={} had_void={}",
            last_incr_skip(),
            cache256.as_ref().unwrap().had_enclosed_void
        );
        assert!(
            !cache256.as_ref().unwrap().had_enclosed_void,
            "256 pulled cube sealed_pocket={sealed256} must not latch void"
        );
        let _ = voxel_remesh_mesh_incremental(
            &cube256, 256, true, 0.1, 1.0, 52, 0, false, &mut cache256,
        )
        .expect("commit 256");
        assert!(
            !cache256.as_ref().unwrap().had_enclosed_void,
            "256 lattice commit must not latch void"
        );

        // Extrude / subdivide is skip=topo — must not acquire the lock either.
        let sphere = HalfEdgeMesh::new_uv_sphere(0.5, 8, 6);
        let _ = voxel_remesh_mesh_incremental(
            &sphere, RES, true, 0.1, 1.0, 51, 0, false, &mut cache,
        )
        .expect("topo");
        assert_eq!(last_incr_skip(), "topo");
        assert!(
            !cache.as_ref().unwrap().had_enclosed_void,
            "topology change on a solid must not latch void"
        );
    }

    /// Fixture: hollow cube (outer + inner shell) — first eval must lock, later evals stay full.
    #[test]
    fn incremental_refuses_fields_with_sealed_voids() {
        let outer = HalfEdgeMesh::new_box(Vec3::splat(0.9));
        let inner = HalfEdgeMesh::new_box(Vec3::splat(0.4));
        // Two disjoint shells: the gap between them is air the border flood cannot reach
        // (enclosure only reads reachability, not winding) — a sealed pocket by construction.
        let hollow = crate::poly::boolean::merge_meshes(&outer, &inner);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental(&hollow, 32, true, 0.1, 1.0, 9, 0, false, &mut cache)
            .expect("first eval");
        let hollow_sealed = {
            let (_, _, _, n, _, _) =
                voxel_remesh_full(&hollow, 32, true, 0.1, 1.0, 1.0, false, false).expect("hollow count");
            n
        };
        eprintln!("[VOIDLOCK] hollow @32 sealed_pocket={hollow_sealed}");
        assert!(
            cache.as_ref().unwrap().had_enclosed_void,
            "hollow fixture must report a sealed void (else this gate proves nothing) sealed={hollow_sealed}"
        );

        // Any subsequent edit — even an AABB-preserving interior nudge, which is exactly
        // the case the `exact` lattice check would wave through — must go stateless.
        let mut edited = hollow.clone();
        let (mn, mx) = edited.bounding_box();
        let vi = (0..edited.vertices.len())
            .find(|&i| {
                let p = edited.vertices[i].position;
                p.x > mn.x + 0.1 && p.x < mx.x - 0.1
            })
            .unwrap_or(0);
        edited.vertices[vi].position.y += 0.01;
        let out = voxel_remesh_mesh_incremental(&edited, 32, true, 0.1, 1.0, 9, 0, false, &mut cache)
            .expect("second eval");
        assert!(
            !cache.as_ref().unwrap().last_eval_incremental,
            "a field with a filled void must NEVER take the incremental path (void filling \
             is one-way — a re-opened void cannot be restored by a local splice)"
        );
        let reference = voxel_remesh_mesh(&edited, 32, true, 0.1, 1.0).expect("reference");
        assert_eq!(out.vertex_count(), reference.vertex_count());
        assert_eq!(out.face_count(), reference.face_count());
    }

    /// GATE 0.5 REGRESSION GATE (`REMESHSOLIDNOINCR`, fixed 2026-08-10) — a CLOSED SOLID must be
    /// able to take the incremental path.
    ///
    /// It could not, from the day incremental remesh shipped until this fix: `band_finalize`'s
    /// single `enclosed` counter included every far cell the border flood missed, which for a solid
    /// is its entire interior (solid cube @128 → 512000 cells). `voxel_remesh_full` read that as
    /// "a void was filled" and latched the one-way `had_enclosed_void` lockout on the FIRST eval,
    /// so every later drag frame took the full stateless path. Measured cost at res 256: 191 ms per
    /// frame on a solid vs 26 ms on an open crust.
    ///
    /// The bug hid because every other incremental fixture in this file is `new_plane` — an open
    /// crust, the one shape class with no far interior and therefore the one class where the flag
    /// stayed false. This test is deliberately a SOLID, and pairs with
    /// `incremental_refuses_fields_with_sealed_voids`: together they pin that the lockout still
    /// fires for a real sealed void and no longer fires for an ordinary solid.
    #[test]
    fn incremental_engages_on_a_closed_solid() {
        let mut mesh = HalfEdgeMesh::new_uv_sphere(0.5, 24, 12);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental(&mesh, 32, true, 0.1, 1.0, 31, 0, false, &mut cache)
            .expect("first eval");
        assert!(
            !cache.as_ref().unwrap().had_enclosed_void,
            "a closed solid has NO sealed pocket — its interior is far-solid, not a filled void. \
             If this fires, band_finalize is counting far_solid into sealed_pocket again."
        );

        // AABB-preserving interior nudge, the case the `exact` lattice check waves through.
        let (mn, mx) = mesh.bounding_box();
        let vi = (0..mesh.vertices.len())
            .find(|&i| {
                let p = mesh.vertices[i].position;
                p.x > mn.x + 0.05 && p.x < mx.x - 0.05
                    && p.y > mn.y + 0.05 && p.y < mx.y - 0.05
                    && p.z > mn.z + 0.05 && p.z < mx.z - 0.05
            })
            .expect("sphere has a strictly-interior-of-AABB vertex");
        let before = mesh.bounding_box();
        mesh.vertices[vi].position *= 0.98; // pull toward centre — cannot grow the AABB
        assert_eq!(before, mesh.bounding_box(), "fixture must keep the AABB (else the test is void)");

        let out = voxel_remesh_mesh_incremental(&mesh, 32, true, 0.1, 1.0, 31, 0, false, &mut cache)
            .expect("second eval");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "REMESHSOLIDNOINCR: a solid must be able to take the incremental path"
        );
        assert!(out.face_count() > 0 && out.validate().is_ok());

        // Same lattice fit ⇒ the splice must equal a stateless eval exactly.
        let reference = voxel_remesh_mesh(&mesh, 32, true, 0.1, 1.0).expect("stateless reference");
        assert_eq!(out.vertex_count(), reference.vertex_count(), "vertex count");
        assert_eq!(out.face_count(), reference.face_count(), "face count");
        for i in 0..out.vertices.len() {
            assert_eq!(
                out.vertices[i].position, reference.vertices[i].position,
                "vertex {i} diverged from the stateless reference"
            );
        }
    }

    /// B — drag (`episode_active`) skips TwinLinker; release (`episode_active == false`)
    /// rebuilds a manifold on the same incremental grid. Positions must match so the
    /// surface does not pop on mouse-up.
    #[test]
    fn remesh_episode_draft_then_commit_rebuilds_twins() {
        use core_math::half_edge::NO_TWIN;
        let mut mesh = HalfEdgeMesh::new_uv_sphere(0.5, 16, 10);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental(&mesh, 24, true, 0.1, 1.0, 41, 0, false, &mut cache)
            .expect("fill");

        let (mn, mx) = mesh.bounding_box();
        let vi = (0..mesh.vertices.len())
            .find(|&i| {
                let p = mesh.vertices[i].position;
                p.x > mn.x + 0.05
                    && p.x < mx.x - 0.05
                    && p.y > mn.y + 0.05
                    && p.y < mx.y - 0.05
                    && p.z > mn.z + 0.05
                    && p.z < mx.z - 0.05
            })
            .expect("sphere has an interior-of-AABB vertex");
        let before = mesh.bounding_box();
        mesh.vertices[vi].position *= 0.98;
        assert_eq!(before, mesh.bounding_box(), "fixture must keep the AABB");

        let drag = voxel_remesh_mesh_incremental(&mesh, 24, true, 0.1, 1.0, 41, 0, true, &mut cache)
            .expect("episode drag");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "AABB-preserving drag must stay incremental"
        );
        let drag_twins = drag
            .half_edges
            .iter()
            .filter(|e| e.twin != NO_TWIN)
            .count();
        assert_eq!(drag_twins, 0, "B: drag extract is from_mesh_draft — no twins");

        let commit =
            voxel_remesh_mesh_incremental(&mesh, 24, true, 0.1, 1.0, 41, 0, false, &mut cache)
                .expect("episode commit");
        let commit_twins = commit
            .half_edges
            .iter()
            .filter(|e| e.twin != NO_TWIN)
            .count();
        assert!(commit_twins > 0, "B: commit rebuilds twins via from_mesh");
        assert!(commit.validate().is_ok(), "commit remesh must validate");
        assert_eq!(drag.vertex_count(), commit.vertex_count(), "vertex count");
        assert_eq!(drag.face_count(), commit.face_count(), "face count");
        for i in 0..drag.vertices.len() {
            assert_eq!(
                drag.vertices[i].position, commit.vertices[i].position,
                "vertex {i} popped between draft and commit"
            );
        }
    }

    /// V1 GATE — topology change (different mesh entirely) must fall back to the stateless
    /// path and refresh the cache, never splice into a stale grid.
    #[test]
    fn incremental_topology_change_falls_back() {
        let plane = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental(&plane, 24, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("fill");
        let cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let out = voxel_remesh_mesh_incremental(&cube, 24, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("topology change eval");
        assert!(
            !cache.as_ref().unwrap().last_eval_incremental,
            "topology change must take the stateless path"
        );
        let reference = voxel_remesh_mesh(&cube, 24, true, 0.1, 1.0).expect("reference");
        assert_eq!(out.vertex_count(), reference.vertex_count());
        assert_eq!(out.face_count(), reference.face_count());
    }

    /// AABB-changing move reuses the latched lattice whenever the mesh still fits
    /// (episode or not). Full remesh only if it outgrows the 2-cell air margin.
    #[test]
    fn incremental_latch_is_episode_scoped() {
        // Subdivided plane: faces are LOCAL, so a corner drag dirties a small region. (A
        // raw cube is the counter-fixture: its faces span the whole body, the dirty box
        // covers >50% of the lattice, and the volume bail correctly forces a full eval.)
        let plane = HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4);
        let mut cache = None;
        let _ = voxel_remesh_mesh_incremental(&plane, 24, true, 0.1, 1.0, 7, 0, true, &mut cache)
            .expect("fill");

        // Pull a hull vertex outward along a non-degenerate axis — AABB grows, still
        // inside the (episode-inflated) latch margin.
        let (mn, mx) = plane.bounding_box();
        let axis = if mx.x - mn.x > 1e-6 { 0 } else { 1 };
        let hull_max = if axis == 0 { mx.x } else { mx.y };
        let mut dragged = plane.clone();
        let ci = (0..dragged.vertices.len())
            .find(|&i| {
                let p = dragged.vertices[i].position;
                ((if axis == 0 { p.x } else { p.y }) - hull_max).abs() < 1e-6
            })
            .expect("hull vertex");
        match axis {
            0 => dragged.vertices[ci].position.x += 0.04,
            _ => dragged.vertices[ci].position.y += 0.04,
        }

        // During an episode: latched incremental, diverged.
        let out = voxel_remesh_mesh_incremental(&dragged, 24, true, 0.1, 1.0, 7, 0, true, &mut cache)
            .expect("latched eval");
        assert!(cache.as_ref().unwrap().last_eval_incremental, "latched path");
        assert!(cache.as_ref().unwrap().diverged, "AABB changed under latch ⇒ diverged");
        assert!(out.face_count() > 0 && out.validate().is_ok());

        // Mouse-up / no episode: still incremental if the latch has room (the 140 ms
        // skip=lattice hitch). Only a pull that breaks the air margin full-remeshes.
        let mut dragged2 = dragged.clone();
        dragged2.vertices[ci].position += Vec3::splat(0.02);
        let _ = voxel_remesh_mesh_incremental(&dragged2, 24, true, 0.1, 1.0, 7, 0, false, &mut cache)
            .expect("release eval");
        assert!(
            cache.as_ref().unwrap().last_eval_incremental,
            "release must keep the latch when the mesh still fits (skip={})",
            last_incr_skip()
        );
        assert!(
            cache.as_ref().unwrap().diverged,
            "still on the inflated lattice until a true grow"
        );
    }

    /// GATE 0 PROBE (REMESH_GPU_PLAN.md) — can a SOLID mesh ever take the incremental path?
    ///
    /// `band_finalize` (io_bridge) counts `enclosed` as every FAR cell the border flood did not
    /// reach — which for any closed solid is its whole interior, not a sealed void. A solid cube at
    /// res 128 reports 512000. `voxel_remesh_full` then sets `had_enclosed_void: enclosed > 0`, and
    /// the incremental path refuses any cache with that flag. If this probe reports "never", V1
    /// incremental remesh is inert for every solid model and only engages on thin crusts — which is
    /// exactly what every incremental test fixture is (`new_plane`).
    ///
    /// Run: cargo test -p app --release gate0_incremental_on_solid -- --ignored --nocapture
    #[test]
    #[ignore]
    fn gate0_incremental_on_solid() {
        for (name, mut mesh) in [
            ("solid cube", HalfEdgeMesh::new_box(Vec3::splat(0.5))),
            ("open plane", HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16)),
        ] {
            let mut cache = None;
            let _ = voxel_remesh_mesh_incremental(&mesh, 64, true, 0.1, 1.0, 11, 0, true, &mut cache)
                .expect("fill");
            let void = cache.as_ref().unwrap().had_enclosed_void;

            // AABB-preserving interior nudge — the case `exact` would wave straight through.
            let (mn, mx) = mesh.bounding_box();
            let vi = (0..mesh.vertices.len())
                .find(|&i| {
                    let p = mesh.vertices[i].position;
                    p.x > mn.x + 1e-4 && p.x < mx.x - 1e-4
                })
                .unwrap_or(0);
            mesh.vertices[vi].position.y += 1e-4;
            let _ = voxel_remesh_mesh_incremental(&mesh, 64, true, 0.1, 1.0, 11, 0, true, &mut cache)
                .expect("second eval");
            let took = cache.as_ref().unwrap().last_eval_incremental;
            eprintln!(
                "[GATE0] {name:<12} had_enclosed_void={void:<5} incremental_path={}",
                if took { "YES" } else { "NEVER" }
            );
        }
    }

    /// GATE 0 BASELINE (REMESH_GPU_PLAN.md) — the drag frame on a SOLID, which is what a user
    /// actually remeshes. Every incremental fixture in this file is `new_plane`, an open crust, and
    /// per `gate0_incremental_on_solid` that is the only shape class where the incremental path can
    /// engage at all. So the octree plan's headline "24.9 ms drag frame @256" describes a sheet, not
    /// a model. This reports both so the gap is a number rather than an argument.
    ///
    /// Run: cargo test -p app --release gate0_solid_drag_baseline -- --ignored --nocapture
    #[test]
    #[ignore]
    fn gate0_solid_drag_baseline() {
        for res in [128u32, 256] {
            for (name, mut mesh) in [
                ("plane (crust)", HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16)),
                ("uv sphere (solid)", HalfEdgeMesh::new_uv_sphere(0.5, 48, 24)),
            ] {
                let mut cache = None;
                let t0 = std::time::Instant::now();
                let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 21, 0, true, &mut cache)
                    .expect("fill");
                let t_full = t0.elapsed().as_secs_f64() * 1e3;

                // Drag one hull vertex a fraction of a cell per frame.
                let (mn, mx) = mesh.bounding_box();
                let vi = (0..mesh.vertices.len())
                    .max_by(|&a, &b| {
                        mesh.vertices[a].position.x.total_cmp(&mesh.vertices[b].position.x)
                    })
                    .unwrap();
                let step = (mx.x - mn.x) * 0.002;
                const N: u32 = 6;
                let mut incr = 0u32;
                let t1 = std::time::Instant::now();
                for _ in 0..N {
                    mesh.vertices[vi].position.x += step;
                    let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 21, 0, true, &mut cache)
                        .expect("drag frame");
                    if cache.as_ref().unwrap().last_eval_incremental {
                        incr += 1;
                    }
                }
                let per = t1.elapsed().as_secs_f64() * 1e3 / N as f64;
                eprintln!(
                    "[GATE0] res={res:<4} {name:<18} first={t_full:>7.1}ms  drag frame={per:>7.1}ms  \
                     incremental {incr}/{N}"
                );
            }
        }
    }

    /// MEASUREMENT — how much of a drag frame is topology change vs pure vertex motion?
    ///
    /// Decides two open design questions with one number:
    ///  * **Tier 2.5** ("skip extract entirely when topology is stable, just reproject
    ///    vertices"): only viable if a meaningful share of drag frames flip ZERO cells
    ///    across the iso threshold. The check is free in production — `write_region_from_sdf`
    ///    already returns the per-cell pre-values.
    ///  * **V2** (incremental extract): payoff scales with how FEW 16³ chunks contain a
    ///    flipped cell. A handful of dirty chunks out of thousands ⇒ large win.
    ///
    /// Sweeps drag speeds in units of one voxel per frame, since that is what governs how
    /// far the iso surface sweeps between frames.
    /// Run: cargo test -p app --release measure_drag_topology_churn -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_drag_topology_churn() {
        const CHUNK: u32 = 16;
        for res in [128u32, 256] {
            for speed_cells in [0.1f32, 0.25, 1.0, 3.0] {
                let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16);
                let mut cache = None;
                let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache)
                    .expect("fill");
                let cell = cache.as_ref().unwrap().grid.voxel_size();
                let step = cell * speed_cells;
                let (mn, mx) = mesh.bounding_box();
                let axis_x = mx.x - mn.x > 1e-6;
                let hull = if axis_x { mx.x } else { mx.y };
                let vi = (0..mesh.vertices.len())
                    .find(|&i| {
                        let p = mesh.vertices[i].position;
                        ((if axis_x { p.x } else { p.y }) - hull).abs() < 1e-6
                    })
                    .unwrap();

                let solid_mask = |c: &RemeshIncrCache| -> Vec<bool> {
                    let n = (c.res as usize).pow(3);
                    let iso = c.grid.iso;
                    (0..n).map(|i| c.grid.density.get_idx(i) >= iso).collect()
                };
                let mut prev = solid_mask(cache.as_ref().unwrap());
                let cdim = (res as usize + CHUNK as usize - 1) / CHUNK as usize;
                let (mut stable_frames, mut tot_flips, mut tot_chunks) = (0u32, 0u64, 0u64);
                const N: u32 = 12;
                for _ in 0..N {
                    if axis_x {
                        mesh.vertices[vi].position.x += step;
                    } else {
                        mesh.vertices[vi].position.y += step;
                    }
                    let _ = voxel_remesh_mesh_incremental(
                        &mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache,
                    )
                    .expect("drag frame");
                    let cur = solid_mask(cache.as_ref().unwrap());
                    let mut flips = 0u64;
                    let mut dirty_chunks = vec![false; cdim * cdim * cdim];
                    let r = res as usize;
                    for i in 0..cur.len() {
                        if cur[i] != prev[i] {
                            flips += 1;
                            let (x, y, z) = (i % r, (i / r) % r, i / (r * r));
                            let ci = (x / CHUNK as usize)
                                + (y / CHUNK as usize) * cdim
                                + (z / CHUNK as usize) * cdim * cdim;
                            dirty_chunks[ci] = true;
                        }
                    }
                    if flips == 0 {
                        stable_frames += 1;
                    }
                    tot_flips += flips;
                    tot_chunks += dirty_chunks.iter().filter(|&&d| d).count() as u64;
                    prev = cur;
                }
                let total_chunks = cdim * cdim * cdim;
                eprintln!(
                    "[CHURN] res={res:3} speed={speed_cells:>4}cell/frame: \
                     topology-stable {stable_frames}/{N} frames | avg flipped cells {:>8.0} \
                     ({:>5.2}% of grid) | avg dirty chunks {:>6.1}/{total_chunks} ({:>5.2}%)",
                    tot_flips as f64 / N as f64,
                    100.0 * (tot_flips as f64 / N as f64) / (res as f64).powi(3),
                    tot_chunks as f64 / N as f64,
                    100.0 * (tot_chunks as f64 / N as f64) / total_chunks as f64,
                );
            }
        }
    }

    /// PERF PROBE — slow drag (0.1 cell/frame) where extract topology is often stable, so
    /// the regional S3 splice can fire. The default drag benches step ~0.75 cell/frame and
    /// flip cells every frame, which forces a full project (see `ProjectScope::Region`).
    /// Run: cargo test -p app --release bench_remesh_slow_drag -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_remesh_slow_drag() {
        for res in [128u32, 256] {
            for (name, mut mesh) in [
                ("crust", HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16)),
                ("solid", HalfEdgeMesh::new_uv_sphere(0.5, 48, 24)),
                ("dense", HalfEdgeMesh::new_uv_sphere(0.5, 96, 48)),
            ] {
                let mut cache = None;
                let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache)
                    .expect("fill");
                let cell = cache.as_ref().unwrap().grid.voxel_size();
                let step = cell * 0.1;
                let vi = (0..mesh.vertices.len())
                    .max_by(|&a, &b| {
                        mesh.vertices[a].position.x.total_cmp(&mesh.vertices[b].position.x)
                    })
                    .unwrap();
                // Warm the incremental cache / prev_projected.
                for _ in 0..4 {
                    mesh.vertices[vi].position.x += step;
                    let _ = voxel_remesh_mesh_incremental(
                        &mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache,
                    )
                    .expect("warm");
                }
                const N: u32 = 8;
                let t1 = std::time::Instant::now();
                let mut incr = 0u32;
                for _ in 0..N {
                    mesh.vertices[vi].position.x += step;
                    let _ = voxel_remesh_mesh_incremental(
                        &mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache,
                    )
                    .expect("slow drag");
                    if cache.as_ref().unwrap().last_eval_incremental {
                        incr += 1;
                    }
                }
                let per = t1.elapsed().as_secs_f64() * 1e3 / N as f64;
                eprintln!(
                    "[REMESH SLOW] res={res:<4} {name:<6} drag frame={per:>7.1}ms  incremental {incr}/{N}"
                );
            }
        }
    }

    /// PERF PROBE — incremental drag frame vs stateless full eval, res 128/256 on a cube
    /// corner drag (the latched-episode path, the real drag-under-Remesh scenario).
    /// Run: cargo test -p app --release bench_remesh_incremental_drag -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_remesh_incremental_drag() {
        for res in [128u32, 256] {
            // Subdivided plane (16×16): a corner drag dirties a LOCAL region — the shape
            // incremental remesh exists for. (A raw cube's whole-body faces dirty ~half
            // the lattice and mostly measure the volume-bail path instead.)
            let mut mesh = HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16);
            let mut cache = None;
            let t0 = std::time::Instant::now();
            let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache)
                .expect("fill");
            let t_full = t0.elapsed();
            let (mn, mx) = mesh.bounding_box();
            let axis_x = mx.x - mn.x > 1e-6;
            let hull_max = if axis_x { mx.x } else { mx.y };
            let ci = (0..mesh.vertices.len())
                .find(|&i| {
                    let p = mesh.vertices[i].position;
                    ((if axis_x { p.x } else { p.y }) - hull_max).abs() < 1e-6
                })
                .unwrap();
            const N: u32 = 8;
            let t1 = std::time::Instant::now();
            for _ in 0..N {
                if axis_x {
                    mesh.vertices[ci].position.x += 0.003;
                } else {
                    mesh.vertices[ci].position.y += 0.003;
                }
                let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache)
                    .expect("drag frame");
                assert!(cache.as_ref().unwrap().last_eval_incremental, "must stay incremental");
            }
            let per = t1.elapsed().as_secs_f64() * 1e3 / N as f64;
            eprintln!(
                "[REMESH INCR] res={res:3}: full={:>7.1}ms  incremental drag frame={per:>7.1}ms",
                t_full.as_secs_f64() * 1e3
            );
        }
    }

    /// GATE 0c — per-STAGE breakdown of a real mid-drag incremental frame.
    ///
    /// `bench_remesh_incremental_drag` gives the frame TOTAL; the stage harness
    /// (`core_math/tests/bench_remesh_stages.rs`) gives per-stage costs but on an analytic
    /// SPHERE. Neither answers the question Gate 1 is scoped on: of an 83 ms drag frame at
    /// res 256, how much is S1 seal (whole-field, does NOT shrink with the dirty region) vs
    /// S2 extract + S3 reproject + S4 rebuild (all regionalisable)?
    ///
    /// Method: drive the real incremental path to a mid-drag state, then time the exact calls
    /// that path makes (`remesh.rs` 403 / 93 / 116 / 128) on the exact grid it makes them on.
    /// The seal is timed TWICE — a drag frame always re-seals an already-sealed field, so the
    /// repeat call is the representative steady-state cost.
    ///
    /// Reported as MEDIAN OF 5 — the S1-vs-S2+S4 comparison this gate turns on is a ~16% gap, and
    /// `core_math/tests/bench_poly_smooth.rs:51` already records that "one-shot timings on Windows
    /// are noisy enough to invert an ordering". Same helper shape, kept local (that one is a
    /// different crate's test binary).
    ///
    /// Run: cargo test -p app --release measure_drag_frame_stages -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_drag_frame_stages() {
        fn ms(f: impl FnOnce()) -> f64 {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        }
        fn ms5(mut f: impl FnMut()) -> f64 {
            let mut v: Vec<f64> = (0..5).map(|_| ms(&mut f)).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[2]
        }
        if cfg!(debug_assertions) {
            eprintln!("\n!! DEBUG BUILD - numbers are meaningless. Re-run with --release.\n");
        }
        for res in [128u32, 256] {
            // BOTH fixture classes. Until 2026-08-10 this bench ran the crust only, and every
            // stage figure in REMESH_ADAPTIVE_OCTREE_PLAN.md §12 is therefore a crust figure —
            // which is how REMESHSOLIDNOINCR (incremental never engaged on a solid) went unseen
            // for the life of the feature. The crust path below is deliberately UNCHANGED
            // (same vertex pick, same 0.003 step) so its numbers stay comparable to that log.
            for (fixture, mut mesh) in [
                ("crust", HalfEdgeMesh::new_plane(2.0, 2.0, 16, 16)),
                ("solid", HalfEdgeMesh::new_uv_sphere(0.5, 48, 24)),
                // Dense cage: same solid, more source tris. Times the clip-source-tris
                // lever that the 2300-tri "solid" fixture cannot show.
                ("dense", HalfEdgeMesh::new_uv_sphere(0.5, 96, 48)),
            ] {
            let mut cache = None;
            let _ = voxel_remesh_mesh_incremental(&mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache)
                .expect("fill");
            let (mn, mx) = mesh.bounding_box();
            let axis_x = mx.x - mn.x > 1e-6;
            let hull = if axis_x { mx.x } else { mx.y };
            let vi = (0..mesh.vertices.len())
                .find(|&i| {
                    let p = mesh.vertices[i].position;
                    ((if axis_x { p.x } else { p.y }) - hull).abs() < 1e-6
                })
                // A tessellated solid need not have a vertex EXACTLY on its bounding plane;
                // fall back to the extreme one. The plane fixture always hits the `find`, so
                // its behaviour — and its logged numbers — are untouched.
                .or_else(|| {
                    (0..mesh.vertices.len()).max_by(|&a, &b| {
                        let (pa, pb) = (mesh.vertices[a].position, mesh.vertices[b].position);
                        let (ca, cb) = if axis_x { (pa.x, pb.x) } else { (pa.y, pb.y) };
                        ca.total_cmp(&cb)
                    })
                })
                .unwrap();
            // Drive to a representative mid-drag state through the real path.
            for _ in 0..4 {
                if axis_x { mesh.vertices[vi].position.x += 0.003; }
                else { mesh.vertices[vi].position.y += 0.003; }
                let _ = voxel_remesh_mesh_incremental(
                    &mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache,
                ).expect("drag frame");
            }
            // Whole incremental frame: median over 5 successive real drag frames.
            let frame_ms = ms5(|| {
                if axis_x { mesh.vertices[vi].position.x += 0.003; }
                else { mesh.vertices[vi].position.y += 0.003; }
                let _ = voxel_remesh_mesh_incremental(
                    &mesh, res, true, 0.1, 1.0, 7, 0, true, &mut cache,
                ).expect("drag frame");
            });

            let c = cache.as_mut().unwrap();
            // How many cz slabs the REGIONAL extract actually recomputed on that frame. This is
            // the anti-vacuity signal: the `S2 extract` column below deliberately calls the
            // UNCACHED path so the stage stays comparable across commits, so without this number
            // there is nothing showing the cache is live on the production path.
            let slabs_recomputed = c.extract.recomputed_slabs;
            let slabs_total = (res - 1) as usize;
            // S1 — whole-field enclosure reconcile (remesh.rs:403). First call separately, then
            // the median of 5 repeats: a drag frame always re-seals an ALREADY-sealed field, so
            // the repeat is the representative steady-state cost.
            let mut filled_1 = 0usize;
            let seal_first = ms(|| filled_1 = c.grid.seal_enclosed_air());
            let mut filled_2 = 0usize;
            let seal_repeat = ms5(|| filled_2 = c.grid.seal_enclosed_air());
            // S2 — dual-contour extract (remesh.rs:93).
            let mut extracted = core_math::mesh::Mesh {
                vertices: Vec::new(),
                normals: Vec::new(),
                quads: Vec::new(),
            };
            let s2 = ms5(|| {
                extracted = c
                    .grid
                    .surface_nets_placed(true, core_math::voxel_sculpt::CellPlacement::Qef);
            });
            // S3 — surface reprojection (remesh.rs:116).
            let (raster, indices) = mesh.to_raster_data();
            let sv: Vec<Vec3> = raster.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
            let pts: Vec<Vec3> = extracted.vertices.iter().map(|v| *v + c.center).collect();
            let max_dist = c.grid.voxel_size() * PROJECT_MAX_CELLS;
            let mut snapped = None;
            let s3 = ms5(|| {
                snapped = io_bridge::mesh_to_sdf::project_points_to_mesh(
                    &pts, &sv, &indices, max_dist,
                );
            });
            // S4 — half-edge rebuild (remesh.rs:128). Also time the B draft path.
            let mut he = HalfEdgeMesh::new();
            let s4 = ms5(|| he = HalfEdgeMesh::from_mesh(&extracted));
            let mut he_draft = HalfEdgeMesh::new();
            let s4_draft = ms5(|| he_draft = HalfEdgeMesh::from_mesh_draft(&extracted));

            let incremental = cache.as_ref().unwrap().last_eval_incremental;
            eprintln!(
                "[DRAG STAGES med5] res={res:3} {fixture:<6} incr={incremental:<5} \
                 frame={frame_ms:>6.1}ms | S1 seal={seal_first:>6.1}ms \
                 (repeat {seal_repeat:>6.1}ms, filled {filled_1}/{filled_2})  S2 extract={s2:>6.1}ms  \
                 S3 reproject={s3:>6.1}ms  S4 from_mesh={s4:>6.1}ms  S4 draft={s4_draft:>6.1}ms  \
                 | S2+S4={:>6.1}ms  \
                 | REGIONAL {slabs_recomputed}/{slabs_total} slabs  \
                 verts={} quads={} he_faces={} snapped={}",
                s2 + s4,
                extracted.vertices.len(),
                extracted.quads.len(),
                he.face_count(),
                snapped.is_some(),
            );
            }
        }
        for res in [128u32, 256] {
            for skip_relax in [false, true] {
            let mut mesh = HalfEdgeMesh::new_uv_sphere(0.5, 48, 24);
            let mut cache = None;
            let _ = voxel_remesh_mesh_incremental_ex(
                &mesh, res, true, 0.1, 1.0, false, true, 7, 0, true, skip_relax, false, &mut cache,
            )
            .expect("collapse fill");
            let vi = (0..mesh.vertices.len())
                .max_by(|&a, &b| {
                    mesh.vertices[a]
                        .position
                        .x
                        .total_cmp(&mesh.vertices[b].position.x)
                })
                .unwrap();
            for _ in 0..4 {
                mesh.vertices[vi].position.x += 0.003;
                let _ = voxel_remesh_mesh_incremental_ex(
                    &mesh, res, true, 0.1, 1.0, false, true, 7, 0, true, skip_relax, false, &mut cache,
                )
                .expect("collapse warm");
            }
            let frame_ms = ms5(|| {
                mesh.vertices[vi].position.x += 0.003;
                let _ = voxel_remesh_mesh_incremental_ex(
                    &mesh, res, true, 0.1, 1.0, false, true, 7, 0, true, skip_relax, false, &mut cache,
                )
                .expect("collapse drag");
            });
            let c = cache.as_mut().unwrap();
            let mut extracted = core_math::mesh::Mesh {
                vertices: Vec::new(),
                normals: Vec::new(),
                quads: Vec::new(),
            };
            let s2 = ms5(|| {
                extracted = c.grid.surface_nets_octree(true, REMESH_COLLAPSE_EPS);
            });
            let parents_recomputed = c.collapse_extract.recomputed_parent_slabs;
            let leftover_recomputed = c.collapse_extract.recomputed_leftover_slabs;
            let mut extracted_c = core_math::mesh::Mesh {
                vertices: Vec::new(),
                normals: Vec::new(),
                quads: Vec::new(),
            };
            let s2c = ms5(|| {
                extracted_c = c.grid.surface_nets_octree_cached(
                    true,
                    REMESH_COLLAPSE_EPS,
                    &mut c.collapse_extract,
                    core_math::voxel_sculpt::DirtyCells::None,
                );
            });
            let mut he = HalfEdgeMesh::new();
            let s4 = ms5(|| he = HalfEdgeMesh::from_mesh(&extracted));
            let s4_draft = ms5(|| he = HalfEdgeMesh::from_mesh_draft(&extracted));
            eprintln!(
                "[DRAG STAGES med5] res={res:3} solid  clps=1 norelax={} incr={} frame={frame_ms:>6.1}ms \
                 S2 extract={s2:>6.1}ms  S2 cached(None)={s2c:>6.1}ms  \
                 S3 dirty_verts={}  S4 from_mesh={s4:>6.1}ms  S4 draft={s4_draft:>6.1}ms  \
                 | S2+S4={:>6.1}ms  parents={parents_recomputed} leftover={leftover_recomputed} \
                 verts={} quads={} he_faces={}",
                u8::from(skip_relax),
                c.last_eval_incremental,
                c.collapse_extract.last_project_dirty_verts,
                s2 + s4,
                extracted.vertices.len(),
                extracted.quads.len(),
                he.face_count(),
            );
            }
        }
    }

    #[test]
    fn remesh_resolution_range_includes_512() {
        assert_eq!(REMESH_RES_MAX, 512);
        assert!(REMESH_RES_MIN < REMESH_LIVE_SCRUB_MAX);
        assert!(REMESH_LIVE_SCRUB_MAX < REMESH_RES_MAX);
        assert_eq!(256u32.clamp(REMESH_RES_MIN, REMESH_RES_MAX), 256);
        assert_eq!(512u32.clamp(REMESH_RES_MIN, REMESH_RES_MAX), 512);
        assert_eq!(1024u32.clamp(REMESH_RES_MIN, REMESH_RES_MAX), 512);
    }

    /// Max-res remesh (512³, ~134M cells). Transient dense field; #[ignore] because
    /// this is release-bench / memory territory, not a default unit-test budget.
    /// Run: cargo test -p app --release remesh_at_max_resolution -- --ignored --nocapture
    #[test]
    #[ignore]
    fn remesh_at_max_resolution_is_valid() {
        let cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let t = std::time::Instant::now();
        let out = voxel_remesh_mesh(&cube, REMESH_RES_MAX, true, 0.1, 1.0).expect("max-res remesh");
        eprintln!(
            "[REMESH MAX] res={REMESH_RES_MAX}: {:.1}ms  {}v/{}f",
            t.elapsed().as_secs_f64() * 1e3,
            out.vertex_count(),
            out.face_count()
        );
        assert!(out.validate().is_ok(), "max-res remesh must validate");
        let (mn, mx) = out.bounding_box();
        let extent = (mx - mn).max_element();
        assert!(extent > 0.5 && extent < 2.0, "max-res cube extent out of range: {extent}");
    }

    #[test]
    fn remesh_higher_res_has_more_detail() {
        let cube = HalfEdgeMesh::new_box(Vec3::splat(0.5));
        let lo = voxel_remesh_mesh(&cube, 16, false, 0.1, 1.0).expect("lo");
        let hi = voxel_remesh_mesh(&cube, 48, false, 0.1, 1.0).expect("hi");
        assert!(
            hi.face_count() > lo.face_count(),
            "higher res should emit more faces ({} vs {})",
            hi.face_count(),
            lo.face_count()
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // REGRESSION GATE — REMESHSPIKE (owner 2026-08-11, "remesh always produces
    // weird geometry outside the original mesh").
    //
    // A voxel remesh vertex is a lattice-quantised point on the source surface,
    // so it can never sit further from that surface than the quantisation itself
    // (~1 cell diagonal, and ~0 once reprojection runs). Anything beyond that is
    // a defect, not a resolution artefact — that is the whole assertion.
    //
    // The three dirty fixtures are the three ways the OLD facing-based sign
    // classification broke, all of which shipped blades and internal shells:
    // interpenetrating parts, a few inconsistently-wound polygons, and an
    // inside-out mesh. The clean fixtures pin that nothing moved for well-formed
    // input. See `io_bridge::mesh_to_sdf::band_edge_crossings`.
    // Run: cargo test -p app --release remesh_stays_on -- --nocapture
    // ─────────────────────────────────────────────────────────────────────────

    /// Generalized winding number (Jacobson et al.) — exact inside/outside for a closed
    /// mesh, independent of the sampler under test. ~1 inside, ~0 outside.
    fn winding_number(p: Vec3, verts: &[Vec3], indices: &[u32]) -> f32 {
        let mut w = 0.0f64;
        for t in indices.chunks(3) {
            if t.len() < 3 { break; }
            let a = (verts[t[0] as usize] - p).as_dvec3();
            let b = (verts[t[1] as usize] - p).as_dvec3();
            let c = (verts[t[2] as usize] - p).as_dvec3();
            let (la, lb, lc) = (a.length(), b.length(), c.length());
            if la < 1e-12 || lb < 1e-12 || lc < 1e-12 { continue; }
            let num = a.dot(b.cross(c));
            let den = la * lb * lc + a.dot(b) * lc + a.dot(c) * lb + b.dot(c) * la;
            w += 2.0 * num.atan2(den);
        }
        (w / (4.0 * std::f64::consts::PI)) as f32
    }

    #[test]
    fn remesh_stays_on_the_source_surface() {
        use core_math::half_edge::FaceId;
        // A sphere with 3 faces deliberately reversed — the "imported model with a few
        // inconsistently-wound polygons" case.
        let mut flipped = HalfEdgeMesh::new_uv_sphere(0.5, 32, 24);
        let fids: Vec<FaceId> = (0..flipped.faces.len() as u32)
            .map(FaceId)
            .filter(|&f| flipped.is_face_live(f))
            .collect();
        for &f in [fids[100], fids[101], fids[300]].iter() {
            flipped.flip_face(f);
        }
        // Kitbash: two interpenetrating boxes concatenated into ONE mesh (no boolean) —
        // the shape of a model assembled from overlapping parts (body + ear + leg).
        let mut b2 = HalfEdgeMesh::new_box(Vec3::new(0.25, 0.25, 0.25));
        for v in b2.vertices.iter_mut() {
            v.position += Vec3::new(0.4, 0.3, 0.0);
        }
        let kitbash = crate::poly::boolean::merge_meshes(&HalfEdgeMesh::new_box(Vec3::splat(0.5)), &b2);

        let fixtures: Vec<(&str, HalfEdgeMesh)> = vec![
            ("box", HalfEdgeMesh::new_box(Vec3::splat(0.5))),
            ("kitbash_2_boxes", kitbash),
            ("uv_sphere_lo", HalfEdgeMesh::new_uv_sphere(0.5, 12, 8)),
            ("uv_sphere_hi", HalfEdgeMesh::new_uv_sphere(0.5, 32, 24)),
            ("sphere_3_flipped_faces", flipped),
            ("torus", HalfEdgeMesh::new_torus(0.5, 0.16, 32, 16)),
            ("cone", HalfEdgeMesh::new_cone(0.5, 1.0, 20)),
            ("cylinder", HalfEdgeMesh::new_cylinder(0.4, 1.0, 20)),
        ];
        for (name, src) in &fixtures {
            let (sv_raw, sidx) = src.to_raster_data();
            let sv: Vec<Vec3> = sv_raw.iter().map(|v| Vec3::new(v[0], v[1], v[2])).collect();
            // Orientation probe: signed volume of the tri soup (>0 = outward CCW winding).
            let mut vol = 0.0f64;
            for t in sidx.chunks(3) {
                let (a, b, c) = (
                    sv[t[0] as usize].as_dvec3(),
                    sv[t[1] as usize].as_dvec3(),
                    sv[t[2] as usize].as_dvec3(),
                );
                vol += a.dot(b.cross(c)) / 6.0;
            }
            eprintln!("[{name}] tris={} signed_volume={vol:.5}", sidx.len() / 3);
            for &project in &[0.0f32, 1.0] {
                let res = 64u32;
                let (out, grid, center, sealed, _projected, _) =
                    voxel_remesh_full(src, res, true, 0.1, 1.0, project, false, false)
                        .unwrap_or_else(|| panic!("[{name}] project={project}: remesh returned None"));
                let cell = grid.voxel_size();
                let ov: Vec<Vec3> = out.vertices.iter().map(|v| v.position).collect();
                // Distance to the source surface: snap with a generous cap, measure travel.
                let snapped = io_bridge::mesh_to_sdf::project_points_to_mesh(
                    &ov, &sv, &sidx, cell * 40.0,
                )
                .expect("project");
                let mut d: Vec<f32> =
                    ov.iter().zip(&snapped).map(|(a, b)| (*a - *b).length()).collect();
                let worst_i = (0..d.len()).max_by(|&a, &b| d[a].total_cmp(&d[b])).unwrap_or(0);
                let worst_p = ov[worst_i];
                let over = d.iter().filter(|&&x| x > 2.0 * cell).count();
                d.sort_by(f32::total_cmp);
                let p99 = d[(d.len() * 99 / 100).min(d.len() - 1)];
                eprintln!(
                    "[{name}] project={project} res={res} cell={cell:.4} sealed={sealed} \
                     verts={} | dist/cell: p99={:.2} max={:.2} | >2cell: {over}",
                    ov.len(),
                    p99 / cell,
                    d[d.len() - 1] / cell,
                );
                if over > 0 {
                    // Report WHY before failing: a vertex this far out means the lattice cell
                    // under it was classified on the wrong side of the surface.
                    let g = ((worst_p - center) / cell
                        + Vec3::splat((res as f32 - 1.0) * 0.5))
                        .round();
                    let (gx, gy, gz) = (
                        (g.x as i32).clamp(0, res as i32 - 1) as u32,
                        (g.y as i32).clamp(0, res as i32 - 1) as u32,
                        (g.z as i32).clamp(0, res as i32 - 1) as u32,
                    );
                    let gp = grid.cell_world_pos(gx, gy, gz) + center;
                    let wn = winding_number(gp, &sv, &sidx);
                    let n = res as usize;
                    let dens = grid
                        .density
                        .get_idx(gx as usize + gy as usize * n + gz as usize * n * n);
                    panic!(
                        "[{name}] project={project}: {over}/{} remesh vertices are further than \
                         2 cells from the source surface (max {:.2} cells). Worst {worst_p:?} \
                         -> grid({gx},{gy},{gz}) density={dens:.3} iso={:.3} winding={wn:.3} [{}]",
                        ov.len(),
                        d[d.len() - 1] / cell,
                        grid.iso,
                        if (dens >= grid.iso) != (wn > 0.5) { "SIGN MISMATCH" } else { "sign ok" },
                    );
                }
                // Reprojection is bounded by `PROJECT_MAX_CELLS`, so with it on every vertex
                // must land ON the source surface — a stronger statement than the 2-cell gate.
                if project > 0.5 {
                    assert!(
                        d[d.len() - 1] <= 0.5 * cell,
                        "[{name}] reprojected remesh should sit on the source surface, \
                         max travel {:.2} cells",
                        d[d.len() - 1] / cell
                    );
                }
            }
        }
    }

    /// Ctrl+B + Remesh modifier: each drag frame rebuilds from original then remeshes.
    /// Reports whether incremental remesh survives a width scrub (same segs/selection).
    ///
    /// Run: cargo test -p app --release measure_bevel_with_remesh -- --ignored --nocapture
    #[test]
    #[ignore]
    fn measure_bevel_with_remesh() {
        use core_math::bevel_op::{bevel_edges_offset, BevelParams};
        use core_math::half_edge::{HalfEdgeId, NO_FACE};
        use std::collections::HashSet;

        fn subdiv_n(mut m: HalfEdgeMesh, n: u32) -> HalfEdgeMesh {
            for _ in 0..n {
                let faces: HashSet<u32> = (0..m.faces.len() as u32)
                    .filter(|&fi| m.is_face_live(core_math::half_edge::FaceId(fi)))
                    .collect();
                m.subdivide_faces(&faces);
            }
            m
        }
        fn one_loop(m: &HalfEdgeMesh) -> HashSet<u32> {
            for i in 0..m.half_edges.len() as u32 {
                if m.he(HalfEdgeId(i)).face == NO_FACE {
                    continue;
                }
                let loop_e = m.select_edge_loop(HalfEdgeId(i));
                if loop_e.len() >= 4 {
                    return loop_e.iter().map(|h| h.0).collect();
                }
            }
            HashSet::new()
        }
        fn all_edges(m: &HalfEdgeMesh) -> HashSet<u32> {
            (0..m.half_edges.len() as u32)
                .filter(|&i| m.he(HalfEdgeId(i)).face != NO_FACE)
                .collect()
        }

        let fixtures: Vec<(&str, HalfEdgeMesh, u32)> = vec![
            ("cube-sub2", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 2), 128),
            ("cube-sub3", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 3), 128),
            ("cube-sub3", subdiv_n(HalfEdgeMesh::new_box(Vec3::splat(0.5)), 3), 256),
        ];
        for (name, base, res) in &fixtures {
            let loop_e = one_loop(base);
            let all = all_edges(base);
            for (sel_name, sel) in [("1-loop", &loop_e), ("all-edge", &all)] {
                let p = |w: f32| BevelParams {
                    width: w,
                    segments: 3,
                    shape: 0.5,
                    clamp: true,
                };
                let mut cache = None;
                let t0 = std::time::Instant::now();
                let _ = voxel_remesh_mesh_incremental_ex(
                    base, *res, true, 0.1, 1.0, false, true, 9, 0, true, true, true, &mut cache,
                )
                .expect("pre-bevel remesh");
                let t_pre = t0.elapsed().as_secs_f64() * 1e3;
                eprintln!(
                    "[BEVEL+REMESH] {name} res={res} {sel_name} pre={t_pre:.1}ms skip={} inV={}",
                    last_incr_skip(),
                    base.vertex_count()
                );
                let mut prev_counts: Option<(usize, usize, usize)> = None;
                let mut prev_pos: Option<Vec<Vec3>> = None;
                for (i, w) in [0.010, 0.015, 0.020, 0.025, 0.030].iter().enumerate() {
                    let beveled = bevel_edges_offset(base, sel, &p(*w));
                    let counts = (
                        beveled.vertices.len(),
                        beveled.half_edges.len(),
                        beveled.faces.len(),
                    );
                    let moved = if let Some(pp) = &prev_pos {
                        if pp.len() == beveled.vertices.len() {
                            beveled
                                .vertices
                                .iter()
                                .zip(pp)
                                .filter(|(v, p)| v.position != **p)
                                .count()
                        } else {
                            beveled.vertices.len()
                        }
                    } else {
                        beveled.vertices.len()
                    };
                    let topo_same = prev_counts == Some(counts);
                    let t1 = std::time::Instant::now();
                    let _ = voxel_remesh_mesh_incremental_ex(
                        &beveled, *res, true, 0.1, 1.0, false, true, 9, 0, true, true, true,
                        &mut cache,
                    )
                    .expect("bevel remesh frame");
                    let ms = t1.elapsed().as_secs_f64() * 1e3;
                    let incr = cache.as_ref().unwrap().last_eval_incremental;
                    eprintln!(
                        "[BEVEL+REMESH] {name} res={res} {sel_name} f{i} w={w:.3}  \
                         {ms:>6.1}ms incr={} skip={:<8} topo_same={}  \
                         {}v/{}he/{}f moved={moved}/{}",
                        u8::from(incr),
                        last_incr_skip(),
                        u8::from(topo_same),
                        counts.0,
                        counts.1,
                        counts.2,
                        beveled.vertices.len()
                    );
                    prev_counts = Some(counts);
                    prev_pos = Some(beveled.vertices.iter().map(|v| v.position).collect());
                }
            }
        }
    }
}

