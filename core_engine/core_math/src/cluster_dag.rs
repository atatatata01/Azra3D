//! Phase D3 — Cluster DAG data model (PHASE_D3_CLUSTER_DAG_PLAN.md).
//!
//! CPU-side virtual-geometry hierarchy: the source mesh split into ~124-triangle
//! clusters (level 0 = finest), clusters grouped and re-simplified into coarser
//! clusters per level, up to a handful of roots. D4's GPU cut selects, per frame,
//! one "cut" through this DAG by screen-space error.
//!
//! PURE DATA + invariants only — the builder (meshopt-backed) lives in
//! `render/src/cluster_build.rs`; this crate stays dependency-free so
//! `cargo test -p core_math` stays fast.
//!
//! This type is a CACHE BLOB, never part of `.fl4` (caches are not authored
//! state). The disk format carries its own magic + version — see G8 in the plan.

use serde::{Deserialize, Serialize};

/// Sentinel for "no group" (level-0 clusters have no finer child group).
pub const NO_GROUP: u32 = u32::MAX;

/// Build parameters. Part of the content key — any change misses the cache.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ClusterBakeParams {
    /// Max vertices per meshlet (meshopt canonical: 64).
    pub max_vertices: u32,
    /// Max triangles per meshlet (meshopt canonical: 124).
    pub max_triangles: u32,
    /// Target group size in clusters (grouping aims for [target/2, target]).
    pub group_size: u32,
    /// Per-level triangle reduction target (0.5 = halve each level).
    pub simplify_ratio: f32,
    /// Stop when a level has ≤ this many clusters.
    pub root_cluster_target: u32,
}

impl Default for ClusterBakeParams {
    fn default() -> Self {
        Self {
            max_vertices: 64,
            max_triangles: 124,
            group_size: 6,
            simplify_ratio: 0.5,
            root_cluster_target: 4,
        }
    }
}

impl ClusterBakeParams {
    /// Stable field-order hash contribution for the disk-cache key (the
    /// `DecimateParams::hash_into` precedent). Caller supplies a STABLE hasher —
    /// never `DefaultHasher` (SipHash keys are not guaranteed across versions).
    pub fn hash_into(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.max_vertices.hash(h);
        self.max_triangles.hash(h);
        self.group_size.hash(h);
        self.simplify_ratio.to_bits().hash(h);
        self.root_cluster_target.hash(h);
    }
}

/// One cluster: a contiguous slice of `ClusterDag::indices`, with culling bounds
/// and the two DAG errors that drive the per-frame cut.
/// `repr(C)` + Pod: the disk cache stores cluster arrays as raw little-endian
/// slabs (bincode is too slow for the <50ms 1M-tri load gate).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cluster {
    pub index_offset: u32,
    /// ≤ max_triangles * 3.
    pub index_count: u32,
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
    /// Backface cone (G12): cluster invisible when dot(view, axis) ≥ cutoff.
    pub cone_axis: [f32; 3],
    pub cone_cutoff: f32,
    /// Simplification error of this cluster vs the geometry it replaced
    /// (0 at level 0 — level 0 IS the source).
    pub self_error: f32,
    /// Error of the coarser group that replaces this cluster (f32::INFINITY at
    /// the roots). DAG cut invariant: draw a cluster iff
    /// `self_error ≤ threshold < parent_error`.
    pub parent_error: f32,
    /// Owning group at this cluster's level (`NO_GROUP` for ungrouped roots).
    pub group: u32,
    /// The group one level FINER whose clusters this one (with its group
    /// siblings) simplifies. `NO_GROUP` at level 0.
    pub child_group: u32,
}

/// A set of same-level clusters merged + re-simplified into the next level.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ClusterGroup {
    /// Range into `ClusterDag::clusters` (groups are built over contiguous,
    /// stably-ordered cluster ranges — determinism requirement).
    pub first_cluster: u32,
    pub cluster_count: u32,
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
    /// Max simplification error accumulated by this group's merge+simplify.
    pub merged_error: f32,
}

/// Clusters belonging to one DAG level. Level 0 = finest (source triangles).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LevelRange {
    pub first_cluster: u32,
    pub cluster_count: u32,
}

/// The full hierarchy over shared vertex/index pools.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClusterDag {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Shared index pool; every cluster is a contiguous slice of it.
    pub indices: Vec<u32>,
    pub clusters: Vec<Cluster>,
    pub groups: Vec<ClusterGroup>,
    pub levels: Vec<LevelRange>,
    /// Content key of the source mesh + params this DAG was built from.
    pub source_hash: u64,
}

impl ClusterDag {
    /// Total triangles at one level.
    pub fn level_triangles(&self, level: usize) -> u32 {
        let l = &self.levels[level];
        self.clusters[l.first_cluster as usize..(l.first_cluster + l.cluster_count) as usize]
            .iter()
            .map(|c| c.index_count / 3)
            .sum()
    }

    /// Structural invariants. A DAG failing this must never be cached or drawn —
    /// callers treat `Err` as a build bug (assert in debug, discard + log in release).
    pub fn validate(&self, params: &ClusterBakeParams) -> Result<(), String> {
        let nv = self.positions.len();
        if self.normals.len() != nv {
            return Err(format!("normals {} != positions {}", self.normals.len(), nv));
        }
        for (i, &ix) in self.indices.iter().enumerate() {
            if ix as usize >= nv {
                return Err(format!("index[{i}]={ix} out of {nv} verts"));
            }
        }
        // Levels partition the cluster array contiguously from 0.
        let mut expect = 0u32;
        for (li, l) in self.levels.iter().enumerate() {
            if l.first_cluster != expect {
                return Err(format!("level {li} starts at {} expected {expect}", l.first_cluster));
            }
            if l.cluster_count == 0 {
                return Err(format!("level {li} empty"));
            }
            expect += l.cluster_count;
        }
        if expect as usize != self.clusters.len() {
            return Err(format!("levels cover {expect} of {} clusters", self.clusters.len()));
        }
        let max_ic = params.max_triangles * 3;
        for (ci, c) in self.clusters.iter().enumerate() {
            if c.index_count == 0 || c.index_count % 3 != 0 || c.index_count > max_ic {
                return Err(format!("cluster {ci} bad index_count {}", c.index_count));
            }
            let end = c.index_offset as usize + c.index_count as usize;
            if end > self.indices.len() {
                return Err(format!("cluster {ci} slice {end} > {}", self.indices.len()));
            }
            // The DAG-cut invariant: a cluster's own error must not exceed the
            // error of the coarser geometry that would replace it.
            if !(c.self_error <= c.parent_error) {
                return Err(format!(
                    "cluster {ci} self_error {} > parent_error {} (cut invariant)",
                    c.self_error, c.parent_error
                ));
            }
            if c.group != NO_GROUP {
                let g = self
                    .groups
                    .get(c.group as usize)
                    .ok_or_else(|| format!("cluster {ci} group {} oob", c.group))?;
                let gi = ci as u32;
                if gi < g.first_cluster || gi >= g.first_cluster + g.cluster_count {
                    return Err(format!("cluster {ci} not inside its group {} range", c.group));
                }
            }
            if c.child_group != NO_GROUP && self.groups.get(c.child_group as usize).is_none() {
                return Err(format!("cluster {ci} child_group {} oob", c.child_group));
            }
        }
        // Level 0 must be exact (source geometry): zero self_error.
        if let Some(l0) = self.levels.first() {
            for ci in l0.first_cluster..(l0.first_cluster + l0.cluster_count) {
                let c = &self.clusters[ci as usize];
                if c.self_error != 0.0 {
                    return Err(format!("L0 cluster {ci} self_error {} != 0", c.self_error));
                }
                if c.child_group != NO_GROUP {
                    return Err(format!("L0 cluster {ci} has child_group"));
                }
            }
        }
        // Roots must never be replaced: parent_error == +inf on the last level.
        if let Some(root) = self.levels.last() {
            for ci in root.first_cluster..(root.first_cluster + root.cluster_count) {
                if self.clusters[ci as usize].parent_error != f32::INFINITY {
                    return Err(format!("root cluster {ci} parent_error not INF"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built 2-level toy: 2 fine clusters (one group) → 1 root cluster.
    fn toy() -> (ClusterDag, ClusterBakeParams) {
        let params = ClusterBakeParams {
            max_vertices: 64,
            max_triangles: 124,
            group_size: 2,
            simplify_ratio: 0.5,
            root_cluster_target: 1,
        };
        let dag = ClusterDag {
            positions: vec![[0.0; 3]; 6],
            normals: vec![[0.0, 1.0, 0.0]; 6],
            // 2 tris + 2 tris (L0) + 1 tri (root)
            indices: vec![0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4, 0, 1, 5],
            clusters: vec![
                Cluster {
                    index_offset: 0,
                    index_count: 6,
                    bounds_center: [0.0; 3],
                    bounds_radius: 1.0,
                    cone_axis: [0.0, 1.0, 0.0],
                    cone_cutoff: 1.0,
                    self_error: 0.0,
                    parent_error: 0.1,
                    group: 0,
                    child_group: NO_GROUP,
                },
                Cluster {
                    index_offset: 6,
                    index_count: 6,
                    bounds_center: [0.0; 3],
                    bounds_radius: 1.0,
                    cone_axis: [0.0, 1.0, 0.0],
                    cone_cutoff: 1.0,
                    self_error: 0.0,
                    parent_error: 0.1,
                    group: 0,
                    child_group: NO_GROUP,
                },
                Cluster {
                    index_offset: 12,
                    index_count: 3,
                    bounds_center: [0.0; 3],
                    bounds_radius: 1.2,
                    cone_axis: [0.0, 1.0, 0.0],
                    cone_cutoff: 1.0,
                    self_error: 0.1,
                    parent_error: f32::INFINITY,
                    group: NO_GROUP,
                    child_group: 0,
                },
            ],
            groups: vec![ClusterGroup {
                first_cluster: 0,
                cluster_count: 2,
                bounds_center: [0.0; 3],
                bounds_radius: 1.2,
                merged_error: 0.1,
            }],
            levels: vec![
                LevelRange { first_cluster: 0, cluster_count: 2 },
                LevelRange { first_cluster: 2, cluster_count: 1 },
            ],
            source_hash: 42,
        };
        (dag, params)
    }

    #[test]
    fn toy_dag_validates() {
        let (dag, params) = toy();
        dag.validate(&params).expect("toy DAG must validate");
        assert_eq!(dag.level_triangles(0), 4);
        assert_eq!(dag.level_triangles(1), 1);
    }

    #[test]
    fn validate_catches_cut_invariant_violation() {
        let (mut dag, params) = toy();
        dag.clusters[0].parent_error = -1.0; // self_error 0 > parent -1
        assert!(dag.validate(&params).is_err());
    }

    #[test]
    fn validate_catches_level_gap() {
        let (mut dag, params) = toy();
        dag.levels[1].first_cluster = 3;
        assert!(dag.validate(&params).is_err());
    }

    #[test]
    fn validate_catches_oob_index() {
        let (mut dag, params) = toy();
        dag.indices[0] = 99;
        assert!(dag.validate(&params).is_err());
    }

    #[test]
    fn validate_catches_root_without_inf() {
        let (mut dag, params) = toy();
        dag.clusters[2].parent_error = 5.0;
        assert!(dag.validate(&params).is_err());
    }

    #[test]
    fn params_hash_stable_and_sensitive() {
        // FNV-1a inline — the stable-hasher discipline the cache key will use.
        struct Fnv(u64);
        impl std::hash::Hasher for Fnv {
            fn finish(&self) -> u64 {
                self.0
            }
            fn write(&mut self, bytes: &[u8]) {
                for &b in bytes {
                    self.0 = (self.0 ^ b as u64).wrapping_mul(0x100000001b3);
                }
            }
        }
        let p = ClusterBakeParams {
            max_vertices: 64,
            max_triangles: 124,
            group_size: 6,
            simplify_ratio: 0.5,
            root_cluster_target: 4,
        };
        let mut h1 = Fnv(0xcbf29ce484222325);
        p.hash_into(&mut h1);
        let mut h2 = Fnv(0xcbf29ce484222325);
        p.hash_into(&mut h2);
        assert_eq!(std::hash::Hasher::finish(&h1), std::hash::Hasher::finish(&h2));
        let mut h3 = Fnv(0xcbf29ce484222325);
        ClusterBakeParams { group_size: 7, ..p }.hash_into(&mut h3);
        assert_ne!(std::hash::Hasher::finish(&h1), std::hash::Hasher::finish(&h3));
    }
}
