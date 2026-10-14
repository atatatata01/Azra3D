# Mesh + Voxel + SDF — Sculpt Architecture & Performance Plan

Architecture review requested by the owner (2026-07-02). Grounded in a code pass against master
`d51e024` + a 4-candidate design workflow (each adversarially reviewed). Sister-doc to
`SCULPT_FEEL_LOG.md` (the feel work) and `RENDER_ARCHITECTURE.md` (the 3-viewport umbrella).

## 0. Owner's plan — three threads
1. **A great sculpting tool** (ongoing feel work: falloff/clay/symmetrize + how far to push it).
2. **A "sculpt → SDF" modifier + action** — merge the SDF-procedural and voxel-sculpt workflows.
3. **Great viewport performance with millions of polys.**

## 1. Reframe — what the code pass corrected
- **Per-edit sparse sculpting is ALREADY near-minimal.** A stamp = GPU brush scatter + full-screen
  atlas raymarch; NO readback, NO remesh, NO upload per edit (deferred to release). The Plan-B1
  raymarch is live at **≥256³** (docs saying ">512" are STALE; `SPARSE_RES_THRESHOLD=256`).
- **"Millions of polys" is NOT the sculpt-perf problem.** That is the **Assembly viewport**
  (`RENDER_ARCHITECTURE.md §3.3`): a read-only scene renderer with instancing + frustum/occlusion
  cull + LOD + shadow maps + batching. It is a *separate renderer that does not exist yet*. This is
  the biggest single new piece in the plan and the true home of Thread 3.
- **The sculpt "resolution perf" issues are LATENCY walls, not throughput:** blocking save readback
  at 1024³ (freeze), ~21 s resident-mesh rebuild, sparse Move degrades to an elastic warp.
- **Source-of-truth is the crux for Thread 1 depth.** Today the density grid is authoritative;
  `voxel_sync_node_mesh` re-derives the mesh FROM the grid on save/switch, so any detail finer than
  voxel res is destroyed, and there is no serialized mesh-as-truth field nor a vertex-undo lane.
- **Sculpt→SDF (Thread 2) is partly plumbed:** `io_bridge::region_mesh_to_sdf` already exists (the
  Move immediate-voxelize path uses it). So sculpt→mesh→SDF is a short hop.

## 2. Candidate architectures (design workflow + adversarial verdicts)

| Candidate | Score /10 | Verdict |
|---|---|---|
| **Pure-perf baseline** (voxel stays truth; async save, half-res march, sparse-Move, rebuild throughput) | **7** | Red-line clean, severable, biggest immediate latency win. **DEFER its sparse-Move phase** (write-race + brick-alloc gaps → research-grade). Removes freeze/latency walls; does NOT raise raw resolution. |
| **Dual-Room Surface** (mesh-authoritative Surface room, 3D-Coat Voxel/Surface) | **6** | The real 3D-Coat capability (detail beyond voxel res, survives round-trip). Display-Invariant-clean by construction. Gaps to close: (a) serialization safety proof, (b) GPU→CPU vertex commit + CPU brush parity, (c) honest perf framing. High complexity. |
| **Layered Detail Field** (sparse displacement overlay on voxel base) | **5.5** | **Rejected as primary:** object-local anchoring means detail **slides off on Move/Smudge** (fatal for sculpting); tessellate-then-displace re-introduces the meshing cost it claims to dodge; does not move perf. |
| voxel-auth-deferred | — | design agent failed to emit; conceptually = "generalize the shipping deferred-Move mechanism," subsumed by Pure-perf + Dual-Room Phase 0-1. |

### 2.1 Serialization rule for ANY new persisted layer (corrected — load-bearing)
- **DO:** add a trailing `#[serde(default)] Vec<XxxRecord>` on the **top-level `ProjectFile`** (after
  `poly_layers`, BEFORE the `FL4ORDER` tail), reattached to nodes by `node_id` on load. This is the
  **proven, tested `voxel_grids` / `poly_layers` precedent** (`project.rs:28-48`). Keep the node's
  field `#[serde(skip)]` (like `VoxelBody.grid`).
- **DON'T:** add a trailing field to the `NodePayload::VoxelBody` **variant** — bincode variants are
  positional; a missing trailing field misreads subsequent bytes (`scene_graph.rs:328-337`).
  `#[serde(default)]` does NOT rescue a variant's trailing field.
- **DON'T:** insert/reorder `NodePayload` variants (VoxelBody must stay index 10); DON'T add a 2nd
  magic after `FL4ORDER`.
- **GATE:** a genuine *pre-field* old-file load test (not a re-saved file) — the QA gap the reviewer
  correctly flagged.

## 3. Recommended gated plan (mapped to the threads)

**Thread 3a — kill the high-res sculpt latency walls (quick, safe, score-7).**
Pure-perf baseline, phases in risk order: (P0) fix the 256/512 doc-vs-code discrepancy; (P1) half-res
sculpt raymarch + upscale during interaction; (P2) async save readback (`map_async` + poll — 3
existing precedents) to kill the 1024³ save freeze; (P4) resident-rebuild throughput (batch chunks,
frustum-first drain). **Defer P3 (sparse forward-scatter Move)** until it's a real gated feature.

**Thread 3b — the millions-of-polys Assembly viewport (the big one, the true Thread 3).**
Build `RENDER_ARCHITECTURE.md §3.3`: read-only raster scene renderer — persistent buffers, instancing,
frustum + occlusion culling, LOD, shadow maps, draw batching. Consumes baked meshes only. This is the
largest new piece and the actual "fast with millions of polys" deliverable.

**Thread 2 — sculpt → SDF bridge (self-contained interop, high workflow-merge value).**
- *Action* (one-shot): sculpt → Surface-Nets mesh → `region_mesh_to_sdf` → new `SdfBody`.
- *Modifier* (live): an SDF-stack node referencing the sculpt, re-baking on change.
- Closes the loop with the existing SDF→sculpt rule (SDF as a boolean modifier stamping the voxel field).

**Thread 1 — sculpting-tool depth (surface sculpting beyond voxel res).**
Direction = **Dual-Room Surface** (score 6), but gated behind closing its three gaps first:
serialization via §2.1 (+ genuine old-file test), a defined GPU→CPU vertex commit + CPU parity path,
honest perf framing. Its **Phase 0-1** (mode enum + dormant fields + mode-aware `voxel_sync_node_mesh`)
is low-risk plumbing that unblocks the rest. Continue the FEEL work (Clay-2…) as small increments in
parallel — it's independent of all the above.

## 4. Open risks to resolve before committing
- **Dual-Room GPU→CPU vertex parity:** Surface displacement is GPU-vertex-shader only today; needs a
  CPU reference + readback/commit path to satisfy the 2e-3 brush-parity red line.
- **Sparse Move real grab:** forward-scatter needs GPU sparse brick allocation + atomic-add
  accumulation + a normalization pass — a research task, not a "baseline."
- **Assembly viewport scope:** occlusion culling + LOD generation are each substantial; phase them.
- **Serialization:** prove back-compat with a genuine pre-field `.fl4`, per §2.1.

## 5. Decision needed
Which thread executes first? Candidates: (a) Assembly millions-poly renderer (Thread 3b — biggest,
the stated headline); (b) Pure-perf baseline P0-P2 (Thread 3a — quick, kills freezes); (c) sculpt→SDF
bridge (Thread 2 — small, merges workflows); (d) continue sculpt feel + Dual-Room Phase 0 (Thread 1).
