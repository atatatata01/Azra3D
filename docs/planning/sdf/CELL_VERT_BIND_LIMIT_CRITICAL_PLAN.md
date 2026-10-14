# CRITICAL PLAN — DC `cell_vert` Storage Binding Limit / Device Loss

**Status:** IMPLEMENTATION IN PROGRESS (2026-07-20)  
**Severity:** P0 — device loss / app crash; freezes mesh updates; confuses Path Trace  
**Owner surfaces:** SDF dual-contour mesher (`sdf_dc_*`), not the path-trace kernel  
**Evidence:** `%TEMP%\fourthlayer_gpu_error.txt`, `visdump_*.log`, R3c comments in `resident.rs`  
**Date:** 2026-07-20  

### Implementation progress

| Phase | Status | Verification |
|-------|--------|--------------|
| L0 policy helpers + unit tests | **DONE** | 8/8 `cell_vert_bind_limit_tests` pass |
| L1 dense entry gates (`mesh_into_slot*`) | **DONE** | return `bool`; refuse res>512 |
| L2 `ensure_output_caps` bind-limit + poison heal | **DONE** | refuse create; `sanitize_cell_vert_if_poisoned` |
| Sparse over-limit refuse | **DONE** | early return + `envelope_over_budget` |
| Populate fallthrough I7 | **DONE** | no dense mesh when nverts>512 without sparse |
| PT temp path | **DONE** | shares sparse policy |
| Live owner L1–L6 stress | **PENDING** | manual |

---

## 0. Executive summary

### What breaks

```
Device::create_bind_group label='sdf_dc_bg'
  Buffer binding 2 range 4282396672
  exceeds max_storage_buffer_binding_size 2147483648
```

Then: invalid bind group → failed compute passes → invalid encoder → **device dropped** → app crash.

### What binding 2 is

`cell_vert` — intermediate storage for dual-contour (one slot per DC cell, sparse-addressed as brick-local on sparse path).

### Size fingerprint

| Expression | Bytes |
|------------|------:|
| Error `given` | **4 282 396 672** |
| Dense formula `(1024−1)³ × 4` | **4 282 396 668** (+4 align) |

**Exact match to dense `dc_caps(1024)` cell_vert sizing.** Not a random VRAM OOM.

### Driver reality (RTX 4090 / Vulkan, this app)

| Limit | Typical value |
|-------|----------------|
| `max_storage_buffer_binding_size` | **2 GiB** (`2^31`) |
| `max_buffer_size` | Often **larger** (multi-GB create allowed) |

**Create can succeed; bind fails.** That is why the fault shows up at `create_bind_group`, not always at `create_buffer`.

### One-sentence root cause

**Something allocated / retained a dense-1024-sized `cell_vert` (~4 GiB) and then tried to bind the entire buffer as storage — which is structurally illegal on this GPU class.**

### Proper vs band-aid (design stance)

This codebase **already chose** the correct architecture:

| Regime | Res | `cell_vert` model | Bindable? |
|--------|-----|-------------------|-----------|
| Dense GridBacked | ≤ **512³** | `(res−1)³` elements | Yes (~0.5 GiB @512) |
| Sparse bricks | 512+ / 1024 / 2048 | `allocated_bricks × 512` | Yes if surface-bounded + capped |

**Proper work** = make that contract **unbreakable + recoverable**.  
**Band-aid** = catch one call site, leave grow-only poison and illegal dense@1024 paths alive.

---

## 1. Problem statement (full cascade)

```
[illegal cell_vert size or poisoned slot]
        │
        ▼
create_bind_group(sdf_dc_bg)  → BufferRangeTooLarge binding 2
        │
        ▼
ComputePass set_bind_group invalid  (sdf_dc_vertex / sdf_dc_quads)
        │
        ▼
CommandEncoder invalid
        │
        ▼
Further copies / PT / bake submissions fail
        │
        ▼
Device lost / process exit (0xcfffffff-class / Device dropped)
```

**User perception:** “Path Tracer crashed” because PT is active while remesh/bake/PT snapshot mesh runs.  
**Actual fault domain:** SDF DC mesher (`app/src/sdf_gpu_mesh/*`, `app_poll_gpu.rs::sdf_populate_resident`).

Separate, **already fixed** PT issue: `clear_texture` without `CLEAR_TEXTURE` feature (orbit half→full). Do **not** conflate.

---

## 2. Architecture inventory (must not regress)

### 2.1 Bake regime

| Source | Cap | Role |
|--------|-----|------|
| `max_gridbacked_resolution()` | **512** | Dense field + dense DC only |
| `max_sparse_resolution()` | **2048** | Sparse brick bake + surface mesh |
| `max_textureonly_resolution()` | **512** | Legacy volume path |
| `BodyVolumeCache::max_mesh_resolution()` | sparse? 2048 : 512 | Object Mode ceiling |
| `sparse_bake_eligible(res)` | res≥512 + Object Mode + sparse regime | Fork dense vs sparse bake |

**Invariant A — Bake:** No dense GridBacked field at res > 512.

### 2.2 Mesh dispatch arms (`sdf_populate_resident`)

| Condition | Arm | Entry | `cell_vert` sizing |
|-----------|-----|-------|-------------------|
| `brick_resident_ok` or classify sparse | Sparse tiled → mono fallback | `mesh_into_slot_sparse_tiled` / `mesh_into_slot_sparse` | `allocated_bricks × 512` |
| `use_resident_field` | Dense field-direct | `mesh_into_slot_from_resident` | `dc_caps(res).0` with **res = dispatch res** |
| else + CPU grid | Dense upload | `mesh_into_slot` | `dc_caps(res).0` with **res = dispatch res** |

Comments in-tree already document R3c: sizing from **`capacity_res` at 1024** was the multi-body “surface breaks + res can’t recover” bug.

**Invariant B — Dense mesh:** Dispatch res ≤ 512; `cell_vert` request = `dc_caps(dispatch_res).0` only.

**Invariant C — Sparse mesh:** `cell_vert` request = surface-proportional only; never volume `(res−1)³`.

### 2.3 Slot lifecycle

| Op | Behavior |
|----|----------|
| `SdfBodyMeshSlot::new(res)` | Placeholder buffers; `capacity_res = res` (can be 1024!) |
| `ensure_output_caps` | Grow-only verts/idx/cell_vert; **verts/idx** clamped to dense ceiling; **`cell_vert` NOT clamped** |
| `trim_output_caps` | Shrink only if current ≥ 2× need (settle-time) |
| `as_entire_binding()` | Always binds **full buffer size**, not “logical need” |

**Invariant D — Bind:** Bound storage size ≤ `device.limits().max_storage_buffer_binding_size` **always**.

**Invariant E — Capacity:** Grow-only must never produce a buffer that violates D. If it would, recreate at legal size or refuse.

### 2.4 Consumers of the mesh

| Consumer | Path |
|----------|------|
| Raster G-buffer / picking | Resident slot draw or `mesh_cache` |
| Path Trace BLAS | `mesh_cache` via PT snapshot (`dispatch_sdf_display_readback` → temp sparse slot) |
| Export | Same materialize arms |

PT snapshot sparse arm:

```text
SdfBodyMeshSlot::new(device, res)  // res can be 1024
mesh_into_slot_sparse(..., pool: None)  // unpooled temp
readback_slot → mesh_cache
```

Same `cell_vert` rules apply to **temp PT slots**.

---

## 3. Failure modes (every angle)

### F1 — Dense math at 1024 (primary log fingerprint)

**How**

- Any dense arm with `slot.res == 1024` (or `nverts == 1024` without sparse_bricks).
- Historical: `ensure_output_caps(..., dc_caps(capacity_res))` with capacity_res=1024.

**Why it still happens**

| Gap | Detail |
|-----|--------|
| Slot capacity watermark | `need_new` only when `capacity_res < nverts`. Raising 512→1024 **creates** slot with `capacity_res=1024`. |
| Sparse gate fallthrough | If `sparse_bricks` is `None` at 1024 (atlas miss, classify off, empty grid, flag off), code can hit dense `mesh_into_slot` / field-direct. |
| Assert vs release | `ensure_output_caps` has assert on request > bind limit — panics in debug; older builds / paths without assert created unbindable buffers. |
| Bind whole buffer | Even if request is legal later, **existing 4 GiB buffer** still binds as 4 GiB. |

**Severity:** P0 crash / freeze.

### F2 — Poisoned grow-only slot (recovery hole)

**How**

1. Once `cell_vert` buffer size > bind limit (or was created in older build).
2. Every later DC bind uses `as_entire_binding()` → permanent failure.
3. Grid down / sparse remesh does **not** automatically shrink past grow-only rules unless settle trim fires with 2× hysteresis.

**Severity:** P0 sticky failure — “res change can’t recover” (named in code).

### F3 — Sparse near-full-volume atlas

**How**

- Full brick grid @1024 = 128³ ≈ 2.1M bricks.
- If `allocated_bricks` approaches full grid: `2.1M × 512 × 4 ≈ 4 GiB` again.

**When:** Pathological / solid-interior mis-classify / entire volume “surface.”  
**Severity:** P0 rare; pool ceiling + self-heal should fire first, but must not rely on luck.

### F4 — Pool / verts ceiling vs cell_vert asymmetry

**How**

- Verts/idx: clamped via `dc_output_caps_dense` (≤ ~1 GiB / half bind).
- `cell_vert`: explicitly **not** clamped in `ensure_output_caps`.
- Truncation toasts exist for verts; **no equivalent** for illegal `cell_vert`.

**Severity:** P1 design asymmetry — verts fail loud, cell_vert fails as device loss.

### F5 — Empty binding 0 (`range 0`)

**How**

- Dense path binds `slot.field` / resident field; placeholder or missing field → invalid bind range 0.
- Seen in same GPU error log family.

**Severity:** P1 cascade companion; often secondary after first invalid encoder.

### F6 — PT snapshot temp slot at high res

**How**

- Res fix keys PT snap on `(hash, res)` → more frequent high-res sparse remesh into **unpooled temp** slot.
- Increases stress on F1/F2/F3 during Path Trace sessions (user blames PT).

**Severity:** P1 interaction; not root of formula but raises repro rate.

### F7 — Multi-body / LRU eviction

**How**

- Atlas eviction mid-session → `brick_resident_ok` false → may fall to dense/classify path with wrong assumptions.
- Multi 512³ bodies historically noted as VRAM pressure (comments ~3.2 GB dense slot fantasy).

**Severity:** P1 multi-body device loss.

### F8 — LOD path (`mesh_into_slot_sparse_lod`)

**How**

- Level > 0 remesh from atlas; still uses same slot + cell_vert policy.
- After pooled tiled mesh, unpooled LOD must not bind placeholders (known P1, partially gated by tests).

**Severity:** P1 if LOD on; default LOD off.

### F9 — Validation after first error

**How**

- wgpu invalidates encoder; subsequent errors are noise.
- Device lost callback / visdump shows “Dropped” without original message if log rotated.

**Severity:** Diagnostics — plan requires first-error capture + breadcrumb.

### F10 — Confusion with other crashes

| Issue | Domain | Status |
|-------|--------|--------|
| `CLEAR_TEXTURE` | PT reservoir clear | Fixed (feature + guard) |
| Gizmo vs Spine panic | Input / tool mode | Unrelated |
| Pool ceiling self-heal drop res | Sparse bake | Intended recovery |

Plan scope = **F1–F9 only** (DC bind limit family).

---

## 4. Invariant matrix (definition of done)

Any PR that claims “fixed” must hold **all** of these under test + live checklist:

| ID | Invariant | Enforce at |
|----|-----------|------------|
| **I1** | Dense DC dispatch res ≤ `max_gridbacked_resolution()` (512) | Entry of `mesh_into_slot`, `mesh_into_slot_from_resident`, populate dense arms |
| **I2** | Sparse `cell_vert` request = f(allocated_bricks) only, never `(res−1)³` | `mesh_into_slot_sparse*` |
| **I3** | `cell_vert.size() ≤ max_storage_buffer_binding_size` before any `create_bind_group` | `ensure_output_caps` + bind site |
| **I4** | Verts/idx already ≤ bind ceiling | Keep `dc_output_caps_dense` |
| **I5** | Grow-only never crosses I3; if need would cross → recreate legal or hard-fail with toast | Capacity policy |
| **I6** | Poisoned slot detect + heal (size > limit → recreate at need or destroy slot) | Poll start / before mesh |
| **I7** | High res without sparse atlas **never** falls into dense DC | `sdf_populate_resident` fallthrough |
| **I8** | PT temp slot obeys I2–I6 | `dispatch_sdf_display_readback` |
| **I9** | Fail closed: no submit with invalid BG; no silent freeze | All DC encode paths |
| **I10** | Loud degradation: toast + log + optional res self-heal | User-visible |

---

## 5. Proper fix architecture (not band-aids)

### Layer 0 — Single policy module (source of truth)

Introduce (or consolidate into) one pure policy API, e.g.:

```text
dc_cell_vert_cap_dense(dispatch_res) -> Result<u64, ExceedsBind>
dc_cell_vert_cap_sparse(allocated_bricks) -> Result<u64, ExceedsBind>
dc_max_bind_elems(device) -> u64  // max_storage_buffer_binding_size / 4
assert_bindable_bytes(bytes, limit)
```

**Rules**

- Dense: res > 512 → **Err** (not clamp silently to wrong geometry).
- Sparse: `min(allocated_bricks * 512, max_bind_elems)`; if clamp would under-serve full surface → **Err** (caller self-heals res / refuses mesh).
- All mesh arms call **only** these helpers for `cell_vert` wants.

This is **proper**: one definition, compile-time searchable.

### Layer 1 — Entry-point hard gates

| Site | Gate |
|------|------|
| `mesh_into_slot` | `assert!(res <= 512)` or return Err |
| `mesh_into_slot_from_resident` | same |
| `sdf_populate_resident` dense branch | If `nverts > 512 && !sparse_bricks` → **do not** dense mesh; toast + request sparse rebake / drop res |
| Sparse arms | Refuse atlas with `allocated_bricks * 512 * 4 > bind_limit` |

No “UI only” block — **internal** paths enforced.

### Layer 2 — Capacity policy (grow-only completed)

| Event | Action |
|-------|--------|
| `want_cv * 4 > bind_limit` | **Never** create that buffer |
| Existing `cell_vert.size() > bind_limit` | **Poison detect** → destroy + recreate at legal want, or drop slot + full remesh |
| Dense→sparse or sparse→dense | Reset `cell_vert` to placeholder; re-size for new regime (don’t keep 1024 dense map under sparse) |
| Settle trim | Keep; also trim when size illegal regardless of 2× hysteresis |

Grow-only stays for **legal** mid-gesture growth. Illegal sizes are **not** “grown into.”

### Layer 3 — Bind hygiene

Options (pick one primary):

| Option | Proper? | Notes |
|--------|---------|-------|
| **A. Entire binding only if size ≤ limit** (always true after L2) | Yes | Simplest; no API risk |
| **B. Buffer binding offset+size subrange** | Yes if needed later | Only if multi-region cell_vert sharing |
| **C. Tile cell_vert like C1 tiles** | Yes for extreme sparse | Bigger project; defer unless F3 fires in metrics |

**Default plan: A after L0–L2.** B/C are phase-2 only.

### Layer 4 — Failure recovery (product)

| Failure | Recovery |
|---------|----------|
| Illegal dense@high res | Force sparse path or clamp **effective bake res** with toast (existing self-heal pattern) |
| Sparse cell_vert would exceed bind | Drop body res one notch + rebake (mirror pool ceiling self-heal in `app_poll_bake`) |
| Poisoned slot | Recreate slot; remesh once; toast if truncated |
| Encoder invalid after error | Stop further GPU work that frame; do not cascade submits |

### Layer 5 — Observability

| Signal | Action |
|--------|--------|
| `[DcBind] reject cell_vert=XMB limit=YMB body=… res=… arm=dense|sparse` | Always log on reject |
| Visdump / breadcrumb on first BufferRangeTooLarge | Correlate frame + body + res + arm |
| Dev HUD: `slot.cell_vert MB`, `capacity_res`, `allocated_bricks` | Owner debug |
| Perf counter: mesh bind rejects / poison heals | CI / stress |

---

## 6. Implementation plan (phased PRs)

### Phase 0 — Freeze & measure (0.5–1 day)

**Goal:** No behavior change; prove repro + baselines.

1. Document repro recipes (below).
2. Add temporary **debug-only** logging at `ensure_output_caps` when `want_cv * 4 > max_bind` (if not already panicking).
3. Capture one clean repro: Grid 1024, multi-body optional, PT on, orbit + res change.
4. Record: arm path (`[GpuMeshPath]`), `capacity_res`, `cell_vert.size()`, `allocated_bricks`.

**Exit:** Written repro + one log chain matching F1/F2.

### Phase 1 — Invariants + hard fail (1–2 days) — **CRITICAL CORE**

**Goal:** Impossible to submit unbindable `cell_vert`.

1. **L0** policy helpers + unit tests:
   - `dc_caps(256/512)` bindable under 2 GiB.
   - `dc_caps(1024)` **rejected** for dense.
   - Sparse caps for N bricks bindable; over-limit N rejected.
2. **L1** gates on all dense entry points.
3. **L2** `ensure_output_caps`: reject / recreate instead of creating > bind limit; **remove path that can create 4 GiB cell_vert**.
4. **I7** fallthrough: `nverts > 512 && sparse_bricks.is_none()` → no dense mesh; escalate toast + dirty for sparse rebake or res drop.
5. Tests:
   - `tests_dc` existing slot@512 fits.
   - New: dense@1024 refused.
   - New: sparse cell_vert request over limit refused without device loss.
   - New: poisoned buffer size simulated → heal recreates legal buffer.

**Exit:** Cannot get `BufferRangeTooLarge` binding 2 from dense-1024 formula in validation (debug + release). Failures are toast/assert, not device loss.

### Phase 2 — Poison recovery + regime switch (1 day)

**Goal:** Sticky “can’t recover after res thrash” dies.

1. `slot.sanitize_for_bind(device)` before every DC encode:
   - if `cell_vert.size() > max_bind` → recreate placeholder + ensure legal need.
2. On `capacity_res` change across 512 boundary (dense↔sparse era): reset cell_vert/field regime state.
3. PT temp slot: after readback drop, ensure no leak of huge temps (drop scope already; verify no pool retention).
4. Live checklist: 512→1024→256→1024 thrash with PT on, multi-body.

**Exit:** Res thrash recovers mesh without restart.

### Phase 3 — Sparse ceiling alignment (0.5–1 day)

**Goal:** F3 + pool self-heal consistency.

1. Unify “cannot mesh” with bake pool ceiling: same `pool_ceiling_bricks` / bind limit math.
2. If sparse mesh would need `cell_vert` > bind limit → same self-heal as bake (drop res notch + toast), **before** dispatch.
3. Optional metric: max `allocated_bricks` / body.

**Exit:** No sparse path creates unbindable cell_vert; user sees res drop toast instead of crash.

### Phase 4 — PT / bake interaction hardening (0.5 day)

**Goal:** PT session doesn’t amplify F1.

1. PT snapshot path uses same L0–L2 (already shares `mesh_into_slot_sparse`).
2. Rate-limit PT snap remesh cost if needed (already 1/poll) — verify 1024 snap + live PT doesn’t TDR.
3. Breadcrumb: on mesh bind reject, write `fourthlayer_gpu_breadcrumb.txt` with body/res/arm.

**Exit:** PT + Grid 1024 stress without device loss (or controlled degrade).

### Phase 5 — Optional extreme sparse (defer)

Only if production metrics show near-full brick allocation:

- Tiled cell_vert / subrange binds.
- Not required to close the logged 4 GiB dense-formula crash.

---

## 7. Test plan

### 7.1 Unit / GPU tests (CI-capable where possible)

| Test | Asserts |
|------|---------|
| Dense caps @512 | `cell_vert` bytes ≤ bind limit |
| Dense caps @1024 | Policy returns Err / gate refuses |
| Sparse caps | `N * 512 * 4` ≤ limit for N under ceiling |
| `ensure_output_caps` poison | Pre-sized illegal buffer → sanitize → bindable |
| Populate fallthrough | Mock nverts=1024, no atlas → no dense dispatch |
| PT readback sparse | Temp slot after sparse mesh has bindable cell_vert |

### 7.2 Live owner checklist (manual)

| # | Scenario | Pass criteria |
|---|----------|---------------|
| L1 | Single body 256→512→1024 Grid, Object Mode | Mesh updates; no device loss; console no BufferRangeTooLarge |
| L2 | 1024 thrash 10× with Adaptive off | Recover each time |
| L3 | Two bodies @512 + one @1024 | No multi-body loss |
| L4 | Path Trace on during L1–L3 | No crash; snap lands (`[PtSnap]`) |
| L5 | Orbit stop half→full (CLEAR_TEXTURE) | Still green (prior fix) |
| L6 | Force sparse fail (evict atlas) @1024 | Toast / res drop — **not** silent freeze |
| L7 | Extreme solid (if available) | Self-heal res, not crash |

### 7.3 Negative tests (must fail loud)

| # | Injection | Expected |
|---|-----------|----------|
| N1 | Call dense mesh API with res=1024 | Err/assert/toast, no submit |
| N2 | `allocated_bricks` huge | Refuse mesh / drop res |

---

## 8. Risk register

| Risk | Mitigation |
|------|------------|
| Asserts crash debug only; release still poisons | Phase 1: **return Err + toast in release**, not only `assert!` |
| Fix hides truncation | Loud toast when sparse clamped / res dropped (I10) |
| Performance: recreate slot often | Only on poison or regime change; not every frame |
| Break grow-only gesture VRAM pattern | Keep grow-only **under** bind limit |
| PT snap latency after res keying | Already settle-gated; measure; don’t re-break res keying |
| Multi-PR regression | Each phase must keep full invariant matrix green |

---

## 9. Out of scope (explicit)

- Path tracer kernel / ReSTIR / denoise quality (except CLEAR_TEXTURE already done).
- Changing `max_gridbacked_resolution` above 512 without a new dense algorithm.
- WebGPU / macOS lower bind limits (same code must respect `device.limits()`).
- Full redesign of DC to eliminate `cell_vert` (research, not this P0).

---

## 10. Success criteria (ship bar)

1. **No** `BufferRangeTooLarge` for `sdf_dc_bg` binding 2 under owner stress (L1–L6).  
2. **No** sticky mesh freeze after 512↔1024 thrash without app restart.  
3. High-res failures **degrade** (toast + res drop / refuse mesh), never **device lost**.  
4. Invariants I1–I10 encoded in tests or runtime checks.  
5. Plan items classified: **no band-aid-only ship** (clamp without poison heal + dense@1024 gate is incomplete).

---

## 11. Recommended PR stack (Graphite / sequential)

| PR | Title | Phase |
|----|-------|-------|
| PR-A | `dc_cell_vert` policy helpers + unit tests | 0–1 |
| PR-B | Dense entry gates + populate fallthrough | 1 |
| PR-C | `ensure_output_caps` bind-limit enforcement + poison sanitize | 1–2 |
| PR-D | Sparse over-limit → self-heal res (align bake) | 3 |
| PR-E | Diagnostics breadcrumb + live checklist doc | 4 |

**Do not merge PR-B without PR-A.**  
**Do not call “done” after PR-B alone** (still need poison heal).

---

## 12. File map (implementation touch list)

| Area | Files |
|------|--------|
| Policy | `sdf_gpu_mesh/dc_pipeline.rs` (or new `dc_limits.rs`) |
| Slot capacity | `sdf_gpu_mesh/resident.rs` (`ensure_output_caps`, `new`, sanitize) |
| Populate routing | `app_poll_gpu.rs` (`sdf_populate_resident`) |
| Overflow grow | `app_poll_gpu.rs` (`poll_sdf_mesh_output_overflow`) |
| Bake self-heal parity | `app_poll_bake.rs` (existing pool ceiling) |
| PT temp mesh | `mesh_presentation.rs` (`dispatch_sdf_display_readback`) |
| Tests | `sdf_gpu_mesh/tests_dc.rs`, sparse tests |
| Limits constants | `app_gpu/src/gpu_jobs.rs` (already defines caps) |

---

## 13. Decision log (for implementers)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Dense max | Keep 512 | Bind math; not negotiable without new algorithm |
| cell_vert clamp | Fail / self-heal, don’t silent under-size sparse | Silent under-size = wrong mesh / vanish |
| Subrange bind | Defer | Not needed for dense-1024 fingerprint |
| Grow-only | Keep under legal ceiling | Gesture VRAM + correctness |
| PT blame | Document; fix mesher | Correct domain |

---

## 14. Appendix — Size reference

| Res | Dense `cell_vert` | @2 GiB bind |
|-----|------------------:|-------------|
| 256 | ~62 MB | OK |
| 512 | ~497 MB | OK |
| 1024 | **~3.99 GB** | **FAIL** |
| 2048 | ~32 GB | FAIL |

Sparse: `allocated_bricks × 2 KiB` per brick (512 f32).  
Bind budget ≈ **~1 048 576 bricks** max if entire bind used for cell_vert alone (2 GiB / 2 KiB).

---

## 15. Final stance

This is **critical infrastructure**.  

- **Proper fix set** = L0 policy + L1 gates + L2 capacity/poison + L4 recovery + tests.  
- **Band-aid** = only toast, only UI disable 1024, only catch validation once, or only PT workarounds.  

Ship bar is **device never dies** from DC bind limits; high res either works sparse-legal or **degrades loudly**.

**No implementation in this document — plan only until owner greenlights Phase 0.**
