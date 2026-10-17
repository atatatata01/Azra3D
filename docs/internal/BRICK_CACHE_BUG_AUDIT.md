# Brick Cache Bug Audit

**Date:** 2026-03-27
**Scope:** All brick pool + volume cache systems
**Goal:** Find every bug before building the cache tier system on top

---

## BUG-01: `remove_body()` does not deallocate from brick_pool

**File:** `app/src/body_volume_cache.rs:1144-1167`
**Severity:** LEAK
**Phase:** Introduced in Phase 1/2 brick implementation

When a body is deleted via `remove_body()`, its brick pool slots are never freed. The slots remain allocated in `brick_pool.body_bricks` and `brick_pool.body_last_used`, permanently reducing pool capacity.

```rust
pub(crate) fn remove_body(&mut self, body_id: NodeId) {
    self.body_grids.remove(&body_id);
    // ... cleans up generations, dirty_bodies, in_flight, etc ...
    self.volume_pool.release(body_id);
    // ⚠ MISSING: self.brick_pool.deallocate_body(body_id);
    // ⚠ MISSING: self.body_brick_volumes.remove(&body_id);
}
```

**Callers:** `gizmo.rs:237`, `input_keyboard.rs:855`, `outliner_panel.rs:171`, `prim_actions.rs:343` — all paths to body deletion.

**Fix:** Add `self.brick_pool.deallocate_body(body_id);` and `self.body_brick_volumes.remove(&body_id);` to `remove_body()`.

---

## BUG-02: `invalidate_all()` does not clear brick pool or body_brick_volumes

**File:** `app/src/body_volume_cache.rs:505-536`
**Severity:** LEAK
**Phase:** Introduced in Phase 1/2 brick implementation

`invalidate_all()` clears `body_grids`, `ram_cache`, `volume_pool`, etc. but does NOT clear `body_brick_volumes` or deallocate bricks from `brick_pool`. After `invalidate_all()`, the brick pool still holds allocated slots for bodies that are about to be rebaked with fresh data. The stale `BodyBrickVolume` entries reference pool slots that will be overwritten when `classify_bricks_for_body` runs for the same body again (it calls `deallocate_body` first), so this is a leak, not corruption — but the pool reports incorrect `used_bricks` until the bodies are re-classified.

**Callers:** `new_scene()` (project_new.rs:74), `load_project()` (project_load.rs:264), `set_params()` (body_volume_cache.rs:543).

```rust
pub(crate) fn invalidate_all(&mut self) {
    // ... existing cleanup ...
    // ⚠ MISSING: body_brick_volumes not cleared
    // ⚠ MISSING: brick_pool not cleared
}
```

**Fix:** Add the following after the existing cleanup:
```rust
let brick_nids: Vec<NodeId> = self.body_brick_volumes.keys().copied().collect();
for nid in brick_nids {
    self.brick_pool.deallocate_body(nid);
}
self.body_brick_volumes.clear();
```

---

## BUG-03: `invalidate_for_undo()` does not invalidate brick data

**File:** `app/src/body_volume_cache.rs:1287-1382`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 1/2 brick implementation

When undo detects per-body hash mismatches (lines 1319-1321), it removes the body's grid from `body_grids` and marks it dirty. But it does NOT deallocate the body's bricks from `brick_pool` or remove the `body_brick_volumes` entry. The stale brick data persists on GPU.

The stale brick indirection still maps to pool slots that may now contain wrong SDF values for the restored scene graph state. If the volume preview shader samples from bricks before the body is re-classified, it will read stale/incorrect SDF data.

**Also applies to:** `invalidate_for_undo_full()` (lines 1386-1436) — same omission.

**Fix:** For each invalidated body, add:
```rust
self.brick_pool.deallocate_body(nid);
self.body_brick_volumes.remove(&nid);
```

---

## BUG-04: `deallocate()` does not update `body_bricks` HashMap

**File:** `graphics/src/brick_pool.rs:90-96`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 1

The `deallocate(&mut self, slots: &[u32])` method returns slots to the free list and decrements `used_bricks`, but does NOT remove the slots from the `body_bricks` HashMap. Any body that previously owned these slots still has them listed in `body_bricks[body_nid]`.

This creates a state where:
- A slot is in BOTH `free_list` AND `body_bricks[some_nid]`
- `body_brick_count()` reports the wrong count
- A subsequent `allocate()` can hand out a slot that another body still "owns"
- `deallocate_body()` for the original owner would push the slot to free_list AGAIN → **duplicate free slots**

**Current usage:** `deallocate()` is only called in tests (`basic_alloc_dealloc` test at line 451). It is NOT called in production code — `deallocate_body()` is used instead. So this is not currently exploitable, but the API is a trap.

**Fix:** Either:
- Remove `deallocate()` (it's unused in production)
- Or scan `body_bricks` to remove the deallocated slots

---

## BUG-05: `cpu_data` wastes memory proportional to max pool slot index

**File:** `app/src/body_volume_cache.rs:1998-2005`
**Severity:** LEAK (memory waste, not a true leak)
**Phase:** Introduced in Phase 2

```rust
let max_slot = slots.iter().copied().max().unwrap_or(0) as usize;
let mut cpu_data = vec![100.0f32; (max_slot + 1) * bv];
```

If the pool has 65,536 capacity and this body's bricks happen to get slot index 60,000, `cpu_data` allocates `60,001 * 512 * 4 = ~117 MB` for a body that might only have 500 active bricks (~1 MB of actual data). The allocation is indexed by pool slot, creating huge sparse gaps.

**Worst case:** 50 bodies, each gets some high-index slots → each body's `cpu_data` is ~117 MB → 5.8 GB total RAM for data that should be ~50 MB.

**Fix:** Store `cpu_data` indexed by sequential brick index (0, 1, 2, ...) instead of pool slot index. The `BrickGridAccessor` would need a slot→sequential lookup table (or the indirection table itself provides this mapping).

---

## BUG-06: `classify_bricks_for_body()` can evict the body it is about to classify

**File:** `app/src/body_volume_cache.rs:1938-1946`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 2

```rust
let slots = match self.brick_pool.allocate_with_eviction(
    body_nid, n, body_nid, // protect self during alloc
) {
```

The `protect` parameter is `body_nid` — the body being classified. This looks correct because `deallocate_body(body_nid)` was already called on line 1934, so this body has no bricks to evict. However, if `allocate_with_eviction` evicts OTHER bodies, those bodies' `BodyBrickVolume` entries in `body_brick_volumes` are NOT cleaned up — only the `brick_pool.body_bricks` entry is removed. The `body_brick_volumes` entry for the evicted body will have stale indirection pointing to freed (and now reallocated) pool slots.

**Impact:** If the evicted body's brick volume is later read (e.g., by the volume preview shader or DC extractor), the indirection table points to slots now owned by a different body → **incorrect SDF data read**.

**Fix:** `classify_bricks_for_body` must also remove `body_brick_volumes` entries for any body that `allocate_with_eviction` evicts. This requires `BrickPool` to report which bodies were evicted.

---

## BUG-07: `allocate_with_eviction()` does not report evicted bodies

**File:** `graphics/src/brick_pool.rs:182-199`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 1

```rust
pub fn allocate_with_eviction(
    &mut self, body_nid: NodeId, n: u32, protect: NodeId,
) -> Option<Vec<u32>> {
    // ...
    while self.free_bricks() < n {
        if self.evict_lru(protect).is_none() {
            return None;
        }
    }
    self.allocate(body_nid, n)
}
```

The function silently evicts bodies in the loop but does not return which bodies were evicted. The caller (`classify_bricks_for_body`) has no way to know which `body_brick_volumes` entries became stale. This is the root cause of BUG-06.

**Fix:** Change return type to `Option<(Vec<u32>, Vec<NodeId>)>` where the second element is the list of evicted NodeIds. Or add a callback parameter.

---

## BUG-08: `evict_lru()` can fail silently when only protected body remains

**File:** `graphics/src/brick_pool.rs:114-121`
**Severity:** MINOR
**Phase:** Introduced in Phase 1

If the only body in the pool is the protected body, `evict_lru()` returns `None`. The `allocate_with_eviction()` loop then returns `None` — allocation fails. This is correct behavior but the failure is silent. The only log is line 1943: `"Failed to allocate N bricks"`. No information about WHY (protected body occupies all space).

**Fix:** Add diagnostic eprintln when eviction fails due to protection.

---

## BUG-09: `BodyBrickVolume` indirection buffer not invalidated on body_brick_volumes remove

**File:** `graphics/src/brick_pool.rs:222` (indirection_buffer field)
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 2

When a `BodyBrickVolume` is removed from `body_brick_volumes` (currently never happens in production due to BUG-01/02/03, but should happen), the `indirection_buffer: Option<wgpu::Buffer>` is dropped. This is fine — wgpu buffers are reference-counted and drop safely.

However, if a bind group still references this buffer (e.g., `volume_preview_bg`), the bind group holds a reference, keeping the buffer alive but stale. The next frame's render will use the stale indirection.

**Impact:** Limited because `viewport_render.rs:952-960` rebuilds the bind group reference each frame from `body_brick_volumes.get(&active_body_nid)`. If the entry is removed, it falls back to `brick_pool_dummy_buf`. The issue would only manifest if the bind group is cached across frames AND the entry is removed mid-frame.

**Fix:** Set `gpu.volume_preview_bg = None` whenever `body_brick_volumes` is modified (already done in `poll_body_vol_cache` line 288, but not in other paths).

---

## BUG-10: `classify_bricks_for_body()` uploads to GPU even when `queue` is None

**File:** `app/src/body_volume_cache.rs:1981-1993`
**Severity:** MINOR
**Phase:** Introduced in Phase 2

The upload block is correctly guarded by `if let Some(q) = queue`, so no crash. But if `queue` is None (no GPU available), the CPU-side data structures are still built and stored. The `bbv.cpu_data` and `bbv.indirection` are valid, but the GPU SSBO contains stale/uninitialized data for these slots. If the GPU later becomes available and the preview shader reads from these slots without re-upload, it reads garbage.

**Impact:** Very unlikely in practice — `queue` is always available when GPU is present. But if `init_gpu()` hasn't been called yet and `buffer()` returns `None`, the upload is silently skipped (line 1982 `if let Some(buf)`).

**Fix:** Mark the BodyBrickVolume as "needs GPU upload" when the upload is skipped, and retry on next frame when GPU is available.

---

## BUG-11: `volume_preview_bg` not always invalidated after brick data changes

**File:** `app/src/app_poll.rs:287-289`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 2

```rust
if self.body_vol_cache.use_brick_pool {
    if let Some(gpu) = &mut self.gpu { gpu.volume_preview_bg = None; }
}
```

This invalidation only runs for bodies in the `baked_bodies` list (from `poll()`). If a body's bricks are reclassified through a different path (e.g., `classify_all_cached_bodies`), the bind group is not invalidated. The shader would continue reading from the old bind group, which may reference the old indirection buffer.

**Fix:** Always set `gpu.volume_preview_bg = None` after any call to `classify_bricks_for_body()`.

---

## BUG-12: Race condition — async bake result arrives for deleted body

**File:** `app/src/body_volume_cache.rs:976-1001` and `app/src/app_poll.rs:277-290`
**Severity:** CORRECTNESS
**Phase:** Pre-existing (volume cache), extends to brick pool

Flow:
1. Body A dispatches async bake (enters `in_flight`)
2. User deletes body A → `remove_body(A)` called
3. `remove_body` clears `body_grids[A]`, but does NOT clear `in_flight` for A's hash — it does `self.in_flight.retain(|(bid, _)| *bid != body_id)` which IS correct
4. BUT: the bake thread still holds `bake_tx`. The result arrives at `poll()`.
5. `poll()` at line 979 calls `self.in_flight.remove(...)` — the entry was already removed by `remove_body`, so this is a no-op.
6. Generation check at line 982: `self.generations.get(&result.body_id)` — returns None (generation removed by `remove_body`), so `current_gen = 0`.
7. `result.generation >= current_gen` (0 >= 0) → **passes** the stale check
8. Line 991: `self.body_grids.insert(result.body_id, ...)` — **inserts a grid for a deleted body**

Then at `app_poll.rs:277`, `classify_bricks_for_body(body_nid)` runs for this deleted body, allocating brick pool slots for a body that no longer exists in the scene graph.

**Fix:** In `poll()`, after the generation check, also verify that the body still exists in the scene graph (or check against a set of known valid bodies). Alternatively, `remove_body()` should bump the generation to `u64::MAX` so stale results are always rejected.

---

## BUG-13: Race condition — `classify_bricks_for_body` called for body not in `body_grids`

**File:** `app/src/body_volume_cache.rs:1898-1901`
**Severity:** MINOR (properly guarded)
**Phase:** Introduced in Phase 2

```rust
let entry = match self.body_grids.get(&body_nid) {
    Some(e) => e,
    None => return,
};
```

This is correctly guarded with an early return. If a body was deleted between bake dispatch and completion, `body_grids` won't have an entry and `classify_bricks_for_body` exits safely.

**Not a bug** — the guard is correct. But it silently returns without cleaning up a potentially orphaned allocation from a previous classification of the same body. If the body was previously classified (had bricks allocated), those bricks remain allocated. Combined with BUG-01, this can leak.

---

## BUG-14: Stale `body_brick_volumes` entries after body hash changes

**File:** `app/src/body_volume_cache.rs:1934` and `app/src/body_volume_cache.rs:2011`
**Severity:** CORRECTNESS
**Phase:** Introduced in Phase 2

When `classify_bricks_for_body` runs, it:
1. Calls `brick_pool.deallocate_body(body_nid)` — frees old slots
2. Allocates new slots
3. Overwrites `body_brick_volumes[body_nid]` with new BBV (line 2011)

This is correct for the classified body. BUT: if `allocate_with_eviction` evicts OTHER bodies (BUG-06/07), those evicted bodies' `body_brick_volumes` entries have indirection tables pointing to slots that were just freed and handed to the new body. Reading those stale entries gives wrong data.

**Fix:** Same as BUG-06/07 — remove evicted bodies' `body_brick_volumes` entries.

---

## BUG-15: `body_last_used` keys can diverge from `body_bricks` keys

**File:** `graphics/src/brick_pool.rs:124-126` and `99-108`
**Severity:** MINOR
**Phase:** Introduced in Phase 1

`touch()` unconditionally inserts into `body_last_used`:
```rust
pub fn touch(&mut self, body_nid: NodeId, frame: u64) {
    self.body_last_used.insert(body_nid, frame);
}
```

If `touch()` is called for a body that has no bricks allocated (e.g., after its bricks were evicted), `body_last_used` will have an entry without a corresponding `body_bricks` entry. `evict_lru()` would then try to deallocate this body, calling `deallocate_body()` which finds no bricks (harmless) and removes the `body_last_used` entry.

**Impact:** Harmless — `evict_lru` wastes one iteration but doesn't corrupt state. However, `utilization()` and `body_brick_count()` remain correct because they read from `body_bricks`, not `body_last_used`.

**Fix:** Guard `touch()`: only insert if `body_bricks.contains_key(body_nid)`.

---

## BUG-16: `classify_bricks_for_body` source_hash not set on BodyBrickVolume

**File:** `app/src/body_volume_cache.rs:1949-2011`
**Severity:** MINOR
**Phase:** Introduced in Phase 2

The `BodyBrickVolume::new()` constructor sets `source_hash: 0`. The `classify_bricks_for_body` function builds the BBV at line 1949 but never sets `bbv.source_hash` before inserting at line 2011. The BBV always has `source_hash = 0`.

This means any future code that checks `bbv.source_hash` for cache invalidation (e.g., the tiered cache plan) will always see 0, never matching the actual body hash.

**Fix:** Add `bbv.source_hash = entry.source_hash;` after creating the BBV (before line 2011).

---

## BUG-17: `aabb_to_brick_range` can produce inverted ranges when query AABB is outside volume AABB

**File:** `graphics/src/brick_pool.rs:305-330`
**Severity:** MINOR
**Phase:** Introduced in Phase 1

When the query AABB is entirely outside the volume AABB (e.g., `aabb_min > self.aabb_max`), the `t_min` values will be > 1.0 and `t_max` may be < 0.0 after normalization. The `floor().max(0.0) as u32` cast clamps `bmin` to 0, and the `ceil() as u32` cast of a negative float becomes 0 (or wraps on some platforms). The `min(dims-1)` clamp then produces `bmax = 0`.

Result: `bmin = [0,0,0]`, `bmax = [0,0,0]` — returns a single brick at origin instead of an empty range.

**Impact:** Callers that iterate `bmin..=bmax` would process one extra brick (the origin brick). Not a crash, but wastes one brick's worth of work.

**Fix:** Return a sentinel or add an `is_empty()` check: if any `bmin[i] > bmax[i]`, return an empty range.

---

## BUG-18: `BodyBrickVolume::total_bricks()` can overflow for large dimensions

**File:** `graphics/src/brick_pool.rs:254-256`
**Severity:** MINOR
**Phase:** Introduced in Phase 1

```rust
pub fn total_bricks(&self) -> u32 {
    self.dims_bricks[0] * self.dims_bricks[1] * self.dims_bricks[2]
}
```

If `dims_bricks = [64, 64, 64]` (512³ resolution / 8), the product is 262,144 — fits in u32. But [128, 128, 128] (1024³ / 8) = 2,097,152 — still fine. [256, 256, 256] (2048³ / 8) = 16,777,216 — fine. Only truly astronomical dimensions would overflow. Not practically reachable with current resolution limits (max 256³).

**Impact:** None with current limits. Theoretical only.

**Fix:** No action needed currently. Add a debug_assert if desired.

---

## BUG-19: `upload_indirection` buffer size check is incorrect

**File:** `graphics/src/brick_pool.rs:291-299`
**Severity:** MINOR
**Phase:** Introduced in Phase 1

```rust
// Recreate if size changed
if buf.size() < data.len() as u64 {
    *buf = device.create_buffer(...);
}
queue.write_buffer(buf, 0, data);
```

The check is `buf.size() < data.len()` — recreates only if the buffer is too SMALL. If the new data is SMALLER than the existing buffer (e.g., body's brick grid shrank after removing prims), the old larger buffer is reused. This wastes some GPU memory but is not incorrect — `write_buffer` writes only `data.len()` bytes. However, the shader reads the full buffer size and may see stale data in the tail if the shader doesn't know the actual dims changed.

**Impact:** The shader reads `brick_info.dims` to know the valid range, so stale tail data is never accessed. This is not a functional bug, just a minor inefficiency.

**Fix:** No action needed. Buffer reuse is actually preferable to avoid GPU allocation churn.

---

## BUG-20: No bind group invalidation in late-result path

**File:** `app/src/app_poll.rs:292-305`
**Severity:** MINOR
**Phase:** Introduced in Phase 2

The late-result path at line 300-304 calls `classify_bricks_for_body` but does NOT invalidate `gpu.volume_preview_bg`. Only the main baked_bodies path (line 287-289) does.

```rust
for body_nid in late {
    // ...
    self.body_vol_cache.classify_bricks_for_body(body_nid, d, q);
    // ⚠ MISSING: if self.body_vol_cache.use_brick_pool { gpu.volume_preview_bg = None; }
}
```

**Fix:** Add the same bind group invalidation as the main path.

---

## BUG-21: `new_scene()` does not reset `use_brick_pool` properly

**File:** `app/src/project_new.rs:74-75`
**Severity:** MINOR
**Phase:** Pre-existing interaction

```rust
self.body_vol_cache.invalidate_all();
self.use_body_vol_cache = false;
```

`invalidate_all()` does not clear `body_brick_volumes` or `brick_pool` (BUG-02). Setting `use_body_vol_cache = false` disables the volume cache. But `use_brick_pool` (a separate flag on `body_vol_cache`) is NOT reset. If it was true before new_scene, it remains true. The next time `use_body_vol_cache` is re-enabled, the brick pool still holds stale data from the old scene.

**Fix:** Add `self.body_vol_cache.use_brick_pool = false;` in `new_scene()`, or fix BUG-02 so `invalidate_all()` clears brick state.

---

## BUG-22: `VolumePool::evict_lru()` can return index 0 when pool is empty

**File:** `graphics/src/gpu_volume.rs:395-422`
**Severity:** MINOR
**Phase:** Pre-existing (volume pool)

```rust
fn evict_lru(&mut self) -> (usize, Option<NodeId>) {
    let mut best_idx = 0;
    // ...
    for (i, slot_opt) in self.slots.iter().enumerate() {
        if let Some(slot) = slot_opt { ... }
    }
    // If no slots are occupied, loop never updates best_idx
    let evicted_nid = self.slots[best_idx].as_ref().map(|s| s.body_nid);
    if let Some(slot) = self.slots[best_idx].take() { ... }
    (best_idx, evicted_nid) // returns (0, None)
}
```

If all slots are `None` (empty pool), the function returns `(0, None)` — it "evicts" slot 0 which is already empty. The `take()` on a `None` slot is a no-op. This only happens if `acquire()` is called when all slots are empty AND the first-empty-slot check at line 358 somehow fails, which shouldn't happen. So this is unreachable in practice.

**Impact:** None — dead code path.

**Fix:** Add a debug_assert or return `(0, None)` explicitly before the loop.

---

## BUG-23: `classify_bricks_for_body` does not validate `bake_resolution`

**File:** `app/src/body_volume_cache.rs:1903`
**Severity:** MINOR
**Phase:** Introduced in Phase 2

```rust
let res = entry.bake_resolution;
```

If `bake_resolution` is 0 or 1, the brick grid dimensions would be:
- `res=0`: `dims = [(0+7)/8, ...] = [0, 0, 0]` → no bricks, no allocation → safe but wasteful
- `res=1`: `dims = [(1+7)/8, ...] = [1, 1, 1]` → 1 brick, 1 slot → safe

Division by zero is NOT possible because `voxel_size = extent / res as f32` where `res` could be 0 → `inf` or `NaN` for `brick_diag` → `threshold` is `NaN` → `val.abs() < threshold` always false → no active bricks. Safe.

**Impact:** None — naturally handles edge cases. But a `res=0` body wastes a classification attempt.

**Fix:** Add early return for `res < BRICK_DIM`.

---

---

## Summary Table

| ID | Severity | File | Short Title |
|---|---|---|---|
| BUG-01 | LEAK | body_volume_cache.rs:1144 | `remove_body()` doesn't deallocate from brick_pool |
| BUG-02 | LEAK | body_volume_cache.rs:505 | `invalidate_all()` doesn't clear brick pool/volumes |
| BUG-03 | CORRECTNESS | body_volume_cache.rs:1287 | `invalidate_for_undo()` doesn't invalidate brick data |
| BUG-04 | CORRECTNESS | brick_pool.rs:90 | `deallocate()` doesn't update `body_bricks` HashMap |
| BUG-05 | LEAK | body_volume_cache.rs:1998 | `cpu_data` allocation proportional to max pool slot index |
| BUG-06 | CORRECTNESS | body_volume_cache.rs:1938 | Eviction during classify leaves stale `body_brick_volumes` |
| BUG-07 | CORRECTNESS | brick_pool.rs:182 | `allocate_with_eviction()` doesn't report evicted bodies |
| BUG-08 | MINOR | brick_pool.rs:114 | Silent failure when only protected body remains |
| BUG-09 | CORRECTNESS | brick_pool.rs:222 | Stale bind group can reference dropped indirection buffer |
| BUG-10 | MINOR | body_volume_cache.rs:1981 | GPU upload skipped silently when queue is None |
| BUG-11 | CORRECTNESS | app_poll.rs:287 | `volume_preview_bg` not always invalidated after brick changes |
| BUG-12 | CORRECTNESS | body_volume_cache.rs:976 | Async bake result for deleted body re-inserts stale grid |
| BUG-13 | MINOR | body_volume_cache.rs:1898 | Guard is correct but orphan bricks possible via BUG-01 |
| BUG-14 | CORRECTNESS | body_volume_cache.rs:1934 | Stale `body_brick_volumes` after eviction during classify |
| BUG-15 | MINOR | brick_pool.rs:124 | `body_last_used` keys can diverge from `body_bricks` keys |
| BUG-16 | MINOR | body_volume_cache.rs:1949 | `source_hash` never set on BodyBrickVolume |
| BUG-17 | MINOR | brick_pool.rs:305 | `aabb_to_brick_range` returns [0,0,0] for out-of-range AABB |
| BUG-18 | MINOR | brick_pool.rs:254 | `total_bricks()` u32 overflow for extreme dimensions |
| BUG-19 | MINOR | brick_pool.rs:291 | Indirection buffer never shrinks (minor waste) |
| BUG-20 | MINOR | app_poll.rs:300 | No bind group invalidation in late-result path |
| BUG-21 | MINOR | project_new.rs:74 | `new_scene()` doesn't reset `use_brick_pool` flag |
| BUG-22 | MINOR | gpu_volume.rs:395 | `evict_lru()` returns (0, None) when pool empty (unreachable) |
| BUG-23 | MINOR | body_volume_cache.rs:1903 | No `bake_resolution` validation (safe but wasteful) |

---

## Recommended Fix Order

### 1. LEAK (fix first — they accumulate silently)

| Order | Bug | Dependency | Effort |
|-------|-----|------------|--------|
| 1.1 | **BUG-01** | None | 2 lines |
| 1.2 | **BUG-02** | None | 5 lines |
| 1.3 | **BUG-05** | None | Moderate refactor of cpu_data indexing |

**BUG-01 and BUG-02** should be fixed in the same commit — they're the same class of missing cleanup. BUG-05 is a separate refactor that can wait until the cache tier work (which already addresses it with compact sequential storage).

### 2. CORRECTNESS (fix second — they cause wrong data)

| Order | Bug | Dependency | Effort |
|-------|-----|------------|--------|
| 2.1 | **BUG-07** | None | Change return type of `allocate_with_eviction` |
| 2.2 | **BUG-06** | Requires BUG-07 fix | Wire evicted body list into classify |
| 2.3 | **BUG-14** | Same as BUG-06 | Same fix |
| 2.4 | **BUG-03** | Requires BUG-01 pattern | Add brick cleanup to undo path |
| 2.5 | **BUG-12** | None | Bump generation on remove_body |
| 2.6 | **BUG-04** | None | Remove unused `deallocate()` method |
| 2.7 | **BUG-11** | None | Add bg invalidation after all classify calls |
| 2.8 | **BUG-09** | None | Already mitigated by per-frame rebuild, low priority |
| 2.9 | **BUG-16** | None | 1 line: set source_hash |

**BUG-06/07/14** form a cluster — they all stem from eviction during classification not being visible to the caller. Fix BUG-07 first (API change), then BUG-06 and BUG-14 follow naturally.

**BUG-12** is an independent race condition — fix by bumping generation in `remove_body()`.

### 3. MINOR (fix opportunistically)

| Order | Bug | Dependency | Effort |
|-------|-----|------------|--------|
| 3.1 | **BUG-20** | None | 2 lines |
| 3.2 | **BUG-21** | BUG-02 fix preferred | 1 line or handled by BUG-02 |
| 3.3 | **BUG-15** | None | 1-line guard in touch() |
| 3.4 | **BUG-08** | None | Add eprintln |
| 3.5 | **BUG-10** | None | Add "needs_upload" flag |
| 3.6 | **BUG-17** | None | Add empty range check |
| 3.7 | **BUG-23** | None | Add early return |
| 3.8 | **BUG-22** | None | Unreachable, optional |
| 3.9 | **BUG-18** | None | Theoretical, optional |
| 3.10 | **BUG-19** | None | Not a bug, intentional |
| 3.11 | **BUG-13** | Fixed by BUG-01 | No separate fix needed |

---

## Pre-Existing vs New

| Bug | Origin |
|-----|--------|
| BUG-01 | **Phase 1/2** — brick implementation didn't add cleanup to existing `remove_body()` |
| BUG-02 | **Phase 1/2** — brick implementation didn't add cleanup to existing `invalidate_all()` |
| BUG-03 | **Phase 1/2** — brick implementation didn't add cleanup to existing `invalidate_for_undo()` |
| BUG-04 | **Phase 1** — `deallocate()` API designed without `body_bricks` awareness |
| BUG-05 | **Phase 2** — `cpu_data` indexing scheme chosen for simplicity over compactness |
| BUG-06 | **Phase 2** — eviction side-effect not propagated to `body_brick_volumes` |
| BUG-07 | **Phase 1** — `allocate_with_eviction` API doesn't report evictions |
| BUG-08 | **Phase 1** — minor diagnostic gap |
| BUG-09 | **Phase 2** — bind group lifecycle oversight |
| BUG-10 | **Phase 2** — silent skip of GPU upload |
| BUG-11 | **Phase 2** — bind group invalidation incomplete |
| BUG-12 | **Pre-existing** — volume cache race condition, now extends to brick pool |
| BUG-13 | **Phase 2** — correctly guarded, not a bug |
| BUG-14 | **Phase 2** — same root cause as BUG-06 |
| BUG-15 | **Phase 1** — minor state divergence |
| BUG-16 | **Phase 2** — field initialization omission |
| BUG-17 | **Phase 1** — edge case in AABB conversion |
| BUG-18 | **Phase 1** — theoretical overflow |
| BUG-19 | **Phase 1** — buffer reuse is intentional |
| BUG-20 | **Phase 2** — copy-paste omission from main path |
| BUG-21 | **Phase 1/2 interaction** — `use_brick_pool` flag not reset on scene change |
| BUG-22 | **Pre-existing** — unreachable edge case in VolumePool |
| BUG-23 | **Phase 2** — missing validation |

---

## Cross-Cutting Flow Analysis

### Body Deletion Flow

```
User deletes body → one of:
  gizmo.rs:237        → body_vol_cache.remove_body(nid) → scene_graph.remove(nid)
  input_keyboard.rs:855 → body_vol_cache.remove_body(nid) → scene_graph.remove(nid)
  outliner_panel.rs:171 → body_vol_cache.remove_body(nid) → scene_graph.remove(nid)
  prim_actions.rs:343   → body_vol_cache.remove_body(nid) → scene_graph.remove(nid)
```

**remove_body cleans up:**
- ✅ body_grids
- ✅ generations
- ✅ dirty_bodies
- ✅ in_flight
- ✅ bake_start_times
- ✅ body_bounds
- ✅ transform_safe_bodies
- ✅ dirty_regions
- ✅ cascades
- ✅ body_chunked_meshes
- ✅ last_bake_dirty_range
- ✅ ram_cache
- ✅ volume_pool
- ❌ **brick_pool** (BUG-01)
- ❌ **body_brick_volumes** (BUG-01)

### Undo Flow

```
apply_undo_entry() → restore scene_graph → body_vol_cache.invalidate_for_undo(&sg)
```

**invalidate_for_undo cleans up (per invalidated body):**
- ✅ body_grids (moved to ram_cache)
- ✅ body_chunked_meshes
- ✅ last_bake_dirty_range
- ✅ dirty_regions
- ✅ cascades
- ❌ **brick_pool** (BUG-03)
- ❌ **body_brick_volumes** (BUG-03)

### New Scene Flow

```
new_scene() → body_vol_cache.invalidate_all()
```

**invalidate_all cleans up:**
- ✅ body_grids
- ✅ ram_cache
- ✅ volume_pool (recreated)
- ✅ dirty_bodies, in_flight, bake_start_times, generations, body_bounds
- ✅ transform_safe_bodies, dirty_regions, cascades
- ✅ body_chunked_meshes, last_bake_dirty_range, primary_bake_requests
- ❌ **brick_pool** (BUG-02)
- ❌ **body_brick_volumes** (BUG-02)

### Load Project Flow

```
load_from_file() → app.body_vol_cache.invalidate_all()
```

Same as New Scene — **brick pool not cleared** (BUG-02).

### Prim Add/Remove Flow

```
Prim added/removed → mark_body_dirty(body_nid) → dirty_bodies.insert()
  → trigger_bake() → bake result → poll() → body_grids.insert()
  → classify_bricks_for_body() → brick_pool.deallocate_body() + allocate new
```

✅ This flow is correct — `classify_bricks_for_body` first deallocates old bricks, then allocates new ones. The body's brick data is fully replaced.
