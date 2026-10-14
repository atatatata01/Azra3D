//! Phase D3 — cluster DAG builder (PHASE_D3_CLUSTER_DAG_PLAN.md).
//!
//! meshopt-backed: `build_meshlets` (~124-tri clusters + bounds/cones) and
//! border-locked simplification (G7's hard locks without touching the house
//! decimator). This module must stay DETERMINISTIC end-to-end — meshopt's C is
//! deterministic per call; every ordering on our side keys on stable indices,
//! never on hash-map iteration.
//!
//! Commit 2 = spike: dependency + smoke tests only (go/no-go for the meshopt
//! path; fallback documented in the plan). The builder proper lands in 3–5.

use core_math::cluster_dag::{Cluster, ClusterBakeParams, ClusterDag, LevelRange, NO_GROUP};

fn vertex_adapter(pos: &[[f32; 3]]) -> meshopt::VertexDataAdapter<'_> {
    let bytes: &[u8] = bytemuck::cast_slice(pos);
    meshopt::VertexDataAdapter::new(bytes, 12, 0).expect("vertex adapter")
}

/// Build level 0: split the source triangles into ≤(max_vertices, max_triangles)
/// meshlets and record them as `Cluster`s slicing a NEW shared index pool (global
/// indices — the original vertex pool is reused untouched, so level 0 is exact:
/// self_error = 0). Deterministic: meshopt is deterministic per call and clusters
/// are emitted in meshopt's stable output order.
pub fn build_level0(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    params: &ClusterBakeParams,
    source_hash: u64,
) -> ClusterDag {
    let va = vertex_adapter(positions);
    // Cache-optimize first: raw authoring index order gives fragmented strip
    // meshlets (~32 tris instead of ~124, near-100% border verts) — which then
    // makes LockBorder lock EVERYTHING and the DAG can't coarsen at all
    // (measured: 786440 → 786428 tris in one level). Deterministic (pure C).
    let indices = meshopt::optimize_vertex_cache(indices, positions.len());
    let meshlets = meshopt::build_meshlets(
        &indices,
        &va,
        params.max_vertices as usize,
        params.max_triangles as usize,
        0.0,
    );
    let mut dag = ClusterDag {
        positions: positions.to_vec(),
        normals: normals.to_vec(),
        indices: Vec::with_capacity(indices.len()),
        clusters: Vec::with_capacity(meshlets.meshlets.len()),
        groups: Vec::new(),
        levels: Vec::new(),
        source_hash,
    };
    for m in meshlets.iter() {
        let index_offset = dag.indices.len() as u32;
        // meshlet triangles index the meshlet-local vertex list → back to global.
        for &t in m.triangles {
            dag.indices.push(m.vertices[t as usize]);
        }
        let b = meshopt::compute_meshlet_bounds(m, &va);
        dag.clusters.push(Cluster {
            index_offset,
            index_count: m.triangles.len() as u32,
            bounds_center: b.center,
            bounds_radius: b.radius,
            cone_axis: b.cone_axis,
            cone_cutoff: b.cone_cutoff,
            self_error: 0.0,
            // Until coarser levels exist, level 0 is also the root: never replaced.
            parent_error: f32::INFINITY,
            group: NO_GROUP,
            child_group: NO_GROUP,
        });
    }
    dag.levels.push(LevelRange {
        first_cluster: 0,
        cluster_count: dag.clusters.len() as u32,
    });
    dag
}

/// Partition one level's clusters into groups of ~`group_size` by recursive
/// bisection over bounds centers. DETERMINISTIC.
///
/// **Sibling cohesion:** co-simplified meshlets share `child_group` and must land
/// in the **same** next-level group so they share one parent_error / parent sphere.
/// Spatial bisection alone split them → partial selection → undrawn islands.
/// Supers (atomic sibling sets, or singletons when `child_group == NO_GROUP`) are
/// partitioned; a super never splits even if larger than `group_size`.
pub fn partition_groups(clusters: &[Cluster], group_size: u32) -> (Vec<u32>, u32) {
    let n = clusters.len();
    if n == 0 {
        return (Vec::new(), 0);
    }
    // Build supers in first-seen order (stable cluster index order).
    let mut super_members: Vec<Vec<u32>> = Vec::new();
    let mut cg_to_super: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for (i, c) in clusters.iter().enumerate() {
        let i = i as u32;
        if c.child_group == NO_GROUP {
            super_members.push(vec![i]);
            continue;
        }
        if let Some(&si) = cg_to_super.get(&c.child_group) {
            super_members[si].push(i);
        } else {
            cg_to_super.insert(c.child_group, super_members.len());
            super_members.push(vec![i]);
        }
    }
    for m in &mut super_members {
        m.sort_unstable();
    }
    let super_centers: Vec<[f32; 3]> = super_members
        .iter()
        .map(|members| {
            let mut s = [0.0f64; 3];
            for &ci in members {
                let c = clusters[ci as usize].bounds_center;
                s[0] += c[0] as f64;
                s[1] += c[1] as f64;
                s[2] += c[2] as f64;
            }
            let inv = 1.0 / members.len() as f64;
            [(s[0] * inv) as f32, (s[1] * inv) as f32, (s[2] * inv) as f32]
        })
        .collect();
    let (super_group, n_groups) = partition_points(&super_centers, group_size);
    let mut group_of = vec![0u32; n];
    for (si, members) in super_members.iter().enumerate() {
        let g = super_group[si];
        for &ci in members {
            group_of[ci as usize] = g;
        }
    }
    (group_of, n_groups)
}

/// Recursive bisection of points (super-node centers). Same median-split rules as
/// the old per-cluster partition — longest axis, stable index ties.
fn partition_points(centers: &[[f32; 3]], group_size: u32) -> (Vec<u32>, u32) {
    let n = centers.len();
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut group_of = vec![0u32; n];
    let mut next_group = 0u32;
    let mut stack = vec![(0usize, n)];
    while let Some((start, end)) = stack.pop() {
        let count = end - start;
        if count as u32 <= group_size.max(2) {
            for &si in &order[start..end] {
                group_of[si as usize] = next_group;
            }
            next_group += 1;
            continue;
        }
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for &si in &order[start..end] {
            let c = centers[si as usize];
            for a in 0..3 {
                mn[a] = mn[a].min(c[a]);
                mx[a] = mx[a].max(c[a]);
            }
        }
        let ext = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
        let axis = if ext[0] >= ext[1] && ext[0] >= ext[2] {
            0
        } else if ext[1] >= ext[2] {
            1
        } else {
            2
        };
        order[start..end].sort_unstable_by_key(|&si| {
            let v = centers[si as usize][axis];
            let bits = v.to_bits();
            let key = if bits & 0x8000_0000 != 0 {
                !bits
            } else {
                bits | 0x8000_0000
            };
            (key, si)
        });
        let mid = start + count / 2;
        stack.push((start, mid));
        stack.push((mid, end));
    }
    (group_of, next_group)
}

use core_math::cluster_dag::ClusterGroup;

/// Compact a submesh (global indices) to a dense local vertex set. meshopt calls
/// scale with the VERTEX BUFFER size, not the index count — feeding the full pool
/// per group made a 786k-tri build run for minutes (each of ~1k group calls per
/// level scanned 393k verts). Deterministic: sorted-unique remap, no hash maps.
fn compact_submesh(
    positions: &[[f32; 3]],
    global_indices: &[u32],
) -> (Vec<[f32; 3]>, Vec<u32>, Vec<u32>) {
    let mut uniq: Vec<u32> = global_indices.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    let local_pos: Vec<[f32; 3]> = uniq.iter().map(|&g| positions[g as usize]).collect();
    let local_idx: Vec<u32> = global_indices
        .iter()
        .map(|g| uniq.binary_search(g).expect("index in uniq") as u32)
        .collect();
    (local_pos, local_idx, uniq)
}

/// Weld vertices by POSITION. Raster vertex copies are flat-shaded (each quad
/// owns its vertices — measured: exactly 32 tris per 64-vert meshlet on the
/// dense-stress body), which makes EVERY edge topologically open → LockBorder
/// locks the whole mesh and the DAG cannot coarsen. Weld for DAG topology;
/// normals become area-implied averages (far-LOD geometry — D4 renders L0+ from
/// these pools; attribute-seam-aware splitting is a D4+ refinement).
/// Deterministic: sorted position bits, stable first-occurrence representative.
fn weld_by_position(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    // Sort (position bits, original index) → identical positions adjacent;
    // representative = lowest original index (stable).
    let key = |p: [f32; 3]| (p[0].to_bits(), p[1].to_bits(), p[2].to_bits());
    let mut order: Vec<u32> = (0..positions.len() as u32).collect();
    order.sort_unstable_by_key(|&i| (key(positions[i as usize]), i));
    let mut remap = vec![0u32; positions.len()];
    let mut welded_pos: Vec<[f32; 3]> = Vec::new();
    let mut welded_nrm: Vec<[f32; 3]> = Vec::new();
    let mut i = 0usize;
    while i < order.len() {
        let k = key(positions[order[i] as usize]);
        let new_id = welded_pos.len() as u32;
        welded_pos.push(positions[order[i] as usize]);
        // Average the duplicate normals (order[i..j] is index-stable) — good
        // enough for coarse LOD shading; renormalized below.
        let mut n = [0.0f32; 3];
        let mut j = i;
        while j < order.len() && key(positions[order[j] as usize]) == k {
            remap[order[j] as usize] = new_id;
            for a in 0..3 {
                n[a] += normals[order[j] as usize][a];
            }
            j += 1;
        }
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-20);
        welded_nrm.push([n[0] / len, n[1] / len, n[2] / len]);
        i = j;
    }
    let welded_idx: Vec<u32> = indices.iter().map(|&ix| remap[ix as usize]).collect();
    (welded_pos, welded_nrm, welded_idx)
}

/// Build the full DAG: level 0 from `build_level0`, then repeatedly
/// group → merge → border-locked simplify → re-meshlet until the level is small.
///
/// The group boundary IS the merged submesh's open boundary, so meshopt's
/// `LockBorder` gives G7's hard locks for free: boundary vertices are never
/// moved or removed → watertight seams between adjacent groups at every level.
/// Errors accumulate monotonically (`child self_error + group simplify error`),
/// preserving the DAG-cut invariant `self_error ≤ parent_error`.
pub fn build_cluster_dag(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    params: &ClusterBakeParams,
    source_hash: u64,
) -> ClusterDag {
    // Weld first — flat-shaded raster copies make every edge a border (see fn doc).
    // Degenerate triangles produced by welding (zero-area seams) are dropped.
    let (positions, normals, indices) = weld_by_position(positions, normals, indices);
    let indices: Vec<u32> = indices
        .chunks_exact(3)
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .flatten()
        .copied()
        .collect();
    let (positions, normals, indices) = (&positions[..], &normals[..], &indices[..]);
    // Characteristic mesh size (AABB diagonal). Floors world-space LOD error so
    // screen-space bands open around thr≈1px at normal camera distances. With a
    // pure meshopt floor of 1e-7, L0 claimed only [0, ≪1) and thr=1 left ~17k
    // unclaimed L0 regions on dense 1x9 (owner debug-view swiss cheese).
    let mesh_extent = {
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for p in positions {
            for a in 0..3 {
                mn[a] = mn[a].min(p[a]);
                mx[a] = mx[a].max(p[a]);
            }
        }
        let dx = mx[0] - mn[0];
        let dy = mx[1] - mn[1];
        let dz = mx[2] - mn[2];
        (dx * dx + dy * dy + dz * dz).sqrt().max(1e-3)
    };
    // ~0.2% of extent: at d≈2·extent, H=1000, tan½fov=0.5 → ~1px for this error.
    let min_err_step = (mesh_extent * 0.002).max(1e-5);

    let mut dag = build_level0(positions, normals, indices, params, source_hash);
    loop {
        let level = dag.levels.len() - 1;
        let lr = *dag.levels.last().expect("level exists");
        if lr.cluster_count <= params.root_cluster_target {
            break;
        }
        let lo = lr.first_cluster as usize;
        let hi = lo + lr.cluster_count as usize;

        // Partition this level into groups, then make groups CONTIGUOUS by stably
        // reordering the level's clusters by (group, original index). Nothing
        // references these cluster indices yet (child_group links are assigned
        // below, group records after the permute) — safe to permute.
        let (group_of, n_groups) = partition_groups(&dag.clusters[lo..hi], params.group_size);
        let mut order: Vec<u32> = (0..lr.cluster_count).collect();
        order.sort_unstable_by_key(|&i| (group_of[i as usize], i));
        let permuted: Vec<Cluster> = order.iter().map(|&i| dag.clusters[lo + i as usize]).collect();
        dag.clusters[lo..hi].copy_from_slice(&permuted);

        let group_base = dag.groups.len() as u32;
        let next_first_cluster = dag.clusters.len() as u32;
        let mut next_clusters: Vec<Cluster> = Vec::new();

        let mut cursor = 0u32;
        for g in 0..n_groups {
            // Contiguous run of this group's clusters after the permute.
            let g_start = cursor;
            while cursor < lr.cluster_count && group_of[order[cursor as usize] as usize] == g {
                cursor += 1;
            }
            let g_end = cursor;
            let g_clusters = &dag.clusters[lo + g_start as usize..lo + g_end as usize];

            // Merge the group's indices (global pool — shared vertices dedupe naturally).
            let mut merged: Vec<u32> = Vec::new();
            let mut child_err_max = 0.0f32;
            for c in g_clusters {
                merged.extend_from_slice(
                    &dag.indices[c.index_offset as usize..(c.index_offset + c.index_count) as usize],
                );
                child_err_max = child_err_max.max(c.self_error);
            }

            // Compact to a LOCAL vertex set — meshopt cost scales with the vertex
            // buffer, not the index count (full-pool feeding was a multi-minute
            // stall at 786k tris).
            let (local_pos, local_idx, local_to_global) = compact_submesh(&dag.positions, &merged);
            let lva = vertex_adapter(&local_pos);
            // Border-locked simplify toward the ratio. The submesh's open border
            // (the group seam) is locked — never moved, never collapsed.
            let target = ((merged.len() as f32 * params.simplify_ratio) as usize / 3).max(1) * 3;
            let mut rel_err = 0.0f32;
            let simplified_local = meshopt::simplify(
                &local_idx,
                &lva,
                target,
                0.25,
                meshopt::SimplifyOptions::LockBorder,
                Some(&mut rel_err),
            );
            // meshopt's `result_error` is RELATIVE TO THE SUBSET EXTENTS — it must be
            // multiplied by `simplify_scale` to become a WORLD-SPACE length. Storing
            // the raw relative value made every DAG error 10-100x too small, so the
            // screen-space test never crossed 1px at realistic distances and the cut
            // collapsed to a near-constant level: LOD with no distance gradient
            // (owner 2026-08-05: "the clusters do not seem to have any logic").
            let abs_err = (rel_err * meshopt::simplify_scale(&lva)).max(min_err_step);
            // Strictly monotone error up the DAG: `self ≤ τ < parent` is unsatisfiable
            // when self_error == parent_error. Floor each step by mesh-extent so
            // projected bands open around thr=1 at normal camera distances.
            let mut group_err = child_err_max + abs_err;
            let min_step = (child_err_max.abs() * 1e-5).max(min_err_step);
            if group_err <= child_err_max {
                group_err = child_err_max + min_step;
            }

            // Group bounds must CONTAIN every sphere below it — its members' own
            // spheres AND the spheres their errors were measured over (their child
            // groups). Nesting is what makes the projected error non-decreasing up
            // the DAG: if sphere_p ⊇ sphere_c then (d_p − r_p) ≤ (d_c − r_c) by the
            // triangle inequality, so with error_p ≥ error_c the projected error can
            // only grow. That monotonicity is exactly what makes the per-cluster cut
            // TOTAL — every region gets exactly one drawn level. Loose (non-nesting)
            // group spheres broke it and tore holes in large meshes (the 2026-08-04
            // terrain flicker: a parent sphere whose centre sat further from the
            // camera could project a SMALLER error than its child, leaving a
            // threshold band that no level claimed).
            let mut spheres: Vec<([f32; 3], f32)> = Vec::with_capacity(g_clusters.len() * 2);
            for c in g_clusters {
                spheres.push((c.bounds_center, c.bounds_radius));
                if c.child_group != NO_GROUP {
                    let cg = &dag.groups[c.child_group as usize];
                    spheres.push((cg.bounds_center, cg.bounds_radius));
                }
            }
            let mut center = [0.0f64; 3];
            for (c, _) in &spheres {
                for a in 0..3 {
                    center[a] += c[a] as f64;
                }
            }
            let inv = 1.0 / spheres.len() as f64;
            let center = [(center[0] * inv) as f32, (center[1] * inv) as f32, (center[2] * inv) as f32];
            let mut radius = 0.0f32;
            for (c, r) in &spheres {
                let d = (0..3).map(|a| (c[a] - center[a]).powi(2)).sum::<f32>().sqrt();
                radius = radius.max(d + r);
            }
            let group_id = group_base + g;
            dag.groups.push(ClusterGroup {
                first_cluster: lr.first_cluster + g_start,
                cluster_count: g_end - g_start,
                bounds_center: center,
                bounds_radius: radius,
                merged_error: group_err,
            });
            // Children: owned by this group; replaced at error `group_err`.
            for c in &mut dag.clusters[lo + g_start as usize..lo + g_end as usize] {
                c.group = group_id;
                c.parent_error = group_err;
            }

            // Re-meshlet the simplified geometry into next-level clusters (still in
            // local space; indices map back to the global pool on push). Cache-
            // optimize first — same fragmentation trap as level 0.
            let simplified_local =
                meshopt::optimize_vertex_cache(&simplified_local, local_pos.len());
            let ml = meshopt::build_meshlets(
                &simplified_local,
                &lva,
                params.max_vertices as usize,
                params.max_triangles as usize,
                0.0,
            );
            for m in ml.iter() {
                let index_offset = dag.indices.len() as u32;
                for &t in m.triangles {
                    dag.indices.push(local_to_global[m.vertices[t as usize] as usize]);
                }
                let b = meshopt::compute_meshlet_bounds(m, &lva);
                next_clusters.push(Cluster {
                    index_offset,
                    index_count: m.triangles.len() as u32,
                    bounds_center: b.center,
                    bounds_radius: b.radius,
                    cone_axis: b.cone_axis,
                    cone_cutoff: b.cone_cutoff,
                    self_error: group_err,
                    parent_error: f32::INFINITY,
                    group: NO_GROUP,
                    child_group: group_id,
                });
            }
        }

        // Convergence guard: a level that failed to shrink (heavy locked borders)
        // would loop forever — stop; the current level stays root.
        let old_tris: u32 = (lo..hi).map(|i| dag.clusters[i].index_count / 3).sum();
        let new_tris: u32 = next_clusters.iter().map(|c| c.index_count / 3).sum();
        if std::env::var("AZRA_CLUSTER_LOG").is_ok_and(|v| v == "1") {
            eprintln!(
                "[ClusterBake] level {} → {}: {} → {} tris ({} groups)",
                level,
                level + 1,
                old_tris,
                new_tris,
                n_groups
            );
        }
        if next_clusters.is_empty() || new_tris * 10 >= old_tris * 9 {
            // Undo the parent links — this level remains the root (group records
            // are truncated below, so the group ids assigned above dangle: clear).
            for c in &mut dag.clusters[lo..hi] {
                c.parent_error = f32::INFINITY;
                c.group = NO_GROUP;
            }
            dag.groups.truncate(group_base as usize);
            let _ = level;
            break;
        }
        dag.clusters.extend(next_clusters);
        dag.levels.push(LevelRange {
            first_cluster: next_first_cluster,
            cluster_count: dag.clusters.len() as u32 - next_first_cluster,
        });
    }
    // Strict self < parent bands only. Sibling agreement comes from
    // `partition_groups` cohesion (same next-level group → same parent_error).
    // Post-hoc parent_error **max** opened dual-select τ windows (A stays selected
    // while coarser also selects) → WP11 thrash; rejected in favor of cohesion.
    enforce_cut_invariants(&mut dag);
    dag
}

/// Post-pass: every cluster has a **satisfiable** error band
/// (`self ≤ τ < parent` impossible when self >= parent).
fn enforce_cut_invariants(dag: &mut ClusterDag) {
    for c in &mut dag.clusters {
        if c.parent_error.is_finite() && c.self_error >= c.parent_error {
            let bump = (c.self_error.abs() * 1e-5).max(1e-7);
            c.parent_error = c.self_error + bump;
        }
    }
}

// ============================================================
// G8 — content-addressed disk cache (raw-slab blob, atomic write)
// ============================================================

/// Bumped whenever the builder's OUTPUT could change (algorithm, params meaning,
/// blob layout). Part of the cache key → old blobs simply miss, never misread.
/// v7: sibling-cohesive group partition (no split of same child_group).
/// v6: mesh-extent min error step. v5: post-hoc parent_error max (reverted).
/// v4: strict self < parent. v3: world-space meshopt error.
pub const CLUSTER_BUILDER_VERSION: u32 = 7;
const BLOB_MAGIC: &[u8; 8] = b"FL4CDAG1";

/// FNV-1a — STABLE across processes/versions (std DefaultHasher is not).
pub struct StableHasher(pub u64);
impl Default for StableHasher {
    fn default() -> Self {
        Self(0xcbf29ce484222325)
    }
}
impl std::hash::Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
}

/// Content key: **positions + indices + params + builder version**.
///
/// Normals are **not** in the key (topology / position only). Cold builds always
/// receive the live normal snapshot from the mesh cache at spawn, so a pure
/// normal-only edit still invalidates via mesh `pt_stamp` / re-bake, not this key.
/// Blob still stores normals for load-without-rebuild. WP14: documented; no bump.
pub fn dag_cache_key(positions: &[[f32; 3]], indices: &[u32], params: &ClusterBakeParams) -> u64 {
    use std::hash::Hasher;
    let mut h = StableHasher::default();
    h.write(bytemuck::cast_slice(positions));
    h.write(bytemuck::cast_slice(indices));
    params.hash_into(&mut h);
    h.write(&CLUSTER_BUILDER_VERSION.to_le_bytes());
    h.finish()
}

fn push_slab<T: bytemuck::Pod>(out: &mut Vec<u8>, v: &[T]) {
    out.extend_from_slice(&(v.len() as u64).to_le_bytes());
    out.extend_from_slice(bytemuck::cast_slice(v));
}

fn read_slab<T: bytemuck::Pod + Clone>(data: &[u8], off: &mut usize) -> Option<Vec<T>> {
    let n = u64::from_le_bytes(data.get(*off..*off + 8)?.try_into().ok()?) as usize;
    *off += 8;
    let bytes = n.checked_mul(std::mem::size_of::<T>())?;
    let slice = data.get(*off..*off + bytes)?;
    *off += bytes;
    // Alignment-safe: copy through pod_read (cast_slice would require alignment).
    let mut out = vec![T::zeroed(); n];
    bytemuck::cast_slice_mut::<T, u8>(&mut out).copy_from_slice(slice);
    Some(out)
}

/// Serialize to the raw-slab blob (little-endian; bincode measured too slow for
/// the <50ms 1M-tri load gate — slabs deserialize as 4 memcpys).
pub fn dag_to_blob(dag: &ClusterDag) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        64 + dag.positions.len() * 24 + dag.indices.len() * 4 + dag.clusters.len() * 52,
    );
    out.extend_from_slice(BLOB_MAGIC);
    out.extend_from_slice(&CLUSTER_BUILDER_VERSION.to_le_bytes());
    out.extend_from_slice(&dag.source_hash.to_le_bytes());
    push_slab(&mut out, &dag.positions);
    push_slab(&mut out, &dag.normals);
    push_slab(&mut out, &dag.indices);
    push_slab(&mut out, &dag.clusters);
    push_slab(&mut out, &dag.groups);
    push_slab(&mut out, &dag.levels);
    out
}

/// Parse a blob. ANY malformation → None (cache miss, fail closed, rebuild).
pub fn dag_from_blob(data: &[u8]) -> Option<ClusterDag> {
    if data.get(..8)? != BLOB_MAGIC {
        return None;
    }
    let ver = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
    if ver != CLUSTER_BUILDER_VERSION {
        return None;
    }
    let source_hash = u64::from_le_bytes(data.get(12..20)?.try_into().ok()?);
    let mut off = 20usize;
    let dag = ClusterDag {
        positions: read_slab(data, &mut off)?,
        normals: read_slab(data, &mut off)?,
        indices: read_slab(data, &mut off)?,
        clusters: read_slab(data, &mut off)?,
        groups: read_slab(data, &mut off)?,
        levels: read_slab(data, &mut off)?,
        source_hash,
    };
    if off != data.len() {
        return None; // trailing garbage = corrupt
    }
    Some(dag)
}

/// Write `<dir>/<key:016x>.dagblob` atomically (temp + rename).
pub fn write_dag_cached(dir: &std::path::Path, key: u64, dag: &ClusterDag) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(format!("{key:016x}.dagblob"));
    let tmp_path = dir.join(format!("{key:016x}.dagblob.tmp"));
    std::fs::write(&tmp_path, dag_to_blob(dag))?;
    // Windows rename-over-existing fails — remove first (another writer racing us
    // wrote identical content, so losing the race is harmless).
    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&tmp_path, &final_path)
}

/// Load by key; None on absent/corrupt/version-mismatch (fail closed → rebuild).
pub fn load_dag_cached(dir: &std::path::Path, key: u64) -> Option<ClusterDag> {
    let data = std::fs::read(dir.join(format!("{key:016x}.dagblob"))).ok()?;
    dag_from_blob(&data)
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn blob_roundtrips_byte_exact() {
        let (pos, nrm, idx) = test_uv_sphere(24, 32);
        let params = ClusterBakeParams::default();
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, 5);
        let blob = dag_to_blob(&dag);
        let back = dag_from_blob(&blob).expect("roundtrip parse");
        assert_eq!(format!("{dag:?}"), format!("{back:?}"));
        assert_eq!(dag_to_blob(&back), blob, "re-serialize must be byte-identical");
    }

    #[test]
    fn key_changes_with_params_and_content() {
        let (pos, _, idx) = test_uv_sphere(12, 16);
        let p = ClusterBakeParams::default();
        let k = dag_cache_key(&pos, &idx, &p);
        assert_eq!(k, dag_cache_key(&pos, &idx, &p), "key must be stable");
        let p2 = ClusterBakeParams { group_size: p.group_size + 1, ..p };
        assert_ne!(k, dag_cache_key(&pos, &idx, &p2));
        let mut pos2 = pos.clone();
        pos2[0][0] += 0.001;
        assert_ne!(k, dag_cache_key(&pos2, &idx, &p));
    }

    #[test]
    fn corrupt_blob_is_a_miss() {
        let (pos, nrm, idx) = test_uv_sphere(12, 16);
        let params = ClusterBakeParams::default();
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, 5);
        let mut blob = dag_to_blob(&dag);
        assert!(dag_from_blob(&blob[..blob.len() - 3]).is_none(), "truncated must miss");
        blob[0] ^= 0xFF;
        assert!(dag_from_blob(&blob).is_none(), "bad magic must miss");
    }

    #[test]
    fn disk_write_load_roundtrip() {
        let dir = std::env::temp_dir().join("azra_cluster_cache_test");
        let _ = std::fs::remove_dir_all(&dir);
        let (pos, nrm, idx) = test_uv_sphere(12, 16);
        let params = ClusterBakeParams::default();
        let key = dag_cache_key(&pos, &idx, &params);
        assert!(load_dag_cached(&dir, key).is_none(), "empty cache must miss");
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, key);
        write_dag_cached(&dir, key, &dag).expect("write");
        let back = load_dag_cached(&dir, key).expect("hit");
        assert_eq!(format!("{dag:?}"), format!("{back:?}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
pub(crate) fn test_uv_sphere(rings: u32, segs: u32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let (pos, idx) = spike_tests::uv_sphere(rings, segs);
    let normals = pos.clone(); // unit sphere: normal == position
    (pos, normals, idx)
}

#[cfg(test)]
mod dag_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Boundary edge set (edges used by exactly one triangle) as sorted pairs.
    fn boundary_edges(indices: &[u32]) -> BTreeSet<(u32, u32)> {
        let mut count = std::collections::BTreeMap::new();
        for t in indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let k = (a.min(b), a.max(b));
                *count.entry(k).or_insert(0u32) += 1;
            }
        }
        count.into_iter().filter(|&(_, n)| n == 1).map(|(k, _)| k).collect()
    }

    fn full_dag() -> (ClusterDag, ClusterBakeParams) {
        let (pos, nrm, idx) = test_uv_sphere(48, 64); // 6144 tris → several levels
        let params = ClusterBakeParams::default();
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, 9);
        (dag, params)
    }

    #[test]
    fn dag_builds_shrinks_and_validates() {
        let (dag, params) = full_dag();
        dag.validate(&params).expect("full DAG must validate");
        assert!(dag.levels.len() >= 3, "expected multiple levels, got {}", dag.levels.len());
        for l in 1..dag.levels.len() {
            assert!(
                dag.level_triangles(l) < dag.level_triangles(l - 1),
                "level {l} did not shrink"
            );
        }
        eprintln!(
            "[DAG] levels={} tris: {:?}",
            dag.levels.len(),
            (0..dag.levels.len()).map(|l| dag.level_triangles(l)).collect::<Vec<_>>()
        );
    }

    /// Phase-3: `self ≤ τ < parent` needs a **strict** band — equal errors are undrawable.
    #[test]
    fn dag_cut_bands_are_strictly_open() {
        let (dag, params) = full_dag();
        dag.validate(&params).expect("validate");
        let mut checked = 0usize;
        for c in &dag.clusters {
            if c.parent_error.is_infinite() {
                continue;
            }
            assert!(
                c.self_error < c.parent_error,
                "self_error {} >= parent_error {} (unsatisfiable cut band)",
                c.self_error,
                c.parent_error
            );
            checked += 1;
        }
        assert!(checked > 0, "expected non-root clusters");
    }

    #[test]
    fn dag_deterministic() {
        let (pos, nrm, idx) = test_uv_sphere(48, 64);
        let params = ClusterBakeParams::default();
        let a = build_cluster_dag(&pos, &nrm, &idx, &params, 9);
        let b = build_cluster_dag(&pos, &nrm, &idx, &params, 9);
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "DAG build must be bit-identical");
    }

    /// WATERTIGHT GATE: for every group, the boundary edge set of the merged
    /// children equals the boundary edge set of the simplified replacement —
    /// LockBorder must keep group seams exactly (positions untouched: simplify
    /// is collapse-only over the shared pool, so edge identity == geometry).
    #[test]
    fn dag_watertight_group_seams() {
        let (dag, _params) = full_dag();
        assert!(!dag.groups.is_empty());
        for (gi, g) in dag.groups.iter().enumerate() {
            let mut child_idx = Vec::new();
            for c in
                &dag.clusters[g.first_cluster as usize..(g.first_cluster + g.cluster_count) as usize]
            {
                child_idx.extend_from_slice(
                    &dag.indices[c.index_offset as usize..(c.index_offset + c.index_count) as usize],
                );
            }
            let mut parent_idx = Vec::new();
            for c in &dag.clusters {
                if c.child_group == gi as u32 {
                    parent_idx.extend_from_slice(
                        &dag.indices
                            [c.index_offset as usize..(c.index_offset + c.index_count) as usize],
                    );
                }
            }
            if parent_idx.is_empty() {
                continue; // root level group with no coarser replacement (shouldn't happen, but not a seam issue)
            }
            assert_eq!(
                boundary_edges(&child_idx),
                boundary_edges(&parent_idx),
                "group {gi}: seam boundary changed across levels"
            );
        }
    }

    /// Sabotage: WITHOUT LockBorder the seams must break — proves the gate has teeth.
    #[test]
    fn dag_watertight_sabotage_no_locks_fails() {
        let (pos, nrm, idx) = test_uv_sphere(48, 64);
        let params = ClusterBakeParams::default();
        let dag = build_level0(&pos, &nrm, &idx, &params, 9);
        let (group_of, n_groups) = partition_groups(&dag.clusters, params.group_size);
        let va = vertex_adapter(&dag.positions);
        let mut any_seam_broken = false;
        for g in 0..n_groups {
            let mut merged = Vec::new();
            for (ci, c) in dag.clusters.iter().enumerate() {
                if group_of[ci] == g {
                    merged.extend_from_slice(
                        &dag.indices[c.index_offset as usize..(c.index_offset + c.index_count) as usize],
                    );
                }
            }
            let target = (merged.len() / 2 / 3).max(1) * 3;
            let simplified = meshopt::simplify(
                &merged,
                &va,
                target,
                0.25,
                meshopt::SimplifyOptions::empty(), // NO lock — the sabotage
                None,
            );
            if boundary_edges(&merged) != boundary_edges(&simplified) {
                any_seam_broken = true;
                break;
            }
        }
        assert!(any_seam_broken, "sabotage failed: unlocked simplify never broke a seam");
    }
}

#[cfg(test)]
mod hausdorff_tests {
    use super::*;
    use core_math::mesh_boolean::point_triangle_dist_sq;
    use glam::Vec3;

    /// Deterministic per-triangle barycentric samples (LCG, fixed seed).
    fn sample_level(dag: &ClusterDag, level: usize, n: usize) -> Vec<Vec3> {
        let lr = dag.levels[level];
        let mut tris = Vec::new();
        for c in
            &dag.clusters[lr.first_cluster as usize..(lr.first_cluster + lr.cluster_count) as usize]
        {
            for t in dag.indices[c.index_offset as usize..(c.index_offset + c.index_count) as usize]
                .chunks_exact(3)
            {
                tris.push([t[0], t[1], t[2]]);
            }
        }
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next01 = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32) / (u32::MAX >> 1) as f32
        };
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let t = tris[k % tris.len()];
            let (mut u, mut v) = (next01(), next01());
            if u + v > 1.0 {
                u = 1.0 - u;
                v = 1.0 - v;
            }
            let p = |i: u32| Vec3::from(dag.positions[i as usize]);
            out.push(p(t[0]) * (1.0 - u - v) + p(t[1]) * u + p(t[2]) * v);
        }
        out
    }

    fn dist_to_source(p: Vec3, pos: &[[f32; 3]], idx: &[u32]) -> f32 {
        let mut best = f32::MAX;
        for t in idx.chunks_exact(3) {
            let d = point_triangle_dist_sq(
                p,
                Vec3::from(pos[t[0] as usize]),
                Vec3::from(pos[t[1] as usize]),
                Vec3::from(pos[t[2] as usize]),
            );
            best = best.min(d);
        }
        best.sqrt()
    }

    /// HAUSDORFF GATE: every level's surface stays within a tolerance of the
    /// source that scales with the level's own reported error. Unit sphere →
    /// absolute tolerances are meaningful.
    #[test]
    fn dag_levels_stay_near_source() {
        let (pos, nrm, idx) = test_uv_sphere(48, 64);
        let params = ClusterBakeParams::default();
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, 3);
        for level in 0..dag.levels.len() {
            let lr = dag.levels[level];
            let reported = dag.clusters
                [lr.first_cluster as usize..(lr.first_cluster + lr.cluster_count) as usize]
                .iter()
                .map(|c| c.self_error)
                .fold(0.0f32, f32::max);
            // meshopt reports error relative to mesh extent (~2 for a unit sphere);
            // allow headroom factor 4 + an absolute floor for level 0 (exact).
            let tol = (reported * 2.0 * 4.0).max(1e-6);
            let samples = sample_level(&dag, level, 800);
            let mut worst = 0.0f32;
            for &s in &samples {
                worst = worst.max(dist_to_source(s, &pos, &idx));
            }
            eprintln!("[HAUSDORFF] level={level} reported={reported:.5} worst={worst:.5} tol={tol:.5}");
            assert!(
                worst <= tol,
                "level {level}: one-sided Hausdorff {worst} > tol {tol} (reported err {reported})"
            );
        }
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;

    /// Co-simplified meshlets (same child_group) must share parent_error and group
    /// after a full multi-level build — partial selection is impossible then.
    #[test]
    fn siblings_share_parent_band_after_cohesive_partition() {
        let (pos, nrm, idx) = test_uv_sphere(48, 64);
        let params = ClusterBakeParams::default();
        let dag = build_cluster_dag(&pos, &nrm, &idx, &params, 9);
        assert!(dag.levels.len() >= 2, "need multi-level DAG");
        let mut by_cg: std::collections::BTreeMap<u32, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, c) in dag.clusters.iter().enumerate() {
            if c.child_group != NO_GROUP {
                by_cg.entry(c.child_group).or_default().push(i);
            }
        }
        let mut multi = 0u32;
        for (_cg, members) in by_cg {
            if members.len() < 2 {
                continue;
            }
            multi += 1;
            let pe0 = dag.clusters[members[0]].parent_error;
            let g0 = dag.clusters[members[0]].group;
            for &i in &members[1..] {
                assert_eq!(
                    dag.clusters[i].parent_error.to_bits(),
                    pe0.to_bits(),
                    "sibling parent_error mismatch"
                );
                assert_eq!(
                    dag.clusters[i].group, g0,
                    "sibling group mismatch (cohesion failed)"
                );
            }
        }
        assert!(multi > 0, "expected at least one multi-meshlet child_group");
    }

    #[test]
    fn groups_cover_all_and_are_bounded() {
        let (pos, nrm, idx) = test_uv_sphere(24, 32);
        let params = ClusterBakeParams::default();
        let dag = build_level0(&pos, &nrm, &idx, &params, 1);
        let (group_of, n_groups) = partition_groups(&dag.clusters, params.group_size);
        assert_eq!(group_of.len(), dag.clusters.len());
        assert!(n_groups >= 1);
        let mut sizes = vec![0u32; n_groups as usize];
        for &g in &group_of {
            assert!(g < n_groups);
            sizes[g as usize] += 1;
        }
        for (gi, &s) in sizes.iter().enumerate() {
            assert!(s >= 1, "group {gi} empty");
            assert!(
                s <= params.group_size.max(2),
                "group {gi} size {s} > {}",
                params.group_size
            );
        }
    }

    #[test]
    fn grouping_deterministic() {
        let (pos, nrm, idx) = test_uv_sphere(24, 32);
        let params = ClusterBakeParams::default();
        let dag = build_level0(&pos, &nrm, &idx, &params, 1);
        let a = partition_groups(&dag.clusters, params.group_size);
        let b = partition_groups(&dag.clusters, params.group_size);
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod level0_tests {
    use super::*;

    #[test]
    fn level0_covers_all_triangles_and_validates() {
        let (pos, nrm, idx) = test_uv_sphere(24, 32);
        let params = ClusterBakeParams::default();
        let dag = build_level0(&pos, &nrm, &idx, &params, 7);
        dag.validate(&params).expect("L0 DAG must validate");
        assert_eq!(dag.indices.len(), idx.len(), "same triangle count, remapped order");
        assert_eq!(dag.level_triangles(0) as usize, idx.len() / 3);
        // Triangle MULTISET must match the source exactly (order-independent).
        let key = |i: &[u32]| {
            let mut t = [i[0], i[1], i[2]];
            t.sort_unstable();
            t
        };
        let mut src: Vec<_> = idx.chunks_exact(3).map(key).collect();
        let mut got: Vec<_> = dag.indices.chunks_exact(3).map(key).collect();
        src.sort_unstable();
        got.sort_unstable();
        assert_eq!(src, got, "L0 must be the exact source geometry");
    }

    #[test]
    fn level0_deterministic() {
        let (pos, nrm, idx) = test_uv_sphere(24, 32);
        let params = ClusterBakeParams::default();
        let a = build_level0(&pos, &nrm, &idx, &params, 7);
        let b = build_level0(&pos, &nrm, &idx, &params, 7);
        // Debug formatting covers every field bit-exactly for these plain types
        // (float bits print identically or differ) — no serializer dev-dep needed.
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "L0 build must be bit-identical");
    }
}

#[cfg(test)]
mod spike_tests {
    /// Deterministic UV-sphere triangle soup (positions + indices).
    pub(super) fn uv_sphere(rings: u32, segs: u32) -> (Vec<[f32; 3]>, Vec<u32>) {
        let mut pos = Vec::new();
        for r in 0..=rings {
            let phi = std::f32::consts::PI * r as f32 / rings as f32;
            for s in 0..=segs {
                let theta = std::f32::consts::TAU * s as f32 / segs as f32;
                pos.push([
                    phi.sin() * theta.cos(),
                    phi.cos(),
                    phi.sin() * theta.sin(),
                ]);
            }
        }
        let stride = segs + 1;
        let mut idx = Vec::new();
        for r in 0..rings {
            for s in 0..segs {
                let a = r * stride + s;
                let b = a + stride;
                idx.extend_from_slice(&[a, b, a + 1]);
                idx.extend_from_slice(&[a + 1, b, b + 1]);
            }
        }
        (pos, idx)
    }

    fn adapter(pos: &[[f32; 3]]) -> meshopt::VertexDataAdapter<'_> {
        let bytes: &[u8] = bytemuck::cast_slice(pos);
        meshopt::VertexDataAdapter::new(bytes, 12, 0).expect("adapter")
    }

    #[test]
    fn spike_meshlets_cover_and_bound() {
        let (pos, idx) = uv_sphere(24, 32);
        let va = adapter(&pos);
        let meshlets = meshopt::build_meshlets(&idx, &va, 64, 124, 0.0);
        assert!(!meshlets.meshlets.is_empty(), "no meshlets built");
        let total: usize = meshlets
            .iter()
            .map(|m| m.triangles.len() / 3)
            .sum();
        assert_eq!(total, idx.len() / 3, "every source tri in exactly one meshlet");
        for m in meshlets.iter() {
            assert!(m.triangles.len() / 3 <= 124);
            assert!(m.vertices.len() <= 64);
        }
        // Bounds + backface cones (G12 input) come out well-formed.
        let first = meshlets.iter().next().unwrap();
        let b = meshopt::compute_meshlet_bounds(first, &va);
        assert!(b.radius > 0.0);
        let axis_len2: f32 = b.cone_axis.iter().map(|v| v * v).sum();
        assert!(axis_len2 > 0.01, "degenerate cone axis");
    }

    #[test]
    fn spike_meshlets_deterministic() {
        let (pos, idx) = uv_sphere(24, 32);
        let run = || {
            let va = adapter(&pos);
            let ml = meshopt::build_meshlets(&idx, &va, 64, 124, 0.0);
            let mut sig = Vec::new();
            for m in ml.iter() {
                sig.push(m.vertices.len() as u32);
                sig.extend(m.triangles.iter().map(|&t| t as u32));
                sig.extend_from_slice(m.vertices);
            }
            sig
        };
        assert_eq!(run(), run(), "meshlet build must be bit-identical run-to-run");
    }

    #[test]
    fn spike_simplify_locks_border_and_is_deterministic() {
        let (pos, idx) = uv_sphere(24, 32);
        let va = adapter(&pos);
        // Simplify to ~50% with LockBorder — G7's hard-lock semantics.
        let target = idx.len() / 2;
        let run = || {
            meshopt::simplify(
                &idx,
                &va,
                target,
                0.05,
                meshopt::SimplifyOptions::LockBorder,
                None,
            )
        };
        let a = run();
        let b = run();
        assert_eq!(a, b, "simplify must be bit-identical run-to-run");
        assert!(a.len() < idx.len(), "simplify did nothing");
        assert!(a.len() % 3 == 0);
        // A closed sphere has no border — lock semantics are exercised end-to-end
        // (open-boundary lock behavior is asserted by the D3 watertight gate in
        // commit 5, on real group boundaries).
        // Referenced vertices must be a subset of the original pool, positions
        // untouched (simplify never moves vertices — collapse-only).
        for &v in &a {
            assert!((v as usize) < pos.len());
        }
    }
}
