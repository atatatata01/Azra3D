// core_engine/app/src/voxel_sculpt_types.rs
//
// Slice 29 (app-crate de-god, PURE RELOCATION from main.rs): the voxel-sculpt data types +
// their inherent impls (VoxelObject and friends). NONE are methods on FourthLayerApp/AzraApp —
// they are the standalone sculpt-object model. Every flat path (crate::VoxelObject,
// crate::MeshGrab, crate::UndoKind, …) still resolves via the `pub(crate) use voxel_sculpt_types::*;`
// re-export in main.rs.

/// Which undo system a committed voxel stroke belongs to (Phase A.2). The Dense/box path records a CPU
/// density delta (`voxel_undo_stack`); the sparse ATLAS path records a GPU brick snapshot
/// (`voxel_atlas_undo`); `CpuColor` reverts a vertex-paint edit; `Grid` restores a full pre-change
/// `VoxelGridRecord` (Move grow / Ctrl+R remesh — undoes resolution + density together).
/// One marker is pushed per stroke so undo/redo dispatches in strict interleave order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UndoKind {
    Cpu, Atlas, CpuColor, Grid,
    /// Surface-mode mesh stroke (3D-Coat Surface mode): the entry restores the affected chunks'
    /// pre-stroke VERTEX arrays (`voxel_surface_undo_stack`) — no density involved. Barriered:
    /// while surface edits are un-voxelized, density-kind entries below it refuse to pop (see undo()).
    Surface,
}

/// Brush-tip **alpha orientation** (ZBrush Alpha → Orientation / 3D-Coat Stamp Orientation).
/// Combined with `tip_angle` (fixed degrees) and optional Random Rotation dynamics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u32)]
pub(crate) enum TipOrientation {
    /// Constant angle = `tip_angle` only (no stroke/view spin).
    #[default]
    Fixed = 0,
    /// Rotate UV with stroke tangent in the stamp plane (Follow Stroke).
    FollowStroke = 1,
    /// Align tip “up” with camera up projected into the stamp plane.
    View = 2,
    /// Full random spin each dab (±180°); still adds `tip_angle` offset.
    Random = 3,
}

impl TipOrientation {
    pub const ALL: [TipOrientation; 4] = [
        TipOrientation::Fixed,
        TipOrientation::FollowStroke,
        TipOrientation::View,
        TipOrientation::Random,
    ];
    #[inline]
    pub fn as_u32(self) -> u32 { self as u32 }
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => TipOrientation::FollowStroke,
            2 => TipOrientation::View,
            3 => TipOrientation::Random,
            _ => TipOrientation::Fixed,
        }
    }
    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            TipOrientation::Fixed => "Fixed",
            TipOrientation::FollowStroke => "Follow",
            TipOrientation::View => "View",
            TipOrientation::Random => "Random",
        }
    }
    #[inline]
    pub fn tip(self) -> &'static str {
        match self {
            TipOrientation::Fixed => "Fixed — use Alpha Angle only (no stroke/view spin).",
            TipOrientation::FollowStroke => "Follow Stroke — tip UV tracks stroke tangent (ZBrush Follow Stroke).",
            TipOrientation::View => "View — tip up aligns with camera up on the stamp plane.",
            TipOrientation::Random => "Random — full spin each dab (±180°). Extra Random Rotation still adds.",
        }
    }
}

// ==================================================================
// Voxel sculpt undo/redo stack (VS5)
// ==================================================================
/// Bounded voxel-delta stack for sculpt undo/redo. Eviction (oldest-first) is owned
/// here at the single push chokepoint, enforcing BOTH an aggregate byte budget (so a
/// long session can't grow resident memory without bound — the previous count-only cap
/// allowed ~800 MB) and a secondary entry-count cap. `total_bytes` is maintained
/// incrementally so it is always exact without an O(n) recount.
#[derive(Default)]
pub(crate) struct VoxelUndoStack {
    deque: std::collections::VecDeque<Vec<(u32, f32)>>,
    total_bytes: usize,
}

impl VoxelUndoStack {
    const MAX_BYTES: usize = 64 * 1024 * 1024; // ~64 MB, like production sculpt undo
    const MAX_ENTRIES: usize = 50;

    fn entry_bytes(delta: &[(u32, f32)]) -> usize { delta.len() * std::mem::size_of::<(u32, f32)>() }

    /// Push a stroke's delta; returns the number of OLDEST entries evicted to stay within budget. The
    /// caller MUST prune that many front `UndoKind::Cpu` markers from `voxel_undo_order` so the shared
    /// order stream can't outlive its backing entries (else undo silently stops early — finding #4).
    pub(crate) fn push(&mut self, delta: Vec<(u32, f32)>) -> usize {
        let bytes = Self::entry_bytes(&delta);
        let mut evicted = 0usize;
        // Evict oldest until within byte budget — but never drop below 1 entry, so a
        // single huge stroke is never silently lost.
        while self.total_bytes + bytes > Self::MAX_BYTES && !self.deque.is_empty() {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; }
        }
        // Secondary count guard for many small strokes.
        while self.deque.len() >= Self::MAX_ENTRIES {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; } else { break; }
        }
        self.total_bytes += bytes;
        self.deque.push_back(delta);
        debug_assert_eq!(self.total_bytes, self.deque.iter().map(|d| Self::entry_bytes(d)).sum::<usize>());
        evicted
    }

    pub(crate) fn pop_back(&mut self) -> Option<Vec<(u32, f32)>> {
        let d = self.deque.pop_back();
        if let Some(ref d) = d { self.total_bytes -= Self::entry_bytes(d); }
        d
    }

    pub(crate) fn clear(&mut self) { self.deque.clear(); self.total_bytes = 0; }
}

// ==================================================================
// Surface-mode mesh-stroke undo stack (3D-Coat Surface mode)
// ==================================================================
/// One surface-mode stroke's undo payload: for each affected chunk (by linear chunk index `ci`),
/// the FULL pre-stroke vertex array (`[px,py,pz,nx,ny,nz,extra,extra]` per vertex — the raster
/// layout `mesh_cache::chunk_verts` holds). Topology (index buffer) is untouched by surface
/// strokes, so restoring positions+normals via `update_chunk_verts_in_place` is exact.
pub(crate) type SurfaceMeshDelta = Vec<(u32, Vec<[f32; 8]>)>;

/// Bounded stack for surface-mode mesh strokes — the same eviction contract as `VoxelUndoStack`
/// (byte budget + entry cap at the single push chokepoint; `push` returns the evicted count so
/// the caller prunes that many front `UndoKind::Surface` markers from the shared order stream).
#[derive(Default)]
pub(crate) struct VoxelSurfaceUndoStack {
    deque: std::collections::VecDeque<SurfaceMeshDelta>,
    total_bytes: usize,
}

impl VoxelSurfaceUndoStack {
    const MAX_BYTES: usize = 96 * 1024 * 1024; // vertex snapshots are bulkier than density deltas
    const MAX_ENTRIES: usize = 50;

    fn entry_bytes(delta: &SurfaceMeshDelta) -> usize {
        delta.iter().map(|(_, v)| v.len() * 32 + 16).sum()
    }

    pub(crate) fn push(&mut self, delta: SurfaceMeshDelta) -> usize {
        let bytes = Self::entry_bytes(&delta);
        let mut evicted = 0usize;
        while self.total_bytes + bytes > Self::MAX_BYTES && !self.deque.is_empty() {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; }
        }
        while self.deque.len() >= Self::MAX_ENTRIES {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; } else { break; }
        }
        self.total_bytes += bytes;
        self.deque.push_back(delta);
        evicted
    }

    pub(crate) fn pop_back(&mut self) -> Option<SurfaceMeshDelta> {
        let d = self.deque.pop_back();
        if let Some(ref d) = d { self.total_bytes -= Self::entry_bytes(d); }
        d
    }

    pub(crate) fn clear(&mut self) { self.deque.clear(); self.total_bytes = 0; }
}

/// Bounded voxel COLOUR-delta stack for the sculpt vertex-paint undo/redo path (the "Fill All" button,
/// and any future colour-only edit). Identical budgeting policy to `VoxelUndoStack` but stores
/// `(idx, color_word)` pairs instead of `(idx, density)` — colours are their own undo channel because the
/// density stack reverts shape, not paint. Pushes a `UndoKind::CpuColor` marker on the shared order
/// stream so colour and shape edits interleave yet pop in strict order.
#[derive(Default)]
pub(crate) struct VoxelColorUndoStack {
    deque: std::collections::VecDeque<Vec<(u32, u32)>>,
    total_bytes: usize,
}

impl VoxelColorUndoStack {
    const MAX_BYTES: usize = 64 * 1024 * 1024; // ~64 MB, matching the density stack
    const MAX_ENTRIES: usize = 50;

    fn entry_bytes(delta: &[(u32, u32)]) -> usize { delta.len() * std::mem::size_of::<(u32, u32)>() }

    /// Push a colour delta; returns the count of OLDEST entries evicted to stay within budget. The caller
    /// MUST prune that many front `UndoKind::CpuColor` markers from the order stream (mirrors the density
    /// stack's contract — else undo silently stops early).
    pub(crate) fn push(&mut self, delta: Vec<(u32, u32)>) -> usize {
        let bytes = Self::entry_bytes(&delta);
        let mut evicted = 0usize;
        while self.total_bytes + bytes > Self::MAX_BYTES && !self.deque.is_empty() {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; }
        }
        while self.deque.len() >= Self::MAX_ENTRIES {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; } else { break; }
        }
        self.total_bytes += bytes;
        self.deque.push_back(delta);
        debug_assert_eq!(self.total_bytes, self.deque.iter().map(|d| Self::entry_bytes(d)).sum::<usize>());
        evicted
    }

    pub(crate) fn pop_back(&mut self) -> Option<Vec<(u32, u32)>> {
        let d = self.deque.pop_back();
        if let Some(ref d) = d { self.total_bytes -= Self::entry_bytes(d); }
        d
    }

    pub(crate) fn clear(&mut self) { self.deque.clear(); self.total_bytes = 0; }
}

/// Full-grid undo for resolution / bounds identity changes (Move grow, Ctrl+R remesh).
/// Stores compact `VoxelGridRecord`s (dense buffer or sparse bricks). Budget is tight — a few
/// 128³–256³ snapshots, not dozens — so grow thrash cannot balloon RAM.
#[derive(Default)]
pub(crate) struct VoxelGridUndoStack {
    deque: std::collections::VecDeque<core_math::voxel_sculpt::VoxelGridRecord>,
    total_bytes: usize,
}

impl VoxelGridUndoStack {
    const MAX_BYTES: usize = 96 * 1024 * 1024; // ~96 MB — a few dense mid-res snapshots
    const MAX_ENTRIES: usize = 8;

    fn entry_bytes(r: &core_math::voxel_sculpt::VoxelGridRecord) -> usize {
        use core_math::voxel_sculpt::VoxelGridPayload;
        let payload = match &r.payload {
            VoxelGridPayload::Dense(d) => d.len() * 4,
            VoxelGridPayload::Sparse { tags, bricks } => tags.len() * 4 + bricks.len() * 4,
        };
        payload + 32
    }

    /// Push a pre-change grid snapshot. Returns how many oldest Grid markers to prune from the order stream.
    pub(crate) fn push(&mut self, rec: core_math::voxel_sculpt::VoxelGridRecord) -> usize {
        let bytes = Self::entry_bytes(&rec);
        let mut evicted = 0usize;
        while self.total_bytes + bytes > Self::MAX_BYTES && !self.deque.is_empty() {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; }
        }
        while self.deque.len() >= Self::MAX_ENTRIES {
            if let Some(old) = self.deque.pop_front() { self.total_bytes -= Self::entry_bytes(&old); evicted += 1; } else { break; }
        }
        self.total_bytes += bytes;
        self.deque.push_back(rec);
        evicted
    }

    pub(crate) fn pop_back(&mut self) -> Option<core_math::voxel_sculpt::VoxelGridRecord> {
        let d = self.deque.pop_back();
        if let Some(ref d) = d { self.total_bytes -= Self::entry_bytes(d); }
        d
    }

    pub(crate) fn clear(&mut self) { self.deque.clear(); self.total_bytes = 0; }
}

// ==================================================================
// New voxel sculpt object (3D-Coat-style voxel room)
// ==================================================================
/// A live density-voxel sculpt object. The grid is sculpted directly (add/remove
/// material); its surface is a Surface Nets mesh rendered through the existing raster
/// mesh pipeline via an ImportedMesh scene node. NOT an SDF.
pub(crate) struct VoxelObject {
    pub grid: core_math::voxel_sculpt::VoxelGrid,
    pub node: core_math::scene_graph::NodeId,
    pub mesh_id: core_math::scene_graph::MeshCacheId,
    pub brush: core_math::voxel_sculpt::VoxelBrush,
    pub radius: f32,
    pub strength: f32,
    /// Continuous radial brush hardness 0..1 (0 = soft round shoulder, 1 = sharp pointed core). TOOL
    /// state — carries across body switches like brush/radius. Threaded into `apply_brush` (and the GPU
    /// brush params) so EVERY brush shares the chosen falloff. Only affects [`FalloffPreset::Blend`].
    pub falloff: f32,
    /// Blender falloff family (F1b). Default Smooth. TOOL state.
    pub falloff_preset: core_math::voxel_sculpt::FalloffPreset,
    /// Mask-paint mode: strokes write the dense sculpt mask instead of density. Ctrl = clear mask.
    pub mask_paint: bool,
    /// Once-per-session toast for sparse Move honesty.
    pub sparse_move_warned: bool,
    /// How the per-stamp deposit DIRECTION + oriented brush ring are sourced.
    /// Default `FirstPoint` = first-click normal of the stroke (3D-Coat / Original Normal).
    /// `Surface` = re-sample live each stamp while stroking. Also View / X/Y/Z axis locks.
    /// Resolved once per stamp into the `normal` for plane/normal-relative brushes; Move's kernel
    /// still ignores deposit normal (cursor drag), but FirstPoint still latches for the ring.
    /// TOOL state — carries across body switches like brush/falloff/symmetry.
    pub brush_dir: core_math::voxel_sculpt::BrushDirection,
    /// **Add / Remove** polarity (ZBrush Zadd/Zsub, 3D-Coat Add/Remove).
    /// `true` = Add (deposit / forward action), `false` = Remove (carve / reverse action).
    /// **Ctrl** temporarily flips this for every brush (Shift still forces Smooth, no invert).
    pub brush_add: bool,
    /// **Average Region** — memory length for running Average Normal (0.25..=4, default 1).
    /// Higher = longer-running / smoother average of surface normals along the stroke.
    /// Used by `BrushDirection::Average` and by Smooth Normal blending.
    pub dir_average_region: f32,
    /// **Smooth Normal** — blend resolved deposit direction toward the stroke average (0..=1, default 0).
    /// 0 = pure Direction mode; 1 = fully averaged. Softens Surface/Normal jitter without locking Original.
    pub dir_smooth_normal: f32,
    /// Surface normal LATCHED at the FIRST click of the current stroke (voxel-LOCAL space), used by
    /// `BrushDirection::FirstPoint` ("Original Normal") for deposit plane + brush ring. `None` at
    /// stroke start / outside a stroke; set on the first stamp and held until the stroke ends.
    pub first_point_normal: Option<glam::Vec3>,
    /// Surface position latched with `first_point_normal` — stamp centers project onto this plane
    /// for Original Normal so Draw Add does not climb the pile (Remove stays controlled).
    pub first_point_pos: Option<glam::Vec3>,
    /// Running sum of unit surface normals this stroke — for `BrushDirection::Average`.
    /// Reset at drag-start. `average_normal_count` is the sample count.
    pub average_normal_sum: glam::Vec3,
    pub average_normal_count: u32,
    /// Previous stamp position this stroke (any brush) for Brush/Tangent direction + Move drag.
    /// `None` at stroke start.
    pub stroke_prev_pos: Option<glam::Vec3>,
    /// Last *stable* surface hit + normal this stroke (voxel-LOCAL). Used to:
    ///   • stick the tip when raycast misses mid-stroke (no random air gaps)
    ///   • reject wild position/normal jumps (atlas CPU-stale vs live gbuf fight)
    ///   • keep the oriented ring continuous on long strokes (Normal/Surface).
    /// Reset at stroke start; updated every accepted stamp frame.
    pub stroke_last_surface: Option<(glam::Vec3, glam::Vec3)>,
    /// Fully resolved primary stamps deferred by the per-frame budget (not dropped). These already
    /// carry their final direction/orientation/dynamics and must not pass through those stages again.
    /// Drained FIFO next frame / on pen-up; symmetry expands only the subset actually dispatched.
    pub deferred_stamps: Vec<core_math::sculpt_layer::StrokeSample>,
    /// Random stream identity and absolute primary-dab cursor for this stroke. These are interaction
    /// state, not brush settings: they reset once on pen-down and make dynamics independent of FPS.
    pub dyn_stroke_seed: u32,
    pub dyn_dab_index: u32,
    /// Previous stamp's surface position WITHIN the current Move/Grab stroke (voxel-LOCAL space), used to
    /// derive the per-stamp drag delta (`pos - move_prev_pos`) that the Move brush advects the field along.
    /// `None` at stroke start (first stamp warps nothing) and outside a stroke. Only the Move brush uses it.
    pub move_prev_pos: Option<glam::Vec3>,
    /// ELASTIC GRAB state for the Move/Grab brush (3D-Coat style). The grabbed region is captured at
    /// stroke start; each drag frame the field is restored to pristine and re-deformed by the CUMULATIVE
    /// drag (anchor → cursor), so the pull is a reversible elastic stretch rather than per-frame voxel
    /// accumulation. Committed to undo as ONE delta on release. Only the Move brush uses it; inactive
    /// (`is_active() == false`) outside a Move stroke.
    pub elastic_grab: core_math::voxel_sculpt::ElasticGrab,
    /// MESH-VERTEX GRAB state for the Move brush (Meta-Medium style). The Move brush no longer warps
    /// the density field (which mathematically can't pull material past the brush radius — it folds /
    /// vanishes). Instead it deforms the TRIANGLE MESH directly during the drag (an unbounded, linear,
    /// screen-space vertex grab) and re-voxelizes the deformed mesh back to density ONCE on release
    /// (Phase 2). Inactive (`active == false`) outside a Move stroke; Smudge keeps `elastic_grab`.
    pub mesh_grab: MeshGrab,
    /// DEFERRED VOXELIZE ("surface mode", 3D-Coat's "voxelize immediately = OFF"). When `move_deferred`
    /// is on (default), a Move release does NOT re-voxelize (the slow mesh→SDF round-trip); instead the
    /// finished grab's symmetry-expanded instances are PUSHED here and kept displaying live via the
    /// G-buffer vertex shader (`CameraUniforms.grab_*`). Multiple moves accumulate (the shader SUMS them);
    /// the stack is baked into the density grid ONCE on demand — a Voxelize button / Enter / ~2s idle /
    /// the `MAX_GRAB_INSTANCES` cap (`voxel_flush_pending_move`). Empty outside surface mode.
    pub pending_move_grabs: Vec<PendingGrab>,
    /// Per deferred Move release: how many leading/trailing instances that stroke pushed onto
    /// `pending_move_grabs` (symmetry expands to 1..8). LIFO stack parallel to releases — Ctrl+Z pops
    /// the last group without voxelizing. Cleared when the stack is fully drained on bake land.
    pub pending_move_group_sizes: Vec<usize>,
    /// Redo stack for surface-mode undos (groups of `PendingGrab` removed by Ctrl+Z before voxelize).
    pub pending_move_redo: Vec<Vec<PendingGrab>>,
    /// Surface-mode toggle: ON (default, like 3D-Coat) = Move release defers the re-voxelize (accumulate in
    /// `pending_move_grabs`); OFF = the legacy voxelize-on-release (full mesh→SDF + undo + drain each release).
    /// Factory **Dynamic Topology ON** ⇒ `move_deferred = false` (voxelize each release).
    pub move_deferred: bool,
    /// Move **Elasticity** 0..2 — scales grab drag (1 = full screen-space pull). TOOL state.
    pub move_elasticity: f32,
    /// Move **Pull Strength** 0..2 — second drag multiplier (1 = 1:1 with Elasticity). TOOL state.
    pub move_pull_strength: f32,
    /// Move **Connected Only** — prefer island-local grab (surface component under hit). TOOL state.
    /// Currently soft: grab still uses brush sphere falloff; reserved for connectivity flood.
    pub move_connected_only: bool,
    /// Auto-voxelize timer: set to `now + ~2s` on each deferred Move release; when it elapses (checked
    /// per-frame) `voxel_flush_pending_move` bakes the pending stack. Re-armed by each new release; cleared
    /// when the stack is flushed. `None` = no pending auto-voxelize.
    pub move_voxelize_due: Option<std::time::Instant>,
    /// 3D-Coat-style **Surface vs Voxel sculpting mode** (per-object, runtime working-set state —
    /// resets to Voxel when the working set is rebuilt, which is coherent because every leave-edit /
    /// save chokepoint revoxelizes first). `false` = Voxel mode (today's behaviour: brushes write the
    /// density field, topology is free). `true` = Surface mode: supported brushes displace the EXISTING
    /// Surface-Nets chunk mesh vertices directly (fixed topology, no density writes) — the MESH is the
    /// authority and the density field goes stale over `surface_dirty_region` until the next
    /// revoxelization (mode toggle back / save / leave-edit).
    pub surface_mode: bool,
    /// Cell-AABB union of every surface-mode mesh edit since the last revoxelization. `Some` =
    /// density is STALE over this region (mesh is authoritative); `None` = density in sync.
    /// Consumed by `voxel_surface_revoxelize` (mesh→SDF over exactly this region).
    pub surface_dirty_region: Option<[u32; 6]>,
    // NOTE: the live-stroke first-touch snapshot (`surface_stroke_snap`) moved to `SculptState`
    // (2026-07-23) — it is PER-STROKE state whose lifecycle is owned by `surface_end_stroke`.
    /// Surface-mode DISPLACED-CHUNK registry: linear chunk index → the chunk's CURRENT vertex AABB
    /// (voxel-LOCAL min/max), updated on every surface vertex write to that chunk. THE INVARIANT
    /// KEEPER for "a position moves ⇒ ALL copies move": a vertex belongs FOREVER to the chunk that
    /// meshed it, but accumulated displacement (deep / repeated carves, Snake Hook pulls) can drag
    /// its current position arbitrarily far from that chunk's HOME density cells — so a dab's
    /// chunk list derived from grid cells alone misses the owner chunk, its copy of a duplicated
    /// border vertex stays put while the neighbour chunk's copy moves, and the diverged copy
    /// renders as a NEEDLE SPIKE. `voxel_surface_candidate_chunks` unions the grid-cell list with
    /// every chunk here whose current AABB intersects the footprint. Cleared with the snapshots on
    /// revoxelization / mode entry (a density remesh puts every vertex back in its home cells).
    pub surface_moved_aabbs: std::collections::HashMap<u32, (glam::Vec3, glam::Vec3)>,
    /// Gate-1 pool sync, GENERATION-KEYED (chunk → the `(pool_slot, slot content generation,
    /// pool generation)` key the chunk's topology was last surface-uploaded against). A verts-only
    /// in-place upload is allowed ONLY when the stored key equals the slot's CURRENT key
    /// (`pool_verts_only_allowed`) — any GPU-mesher rewrite, slot release/re-acquire, cap-grow
    /// re-seed or pool realloc changes the current key, forcing a topology re-upload. A chunk NOT
    /// in this map still shows GPU-mesher output (self-consistent) until its first surface write.
    /// The mode-entry/exit/realloc/rebuild `clear()`s remain as belt-and-braces, but correctness no
    /// longer depends on any of them (the key comparison is the enforcement).
    pub surface_pool_synced: std::collections::HashMap<u32, crate::voxel_gpu_mesh::PoolSlotSyncKey>,
    /// Surface-mode Snake Hook: stroke-start anchor (voxel-LOCAL) + view-plane normal, latched on
    /// the first drag frame. `Some` only while a Snake Hook stroke is live; cleared on release/cancel.
    /// The moving tube pull = `grab_delta = cursor_on_plane − anchor` is recomputed each frame from
    /// this anchor over the pristine `surface_stroke_snap` (elastic, reversible). Runtime-only.
    pub snake_anchor: Option<glam::Vec3>,
    pub snake_plane_n: glam::Vec3,
    /// Mirror sculpting across the object-LOCAL X / Y / Z planes (ZBrush/3D-Coat symmetry). Each
    /// enabled axis duplicates every brush stamp at the plane-reflected position+normal, applied at
    /// the STAMP level in `handle_voxel_sculpt_input` (never inside the core brush kernel) so the
    /// mirror lands on every path — CPU, GPU box, GPU atlas — identically.
    pub symmetry: [bool; 3],
    /// Last extracted triangle count (for the remesh UI's "current polygons" readout).
    pub tris: u32,
    /// Remesh popup (Ctrl+R) open state + the resolution the user is dialing in.
    pub remesh_open: bool,
    pub remesh_target_res: u32,
    /// LazyMouse stabilizer amount 0..1 (0 = off). The brush lags the cursor by a rope of
    /// `lazy * scale` screen px, smoothing jittery strokes (ZBrush LazyMouse / 3D-Coat).
    pub lazy: f32,
    /// Smoothed (lagged) screen position carried across the drag; None outside a stroke.
    pub lazy_pos: Option<glam::Vec2>,
    /// Stroke dab spacing as a fraction of brush radius (Blender default ~0.10). Drives
    /// `StrokeInterpolator.spacing` at stroke start. TOOL state — shared across brushes.
    /// Smaller = denser continuous clay; larger = faster/lighter stamps (risk of beads if too high).
    pub stroke_spacing: f32,
    /// P-B chunked rendering: the grid's cells are split into a `chunk_dim³` grid of chunks
    /// (VOXEL_CHUNK cells/axis). Each chunk has its own mesh-cache id (its own persistent GPU
    /// buffer) and tri count; a brush re-meshes + re-uploads ONLY the chunks it touched, so the
    /// per-frame cost scales with the brush, not the whole surface. `chunk_tris[i]==0` = empty
    /// (skipped in the raster draw). The voxel node renders all non-empty chunks.
    pub chunk_dim: u32,
    pub chunks: Vec<core_math::scene_graph::MeshCacheId>,
    pub chunk_tris: Vec<u32>,
    /// Last known **surface** chunk count (non-empty shell). Used to size the GPU mesh pool —
    /// never use `chunks.len()` (that is `chunk_dim³`, ~262k at 1024³). Updated on remesh swap /
    /// `voxel_build_all_chunks` / dirty drain. 0 = unknown (pool uses a safe clamp).
    pub surface_chunk_count: u32,
    /// Chunks awaiting re-mesh (linear chunk index). Populated by the brush (only the chunks each
    /// stamp actually passes through — NOT the bounding box of the whole frame's stamps, which a
    /// fast flick blows up to the whole grid) and drained at a capped rate per frame (Frame Budget
    /// Rule), so no single frame re-scans the entire chunk grid.
    pub dirty_chunks: std::collections::HashSet<u32>,
    /// A resolution change (Ctrl+R) rebuilds the whole surface off-screen here while the CURRENT
    /// grid/chunks keep rendering, then swaps in atomically when done — so the model never
    /// disappears or pops in chunk-by-chunk. None = no rebuild in progress.
    pub pending_rebuild: Option<PendingVoxelRebuild>,
    /// ASYNC Move voxelize in flight (the whole-object mesh→SDF bake runs on a worker thread —
    /// `voxel_spawn_move_bake` — so the UI never freezes; measured 1.7–2.4s synchronous before).
    /// The covered `pending_move_grabs` entries STAY in the list while baking (the G-buffer vertex
    /// shader keeps displaying the displaced surface exactly as during a deferred move); density
    /// strokes / voxel undo / remesh are paused while in flight (the `pending_rebuild` precedent).
    /// Landed atomically by `voxel_land_move_bake`. None = no bake in flight.
    pub pending_move_bake: Option<PendingMoveBake>,
    /// Per-phase sculpt timing (toggle in the Sculpt tools). Diagnostic only.
    pub profile: SculptProfile,
    /// Last GPU-vs-CPU mesh parity result (debug); shown persistently in the Sculpt panel.
    pub parity_msg: String,
    /// Live GPU meshing toggle (sculpt Phase 1c): when on, `voxel_flush_dirty_chunks` meshes the
    /// dirty chunks on the GPU instead of the CPU. Off = the CPU path (safe default / fallback).
    pub gpu_mesh: bool,
    /// GPU-RESIDENT mesh sub-toggle (sculpt Phase 6, requires gpu_mesh). When on, meshed chunks stay in
    /// GPU buffers drawn via `draw_indexed_indirect` — NO per-chunk CPU readback, removing the
    /// ~5000 chunks/sec ceiling that causes fast-stroke chunk-patch tearing. Off (default) = the proven
    /// readback→mesh_cache→draw_indexed path (also the save/export source). EXPERIMENTAL until validated.
    pub gpu_resident_mesh: bool,
    /// Live GPU brush toggle (sculpt Phase 2): when on, the 8 in-place ops apply on the GPU instead
    /// of the CPU `apply_brush`. Smooth/Inflate/Pinch always stay on CPU. Off = full CPU path.
    pub gpu_brush: bool,
    /// GPU-RESIDENT brush sub-toggle (sculpt Phase 5a, requires gpu_brush + a Sparse grid). When on,
    /// the brush gathers/writes the GPU atlas in place (no per-frame CPU box round-trip) and the CPU
    /// grid is synced lazily on stroke release — the large-brush perf path. Pinch falls back to the
    /// box path. Off (default) = the proven box-round-trip GPU brush. EXPERIMENTAL until validated.
    pub gpu_brush_atlas: bool,
    /// Sculpt-vertex-paint toggle (Blender-style). When on, every brush stamp ALSO deposits the chosen
    /// BRUSH paint colour (`paint_color`, NOT the body material albedo) onto the voxels it touches (live,
    /// via the CPU dense mesh path); off = shape only. TOOL state — carries across body switches like
    /// brush/radius.
    pub paint_color_on: bool,
    /// The BRUSH paint colour (linear RGB), deposited per-voxel while `paint_color_on` is on and by the
    /// "Fill All" button. DELIBERATELY separate from the body's Material > Albedo (the base material of
    /// the whole object): painting recolours individual voxels with THIS colour so the painted result is
    /// visible against the untouched base albedo. TOOL state — carries across body switches like the
    /// brush, radius, and the paint toggle. Defaults to a neutral mid-grey.
    pub paint_color: glam::Vec3,
    /// Voxel-AABB union of everything THIS stroke has edited, accumulated each frame. On stroke
    /// release the grid's sparse bricks over this region are compacted (uniform → FULL/EMPTY tag),
    /// keeping the brick arena from growing as a session builds up new solid material. None outside
    /// a stroke; no-op for a Dense grid.
    pub stroke_aabb: Option<[u32; 6]>,
    /// Chunk coords (cgx,cgy,cgz) of the brush's most recent edit — the per-frame mesh-chunk budget
    /// drains the dirty chunks NEAREST this first, so the region under the cursor always re-meshes
    /// this frame and any catch-up trails off in the periphery (instead of a seam at the cursor when
    /// a fast brush dirties more than the per-frame cap). None outside a stroke.
    pub focus_chunk: Option<[u32; 3]>,
    /// Phase C (depth-raycast): the most recent surface point + normal under the cursor, in voxel-LOCAL
    /// space, read back from the rendered `gbuf_normal` (one frame late). The atlas-path brush uses this
    /// instead of a CPU `grid.raycast`, so brush positioning needs no CPU grid. `None` = cursor over
    /// empty space / no readback yet (fall back to the CPU raycast while the CPU grid is still current).
    pub last_surface_hit: Option<(glam::Vec3, glam::Vec3)>,
    /// Adaptive per-frame resident re-mesh budget (chunks). Feedback-controlled toward a frame-time
    /// target so high resolution (1024³ = ~8× the chunks per stroke) drains its backlog as fast as the
    /// budget allows without tanking fps: raised when the cap is hit + meshing is cheap + backlog remains,
    /// lowered when the mesh phase runs long. Starts at the old fixed 512.
    pub resident_cap: u32,
    /// True while a GPU-RESIDENT (atlas) stroke is in progress: the brush wrote the GPU atlas POOL
    /// directly, so the CPU grid is BEHIND the atlas over `stroke_aabb`. On release (Phase A.2) the
    /// per-stroke POOL snapshot is finalised into the GPU brick-snapshot undo (NO readback), the CPU
    /// grid is flagged `cpu_grid_stale` (materialised lazily later), then this clears.
    pub atlas_stroke: bool,
    /// The brush path LATCHED at stroke start (drag begin): the GPU-resident vs box decision must not
    /// change mid-stroke (mixing CPU- and GPU-authoritative frames corrupts the accumulator + undo).
    /// `Some(true)` = this stroke uses the atlas path; `Some(false)` = box path; `None` = not stroking.
    pub stroke_uses_atlas: Option<bool>,
    /// Phase A.2 (GPU brick-snapshot undo): the CPU grid is BEHIND the GPU atlas because atlas strokes no
    /// longer read back on pen-up. Set true after any atlas stroke / atlas undo-redo. Consumed lazily by
    /// `materialize_cpu_from_atlas` (a ONE blocking whole-grid readback) the next time the CPU grid is
    /// actually needed (resample, save, duplicate) — never on the sculpt hot path.
    pub cpu_grid_stale: bool,
    // ---- Pen / tablet PRESSURE SENSITIVITY (tool state — carries across body switches) ----
    /// Master enable for stylus-pressure modulation of the brush. Default OFF. When on, the latest
    /// touch/pen force (`egui::Event::Touch { force }`, 0..1) read each frame in
    /// `handle_voxel_sculpt_input` scales the brush via the two sub-toggles below. When the input
    /// device reports no force (ordinary mouse, or a Windows pen arriving as plain mouse events),
    /// pressure falls back to 1.0 → behaviour is identical to today.
    pub pressure_enabled: bool,
    /// Pressure → Strength: `effective_strength = strength * pressure`. Default ON (the natural mapping).
    pub pressure_to_strength: bool,
    /// Pressure → Radius: `effective_radius = radius * mix(1−amount, 1.0, pressure)`. Default OFF.
    pub pressure_to_radius: bool,
    /// How much pressure shrinks radius: `radius *= (1 − amount) + amount * pressure`.
    /// Spec Clay BuildUp uses **0.30** (30%). Default **0.60** matches the old mix(0.4, 1.0) curve.
    pub pressure_radius_amount: f32,
    /// PER-BRUSH SETTINGS toggle. OFF (default) = radius / strength / falloff / brush_dir are SHARED
    /// across all brushes (today's behaviour). ON = each `VoxelBrush` remembers its OWN four settings:
    /// switching the active brush SAVES the outgoing brush's current settings into `brush_settings` and
    /// RESTORES the incoming brush's saved settings (or seeds them from the current values the first time
    /// a brush is selected). TOOL state — carries across body switches like the rest of the brush setup.
    pub per_brush_settings: bool,
    /// Saved settings per brush variant — populated lazily as brushes are switched while `per_brush_settings`
    /// is ON. A brush with no entry yet inherits the current live settings on its first activation.
    pub brush_settings: std::collections::HashMap<core_math::voxel_sculpt::VoxelBrush, VoxelBrushSettings>,
    /// Pinch gather-reach intensity (ZBrush-style Pinch Strength). Multiplies tangential sample offset.
    /// 1.0 = legacy parity. TOOL state — carries across body switches. Range ~0.1..4 in UI.
    pub pinch_intensity: f32,
    /// Optional greyscale brush-tip alpha map (from loaded image). Multiplies radial falloff in the
    /// stamp plane. When set, stamps take the CPU path (GPU tip texture not yet wired). TOOL state.
    pub brush_tip: Option<core_math::voxel_sculpt::BrushTipAlpha>,
    /// Display name of the loaded tip file (for the panel label).
    pub brush_tip_name: String,
    /// Full path to tip image (for Save As / reload). Empty if none.
    pub brush_tip_path: Option<String>,
    /// Align tip UV to stroke tangent each dab (legacy bool; kept in sync with `tip_orient`).
    pub tip_follow_stroke: bool,
    /// **Alpha Orientation** — Fixed / Follow Stroke / View / Random (`TipOrientation` as u32).
    pub tip_orient: u32,
    /// **Alpha Angle** — fixed rotation in degrees (0..360), added after orientation mode. Default 0.
    pub tip_angle: f32,
    /// Tip alpha mid-point contrast (`1` = identity). Clay BuildUp factory uses **0.85**.
    pub tip_contrast: f32,
    /// Skip dabs whose stamp normal faces away from the camera (Front Faces Only / Backface Mask).
    pub front_faces_only: bool,
    /// After each density stamp, mild Laplacian relax (ZBrush-style auto smooth). TOOL state.
    pub auto_smooth: bool,
    /// Auto smooth amount 0..1 (maps to relax strength). Default 0.35 when enabled.
    pub auto_smooth_intensity: f32,
    /// Force deposit along live surface normal (and keep strokes skin-relative). TOOL state.
    pub surface_extrude: bool,
    /// **Sculpt on Surface** (ZBrush / Blender): brush sticks to the visible mesh — GPU depth pick +
    /// raycast, no free-space painting. Default ON. Off = free / last-plane freer strokes.
    pub sculpt_on_surface: bool,
    /// **Layer Height** — proud plane as ×radius (Build/Clay). Default 0.14.
    pub layer_height: f32,
    /// **Layer×Str** — offset += gain × strength. Default 0.22.
    pub layer_gain: f32,
    /// **Plane Offset** — base plane shift as ×radius (Clay). Default 0.10.
    pub plane_offset: f32,
    /// **Plane Bias** — extra normal offset as ×radius. Default 0.0.
    pub plane_bias: f32,
    /// **Build Rate** — converge pack speed (legacy 2.0). Default 2.0.
    pub build_rate: f32,
    /// **Height Limit** — max density for Build target 0..1. Default 1.0.
    pub height_limit: f32,
    /// **Flatten Amount** — PlanePush scale (legacy 0.5). Default 0.5.
    pub flatten_amount: f32,
    /// **Volume Preservation** — Flatten softens in solid cores 0..1. Default 0.0.
    pub volume_preservation: f32,
    /// **Accumulate** — additive PlaneFill (true) vs Max blend (false). Default true.
    pub accumulate: bool,
    /// **Inflate Speed** — multiplier on Inflate density delta. Default 1.0.
    pub inflate_speed: f32,
    /// **Inflate Direction** — Gradient / Surface / Radial (`InflateDirection` as u32).
    pub inflate_direction: u32,
    /// **Thickness Clamp** — max shell density band for Inflate 0..1 (1 = no clamp).
    pub thickness_clamp: f32,
    /// **Pinch Width** — gather offset scale (legacy 1.0 = 0.5×radius). Default 1.0.
    pub pinch_width: f32,
    /// **Pinch Amount** — gather blend weight. Default 1.0.
    pub pinch_amount: f32,
    /// **Crease Width** — groove falloff width. Default 1.0.
    pub crease_width: f32,
    /// **Crease Depth** — groove subtract strength. Default 1.0.
    pub crease_depth: f32,
    /// **Trim Plane Angle** — cut threshold as ×radius. Default 0.
    pub plane_angle: f32,
    /// **Cut Depth** — Trim/Scrape subtract strength. Default 1.0.
    pub cut_depth: f32,
    /// **Fill Holes** — Trim also seals keep-side. Default false.
    pub fill_holes: bool,
    // ── Brush Dynamics (Photoshop/ZBrush-style dab randomization) ──────────
    /// **Jitter** — small position noise in the stamp plane as ×radius (0..1). Default 0.
    pub dyn_jitter: f32,
    /// **Scatter** — larger position scatter as ×radius (0..2). Default 0.
    pub dyn_scatter: f32,
    /// **Random Rotation** — tip alpha rotation range in turns (0..1 = ±180°). Default 0.
    pub dyn_random_rotation: f32,
    /// **Random Strength** — ± strength variation 0..1. Default 0.
    pub dyn_random_strength: f32,
    /// **Random Radius** — ± radius variation 0..1. Default 0.
    pub dyn_random_radius: f32,
    /// **Stroke Noise** — low-frequency path noise as ×radius (0..1). Default 0.
    pub dyn_stroke_noise: f32,
    /// Auto Mask feature mode (Off / Curv / Peak / Cavity / Flat). TOOL state — system knob.
    pub auto_mask: core_math::voxel_sculpt::AutoMaskMode,
    /// Auto Mask strength 0..1 (how hard features lock). Default 0.75 when mode is on.
    pub auto_mask_intensity: f32,
    /// Invert auto-mask feature response (protect flats when Peak, etc.).
    pub auto_mask_invert: bool,
    /// Stack +Gather (Pinch advect) after the base op — Flatten+Gather, Draw+Gather, etc.
    pub stack_gather: bool,
    /// Stack +Flatten (PlanePush) after the base op — Pinch+Flatten, Fill+Flatten, etc.
    pub stack_flatten: bool,
    /// Stack +Fill (cavity PlaneFill) after the base op.
    pub stack_fill: bool,
    /// Stack +Crease (subtract groove) after the base op — Draw+Crease, etc.
    pub stack_crease: bool,
    /// Stack +Inflate after the base op.
    pub stack_inflate: bool,
}

/// MESH-VERTEX GRAB state for the voxel-sculpt MOVE tool (Meta-Medium mesh-deform → SDF round-trip).
///
/// Why this exists: the legacy Move warps the voxel DENSITY field (`elastic_grab` /
/// `elastic_grab_preview`'s semi-Lagrangian gather), which mathematically CANNOT pull material past
/// the brush radius — the gather source `p - drag*falloff` folds back inside the footprint, so a long
/// pull vanishes/creases instead of stretching. Meta Medium hit the same wall with direct-SDF and
/// switched to: deform a TRIANGLE MESH, then convert the deformed mesh back to SDF. We do the same.
///
/// Phase 1 (live, during the drag): snapshot the ORIGINAL vertices of every chunk with a vertex within
/// `outer_r` of `anchor`, then each frame displace each snapshot vertex `v0` by
/// `f * drag` with `f = smoothstep(outer_r, inner_r, |v0 - anchor|)` (edge0=outer > edge1=inner ⇒ f=1
/// inside `inner_r`, 0 outside `outer_r`) — an UNBOUNDED, LINEAR, stretching grab. Display-only: NO
/// density mutation and NO undo push during the drag.
///
/// Phase 2 (release): gather the whole object's current mesh, re-voxelize the swept AABB back to
/// density via `io_bridge::region_mesh_to_sdf`, record ONE sculpt-undo delta, and full-remesh.
#[derive(Default)]
pub(crate) struct MeshGrab {
    /// True once latched on the first drag frame of a Move stroke; cleared on release / cancel.
    pub active: bool,
    /// Grab anchor — the surface contact point at stroke start (voxel-LOCAL space). The drag is
    /// measured against this; the falloff is centred here.
    pub anchor: glam::Vec3,
    /// Surface normal at the hit (voxel-LOCAL), latched for the grab frame. Translation-only Move does
    /// not use it, but it is captured for completeness / a future rotation grab.
    pub normal: glam::Vec3,
    /// Inner falloff radius — vertices within this distance of `anchor` move by the full `drag` (f=1).
    pub inner_r: f32,
    /// Outer falloff radius — vertices beyond this distance do not move (f=0). The captured chunk set
    /// is exactly the chunks with a vertex within `outer_r` of `anchor`.
    pub outer_r: f32,
    /// View-plane normal latched at stroke start (voxel-LOCAL): the camera ray dir through the grab
    /// point. The per-frame `drag` is the cursor ray intersected with the plane (`anchor`, `plane_n`),
    /// so the grab follows the mouse in the VIEW PLANE — a screen-space pull (same projection the
    /// legacy `elastic_grab` uses). Translation form: `drag = NewTransform.translation − anchor`.
    pub plane_n: glam::Vec3,
    /// The most recent drag vector (voxel-LOCAL), updated every preview frame. Phase 2 (release) reads it
    /// to size the swept AABB (`anchor ± outer_r`, expanded by `drag`) and to displace the gathered mesh.
    pub last_drag: glam::Vec3,
}

impl MeshGrab {
    /// True if a mesh grab is in progress.
    pub fn is_active(&self) -> bool { self.active }

    /// Latch a fresh grab at `anchor`/`normal` (voxel-LOCAL) with the falloff radii. The live preview is the
    /// GPU vertex shader (no CPU snapshot); the release commit re-meshes the whole object fresh.
    pub fn begin(&mut self, anchor: glam::Vec3, normal: glam::Vec3, inner_r: f32, outer_r: f32, plane_n: glam::Vec3) {
        self.anchor = anchor;
        self.normal = normal.normalize_or_zero();
        self.inner_r = inner_r;
        self.outer_r = outer_r.max(1e-4); // soft-round uses outer only; keep positive
        self.plane_n = plane_n.normalize_or_zero();
        self.last_drag = glam::Vec3::ZERO;
        self.active = true;
    }

    /// Soft round tip weight at `dist` from an anchor — MUST match `gbuffer.wgsl` Move falloff and
    /// the bake worker (`grab_soft_round_falloff`). `inner_r` is unused for the curve (kept for
    /// uniform packing / older data); falloff is pure soft-round over `outer_r`.
    #[inline]
    pub fn plateau(&self, dist: f32) -> f32 {
        core_math::voxel_sculpt::grab_soft_round_falloff(dist, self.outer_r)
    }

    /// Local-space `(anchor, drag)` instances for the enabled SYMMETRY axes: 1 (no symmetry) up to 8
    /// (X+Y+Z). Reflects the set across each enabled local axis-0 plane (the grid is centred on its node),
    /// matching the per-stamp symmetry octant convention so Move mirrors exactly like every other brush.
    /// Both the GPU live preview (uploaded to `CameraUniforms.grab_*`) and the release commit iterate these.
    pub fn mirror_instances(&self, symmetry: [bool; 3]) -> Vec<(glam::Vec3, glam::Vec3)> {
        let mut out = vec![(self.anchor, self.last_drag)];
        for axis in 0..3 {
            if !symmetry[axis] { continue; }
            let mirrored: Vec<_> = out.iter().map(|&(a, d)| {
                let (mut aa, mut dd) = (a.to_array(), d.to_array());
                aa[axis] = -aa[axis];
                dd[axis] = -dd[axis];
                (glam::Vec3::from_array(aa), glam::Vec3::from_array(dd))
            }).collect();
            out.extend(mirrored);
        }
        out.truncate(8); // shader/uniform hold at most 8 (all three axes)
        out
    }

    /// Drop the grab without committing (cancel / no movement).
    pub fn cancel(&mut self) { self.active = false; }
}

/// ONE accumulated deferred-voxelize ("surface mode") Move instance — a completed grab held in
/// `VoxelObject::pending_move_grabs` until a Voxelize flush bakes it into the density grid (3D-Coat's
/// "voxelize immediately = OFF" model). All fields are voxel-LOCAL and ALREADY symmetry-expanded: one
/// `PendingGrab` per mirror octant (the release path pushes `MeshGrab::mirror_instances` entries). The
/// G-buffer vertex shader displays the stack live (each is uploaded as a `CameraUniforms.grab_*` instance),
/// so a deferred Move release shows the moved mesh persisting with NO re-voxelize pause. Per-instance radii
/// (vs one global pair) let moves made at different brush sizes compose correctly — both the shader preview
/// and the CPU re-voxelize evaluate each grab with its OWN `inner_r`/`outer_r` (`MeshGrab::plateau` curve).
#[derive(Clone, Copy)]
pub(crate) struct PendingGrab {
    /// Grab anchor (voxel-LOCAL) — the falloff is centred here; the displacement tapers from it.
    pub anchor: glam::Vec3,
    /// Displacement applied at the core (voxel-LOCAL), weighted by `smoothstep(outer_r, inner_r, dist)`.
    pub drag: glam::Vec3,
    /// Inner falloff radius (full-drag plateau within this distance of `anchor`).
    pub inner_r: f32,
    /// Outer falloff radius (no displacement beyond this distance of `anchor`).
    pub outer_r: f32,
}

/// ASYNC Move voxelize in flight (see `VoxelObject::pending_move_bake`). Worker owns a grid clone +
/// instances; main keeps mesh preview via `pending_move_grabs` and applies density writes on land.
pub(crate) struct PendingMoveBake {
    /// Worker → main: exactly one `MoveBakeResult` is sent, then the sender drops.
    pub rx: std::sync::mpsc::Receiver<MoveBakeResult>,
    /// How many LEADING `pending_move_grabs` entries this bake covers — drained on land. (Move
    /// strokes may append new entries while baking; they stay for the NEXT flush.)
    pub baked_n: usize,
    /// Whether the landed density delta pushes a sculpt-undo entry (false when a full pre-grow
    /// `pre_grid` snapshot is used instead — undoing that restores res + pre-move shape).
    pub push_undo: bool,
    /// The grid GREW at spawn → on land, spawn the whole-surface off-screen mesh rebuild.
    pub grew: bool,
    /// Pre-grow (and pre-move) full grid snapshot. When set, land pushes `UndoKind::Grid` so Ctrl+Z
    /// restores the previous resolution AND shape in one step (owner: undo on resolution change).
    pub pre_grid: Option<core_math::voxel_sculpt::VoxelGridRecord>,
}

/// Result of the ASYNC Move voxelize worker (density-only; no mesh→SDF).
pub(crate) struct MoveBakeResult {
    /// Dirty AABB (grid cells) for remesh marks.
    pub union: [u32; 6],
    /// Unused (legacy mesh→SDF slot kept empty). Density path only.
    pub sdf: Vec<f32>,
    /// Unused on density path (legacy iso band width).
    pub w: f32,
    /// Per-voxel pre/post density after warp + redistance. Land writes `post`, undo stores `pre`.
    pub density_writes: Vec<(u32, f32, f32)>,
    /// Guard: grid res/bounds at spawn.
    pub res: u32,
    pub bounds: f32,
    /// Per-instance swept AABBs — re-mesh marks on land.
    pub inst_aabbs: Vec<[u32; 6]>,
}

/// Full per-brush tool state for the brush system (not a tiny subset).
/// When "Per-brush settings" is ON, each `VoxelBrush` base op stores its own copy of these knobs.
/// Named `VoxelBrushSettings` to avoid clashing with `core_math::sdf_volume::BrushSettings`.
#[derive(Clone, Copy)]
pub(crate) struct VoxelBrushSettings {
    pub radius: f32,
    pub strength: f32,
    pub falloff: f32,
    pub falloff_preset: core_math::voxel_sculpt::FalloffPreset,
    pub brush_dir: core_math::voxel_sculpt::BrushDirection,
    /// true = Add, false = Remove (Ctrl flips temporarily).
    pub brush_add: bool,
    pub dir_average_region: f32,
    pub dir_smooth_normal: f32,
    pub stroke_spacing: f32,
    pub lazy: f32,
    pub pinch_intensity: f32,
    pub auto_smooth: bool,
    pub auto_smooth_intensity: f32,
    pub surface_extrude: bool,
    pub sculpt_on_surface: bool,
    pub layer_height: f32,
    pub layer_gain: f32,
    pub plane_offset: f32,
    pub plane_bias: f32,
    pub build_rate: f32,
    pub height_limit: f32,
    pub flatten_amount: f32,
    pub volume_preservation: f32,
    pub accumulate: bool,
    pub inflate_speed: f32,
    pub inflate_direction: u32,
    pub thickness_clamp: f32,
    pub pinch_width: f32,
    pub pinch_amount: f32,
    pub crease_width: f32,
    pub crease_depth: f32,
    pub plane_angle: f32,
    pub cut_depth: f32,
    pub fill_holes: bool,
    pub dyn_jitter: f32,
    pub dyn_scatter: f32,
    pub dyn_random_rotation: f32,
    pub dyn_random_strength: f32,
    pub dyn_random_radius: f32,
    pub dyn_stroke_noise: f32,
    pub auto_mask: core_math::voxel_sculpt::AutoMaskMode,
    pub auto_mask_intensity: f32,
    pub auto_mask_invert: bool,
    pub stack_gather: bool,
    pub stack_flatten: bool,
    pub stack_fill: bool,
    pub stack_crease: bool,
    pub stack_inflate: bool,
    pub pressure_radius_amount: f32,
    pub tip_follow_stroke: bool,
    pub tip_orient: u32,
    pub tip_angle: f32,
    pub tip_contrast: f32,
    pub front_faces_only: bool,
    pub move_elasticity: f32,
    pub move_pull_strength: f32,
    pub move_connected_only: bool,
}

impl Default for VoxelBrushSettings {
    fn default() -> Self {
        use core_math::voxel_sculpt::{BrushDirection, FalloffPreset};
        Self {
            radius: 0.15,
            // 0.5 = Blender's default brush strength. 1.0 made every fresh object's first
            // stroke deposit at double the factory feel (owner 2026-08-19: "10x too strong",
            // together with the surface path ignoring Accumulate-OFF — see displace_verts_for_dabs).
            strength: 0.5,
            falloff: 0.0,
            falloff_preset: FalloffPreset::Smooth,
            brush_dir: BrushDirection::Surface,
            brush_add: true,
            dir_average_region: 1.0,
            dir_smooth_normal: 0.0,
            stroke_spacing: 0.10,
            lazy: 0.0,
            pinch_intensity: 1.0,
            auto_smooth: false,
            auto_smooth_intensity: 0.35,
            surface_extrude: false,
            sculpt_on_surface: true,
            layer_height: 0.14,
            layer_gain: 0.22,
            plane_offset: 0.10,
            plane_bias: 0.0,
            build_rate: 2.0,
            height_limit: 1.0,
            flatten_amount: 0.5,
            volume_preservation: 0.0,
            accumulate: true,
            inflate_speed: 1.0,
            inflate_direction: 0,
            thickness_clamp: 1.0,
            pinch_width: 1.0,
            pinch_amount: 1.0,
            crease_width: 1.0,
            crease_depth: 1.0,
            plane_angle: 0.0,
            cut_depth: 1.0,
            fill_holes: false,
            dyn_jitter: 0.0,
            dyn_scatter: 0.0,
            dyn_random_rotation: 0.0,
            dyn_random_strength: 0.0,
            dyn_random_radius: 0.0,
            dyn_stroke_noise: 0.0,
            auto_mask: core_math::voxel_sculpt::AutoMaskMode::Off,
            auto_mask_intensity: 0.75,
            auto_mask_invert: false,
            stack_gather: false,
            stack_flatten: false,
            stack_fill: false,
            stack_crease: false,
            stack_inflate: false,
            pressure_radius_amount: 0.60,
            tip_follow_stroke: false,
            tip_orient: 0,
            tip_angle: 0.0,
            tip_contrast: 1.0,
            front_faces_only: false,
            move_elasticity: 1.0,
            move_pull_strength: 1.0,
            move_connected_only: false,
        }
    }
}

#[inline]
fn voxel_resolution_uses_live_raymarch(is_sparse: bool, res: u32) -> bool {
    is_sparse && res > 512
}

#[cfg(test)]
mod viewport_contract_tests {
    use super::voxel_resolution_uses_live_raymarch;

    #[test]
    fn raster_owns_512_and_raymarch_owns_only_above_512() {
        assert!(!voxel_resolution_uses_live_raymarch(true, 512));
        assert!(voxel_resolution_uses_live_raymarch(true, 513));
        assert!(voxel_resolution_uses_live_raymarch(true, 768));
        assert!(!voxel_resolution_uses_live_raymarch(false, 1024));
    }
}

impl VoxelObject {
    /// Snapshot all system knobs into a `VoxelBrushSettings` POD (for per-brush memory / Save As seed).
    pub fn capture_brush_settings(&self) -> VoxelBrushSettings {
        VoxelBrushSettings {
            radius: self.radius,
            strength: self.strength,
            falloff: self.falloff,
            falloff_preset: self.falloff_preset,
            brush_dir: self.brush_dir,
            brush_add: self.brush_add,
            dir_average_region: self.dir_average_region,
            dir_smooth_normal: self.dir_smooth_normal,
            stroke_spacing: self.stroke_spacing,
            lazy: self.lazy,
            pinch_intensity: self.pinch_intensity,
            auto_smooth: self.auto_smooth,
            auto_smooth_intensity: self.auto_smooth_intensity,
            surface_extrude: self.surface_extrude,
            sculpt_on_surface: self.sculpt_on_surface,
            layer_height: self.layer_height,
            layer_gain: self.layer_gain,
            plane_offset: self.plane_offset,
            plane_bias: self.plane_bias,
            build_rate: self.build_rate,
            height_limit: self.height_limit,
            flatten_amount: self.flatten_amount,
            volume_preservation: self.volume_preservation,
            accumulate: self.accumulate,
            inflate_speed: self.inflate_speed,
            inflate_direction: self.inflate_direction,
            thickness_clamp: self.thickness_clamp,
            pinch_width: self.pinch_width,
            pinch_amount: self.pinch_amount,
            crease_width: self.crease_width,
            crease_depth: self.crease_depth,
            plane_angle: self.plane_angle,
            cut_depth: self.cut_depth,
            fill_holes: self.fill_holes,
            dyn_jitter: self.dyn_jitter,
            dyn_scatter: self.dyn_scatter,
            dyn_random_rotation: self.dyn_random_rotation,
            dyn_random_strength: self.dyn_random_strength,
            dyn_random_radius: self.dyn_random_radius,
            dyn_stroke_noise: self.dyn_stroke_noise,
            auto_mask: self.auto_mask,
            auto_mask_intensity: self.auto_mask_intensity,
            auto_mask_invert: self.auto_mask_invert,
            stack_gather: self.stack_gather,
            stack_flatten: self.stack_flatten,
            stack_fill: self.stack_fill,
            stack_crease: self.stack_crease,
            stack_inflate: self.stack_inflate,
            pressure_radius_amount: self.pressure_radius_amount,
            tip_follow_stroke: self.tip_follow_stroke,
            tip_orient: self.tip_orient,
            tip_angle: self.tip_angle,
            tip_contrast: self.tip_contrast,
            front_faces_only: self.front_faces_only,
            move_elasticity: self.move_elasticity,
            move_pull_strength: self.move_pull_strength,
            move_connected_only: self.move_connected_only,
        }
    }

    /// Apply a full settings POD to live tool state.
    pub fn apply_brush_settings(&mut self, s: VoxelBrushSettings) {
        self.radius = s.radius;
        self.strength = s.strength;
        self.falloff = s.falloff;
        self.falloff_preset = s.falloff_preset;
        self.brush_dir = s.brush_dir;
        self.brush_add = s.brush_add;
        self.dir_average_region = s.dir_average_region.clamp(0.25, 4.0);
        self.dir_smooth_normal = s.dir_smooth_normal.clamp(0.0, 1.0);
        self.stroke_spacing = s.stroke_spacing;
        self.lazy = s.lazy;
        self.pinch_intensity = s.pinch_intensity;
        self.auto_smooth = s.auto_smooth;
        self.auto_smooth_intensity = s.auto_smooth_intensity;
        self.surface_extrude = s.surface_extrude;
        self.sculpt_on_surface = s.sculpt_on_surface;
        self.layer_height = s.layer_height;
        self.layer_gain = s.layer_gain;
        self.plane_offset = s.plane_offset;
        self.plane_bias = s.plane_bias;
        self.build_rate = s.build_rate;
        self.height_limit = s.height_limit;
        self.flatten_amount = s.flatten_amount;
        self.volume_preservation = s.volume_preservation;
        self.accumulate = s.accumulate;
        self.inflate_speed = s.inflate_speed;
        self.inflate_direction = s.inflate_direction;
        self.thickness_clamp = s.thickness_clamp;
        self.pinch_width = s.pinch_width;
        self.pinch_amount = s.pinch_amount;
        self.crease_width = s.crease_width;
        self.crease_depth = s.crease_depth;
        self.plane_angle = s.plane_angle;
        self.cut_depth = s.cut_depth;
        self.fill_holes = s.fill_holes;
        self.dyn_jitter = s.dyn_jitter;
        self.dyn_scatter = s.dyn_scatter;
        self.dyn_random_rotation = s.dyn_random_rotation;
        self.dyn_random_strength = s.dyn_random_strength;
        self.dyn_random_radius = s.dyn_random_radius;
        self.dyn_stroke_noise = s.dyn_stroke_noise;
        self.auto_mask = s.auto_mask;
        self.auto_mask_intensity = s.auto_mask_intensity;
        self.auto_mask_invert = s.auto_mask_invert;
        self.stack_gather = s.stack_gather;
        self.stack_flatten = s.stack_flatten;
        self.stack_fill = s.stack_fill;
        self.stack_crease = s.stack_crease;
        self.stack_inflate = s.stack_inflate;
        self.pressure_radius_amount = s.pressure_radius_amount;
        // Prefer tip_orient; migrate legacy tip_follow_stroke=true → FollowStroke.
        let mut orient = s.tip_orient.min(3);
        if s.tip_follow_stroke && orient == TipOrientation::Fixed.as_u32() {
            orient = TipOrientation::FollowStroke.as_u32();
        }
        self.tip_orient = orient;
        self.tip_follow_stroke = orient == TipOrientation::FollowStroke.as_u32();
        self.tip_angle = s.tip_angle.rem_euclid(360.0);
        self.tip_contrast = s.tip_contrast;
        self.front_faces_only = s.front_faces_only;
        self.move_elasticity = s.move_elasticity;
        self.move_pull_strength = s.move_pull_strength;
        self.move_connected_only = s.move_connected_only;
    }

    /// Industry / starter factory settings for **every** base op.
    /// **Radius is always kept from `base`** (user-controlled). All knobs live in the brush system.
    pub fn factory_settings_for(
        brush: core_math::voxel_sculpt::VoxelBrush,
        base: VoxelBrushSettings,
    ) -> VoxelBrushSettings {
        use core_math::voxel_sculpt::{BrushDirection, FalloffPreset, VoxelBrush};
        let mut s = base;
        // Shared clean slate for dynamics (per-brush can re-enable).
        s.dyn_jitter = 0.0;
        s.dyn_scatter = 0.0;
        s.dyn_random_rotation = 0.0;
        s.dyn_random_strength = 0.0;
        s.dyn_random_radius = 0.0;
        s.dyn_stroke_noise = 0.0;
        s.stack_gather = false;
        s.stack_flatten = false;
        s.stack_fill = false;
        s.stack_crease = false;
        s.stack_inflate = false;
        s.surface_extrude = false;
        s.sculpt_on_surface = true; // ZBrush/Blender default: stick to the mesh
        s.tip_contrast = 1.0;
        s.tip_follow_stroke = false;
        s.tip_orient = TipOrientation::Fixed.as_u32();
        s.tip_angle = 0.0;
        s.front_faces_only = false;
        s.move_elasticity = 1.0;
        s.move_pull_strength = 1.0;
        s.move_connected_only = false;
        s.pressure_radius_amount = 0.60;
        // Direction fine-controls (Average Region / Smooth Normal) — keep sensible defaults.
        s.dir_average_region = 1.0;
        s.dir_smooth_normal = 0.0;
        s.brush_add = true; // default Add; Erase/Scrape/Crease override to Remove
        s.height_limit = 1.0; // off — Flatten/Build may lower; never inherit a dead ceiling

        match brush {
            // Reliable Draw factory (no experimental direction/spacing hacks).
            VoxelBrush::Draw => {
                s.strength = 0.40;
                s.stroke_spacing = 0.10;
                s.falloff = 0.20;
                s.falloff_preset = FalloffPreset::Smooth;
                // Blender stroke model (2026-07-23): deposit along the LIVE per-dab surface
                // normal so the stroke wraps curvature. Original (FirstPoint) stays an option.
                s.brush_dir = BrushDirection::Surface;
                s.dir_average_region = 1.0;
                s.brush_add = true;
                s.auto_smooth = false;
                // Soft dual approach by default — Add matches Remove; Accu ON = mild residual pile.
                s.accumulate = false;
                s.height_limit = 1.0;
            }
            VoxelBrush::Erase => {
                s.strength = 0.45;
                s.stroke_spacing = 0.06;
                s.falloff = 0.20;
                s.falloff_preset = FalloffPreset::Smooth;
                s.brush_dir = BrushDirection::Surface; // carve follows the local form
                s.brush_add = false; // Remove by default (Ctrl → Add)
                s.auto_smooth = false;
            }
            VoxelBrush::Smooth => {
                s.strength = 0.55;
                s.stroke_spacing = 0.08;
                s.falloff = 0.0;
                s.falloff_preset = FalloffPreset::Smooth;
                s.brush_dir = BrushDirection::Surface;
                s.auto_smooth = false; // Smooth IS the relax
            }
            // Flatten polish (hPolish / Coat Flatten).
            VoxelBrush::Flatten => {
                s.strength = 0.48;
                s.falloff = 0.92;
                s.stroke_spacing = 0.04;
                s.falloff_preset = FalloffPreset::Sharp;
                s.accumulate = false;
                s.auto_smooth = false;
                s.plane_bias = 0.00;
                s.flatten_amount = 1.10;
                s.height_limit = 0.90;
                s.volume_preservation = 0.0;
                // Plane brush: AREA plane under the tip, re-evaluated as the stroke moves
                // (Blender flatten). Original lock stays an explicit option.
                s.brush_dir = BrushDirection::Average;
                s.front_faces_only = true;
            }
            // Clay plane fill — simple factory (plane offset 0.10, original normal).
            VoxelBrush::Clay => {
                s.strength = 0.42;
                s.stroke_spacing = 0.08;
                s.falloff = 0.50;
                s.falloff_preset = FalloffPreset::Smooth;
                s.accumulate = true;
                s.auto_smooth = false;
                s.plane_offset = 0.10;
                s.plane_bias = 0.0;
                s.brush_dir = BrushDirection::Average; // live area plane (Blender clay strips)
                s.dir_average_region = 1.0;
                s.front_faces_only = false;
                s.height_limit = 1.0;
            }
            VoxelBrush::Scrape => {
                s.strength = 0.45;
                s.stroke_spacing = 0.05;
                s.falloff = 0.80;
                s.falloff_preset = FalloffPreset::Sharp;
                s.plane_angle = 0.0;
                s.cut_depth = 1.1;
                s.fill_holes = false;
                s.brush_dir = BrushDirection::Average; // live area plane (Blender scrape)
                s.brush_add = false; // trim = Remove; Ctrl → inverse trim (build side)
                s.auto_smooth = false;
                s.front_faces_only = true;
            }
            // Dam Standard → Crease (thin tip + follow stroke).
            VoxelBrush::Crease => {
                s.strength = 0.50;
                s.stroke_spacing = 0.04;
                s.falloff = 0.96;
                s.falloff_preset = FalloffPreset::Sharp;
                s.accumulate = false;
                s.auto_smooth = false;
                s.plane_offset = 0.0;
                s.plane_bias = 0.0;
                s.volume_preservation = 0.0;
                s.brush_dir = BrushDirection::Surface;
                s.brush_add = false; // groove; Ctrl → ridge
                s.pressure_radius_amount = 0.20;
                s.tip_follow_stroke = true;
                s.tip_orient = TipOrientation::FollowStroke.as_u32();
                s.front_faces_only = true;
                s.crease_width = 0.80;
                s.crease_depth = 1.35;
            }
            VoxelBrush::Fill => {
                s.strength = 0.45;
                s.stroke_spacing = 0.08;
                s.falloff = 0.45;
                s.falloff_preset = FalloffPreset::Smooth;
                s.accumulate = true;
                s.plane_offset = 0.0;
                s.brush_dir = BrushDirection::Average; // live area plane (fill follows the form)
                s.dir_average_region = 1.0;
                s.auto_smooth = false;
                s.height_limit = 1.0;
            }
            // ZBrush Clay BuildUp — hard planar strips, Original normal, dense spacing, mild AS.
            VoxelBrush::ClayBuildup => {
                s.strength = 0.42;
                s.stroke_spacing = 0.05;
                s.falloff_preset = FalloffPreset::Blend;
                s.falloff = 0.88;
                s.accumulate = true;
                s.auto_smooth = true;
                s.auto_smooth_intensity = 0.05;
                s.plane_offset = 0.0;
                s.brush_dir = BrushDirection::Average; // live area plane per stroke step (Blender clay strips)
                s.volume_preservation = 0.0;
                s.plane_bias = 0.18;
                s.height_limit = 1.0;
                s.layer_height = 0.12;
                s.layer_gain = 0.28;
                s.build_rate = 2.6;
                s.pressure_radius_amount = 0.30;
                s.tip_follow_stroke = true;
                s.tip_orient = TipOrientation::FollowStroke.as_u32();
                s.tip_contrast = 0.90;
                s.front_faces_only = true;
            }
            VoxelBrush::Inflate => {
                s.strength = 0.35;
                s.stroke_spacing = 0.10;
                s.falloff = 0.25;
                s.falloff_preset = FalloffPreset::Smooth;
                s.inflate_speed = 1.0;
                s.inflate_direction = 0; // Gradient
                s.thickness_clamp = 1.0;
                s.brush_dir = BrushDirection::Surface;
                s.auto_smooth = false;
                s.volume_preservation = 0.0;
            }
            VoxelBrush::Pinch => {
                s.strength = 0.45;
                s.stroke_spacing = 0.08;
                s.falloff = 0.60;
                s.falloff_preset = FalloffPreset::Sharp;
                s.pinch_intensity = 1.25;
                s.pinch_width = 1.0;
                s.pinch_amount = 1.0;
                s.brush_dir = BrushDirection::Surface; // pinch plane follows the local form
                s.auto_smooth = false;
            }
            VoxelBrush::Move => {
                s.strength = 1.00;
                s.falloff = 0.55;
                s.falloff_preset = FalloffPreset::Smooth;
                s.stroke_spacing = 0.08;
                s.auto_smooth = false;
                s.move_elasticity = 1.00;
                s.move_pull_strength = 1.00;
                s.move_connected_only = true;
                s.front_faces_only = false;
            }
            VoxelBrush::Smudge => {
                s.strength = 0.55;
                s.falloff = 0.35;
                s.falloff_preset = FalloffPreset::Smooth;
                s.stroke_spacing = 0.10;
                s.auto_smooth = false;
                s.move_elasticity = 1.0;
                s.move_pull_strength = 1.0;
                s.front_faces_only = false;
            }
            VoxelBrush::SnakeHook => {
                // Surface-mode tube pull (Blender Snake Hook). Full-strength grab like Move; dragged
                // (no dab spacing); crease_pinch stored in pinch_amount (0.5 = OFF, the Blender
                // default). No autosmooth (would smear the tube).
                s.strength = 1.00;
                s.falloff = 0.55;
                s.falloff_preset = FalloffPreset::Smooth;
                s.stroke_spacing = 0.02;
                s.auto_smooth = false;
                s.move_elasticity = 1.00;
                s.move_pull_strength = 1.00;
                s.pinch_amount = 0.5; // crease_pinch default = off
                s.front_faces_only = false;
            }
        }
        s
    }

    /// Short display name for the factory recipe (UI Reset button).
    pub fn factory_label(brush: core_math::voxel_sculpt::VoxelBrush) -> &'static str {
        use core_math::voxel_sculpt::VoxelBrush;
        match brush {
            VoxelBrush::Draw => "Draw",
            VoxelBrush::Erase => "Erase",
            VoxelBrush::Smooth => "Smooth",
            VoxelBrush::Flatten => "Flatten",
            VoxelBrush::Clay => "Clay",
            VoxelBrush::Scrape => "Scrape",
            VoxelBrush::Crease => "Dam Standard",
            VoxelBrush::Fill => "Fill",
            VoxelBrush::ClayBuildup => "Clay BuildUp",
            VoxelBrush::Inflate => "Inflate",
            VoxelBrush::Pinch => "Pinch",
            VoxelBrush::Move => "Move/Grab",
            VoxelBrush::Smudge => "Smudge",
            VoxelBrush::SnakeHook => "Snake Hook",
        }
    }

    /// True if the current tip is a procedural factory alpha (safe to replace / clear).
    pub fn is_factory_tip_name(name: &str) -> bool {
        matches!(
            name,
            "" | "Square Clay Alpha" | "Square" | "Thin Line / Sharp Crease"
        )
    }

    /// Optional procedural tips for recipes that need them (Crease / Build / Flatten).
    /// **Not** applied on normal brush switch — tips force the CPU path. Use on explicit Reset
    /// with alphas, or when the user imports a tip.
    pub fn ensure_factory_tip(&mut self, brush: core_math::voxel_sculpt::VoxelBrush) {
        use core_math::voxel_sculpt::VoxelBrush;
        match brush {
            VoxelBrush::ClayBuildup => {
                if self.brush_tip.is_some() && !Self::is_factory_tip_name(&self.brush_tip_name) {
                    return;
                }
                self.brush_tip = Some(core_math::voxel_sculpt::BrushTipAlpha::square_clay(128, 0.12));
                self.brush_tip_name = "Square Clay Alpha".into();
                self.brush_tip_path = None;
            }
            VoxelBrush::Crease => {
                if self.brush_tip.is_some() && !Self::is_factory_tip_name(&self.brush_tip_name) {
                    return;
                }
                self.brush_tip = Some(core_math::voxel_sculpt::BrushTipAlpha::thin_line_crease(128, 0.055));
                self.brush_tip_name = "Thin Line / Sharp Crease".into();
                self.brush_tip_path = None;
            }
            VoxelBrush::Flatten => {
                if self.brush_tip.is_some() && !Self::is_factory_tip_name(&self.brush_tip_name) {
                    return;
                }
                self.brush_tip = Some(core_math::voxel_sculpt::BrushTipAlpha::square_clay(128, 0.06));
                self.brush_tip_name = "Square".into();
                self.brush_tip_path = None;
            }
            _ => {
                // Clear procedural factory tip so GPU path stays live when switching away.
                if Self::is_factory_tip_name(&self.brush_tip_name) {
                    self.brush_tip = None;
                    self.brush_tip_name.clear();
                    self.brush_tip_path = None;
                }
            }
        }
    }

    /// Apply the factory recipe for `brush` through the **shared brush system** (settings POD).
    /// Keeps **Radius**. Does **not** install tip alphas (keeps GPU path); use
    /// [`apply_brush_factory_with_tip`] for recipe alphas.
    pub fn apply_brush_factory(&mut self, brush: core_math::voxel_sculpt::VoxelBrush) {
        self.apply_brush_factory_ex(brush, false);
    }

    /// Like [`apply_brush_factory`], and install recipe tip alphas (forces CPU stamps while tip is set).
    pub fn apply_brush_factory_with_tip(&mut self, brush: core_math::voxel_sculpt::VoxelBrush) {
        self.apply_brush_factory_ex(brush, true);
    }

    fn apply_brush_factory_ex(&mut self, brush: core_math::voxel_sculpt::VoxelBrush, with_tip: bool) {
        use core_math::voxel_sculpt::VoxelBrush;
        let s = Self::factory_settings_for(brush, self.capture_brush_settings());
        self.apply_brush_settings(s);
        self.brush = brush;
        self.brush_settings.insert(brush, s);
        // Pressure routing for tablet-friendly industry recipes.
        match brush {
            VoxelBrush::ClayBuildup | VoxelBrush::Crease | VoxelBrush::Clay | VoxelBrush::Fill => {
                self.pressure_enabled = true;
                self.pressure_to_strength = true;
                self.pressure_to_radius = true;
            }
            VoxelBrush::Move | VoxelBrush::Smudge => {
                // Grab: pressure optional; leave user pressure master alone except elasticity is on.
            }
            _ => {}
        }
        if brush == VoxelBrush::Move {
            self.move_deferred = false; // Dynamic Topology ON
        }
        if with_tip {
            self.ensure_factory_tip(brush);
        } else if Self::is_factory_tip_name(&self.brush_tip_name) {
            // Drop previous factory tip so switching brushes does not stay on CPU.
            self.brush_tip = None;
            self.brush_tip_name.clear();
            self.brush_tip_path = None;
        }
    }

    /// Back-compat aliases used by older UI call sites.
    pub fn apply_clay_buildup_factory(&mut self) {
        self.apply_brush_factory_with_tip(core_math::voxel_sculpt::VoxelBrush::ClayBuildup);
    }
    pub fn apply_dam_standard_factory(&mut self) {
        self.apply_brush_factory_with_tip(core_math::voxel_sculpt::VoxelBrush::Crease);
    }
    pub fn apply_flatten_factory(&mut self) {
        self.apply_brush_factory(core_math::voxel_sculpt::VoxelBrush::Flatten);
    }
    pub fn apply_move_factory(&mut self) {
        self.apply_brush_factory(core_math::voxel_sculpt::VoxelBrush::Move);
    }

    /// True when per-voxel PAINT colour is in play for this body — Paint Color armed, painted voxels
    /// present, or mask-paint mode (writes the mask buffer on the CPU path). Protect-mask tint alone
    /// does NOT force this: the GPU box brush samples mask on device, and `needs_gpu_vertex_recolor`
    /// packs the viewport tint after mesh. Paint is dense-only; sparse always returns false here so
    /// the GPU brush/atlas paths stay live.
    pub fn cpu_paint_path(&self) -> bool {
        !self.grid.density.is_sparse()
            && (self.paint_color_on
                || !self.grid.colors.is_empty()
                || self.mask_paint)
    }

    /// Decision B (STEP 9): true when a CPU-forced (`cpu_paint_path`) stroke should still route its
    /// DENSITY + TOPOLOGY through the GPU mesher rather than CPU `mesh_region`, applying the per-voxel
    /// PAINT colour as a separate in-place vertex-colour write after the dispatch lands. This is the
    /// deposit/carve-while-painting case: a density (topology-changing) brush whose only CPU-forcing
    /// reason is that colour is involved. "CPU-forced" then means ONLY "colour is involved", never "CPU
    /// per-frame Surface-Nets re-mesh".
    ///
    /// EXCLUDED (they keep their own zero-remesh CPU paths, never the GPU mesher here):
    ///   - Move / Smudge: advect material coherently → STEP 7 reprojects positions in place.
    /// Those change no topology (or handle their own), so meshing them on the GPU would be waste
    /// or wrong. Dense-only (sparse strokes raymarch and never reach the mesher); requires gpu_mesh + a
    /// GPU context (checked at the call site) — else the CPU `mesh_region` fallback stands.
    pub fn gpu_mesh_paint_topology(&self) -> bool {
        use core_math::voxel_sculpt::VoxelBrush;
        self.cpu_paint_path()
            && !matches!(self.brush, VoxelBrush::Move | VoxelBrush::Smudge)
    }

    /// True when this body needs per-vertex colour packed after a GPU mesh lands (paint and/or
    /// protect-mask tint). Dense grids only — sparse displays tint by forcing the triangle mesh
    /// (raymarch off while mask is visible) and CPU-path colour packing on remesh.
    pub fn needs_gpu_vertex_recolor(&self) -> bool {
        !self.grid.density.is_sparse()
            && (self.paint_color_on
                || !self.grid.colors.is_empty()
                || self.grid.mask_visible())
    }

    /// True when the sparse raymarch overlay must yield to the triangle mesh (Move grab preview,
    /// deferred pending moves, or mask/paint tint that only the mesh path can show).
    pub fn prefers_mesh_over_raymarch(&self) -> bool {
        self.mesh_grab.is_active()
            || !self.pending_move_grabs.is_empty()
            || self.grid.mask_visible()
            || self.paint_color_on
            || !self.grid.colors.is_empty()
    }

    /// High-res sparse (≥512³): remesh cannot keep up (6586+ surface chunks @848 → tearing + 20ms/frame).
    /// Live look = atlas raymarch; triangle remesh is for Object leave / pick / shadows only (budgeted idle).
    #[inline]
    pub fn high_res_live_raymarch(&self) -> bool {
        voxel_resolution_uses_live_raymarch(self.grid.density.is_sparse(), self.grid.res)
    }

    /// Use sparse raymarch as the Sculpt-room surface (no mesh z-fight).
    ///
    /// When the **active stroke** is on the box/CPU path (`stroke_uses_atlas == Some(false)`),
    /// density lands on the CPU grid while raymarch still samples the atlas → stale surface.
    /// In that case we fall back to mesh remesh so clay stays live under the tip.
    #[inline]
    pub fn use_sculpt_raymarch_overlay(&self) -> bool {
        if !self.high_res_live_raymarch() || self.prefers_mesh_over_raymarch() {
            return false;
        }
        // Box-path stroke: remesh under the tip instead of frozen atlas raymarch.
        if self.stroke_uses_atlas == Some(false) {
            return false;
        }
        true
    }

    /// True when Pinch/Gather parameters affect this stamp (base is Pinch or +Gather ON).
    #[inline]
    pub fn pinch_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::Pinch) || self.stack_gather
    }
    /// True when Flatten parameters affect this stamp.
    #[inline]
    pub fn flatten_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::Flatten) || self.stack_flatten
    }
    /// True when Fill / plane-fill stack is active.
    #[inline]
    pub fn fill_active(&self) -> bool {
        matches!(
            self.brush,
            core_math::voxel_sculpt::VoxelBrush::Fill
                | core_math::voxel_sculpt::VoxelBrush::Clay
                | core_math::voxel_sculpt::VoxelBrush::ClayBuildup
        ) || self.stack_fill
    }
    /// True when Crease parameters affect this stamp.
    #[inline]
    pub fn crease_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::Crease) || self.stack_crease
    }
    /// True when Inflate parameters affect this stamp.
    #[inline]
    pub fn inflate_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::Inflate) || self.stack_inflate
    }
    /// True when Build layer knobs affect this stamp.
    #[inline]
    pub fn build_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::ClayBuildup)
    }
    /// True when Scrape/Trim knobs affect this stamp.
    #[inline]
    pub fn scrape_active(&self) -> bool {
        matches!(self.brush, core_math::voxel_sculpt::VoxelBrush::Scrape)
    }
    /// True when any dynamics knob is non-zero.
    #[inline]
    pub fn dynamics_active(&self) -> bool {
        self.dyn_jitter > 1e-6
            || self.dyn_scatter > 1e-6
            || self.dyn_random_rotation > 1e-6
            || self.dyn_random_strength > 1e-6
            || self.dyn_random_radius > 1e-6
            || self.dyn_stroke_noise > 1e-6
    }

    /// Switch the active base op to `next` through the **brush system**:
    /// - Always saves outgoing settings when `per_brush_settings` is ON.
    /// - First time a base op is selected → loads its **factory recipe** (radius kept).
    /// - Later visits restore that op's saved knobs (when per-brush is ON).
    /// Tips are **not** auto-installed on switch (keeps GPU path); use "Reset factory" + tip checkbox.
    pub fn set_active_brush(&mut self, next: core_math::voxel_sculpt::VoxelBrush) {
        if self.brush == next {
            return;
        }
        let first_time = !self.brush_settings.contains_key(&next);
        if self.per_brush_settings {
            self.brush_settings.insert(self.brush, self.capture_brush_settings());
        }
        if first_time {
            // Every base op gets a factory seed so system knobs are meaningful immediately.
            self.apply_brush_factory(next);
            return;
        }
        if self.per_brush_settings {
            if let Some(s) = self.brush_settings.get(&next).copied() {
                self.apply_brush_settings(s);
            }
        }
        // Clear procedural tip when leaving a tip-recipe brush without per-brush memory of a user tip.
        if Self::is_factory_tip_name(&self.brush_tip_name)
            && !matches!(
                next,
                core_math::voxel_sculpt::VoxelBrush::ClayBuildup
                    | core_math::voxel_sculpt::VoxelBrush::Crease
                    | core_math::voxel_sculpt::VoxelBrush::Flatten
            )
        {
            self.brush_tip = None;
            self.brush_tip_name.clear();
            self.brush_tip_path = None;
        }
        self.brush = next;
    }
}

/// Lightweight per-frame sculpt profiler. Splits the per-stroke CPU cost into BRUSH (density
/// writes via `apply_brush`) vs MESH (Surface-Nets re-mesh of dirty chunks via `mesh_region`),
/// plus counts, so we can see WHERE the time goes at a given resolution before deciding on GPU /
/// sparse work. Measured every frame (cost ~negligible — two `Instant::now()`); only DISPLAYED when
/// `enabled`. Values are "last frame that ran that phase"; peaks are rolling until reset.
#[derive(Default, Clone, Copy)]
pub(crate) struct SculptProfile {
    pub enabled: bool,
    pub brush_ms: f32,
    pub mesh_ms: f32,
    pub stamps: u32,
    pub chunks: u32,
    pub brush_peak: f32,
    pub mesh_peak: f32,
    /// DIAGNOSTIC: chunks waiting to re-mesh (dirty_chunks backlog). If this grows large during a fast
    /// stroke, the mesher can't keep up (catch-up tearing). If it stays small, tearing is elsewhere.
    pub dirty_backlog: u32,
    /// DIAGNOSTIC: stamps the interpolator produced THIS frame before the per-frame cap subsampled them.
    /// If `stamps_raw` >> `stamps`, the stroke is being subsampled (potential gaps between impressions).
    pub stamps_raw: u32,
    /// DIAGNOSTIC: GPU time (ms) of the mesh dispatch's two compute passes (from VoxelMeshGpu's own query
    /// set, one-frame latency). The key MESH-bound-vs-DRAW-bound signal: if this is small but the whole
    /// frame (`frame_ms`) is large at 1024³, the cost is the DRAW (12k draw calls), not the mesh.
    pub mesh_gpu_ms: f32,
    /// DIAGNOSTIC: whole previous-frame wall time (ms), snapshotted so the readout can compare it to
    /// `mesh_gpu_ms` without a second borrow of the app.
    pub frame_ms: f32,
}

/// Messages streamed from the rebuild worker thread, which does the WHOLE expensive resolution
/// change off the UI thread: the grid resample (the ~1s cost), the surface-chunk scan, and the
/// meshing. Order: `Total` (surface count) → each `Chunk` / `EmptyChunk` → `Grid` (moved last).
pub(crate) enum VoxelRebuildMsg {
    Total(usize),
    /// Non-empty Surface-Nets chunk ready for mesh_cache upload.
    Chunk(u32, Vec<[f32; 8]>, Vec<u32>),
    /// Surface-mask hit but mesh_region emitted nothing — still counts toward `done` (no upload).
    EmptyChunk(u32),
    Grid(core_math::voxel_sculpt::VoxelGrid),
}

/// Off-screen rebuild of the voxel at a new resolution. A WORKER THREAD resamples + meshes (so the
/// UI thread never stalls — neither on the resample nor the meshing) and streams results via `rx`;
/// the main thread only uploads each chunk into its own buffer while the old surface keeps
/// rendering, then swaps the whole grid/chunk set in atomically once everything has landed.
pub(crate) struct PendingVoxelRebuild {
    pub chunk_dim: u32,
    pub chunks: Vec<core_math::scene_graph::MeshCacheId>,
    pub chunk_tris: Vec<u32>,
    pub rx: std::sync::mpsc::Receiver<VoxelRebuildMsg>,
    /// The resampled grid (for the swap) — `None` until the worker sends it (last message).
    pub grid: Option<core_math::voxel_sculpt::VoxelGrid>,
    /// Surface-chunk count for the progress readout — `None` until the worker reports it.
    pub total: Option<usize>,
    /// Chunks received + uploaded so far.
    pub done: usize,
}
