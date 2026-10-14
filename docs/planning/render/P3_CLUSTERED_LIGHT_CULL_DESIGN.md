# P3.1 — Clustered / CPU Light Cull with Index Mapping Design

**Status:** Phase A+B+C+D **SHIPPED** on perf/pbr-lights (2026-07-19)  
**Branch / worktree:** `perf/pbr-lights`  
**Date:** 2026-07-19  
**Depends on:** P0–P2.2 light perf (`PASS_RT_SHADOW`, dual RT packs, vol bitfield)  
**Impl:** core_engine/app/src/light_pack.rs + wire in scene_actions / iewport_render / HUD.
- Phase A: single packer → lights + shade-order link entries + vol_bits; prim_uid→flat resolve
- Phase B: top-K score cull (MAX_SHADE=8), sun→shade[0], force selected + non-Default link lights; PT full list
- Tests: 7 unit tests under light_pack::tests

**Why blocked before:** Reordering or culling the GPU light buffer without remapping **breaks light linking**, RT pack channels, and vol bitfields.

---

## 0. Goal

Enable **more than 8 authored lights** (and better use of the 8-slot shade budget) by:

1. **Selecting** which lights matter for the current frame (frustum + range + importance)  
2. **Packing** a compact GPU shade list (`N ≤ MAX_SHADE`, default 8)  
3. **Preserving** light-linking semantics under that remapping  
4. Leaving a clean path to true **clustered deferred** (per-tile light lists) later  

Success metric (1080p PBR, Medium quality, RT hard on):

| Scene | Today (est.) | Target |
|-------|----------------|--------|
| 1 sun + 4 points | baseline | same look |
| 1 sun + 16 points (4 near, 12 far) | shades first 8 visible only | shades **8 most relevant** |
| Include/Exclude linking on reordered list | N/A / broken if reordered | **identical** to pre-cull semantics for selected lights |

---

## 1. Current identity model (as shipped)

### 1.1 Three different “light indices”

| Name | Where | Meaning | Stability |
|------|--------|---------|-----------|
| **Author light id** | `LightData.id` | Stable scene identity (outliner, save/load, UI) | Stable across frames |
| **Visible enumerate order** | `scene_graph.lights().filter(visible)` | Order of GPU upload today | Changes if visibility / graph order changes |
| **GPU shade index `li`** | `light_data` slot `i` after header | What shaders loop: `0..min(num_lights, 8)` | **Assumed == visible enumerate order** |

Today:

```
serialize_lights_gpu():
  visible = lights().filter(visible)   // graph order
  light_data = [count | light0…lightN-1]  // 22 floats each

build_light_link_masks(light_snapshot):
  light_snapshot built in SAME visible order
  bit N on prim P ⇔  “GPU light index N affects prim P”
```

### 1.2 Light linking GPU contract

```16:51:core_engine/app/src/ui_update.rs
// Each prim gets 2 u32 (64 bits). Bit N = 1 means light N affects this prim.
// Include/Exclude operate on GPU index li, not LightData.id.
```

```wgsl
// sdf_lighting.wgsl
fn light_linked(prim_id: u32, light_idx: u32) -> bool {
    // bit light_idx of that prim's mask
}
// deferred_entry.wgsl
for (var li = 0u; li < min(num_lights(), 8u); li++) {
    if (!light_linked(prim_sid, li)) { continue; }
    // shade light_data[li]
}
```

**Critical invariant:**  
`light_link_masks` bit `li` must refer to the **same physical light** as `light_data` record `li`.

### 1.3 Other consumers of GPU index

| Consumer | Cap | Dependency on GPU index |
|----------|-----|-------------------------|
| Deferred / Lit loop | 8 | `li` + link bit `li` |
| RT shadow packs | 8 (2×RGBA) | channel / pack = GPU `li` |
| `fog_has_vol_lights` bitfield | 32 bits | bit `i` = GPU light `i` has VOL |
| PT path | up to 256 | `num_lights()` order |
| Ambient special-case | light `li==0` | first GPU light contributes ambient |

### 1.4 Why naive reorder/cull breaks things

| Naïve change | Failure |
|--------------|---------|
| Sort lights by camera distance, upload first 8 | Link bit 3 still means “3rd **visible graph order** light”, not “3rd **sorted** light” → wrong Include/Exclude |
| Drop far lights from buffer without remap | Same |
| Only rebuild light buffer, not link masks | Masks and lights desync |
| Rebuild masks in new order but forget RT/vol bitfields | Wrong VOL/RT channels |
| Use `link_targets` as flat prim index (`uid as usize`) | **Pre-existing fragility** (see §7) — design must fix or document |

---

## 2. Design principles

1. **Author space is stable** — `LightData.id` never becomes a GPU loop index.  
2. **GPU shade list is ephemeral** — rebuilt when camera/lights/dirty flags change.  
3. **Linking is evaluated in author space, applied in shade space** — never “hope” order matches.  
4. **Single source of truth for packing** — one CPU function produces lights + masks + vol bits + RT order.  
5. **Deferred stays simple** — still loops `0..shade_count` with bitmasks; clustering is optional later.  
6. **Caps are explicit** — `MAX_SHADE = 8` (today’s deferred/RT), `MAX_LINK_BITS = 64`, `MAX_AUTHOR` unbounded (practical soft cap 64 for masks).

---

## 3. Recommended architecture: **Shade List + Remap Table**

### 3.1 Overview

```
┌─────────────────────────────────────────────────────────────┐
│ AUTHOR SPACE (stable)                                       │
│  lights[] by LightData.id / graph order                     │
│  link_mode + link_targets (prim uid or flat index — §7)     │
└──────────────────────────┬──────────────────────────────────┘
                           │  CPU each dirty frame
                           ▼
┌─────────────────────────────────────────────────────────────┐
│ SELECTOR (importance / cull)                                │
│  candidates = visible lights                                │
│  score = f(type, dist, screen footprint, intensity, VOL/RT) │
│  shade_list = top K (K ≤ MAX_SHADE=8), with rules:          │
│    - always include first Directional (sun) if any          │
│    - prefer RT-enabled points when RT on                    │
│    - hard includes for selected / solo lights               │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│ MAPPING                                                     │
│  shade_to_author[li] = LightData.id   (or author_slot)      │
│  author_to_shade[id] = Option<li>     (None = culled)       │
└──────────────────────────┬──────────────────────────────────┘
                           │
           ┌───────────────┼───────────────┐
           ▼               ▼               ▼
    light_data[]     link masks      vol bitfield
    (shade order)    (shade bits)    (shade bits)
           │               │               │
           └───────────────┴───────────────┘
                           ▼
                    GPU deferred / RT / fog
```

### 3.2 Data structures (CPU)

```rust
/// One frame's compact shade list.
struct LightShadeList {
    /// Author light ids in GPU shade order (length ≤ MAX_SHADE).
    shade_ids: Vec<u32>,
    /// Reverse map: LightData.id → shade index, if packed this frame.
    id_to_shade: HashMap<u32, u8>,
    /// Full visible count before cull (for HUD / debug).
    visible_count: u32,
    /// Culled count (visible - shade).
    culled_count: u32,
}

const MAX_SHADE: usize = 8;      // deferred + RT dual pack
const MAX_LINK_LIGHTS: usize = 64; // bitmask width (2×u32)
```

**Never store remap on LightData** — it is frame-local, like `cached_light_gpu_data`.

### 3.3 Selector scoring (v1 — CPU, no clusters)

For each **visible** light:

| Factor | Weight idea | Notes |
|--------|-------------|--------|
| Type | Directional: force include (slot 0 preferred) | Sun always shades |
| Distance | Point/Spot: `1 / (1 + dist²)` from camera or target | Cheap |
| Range | If `dist > range * 1.1` → score 0 (cull candidate) | Matches atten early-out |
| Intensity | `intensity * luminance(color)` | Dim lights lose |
| Spot cone | Bonus if camera target in cone | Optional v1.1 |
| VOL enabled | Small bonus if fog/vol active | Keeps god rays |
| RT enabled | Small bonus if global RT on | Fills RT packs usefully |
| Selected | Force include | Authoring UX |
| Solo / only light of type | Force include | Avoid black scene |

**Tie-break:** lower graph order, then lower id (deterministic).

**Hard rules (non-negotiable):**

1. If ≥1 visible Directional exists → at least one Directional in shade list (prefer highest intensity).  
2. If shade list would be empty but visible lights exist → pack closest / brightest until `K`.  
3. Culled lights **do not** affect surfaces this frame (acceptable for far fill lights; document).

### 3.4 Building GPU buffers (single function)

Replace ad-hoc `serialize_lights_gpu` + separate `light_snapshot` with:

```text
fn build_frame_lights(app) -> FrameLightGpu {
    1. enumerate visible lights → AuthorLight[]
    2. select shade_list (scores + hard rules)
    3. pack light_data in shade order (same 22-float layout)
    4. build link masks IN SHADE SPACE (see §4)
    5. build fog_has_vol_lights bitfield IN SHADE SPACE
    6. return { light_data, link_masks, vol_bits, shade_list, debug }
}
```

Call sites (must stay in lockstep):

| Site | Today | After |
|------|--------|--------|
| `cached_light_gpu_data` | `serialize_lights_gpu()` | `build_frame_lights().light_data` |
| `build_light_link_masks` | visible order | shade-order modes + remapped bits |
| `gu.fog_has_vol_lights` | `volumetric_light_bitfield()` visible order | shade-order bits |
| RT packs | GPU `li` 0..7 | same, but `li` is shade index |

**Dirty flags:** rebuild when any of:

- `DirtyFlags::LIGHT`  
- camera moved beyond threshold (or every frame while orbiting if list is cheap)  
- selection change (force-include)  
- fog/vol toggle / RT toggle (scoring)

Camera-only rebuild can re-use author data without re-reading graph payloads if we cache `AuthorLight` snapshots.

---

## 4. Light linking remap (the core mapping)

### 4.1 Correct algorithm

Author-space linking is defined as:

> Light **L** (id) affects prim **P** under mode Default / Include / Exclude.

Shade-space mask must satisfy:

> For every shade index `li` packing author light `id = shade_ids[li]`:  
> `mask_bit(P, li) == author_affects(id, P)`.

```text
// Pseudocode
masks = all_ones(num_prims)  // Default baseline

for (li, id) in shade_list.enumerate():
    light = author[id]
    match light.link_mode:
      Default:
        // bit already 1 — nothing
      Include:
        clear bit li on ALL prims
        for target in light.link_targets:
          set bit li on prim(target)
      Exclude:
        for target in light.link_targets:
          clear bit li on prim(target)
```

This is **exactly** `build_light_link_masks` today, **if and only if** the input list is already in shade order.

**So remap = select order first, then run existing mask builder on the shade-ordered list.**  
No shader change required for linking.

### 4.2 Culled lights and linking

| Case | Behavior |
|------|----------|
| Light culled (not in shade list) | Does not shade anyone this frame — **Include targets lose that light** until it re-enters |
| Light in shade list, Default | Affects all (as today) |
| Light in shade list, Include/Exclude | Same as today, bits on shade `li` |

Document for users: “Only the N most relevant lights are evaluated per frame; far lights are dropped for performance.” Optional: UI badge “8 of 24 lights active”.

### 4.3 Force-include for linking correctness (optional policy)

If a light has **non-Default** link mode and is currently contributing to a **visible** prim in the frustum, force-include it even if score is low (up to `MAX_SHADE`). Prevents “my key light Exclude set disappeared because it ranked 9th”.

Priority order when `force_includes > MAX_SHADE`:

1. Selected lights  
2. Non-default link lights affecting frustum prims  
3. Directionals  
4. Highest score  

---

## 5. Downstream pack consistency

### 5.1 RT dual packs (already 8)

RT writes visibility for GPU `li` 0..7. After shade packing, `li` is shade index → **automatic**.  
No RT shader change if light buffer order == shade order.

### 5.2 Vol bitfield

Today:

```rust
// bit i of visible order
volumetric_light_bitfield()
```

After:

```rust
// bit li of shade list
for (li, id) in shade_ids.enumerate() {
  if author[id].volumetric.enabled { bits |= 1 << li; }
}
```

Fog `vol_light_inscatter` already iterates by bit / shade index — OK.

### 5.3 Ambient from `li == 0`

Deferred uses first light’s ambient. Selector must put **primary Directional** (or brightest ambient contributor) at shade index 0 when present.

### 5.4 Path tracer

PT can either:

| Policy | Pros | Cons |
|--------|------|------|
| **A. Full visible list** (no cull) | Correctness for offline-ish PT | Cost |
| **B. Same shade list as deferred** | Consistency | Culled lights missing in PT |
| **C. Larger PT cap** (e.g. 32) with same selector | Better PT | Extra path |

**Recommendation:** Policy **A** for PathTrace mode (full visible, max 256 existing PT cap); Policy **B** for PBR/Lit. Selector is mode-aware.

### 5.5 CUDA / offline paths

Any non-WGSL consumer of `serialize_lights_gpu` must switch to `build_frame_lights` or keep a `serialize_all_visible_lights()` for offline.

---

## 6. Path to true clustered deferred (phase after CPU shade list)

CPU shade list is **not** clustered shading. It is a **global top-K**. Next step:

### 6.1 Clustered lighting (outline only)

```
1. CPU shade list may grow to MAX_CLUSTER_LIGHTS (e.g. 32–64) in light_data
2. GPU: 3D cluster grid (tile_x, tile_y, depth_slice)
3. CPU or GPU assign light indices per cluster (sphere vs frustum AABB)
4. Deferred: for pixel → cluster → loop only that cluster's light indices
5. Link masks: still shade-space bits; cluster list only contains shade indices
```

Mapping remains: **author → shade index → cluster membership**.  
Linking never uses cluster indices.

### 6.2 Why not skip straight to clustered

- Deferred already hard-caps 8; top-K CPU gives most of the win for 16–32 light scenes.  
- Cluster assignment + depth slicing is a larger shader/BGL change.  
- RT still only packs 8 channels without further packing work.

**Implement CPU shade list first; keep `MAX_SHADE` configurable (8 → 16 later with RT pack growth).**

---

## 7. Pre-existing linking footgun (must address in design)

`build_light_link_masks` treats `link_targets` entries as:

```rust
let pi = uid as usize;  // used as prim FLAT INDEX
```

But `LightData.link_targets` is documented / UI-fed as **prim_uid** in places. If uid ≠ flat index, Include/Exclude is wrong **even today**.

### 7.1 Required fix as part of P3.1

| Choice | Action |
|--------|--------|
| **Canonical: prim_uid** | Resolve `uid → flat_index` via `prim_index_by_uid` when building masks; skip missing |
| **Canonical: flat index** | Document and fix UI to store indices (fragile under reorder) |

**Recommendation:** **prim_uid** everywhere in author data; resolve at mask build time.

Also: poly/NURBS/imported bodies are not in `primitives[]` — linking currently SDF-flat only. Document as known limitation; future “body_uid” linking is out of scope for P3.1.

---

## 8. Phased implementation plan

### Phase A — Identity & single packer (no cull yet)

**Goal:** One code path builds lights + masks + vol bits; behavior bit-identical.

1. Introduce `AuthorLightRef { id, data }` and `build_frame_lights_visible_order()`  
2. Replace `serialize_lights_gpu` + `volumetric_light_bitfield` + light_snapshot collection with that packer (order = current visible graph order)  
3. Fix `link_targets` → prim_uid resolution  
4. Unit tests: mask bits match old builder for Default/Include/Exclude fixtures  
5. Gates: golden link mask vectors; light buffer layout test unchanged  

**Exit A:** No visual change; safer identity plumbing.

### Phase B — CPU top-K cull + remap

**Goal:** `MAX_SHADE` most relevant lights only.

1. Selector scoring (§3.3) + hard rules  
2. Pack shade list; rebuild masks/vol bits in shade order  
3. HUD: `Lights: 8/24 active` when culled  
4. Force-include selected + non-default link lights  
5. Tests:  
   - reorder + Include only prim 0 still correct after remap  
   - far light culled → no contribution (probe)  
   - sun always present when visible  
6. Mode: PBR/Lit use top-K; PathTrace uses full visible (Policy A)

**Exit B:** 16–32 light scenes interactive; linking correct for packed lights.

### Phase C — Importance stability — **SHIPPED**

**Goal:** Reduce flicker when ranking swaps every frame.

1. Soft hysteresis: STICKY_SCORE_MULT on previous shade members still in range  
2. Hold boost: extra score mult while hold[id] > 0 (decays when dropped; refreshed when packed)  
3. Order stabilize: survivors keep previous shade order (sun @0); reduces RT pack channel flicker  
4. Out-of-range (score < 0) always drops regardless of hold  
5. PathTrace (!cull) clears hysteresis  

**Impl:** ShadeHysteresis in cache.shade_hysteresis + light_pack::build_frame_lights(..., hyst).

**Exit C:** No popping when orbiting (unit-tested borderline swap).

### Phase D — Clustered deferred — **SHIPPED**

1. Raise packed light count to **32** (MAX_SHADE_PBR) for deferred shade list  
2. CPU cluster grid: 32px tiles × 8 log-depth slices, max 12 lights/cluster  
3. light_cluster_buffer @ binding **24**; deferred loops light_eval_count/li  
4. RT still samples first **8** shade lights (dual packs unchanged)  
5. Directional lights injected into every cluster; PathTrace keeps full list / clusters disabled  

**Impl:** pp/src/light_cluster.rs + FrameLightGpu.cluster_data + deferred_entry.wgsl.

**Exit D:** Many local lights with per-tile cost (unit tests for assignment + packer).

---

## 9. File / API touch list (implementation map)

| Area | Files | Change |
|------|--------|--------|
| Packer | `scene_actions.rs` or new `light_pack.rs` | `build_frame_lights` |
| Link masks | `ui_update.rs::build_light_link_masks` | Input = shade-ordered; uid resolve |
| Vol bits | `scene_state.rs` | Build from shade list |
| Cache | `viewport_render.rs` + light dirty | Cache `FrameLightGpu` |
| HUD | `viewport_overlays.rs` | active/total lights |
| Selector | new `light_select.rs` | scores + hard rules |
| Tests | `ui_update` / `light_pack` unit tests | remap fixtures |
| Docs | this file + `PBR_LIGHT_PERF_PLAN.md` | status |

**Shader:** Phase A–C ideally **zero** deferred/RT/fog entry changes (only data order).  
Phase D adds cluster buffer + deferred loop.

---

## 10. Test matrix (must pass before merge)

| ID | Scenario | Expect |
|----|----------|--------|
| T1 | 3 lights Default, no cull | Bit-identical light buffer + masks to pre-change |
| T2 | 12 points, camera at origin | Shade list = 8 nearest (or scored); no crash |
| T3 | Light A Include prim_uid X only; A ranks 5th | Only prim X lit by A; others not |
| T4 | Light B Exclude prim Y; B force-included | Y unlit by B; others lit |
| T5 | Sun + 10 points | Sun always shade index 0 (or first directional) |
| T6 | Select far light | Force in shade list |
| T7 | Toggle visibility | Remap stable, no stale cache |
| T8 | VOL on light ranked 9th, fog on | Either force-include or intentional drop (policy documented) |
| T9 | RT on, 8 points | Packs 0–7 match shade indices; no soft_shadow storm |
| T10 | PathTrace + 16 lights | Full list or documented PT policy |

---

## 11. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Popping when lights enter/leave top-K | Phase C hysteresis |
| Include light culled → “bug” reports | Force-include non-Default; HUD count |
| link_targets uid vs index | Phase A resolve by uid |
| Ambient wrong after reorder | Directional at shade 0 |
| Double rebuild cost every orbit | Score only on camera delta threshold |
| PT divergence from PBR | Mode-aware policy A/B |
| Cache desync lights vs masks | Single `FrameLightGpu` blob, one dirty flag |

---

## 12. Non-goals (P3.1)

- IES profiles / physical units  
- Shadow map atlas for many spotlights  
- Linking poly/NURBS bodies (still SDF prim list)  
- Raising deferred hard cap past 8 without RT pack growth  
- GPU-driven light culling (compute) in v1  

---

## 13. Decision summary

| Question | Decision |
|----------|----------|
| Can we reorder lights freely? | **Only after** shade-order mask rebuild |
| Where is truth for “who is light 3”? | **Shade list this frame**, not `LightData.id` |
| How do we map linking? | Evaluate author Include/Exclude, **write bits on shade `li`** |
| Culled lights? | Not shaded; optional force-include for linked/selected |
| Clustered now? | **No** — top-K CPU first; clustered is Phase D |
| PT? | Prefer full visible list |
| Biggest pre-fix? | `link_targets` as prim_uid vs flat index |

---

## 14. Suggested implementation order when you say GO

1. **Phase A** (identity packer + uid fix) — safety, no cull  
2. **Phase B** (top-K + remap) — real perf win  
3. **Phase C** (hysteresis) — polish  
4. **Phase D** (clustered) — only if 32+ local lights become a product need  

---

## 15. Appendix — current vs target buffer shapes

### Today

```
light_data:  [N | L0 | L1 | … | L(N-1)]     N = visible count
link_masks:  per prim: 2×u32, bit i → visible-order light i
vol_bits:    bit i → visible-order light i has VOL
deferred:    for li in 0..min(N,8)
```

### Target (Phase B)

```
light_data:  [K | S0 | S1 | … | S(K-1)]     K ≤ 8, Si = shade-selected author lights
link_masks:  per prim: 2×u32, bit li → shade light li affects prim
vol_bits:    bit li → shade light li has VOL
remap_cpu:   shade_ids[li] = author LightData.id   (debug / HUD only)
deferred:    for li in 0..K   (unchanged shader)
```

---

*End of design. No code changes required until implementation GO on Phase A.*
