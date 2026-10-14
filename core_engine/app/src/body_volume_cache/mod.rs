//! Per-body SDF volume caching — bake each body independently for DC mesh extraction.
//!
//! When a body's primitives change, only that body's grid is rebaked.
//! Each body gets its own cached grid; DC mesh extraction runs per-body.
//! This avoids re-evaluating all primitives when only one body changes.

use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc};
use std::time::Instant;
use glam::Vec3;
use core_math::primitives::BooleanOp;
use core_math::scene_graph::{SceneGraph, NodeId};
pub(crate) use graphics::gpu_volume::ResidentVolume;
pub(crate) use crate::bake_utils::*;
pub(crate) mod brick_allocator;

mod brick_classify;
mod duplication;
mod cascade;
mod undo;
mod resident_volume;
mod dirty;
pub(crate) mod bake_dispatch;
mod bake_readback;

/// Feature flag: when true, prim.position is body-local (relative to body.origin).
/// When false, prim.position is world-space (legacy behavior).
pub(super) use crate::BODY_LOCAL_PRIMS;

// ============================================================
// Dirty-region tracking for partial bake
// ============================================================

/// P1-#6b: per-body accumulation of pure-translation prim edits since the last dispatch
/// consume — see the `rigid_accum` field doc. `deltas` sums each prim's translation;
/// rigidity at consume ⟺ NOT poisoned AND every prim of the body is present with the SAME
/// summed delta.
#[derive(Default)]
pub(super) struct RigidAccum {
    /// prim_uid → summed body-local translation since the last consume.
    pub(super) deltas: HashMap<u32, Vec3>,
    /// A non-translation dirty mark arrived this burst — the field genuinely changed
    /// somewhere; rigidity is off the table until the accumulation is consumed. Sticky:
    /// a later translate note must NOT resurrect a clean claim over a poisoned burst.
    pub(super) poisoned: bool,
}

/// Body-local AABB marking a region that needs re-evaluation during bake.
/// Unlike `core_math::sdf_volume::DirtyRegion` (voxel indices for sculpt),
/// this uses body-local float coordinates matching the bake grid space.
#[derive(Clone, Copy, Debug)]
pub(super) struct BakeDirtyRegion {
    /// Body-local AABB min corner.
    pub(super) local_min: Vec3,
    /// Body-local AABB max corner.
    pub(super) local_max: Vec3,
}

impl BakeDirtyRegion {
    /// Check if two regions overlap or are adjacent (within `margin`).
    fn overlaps_or_adjacent(&self, other: &BakeDirtyRegion, margin: f32) -> bool {
        self.local_min.x - margin <= other.local_max.x
            && self.local_max.x + margin >= other.local_min.x
            && self.local_min.y - margin <= other.local_max.y
            && self.local_max.y + margin >= other.local_min.y
            && self.local_min.z - margin <= other.local_max.z
            && self.local_max.z + margin >= other.local_min.z
    }

    /// Merge another region into this one (union of AABBs).
    fn merge(&mut self, other: &BakeDirtyRegion) {
        self.local_min = self.local_min.min(other.local_min);
        self.local_max = self.local_max.max(other.local_max);
    }
}

/// Maximum number of separate dirty regions before merging all into one.
pub(super) const MAX_DIRTY_REGIONS: usize = 8;

// pad_grid_bounds, GRID_PAD_FRAC, GRID_PAD_MIN moved to bake_utils.rs

// ============================================================
// Display state — tells raster_render how to draw each body
// ============================================================

/// What the raster pipeline should do with a body this frame.
pub(crate) enum BodyDisplayState {
    /// Clean cache with current mesh — draw it.
    Raster,
    /// Baking/meshing in progress — draw the previous (stale) mesh.
    RasterOldCache,
    /// Rigid transform only — cached mesh is valid, just needs updated instance
    /// matrix from body.origin. No rebake required.
    TransformSafe,
    /// First-time body — no mesh yet, body INVISIBLE until first bake completes.
    /// No analytical show-through: the march pipeline does NOT render NeverBaked
    /// bodies as a visible surface. Hidden analytical evaluation (for bake,
    /// deferred queries) is unaffected.
    NeverBaked,
}

// ============================================================
// Per-body cache entry
// ============================================================

#[derive(Clone)]
pub(super) struct BodyCacheEntry {
    /// The cached SDF grid for this body (f32 per voxel). Arc so the RAM-cache snapshot, the chunked
    /// texture-upload snapshot, and a Shift+D duplicate share the allocation instead of each cloning it
    /// (~512MB at 512³). The grid is NEVER mutated in place — a rebake REPLACES it with a fresh Arc — so
    /// no copy-on-write ever fires (verified: zero `&mut grid` sites).
    pub(super) grid: Arc<Vec<f32>>,
    /// Hash of the primitives used to generate this grid.
    pub(super) source_hash: u64,
    /// Body-local min corner of the bake grid.
    pub(super) grid_min: Vec3,
    /// Body-local max corner of the bake grid.
    pub(super) grid_max: Vec3,
    /// Resolution this grid was baked at (may differ from cache.resolution during draft mode).
    pub(super) bake_resolution: u32,
    /// When `Some(src)`, this entry was cloned from `src` after a Shift+D duplicate.
    /// The grid data is valid but the next edit should trigger a real bake.
    /// Cleared by `invalidate_shared_cache()` / `mark_body_dirty()`.
    pub(super) shared_source: Option<NodeId>,
    /// S2.4 (SparseBricks): `Some(atlas_generation)` = this bake wrote the
    /// persistent `gpu.sdf_brick_atlas` POOL in place (mode 2) instead of a dense grid — `grid` is
    /// EMPTY on purpose. The value is `SdfBrickAtlasGpu.generation` at dispatch time; the DC-mesh
    /// sparse arm only trusts the atlas when body + res + generation all still match. This marker
    /// is what every consumer contract keys on (DC arm, heal/re-tighten skip, RAM-cache skip, the
    /// edit-mode re-bake guard). `None` on every dense/TextureOnly path.
    pub(super) brick_resident: Option<u64>,
    /// S2.5: CPU-retained copy of the FROZEN classifier index the last FULL SparseBricks bake
    /// uploaded to `gpu.sdf_brick_atlas`. S2.4 only uploaded the index GPU-side and dropped the
    /// CPU copy after dispatch; this is the retained twin, stored HERE next to the
    /// `brick_resident` marker so both share one lifecycle: set together by `record_sparse_bake`
    /// (completion — the same provenance moment as the marker), replaced together by the next
    /// full sparse bake, dropped together when any dense/TextureOnly bake replaces the entry.
    /// `Some` iff `brick_resident` is `Some`. Read by `trigger_bake`'s frozen-index partial path
    /// (S2.5): the dilated dirty-window brick check runs against THIS index — no re-classify per
    /// drag frame. Arc so entry clones (RAM-cache snapshots never happen for empty-grid entries;
    /// the dead duplicate path nulls it) never copy the ~1MB index.
    pub(super) sparse_frozen: Option<Arc<core_math::chunked_dc::SparseIndexOnly>>,
    /// P1-#6b (fixed): body-local translation the CURRENT resident mesh must be drawn at —
    /// nonzero ONLY between a rigid-translate consume and the next mesh dispatched from a bake
    /// at the entry's current bounds (which resets it). Accumulated EXPLICITLY by the rigid
    /// fast path — NEVER inferred from bounds deltas: bounds also move on escape-grow and
    /// settle-retighten where the geometry does NOT move, and the original derivation
    /// (`entry.grid_min − slot.meshed_grid_min`) shifted the drawn mesh by those bogus deltas
    /// (the owner's "sphere detaches from the gizmo at 128" screenshot — escape/retighten
    /// thrash at dense res left the wrong offset up for frames at a time).
    pub(super) mesh_rigid_offset: Vec3,
}

/// RAM cache entry — survives invalidate_for_undo(), validated by hash+resolution+bounds+epoch.
pub(super) struct RamCacheEntry {
    /// Arc (matches BodyCacheEntry.grid) so evict→RAM and RAM→restore move/clone the refcount, not the
    /// ~512MB buffer. Eviction shares the allocation with the still-resident body_grids entry.
    pub(super) grid: Arc<Vec<f32>>,
    pub(super) source_hash: u64,
    pub(super) grid_min: Vec3,
    pub(super) grid_max: Vec3,
    pub(super) bake_resolution: u32,
    pub(super) invalidation_epoch: u64,
    pub(super) inserted_at: std::time::Instant,
}

// ============================================================
// Background bake result
// ============================================================

pub(crate) struct BodyBakeResult {
    pub body_id: NodeId,
    pub grid: Vec<f32>,
    pub source_hash: u64,
    pub generation: u64,
    pub grid_min: Vec3,
    pub grid_max: Vec3,
    /// Resolution this grid was baked at.
    pub bake_resolution: u32,
    /// GATE 2 — the voxel window `[min, max)` this bake actually re-evaluated (full bakes carry
    /// the full `[0, res)` box). Acceptance (`poll`) folds it into `dirty_mesh_region` — the
    /// single accept-time producer contract.
    pub bake_window: ([u32; 3], [u32; 3]),
}

/// GATE 2 — the ONE dirty-mesh region: the accumulated (unioned) accepted-bake voxel window
/// `[min, max)` since the last consume. See `BodyVolumeCache::dirty_mesh_region`.
pub(crate) type DirtyMeshRegion = ([u32; 3], [u32; 3]);

// ============================================================
// BodyVolumeCache
// ============================================================

// ============================================================
// Level 4 — Progressive Resolution Cascade
// ============================================================

/// Tracks a multi-resolution cascade for a single body.
///
/// When a body needs a fresh bake (first creation, topology change) and has no
/// existing grid, we do a synchronous 64^3 bake for instant feedback, then
/// cascade through higher resolutions asynchronously.
pub(super) struct BakeResolutionCascade {
    /// Resolution targets for each cascade level (e.g., [64, 128, 256]).
    pub(super) target_resolutions: [u32; 3],
    /// Which levels have completed.
    pub(super) completed: [bool; 3],
    /// Index of the next level to dispatch (0 = 64, 1 = 128, 2 = 256).
    pub(super) current_level: usize,
    /// Generation counter at cascade start — stale if body re-dirtied.
    pub(super) generation: u64,
}

/// Manages per-body SDF volume caches for DC mesh extraction.
///
/// **Flow:**
/// 1. `mark_body_dirty(body_id)` — called when a body's primitives change
/// 2. `trigger_bake(scene_graph, res, bounds)` — spawns rayon jobs for dirty bodies
/// 3. `poll()` — receives completed body grids, dispatches DC extraction
pub(crate) struct BodyVolumeCache {
    /// Per-body cached SDF grids.
    pub(super) body_grids: HashMap<NodeId, BodyCacheEntry>,
    /// Bodies that need rebaking.
    pub(crate) dirty_bodies: HashSet<NodeId>,
    /// Channel for receiving completed body bake results.
    pub(super) bake_rx: mpsc::Receiver<BodyBakeResult>,
    pub(crate) bake_tx: mpsc::Sender<BodyBakeResult>,
    /// P1-#7 ASYNC FULL CLASSIFY: finished full-classify bake requests coming back from the
    /// background worker. `trigger_bake` spawns one worker per sparse FULL — the 38-45ms
    /// `classify_prims_to_sparse_index` was the last frame-thread hitch (select / duplicate /
    /// escalation / escape-full at 512³). The worker completes the request's `sparse` payload
    /// from the prim SNAPSHOT it carries and sends the WHOLE `PrimaryBakeRequest` here;
    /// `poll_async_classifies` (each frame, before the dispatch drain) validates generation +
    /// sparse regime and pushes it into `primary_bake_requests`. The `in_flight` entry inserted
    /// at spawn — exactly like a sync dispatch — keeps the trigger coalescer deferring the body
    /// for the whole flight, so the spawn-time generation stays current unless undo / body
    /// removal / invalidate_all touched it (exactly the events that make the snapshot stale).
    pub(super) classify_rx: mpsc::Receiver<PrimaryBakeRequest>,
    pub(super) classify_tx: mpsc::Sender<PrimaryBakeRequest>,
    /// Spawned-but-not-landed async classifies (test pump + telemetry). Counts BOTH regimes:
    /// CPU worker threads and queued/in-flight GPU classifies.
    pub(super) classify_pending: usize,
    /// STAGE-2 GPU CLASSIFY: capability flag mirrored from the app each poll (`gpu.is_some()`).
    /// When true, `spawn_async_classify` queues a [`QueuedGpuClassify`] here instead of spawning
    /// the CPU worker; `AzraApp::dispatch_pending_gpu_classifies` (poll_background_tasks — Frame
    /// Ordering Contract) drains it into a real `classify_bricks` compute dispatch. The finished
    /// request still lands through `classify_tx` → `poll_async_classifies` — ONE landing point
    /// for both regimes. Default false, so cache-only tests exercise the CPU worker unchanged.
    pub(crate) gpu_classify_ready: bool,
    /// Sparse FULL classifies waiting for their GPU dispatch (drained every poll).
    pub(crate) pending_gpu_classify: Vec<QueuedGpuClassify>,
    /// Per-body generation counter — stale results are discarded.
    pub(super) generations: HashMap<NodeId, u64>,
    /// In-flight jobs (body_id, hash) — prevents duplicate dispatches.
    pub(super) in_flight: HashSet<(NodeId, u64)>,
    /// LAB mesh-SDF operand generation — bumped on every operand load/clear and mixed
    /// into the bake source hash. Without it, trigger_bake's hash-skip drops the
    /// rebake for bodies whose prims didn't change (the operand is a bake INPUT that
    /// lives outside the prim data). See lab_mesh_sdf.rs.
    pub(crate) lab_operand_gen: u64,
    /// When each body's bake was dispatched — used for timeout detection.
    pub(super) bake_start_times: HashMap<NodeId, Instant>,
    /// Grid resolution (uniform, same for all bodies).
    pub(crate) resolution: u32,
    /// Base grid world-space half-extent (user-configured minimum).
    pub(super) bounds: f32,
    /// Per-body bake bounds, stored at dispatch time. Read when result arrives.
    pub(super) body_bounds: HashMap<NodeId, (Vec3, Vec3)>,
    /// Bodies where only a rigid transform changed (translate/rotate/scale of
    /// the whole body). The cached volume/mesh is still valid — only the
    /// instance transform (body.origin) needs updating. Cleared when the body
    /// is marked dirty for a real rebake.
    pub(super) transform_safe_bodies: HashSet<NodeId>,
    /// Number of bodies that had cache hits (not rebaked) in the last bake cycle.
    pub(crate) cache_hits: u32,
    /// Total bodies in the last bake cycle.
    pub(crate) cache_total: u32,
    /// GPU volume pool: LRU-managed multi-body resident volumes (Level 5).
    /// Switching between bodies is instant when the target is still in the pool.
    pub(crate) volume_pool: graphics::gpu_volume::VolumePool,
    /// The NodeId of the body that currently owns the "active" pool slot.
    pub(crate) resident_body_nid: Option<NodeId>,
    /// Pool slot index for the active body (valid when resident_body_nid is Some).
    pub(crate) resident_slot: Option<usize>,
    /// Set when the active body's grid data changed and needs GPU upload.
    pub(super) resident_upload_needed: bool,
    /// True iff `rv.buffer` holds the active body's CURRENT field — the precise gate the field-direct
    /// DC mesh needs. Distinct from `resident_upload_needed`, which also flags a pending *texture*
    /// upload (the path-A in-place bake leaves the buffer current but the texture stale → that flag
    /// stays true). Set true after any buffer write (`update_resident_volume` upload, or a path-A
    /// in-place bake via `note_resident_bake_complete`); set false on realloc (new garbage buffer) or
    /// when the active body's grid changes (accept/poll) before the buffer is re-written.
    pub(super) resident_buffer_valid: bool,
    /// Dedup: (source_hash, bake_resolution) of the last successful upload.
    /// Prevents redundant `upload_full` when bake results re-set the flag
    /// but the grid data hasn't actually changed.
    pub(super) last_upload_key: (u64, u32),
    /// The world-space bounds the resident buffer CONTENT was last actually written at.
    /// This is the ground truth for "what mapping is the buffer content authored under" —
    /// distinct from `rv.bake_bounds` (the *label*, which is overwritten every frame
    /// `resident_upload_needed` is set, even when the content upload dedup-skips). The
    /// upload dedup compares grid bounds against THIS, so a settle re-tighten / escape
    /// that moves the bounds but keeps the same geometry can NEVER dedup-skip the
    /// re-upload (the flat-slab root: content at old bounds, label at new bounds).
    /// Set only when content is actually written; sentinel = inverted (never matches).
    pub(super) last_upload_bounds: (Vec3, Vec3),
    /// Set when the resident volume texture was reallocated (new wgpu::Texture).
    /// Render loop checks this to invalidate the PT bind groups (binding 40).
    pub(crate) resident_volume_reallocated: bool,
    /// Pending dirty regions for partial bake (body-local space).
    /// When non-empty, the next CPU bake dispatches only over these regions.
    /// Cleared after dispatch. Falls back to full bake if empty or too many.
    pub(super) dirty_regions: HashMap<NodeId, Vec<BakeDirtyRegion>>,
    /// Timestamp of the last `mark_body_dirty` / `mark_prim_dirty` call.
    /// Used to debounce bake dispatch during rapid slider drags.
    /// Last time any body was marked dirty (Detail LO settle-only uses this).
    pub(crate) last_dirty_time: Option<Instant>,
    /// Timestamp of the last successfully received bake result.
    /// Used to suppress per-body timeouts while a batch is still making progress.
    pub(super) last_bake_result_time: Option<Instant>,
    /// Most recent successful bake finish: `(body, wall-ms)`. Terrain toast reads via
    /// [`Self::take_last_finished_bake`]. Overwritten each accept; optional consume.
    pub(crate) last_finished_bake: Option<(NodeId, f32)>,
    /// When set, bakes use this lower resolution for fast draft feedback.
    /// Cleared on slider release to trigger a full-resolution rebake.
    pub(super) draft_resolution: Option<u32>,
    /// Bodies that were baked at draft resolution and need full-res promotion.
    pub(super) draft_baked_bodies: HashSet<NodeId>,
    /// Pending primary GPU bake requests (populated by trigger_bake when no workers).
    /// Drained by AzraApp::dispatch_pending_primary_bakes() each frame.
    pub(crate) primary_bake_requests: Vec<PrimaryBakeRequest>,
    /// Per-body chunked meshes for incremental DC extraction (Level 3).
    /// When a bake result arrives with a known dirty region, only the affected
    /// chunks are re-extracted — clean chunks are preserved.
    pub(crate) body_chunked_meshes: HashMap<NodeId, core_math::chunked_dc::ChunkedMesh>,
    /// GATE 2 — THE single per-body dirty-mesh region (`DirtyMeshRegion`), the ONE channel every
    /// mesher reads for "what changed since my last consume". Replaces the former THREE parallel
    /// channels: `last_bake_dirty_range` (dispatch-time overwrite, dense DC + texture upload),
    /// `sparse_mesh_dirty_window` (accept-time union, tiled sparse mesher), and per-consumer reads
    /// of the Partial request plumbing.
    ///
    /// CONTRACT — one producer, accept-time only: every bake ACCEPTANCE (dense readback, no-copy
    /// path-A/lazy, CPU-worker `poll()`, worker-GPU, sparse partial landing) UNIONS its accepted
    /// voxel window `[min, max)` via `note_bake_accepted_window`; every accepted FULL bake (and
    /// sparse full landing) CLEARS via `note_full_bake_accepted`. Entry ABSENT = "no proven
    /// delta" = full re-mesh / full upload — the conservative default of EVERY consumer, so
    /// absence is always safe, never stale. Union (not replace): a skipped/deferred consumer
    /// never loses a delta. Consumers: the tiled sparse mesher TAKES it
    /// (`take_dirty_mesh_region`, gated on actually dispatching); the CPU chunked DC PEEKS it
    /// (dispatch_grid_dc_async — the texture upload still needs it afterwards); the resident
    /// texture upload consumes (removes) it after the upload lands (resident_volume.rs).
    /// Staleness guards (SdfTiledStamp, atlas generations, chunked-mesh presence) fold AROUND
    /// this one region — they gate whether an incremental consume is allowed, never where the
    /// region comes from.
    pub(crate) dirty_mesh_region: HashMap<NodeId, DirtyMeshRegion>,
    /// Active progressive resolution cascades (Level 4).
    /// Bodies with no existing grid get a synchronous 64^3 bake, then cascade
    /// through 128^3 and optionally 256^3 asynchronously.
    pub(super) cascades: HashMap<NodeId, BakeResolutionCascade>,
    /// RAM cache tier: grids demoted from body_grids during invalidate_for_undo().
    /// Keyed by NodeId. Validated by source_hash + bake_resolution + bounds + epoch.
    pub(super) ram_cache: HashMap<NodeId, RamCacheEntry>,
    pub(super) ram_cache_budget_bytes: usize,
    pub(super) ram_cache_used_bytes: usize,
    pub(super) ram_cache_enabled: bool,
    pub(super) invalidation_epoch: u64,
    // --- Phase 1/2: Sparse brick pool (Gate 0 → Phase 6) ---
    /// Shared brick pool for all bodies. Initialized with 256 MB budget.
    pub(crate) brick_pool: graphics::brick_pool::BrickPool,
    /// Per-body sparse brick volumes (indirection tables) — LEGACY, being replaced by BrickAllocator.
    pub(crate) body_brick_volumes: HashMap<NodeId, graphics::brick_pool::BodyBrickVolume>,
    /// Feature flag: enable sparse brick classification after each bake.
    pub(crate) use_brick_pool: bool,
    /// Feature flag: enable direct brick bake (Phase 4) — coarse classify + GPU fill.
    /// When true, bake resolution is reduced to coarse (dims_bricks×2) for classification,
    /// and brick_fill shader evaluates SDF at full voxel resolution per active brick.
    pub(crate) use_brick_bake: bool,
    /// New atlas-composing brick allocator (PLAN_BRICK_ATLAS.md).
    /// Tracks which 8³ regions contain surface per body. Pure CPU bookkeeping.
    pub(crate) brick_allocator: brick_allocator::BrickAllocator,
    /// Feature flag: enable adaptive resolution per body based on AABB extent.
    /// When true, each body's bake resolution scales with its size:
    /// small bodies get lower resolution, large bodies get higher.
    /// L0 cascade level is always 64³ regardless of this flag.
    pub(crate) use_adaptive_resolution: bool,
    /// PoC: **Detail Weighted LO** — source-of-detail budget, not size.
    ///
    /// When true:
    /// 1. Bake/extract at **oversample ×** the picker / adaptive resolution (capped), so soft
    ///    curves get denser dual-contour cells. Soft-feature floor may push higher still.
    /// 2. After DC, auto-run **Field** importance decimate (~`detail_lo_keep_ratio` of tris)
    ///    so flat regions melt first and curves stay — similar total tri budget to the
    ///    unboosted grid.
    ///
    /// Pure SDF + DC + mesh importance. No analytical rims. Viewport A/B only (not export).
    pub(crate) use_detail_weighted_lo: bool,
    /// **REMOVED Adaptive DC LO (B2)** — always false. Field kept so workspace layout stays stable.
    /// Path A only: oversample + Master quality fold (Preprocess / export).
    pub(crate) detail_lo_adaptive_dc: bool,
    /// Fraction of post-oversample triangles to keep when `use_detail_weighted_lo` is on.
    /// Default ~0.30: oversample × linear ≈ surface denser → budget after collapse.
    pub(crate) detail_lo_keep_ratio: f32,
    /// Extract oversample multiplier (1.25–3.0). Default 2.0. Cost-capped at high base res.
    /// Path A oversample multiplier (1.0–3.0).
    pub(crate) detail_lo_oversample: f32,
    /// Curve-protect strength (0.5–2.5). Scales Field |H|, crease, planar melt bias.
    pub(crate) detail_lo_strength: f32,
    /// When > 0, fold uses `target / raw_tri_count` as keep ratio (overrides keep_ratio slider).
    /// 0 = use `detail_lo_keep_ratio` only.
    pub(crate) detail_lo_target_tris: u32,
    /// Quality preset: 0=Balanced, 1=Soft walls (fillets), 2=Sharp (creases/silhouette).
    pub(crate) detail_lo_preset: u8,
    /// Recompute smooth normals after Field fold (cleaner LO shading).
    pub(crate) detail_lo_smooth_normals: bool,
    /// Debug: color DC topology wire by protect importance (blue=flat melt → red=keep).
    pub(crate) detail_lo_heat_wire: bool,
    /// **AZRA edge-shape remesh** (PoC toggle): SDF ridge + distance-to-edge density,
    /// keep main edges straight/curved, starve flats. Pure field — no analytical.
    pub(crate) detail_lo_edge_shape: bool,
    /// Min edge length scale vs auto base (soft-K / avg edge). 0.5–3.0. Only when edge-shape on.
    /// Higher → fewer short zippers on straight edges → lower tri count.
    pub(crate) detail_lo_min_edge_scale: f32,
    /// Curve angle budget (degrees). Keep chain samples that turn more than this on rounds.
    /// Typical 8–15°. Only when edge-shape on.
    pub(crate) detail_lo_curve_angle: f32,
    /// Target edge-length field ℓ(x) from SDF protect (short on edges, long on flats).
    pub(crate) detail_lo_edge_length_field: bool,
    /// After fold: flat-only Laplacian + SDF snap (kill fans, hold surface).
    pub(crate) detail_lo_flat_relax: bool,
    /// After fold: project crease verts to local analytical edges (plane–plane lines)
    /// and flats to planes; optional SDF surface pull. Cleans DC wobbly straight/curve lines.
    pub(crate) detail_lo_analytical_project: bool,
    /// During live edit (drag), skip oversample + fold; apply full Detail LO after settle.
    pub(crate) detail_lo_settle_only: bool,
    /// Runtime: currently in interactive draft (no oversample / no fold). Updated each frame.
    pub(crate) detail_lo_live_draft: bool,
    /// Runtime: hold-to-compare (Alt) — show raw DC extract without Field fold.
    pub(crate) detail_lo_hold_compare: bool,
    /// True after user **Preprocess LO base** — raw oversample extract is cached in
    /// `mesh_mod_base` so fold sliders scrub without rematerialize. Cleared when oversample
    /// / Detail LO toggles / invalidate.
    pub(crate) detail_lo_preprocessed: bool,
    /// Fast fold while scrubbing sliders (Master / fewer passes / no SDF map / no relax).
    /// Cleared after scrub settle → full quality fold.
    pub(crate) detail_lo_interactive: bool,
    /// Full quality fold pending after last slider scrub.
    pub(crate) detail_lo_quality_pending: bool,
    /// Time of last Detail LO slider scrub (for quality settle debounce).
    pub(crate) detail_lo_last_scrub: Option<std::time::Instant>,
    /// Per-body minimum resolution from soft-feature thickness (bevel / soft-K).
    /// Refreshed when Detail LO toggles or floors rebuild; empty when flag off.
    pub(crate) body_detail_floor_res: HashMap<NodeId, u32>,
    /// Diagnostic (measurement-only): when true, `trigger_bake` prints one `[BAKEPROF]`
    /// line per body per dispatch, classifying the bake path (partial / escape→full /
    /// full / first-cascade) with resolution + dirty-region count. Drives the
    /// live-prim-move rebake diagnosis (docs/planning/LIVE_MOVE_PERF.md). Runtime toggle
    /// in Developer Tweaks → Profiling so it can be flipped mid-session without a recompile.
    /// Default false — zero cost when off (single bool check per body).
    pub(crate) bake_prof: bool,
    /// Per-body target resolution (explicit Grid picker and/or adaptive sizing).
    /// Cleared when a body is removed or adaptive resolution is toggled off.
    pub(super) body_target_resolutions: HashMap<NodeId, u32>,
    /// Bodies whose resolution was set by the user (Grid picker / Ctrl+R pie).
    /// Adaptive size + camera LOD must NOT overwrite these — otherwise the picker
    /// appears broken (click 512 → adaptive immediately snaps back to 128).
    pub(super) body_res_user_locked: HashSet<NodeId>,
    /// Set when GPU-to-GPU copy_buffer_to_texture updated the resident 3D texture
    /// directly from the bake output buffer. Checked by `update_resident_volume()` to
    /// skip the CPU write_texture upload entirely (saves 5-45ms depending on resolution).
    pub(crate) gpu_texture_upload_done: bool,
    /// The body whose rv.buffer was actually written by the direct bake that set `gpu_texture_upload_done`.
    /// The flag is consumed (in `update_resident_volume`) ONLY when this matches the active body — so a
    /// non-active body's atlas-only bake can't latch the flag and flip the active body's STALE rv.buffer
    /// "valid" (the white-box ghost: an old field meshed field-direct under the new, larger bake bounds).
    pub(crate) gpu_texture_upload_body: Option<NodeId>,
    /// Last dirty regions dispatched, for debug overlay. Cleared after ~60 frames.
    /// Stored in body-local grid space: (body_nid, local_min, local_max, is_partial).
    pub(crate) debug_last_regions: Vec<(NodeId, Vec3, Vec3, bool)>,
    /// Frames since last region was stored. All regions share one age counter.
    pub(crate) debug_region_age: u32,
    /// When true, bake DISPATCH is suppressed (dirty regions still accumulate).
    /// Set at drag start, cleared at drag end/cancel. One bake fires on release
    /// covering all accumulated dirty regions.
    /// Safety nets: cleared in auto_commit_episodes, project_load, project_new.
    pub(crate) suppress_bake_dispatch: bool,
    /// Bodies that received a PARTIAL (interactive) bake since their last full bake.
    /// Partial bakes reuse the cached grid bounds + per-axis resolution to keep voxel
    /// mapping stable during a drag; an AABB-escape inflates those bounds while the
    /// voxel count stays fixed, so effective resolution on the body collapses (surface
    /// goes coarse / a growing fresh prim never appears) until a FULL bake re-tightens.
    /// `check_idle_retighten()` promotes these to one full bake once editing settles —
    /// the "final high-quality bake on release" pattern. Cleared per-body when a full
    /// bake dispatches, and wholesale on invalidate_all / undo invalidation.
    pub(super) pending_retighten: HashSet<NodeId>,
    /// Bodies whose CPU `body_grids` entry is STALE relative to rv.buffer (a bake completed via the
    /// no-copy field-direct/lazy path, skipping the GPU→CPU readback). Mirrors the voxel-sculpt
    /// `cpu_grid_stale` pattern: the GPU buffer is the live truth during editing; the CPU grid is a
    /// cache refreshed ONCE at settle. Drives two things: (a) `check_idle_retighten` must NOT skip a
    /// stale body as "already tight" (the settle bake is what heals the CPU cache), and (b) the heal
    /// bake is forced onto the readback path via `cpu_heal_queue`. Set by `note_cpu_grid_stale()`
    /// (no-copy completion), cleared by `accept_gpu_result` (readback landed).
    /// The RAM-cache restore is hash-guarded (source_hash of CURRENT prims), so a stale entry can
    /// never be restored for changed prims — staleness is bounded to the edit burst, healed at settle.
    pub(super) cpu_grid_stale: HashSet<NodeId>,
    /// One-shot heal marks set by `check_idle_retighten` for every body it queues: the NEXT dispatched
    /// bake for a marked body must take the READBACK path (not field-direct) so `body_grids` refreshes.
    /// Consumed (removed) at dispatch via `take_cpu_heal()`. This replaces the old `is_busy()` gate on
    /// `resident_full_direct` — interaction-state sniffing — with explicit per-bake intent: ALL edit
    /// bakes are GPU-only (zero CPU copy); exactly ONE readback fires per settle.
    pub(super) cpu_heal_queue: HashSet<NodeId>,
    /// [HealTrace] diagnostic window: set to now() whenever `check_idle_retighten` queues a heal.
    /// While the window is open (2s), every heal-relevant step prints — trigger skip/proceed,
    /// dispatch mode, readback acceptance, no-copy completion, dirty-mark provenance — so a
    /// settle→re-tighten loop shows its FULL chain in one owner log. Settle-scoped: never fires
    /// during drags (no settle queue → window closed).
    pub(super) heal_trace_start: Option<Instant>,
    /// Per-body timestamp of the last successful CPU-grid heal (readback / worker land).
    /// Used by `note_cpu_grid_stale` to suppress IDLE re-arm of `pending_retighten` for a short
    /// cooldown — live log (2026-07-21): no_copy_completion while IDLE → settle → 256³ heal
    /// primary=11s → no_copy again → infinite multi-second loop.
    pub(super) last_cpu_heal_at: HashMap<NodeId, Instant>,
    /// Raw (unpadded) tight bounds recorded at the LAST DISPATCH, per body — BOTH the partial
    /// and full-bake paths write it, so an entry is always at most one dispatch old. Sole
    /// functional reader: the escape policy's movement prediction (`compute_escape_grid_bounds`
    /// diffs the current tight bounds against this entry to tell which grid walls the body is
    /// approaching). NOT read by `check_idle_retighten` — that deliberately recomputes fresh
    /// bounds, because a mid-drag entry carries escape-inflated extent and comparing the grid
    /// against it hid the balloon (the flat-slab bug).
    pub(super) partial_tight_bounds: HashMap<NodeId, (glam::Vec3, glam::Vec3)>,
    /// P1-#6b rigid-translate fast path: per-body accumulation of PURE-TRANSLATION prim edits
    /// since the last consume. When, at dispatch, every prim of the body has accumulated the
    /// SAME delta (and the body has no body-anchored field features — mirror), the whole
    /// body-local field is rigid-translated: the grid bounds shift WITH the prims and nothing
    /// re-classifies, re-bakes, or re-meshes (the drawn mesh re-anchors via the EXPLICIT
    /// `BodyCacheEntry.mesh_rigid_offset`). This is the prim-level sibling of
    /// `transform_safe_bodies` (whole-BODY transform). Lifecycle: noted by the transform
    /// writer's pure-translate arm; POISONED (sticky until consume) by every other dirty mark —
    /// generic marks poison by DEFAULT so a new caller can never silently claim rigidity;
    /// consumed (removed) once per trigger dispatch.
    pub(super) rigid_accum: HashMap<NodeId, RigidAccum>,
    /// P1 scrolling-grid escape: bodies whose frozen sparse index was TRANSLATED this trigger
    /// (`try_sparse_scroll` — the brick-aligned window scroll that replaces the grow-full for
    /// fits-in-window escapes). The GPU atlas still holds the PRE-scroll index, so the partial
    /// arm MUST ship an `index_update` on this body's next request even when the window-allocate
    /// changed nothing (alloc/retag/reclaim all zero) — skipping the upload would leave the GPU
    /// decoding handles at their old (untranslated) brick coords = corrupt mesh. Consumed by the
    /// partial arm; the FULL arms clear it too (a full classify re-uploads the whole index).
    pub(super) sparse_scrolled: HashSet<NodeId>,
    /// When true (Object Mode = `!force_volume_only`), clamp the effective AND adaptive
    /// bake resolution to the GridBacked limit (≤256). Object Mode displays a DC TRIANGLE
    /// mesh, which is only produced by a GridBacked bake; a >256 bake is TextureOnly (empty
    /// CPU grid) → the DC cubic guard skips → no triangles → the body goes invisible and
    /// live prim edits stop landing (they re-bake a texture Object Mode never reads). This
    /// trap is reachable both via the 512/1024 picker AND via adaptive resolution scaling a
    /// large body past 256. Set each frame from the app render mode. SDF Edit Mode (false)
    /// keeps full 512/1024 for volume raymarch. Lift once GPU meshing (Phase G) can mesh >256.
    pub(super) gridbacked_only: bool,
    /// S2.5: running count of frozen-index partial ESCALATIONS (S2.5b: a dirty-window brick's
    /// frozen tag is incompatible with the moved field, or the dispatch-side atlas identity
    /// check failed) → full re-classify. Owner telemetry: measures the escalation rate during
    /// drags to decide whether incremental handle allocation is ever worth building (do NOT
    /// build it speculatively). Reported via `note_sparse_escalation` (visrec "sparse.escalate"
    /// per event + ONE debug/crate::perf_spam_on()-gated `[SparseEscalate]` stderr line per event).
    pub(super) sparse_escalate_count: u64,
    /// Gesture-coalesced escalation (2026-07-16 live attribution): per-body timestamp of the last
    /// OVERSIZED-WINDOW escalation that actually spawned a full classify. During a continuous
    /// gesture every landed escalated-full is immediately stale (the next tick's window is
    /// oversized again) — the owner's one scale gesture at 1024³ ran SIX full
    /// classify+bake+pool-rebuild+mesh pipelines back-to-back. Inside
    /// `ESCALATED_FULL_MIN_INTERVAL` of the stamp, an oversized-window tick DEFERS exactly like
    /// the in-flight coalesce (regions restored, dispatch state unwound, body re-armed) instead
    /// of spawning another full. The corruption-escalate arm is NEVER deferred. Same policy
    /// class as the 40ms bake debounce — bounding dispatch RATE, not correctness (the settle
    /// full at gesture end is authoritative).
    pub(super) sparse_escalate_last: std::collections::HashMap<NodeId, std::time::Instant>,
    /// Bodies that DEFERRED an oversized-window escalation and are still cooling. Lets
    /// `trigger_bake` skip them at the TOP of the loop — before collecting prims, hashing, and
    /// arming `pending_retighten` — instead of redoing that work every frame just to defer again
    /// at the escalation branch (owner log 2026-07-16: bursts of 30 `[RetightenArm]
    /// by=partial_dispatch while IDLE` = one per frame for the whole 300ms cooldown, each with a
    /// full trigger walk behind it). Set on defer, cleared when the cooldown expires or a real
    /// escalation fires. A pure fast-path memo: the deferral decision it replays was already
    /// computed from the real window on the first cooling frame.
    pub(super) sparse_escalate_deferred: std::collections::HashSet<NodeId>,
}

/// Request for a bake dispatch on the primary GPU (Level 2).
/// Created by trigger_bake() when no worker GPUs are available.
/// Dispatched by AzraApp using the primary device + command encoder.
pub(crate) struct PrimaryBakeRequest {
    pub body_nid: NodeId,
    pub prim_floats: Vec<f32>,
    /// Per-axis bake resolution [x, y, z].
    pub resolution: [u32; 3],
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub bake_min: [u32; 3],
    pub bake_max: [u32; 3],
    pub mirror_flags: u32,
    pub mirror_sign: u32,
    pub mirror_center: Vec3,
    pub bevel_radius: f32,
    pub source_hash: u64,
    /// Template hash for instancing — excludes prim_uid so clones match.
    pub template_hash: u64,
    pub generation: u64,
    pub base_grid: Option<Vec<f32>>,
    /// Separate clip curve polygon data buffer for GPU binding 4.
    pub clip_curve_floats: Vec<f32>,
    /// LAB poly→SDF: packed MeshVolume grids for GPU binding 5 (serialize_mesh_grids_for_bake).
    /// hot[86] of each MeshVolume prim indexes into this buffer. Min 1 float.
    pub mesh_grid_floats: Vec<f32>,
    /// T1 image displace: packed height maps for GPU binding 8 (serialize_height_maps_for_bake).
    /// hot[92] indexes into this buffer. Min 1 float.
    pub height_map_floats: Vec<f32>,
    /// S2.4/S2.5 (SparseBricks): `Some` = this is a PRIMARY bake at ≥512 in
    /// Object Mode dispatched as mode 2 — `Full` carries a fresh prim-classified brick index
    /// (S2.1) so the dispatch can upload it into `gpu.sdf_brick_atlas`; `Partial` reuses the
    /// FROZEN resident index (S2.5) over a dilated dirty window. Both rebind the bake to the pool
    /// (output_mode = 2). Classified at REQUEST time (dispatch-time-provenance rule: the prims
    /// used for the index are the exact prims serialized into `prim_floats`). `None` on every
    /// dense path — draft, coarse cascade, TextureOnly, dense partials, and the worker-GPU path.
    pub sparse: Option<SparseBakeRequest>,
    /// O1 (hierarchical classify): true when `coarse_cull_safe(local_prims)` held at request
    /// time — the GPU classify may then run its workgroup coarse-cull (one center eval per 4³
    /// brick super-cell, uniform verdict when the 1-Lipschitz bound proves it). False on every
    /// dense path and for scenes with modifiers/Seam/Extrude/MeshVolume (the fold is not
    /// provably 1-Lipschitz there — the classify runs the full 9-eval per brick as before).
    pub coarse_cull_ok: bool,
    /// P3 — when true, dispatch routes to CPU `BodyField` bake (`world_distance` full stack)
    /// instead of GPU `volume_bake.wgsl` (single non-mirror mod slot). Set at request time via
    /// `body_needs_cpu_full_stack_bake`.
    pub force_cpu_mod_stack: bool,
}

/// STAGE-2 GPU CLASSIFY: a sparse FULL classify queued for a `classify_bricks` compute dispatch
/// (`AzraApp::dispatch_pending_gpu_classifies`). `request` is complete except `sparse` (None
/// placeholder — the GPU verdict readback fills it); `local_prims` is the SAME dispatch-time prim
/// snapshot the CPU worker would classify, retained ONLY for the no-pipeline CPU fallback.
pub(crate) struct QueuedGpuClassify {
    pub(crate) request: PrimaryBakeRequest,
    pub(crate) local_prims: Vec<core_math::primitives::SdfPrimitive>,
    /// Scope-aware field snapshot (nested sub-body boolean trees) for the CPU fallback —
    /// same dispatch-time provenance as `local_prims`.
    pub(crate) field: core_math::field_eval::BodyField,
}

/// S2.4/S2.5: how a SparseBricks (mode-2) primary bake binds the persistent `SdfBrickAtlasGpu`.
pub(crate) enum SparseBakeRequest {
    /// S2.4 FULL sparse bake: a fresh prim-classified index travels with the request; dispatch
    /// uploads it (`upload_index`) and the bake fills the whole pool (full-box `bake_min/max`).
    /// Completion records it as the body's new frozen index (`BodyCacheEntry.sparse_frozen`).
    Full(core_math::chunked_dc::SparseIndexOnly),
    /// S2.5 PARTIAL sparse bake against the FROZEN index: NO full classify happened, NO index
    /// upload happens — the index already resident in the atlas is reused verbatim and the
    /// mode-2 bake updates the pool IN PLACE over the dilated dirty window (`bake_min/max`).
    /// `trigger_bake` already proved every brick overlapping that window is TAG-COMPATIBLE with
    /// the moved field (S2.5b `sparse_window_tags_compatible`: handles absorb any write;
    /// sentinels were mini-classified as still correct — writes into them are dropped by the
    /// mode-2 handle guard AND provably unneeded). `atlas_generation` is the entry's
    /// `brick_resident` marker — dispatch validates the live atlas (body + res + dims +
    /// generation) against it and escalates on any mismatch (generation counters over timing
    /// tricks).
    ///
    /// S2.5c `index_update`: `Some(updated)` when the window check found incompatible sentinel
    /// bricks and INCREMENTALLY ALLOCATED handles for them (plus the Surface apron ring) instead
    /// of escalating — a sentinel is pure compression, so a handled brick baked by this windowed
    /// dispatch is always correct. The dispatch must then `ensure_capacity` (content-PRESERVING
    /// pool growth) + re-upload this index; the completion installs it as the entry's new frozen
    /// index and stamps the (possibly generation-bumped) marker. `None` = plain frozen-index
    /// partial, index untouched. The updated Arc is NOT installed into `sparse_frozen` at trigger
    /// time — a superseded/dropped bake must not leave the CPU index claiming handles whose pool
    /// bricks were never baked.
    Partial {
        dims_bricks: [u32; 3],
        atlas_generation: u64,
        index_update: Option<std::sync::Arc<core_math::chunked_dc::SparseIndexOnly>>,
    },
}

impl BodyVolumeCache {
    /// Record bake wall time when a flight finishes successfully, then clear the start stamp.
    /// Timeout / cancel paths should call `bake_start_times.remove` directly (no toast).
    pub(crate) fn finish_bake_timing(&mut self, body_id: NodeId) {
        if let Some(start) = self.bake_start_times.remove(&body_id) {
            let ms = start.elapsed().as_secs_f64() as f32 * 1000.0;
            self.last_finished_bake = Some((body_id, ms));
        }
    }

    /// Consume the last finished bake timing (terrain toast).
    pub(crate) fn take_last_finished_bake(&mut self) -> Option<(NodeId, f32)> {
        self.last_finished_bake.take()
    }

    /// True while any bake flight for `body_id` is outstanding (terrain settle remesh waits).
    pub(crate) fn is_body_baking(&self, body_id: NodeId) -> bool {
        self.in_flight.iter().any(|(bid, _)| *bid == body_id)
    }

    /// True if `body_id` is in the dirty set (needs or is queued for bake).
    pub(crate) fn is_body_dirty(&self, body_id: NodeId) -> bool {
        self.dirty_bodies.contains(&body_id)
    }

    pub(crate) fn new(resolution: u32, bounds: f32) -> Self {
        let (tx, rx) = mpsc::channel();
        let (classify_tx, classify_rx) = mpsc::channel();
        Self {
            body_grids: HashMap::new(),
            dirty_bodies: HashSet::new(),
            bake_rx: rx,
            bake_tx: tx,
            classify_rx,
            classify_tx,
            classify_pending: 0,
            gpu_classify_ready: false,
            pending_gpu_classify: Vec::new(),
            generations: HashMap::new(),
            in_flight: HashSet::new(),
            lab_operand_gen: 0,
            bake_start_times: HashMap::new(),
            resolution,
            bounds,
            body_bounds: HashMap::new(),
            transform_safe_bodies: HashSet::new(),
            cache_hits: 0,
            cache_total: 0,
            volume_pool: graphics::gpu_volume::VolumePool::new(8), // default 8 slots; refined at first GPU access
            resident_body_nid: None,
            resident_slot: None,
            resident_upload_needed: false,
            resident_buffer_valid: false,
            last_upload_key: (0, 0),
            last_upload_bounds: (Vec3::splat(1e30), Vec3::splat(-1e30)), // inverted sentinel: never matches real bounds
            resident_volume_reallocated: false,
            dirty_regions: HashMap::new(),
            pending_retighten: HashSet::new(),
            cpu_grid_stale: HashSet::new(),
            cpu_heal_queue: HashSet::new(),
            last_cpu_heal_at: HashMap::new(),
            heal_trace_start: None,
            partial_tight_bounds: HashMap::new(),
            rigid_accum: HashMap::new(),
            sparse_scrolled: HashSet::new(),
            gridbacked_only: false,
            sparse_escalate_count: 0,
            sparse_escalate_last: std::collections::HashMap::new(),
            sparse_escalate_deferred: std::collections::HashSet::new(),
            last_dirty_time: None,
            last_bake_result_time: None,
            last_finished_bake: None,
            draft_resolution: None,
            draft_baked_bodies: HashSet::new(),
            primary_bake_requests: Vec::new(),
            body_chunked_meshes: HashMap::new(),
            dirty_mesh_region: HashMap::new(),
            cascades: HashMap::new(),
            ram_cache: HashMap::new(),
            ram_cache_budget_bytes: 2 * 1024 * 1024 * 1024, // 2 GB default
            ram_cache_used_bytes: 0,
            ram_cache_enabled: true,
            invalidation_epoch: 0,
            brick_pool: graphics::brick_pool::BrickPool::new(128 * 1024 * 1024), // 128 MB (max_storage_buffer_binding_size)
            body_brick_volumes: HashMap::new(),
            use_brick_pool: false, // off by default until Phase 2 validated
            use_brick_bake: false, // off by default until Phase 4 validated
            brick_allocator: brick_allocator::BrickAllocator::new(65536), // ~64K bricks budget
            // OFF by default: auto-scaling the bake resolution with body size makes the resolution churn
            // (visible LOD "feel") AND triggers the comb — a res change leaves the resident volume at a
            // different resolution than the bake for a frame, so the fast in-place path (A) can't fire and
            // it falls back to the stale-seed path (the bands). A FIXED resolution (the Grid picker value)
            // keeps res stable → path A always fires → no comb. Re-enable in Render properties if wanted.
            use_adaptive_resolution: false,
            use_detail_weighted_lo: false,
            detail_lo_adaptive_dc: false, // B2 stripped — never enable
            detail_lo_keep_ratio: 0.48, // denser LO: even cylinders / soft-K bands (was 0.30 melt)
            detail_lo_oversample: 2.0,
            detail_lo_strength: 1.45,
            detail_lo_target_tris: 0,
            detail_lo_preset: 1, // Soft walls — strongest match for the PoC use case
            detail_lo_smooth_normals: true,
            detail_lo_heat_wire: false,
            detail_lo_edge_shape: true, // AZRA edge-shape on by default for PoC A/B
            detail_lo_min_edge_scale: 1.0,
            detail_lo_curve_angle: 12.0,
            detail_lo_edge_length_field: true,
            detail_lo_flat_relax: true,
            detail_lo_analytical_project: true, // clean straight/curve features after fold
            detail_lo_settle_only: true,
            detail_lo_live_draft: false,
            detail_lo_hold_compare: false,
            detail_lo_preprocessed: false,
            detail_lo_interactive: false,
            detail_lo_quality_pending: false,
            detail_lo_last_scrub: None,
            body_detail_floor_res: HashMap::new(),
            bake_prof: false,
            body_target_resolutions: HashMap::new(),
            body_res_user_locked: HashSet::new(),
            gpu_texture_upload_done: false,
            gpu_texture_upload_body: None,
            debug_last_regions: Vec::new(),
            debug_region_age: 0,
            suppress_bake_dispatch: false,
        }
    }

    pub(crate) fn ram_cache_enabled(&self) -> bool { self.ram_cache_enabled }
    pub(crate) fn set_ram_cache_enabled(&mut self, enabled: bool) {
        self.ram_cache_enabled = enabled;
        if !enabled {
            self.ram_cache_used_bytes = 0;
            self.ram_cache.clear();
        }
    }

    pub(crate) fn set_ram_cache_budget_gb(&mut self, gb: f32) {
        self.ram_cache_budget_bytes = (gb * 1024.0 * 1024.0 * 1024.0) as usize;
    }
    pub(crate) fn ram_cache_budget_gb(&self) -> f32 {
        self.ram_cache_budget_bytes as f32 / (1024.0 * 1024.0 * 1024.0)
    }
    pub(crate) fn ram_cache_stats(&self) -> (usize, usize, usize) {
        (self.ram_cache.len(), self.ram_cache_used_bytes, self.ram_cache_budget_bytes)
    }
    /// Toggle brick pool on/off with proper lifecycle management.
    /// When toggled ON: marks all bodies dirty so they bake into bricks.
    /// When toggled OFF: deallocates all brick data immediately.
    pub(crate) fn set_brick_pool_enabled(&mut self, enabled: bool, sg: &SceneGraph) {
        let was_enabled = self.use_brick_pool;
        self.use_brick_pool = enabled;
        if enabled && !was_enabled {
            // Toggled ON: mark all bodies dirty so they bake and classify
            let body_nids = collect_all_body_nids(sg);
            for nid in body_nids {
                self.dirty_bodies.insert(nid);
            }
        } else if !enabled && was_enabled {
            // Toggled OFF: release all brick pool slots and indirection tables
            let brick_nids: Vec<NodeId> = self.body_brick_volumes.keys().copied().collect();
            for nid in brick_nids {
                self.brick_pool.deallocate_body(nid);
            }
            self.body_brick_volumes.clear();
        }
    }

    /// Toggle direct brick bake (Phase 4) on/off with proper lifecycle management.
    /// When toggled ON: marks all bodies dirty so they rebake via the brick path.
    /// When toggled OFF: deallocates all brick data (bricks came from the old path).
    pub(crate) fn set_brick_bake_enabled(&mut self, enabled: bool, sg: &SceneGraph) {
        let was_enabled = self.use_brick_bake;
        self.use_brick_bake = enabled;
        if enabled != was_enabled {
            // Either direction: mark all bodies dirty for rebake via the new path,
            // and clear existing brick data (built by the old path).
            let brick_nids: Vec<NodeId> = self.body_brick_volumes.keys().copied().collect();
            for nid in brick_nids {
                self.brick_pool.deallocate_body(nid);
            }
            self.body_brick_volumes.clear();
            let body_nids = collect_all_body_nids(sg);
            for nid in body_nids {
                self.dirty_bodies.insert(nid);
            }
        }
    }

    pub(crate) fn gpu_pool_stats(&self) -> (usize, usize) {
        let occupied = self.volume_pool.occupied_count();
        (occupied, self.volume_pool.max_slots())
    }

    /// Get the active body's resident volume (from pool), if available.
    pub(crate) fn resident_volume(&self) -> Option<&ResidentVolume> {
        let slot_idx = self.resident_slot?;
        let slot = self.volume_pool.get(slot_idx)?;
        slot.volume.as_ref()
    }

    /// True iff `rv.buffer` holds the active body's CURRENT field — the precise gate the field-direct
    /// DC mesh needs (see the field doc on `resident_buffer_valid`). Strictly safer than the
    /// bind-group-oriented `resident_volume_reallocated` flag, and (unlike that flag) it does not stay
    /// stuck across the draft→full resolution settle; and unlike `resident_upload_needed` it is NOT
    /// confused by a pending texture-only re-upload after a path-A in-place bake.
    pub(crate) fn resident_buffer_current(&self) -> bool {
        self.resident_buffer_valid
    }

    /// The world-space bounds `rv.buffer`'s CONTENT was actually last written at (see
    /// `last_upload_bounds`). The field-direct DC mesh maps the buffer using the *current* bake
    /// bounds; if the content was written at DIFFERENT bounds (a settle re-tighten / escape moved
    /// the mapping but the re-upload hasn't landed yet) the buffer is stale for this frame → the
    /// caller must NOT field-direct it (that is the flat slab). `resident_buffer_current()` /
    /// `rv.bake_bounds` both fail to catch this: the former is a bare bool, the latter is the label
    /// overwritten every frame. This returns the provenance the field-direct gate must check.
    pub(crate) fn resident_buffer_bounds(&self) -> (Vec3, Vec3) {
        self.last_upload_bounds
    }

    /// A bake for `body` just completed via the NO-COPY path (field-direct full / lazy partial —
    /// the GPU→CPU readback was skipped): the CPU `body_grids` entry is now stale relative to
    /// rv.buffer. Record the staleness AND re-arm the settle re-tighten (a full bake REMOVES the
    /// body from `pending_retighten` at dispatch — bake_dispatch.rs — so without this re-arm a
    /// no-copy full bake would cancel its own heal and the CPU cache would stay stale forever).
    /// S2.6: is the SparseBricks (mode-2) bake the active regime for a primary FULL bake at
    /// `res`? STRUCTURAL since S2.6 — sparse at ≥512 in Object Mode is THE regime, not a debug
    /// opt-in (the `sparse_bake_debug` toggle is gone; it only ever existed to A/B the dense path
    /// while 3A landed, and its OFF-default was review finding R1: the silent ~553ms dense 512
    /// bake every launch).
    /// Object Mode only (`gridbacked_only`): a sparse bake produces NO dense grid, NO
    /// resident-volume upload and NO atlas texture (mode 2 skips the textureStore) — nothing the
    /// SDF-Edit volume raymarch could display — so under `force_volume_only`
    /// (`gridbacked_only == false`) bodies stay on the dense path, and an existing brick-resident
    /// entry is treated as needing re-bake (see the hash-skip guard in `trigger_bake` and
    /// `needs_rebake_on_leave`).
    pub(super) fn sparse_bake_eligible(&self, res: u32) -> bool {
        self.gridbacked_only && res >= 512
    }

    /// The resolution CEILING for this cache's active regime — the single source every clamp,
    /// cubic-invariant check and TextureOnly boundary keys on (dense-vs-sparse cap split):
    /// - Sparse regime active (SparseBricks: Object Mode — structural since S2.6): 1024 — every
    ///   buffer on that path is surface-proportional (`max_sparse_resolution`), so 1024 runs
    ///   exactly like 512, just with a (res/8)³ brick index and ~4× the allocated bricks.
    /// - Otherwise (SDF Edit Mode / `force_volume_only`): 512. Dense GridBacked stays ≤512
    ///   FOREVER (a dense 1024³ field/cell_vert is 4GB — over the storage-binding limit), and the
    ///   legacy TextureOnly volume texture is also capped at 512
    ///   (`max_textureonly_resolution`) — a 1024 body must NEVER route TextureOnly.
    /// Note this deliberately admits 1024 only when `sparse_bake_eligible(1024)` would hold:
    /// both key on the SAME `gridbacked_only` regime, so a 1024 request can only exist while the
    /// sparse fork will take it (no dense 1024 fall-through by construction).
    pub(crate) fn max_mesh_resolution(&self) -> u32 {
        if self.gridbacked_only {
            crate::gpu_jobs::max_sparse_resolution()
        } else {
            crate::gpu_jobs::max_gridbacked_resolution()
        }
    }

    /// S2.4: the body's `brick_resident` marker (atlas generation at bake time), if its current
    /// cache entry came from a SparseBricks bake. `None` for dense/TextureOnly entries.
    pub(crate) fn brick_resident_gen(&self, body_id: NodeId) -> Option<u64> {
        self.body_grids.get(&body_id).and_then(|e| e.brick_resident)
    }

    /// S2.5: record ONE frozen-index partial ESCALATION (S2.5b: the dirty window holds a brick
    /// whose frozen tag is INCOMPATIBLE with the moved field, or the dispatch-side atlas
    /// identity check failed) — the edit falls back to a full re-classify + full sparse bake.
    /// ONE implementation for both call sites (trigger_bake window check + dispatch atlas
    /// guard) so the owner's escalation-rate telemetry can't drift: visrec "sparse.escalate"
    /// per event, plus ONE `[SparseEscalate]` stderr line PER event in debug builds.
    pub(crate) fn note_sparse_escalation(&mut self, body_id: NodeId, reason: &str) {
        self.sparse_escalate_count += 1;
        let count = self.sparse_escalate_count;
        crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "sparse.escalate", || format!(
            "body={:?} {} (total={})", body_id, reason, count));
        // S2.5b escalation visibility: ONE line PER escalation in debug builds. The previous
        // every-8 counter hid the owner's 100%-escalation rate (a debug run showed zero
        // escalation telemetry while every drag frame took the full classify+bake path).
        // Escalations are designed to be rare one-shot events (real surface crossings / sign
        // flips); if this line spams during a drag, that spam IS the diagnostic.
        if cfg!(debug_assertions) || crate::perf_spam_on() {
            eprintln!("[SparseEscalate] body={:?} reason={} (total={})", body_id, reason, count);
        }
    }

    pub(crate) fn note_cpu_grid_stale(&mut self, body: NodeId) {
        // S2.4 contract C3: brick-resident (SparseBricks) bodies must never enter cpu_grid_stale /
        // the heal machinery — there is NO dense CPU grid behind them to heal, and the heal
        // readback has no dense bake output to read. Keyed on the marker (not grid emptiness) so
        // TextureOnly behavior is unchanged. Unreachable today (sparse completions route through
        // record_sparse_bake, never the no-copy path) — structural guard, not a patch.
        if self.body_grids.get(&body).map_or(false, |e| e.brick_resident.is_some()) { return; }
        // SETTLE-LOOP GUARD (2026-07-16): an ENTRY-LESS body must never arm the heal/settle
        // machinery — there is no CPU grid to be stale relative to, and arming it feeds the
        // endless "[RetightenArm] by=no_copy_completion while IDLE" loop (a first bake that
        // completed no-copy). The dispatch gate in app_poll_bake.rs now prevents that class;
        // this guard makes any future reintroduction a visible one-line diagnostic instead of
        // an infinite loop.
        if !self.body_grids.contains_key(&body) {
            eprintln!("[RetightenArm] body={body:?} no-copy completion for an ENTRY-LESS body — refusing to arm settle (first bakes must take the readback path)");
            return;
        }
        // [visrec] staleness transition (Part B #8) — only log the actual flip.
        if self.cpu_grid_stale.insert(body) {
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                "body={:?} SET (no-copy bake completion — CPU grid stale vs rv.buffer; retighten re-armed)",
                body));
        }
        // LIVE FIX (2026-07-21): IDLE no-copy must NEVER re-arm pending_retighten.
        // Live console death spiral: settle heal → VolumeUpload 256³ → primary=11s →
        // idle no-copy → RetightenArm → settle again. Display meshes field-direct from
        // rv.buffer and does not need a fresh CPU grid every idle frame. Drag-time (not
        // idle) still arms so release settles once and heals.
        //
        // cpu_grid_stale stays set so export / CPU-DC / structural paths can still detect
        // staleness; they force their own bake/heal when needed.
        if self.retighten_idle() {
            return;
        }
        // Drag / edit active: arm settle so release runs one heal/re-tighten.
        let _ = self.pending_retighten.insert(body);
    }

    /// Record that a body's CPU grid was just refreshed (readback / worker land).
    #[inline]
    pub(crate) fn note_cpu_heal_complete(&mut self, body: NodeId) {
        self.last_cpu_heal_at.insert(body, Instant::now());
    }

    /// TEST-ONLY: is the settle/heal machinery fully quiescent? (No re-armed retighten, no
    /// stale CPU grid, no queued heal.) The `[RetightenArm] … while IDLE` settle-loop class
    /// is exactly a violation of this after a completed structural re-bake.
    #[cfg(test)]
    pub(crate) fn settle_quiescent(&self) -> bool {
        self.pending_retighten.is_empty()
            && self.cpu_grid_stale.is_empty()
            && self.cpu_heal_queue.is_empty()
    }

    /// Consume the one-shot heal mark for `body` (set by `check_idle_retighten` for every body it
    /// queues). When true, THIS bake must take the full READBACK path — not field-direct — so
    /// `body_grids` (the CPU cache: RAM-cache eviction, CPU-DC fallback, resample readers) is
    /// refreshed. Exactly one readback per settle; every other bake is GPU-only.
    pub(crate) fn take_cpu_heal(&mut self, body: NodeId) -> bool {
        self.cpu_heal_queue.remove(&body)
    }

    /// [HealTrace] window open? (2s after the last settle heal queue — see `heal_trace_start`.)
    pub(crate) fn heal_tracing(&self) -> bool {
        self.heal_trace_start.map_or(false, |t| t.elapsed().as_secs_f32() < 2.0)
    }

    /// Advance the volume pool frame counter. Call once per frame.
    pub(crate) fn advance_pool_frame(&mut self) {
        self.volume_pool.advance_frame();
    }

    /// Returns true if a body already has a cached voxel grid.
    pub(crate) fn has_cached_grid(&self, body_nid: NodeId) -> bool {
        self.body_grids.contains_key(&body_nid)
    }

    /// Returns true if a body has a cached grid baked at the target resolution.
    /// Used by warming to avoid re-baking bodies that already have valid grids.
    pub(crate) fn has_cached_grid_at_res(&self, body_nid: NodeId, target_res: u32) -> bool {
        self.body_grids.get(&body_nid)
            .map_or(false, |entry| entry.bake_resolution >= target_res)
    }

    /// Returns true if any body in the dirty set has never been baked.
    /// Used to allow first-bake through the dragging guard in process_dirty.
    pub(crate) fn has_unbaked_dirty_bodies(&self) -> bool {
        self.dirty_bodies.iter().any(|nid| !self.body_grids.contains_key(nid))
    }

    /// Move a body's cached grid data to the RAM cache.
    /// Called when the atlas evicts a body — preserves the grid so a future
    /// atlas re-allocation can skip the full bake.
    pub(crate) fn cache_body_to_ram(&mut self, body_nid: NodeId) {
        if !self.ram_cache_enabled { return; }
        let entry = match self.body_grids.get(&body_nid) {
            // S2.4 contract C4: never RAM-cache an EMPTY grid (SparseBricks / TextureOnly entry).
            // A restored empty entry would pass the trigger_bake RAM-hit checks (hash + res +
            // bounds) WITHOUT its brick_resident marker → the body skips its bake forever with
            // nothing to mesh (the stale-empty poison).
            Some(e) if !e.grid.is_empty() => e,
            _ => return, // no dense grid data to cache
        };
        let grid_bytes = entry.grid.len() * 4;
        // Enforce RAM cache budget — evict oldest entries if needed
        while self.ram_cache_used_bytes + grid_bytes > self.ram_cache_budget_bytes {
            let oldest = self.ram_cache.iter()
                .min_by_key(|(_, e)| e.inserted_at)
                .map(|(nid, _)| *nid);
            if let Some(evict_nid) = oldest {
                if let Some(removed) = self.ram_cache.remove(&evict_nid) {
                    self.ram_cache_used_bytes = self.ram_cache_used_bytes.saturating_sub(removed.grid.len() * 4);
                }
            } else {
                break; // nothing to evict
            }
        }
        let ram_entry = RamCacheEntry {
            grid: entry.grid.clone(),
            source_hash: entry.source_hash,
            grid_min: entry.grid_min,
            grid_max: entry.grid_max,
            bake_resolution: entry.bake_resolution,
            invalidation_epoch: self.invalidation_epoch,
            inserted_at: std::time::Instant::now(),
        };
        self.ram_cache_used_bytes += grid_bytes;
        self.ram_cache.insert(body_nid, ram_entry);
    }

    // Dirty marking methods moved to dirty.rs:
    // mark_body_dirty, mark_prim_dirty, mark_prim_dirty_at,
    // mark_prim_dirty_aabb, add_dirty_region, mark_transform_safe

    /// Mark all bodies as dirty (e.g., after load/new scene).
    pub(crate) fn invalidate_all(&mut self) {
        self.body_grids.clear();
        self.ram_cache.clear();
        self.ram_cache_used_bytes = 0;
        self.invalidation_epoch += 1;
        self.dirty_bodies.clear();
        self.in_flight.clear();
        self.bake_start_times.clear();
        self.generations.clear();
        self.body_bounds.clear();
        self.transform_safe_bodies.clear();
        self.dirty_regions.clear();
        self.rigid_accum.clear();
        self.sparse_scrolled.clear();
        self.pending_retighten.clear();
        if !self.cpu_grid_stale.is_empty() {
            // [visrec] staleness transition (Part B #8) — wholesale clear on structural reset.
            crate::vis_recorder::visrec(crate::vis_recorder::vis_frame(), "stale.cpu", || format!(
                "CLEARED ALL (invalidate_all): {:?}", self.cpu_grid_stale));
        }
        self.cpu_grid_stale.clear();
        self.cpu_heal_queue.clear();
        self.brick_allocator.deallocate_all();
        // Drop all pool slots — scene has changed entirely
        self.volume_pool = graphics::gpu_volume::VolumePool::new(self.volume_pool.max_slots());
        self.resident_body_nid = None;
        self.resident_slot = None;
        self.resident_upload_needed = false;
        self.last_upload_key = (0, 0);
        self.last_upload_bounds = (Vec3::splat(1e30), Vec3::splat(-1e30));
        // Reset debounce / draft state
        self.last_dirty_time = None;
        self.draft_resolution = None;
        self.draft_baked_bodies.clear();
        // Cancel all progressive resolution cascades
        self.cascades.clear();
        // Clear per-body adaptive resolutions + user Grid locks (recomputed on next bake)
        self.body_target_resolutions.clear();
        self.body_res_user_locked.clear();
        // Clear stale chunked meshes, dirty ranges, and pending bake requests
        self.body_chunked_meshes.clear();
        self.dirty_mesh_region.clear();
        self.primary_bake_requests.clear();
        // Release all brick pool slots and indirection tables
        let brick_nids: Vec<NodeId> = self.body_brick_volumes.keys().copied().collect();
        for nid in brick_nids {
            self.brick_pool.deallocate_body(nid);
        }
        self.body_brick_volumes.clear();
    }

    /// Pre-synchronize resolution/bounds without triggering invalidation.
    /// Called during project load AFTER invalidate_all() to lock in the loaded
    /// resolution/bounds, preventing set_params() from re-invalidating + bulk-
    /// marking all bodies dirty when trigger_body_vol_bake() runs later.
    pub(crate) fn pre_sync_params(&mut self, resolution: u32, bounds: f32) {
        self.resolution = resolution;
        self.bounds = bounds;
    }

    // ============================================================
    // Adaptive Resolution per Body
    // ============================================================

    /// Reference extent for the "medium body" baseline.
    /// A body with this max AABB axis extent gets exactly `self.resolution`.
    /// 2.0 = a default cube (~2 units padded) bakes at 128³ (base resolution).
    /// Bodies up to ~8 units reach 256³ (4x scale clamp). Small bodies (<1 unit)
    /// get 64-96³ (0.5x scale clamp). Balances quality vs performance.
    const ADAPTIVE_REFERENCE_EXTENT: f32 = 2.0;

    /// Compute the target bake resolution for a body based on its AABB extent.
    ///
    /// Small bodies (< reference) get lower resolution (saves memory).
    /// Large bodies (> reference) get higher resolution (needed for quality).
    /// Result is rounded to nearest multiple of 32 for GPU alignment.
    /// L0 cascade level (64³) is unaffected — this controls L1 target only.
    /// Update per-body LOD resolutions based on camera distance.
    /// Bodies far from camera or small on screen get lower resolution.
    /// Called once per frame before trigger_bake.
    pub(crate) fn update_lod_resolutions(
        &mut self,
        sg: &core_math::scene_graph::SceneGraph,
        cam_pos: Vec3,
        cam_fov_scale: f32,
        viewport_height: f32,
    ) {
        if !self.use_adaptive_resolution { return; }
        let base_res = self.resolution;

        for (body_nid, _) in sg.sdf_bodies() {
            // Resolution is a property of the TOP-LEVEL body only. A sub-body (group) bakes as part
            // of its top-level ancestor's ONE recursive grid at the ANCESTOR's resolution (the bake
            // loop skips sub-bodies — bake_dispatch.rs:257), so assigning a sub-body its own adaptive
            // LOD resolution here is dead (never read by the composed bake) AND harmful — it churns
            // `dirty_bodies` every camera move for a body that never bakes, and shows a meaningless,
            // parent-mismatched number in the resolution picker. Skip sub-bodies (CLAUDE.md: the active
            // analytical unit is the whole top-level subtree).
            if sg.is_sub_body(body_nid) { continue; }
            // Get body world-space AABB
            let world_bounds = sg.body_world_transform(body_nid)
                .and_then(|bt| {
                    sg.get(body_nid).and_then(|n| {
                        if let core_math::scene_graph::NodePayload::SdfBody {
                            origin, mirror_flags, mirror_center, bevel_radius, ..
                        } = &n.payload {
                            // RECURSIVE prim set — NOT the body's OWN `primitives` (CR-class:
                            // "build sites still use OWN prims"). A top-level body whose prims all
                            // moved into a nested sub-body (the outliner Group gesture) has an
                            // EMPTY own list; bailing on it fell through to the degenerate
                            // (ZERO,ONE) fallback box below → wrong (tiny) screen fraction → the
                            // LOD dropped a 1024 sparse body below 512 → sparse regime destroyed →
                            // dense re-bake churn ping-ponging against trigger_bake's full-bake
                            // adaptive recompute (which uses the REAL recursive grid bounds) — the
                            // owner's "group into sub-body → surface lost + [RetightenArm] spam"
                            // loop. Use the SAME recursive collection the bake + settle re-tighten
                            // paths use (bake_utils::collect_body_prims_recursive), so LOD and bake
                            // can never disagree about what the body contains.
                            let prims = crate::bake_utils::collect_body_prims_recursive(sg, body_nid);
                            if prims.is_empty() { return None; }
                            // F9: use the SAME mirror/bevel-aware bounds as the bake path (bake_dispatch),
                            // not hardcoded zeros. A mirrored body's AABB must cover the mirrored half and a
                            // beveled body's must include the bevel skin (cf. the Body Mirror AABB rule) —
                            // otherwise the LOD screen-fraction is computed from an under-sized box and picks
                            // the wrong resolution tier (too coarse for the on-screen size). Mirror center is
                            // offset to body-local exactly as the bake does, since prims are body-local.
                            let local_mirror_center = if BODY_LOCAL_PRIMS {
                                *mirror_center
                            } else {
                                *mirror_center - *origin
                            };
                            let bounds = crate::bake_utils::compute_body_bake_bounds(
                                &prims, *mirror_flags, local_mirror_center, *bevel_radius);
                            Some(bt.local_aabb_to_world(bounds.0, bounds.1))
                        } else { None }
                    })
                });
            // No resolvable bounds (truly empty subtree / non-body): there is nothing to bake,
            // so assigning an LOD tier is meaningless — and the old (ZERO,ONE) degenerate
            // fallback box fed a bogus screen fraction into the tier logic (dirty churn for a
            // body the bake loop skips anyway). Skip instead of guessing.
            let Some((bmin, bmax)) = world_bounds else { continue; };

            let center = (bmin + bmax) * 0.5;
            let radius = (bmax - bmin).length() * 0.5;
            let dist = (cam_pos - center).length().max(0.01);

            // Screen fraction: how much of the viewport height this body covers
            let screen_fraction = radius / (dist * cam_fov_scale);

            // LOD tiers with hysteresis (use current res to determine threshold)
            let current_res = self.body_target_resolutions.get(&body_nid).copied()
                .unwrap_or(base_res);
            let lod_res = if screen_fraction > 0.15 {
                base_res           // full resolution
            } else if screen_fraction > 0.05 {
                (base_res / 2).max(32)  // half resolution
            } else if screen_fraction > 0.01 {
                (base_res / 4).max(32)  // quarter resolution
            } else {
                32                 // minimum
            };

            // Never fight a user Grid-picker lock (same rule as bake-dispatch adaptive).
            if self.body_res_user_locked.contains(&body_nid) {
                continue;
            }
            // Hysteresis: only change resolution if the new tier differs from current
            // by more than one step. Prevents flapping during slow orbit.
            let tier_changed = (lod_res as i32 - current_res as i32).unsigned_abs() > 16;
            if tier_changed {
                let old = self.body_target_resolutions.insert(body_nid, lod_res);
                if old != Some(lod_res) {
                    // Resolution changed — mark body dirty for rebake at new resolution
                    self.dirty_bodies.insert(body_nid);
                }
            }
        }
    }

    fn compute_resolution_for_body(&self, aabb_min: Vec3, aabb_max: Vec3) -> u32 {
        let extent = aabb_max - aabb_min;
        let max_axis = extent.x.max(extent.y).max(extent.z).max(0.001);
        let scale = (max_axis / Self::ADAPTIVE_REFERENCE_EXTENT).clamp(0.5, 4.0);
        let raw = (self.resolution as f32 * scale) as u32;
        // Round to nearest multiple of 32. Cap depends on the active regime:
        // Object Mode: max_mesh_resolution() — dense GridBacked ≤512, or 1024 when the sparse
        // brick pipeline is active (surface-proportional buffers, no dense allocation).
        // SDF Edit Mode: TextureOnly volume texture, capped at 512 (max_textureonly_resolution).
        let max_res = if self.gridbacked_only {
            self.max_mesh_resolution()
        } else {
            crate::gpu_jobs::max_textureonly_resolution()
        };
        let rounded = ((raw + 16) / 32 * 32).clamp(32, max_res);
        rounded
    }

    /// Get the effective bake resolution for a body. When adaptive resolution
    /// is enabled and a per-body resolution has been computed, returns that.
    /// Otherwise returns the global `self.resolution`.
    /// Public for mesh-mod display stamps (must re-fold Decimate when res changes).
    pub(crate) fn effective_resolution(&self, body_nid: NodeId) -> u32 {
        // Per-body resolution override (explicit Grid picker OR adaptive sizing). Falls back to the
        // global default when a body has no override. `body_target_resolutions` is only populated by
        // adaptive sizing or `set_body_resolution`, and is cleared when adaptive is turned off — so a
        // plain (no-adaptive, no-explicit) scene still uses the global resolution exactly as before.
        // This is what makes "change resolution on one body" affect ONLY that body.
        let base = self.body_target_resolutions.get(&body_nid).copied()
            .unwrap_or(self.resolution);
        // Object Mode renders a DC triangle mesh: dense GridBacked ≤512, or up to 1024 when
        // the sparse brick pipeline is active (max_mesh_resolution — the dense-vs-sparse cap
        // split). Clamp here so neither the picker nor adaptive scaling can push an
        // Object-Mode body into a TextureOnly bake (empty CPU grid → no triangles → invisible
        // body + dead live edits). SDF Edit Mode clamps to the TextureOnly ceiling (512 — a
        // 1024 body must never route the legacy dense volume texture).
        let base = if self.gridbacked_only {
            base.min(self.max_mesh_resolution())
        } else {
            base.min(crate::gpu_jobs::max_textureonly_resolution())
        };
        // During interaction, use draft resolution for faster feedback — DENSE bodies only.
        //
        // S2.5e, enforced STRUCTURALLY here (the single clamp policy — every consumer reads
        // this function): a sparse-eligible body NEVER drafts. Draft is a dense-path speed
        // trick; halving a ≥512 sparse body drops it below the sparse threshold → dense bake
        // → `brick_resident` destroyed → regime_ok=false → full re-classify churn. The old
        // code applied the draft FIRST, so trigger_bake's per-body `sparse_body` guard (and
        // the `all_dirty_sparse` check) saw the already-drafted value and could never protect
        // the body — the owner's 1024→512→256→1024 re-bake loop on mere selection / work-mode
        // changes. Eligibility must be decided on the UNDRAFTED base.
        // Path-A draft: half global res during drag (dense bodies only).
        let _undrafted_base = base;
        let base = match self.draft_resolution {
            Some(draft) if !self.sparse_bake_eligible(base) => draft.min(base),
            _ => base,
        };
        // Detail Weighted LO (PoC) — Path A only: oversample + Master quality fold.
        // Adaptive DC LO (B2) was stripped — never dual-res / never fold-skip.
        if self.use_detail_weighted_lo {
            let max_res = if self.gridbacked_only {
                self.max_mesh_resolution()
            } else {
                crate::gpu_jobs::max_textureonly_resolution()
            };
            // Hard mesh-extract cap for Detail LO — 384³+ freezes UI.
            let detail_cap = max_res.min(256);

            // ── Path A: uniform oversample ──
            let mut mult = self.detail_lo_oversample.clamp(1.0, 3.0);
            // Cost cap: high base grids already resolve soft features better.
            if base >= 256 {
                mult = mult.min(1.0); // no oversample above 256 base
            } else if base >= 192 {
                mult = mult.min(1.25);
            } else if base >= 128 {
                mult = mult.min(1.75);
            } else if base >= 64 {
                mult = mult.min(2.25);
            }
            let mut boosted = ((base as f32 * mult).round() as u32).max(base).min(detail_cap);
            if let Some(&floor) = self.body_detail_floor_res.get(&body_nid) {
                // Soft-K floor may not blow past detail_cap (was causing 256→384 stalls).
                boosted = boosted.max(floor.min(detail_cap)).min(detail_cap);
            }
            // Round to 32 for GPU alignment.
            return ((boosted + 16) / 32 * 32).clamp(32, detail_cap);
        }
        base
    }

    /// B2 stripped — always false (call sites keep compiling).
    #[inline]
    pub(crate) fn detail_lo_is_adaptive_dc(&self) -> bool {
        false
    }

    /// True while Detail LO should skip oversample + Field fold (live draft).
    pub(crate) fn detail_lo_is_drafting(&self) -> bool {
        self.use_detail_weighted_lo && self.detail_lo_live_draft
    }

    /// True while Alt hold-compare shows raw extract (no Field fold).
    pub(crate) fn detail_lo_is_compare(&self) -> bool {
        self.use_detail_weighted_lo && self.detail_lo_hold_compare
    }

    /// Compute per-axis resolution from the body's AABB aspect ratio and a
    /// scalar target resolution `n`. The longest AABB axis gets the full `n`;
    /// shorter axes are proportionally smaller (minimum 8 voxels).
    ///
    /// Example: a flat slab with extents (4.0, 0.5, 4.0) and n=256 gets
    /// [256, 32, 256] instead of [256, 256, 256] — saving ~87.5% of voxels.
    pub(crate) fn compute_asymmetric_resolution(n: u32, aabb_min: Vec3, aabb_max: Vec3) -> [u32; 3] {
        let extent = aabb_max - aabb_min;
        let ex = extent.x.abs().max(1e-6);
        let ey = extent.y.abs().max(1e-6);
        let ez = extent.z.abs().max(1e-6);
        let max_extent = ex.max(ey).max(ez);
        let inv = 1.0 / max_extent;
        let rx = ((n as f32 * ex * inv).round() as u32).clamp(8, n);
        let ry = ((n as f32 * ey * inv).round() as u32).clamp(8, n);
        let rz = ((n as f32 * ez * inv).round() as u32).clamp(8, n);
        [rx, ry, rz]
    }

    /// The active body's explicit/adaptive per-body resolution override, if any (for the UI picker
    /// to show which button is highlighted). None = the body uses the global default.
    pub(crate) fn body_resolution(&self, body_nid: NodeId) -> Option<u32> {
        self.body_target_resolutions.get(&body_nid).copied()
    }

    /// Set ONE body's resolution (the Grid picker applied to the active body). Marks ONLY this body
    /// dirty — other bodies keep their own resolution. This is the per-body multi-resolution path.
    ///
    /// Locks the body against adaptive/LOD overwrites until adaptive is toggled or the body is
    /// deleted — owner 2026-07-29: "I cannot change the resolution of sdf body".
    pub(crate) fn set_body_resolution(&mut self, body_nid: NodeId, res: u32) {
        self.body_res_user_locked.insert(body_nid);
        if self.body_target_resolutions.get(&body_nid).copied() == Some(res) {
            // Still re-dirty: user may be re-applying after a failed/stale bake at this res.
            self.dirty_bodies.insert(body_nid);
            self.cancel_cascade(body_nid);
            self.last_dirty_time = None;
            return;
        }
        self.body_target_resolutions.insert(body_nid, res);
        self.dirty_bodies.insert(body_nid);
        self.cancel_cascade(body_nid);
        self.last_dirty_time = None; // bypass debounce — a deliberate one-shot resolution change
    }

    /// Stream / LOD policy resolution — does **not** set user-lock so the Grid picker
    /// and terrain near/far rings can co-exist. No-ops if the body is user-locked or
    /// already at `res` (avoids dirty spam when the camera stays in the same ring).
    pub(crate) fn set_body_resolution_policy(&mut self, body_nid: NodeId, res: u32) {
        if self.body_res_user_locked.contains(&body_nid) {
            return;
        }
        if self.body_target_resolutions.get(&body_nid).copied() == Some(res) {
            return;
        }
        self.body_target_resolutions.insert(body_nid, res);
        self.dirty_bodies.insert(body_nid);
        self.cancel_cascade(body_nid);
        self.last_dirty_time = None;
    }

    /// Recompute adaptive resolution for a single body and mark it dirty
    /// if the resolution changed. Does NOT invalidate other bodies.
    /// Returns true if the body's resolution changed.
    pub(crate) fn update_body_adaptive_resolution(
        &mut self, body_nid: NodeId, aabb_min: Vec3, aabb_max: Vec3,
    ) -> bool {
        if !self.use_adaptive_resolution { return false; }
        let new_res = self.compute_resolution_for_body(aabb_min, aabb_max);
        let old_res = self.body_target_resolutions.get(&body_nid).copied();
        if old_res == Some(new_res) { return false; }
        self.body_target_resolutions.insert(body_nid, new_res);
        // Only dirty the body if it has been baked before at a different resolution.
        // First-bake bodies are already dirty and will pick up the new resolution.
        if let Some(entry) = self.body_grids.get(&body_nid) {
            if entry.bake_resolution != new_res {
                self.dirty_bodies.insert(body_nid);
                self.cancel_cascade(body_nid);
                self.last_dirty_time = None; // bypass debounce for resolution change
                return true;
            }
        }
        false
    }

    /// Toggle adaptive resolution on/off. When toggled on, all bodies are
    /// recalculated on next bake. When toggled off, per-body resolutions are
    /// cleared and all bodies use the global resolution.
    pub(crate) fn set_adaptive_resolution(&mut self, enabled: bool, sg: &SceneGraph) {
        if self.use_adaptive_resolution == enabled { return; }
        self.use_adaptive_resolution = enabled;
        // Manual locks only make sense while adaptive can fight the picker.
        self.body_res_user_locked.clear();
        if !enabled {
            self.body_target_resolutions.clear();
        }
        // Re-dirty all bodies so they rebake at the new resolution scheme
        self.mark_all_bodies_dirty(sg);
    }

    /// Toggle Detail Weighted LO (PoC). Rebakes all bodies so extract res + display fold refresh.
    pub(crate) fn set_detail_weighted_lo(&mut self, enabled: bool, sg: &SceneGraph) {
        if self.use_detail_weighted_lo == enabled {
            return;
        }
        self.use_detail_weighted_lo = enabled;
        self.detail_lo_preprocessed = false;
        if enabled {
            self.refresh_detail_lo_feature_floors(sg);
        } else {
            self.body_detail_floor_res.clear();
            self.detail_lo_adaptive_dc = false;
        }
        self.last_dirty_time = None; // bypass debounce — resolution scheme changed
        self.mark_all_bodies_dirty(sg);
    }

    /// B2 stripped — no-op (always off).
    pub(crate) fn set_detail_lo_adaptive_dc(&mut self, _enabled: bool, _sg: &SceneGraph) -> bool {
        self.detail_lo_adaptive_dc = false;
        false
    }

    /// Rebuild per-body min-res floors from soft-feature thickness (bevel / soft-K).
    /// Goal: ≥ ~3 DC cells across the soft band so LO extract keeps the curve.
    pub(crate) fn refresh_detail_lo_feature_floors(&mut self, sg: &SceneGraph) {
        self.body_detail_floor_res.clear();
        if !self.use_detail_weighted_lo {
            return;
        }
        let max_res = if self.gridbacked_only {
            self.max_mesh_resolution()
        } else {
            crate::gpu_jobs::max_textureonly_resolution()
        };
        // How many cells across a soft fillet before it collapses under DC.
        let cells_across: f32 = 3.25;
        for (body_nid, _) in sg.sdf_bodies() {
            if sg.is_sub_body(body_nid) {
                continue;
            }
            let feat = crate::bake_utils::body_soft_feature_size(sg, body_nid);
            if feat < 1e-4 {
                continue;
            }
            let prims = crate::bake_utils::collect_body_prims_recursive(sg, body_nid);
            if prims.is_empty() {
                continue;
            }
            let (mirror_flags, local_mirror_center, bevel_radius) =
                match sg.get(body_nid).map(|n| &n.payload) {
                    Some(core_math::scene_graph::NodePayload::SdfBody {
                        mirror_flags,
                        mirror_center,
                        bevel_radius,
                        origin,
                        ..
                    }) => {
                        let local_mc = if BODY_LOCAL_PRIMS {
                            *mirror_center
                        } else {
                            *mirror_center - *origin
                        };
                        (*mirror_flags, local_mc, *bevel_radius)
                    }
                    _ => continue,
                };
            let (bmin, bmax) = crate::bake_utils::compute_body_bake_bounds(
                &prims,
                mirror_flags,
                local_mirror_center,
                bevel_radius,
            );
            let extent = (bmax - bmin).max_element().max(1e-4);
            // res so cell_size = extent/res ≤ feat / cells_across
            let need = (extent * cells_across / feat).ceil() as u32;
            let floor = ((need + 16) / 32 * 32).clamp(32, max_res);
            self.body_detail_floor_res.insert(body_nid, floor);
        }
    }

    /// Keep-ratio is fold-only (post-DC). Returns true if the value changed.
    /// Caller must `refresh_detail_lo_displays()` — do **not** rebake (field unchanged).
    pub(crate) fn set_detail_lo_keep_ratio(&mut self, ratio: f32) -> bool {
        let r = ratio.clamp(0.08, 0.9);
        if (self.detail_lo_keep_ratio - r).abs() < 0.005 {
            return false;
        }
        self.detail_lo_keep_ratio = r;
        true
    }

    /// Oversample changes extract resolution → must rebake.
    pub(crate) fn set_detail_lo_oversample(&mut self, mult: f32, sg: &SceneGraph) -> bool {
        let m = mult.clamp(1.25, 3.0);
        if (self.detail_lo_oversample - m).abs() < 0.02 {
            return false;
        }
        self.detail_lo_oversample = m;
        self.detail_lo_preprocessed = false; // extract res changes — need Preprocess again
        self.refresh_detail_lo_feature_floors(sg);
        self.last_dirty_time = None;
        self.mark_all_bodies_dirty(sg);
        true
    }

    /// Curve-protect strength is fold-only. Returns true if changed.
    /// Caller must `refresh_detail_lo_displays()`.
    pub(crate) fn set_detail_lo_strength(&mut self, strength: f32) -> bool {
        let s = strength.clamp(0.5, 2.5);
        if (self.detail_lo_strength - s).abs() < 0.02 {
            return false;
        }
        self.detail_lo_strength = s;
        true
    }

    /// Target triangle budget (0 = off, use keep-ratio). Fold-only.
    pub(crate) fn set_detail_lo_target_tris(&mut self, target: u32) -> bool {
        let t = if target < 64 { 0 } else { target.min(2_000_000) };
        if self.detail_lo_target_tris == t {
            return false;
        }
        self.detail_lo_target_tris = t;
        true
    }

    pub(crate) fn set_detail_lo_settle_only(&mut self, enabled: bool) -> bool {
        if self.detail_lo_settle_only == enabled {
            return false;
        }
        self.detail_lo_settle_only = enabled;
        if !enabled {
            self.detail_lo_live_draft = false;
        }
        true
    }

    pub(crate) fn set_detail_lo_preset(&mut self, preset: u8) -> bool {
        let p = preset.min(2);
        if self.detail_lo_preset == p {
            return false;
        }
        self.detail_lo_preset = p;
        // Apply preset defaults to strength / planar bias knobs the fold reads.
        match p {
            0 => {
                // Balanced
                self.detail_lo_strength = 1.25;
                self.detail_lo_keep_ratio = 0.45;
            }
            1 => {
                // Soft walls / fillets — denser keep for even blend bands
                self.detail_lo_strength = 1.55;
                self.detail_lo_keep_ratio = 0.50;
            }
            _ => {
                // Sharp / silhouette
                self.detail_lo_strength = 1.75;
                self.detail_lo_keep_ratio = 0.40;
            }
        }
        true
    }

    pub(crate) fn set_detail_lo_smooth_normals(&mut self, enabled: bool) -> bool {
        if self.detail_lo_smooth_normals == enabled {
            return false;
        }
        self.detail_lo_smooth_normals = enabled;
        true
    }

    pub(crate) fn set_detail_lo_heat_wire(&mut self, enabled: bool) -> bool {
        if self.detail_lo_heat_wire == enabled {
            return false;
        }
        self.detail_lo_heat_wire = enabled;
        true
    }

    pub(crate) fn set_detail_lo_edge_shape(&mut self, enabled: bool) -> bool {
        if self.detail_lo_edge_shape == enabled {
            return false;
        }
        self.detail_lo_edge_shape = enabled;
        true
    }

    pub(crate) fn set_detail_lo_min_edge_scale(&mut self, scale: f32) -> bool {
        let s = scale.clamp(0.35, 3.5);
        if (self.detail_lo_min_edge_scale - s).abs() < 0.02 {
            return false;
        }
        self.detail_lo_min_edge_scale = s;
        true
    }

    pub(crate) fn set_detail_lo_curve_angle(&mut self, deg: f32) -> bool {
        let d = deg.clamp(4.0, 24.0);
        if (self.detail_lo_curve_angle - d).abs() < 0.25 {
            return false;
        }
        self.detail_lo_curve_angle = d;
        true
    }

    pub(crate) fn set_detail_lo_edge_length_field(&mut self, enabled: bool) -> bool {
        if self.detail_lo_edge_length_field == enabled {
            return false;
        }
        self.detail_lo_edge_length_field = enabled;
        true
    }

    pub(crate) fn set_detail_lo_flat_relax(&mut self, enabled: bool) -> bool {
        if self.detail_lo_flat_relax == enabled {
            return false;
        }
        self.detail_lo_flat_relax = enabled;
        true
    }

    pub(crate) fn set_detail_lo_analytical_project(&mut self, enabled: bool) -> bool {
        if self.detail_lo_analytical_project == enabled {
            return false;
        }
        self.detail_lo_analytical_project = enabled;
        true
    }

    /// Rough triangle budget for a uniform extract at `base_res` (for Match-uniform button).
    /// Surface ~ O(res²); constant tuned for typical DC box/soft bodies.
    pub(crate) fn estimate_uniform_tri_budget(base_res: u32) -> u32 {
        let r = base_res.max(16) as f32;
        // ~2.2 * res² tris is a workable default for closed organic/soft SDF surfaces.
        (2.2 * r * r).round().clamp(200.0, 200_000.0) as u32
    }

    /// Update resolution/bounds (invalidates all caches).
    /// Re-dirties all bodies after invalidation so they rebake at the new resolution.
    pub(crate) fn set_params(&mut self, resolution: u32, bounds: f32, sg: &SceneGraph) {
        if self.resolution != resolution || (self.bounds - bounds).abs() > 0.001 {
            self.resolution = resolution;
            self.bounds = bounds;
            self.invalidate_all();
            // Re-populate dirty_bodies so trigger_bake() rebakes everything
            self.mark_all_bodies_dirty(sg);
        }
    }


    // Bake dispatch methods moved to bake_dispatch.rs:
    // DEBOUNCE_MS, DRAFT_PROMOTE_MS, set_draft_mode, is_draft_mode,
    // has_pending_dirty, check_draft_promotion, trigger_bake

    // Bake readback methods moved to bake_readback.rs:
    // poll, accept_gpu_result, clear_in_flight, clear_all_in_flight

    /// Get a body's cached grid data and bounds.
    /// Returns `(grid_slice, nverts, grid_min, grid_max, source_hash)`.
    /// `nverts` reflects the actual bake resolution (may be draft during slider drag).
    pub(crate) fn body_grid(&self, body_id: NodeId) -> Option<(&[f32], u32, Vec3, Vec3, u64)> {
        self.body_grids.get(&body_id).map(|entry| {
            (entry.grid.as_slice(), entry.bake_resolution, entry.grid_min, entry.grid_max, entry.source_hash)
        })
    }

    /// PT snapshot source (consolidation fix): Some((source_hash, res, grid_min, grid_max)) for
    /// a BRICK-RESIDENT body - the class that has no CPU mesh and therefore needs a GPU-side
    /// snapshot for the path tracer's BLAS. None for dense bodies (their meshes land in
    /// mesh_cache through the normal readback path already).
    pub(crate) fn pt_sparse_snapshot_src(&self, body_id: NodeId) -> Option<(u64, u32, Vec3, Vec3)> {
        let entry = self.body_grids.get(&body_id)?;
        entry.brick_resident?;
        Some((entry.source_hash, entry.bake_resolution, entry.grid_min, entry.grid_max))
    }

    /// PT snapshot source for a FIELD-DIRECT (dense ≤256, non-brick-resident) body — the twin of
    /// `pt_sparse_snapshot_src`. Needed because with the resident GPU mesh ON (default),
    /// `dispatch_grid_dc_async` meshes into the GPU display slot and RETURNS — the CPU mesh_cache
    /// that PT builds its BLAS from is never updated, so prim edits froze in PT (owner repro
    /// 2026-07-16 at 128³). The pump reads this hash and, when it settles, copies the DISPLAY
    /// slot's mesh back (no extra dispatch — the slot already holds the current mesh).
    pub(crate) fn pt_field_snapshot_src(&self, body_id: NodeId) -> Option<(u64, u32, Vec3, Vec3)> {
        let entry = self.body_grids.get(&body_id)?;
        if entry.brick_resident.is_some() { return None; }
        Some((entry.source_hash, entry.bake_resolution, entry.grid_min, entry.grid_max))
    }

    /// Whether any background jobs are pending.
    pub(crate) fn has_pending(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// DIAGNOSTIC (AZRA_AUTOREPRO probes): the frozen sparse-index brick TAG at a body-local
    /// position, for a brick-resident (SparseBricks) body — `BRICK_TAG_EXTERIOR`, `_INTERIOR`,
    /// or a pool handle. `None` when the body has no frozen sparse index or the point is
    /// outside the entry's grid bounds. Read-only; probe use only.
    pub(crate) fn sparse_frozen_tag_at(&self, body_id: NodeId, p: Vec3) -> Option<u32> {
        let entry = self.body_grids.get(&body_id)?;
        let frozen = entry.sparse_frozen.as_ref()?;
        let ext = entry.grid_max - entry.grid_min;
        let t = (p - entry.grid_min) / ext.max(Vec3::splat(1e-9));
        if t.min_element() < 0.0 || t.max_element() > 1.0 { return None; }
        let res = entry.bake_resolution.max(1);
        let bd = crate::body_volume_cache::bake_dispatch::SPARSE_BRICK_DIM;
        let v = (t * (res - 1) as f32).round();
        let [dx, dy, dz] = frozen.dims_bricks;
        let bx = ((v.x as u32) / bd).min(dx.saturating_sub(1));
        let by = ((v.y as u32) / bd).min(dy.saturating_sub(1));
        let bz = ((v.z as u32) / bd).min(dz.saturating_sub(1));
        frozen.index.get((bx + by * dx + bz * dx * dy) as usize).copied()
    }

    /// The body's CURRENT cached grid bounds (entry provenance — moves on rigid shifts and
    /// scrolls).
    pub(crate) fn entry_bounds(&self, body_id: NodeId) -> Option<(Vec3, Vec3)> {
        self.body_grids.get(&body_id).map(|e| (e.grid_min, e.grid_max))
    }

    /// P1-#6b (fixed): the EXPLICIT body-local translation the resident mesh must draw at —
    /// see `BodyCacheEntry.mesh_rigid_offset`. Read every frame by the G-buffer + picking
    /// instance builds; zero for every body outside an un-meshed rigid shift.
    pub(crate) fn mesh_rigid_offset(&self, body_id: NodeId) -> Vec3 {
        self.body_grids.get(&body_id).map_or(Vec3::ZERO, |e| e.mesh_rigid_offset)
    }

    /// Reset the rigid draw offset IFF the mesh just dispatched came from a bake at the
    /// entry's CURRENT bounds (`meshed_min`): that mesh is anchored where the entry lives, so
    /// no re-anchor remains. A mesh from an OLDER bake (a rigid shift raced in after the bake
    /// completed) keeps the offset — its content is still pre-shift.
    pub(crate) fn clear_mesh_rigid_offset_if_current(&mut self, body_id: NodeId, meshed_min: Vec3) {
        if let Some(e) = self.body_grids.get_mut(&body_id) {
            if (e.grid_min - meshed_min).length_squared() < 1e-10 {
                e.mesh_rigid_offset = Vec3::ZERO;
            }
        }
    }

    /// Translate cached mesh/grid bounds for a body by `offset` (body-local).
    /// Called when body.origin shifts so the stale DC mesh and grid bounds
    /// stay consistent with the new prim local positions.
    pub(crate) fn translate_body_cache(&mut self, body_id: NodeId, offset: Vec3) {
        if let Some(chunked) = self.body_chunked_meshes.get_mut(&body_id) {
            chunked.translate_all_vertices(offset);
        }
        if let Some((ref mut bmin, ref mut bmax)) = self.body_bounds.get_mut(&body_id) {
            *bmin += offset;
            *bmax += offset;
        }
        if let Some(entry) = self.body_grids.get_mut(&body_id) {
            entry.grid_min += offset;
            entry.grid_max += offset;
        }
    }

    /// Drop the dense CPU field grid for a **cold** terrain tile (non-resident).
    ///
    /// Phase-1/2 terrain is dense ≤128³ (L3) — there is **no** sparse brick atlas on this
    /// path. The real memory cost of a visited tile is `body_grids` (~8.4 MB at 128³).
    /// Hiding the node alone does not free that. This method **drops** the grid (does not
    /// park it in `ram_cache`, which would keep the leak).
    ///
    /// **Survives (draw path):** `mesh_cache` / `mesh_cache_id` on the SdfBody — DC output is
    /// a separate slot. Cold tiles stay hidden (`SceneNode.visible = false`); when they re-enter
    /// the resident set the last mesh can still draw until a fresh bake lands.
    ///
    /// **Cleared with the grid** so consumers that key on `body_grids` cannot act on a hole:
    /// - `body_chunked_meshes` / `dirty_mesh_region` — dense DC intermediate
    /// - `dirty_regions` / `dirty_bodies` — no partial/full bake while cold
    /// - `pending_retighten` / `cpu_grid_stale` / `cpu_heal_queue` — settle/heal skip missing grids
    /// - in-flight bookkeeping + **generation bump** — a late bake must not re-insert 8.4 MB
    /// - resident volume pointer if this body was the sole resident
    ///
    /// **Kept:** `body_bounds` / `body_target_resolutions` (next bake needs them).
    ///
    /// Returns `true` if a `body_grids` entry was present and removed.
    pub(crate) fn release_cold_tile_dense_grid(&mut self, body_id: NodeId) -> bool {
        let released = self.body_grids.remove(&body_id).is_some();
        if !released {
            // Still scrub soft state — e.g. stale after a prior partial clear.
        }

        // Dense DC intermediate + mesh dirty union for this body.
        self.body_chunked_meshes.remove(&body_id);
        self.dirty_mesh_region.remove(&body_id);
        self.dirty_regions.remove(&body_id);

        // Do not queue rebakes for a hidden tile (re-entry marks dirty explicitly).
        self.dirty_bodies.remove(&body_id);
        self.transform_safe_bodies.remove(&body_id);

        // Settle / heal paths read body_grids — drop membership so they skip this nid.
        self.pending_retighten.remove(&body_id);
        self.cpu_grid_stale.remove(&body_id);
        self.cpu_heal_queue.remove(&body_id);
        self.last_cpu_heal_at.remove(&body_id);
        self.cascades.remove(&body_id);
        self.draft_baked_bodies.remove(&body_id);
        self.rigid_accum.remove(&body_id);
        self.sparse_scrolled.remove(&body_id);

        // Cancel bookkeeping for any in-flight bake; bump generation so a late
        // accept_gpu_result / poll landing is rejected (would re-fill body_grids).
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.bake_start_times.remove(&body_id);
        *self.generations.entry(body_id).or_insert(0) += 1;

        // If this body owned the single resident volume texture, drop the pointer so
        // PATH-A / upload code does not assume a CPU grid still backs it.
        if self.resident_body_nid == Some(body_id) {
            self.resident_body_nid = None;
            self.resident_slot = None;
            self.resident_buffer_valid = false;
            self.resident_upload_needed = false;
        }

        released
    }

    /// True when `body_grids` holds a non-empty dense field for this body.
    pub(crate) fn has_dense_grid(&self, body_id: NodeId) -> bool {
        self.body_grids
            .get(&body_id)
            .map_or(false, |e| !e.grid.is_empty())
    }

    /// Terrain HUD: how many of `nids` are dirty / have a mesh dirty region.
    pub(crate) fn terrain_dirty_counts(
        &self,
        nids: impl Iterator<Item = NodeId>,
    ) -> (u32, u32) {
        let mut dirty_bodies = 0u32;
        let mut dirty_regions = 0u32;
        for nid in nids {
            if self.dirty_bodies.contains(&nid) {
                dirty_bodies += 1;
            }
            if self.dirty_mesh_region.contains_key(&nid) {
                dirty_regions += 1;
            }
        }
        (dirty_bodies, dirty_regions)
    }

    /// Remove a body from the cache (e.g., body deleted).
    pub(crate) fn remove_body(&mut self, body_id: NodeId) {
        self.body_grids.remove(&body_id);
        self.generations.remove(&body_id);
        self.dirty_bodies.remove(&body_id);
        self.in_flight.retain(|(bid, _)| *bid != body_id);
        self.bake_start_times.remove(&body_id);
        self.body_bounds.remove(&body_id);
        self.partial_tight_bounds.remove(&body_id);
        self.pending_retighten.remove(&body_id);
        self.cpu_grid_stale.remove(&body_id);
        self.cpu_heal_queue.remove(&body_id);
        self.transform_safe_bodies.remove(&body_id);
        self.dirty_regions.remove(&body_id);
        self.rigid_accum.remove(&body_id);
        self.sparse_scrolled.remove(&body_id);
        self.cascades.remove(&body_id);
        self.body_target_resolutions.remove(&body_id);
        self.body_res_user_locked.remove(&body_id);
        self.body_chunked_meshes.remove(&body_id);
        self.dirty_mesh_region.remove(&body_id);
        // Remove from RAM cache and reclaim budget
        if let Some(ram_entry) = self.ram_cache.remove(&body_id) {
            self.ram_cache_used_bytes = self.ram_cache_used_bytes.saturating_sub(ram_entry.grid.len() * 4);
        }
        self.brick_allocator.deallocate_body(body_id);
        // Release deleted body's pool slot (if any) and clear active tracking
        self.volume_pool.release(body_id);
        // Release brick pool slots and indirection for this body
        self.brick_pool.deallocate_body(body_id);
        self.body_brick_volumes.remove(&body_id);
        if self.resident_body_nid == Some(body_id) {
            self.resident_body_nid = None;
            self.resident_slot = None;
        }
    }

    /// Number of cached bodies.
    #[allow(dead_code)]
    pub(crate) fn cached_count(&self) -> usize {
        self.body_grids.len()
    }

    // ============================================================
    // Raster-pipeline helpers
    // ============================================================

    /// Determine how the raster pipeline should display this body.
    ///
    /// Cache-first rule: bodies are NEVER shown analytically as the visible
    /// surface.  A body that has no cached mesh yet is invisible (NeverBaked)
    /// until the first bake completes — no analytical show-through.
    pub(crate) fn display_state(&self, body_nid: NodeId) -> BodyDisplayState {
        let has_cache = self.body_grids.contains_key(&body_nid);
        let is_baking = self.in_flight.iter().any(|(bid, _)| *bid == body_nid);
        let is_dirty = self.dirty_bodies.contains(&body_nid);
        let is_transform_safe = self.transform_safe_bodies.contains(&body_nid);

        if has_cache && !is_baking && !is_dirty && is_transform_safe {
            // Rigid transform only — mesh is valid, just need updated instance matrix.
            BodyDisplayState::TransformSafe
        } else if has_cache && !is_baking && !is_dirty {
            BodyDisplayState::Raster
        } else if has_cache && (is_baking || is_dirty) {
            BodyDisplayState::RasterOldCache
        } else {
            // No cache yet — body is invisible until first bake completes.
            // Hidden analytical evaluation (bake pipeline, deferred queries)
            // still runs behind the scenes, but produces no visible pixels.
            BodyDisplayState::NeverBaked
        }
    }

    /// Mark all cached bodies as needing rebake (e.g., after resolution change).
    pub(crate) fn mark_all_bodies_dirty(&mut self, sg: &SceneGraph) {
        let all_nids = collect_all_body_nids(sg);
        for nid in all_nids {
            self.dirty_bodies.insert(nid);
        }
        self.transform_safe_bodies.clear();
    }


    /// Reset the cache entirely (alias for invalidate_all).
    pub(crate) fn reset(&mut self) {
        self.invalidate_all();
    }

    /// Return the current generation counter for a body (0 if never baked).
    pub(crate) fn current_generation(&self, body_id: NodeId) -> u64 {
        self.generations.get(&body_id).copied().unwrap_or(0)
    }

    /// Check if all top-level bodies EXCEPT `active_nid` have clean cached grids.
    /// Check that all top-level body ops are union-compatible (no cross-body
    /// subtract/intersect). Hybrid selective serialization is only valid when
    /// bodies compose additively — subtract/intersect between bodies requires
    /// all bodies in the analytical buffer simultaneously.
    pub(crate) fn all_body_ops_union(sg: &SceneGraph) -> bool {
        let all_bodies = collect_all_body_nids(sg);
        for &nid in &all_bodies {
            if let Some(op) = sg.body_op(nid) {
                match op {
                    BooleanOp::Union { .. } | BooleanOp::ChamferUnion { .. } => {},
                    _ => return false,
                }
            }
        }
        true
    }

    /// Flight-recorder STATE SNAPSHOT (vis_recorder / the Developer Tweaks visdump button).
    /// Formats this cache's own internals for the active body so callers don't need
    /// field-visibility changes. READ-ONLY except the cheap O(len) inside% scan (on click /
    /// rate-limited auto-dump only — never on the frame hot path).
    pub(crate) fn debug_state_snapshot(&self, active: NodeId) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "active_body={:?}", active);
        let _ = writeln!(s, "resident_body_nid={:?} resident_slot={:?}",
            self.resident_body_nid, self.resident_slot);
        match self.resident_volume() {
            Some(rv) => {
                let _ = writeln!(s,
                    "resident_volume: body_nid={:?} resolution={:?} bounds={:.4?}..{:.4?} \
                     bake_bounds={:.4?}..{:.4?} buffer_bytes={}",
                    rv.body_nid, rv.resolution, rv.bounds_min, rv.bounds_max,
                    rv.bake_bounds_min, rv.bake_bounds_max, rv.buffer.size());
            }
            None => { let _ = writeln!(s, "resident_volume: None"); }
        }
        let _ = writeln!(s,
            "resident_buffer_valid={} resident_upload_needed={} last_upload_key={:?} \
             last_upload_bounds={:.4?}..{:.4?}",
            self.resident_buffer_valid, self.resident_upload_needed, self.last_upload_key,
            self.last_upload_bounds.0, self.last_upload_bounds.1);
        let _ = writeln!(s,
            "gpu_texture_upload_done={} gpu_texture_upload_body={:?} resident_volume_reallocated={}",
            self.gpu_texture_upload_done, self.gpu_texture_upload_body,
            self.resident_volume_reallocated);
        let _ = writeln!(s, "cpu_grid_stale={:?}", self.cpu_grid_stale);
        let _ = writeln!(s, "cpu_heal_queue={:?}", self.cpu_heal_queue);
        let _ = writeln!(s, "dirty_bodies={:?}", self.dirty_bodies);
        let _ = writeln!(s, "in_flight={:?}", self.in_flight);
        let _ = writeln!(s, "pending_retighten={:?}", self.pending_retighten);
        let _ = writeln!(s,
            "suppress_bake_dispatch={} draft_resolution={:?}",
            self.suppress_bake_dispatch, self.draft_resolution);
        match self.body_grids.get(&active) {
            Some(e) => {
                // inside% = fraction of grid values < 0 — cheap linear scan, dump-time only.
                let inside = e.grid.iter().filter(|&&d| d < 0.0).count();
                let pct = 100.0 * inside as f64 / e.grid.len().max(1) as f64;
                let _ = writeln!(s,
                    "body_grids[active]: len={} bake_resolution={} grid={:.4?}..{:.4?} \
                     source_hash={:#018x} inside={:.2}% shared_source={:?} brick_resident={:?}",
                    e.grid.len(), e.bake_resolution, e.grid_min, e.grid_max,
                    e.source_hash, pct, e.shared_source, e.brick_resident);
            }
            None => { let _ = writeln!(s, "body_grids[active]: None"); }
        }
        let _ = writeln!(s, "body_bounds[active]={:?}",
            self.body_bounds.get(&active).map(|(a, b)| (*a, *b)));
        let _ = writeln!(s,
            "resolution={} use_adaptive_resolution={} body_target_resolutions[active]={:?} gridbacked_only={}",
            self.resolution, self.use_adaptive_resolution,
            self.body_target_resolutions.get(&active).copied(), self.gridbacked_only);
        s
    }

}

#[cfg(test)]
mod cold_tile_dense_tests {
    use super::*;
    use std::sync::Arc;
    use core_math::scene_graph::NodeId;

    /// Terrain is dense 128³ — cold release must free body_grids, not park in ram_cache
    /// (which would keep the 8.4 MB leak) and must not dirty-queue a hidden tile.
    #[test]
    fn release_cold_tile_drops_dense_grid_not_ram_not_dirty() {
        let mut c = BodyVolumeCache::new(128, 2.0);
        let nid = NodeId(42);
        let n = 128usize * 128 * 128;
        c.body_grids.insert(
            nid,
            BodyCacheEntry {
                grid: Arc::new(vec![0.1; n]),
                source_hash: 1,
                grid_min: Vec3::splat(-1.0),
                grid_max: Vec3::splat(1.0),
                bake_resolution: 128,
                shared_source: None,
                brick_resident: None,
                sparse_frozen: None,
                mesh_rigid_offset: Vec3::ZERO,
            },
        );
        c.pending_retighten.insert(nid);
        c.cpu_grid_stale.insert(nid);
        c.dirty_bodies.insert(nid);
        c.generations.insert(nid, 3);
        c.in_flight.insert((nid, 99));
        c.resident_body_nid = Some(nid);
        c.resident_slot = Some(0);

        assert!(c.has_dense_grid(nid));
        assert_eq!(c.body_grids.get(&nid).unwrap().grid.len(), n);
        assert!(c.release_cold_tile_dense_grid(nid));
        assert!(!c.has_dense_grid(nid));
        assert!(!c.ram_cache.contains_key(&nid), "must not park dense leak in ram_cache");
        assert!(!c.dirty_bodies.contains(&nid), "cold must not queue rebake");
        assert!(!c.pending_retighten.contains(&nid));
        assert!(!c.cpu_grid_stale.contains(&nid));
        assert!(!c.in_flight.iter().any(|(b, _)| *b == nid));
        assert_eq!(c.generations.get(&nid).copied(), Some(4), "gen bump rejects late bake");
        assert_ne!(c.resident_body_nid, Some(nid));
        // Second release: no grid, still scrub (returns false).
        assert!(!c.release_cold_tile_dense_grid(nid));
    }
}

// Body prim collection, grid bake, bounds, hashing → moved to bake_utils.rs
// Functions below are kept here because they reference private types (BodyCacheEntry, BakeDirtyRegion).

// Everything below this point moved to bake_utils.rs
// (collect_body_prims, bake_body_grid, compute_body_bake_bounds, hash_*, etc.)
// Accessed via `use crate::bake_utils::*;` at the top of this file.
// ============================================================
// End of body_volume_cache.rs
// Functions moved to bake_utils.rs:
//   collect_all_body_nids, collect_body_prims_recursive, offset_prims_to_local,
//   bake_body_grid, bake_body_grid_region, compute_body_bake_bounds,
//   body_content_id (BodyContentId), hash_prim_slice, pad_grid_bounds, hash_*
