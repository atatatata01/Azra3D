# Phase D — Q2 Recluster Flow (edit round-trip)

**Date:** 2026-07-31  
**Status:** specification (Phase D entry gate Q2)  
**Roadmap:** `AZRA_MASTER_ROADMAP.md` §3 Q2 / Phase D entry gates  
**Precedent:** `BodyVolumeCache::needs_rebake_on_leave`, poly Object/Edit mode split

This document is the **written recluster flow** required before any cluster raster code lands.
It is not implementation. It is the contract implementers must not invent mid-PR.

---

## 1. Product rule (why this exists)

UE Nanite geometry is immutable at runtime — edit means round-trip to a DCC. AZRA's product
space is **"model inside the same scene that renders big."** Therefore:

* Clustered draw is **Object Mode only**.
* Edit Mode always draws the **live half-edge / mesh_cache** path (existing poly raster).
* Leave Edit **schedules** recluster; it never blocks the frame thread on a full rebuild.

If recluster is missing, delayed forever, or pops without a stable fallback, Phase D fails its
exit gate even if Object Mode is fast.

---

## 2. Modes and ownership

| Mode | Who draws the body | Cluster set membership |
|---|---|---|
| Object Mode, clustered | Cluster path (indirect / virtual) | **In** set |
| Object Mode, never clustered (too small / policy) | Existing mesh_cache raster | **Out** |
| Edit Mode (poly sub-mode ≠ Object) | Existing mesh_cache raster (full detail) | **Out** (forced) |
| Recluster pending after leave | Existing mesh_cache raster until bake ready | **Out** until install |

**Owner of the recluster job:** `app` poll phase (same family as `poll_mesh_cache` /
`BodyVolumeCache` bake), **not** `dispatch_gpu_render`. Frame Ordering Contract: no GPU buffer
create in the render phase; cluster page alloc/grow only in poll.

**Owner of the cluster DAG data:** content-keyed store parallel to `mesh_cache` (proposed name
`cluster_cache`), keyed by the same identity as display mesh (`content_signature` or
`MeshCacheId` + generation). Not a second truth for topology — half-edge remains authoring truth.

---

## 3. State machine (per body / NodeId)

```
                  ┌──────────────────────────────┐
                  │                              │
                  v                              │
            [Unclustered] ──enter Object + eligible──► [ClusterPending]
                  ^                              │
                  │                              │ poll completes install
                  │                              v
                  │                         [Clustered]
                  │                              │
                  │         enter Edit           │
                  └──────[Clustered]─────────────┘
                         (drop out immediately)

            Leave Edit while dirty:
              [Unclustered] or mid-edit → [ClusterPending] (debounce)

            Leave Edit clean (no mesh change):
              if previous DAG still matches key → [Clustered] (no job)
              else → [ClusterPending]
```

### States

| State | Draw path | Notes |
|---|---|---|
| **Unclustered** | mesh_cache multi-instance / structural list | Default today |
| **ClusterPending** | mesh_cache (same as Unclustered) | Job queued or running; **no pop to empty** |
| **Clustered** | cluster path only for this body | Live mesh path **removed** from G-buffer for this body (Display Invariant template: sculpt raymarch point 2) |

---

## 4. Triggers

| Event | Action |
|---|---|
| Enter poly Edit (`poly_work_mode` leaves Object) on body B | B → **Unclustered** immediately; cancel B's pending recluster job; keep last DAG in cache for possible reuse |
| Leave poly Edit → Object on body B | If mesh key unchanged vs last installed DAG → re-enter **Clustered** without job. Else schedule **ClusterPending** |
| `poly_sync_mesh_cache` / content change on B in Object Mode | Invalidate DAG key; schedule **ClusterPending** (debounced) |
| SCENE/MESH dirty that drops B | Cancel job; free DAG pages |
| Project load / new scene | Clear all cluster state |
| Undo/redo that touches B's mesh | Same as content change |

---

## 5. Debounce policy

Mirror volume bake discipline, not frame-thread panic:

| Parameter | Policy |
|---|---|
| **Debounce** | 150–300 ms after last mesh change (constant, tunable in Developer Tweaks) |
| **Coalesce** | Multiple edits within debounce → one job at latest content key |
| **Priority** | Active/selected body first; then camera-near bodies; then LRU of rest |
| **Budget** | At most one heavy recluster **start** per poll tick (Frame Budget Rule); running job continues async |
| **Cap** | If queue > N, drop oldest non-selected pending jobs (they re-queue on next focus) |

Never: recluster every drag frame. Never: block `update()` on recluster complete.

---

## 6. Pop policy (edit-leave and install)

**Forbidden:** one frame with no geometry (empty draw) when leaving edit or when install races.

| Moment | What the user must see |
|---|---|
| Enter Edit | Immediate switch to full mesh_cache draw; cluster draws for B stop **same frame** |
| During Edit | Live mesh only; no dual technique over same pixels |
| Leave Edit, pending | mesh_cache continues uninterrupted |
| Install completes | Atomic swap: next frame B is either fully Clustered or still Unclustered — no half-DAG |
| Install fails / abort | Stay Unclustered; toast once; retry on next leave/focus |

**LOD pop while clustered (camera move):** allowed only along the cluster DAG (parent/child
clusters). Not a hard cut to a different representation. Hysteresis on switch distance
(reuse poly LOD / SDF LOD deadband constants).

---

## 7. Re-enter edit mid-recluster

| Case | Behaviour |
|---|---|
| Job for B still in queue | Cancel; B stays Unclustered (edit path) |
| Job for B running on worker | Cancel flag; worker discards result on completion (generation mismatch) |
| Job for other body C | Continues (independent) |
| User leaves edit again before cancel finishes | New debounce; only latest content key installs |

**Generation:** every schedule bumps `cluster_job_gen[B]`. Install accepts only if
`result.gen == cluster_job_gen[B]` and content key matches live mesh.

---

## 8. Who builds what (bake shape)

```
mesh_cache triangles (display mesh for B)
        │
        │  background job (poll-started, worker or async task)
        v
  cluster builder (~128-tri clusters, QEM parents, DAG)
        │
        │  content key = (mesh content id, builder version)
        v
  cluster_cache install (poll phase only: buffer grow, page upload)
        │
        v
  draw: GPU visibility + multi_draw_indexed_indirect (Phase D later)
```

* **Reuse** `display_mesh_ops` QEM with **hard** `locked_verts` / `locked_edges` for shared
  cluster boundaries (roadmap §2b — soft cost multipliers are forbidden for DAG edges).
* **Determinism:** same mesh → same DAG bytes (test gate).
* **Never** on the frame thread beyond a cheap "is B clustered?" test.

---

## 9. Interaction with structural draw list (Phase C)

Structural list (`structural_draw_calls`) remains the **mesh_cache** path.

When B is **Clustered**:
* B's mesh_cache entries are **not** gathered into the structural list (or are flagged
  `DRAW_KIND_CLUSTERED` and skipped by mesh multi-instance).
* Cluster pass draws B instead (new pass or extended pool), after or instead of mesh path
  for that body only.

When B is **Unclustered** / **ClusterPending**:
* Existing C-path structural list draws B unchanged.

This is the same coexistence rule as SDF resident vs mesh_cache vs sculpt raymarch.

---

## 10. Test gates (must exist before claiming D exit)

| # | Test |
|---|---|
| T1 | Enter Edit on clustered body → same frame no cluster draw; mesh visible |
| T2 | Leave Edit clean → no job if key match; still clustered |
| T3 | Leave Edit dirty → mesh continuous; after install, clustered; no empty frame |
| T4 | Re-enter Edit mid-job → cancel; no install of stale gen |
| T5 | Undo across leave/edit → key mismatch; recluster or unclustered, never corrupt DAG |
| T6 | Determinism: two builds same mesh → identical cluster ids / parent links |
| T7 | Boundary lock: adjacent clusters share exactly the same boundary verts after simplify |

---

## 11. Explicit non-goals (this doc)

* GPU software raster / visibility buffer (later D step; needs Q1 amendment signed).
* Streaming pages / out-of-core (D streaming item).
* Changing half-edge authoring or undo model.
* Clustering during active gizmo drag.

---

## 12. Sign-off

| Role | Name | Date | Notes |
|---|---|---|---|
| Spec author | Grok (implementation) | 2026-07-31 | Entry gate Q2 text |
| Owner (product) | ________ | ________ | Required before raster cluster code |
| CTO / architecture | ________ | ________ | Optional review |

Until owner row is filled, implementers may land **pure offline cluster builder tests** in
`core_math` only if isolated from draw. **No** `dispatch_*` / pool / WGSL cluster path.
