# Phase D4 finish plan — fix everything that blocks shippable clusters

**Date:** 2026-08-05  
**Branch / worktree:** `feat/viewport-perf` @ `F:\PROJECT\FourthLAyerAi\.claude\worktrees\viewport-perf`  
**Authority:** HEAD commit messages + corrected architecture review (not stale plan Status, not superseded scoreboard numbers)  
**Product posture until this plan exits:** `AZRA_CLUSTERS` / `dbg_clusters` stay **OFF** for real work.

---

## 0. Goals and non-goals

### Goals (this plan)

1. **Never invisible geometry** — a draw marked `DRAW_KIND_CLUSTERED` always has a cut+multi_draw path, or falls back to `DRAW_KIND_MESH`.
2. **Never stale clusters after edit** — mesh stamp is source of truth; stamp mismatch → immediate MESH (and drop pool entry), not “wait for debounce.”
3. **Bake keeps working while clustered** — candidates are not skipped just because draw_kind is CLUSTERED.
4. **Coverage holes reduced to a measurable, closable gate** on **non-terrain** dense meshes (terrain is a separate clipmap track per owner `01ab2ca`).
5. **Opt-in path trustworthy** — then visual gates, then only then default-on discussion.
6. **No stealth cost when clusters draw is OFF** — cut/bake/upload policy matches feature flag (or explicit prebake mode).

### Non-goals (explicitly out of this plan)

| Item | Why |
|------|-----|
| Cluster HZB early/late | D4 plan deferred to **D5** |
| Vis-buffer / dual depth / material resolve | **D5** / G5–G6 |
| Streaming / raise residency to hundreds of cids | **D7** (caps stay; overflow → MESH) |
| Dirty-region recluster / full T4 edit moat | **D8** (minimal Edit drop-out only) |
| Geometry clipmaps for heightfield | **Parallel track** (`PHASE_D_TWO_TRACK_GEOMETRY.md`) |
| Restore plan formula “camera inside sphere → ∞” | **Forbidden** — caused mountain flicker |
| Re-litigate Q1 / “one technique per scene” | Per-body dual-draw ban only |

### Do not treat as confirmed bugs (from review correction)

| Prior label | Status in this plan |
|-------------|---------------------|
| H1 “frustum after group cut = visible holes” | **Dropped.** Frustum fully-outside is correct off-screen cull. Residual: **CPU tests don’t apply cone** → test gap only (P3). |
| H3 shadow cascade drops casters | **Closed N/A (WP8)** — `compute_cascade_vps` has caster headroom (light eye 50, near=-r); documented at cascade cut write site. |
| B5 “617 vs 853” as HEAD evidence | **Stale.** Use `0c0944e` (617→8381 LOD gradient) + HEAD: *coverage holes remain severe*. |
| “T2 used exactly 8 bodies” | **Unverified** — do not design caps around that claim; design around **invariant remap ⊆ slots**. |
| M2/M3/M8/M9/M10 as closed findings | **Verify-then-fix** in Phase 3 polish, not Phase 1 blockers. |

---

## 1. Invariants (write these in code comments + tests)

```text
I1. CLUSTERED ⟺ this frame that (cid, instance) is in the cut/draw set
    (stable_cid_slots + instance_slots). Else DRAW_KIND_MESH.

I2. mesh_cache.pt_stamp(cid) is the sole freshness source.
    If pool.stamp or dags.stamp ≠ mesh stamp → drop pool+dag path and draw MESH
    on the same structural rebuild (no multi-frame wait).

I3. Bake candidates are driven by mesh density + mesh stamp, never by
    “draw_kind is not special” alone.

I4. Overflow (args cap, cid cap, instance cap) never erases geometry:
    fall back MESH or assert in debug; never special-without-draw.

I5. Per-body dual-draw forbidden: same body must not be source mesh + cluster
    multi_draw in G-buffer/shadows (regression: 168ee24).

I6. Terrain / heightfield demo bodies do not take the cluster path
    (owner two-track decision 01ab2ca) until clipmaps own them.
```

---

## 1b. Flicker + performance testing (mandatory)

**Rule:** A work package is **not done** until its **correctness tests** and its **phase flicker/perf gate** both pass.  
“Looks fine while debugging” is not a gate. Use the harnesses below and **write numbers into**  
`docs/planning/render/perf_matrix_logs/d4_finish_YYYYMMDD.md` (create if missing).

### Classes of flicker (name them — different phases own different classes)

| Class | Symptom | Typical root | Owned by phase |
|-------|---------|--------------|----------------|
| **F-DUAL** | Shimmer with **static** camera | source mesh + cluster double-draw (z-fight) | **1** (regression of 168ee24), **2** wire |
| **F-SET** | Shimmer / popping with **static** camera, set changes | cut membership thrash (reset race, bad error, overflow) | **1** (no thrash from routing), **3** cut stability |
| **F-LOD** | Moving seams / z-fight when **orbiting** | LOD cut handoff, sibling disagree, threshold band | **3** |
| **F-STALE** | Mesh “jumps back” or pick ≠ display after edit | stale DAG while stamp moved | **1** WP3 |
| **F-HOLE** | Interior missing patches (static or following camera) | incomplete cut, overflow drop, caps | **1** caps, **3** coverage |
| **F-SHADOW** | Shadow acne / swimming under sun | cascade cut / caster policy | **2** WP8 only if audit finds issue |

### Harness inventory (already in tree — use them)

| Env / tool | What it measures | Use in |
|------------|------------------|--------|
| `AZRA_SCOREBOARD=1` | `frame` / `med` / `gpu_ms` / `gpuSh` / `gpuGb` / `lay` | **every** phase |
| `AZRA_QUIT_AFTER_SECS=N` | unattended runs | CI-ish local |
| `AZRA_DENSE_STRESS=1x9` or `8x9` | **preferred D4 coverage scene** (~3.1M tris / body; non-terrain) | 0, 1, 3, 4 |
| `AZRA_CLUSTERS=1` / Tweaks D4 | enable draw path | all cluster gates |
| Demo landscape alone | **WP7 only** after terrain off path — props &lt; 50k tris, Terrain blocked → **0 clusters**; not a D4 verdict | 2 |
| `AZRA_CLUSTER_FORCE_L0=1` | fixed finest set — **exonerate selection** | flicker triage |
| `AZRA_CLUSTER_CUT_PASSTHROUGH=1` | all LODs — overdraw; proves cut was working | sabotage |
| `AZRA_CLUSTER_NO_VIS=1` | skip frustum/cone — isolate LOD cut | hole triage |
| `AZRA_CLUSTER_LOG=1` | bake/route/cut counters | 1–3 |
| `AZRA_FLICKER_WATCH=1` | cut `args_changes` / `geom_changes` vs samples (set thrash) | **1, 3** |
| `AZRA_FLICKER_SHOTS=<dir>` | N consecutive screenshots for pixel-diff | **1, 3, 4** |
| `AZRA_VISDIFF=<dir>` | self-driving visual shots | **3, 4** if available |
| Cluster debug view (Tweaks) | palette through solid = F-DUAL | **1, 2** |
| `AZRA_NO_CLUSTER=1` | bake off (baseline clean) | perf A/B |

### Standard perf recipes (copy-paste)

> **Do not use the demo landscape for D4 coverage.** After WP7 it has zero cluster
> candidates (`[ClusterBake] IDLE` will warn if clusters are on). Always pair
> `AZRA_CLUSTERS=1` with `AZRA_DENSE_STRESS=1x9` (or `8x9`). Wait for
> `[ClusterGpu] resident` before trusting visuals or scoreboard.

**P-BASE — clusters off (product default path)**

```text
AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=20 AZRA_DENSE_STRESS=1x9
# no AZRA_CLUSTERS
cargo run -p app --profile fast-release
```

Record: `med`, `gpu_ms`, `gpuSh`, `gpuGb` after warm (≥5 scoreboard lines, ignore first poll spike).

**P-ON — clusters on, same scene**

```text
AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=20 AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1
cargo run -p app --profile fast-release
```

Warm pipeline + wait until bake/upload done (`[ClusterGpu] resident` logs quiet), then record same fields.
Expect: bake job, then resident with tens of thousands of clusters, then cut counters
(`clSel` on scoreboard / `[ClusterCut]` with `AZRA_CLUSTER_LOG=1`).

**P-T2 — dense multi (perf only, after Phase 1 safety)**

```text
AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=25 AZRA_DENSE_STRESS=8x9 AZRA_CLUSTERS=1
```

Compare to plan table: clustered `gpu_ms` should not **regress >15%** vs last known good on same machine without a documented reason. Absolute T2 ≤16 ms remains the ladder; **per-PR gate is no surprise regression vs P-BASE/P-ON pair**.

**P-OFF-COST — stealth cost (Phase 2)**

```text
# Warm pool with clusters on once, then:
AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=15 AZRA_DENSE_STRESS=1x9
# clusters OFF, but if upload/cut still run, gpu_ms / cut labels show it
```

After WP5: cut must not run; `gpu_ms` within noise of pure P-BASE.

### Standard flicker recipes

**K-STATIC — static camera set thrash (F-SET / F-DUAL)**

```text
AZRA_CLUSTERS=1 AZRA_FLICKER_WATCH=1 AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=15
# optional: AZRA_FLICKER_SHOTS=./flicker_static AZRA_DENSE_STRESS=1x9
```

- Hold camera still (or harness with no orbit).
- **Pass (force L0):** `geom_changes == 0` (or ≈0) after warmup — membership fixed.
- **Pass (full cut):** `geom_changes` low; large continuous change with static cam = **FAIL** (F-SET).
- **Pass (shots):** consecutive PNGs max pixel-diff under threshold (or human: no shimmer).
- **Debug view:** palette must **not** show through full shaded surface (F-DUAL).

**K-ORBIT — LOD handoff (F-LOD)**

```text
AZRA_CLUSTERS=1 AZRA_FLICKER_SHOTS=./flicker_orbit AZRA_DENSE_STRESS=1x9 AZRA_QUIT_AFTER_SECS=20
# gentle auto orbit if available; else owner orbits 0.5°/frame class
```

- **Pass force L0:** no seam z-fight (L0 = source positions; if still fights → F-DUAL residual).
- **Pass full cut:** seams may move with LOD but **no hole flicker**; no dual-draw shimmer.
- Record: acceptable / fail + short note.

**K-EDIT — stale display (F-STALE)**

```text
AZRA_CLUSTERS=1 AZRA_CLUSTER_LOG=1
# After body clustered: apply a visible mesh edit (or stress edit if harness exists)
```

- **Pass:** next frame after stamp change shows **MESH** (or new topology), not pre-edit LOD.
- Pick highlight matches displayed surface.

**K-MULTI-INST — instance cap (F-HOLE / invisible)**

```text
AZRA_CLUSTERS=1
# scene: ≥6 instances of one ≥50k mesh (or multi_mesh stress if available)
```

- **Pass:** all instances visible (clustered and/or MESH fallback).

### Phase × required gates (summary)

| Phase | Flicker gates (required) | Perf gates (required) |
|-------|--------------------------|------------------------|
| **0** | K-STATIC + K-ORBIT **baseline log** (document fail classes) | P-BASE + P-ON med/gpu_ms logged (even if ON is broken) |
| **1** | K-STATIC (L0 + full), K-EDIT, K-MULTI-INST; no F-DUAL | P-BASE not regressed; P-ON must not be **worse than P-BASE by >10% gpu_ms without LOD win** after warm (if cut broken, note; after WP1–3 expect ON ≤ BASE or explain) |
| **2** | K-STATIC after wire fix; P-OFF-COST cut=0 | **P-OFF-COST:** clusters off gpu_ms ≈ P-BASE (no stealth cut) |
| **3** | K-STATIC full cut stable; K-ORBIT; force L0 vs full A/B; overflow=0 | P-ON **and** P-T2: no >15% gpu_ms regression vs Phase 0/1 baseline on same machine; T2 still ≤16 ms if 8x9 run |
| **4** | K-EDIT + Tab edit; sabotage suite shots | P-BASE / P-ON pair archived for owner sign-off |

### Pass/fail thresholds (defaults — adjust only with machine note)

| Metric | Gate |
|--------|------|
| Static cam `geom_changes` / `args_samples` after warm (force L0) | **0** membership changes |
| Static cam full cut `geom_changes` | **≤ 2%** of samples (or 0 if threshold fixed and cam fixed) — investigate any sustained thrash |
| P-OFF-COST vs P-BASE `gpu_ms` med | within **~5%** or absolute **0.2 ms** |
| P-ON vs prior P-ON after Phase 1+ | no **>15%** regression without written cause |
| P-T2 clustered `gpu_ms` | **≤ 16 ms** (ladder); log shadow/gbuf split |
| Overflow counter | **0** on standard dense stress |
| F-DUAL debug view | **zero** palette through solid |

### Who runs what

- **Agent / implementer:** automated scoreboard + FLICKER_WATCH numbers in the log file for every WP.  
- **Owner:** K-ORBIT visual accept for Phase 3/4; default-on sign.  
- **Do not skip Phase 1 flicker** even if unit tests pass — dual-draw and set thrash are runtime.

---

## 2. Work packages (ordered)

Execute **in order**. Each package: implement → unit tests → **phase flicker/perf gate** → commit.  
**Do not start package N+1 until N’s exit criteria and §1b gates for that phase pass.**

---

### Phase 0 — Hygiene + baseline flicker/perf (½–1 day)

**0.1 Doc truth**

- Update `PHASE_D4_CLUSTER_RASTER_PLAN.md` Status block:
  - T2 = **perf** PASS only.
  - Correctness: **open** (holes per HEAD).
  - Default-on: blocked on this finish plan.
  - Point to this document as the active checklist.
- Add `GPU_INVALIDATION_MATRIX.md` rows for **D3 bake** + **D4 pool/cut** (poll-only upload, gen keys, stamp policy).

**0.2 Baseline diagnostics (no product change)**

Record once on a **non-terrain** dense body (`AZRA_DENSE_STRESS=1x9` or UV sphere ≥50k), `AZRA_CLUSTERS=1`:

| Env | What it proves |
|-----|----------------|
| default cut | baseline holes / F-HOLE |
| `AZRA_CLUSTER_FORCE_L0=1` | if holes vanish → selection; if remain → draw/args |
| `AZRA_CLUSTER_NO_VIS=1` | cone+frustum isolation |
| cluster debug view | palette through solid = F-DUAL |
| `AZRA_FLICKER_WATCH=1` static | baseline `geom_changes` rate |
| P-BASE / P-ON scoreboard | baseline med + gpu_ms |

Save under `docs/planning/render/perf_matrix_logs/d4_finish_baseline.md`.

**Exit:**

- [ ] Baseline scene is **not** heightfield demo for coverage work  
- [ ] P-BASE + P-ON numbers written  
- [ ] K-STATIC + K-ORBIT baseline notes written (may FAIL — that is OK; log the class)

---

### Phase 1 — Never invisible / never stale (blockers B1–B3)

**This is the core “fix everything that erases geometry.”**  
**Flicker classes owned here:** F-DUAL regression, F-STALE, F-HOLE from caps, F-SET from bad routing.

#### WP1 — B1 instance + cid cap safety (primary landmine first)

**Problem**

- `MAX_INSTANCES_PER_CID = 4`: common (many instances, one dense cid). Remap all → CLUSTERED; cut only first 4 → **instances 5+ invisible**.
- `MAX_CLUSTERED_CIDS = 8`: remap all pool cids; cut only top-8 sorted → **overflow cids invisible**.

**Files**

- `core_engine/app/src/raster_gbuffer.rs` (remap ~851–861, cut fill ~1608–1617, multi_draw loops)
- `core_engine/render/src/cluster_cut.rs` (`stable_cid_slots`, caps constants)
- Optional: small helper `cluster_route.rs` or functions on `ClusterCutSystem`:
  - `fn routed_cids(pool) -> HashSet<MeshCacheId>`
  - `fn can_route_instance(cid, structural_index, slots) -> bool`

**Implementation**

1. Build the **same** ordered slot list the cut will use **before** remap (or restructure so remap runs after a pure “slot assignment” pass).
2. Remap `MESH → CLUSTERED` only if:
   - `cid` is among `stable_cid_slots` (≤8), **and**
   - this structural draw’s instance is among the ≤4 instance slots filled for that cid this frame.
3. All other dense residents stay **`DRAW_KIND_MESH`** (full source path).
4. Scoreboard / `AZRA_CLUSTER_LOG`:
   - `cluster_route_cid_overflow`
   - `cluster_route_inst_overflow`
   - never silent.

**Tests**

| Test | Pass |
|------|------|
| 1 cid, 8 instances, clusters on | all 8 visible (4 cluster + 4 mesh **or** all mesh if policy is mesh-only for overflow — prefer **mesh fallback for overflow instances**, first 4 clustered) |
| 9 unique ≥50k cids resident | all 9 visible (8 cluster + 1 mesh) |
| Toggle clusters off | all MESH, full visible |

**Flicker / perf (WP1):**

| Gate | Pass |
|------|------|
| **K-MULTI-INST** | all instances visible; no pop when orbiting multi-instance set |
| **K-STATIC** force L0 | `geom_changes≈0`; no F-DUAL (debug view) |
| **P-ON** after warm | body count on screen matches scene; log `gpu_ms` |

**Exit:** no scene can mark CLUSTERED without a multi_draw path for that instance; **K-MULTI-INST green**.

---

#### WP2 — B2 bake candidates while CLUSTERED

**Problem**

`poll_cluster_bake` skips `draw_kind_is_special` → after first remap, CLUSTERED never re-enters candidates → no re-bake after edit.

**Files**

- `core_engine/app/src/app_poll_gpu.rs` (`poll_cluster_bake` ~86–107)
- Possibly `cluster_bake.rs` `consider` only if needed

**Implementation**

1. Candidate selection = structural draws whose **mesh** has ≥ `CLUSTER_TRI_THRESHOLD` tris, **including** `DRAW_KIND_CLUSTERED` (and MESH). Still exclude SDF/voxel special kinds by kind allowlist (MESH | CLUSTERED only), not “not special.”
2. Always pass **live** `mesh_cache.pt_stamp(cid)` into `consider`.
3. Log under `AZRA_CLUSTER_LOG` when a CLUSTERED cid is re-considered after stamp change.

**Tests**

| Test | Pass |
|------|------|
| Cluster body, edit topology, wait debounce | new bake spawns (log or dags stamp updates) |
| Unit: mock structural kinds CLUSTERED + new stamp | `consider` not early-out solely due to kind |

**Flicker / perf (WP2):** no standalone flicker gate — **must ship with WP3** (bake alone leaves F-STALE window).

**Exit:** edit while clustered always schedules a new bake after debounce (WP3 removes the wrong-display window during wait).

---

#### WP3 — B3 immediate MESH on stamp mismatch (standalone)

**Problem**

Even with WP2, timeline is: edit → 500 ms debounce → build → drain → new dag → pool. Until then, **viewport can show pre-edit clusters** while **pick uses new mesh**.

**Files**

- `core_engine/app/src/app_poll_gpu.rs` (retain_fresh + before upload)
- `core_engine/app/src/cluster_bake.rs` (`consider` / `drain`)
- `core_engine/render/src/cluster_gpu.rs` (`retain_fresh` API if needed)
- `raster_gbuffer.rs` remap (only route if stamps match)

**Implementation**

1. **Every poll**, for each cid in `dags` or `cluster_pool`:
   - `live = mesh_cache.pt_stamp(cid)`
   - if `None` (mesh gone) → drop dag + pool entry.
   - if `live != dags.stamp` or `live != pool.stamp` → **drop dag entry and pool entry immediately**, force `structural_draw_stale = true`.
2. `retain_fresh` must take **mesh stamp**, not “whatever dags says is current.”
3. On stamp mismatch in `consider`: drop stale `dags[cid]` first (don’t keep old Arc as truth).
4. Remap only if `pool.entry.stamp == mesh.pt_stamp(cid)`.
5. Debounce remains for **rebuild**, not for **stale draw**.

**Tests**

| Test | Pass |
|------|------|
| Edit clustered mesh frame 0 | next frame draw_kind MESH (or not CLUSTERED); pick and display both new topology |
| After debounce + bake | may return to CLUSTERED with new stamp |
| Delete body | pool/dags no longer hold cid |

**Flicker / perf (WP3):**

| Gate | Pass |
|------|------|
| **K-EDIT** | no F-STALE (display matches new mesh same rebuild); pick matches display |
| **K-STATIC** after re-cluster | no set thrash; optional FLICKER_SHOTS before/after edit |
| **P-ON** | no multi-frame `gpu_ms` spike loop from thrash rebake (log 10s after edit) |

**Exit:** zero multi-frame window of “old clusters + new pick”; **K-EDIT green**.

---

#### WP4 — Bake job hygiene (H4) + scene prune (H5)

**Files:** `cluster_bake.rs`, `app_poll_gpu.rs`, project load path

**Implementation**

1. **One exclusive in-flight job per cid** (any stamp): don’t spawn stamp2 while stamp1 running without gen; only clear `in_flight` if result stamp matches current flight.
2. Validate-fail path **always** signals completion (send fail result or clear inflight) so cid is not stuck forever.
3. Each poll: prune `dags` / `dirty_since` / pool to **live candidate set ∪ scene mesh_cache**.
4. Project new / load: clear `cluster_bake` + `cluster_pool` + cut bind cache gen.

**Tests:** invalid DAG unblocks; two rapid edits don’t leave permanent inflight; scene load empty pool.

**Flicker / perf (WP4):**

| Gate | Pass |
|------|------|
| Rapid edit storm | no F-STALE; no permanent stuck CLUSTERED; P-ON `gpu_ms` returns to warm baseline within ~2s after storm |
| Scene load / new | pool empty → P-BASE-like until bake |

**Exit:** no stuck jobs, no zombie VRAM eating the 8 slots.

#### Phase 1 package exit (all WP1–4)

- [ ] **K-STATIC** force L0: `geom_changes≈0`, no F-DUAL  
- [ ] **K-STATIC** full cut: no sustained F-SET thrash  
- [ ] **K-EDIT** green  
- [ ] **K-MULTI-INST** green  
- [ ] **P-BASE** med/gpu_ms not regressed vs Phase 0 baseline (>5% needs note)  
- [ ] **P-ON** numbers logged next to Phase 0  

---

### Phase 2 — Feature policy + residual draw paths

**Flicker classes:** F-DUAL via wire, stealth perf, terrain mis-route.  
**Perf class:** cut must not tax default-off path.

#### WP5 — Gate cut (and optionally upload) on clusters draw flag (H6)

**Files:** `raster_gbuffer.rs` cut block ~1576; optionally `poll_cluster_bake` upload arm

**Implementation**

1. Cut encode only if `clusters_on && !pool.empty()`.
2. Policy choice (pick one, document):
   - **A (recommended for ship):** bake CPU DAG allowed when off; **GPU upload only when clusters_on** (or first enable).
   - **B:** bake+upload only when clusters_on.
3. Default recommendation: **A** so first enable is faster, but no cut cost when off.

**Flicker / perf (WP5):**

| Gate | Pass |
|------|------|
| **P-OFF-COST** | clusters off: `gpu_ms` med within **5% / 0.2 ms** of P-BASE; no cut encode when `dbg_clusters=false` |
| **P-ON** | still works after warm; no extra hitch vs Phase 1 P-ON |

**Exit:** `AZRA_CLUSTERS=0` → zero cut dispatches; **P-OFF-COST green**.

---

#### WP6 — Wire overlay CLUSTERED skip (H7)

**Files:** `raster_gbuffer.rs` wire loop (~2389+)

**Implementation:** same `continue` as G-buffer mesh path for `DRAW_KIND_CLUSTERED` (or multi_draw cluster into wire — skip is enough for correctness/perf).

**Flicker / perf (WP6):**

| Gate | Pass |
|------|------|
| Wire ON + clusters + **K-STATIC** | no F-DUAL shimmer; debug palette not through solid |
| **P-ON** wire on | `gpu_ms` not doubled vs wire off (no full source re-draw) |

**Exit:** wire + clusters does not re-introduce full-source dense draw for solid path.

---

#### WP7 — Two-track: terrain off clusters (owner 01ab2ca)

**Files:** bake candidates + remap; demo scene markers; docs

**Implementation**

1. Detect heightfield / landscape demo / explicit flag on body (whatever owner track doc defines — follow `PHASE_D_TWO_TRACK_GEOMETRY.md`).
2. Those bodies never enter bake candidates and never remap to CLUSTERED.
3. Coverage work uses non-terrain dense meshes only.

**Flicker / perf (WP7):** demo landscape with clusters on → terrain draws **MESH** only; **P-ON** on non-terrain body still clusters. No hole-hunting on heightfield.

**Exit:** demo terrain cannot take cluster path; D4 hole hunting not blocked by wrong geometry class.

---

#### WP8 — Shadow cascade caster audit (former H3 — investigate)

**Files:** `shadows.rs` / cascade VP build; cluster shadow cut params in `raster_gbuffer.rs`

**Implementation**

1. Document how `cascade_vp` is built (receiver vs caster extent).
2. If extended toward light / caster-safe → **close as N/A**.
3. If tight ortho only → expand bounds or skip frustum on shadow cluster cut; add a small shadow-caster regression scene.

**Flicker / perf (WP8):** if code changes: orbit sun-lit dense cluster — no F-SHADOW swimming vs clusters-off shadows; `gpuSh` in P-ON not worse than Phase 1 without reason.

**Exit:** written decision + test or “no change needed.”

#### Phase 2 package exit

- [ ] **P-OFF-COST** green  
- [ ] Wire does not reintroduce F-DUAL  
- [ ] Terrain not on cluster path  
- [ ] Shadow audit closed (fix or N/A)  

---

### Phase 3 — Coverage holes (B5, corrected scope)

**Do not use 617 vs 853.** Use current HEAD: holes remain after LOD gradient fix (`0c0944e`).  
**Flicker classes owned here:** F-LOD, F-HOLE, F-SET from cut math.  
**Perf:** P-ON + **P-T2** required before calling coverage “improved.”

#### WP9 — Cone + visibility test gap (narrow H1) — **landed**

**Implementation**

1. `CutVis` + `cut_selects_world_vis` / `cut_replay_world_vis` in `cluster_gpu.rs` —
   same frustum + cone predicates as WGSL (world-space model, smax, near-uniform cone).
2. `CutVis::error_only()` ≡ dump default / `AZRA_CLUSTER_NO_VIS` (no frustum, skip cone).
3. Unit: frustum can empty the cut; non-culling planes match error-only set.
4. Live A/B still: owner runs K-STATIC with/without `AZRA_CLUSTER_NO_VIS=1`.

**Exit:** CPU can reproduce GPU visibility drops; instrument no longer error-only only.

---

#### WP10 — Sibling parent_error lockstep — **closed REJECTED**

Sibling `parent_error` max + shared parent sphere broke open-edge / terrain hole tests.
Co-simplified meshlets may sit under different next-level groups. Coverage remains
strict self < parent bands (v4) + partition open-edge tests + world cut replay.
Do not re-land sibling max without a new hole repro that needs it.

---

#### WP11 — Args overflow policy (B4) — **landed**

**Files:** `raster_gbuffer.rs` counter drain, `types_render` scoreboard fields, scoreboard line

**Implementation**

1. `cluster_cut_selected` / `cluster_cut_overflow` always updated from counters[4]/[5].
2. Overflow always logged (first + every 60 samples); scoreboard `clSel`/`clOv`/`clMeshFb`.
3. Streak ≥ 2 with overflow → `cluster_overflow_mesh_fallback`: every frame demotes
   CLUSTERED→MESH and skips remap until overflow clears.
4. Cap remains `MAX_VISIBLE_CLUSTERS_PER_CID` (16 384); count path already clamps.

**Exit:** no silent F-HOLE from cap; overflow visible on scoreboard; policy reverts when clear.

---

#### WP12 — GPU cut agreement gate (plan commit 8, done right)

**Files:** CUT_DUMP / counter path in `raster_gbuffer.rs`; `cluster_gpu.rs` world replay

**Implementation**

1. Dump uses **live** `threshold_px` (env `AZRA_CLUSTER_THRESHOLD_PX`, not hardcode 1.0).
2. Dump uses **instance model matrices** from the draw list (same order as cut
   `instance_slots`) via `cut_replay_world_union` — lockstep with WGSL `smax` +
   `model * sphere`. Identity-only replay is **invalid** on translated instances
   (dense-stress spacing 3.0); empty models → skip agreement, never silent identity.
3. Agreement: GPU ⊆ world-space error-cut; with NO_VIS, sets match; frustum/cone may
   remove more (expected).
4. One-shot env stays; unit test `world_cut_respects_instance_translation`.

**Flicker / perf (WP12):** dump agreement holds while **K-STATIC** / **K-ORBIT** still green (dump must not itself thrash the frame path in normal use — env one-shot only).

**Exit:** reliable dump on multi-instance scenes; no false “0 drawn” / false mismatches from instrument space mismatch.

#### Phase 3 package exit

**Code landed (2026-08-05):** strict bands v4, world CUT_DUMP, CutVis, overflow policy,
projected-band monotonicity (`projected_cut_band` / clusterlod), coverage probe,
args cap 64k, camera cone off by default.

- [x] Unit: partition / terrain / dense-cc / projected-band hole-free (26 tests)  
- [x] deep_dag --ignored green on ~70k (not a 3.1M disproof)  
- [ ] **K-STATIC** full cut stable (FLICKER_WATCH thresholds) — **owner**  
- [ ] **K-ORBIT** owner accept on **AZRA_DENSE_STRESS=1x9** (not demo landscape)  
- [ ] force L0 vs full A/B documented — **owner**  
- [ ] overflow 0 on 1x9 / 8x9 — scoreboard `clOv` / `clCam`  
- [ ] **P-ON** + **P-T2** logged — **owner**  
- [ ] T2 ≤16 ms on reference machine — **owner**  

**Preferred D4 scene:** `AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1` (demo landscape = WP7 only).

---

### Phase 4 — Product safety before default-on

**Flicker:** F-STALE under Tab edit; sabotage suite.  
**Perf:** archive final P-BASE / P-ON / P-T2 for owner.

#### WP13 — Minimal Edit drop-out (Q2 slice) — **landed**

**Files:** `raster_gbuffer.rs` remap / per-frame demote

**Implementation**

1. When `ModelingContext::Polygonal` and `poly_work_mode != Object`, collect the active
   body's `mesh_cache_id` and **never** remap it to CLUSTERED; demote any already-
   CLUSTERED draws of that cid to MESH every frame.
2. Same-frame continuous source mesh; no empty frame; no dual cluster+wire.
3. Full dirty-region recluster remains D8. Stamp drop (WP3) still clears stale DAGs.

**Exit:** Tab into poly Edit on clustered body → immediate source mesh; no dual-draw.

---

#### WP14 — Lazy debug stream + cache key polish — **landed**

**Files:** `cluster_gpu.rs` upload / `ensure_debug_stream`; `cluster_build.rs` `dag_cache_key`

**Implementation**

1. Debug vertex/index streams built only when `dbg_cluster_view` at upload; otherwise
   tiny stubs. Toggle-on after residency: `ensure_debug_stream` fills ≤1 cid/poll.
2. Draw binds debug buffers only when `has_debug_stream` (else production verts).
3. Cache key remains **positions + indices + params + builder version** (topology /
   position only). Normals always supplied at cold build from mesh snapshot; mesh
   `pt_stamp` invalidates pure normal edits. **No version bump.**

---

#### WP15 — Sabotage suite + owner visual + default-on decision

**Harness** — run with §1b recipes (owner / agent log). **Always pair clusters with
dense stress** after WP7 (demo landscape has 0 cluster candidates).

| Env / toggle | Expected | Gate |
|--------------|----------|------|
| (default) clusters off | full mesh, no cut | P-BASE / P-OFF-COST |
| `AZRA_DENSE_STRESS=1x9` only | solid source mesh | P-BASE |
| `AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1` | `[ClusterGpu] resident` then solid LOD | P-ON, K-ORBIT |
| Tweaks force L0 | stable coverage | K-STATIC geom≈0 |
| Tweaks passthrough | overdraw, cut was culling | gpu_ms up, no holes |
| `AZRA_CLUSTER_NO_VIS=1` | isolates LOD vs vis | hole A/B |
| `AZRA_CLUSTER_CONE=1` | re-enable camera cone | optional |
| Tab poly Edit on clustered | MESH drop-out (WP13) | K-EDIT |
| `AZRA_DENSE_STRESS=8x9` | T2 ladder | P-T2 ≤16 ms gpu |
| `AZRA_CLUSTER_CUT_DUMP=1` | world-space agreement | WP12 |
| Scoreboard `clCam` / `clOv` / `clMeshFb` | cam count; ov=0 or mesh_fb | WP11 |

**Copy-paste owner visual (post-rebuild):**
```text
AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1 AZRA_SCOREBOARD=1 AZRA_CLUSTER_LOG=1
```
Wait for `[ClusterGpu] resident`. Orbit. Expect solid body, not islands.
`clCam` = G-buffer multi_draw count; `clSel` = camera+cascades sum (often higher).
1x9 body is at **origin** (multi-body grid keeps spacing 3.0).

**Force L0 vs full A/B (exonerate selection):**
```text
# A — full cut (default)
AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1 AZRA_SCOREBOARD=1 AZRA_CLUSTER_LOG=1

# B — force L0 only (fixed finest set; camera-independent)
AZRA_DENSE_STRESS=1x9 AZRA_CLUSTERS=1 AZRA_CLUSTER_FORCE_L0=1 AZRA_SCOREBOARD=1 AZRA_CLUSTER_LOG=1
```
| Observation | Meaning |
|-------------|---------|
| Holes in A, solid in B | Cut / LOD selection bug |
| Holes in A and B | Draw path / args / residency (not selection) |
| Solid in A and B | Selection OK; remaining issues are LOD quality or perf |

**Default-on:** still **OFF** (`dbg_clusters` default false; only `AZRA_CLUSTERS=1` or Tweaks).
Flip only after owner signs non-terrain dense scene + full §1b Phase 3–4 gates.

#### Phase 4 package exit

- [x] Code: WP13 edit drop-out, WP14 lazy debug  
- [x] Code: projected-band cut + overflow/coverage MESH safety  
- [ ] Full sabotage matrix run logged (owner)  
- [ ] Final **P-BASE / P-ON / P-T2** table in `perf_matrix_logs/`  
- [ ] Owner visual on **K-ORBIT** + static on **1x9**  
- [x] Default-on still **OFF**

---

## 3. Suggested commit series (one concern each)

| Commit | Title | Phase |
|--------|-------|-------|
| 1 | `fix(d4): route CLUSTERED only for slotted cids/instances; overflow → MESH` | WP1 |
| 2 | `fix(d4): bake candidates include CLUSTERED; stamp-driven re-bake` | WP2 |
| 3 | `fix(d4): immediate pool/dag drop + MESH on mesh stamp mismatch` | WP3 |
| 4 | `fix(d3): exclusive in_flight + prune dead cids + scene clear` | WP4 |
| 5 | `perf(d4): gate cut (and upload policy) on dbg_clusters` | WP5 |
| 6 | `fix(d4): skip CLUSTERED on wire path` | WP6 |
| 7 | `feat(d4): two-track — heightfield/terrain never clusters` | WP7 |
| 8 | `docs+fix(d4): cascade_vp caster audit` | WP8 |
| 9 | `test(d4): cone/frustum-aware cut replay + sibling parent lockstep` | WP9–10 |
| 10 | `fix(d4): overflow policy + GPU cut dump agreement` | WP11–12 |
| 11 | `fix(d4): poly Edit forces MESH drop-out` | WP13 |
| 12 | `perf(d4): lazy debug streams + cache validate` | WP14 |
| 13 | `docs(d4): finish plan status + sabotage suite; default-on still off` | WP15 |

**After every draw-path commit (WP1–3, 5–7, 9–13):**

1. **Perf:** P-BASE + P-ON (and P-T2 after Phase 3) — paste med/`gpu_ms`/`gpuSh`/`gpuGb` into the log.  
2. **Flicker:** K-STATIC with `AZRA_FLICKER_WATCH=1` (force L0 once per phase; full cut on Phase 1+3).  
3. **Phase 1 also:** K-EDIT + K-MULTI-INST.  
4. **Phase 3 also:** K-ORBIT (shots or owner note).  
5. Fail the commit review if gates skipped.

---

## 4. File ownership map

| Area | Primary files |
|------|----------------|
| Routing invariant | `raster_gbuffer.rs`, `cluster_cut.rs`, `raster_common.rs` |
| Bake / stamp | `cluster_bake.rs`, `app_poll_gpu.rs` |
| Pool | `cluster_gpu.rs` |
| Cut math / tests | `cluster_cut.wgsl`, `cluster_gpu.rs`, `cluster_build.rs` |
| Shadows audit | shadow cascade setup + `raster_gbuffer.rs` cut params |
| Feature flag | `types_render.rs` / `app_new.rs` / `win_tweaks.rs` |
| Docs | this file, `PHASE_D4_CLUSTER_RASTER_PLAN.md` Status, `GPU_INVALIDATION_MATRIX.md` |

---

## 5. Definition of done

### D4 "finish" for opt-in (must all be true)

**Correctness**

- [ ] I1�I6 hold under tests above
- [ ] No invisible instances/cids when clusters on
- [ ] Edit ? same-frame MESH until new DAG ready
- [ ] Bake re-triggers while CLUSTERED
- [ ] Cut cost 0 when clusters off (WP5)
- [ ] Non-terrain dense mesh: no severe interior holes under force-L0 / no-vis A/B
- [ ] Terrain not on cluster path
- [ ] Double-draw regression still prevented

**Flicker (section 1b)**

- [ ] F-DUAL: no palette through solid; static cam clean (force L0 + full)
- [ ] F-SET: FLICKER_WATCH within thresholds
- [ ] F-STALE: K-EDIT green
- [ ] F-LOD / F-HOLE: K-ORBIT accepted on non-terrain; overflow 0
- [ ] Owner visual static + orbit on non-terrain

**Performance (section 1b)**

- [ ] P-BASE / P-ON pair logged (warm med + gpu_ms + gpuSh + gpuGb)
- [ ] P-OFF-COST � P-BASE when clusters off
- [ ] P-T2 clustered gpu_ms =16 ms when 8x9 run on reference machine
- [ ] No unexplained >15% P-ON regression vs Phase 1 baseline

### Default-on (separate decision)

- [ ] All of the above
- [ ] Commit-8-style GPU agreement gate reliable
- [ ] Full sabotage suite + flicker/perf log archived
- [ ] Owner explicitly flips default

---

## 6. Risk register

| Risk | Mitigation |
|------|------------|
| MESH fallback for overflow looks LOD-inconsistent across instances | Prefer first N clustered, rest MESH; document; later D7 multi-dispatch |
| Debounce feels laggy after edit | Immediate MESH (WP3) hides lag; bake stays async |
| Hole hunting stuck on terrain | WP7 two-track first |
| Scope creep into HZB/vis-buffer | Non-goals list; refuse in PR review |
| Restoring inside-sphere INF | Forbidden in section 0 |
| Unit tests green but still shimmers | Section 1b mandatory � FLICKER_WATCH + shots |
| Perf "feels fine" without numbers | P-BASE/P-ON required in log every phase |

---

## 7. Execution notes for agents

1. Work only in **viewport-perf** / feat/viewport-perf.
2. One WP per PR/commit when possible.
3. After each WP: cargo test -p render + cargo check -p app.
4. **After each phase:** run section 1b gates; append to docs/planning/render/perf_matrix_logs/d4_finish_*.md.
5. Do not enable default clusters in this plan commits.
6. Prefer fall back to MESH over raising caps without streaming.
7. Cite current HEAD for coverage, not 617/853.
8. Flicker triage: force L0 vs full cut vs NO_VIS before changing cut math.

---

## 8. Summary for the owner

| Phase | What you get | Flicker gate | Perf gate |
|-------|----------------|--------------|-----------|
| **0** | Baseline truth | log fails OK | P-BASE / P-ON logged |
| **1** | No erase / freeze | K-STATIC, K-EDIT, K-MULTI-INST | P-BASE stable; P-ON logged |
| **2** | No stealth cut; wire/terrain | no F-DUAL via wire | **P-OFF-COST** |
| **3** | Coverage / LOD | K-ORBIT + FLICKER_WATCH | **P-ON + P-T2** |
| **4** | Edit-safe opt-in | sabotage + owner visual | archive final numbers |

**Start here:** WP1 then WP3. **Phase 1 is not done without K-EDIT + K-MULTI-INST + K-STATIC + P-BASE/P-ON numbers.**