//! Phase D3 — cluster DAG background bake (PHASE_D3_CLUSTER_DAG_PLAN.md commit 8).
//!
//! Mirrors the `body_volume_cache` shape: mpsc channel, debounce, spawn-time
//! content-stamp staleness, results drained in `poll_background_tasks` with a
//! per-frame cap. D3 only STORES the DAG (log + `dags` map) — D4 consumes it.
//!
//! Scope: poly bodies whose display mesh exceeds `CLUSTER_TRI_THRESHOLD`.
//! SDF DC meshes are EXCLUDED until D8 (they churn under bakes — clustering
//! them per edit would waste the whole budget).

use core_math::cluster_dag::{ClusterBakeParams, ClusterDag};
use core_math::scene_graph::MeshCacheId;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

/// Only meshes at least this dense get a DAG (small meshes draw fine as-is).
pub(crate) const CLUSTER_TRI_THRESHOLD: usize = 50_000;

/// Two-track geometry (owner 2026-08-05): heightfield stays on the **clipmap** track.
///
/// **Preferred:** `ClusterBakeState::block_cid` at spawn (property-like, name-independent).
/// **Legacy fallback:** demo name match only — do not rely on this for product bodies
/// (a user mesh named "Terrain" would be wrongly excluded; renaming the demo breaks it).
#[inline]
pub(crate) fn poly_name_blocks_cluster(name: &str) -> bool {
    name == "Terrain"
        || name.starts_with("Terrain.")
        || name.eq_ignore_ascii_case("Heightfield")
}
/// Clusters are heavyweight — debounce editing storms well above the 40ms
/// volume-bake constant.
const DEBOUNCE_MS: u128 = 500;
/// Frame Budget rule: at most one delivered DAG per frame.
const MAX_RESULTS_PER_FRAME: usize = 1;

pub(crate) struct ClusterBakeResult {
    pub cid: MeshCacheId,
    /// mesh_cache `pt_stamp` captured at spawn — stale results are dropped.
    pub stamp: u64,
    /// `None` if the build failed validation — still delivered so `in_flight` clears.
    pub dag: Option<Arc<ClusterDag>>,
    pub cold_ms: f32,
    pub from_cache: bool,
}

pub(crate) struct ClusterBakeState {
    tx: Sender<ClusterBakeResult>,
    rx: Receiver<ClusterBakeResult>,
    /// cid → pt_stamp of the in-flight job (also the debounce armed marker).
    in_flight: HashMap<MeshCacheId, u64>,
    /// cid → when we first saw the current stamp dirty (debounce clock).
    dirty_since: HashMap<MeshCacheId, (u64, Instant)>,
    /// Finished DAGs by mesh + the stamp they were built from.
    pub(crate) dags: HashMap<MeshCacheId, (u64, Arc<ClusterDag>)>,
    /// Explicit cluster-block list (clipmap / heightfield track). Prefer this over name match.
    pub(crate) blocked_cids: std::collections::HashSet<MeshCacheId>,
    pub(crate) params: ClusterBakeParams,
}

impl Default for ClusterBakeState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            in_flight: HashMap::new(),
            dirty_since: HashMap::new(),
            dags: HashMap::new(),
            blocked_cids: std::collections::HashSet::new(),
            params: ClusterBakeParams::default(),
        }
    }
}

/// Cache dir: `%LOCALAPPDATA%/Azra/cluster_cache/` (paths.rs precedent — never a
/// hardcoded user path, never inside `.fl4`).
pub(crate) fn cluster_cache_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Azra")
        .join("cluster_cache")
}

impl ClusterBakeState {
    /// Mark a mesh as clipmap-track only (never DAG bake / cluster draw).
    pub(crate) fn block_cid(&mut self, cid: MeshCacheId) {
        self.blocked_cids.insert(cid);
        self.dags.remove(&cid);
        self.dirty_since.remove(&cid);
        self.in_flight.remove(&cid);
    }

    #[inline]
    pub(crate) fn is_blocked(&self, cid: MeshCacheId) -> bool {
        self.blocked_cids.contains(&cid)
    }

    /// Consider one mesh: debounce, then spawn a background build (cache-first).
    /// `positions`/`indices` are snapshotted only when a job actually spawns.
    pub(crate) fn consider(
        &mut self,
        cid: MeshCacheId,
        stamp: u64,
        snapshot: impl FnOnce() -> Option<(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>)>,
    ) {
        if self.is_blocked(cid) {
            return;
        }
        // Stamp moved: drop the old DAG immediately so the pool/remap cannot keep
        // drawing pre-edit clusters while a new bake is debounced (WP3 / F-STALE).
        if self.dags.get(&cid).is_some_and(|(s, _)| *s != stamp) {
            self.dags.remove(&cid);
        }
        if self.dags.get(&cid).is_some_and(|(s, _)| *s == stamp) {
            self.dirty_since.remove(&cid);
            return; // up to date
        }
        // One exclusive job per cid (any stamp) — no concurrent multi-build thrash.
        // Still arm dirty_since so the new stamp is considered as soon as the job ends.
        if self.in_flight.contains_key(&cid) {
            let entry = self.dirty_since.entry(cid).or_insert((stamp, Instant::now()));
            if entry.0 != stamp {
                *entry = (stamp, Instant::now());
            }
            return;
        }
        let now = Instant::now();
        let entry = self.dirty_since.entry(cid).or_insert((stamp, now));
        if entry.0 != stamp {
            *entry = (stamp, now); // content moved again — restart the debounce
            return;
        }
        if now.duration_since(entry.1).as_millis() < DEBOUNCE_MS {
            return;
        }
        let Some((positions, normals, indices)) = snapshot() else { return };
        self.dirty_since.remove(&cid);
        self.in_flight.insert(cid, stamp);
        eprintln!(
            "[ClusterBake] spawn {cid:?} stamp={stamp} tris={}",
            indices.len() / 3
        );
        let tx = self.tx.clone();
        let params = self.params;
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let dir = cluster_cache_dir();
            let key = render::cluster_build::dag_cache_key(&positions, &indices, &params);
            let (dag_opt, from_cache) = match render::cluster_build::load_dag_cached(&dir, key) {
                Some(dag) => (Some(dag), true),
                None => {
                    let dag = render::cluster_build::build_cluster_dag(
                        &positions, &normals, &indices, &params, key,
                    );
                    if let Err(e) = dag.validate(&params) {
                        eprintln!("[ClusterBake] INVALID DAG for {cid:?}: {e} — discarded");
                        // Still notify so in_flight clears (never stuck forever).
                        let _ = tx.send(ClusterBakeResult {
                            cid,
                            stamp,
                            dag: None,
                            cold_ms: t0.elapsed().as_secs_f32() * 1000.0,
                            from_cache: false,
                        });
                        return;
                    }
                    if let Err(e) = render::cluster_build::write_dag_cached(&dir, key, &dag) {
                        eprintln!("[ClusterBake] cache write failed ({e}) — DAG still delivered");
                    }
                    (Some(dag), false)
                }
            };
            let _ = tx.send(ClusterBakeResult {
                cid,
                stamp,
                dag: dag_opt.map(Arc::new),
                cold_ms: t0.elapsed().as_secs_f32() * 1000.0,
                from_cache,
            });
        });
    }

    /// Drain finished results (capped). `current_stamp` maps cid → live stamp so
    /// stale results (mesh edited while building) are dropped, not stored.
    pub(crate) fn drain(&mut self, mut current_stamp: impl FnMut(MeshCacheId) -> Option<u64>) {
        for _ in 0..MAX_RESULTS_PER_FRAME {
            let Ok(r) = self.rx.try_recv() else { return };
            // Only clear in_flight if this result is for the job we think is running.
            if self.in_flight.get(&r.cid) == Some(&r.stamp) {
                self.in_flight.remove(&r.cid);
            }
            if current_stamp(r.cid) != Some(r.stamp) {
                eprintln!("[ClusterBake] {:?} stale (edited while building) — dropped", r.cid);
                continue;
            }
            let Some(dag) = r.dag else {
                eprintln!("[ClusterBake] {:?} build failed — no DAG stored", r.cid);
                continue;
            };
            eprintln!(
                "[ClusterBake] {:?}: levels={} clusters={} L0_tris={} {} in {:.0}ms",
                r.cid,
                dag.levels.len(),
                dag.clusters.len(),
                dag.level_triangles(0),
                if r.from_cache { "CACHE HIT" } else { "cold build" },
                r.cold_ms,
            );
            self.dags.insert(r.cid, (r.stamp, dag));
        }
    }

    /// Drop DAGs whose mesh is gone or whose stamp no longer matches live mesh content.
    /// `live` = set of (cid, stamp) still wanted this poll (or empty → prune all missing).
    pub(crate) fn retain_live(
        &mut self,
        mut live_stamp: impl FnMut(MeshCacheId) -> Option<u64>,
    ) -> bool {
        let before = self.dags.len();
        self.dags.retain(|cid, (stamp, _)| live_stamp(*cid) == Some(*stamp));
        self.dirty_since.retain(|cid, (stamp, _)| {
            live_stamp(*cid).map(|s| s == *stamp).unwrap_or(false)
        });
        before != self.dags.len()
    }
}
