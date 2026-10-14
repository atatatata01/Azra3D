# REMESH_ADAPTIVE_OCTREE_PLAN.md

## Voxel Remesh — Regional Update + Adaptive Octree Dual Contouring

**Status:** **PROPOSED — code-anchored, NOT yet measured.** Every `file:line` in §3 and §4 was read
from live source on 2026-08-09. Every *performance* claim is explicitly marked UNMEASURED and is
Gate 0's job to produce. Do not treat any number in §5–§8 as a baseline until Gate 0 has run.
**Date:** 2026-08-09
**Branch:** `master` (repo ROOT, `5705bcc`). **Where to implement:** the root checkout had a
concurrent writer during this session, and ~50 files are modified there. Prefer a fresh worktree
branched off current `master`. Note `work/poly-perf` already exists
(`.claude\worktrees\poly-perf`, `cb69eed`) but is **behind master and undocumented in
`docs/WORKTREES.md`** — rebase it or branch anew; do not assume it is current.
**Owner ask (verbatim):**
> "voxel remesh could we add a setting that will bring more resolution to a particular spot of the
> polyshape" … "when i move one poly everything update can we a region update" … "create a plan for
> the next performance boost including octree dual contouring this will make the difference i am
> looking for"

**Goal:** make a drag under an active Remesh modifier cost work proportional to *what moved*, and
make cell size vary across the mesh so detail can be spent where the user asks for it.

**Non-goals:** sculpt-mode remesh (`Ctrl+R` sculpt path), the SDF body bake, the display bake, the
path tracer, Nanite/cluster LOD. This plan touches the **poly Remesh modifier** only.

---

## 0. How to use this document

1. Read §1–§4. §4 is a reuse inventory — most of what this plan needs **already exists** in the SDF
   path and is simply not wired to the poly remesh. Obey CLAUDE.md's *No Duplication* rule: do not
   write a new DC extractor, a new QEF solver, or a new octree classifier. Extend these.
2. **Run Gate 0 before writing any optimization.** Two benches already exist and appear never to have
   been run. A plan built on guessed hot spots is worthless — the last poly perf investigation found
   the cost in the display bake, not the modifier (`project_poly_live_feel_bake_dominates`).
3. Implement **one gate per session/PR**, in order. Each gate has its own acceptance criteria and
   must be green before the next starts. Gates 1 and 2 must each ship **output-neutral** (parity
   gated) before Gate 3 turns on any visible behaviour change.
4. Obey §9 red lines.
5. If a `file:line` no longer matches, the code moved — re-locate it and record the drift in §11.
   Do not implement against a guess.

**Before writing code, re-read `CLAUDE.md`.** Hardest-biting rules here: No Hacks, No Bandaids,
Search-Before-Writing, Frame Budget Rule, and the Display Invariant.

---

## 1. Problem statement

### 1a. "When I move one poly, everything updates"

Confirmed. The **field resample is already regional** — `voxel_remesh_mesh_incremental` diffs vertex
positions, computes a dirty AABB in cell space, and resamples only that box
([`remesh.rs:356-396`](../../../core_engine/app/src/poly/remesh.rs#L356-L396)). That is V1, and it is
golden-gated bit-identical (`incremental_interior_nudge_is_bit_identical`).

Everything downstream of the resample is still whole-mesh, every frame:

| # | Stage | Scope today | Anchor |
|---|-------|-------------|--------|
| S1 | `seal_enclosed_air()` | whole field | [`remesh.rs:403`](../../../core_engine/app/src/poly/remesh.rs#L403) |
| S2 | `surface_nets_placed(Qef)` | every surface chunk of the body | [`remesh.rs:93`](../../../core_engine/app/src/poly/remesh.rs#L93) |
| S3 | `project_points_to_mesh` | every output vertex × every source triangle | [`remesh.rs:116`](../../../core_engine/app/src/poly/remesh.rs#L116) |
| S4 | `HalfEdgeMesh::from_mesh` | full half-edge rebuild from scratch | [`remesh.rs:128`](../../../core_engine/app/src/poly/remesh.rs#L128) |

S2 already got a surface-chunk skip so it is O(surface), not O(res³)
([`grid.rs:1744`](../../../core_engine/core_math/src/voxel_sculpt/grid.rs#L1744)) — but a thin shell
over the *whole body* is still far more than the handful of chunks one dragged vertex touches.

### 1b. "More resolution at a particular spot"

Cannot come from the current lattice. The remesh fits **one uniform cubic grid** to the whole mesh
AABB ([`remesh.rs:185`](../../../core_engine/app/src/poly/remesh.rs#L185)); cell size is a constant of
that lattice. And global resolution cannot simply be raised — the grid is a dense `Vec<f32>`, so 512³
is a ~537 MB allocation per evaluation, which is exactly why `REMESH_RES_MAX = 256`
([`remesh.rs:26`](../../../core_engine/app/src/poly/remesh.rs#L26)).

**Adaptive octree DC solves both problems with one mechanism**, which is why it is the spine of this
plan rather than an optional extra:

* **Perf:** collapsing low-error nodes removes cells on flats — fewer QEF solves, fewer output quads,
  smaller half-edge rebuild. The win is on *every* remesh, not just masked ones.
* **Feature:** node subdivision depth can vary per region. A mask that says "go two levels deeper
  here" gives a genuinely finer cell there — real detail, not redistributed polygons.

---

## 2. Why not the cheaper option

An earlier proposal in this conversation was: remesh uniformly, then coarsen away from the mask with
the existing QEM kernel (`mesh_decimate_qem` + `build_target_edge_length`,
[`op_impls.rs:1397`](../../../core_engine/core_math/src/display_mesh_ops/op_impls.rs#L1397)).

**Superseded, and recorded here so nobody re-proposes it as the primary path.** Decimation cannot add
detail — it redistributes a polygon budget under a ceiling the lattice already fixed. A feature finer
than one 256³ cell is lost during voxelization and no amount of downstream collapsing brings it back.
It also triangulates, discarding the quad topology the remesh exists to produce.

It stays viable as an *independent* modifier ("adaptive decimate with a density mask") and Gate 3
should reuse its mask→ℓ(x) mapping shape. It is not the answer to the owner's ask.

Note for whoever writes that later: `build_target_edge_length`'s base signal is
`mesh_vertex_importance` — pure dihedral/topology, no field. The SDF curvature is an *optional*
`protect_src` blended at `p.max(src[i] * 0.25)`
([`op_impls.rs:587`](../../../core_engine/core_math/src/display_mesh_ops/op_impls.rs#L587)). That
0.25 is calibrated as an assist, so a mask-driven use needs its own scale — it is not literally a
drop-in.

---

## 3. Architecture constraints that bind

* **Display Invariant.** The intermediate voxel field is never rendered; the remesh output enters the
  raster path as ordinary triangles. Nothing in this plan adds a render path. Unaffected.
* **Save format.** `RemeshIncrCache` is runtime-only, and the remesh *output* is a derived display
  mesh. Adding an octree, a chunk cache, or new cache fields carries **zero** `.fl4` risk. The only
  serialized surface is new `PolyModifier` fields (Gate 3), which must ride the `FL4PMODF`/`FL4PSMTH`
  tail like `remesh_project` and `smooth_falloff` — **never** appended to the bincode blob
  ([`node.rs:568-599`](../../../core_engine/core_math/src/scene_graph/node.rs#L568-L599)).
* **Stack hash.** Any new remesh field must be written into `poly_stack_prefix_hash`
  ([`mesh_sync.rs:1729-1735`](../../../core_engine/app/src/poly/mesh_sync.rs#L1729-L1735)) or the
  prefix/display cache serves a stale mesh and the slider appears frozen.
* **Incremental cache eligibility.** Any new field must join the `break 'incr` guard next to
  `padding_bits` ([`remesh.rs:280-290`](../../../core_engine/app/src/poly/remesh.rs#L280-L290)), or a
  parameter change silently splices into a grid built under the old parameter.
* **Enclosure is global.** `seal_enclosed_air` fills sealed voids and the fill is **one-way**, which
  is why `had_enclosed_void` is a permanent lockout on the incremental path
  ([`remesh.rs:236-241`](../../../core_engine/app/src/poly/remesh.rs#L236-L241)). No gate here may
  weaken that lockout. See `project_seal_enclosed_air_false_seals` for how this bit last time.

---

## 4. Reuse inventory — what already exists

This is the core finding of the investigation. The SDF bake path already contains most of the
machinery, built for a different caller.

| Need | Already exists | Anchor |
|------|----------------|--------|
| Per-chunk DC extract | `ChunkedMesh::extract_chunk_from` | [`chunked_dc/extract.rs:214`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L214) |
| **Dirty-region re-extract** | `ChunkedMesh::update_dirty_chunks_from(accessor, dirty_min, dirty_max)` — takes cell coords, exactly what `remesh.rs` already computes as `region` | [`extract.rs:516`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L516) |
| Dirty AABB → chunk list | `dirty_chunk_coords` | [`extract.rs:172`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L172) |
| Crack-free chunk seams | ghost padding on −x/−y/−z, deliberate cross-chunk vertex duplication | [`extract.rs:241-244`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L241-L244) |
| Chunks → one mesh | `stitch.rs::merge_to_mesh` → `(positions, normals, quads)` | [`chunked_dc/stitch.rs:12`](../../../core_engine/core_math/src/chunked_dc/stitch.rs#L12) |
| Storage abstraction | `VolumeAccessor` trait, dense + sparse impls | [`chunked_dc/grid.rs:13`](../../../core_engine/core_math/src/chunked_dc/grid.rs#L13) |
| QEF solve | `solve_qef` — 3×3 normal equations, Tikhonov toward cell centre, Cramer, degenerate fallback | [`mesh.rs:512`](../../../core_engine/core_math/src/mesh.rs#L512) |
| Per-cell DC placement + the three load-bearing details (≥3 crossings, clamp to cell, average crossing normals — never a re-sampled gradient) | `cell_surface_vertex_qef_d` | [`grid.rs:2163`](../../../core_engine/core_math/src/voxel_sculpt/grid.rs#L2163) |
| **Octree node prune predicates** | `BrickClassifier::provably_exterior_region` / `provably_interior_region` | [`chunked_dc/grid.rs:628`](../../../core_engine/core_math/src/chunked_dc/grid.rs#L628), [`:698`](../../../core_engine/core_math/src/chunked_dc/grid.rs#L698) |
| Surface-only chunk mask | `surface_chunk_mask` | [`grid.rs:2575`](../../../core_engine/core_math/src/voxel_sculpt/grid.rs#L2575) |
| Regional mesher (sculpt) | `mesh_region_with_cells` | [`grid.rs:2332`](../../../core_engine/core_math/src/voxel_sculpt/grid.rs#L2332) |

**Two integration hazards found while reading, both real:**

1. **Sign convention mismatch.** `chunked_dc` hard-codes `> 0.0` as its inside test
   ([`extract.rs:313`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L313),
   [`:418`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L418) and siblings), while
   `VoxelGrid` uses `density >= iso` with `iso` typically 0.5. Wiring them naively **inverts the
   surface**. Fix at the boundary, not by editing either convention: a `VolumeAccessor` adapter over
   `DensityStorage` returning `density - iso`. Zero copies, one place to be wrong.
2. **`ChunkedMesh` requires `grid_resolution % chunk_size == 0`-ish** (`new` rounds up,
   [`extract.rs:156`](../../../core_engine/core_math/src/chunked_dc/extract.rs#L156)) while remesh
   resolutions are arbitrary in `8..=256`. Verify boundary chunks at non-divisible resolutions before
   trusting the seam behaviour.

---

## 5. Gate 0 — Measure (no optimization) **← START HERE**

Nothing in §6–§8 may be implemented before this gate produces numbers.

**0a. Tree state — DONE 2026-08-09.** `cargo check -p app` = **exit 0, zero errors** on
`master` + working tree. No work is needed here; recorded because getting to that answer was a trap.

> **Phantom-error trap — read before believing any red build on this tree.** The first check
> reported **50 errors** ("no field `smooth_falloff` on type `PolyModifier`", "no method
> `surface_nets_placed` for `VoxelGrid`", "could not find `CellPlacement` in `voxel_sculpt`").
> **All false** — every item was present on disk at the cited line, correctly exported
> (`pub use grid::*`), single definition. Cargo was replaying **stale cached diagnostics**, because
> concurrent edits landed with mtimes that did not invalidate the fingerprints. Two identical error
> dumps are not evidence: cargo replays the cache deterministically. Fix:
> `git status --porcelain -- '*.rs' | awk '{print $2}' | xargs touch`, then re-check.
> Second trap: `cargo check ... 2>&1 | tail -40` returns **tail's** exit code, so a genuinely failing
> build reads as `exit 0`. Capture to a file and read `$?` before the pipe.
>
> Note the committed trunk is a different question — pristine `721ba0d` does **not** build
> (`project_master_trunk_red_2026_08_09`); only master + working tree does. Bench from the working
> tree, not a fresh clone.

**0b. Run the two benches already in the file** — both `#[ignore]`, both apparently never run:

```
cargo test -p app --release measure_drag_topology_churn   -- --ignored --nocapture
cargo test -p app --release bench_remesh_incremental_drag -- --ignored --nocapture
```

`measure_drag_topology_churn` ([`remesh.rs:651`](../../../core_engine/app/src/poly/remesh.rs#L651))
was written to decide exactly this plan: its doc comment states the V2 payoff scales with how few 16³
chunks contain a flipped cell. Its output is the go/no-go for Gate 1.

**0c. Add a stage-level breakdown.** Instrument S1–S4 (§1a) separately, behind the existing perf-probe
pattern, at res 128 and 256, for: cold eval, mid-drag incremental frame, episode-commit reconcile.

**Acceptance:** a table of ms per stage per scenario, committed into §12 of this document. If S2+S4
are not dominant, **stop and re-plan** — Gates 1–2 are predicated on them being the cost, and the
last poly perf investigation found the cost somewhere nobody expected.

---

## 6. Gate 1 — Regional extract (V2)

**Deliverable:** a drag frame re-extracts only the chunks whose cells actually changed.

1. Implement `VolumeAccessor for DensityStorage` returning `density - iso` (§4 hazard 1).
2. Add a retained `ChunkedMesh` to `RemeshIncrCache`
   ([`remesh.rs:218`](../../../core_engine/app/src/poly/remesh.rs#L218)). Runtime-only; no save impact.
3. Full path: `extract_all_from`. Incremental path: `update_dirty_chunks_from(accessor, region_min,
   region_max)` reusing the `region` already computed at
   [`remesh.rs:364`](../../../core_engine/app/src/poly/remesh.rs#L364).
4. `merge_to_mesh` → the existing `extract_half_edge` tail.

**Known sub-problems — each needs an explicit answer, not a silent skip:**

* **S1 `seal_enclosed_air` is whole-field per frame.** Enclosure is genuinely global (§3), so this
  cannot simply be regionalised. Options: (a) skip the seal when the dirty region provably touches no
  air/solid boundary transition; (b) keep it global but make it cheap. **Fail closed** — if in doubt,
  seal and set `had_enclosed_void`. Do not weaken the lockout to buy speed.
* **S3 reprojection is O(output verts × source tris).** Restrict to vertices in re-extracted chunks,
  and put a BVH over the source triangles. `project_points_to_mesh` is in `io_bridge::mesh_to_sdf` —
  check whether it already has acceleration before adding one.
* **S4 `from_mesh` full rebuild.** Hardest. May not be worth solving in this gate — if Gate 0 shows S4
  is small relative to S2, **leave it and say so**. If it dominates, it needs per-chunk half-edge
  splicing with stable vertex identity, which is a gate of its own.

**Acceptance:**
* Parity: a full (non-incremental) eval through the chunked extractor is **bit-identical** to today's
  `surface_nets_placed(Qef)` output — vertex count, face count, and every position. New test beside
  `incremental_interior_nudge_is_bit_identical`.
* All existing remesh tests green, including the sealed-void lockout gate.
* Measured drag-frame improvement vs the Gate 0 baseline, on the same fixture, stated as a number.
* `cargo test -p core_math` green.

---

## 7. Gate 2 — Adaptive octree DC (uniform depth, output-neutral)

**Deliverable:** the extractor becomes an octree, but configured to *uniform* max depth it produces
the same surface as Gate 1. This gate ships a perf win and zero visible change.

1. **Extract an accumulating QEF type.** `solve_qef` takes crossings and solves once
   ([`mesh.rs:512`](../../../core_engine/core_math/src/mesh.rs#L512)); octree node collapse needs to
   *sum* quadrics up the tree. Refactor to a `Qef { ata: [f32;6], atb: [f32;3], mass: Vec3, n: u32 }`
   with `add`, `solve`, and **`residual`** (the error term that drives collapse). Keep `solve_qef` as
   a thin wrapper so `cell_surface_vertex_qef_d` is untouched and provably unchanged.
   Precedent for the packed-symmetric layout: the QEM decimator's 10-coeff 4×4 quadric
   ([`op_impls.rs:1265`](../../../core_engine/core_math/src/display_mesh_ops/op_impls.rs#L1265)).
2. **Build the octree over the band, per chunk.** Leaves = cells with a sign change. Prune interior/
   exterior nodes with the existing `provably_interior_region` / `provably_exterior_region`
   predicates rather than new ones.
3. **Bottom-up collapse:** a node collapses when its summed QEF residual is under a threshold *and*
   collapsing does not change the sign topology of its corners (the standard Ju et al. guard against
   topology damage).
4. **Dual traversal** (`cellProc`/`faceProc`/`edgeProc`) emits quads across differing levels. This is
   the part that must be written; everything it calls already exists.
5. Preserve the three load-bearing DC details documented at
   [`grid.rs:2154-2162`](../../../core_engine/core_math/src/voxel_sculpt/grid.rs#L2154-L2162) — ≥3
   crossings before solving, clamp to cell, average crossing normals. They were learned the hard way
   ("the jagged crater rim") and an octree does not exempt them.

**Acceptance:**
* Collapse threshold 0 ⇒ output **bit-identical** to Gate 1. This is the gate's central test.
* Threshold > 0 ⇒ fewer quads, and max surface deviation from the Gate 1 mesh bounded by the
  threshold (measure it; assert it).
* No non-manifold output: `validate()` green on every fixture, including the `#[ignore]` max-res one.
* Measured extract-time win at 128 and 256 vs Gate 1.

---

## 8. Gate 3 — Local resolution (the owner's setting)

**Deliverable:** mark a spot, get finer cells there.

1. **Marking already exists** — do not build a new one. `PolyModifier.geo_channel` with
   `GeoChannelKind::Polygroup` (user-marked faces) or `EdgeWeight` (painted 0..1), and
   `vertex_weights_auth(mesh, feather_rings, auth)` already produces a per-vertex 0..1 mask with a
   feathered ramp ([`geometry_channel.rs:476`](../../../core_engine/core_math/src/geometry_channel.rs#L476)).
   Masked Smooth already consumes exactly this.
2. **Rasterise the mask into a per-node depth bias.** Mask 1.0 → `max_depth + detail_levels`;
   mask 0 → `max_depth`. Feather rings give the transition for free, and the octree's differing-level
   dual traversal (Gate 2 step 4) is what makes the transition crack-free — this is precisely why the
   octree has to land first.
3. **New `PolyModifier` fields** — `remesh_detail_levels: u32` (0 = off) and reuse `geo_channel`.
   Persist via the `FL4PMODF`/`FL4PSMTH` tail (§3), add to `poly_stack_prefix_hash`, add to the
   `break 'incr` guard.
4. **UI:** the Remesh card in `properties_poly.rs` already renders `geo_channel` for other modifiers —
   reuse that widget. Name the control for what it does: **"Extra detail in channel (+N levels)"**.
   Not "resolution", which would over-promise a uniform-grid change.

**Acceptance:**
* `detail_levels = 0` ⇒ bit-identical to Gate 2. Old files load unchanged.
* `detail_levels = 2` on a polygroup ⇒ measurably finer cells inside the group, unchanged outside,
  **no cracks at the boundary** (manifold `validate()` + a visual check).
* Save → reload → identical mesh.
* Undo covers the new sliders (CLAUDE.md: every user-visible slider has undo).

---

## 9. Red lines

1. **Never weaken the `had_enclosed_void` lockout** to make a gate faster. Void filling is one-way;
   a re-opened void cannot be reproduced by a local splice.
2. **No new `PolyModifier` field in the bincode blob.** Tail section or nothing. bincode is
   positional — an appended field corrupts every existing `.fl4` carrying a poly modifier.
3. **No new DC extractor, QEF solver, or octree classifier.** §4 lists what exists. Extend it.
4. **No parity claim without a bit-identical test.** Gates 1 and 2 each hinge on one.
5. **Do not touch the sculpt display path.** `CellPlacement::Centroid` is bit-identical to historical
   Surface Nets and the sculpt path depends on that. Octree work is `Qef`-side only.
6. **No unbounded per-frame work.** Chunk re-extraction per frame needs a cap (Frame Budget Rule);
   pick one and name it `MAX_*_PER_FRAME`.
7. **Do not raise `REMESH_RES_MAX` as a shortcut.** 512³ dense is ~537 MB per eval. The octree is the
   answer to the detail ceiling; a bigger uniform grid is not.

---

## 10. Sequencing summary

*(Gate 1 amended 2026-08-09 by Gate 0's measurements — see §12. S1 was an optional sub-problem; it
is now the largest stage of a drag frame and is required work.)*

| Gate | Ships | Visible change | Depends on |
|------|-------|----------------|-----------|
*(2026-08-09: cumulative drag frame @256 is now **81.3 → 29.3 ms**, ~34 fps. Remaining ranked by
measured value: **S4 `from_mesh` 10.8 ms** > **Gate 1b regional Phase A ≈5.5 ms** > S1 3.7 · S3 1.4.
Gate 1b is no longer the biggest lever — see §12d.)*

| 0 | Measurements + green tree | none | — | 
| **1a ✅ DONE** | **Cheaper seal (S1)** — bitset mask + word-skipping flood, bit-identical. *(Shipped as §6 option (b); option (a), the conditional skip, was analysed and rejected — see §12a.)* | none (drag **81.3 → 61.7 ms** @256) | 0 |
| **1b** | Regional extract (S2+S4) | none (drag 61.7 → ~26 ms @256) | 1a |
| 2 | Octree DC, uniform depth | none (fewer polys on flats, faster) | 1b |
| 3 | Local resolution mask | **the owner's feature** | 2 |

1a is listed first deliberately: it is the larger win (50% of the frame vs 43%), it is independent
of the extractor question in §4a, and it does not depend on resolving H5/H6. 1b is where the
`chunked_dc`-vs-`mesh_region_with_cells` decision has to be made.

Gate 3 is small *because* Gates 1–2 did the structural work. Attempting Gate 3 first produces either
a seam-ridden two-lattice hack or a decimation lie (§2).

---

## 11. Anchor drift log

*(Record any `file:line` that no longer matches, with the new location and date.)*

- 2026-08-09 — all anchors verified against live source at time of writing.
- 2026-08-09 (Gate 0, worktree `remesh-octree`) — anchors re-verified against the snapshot the
  worktree was cut from. No line drift found in the anchors exercised so far (`remesh.rs` 26/93/116/
  128/185/218/236-241/280-290/356-396/364/403/651, `chunked_dc/extract.rs` 156/172/214/241-244/313/
  418/516, `chunked_dc/grid.rs` 13/628/698, `stitch.rs` 12, `voxel_sculpt/grid.rs` 2154-2162/2163/
  2332, `mesh.rs` 512). **Four §4 claims were wrong on substance rather than location — see §4a.**
- 2026-08-09 (second pass) — seven anchors that point at the wrong *thing* rather than the wrong line:
  * **§1a S1 attribution is wrong.** `remesh.rs:403` is the right line, but `seal_enclosed_air` is
    **incremental-path-only**, and only inside `if any_changed {` (`:356`). `voxel_remesh_full`
    (`:147-210`) never calls it — the full path's enclosure fill happens inside the sampler
    (`:193` → `io_bridge/src/mesh_to_sdf.rs:648-649`) and is returned as the `enclosed` count. Only
    S2/S3/S4 are "every frame, both paths". *(This does not weaken §12 — S1 being incremental-only
    means it lands squarely on the path Gate 1 optimises.)*
  * **§3 enclosure lockout** — `remesh.rs:236-241` → the field is at **`:239`**; the clause red
    line #1 actually binds on is `|| c.had_enclosed_void` at **`:289`**, set at `:404` and `:436`.
    Pinned by `incremental_refuses_fields_with_sealed_voids` (`:531`).
  * **§1a incremental description** — `remesh.rs:356-396` → the vertex diff is at **`:298-303`**,
    and the dirty AABB accumulates in **LOCAL/model space** (not cell space) at **`:309-331`**;
    conversion to cell indices happens only at `:360-364`. Accurate range: **`:298-406`**.
  * **§3 save format** — `node.rs:568-599` holds only `#[serde(skip)]` field *declarations*. The
    tail mechanism is six other sites: `app/src/project/io.rs:93`+`:105` (magics), `io.rs:201-216`
    (save arms), `io.rs:300-320` (load arms), `app/src/project/mod.rs:189-219` (records),
    `app/src/project_save.rs:300-356`, `app/src/project_load.rs:446-474`.
  * **§3/§8 "ride the `FL4PMODF` tail like `remesh_project`" — MISSING; no such carrier exists.**
    See §4b G3-A; filed as a live bug (`REMESHPARAMSAVE`).
  * **§3 stack hash** — `mesh_sync.rs:1729-1735` is the right block, but it lives inside
    `fn hash_modifier_stack_into` (**`:1650`**); `fn poly_stack_prefix_hash` is at **`:316`** and
    calls it at `:324`. One `h.write_u32(...)` after `:1735` therefore covers both hashes.
  * **§2 QEM note** — `build_target_edge_length` is at **`op_impls.rs:552`**, not `:1397`
    (`mesh_decimate_qem` at `:1397` is correct).

---

## 4a. Integration hazards — CORRECTED 2026-08-09 (Gate 0, read from live source)

§4's reuse inventory is sound about *what exists*. Four of its integration claims are wrong, and
three of them fail silently — they would produce a plausible-looking mesh, not a compile error.

### H1 — the adapter formula in §4 is INVERTED. Use `iso - density`, not `density - iso`.

§4 hazard 1 prescribes "a `VolumeAccessor` adapter over `DensityStorage` returning `density - iso`".
That is backwards, and the failure is silent:

* `chunked_dc` is **true SDF convention — negative inside**. Proof: `BRICK_TAG_EXTERIOR` samples
  `+BRICK_FAR`, `BRICK_TAG_INTERIOR` samples `-BRICK_FAR` (`chunked_dc/grid.rs:43-56`), and the one
  production caller feeds a real SDF from `bake_body_grid_field` straight into `extract_all`
  (`app/src/dc_baseline.rs:128-133`).
* `VoxelGrid` is the opposite — `density >= iso` means SOLID (`cell_surface_vertex_qef_d`,
  `voxel_sculpt/grid.rs:2172`), and the remesh maps `density = 0.5 − sdf/W` (`remesh.rs:7`). So
  `density − iso` is **positive inside**.
* Why it slips through a position-only parity test: the crossing test
  `(corners[a] > 0.0) != (corners[b] > 0.0)` (`extract.rs:313`) is **symmetric**, so a global sign
  flip changes neither the crossing set nor the vertex positions. What it *does* change is quad
  winding — `extract.rs:431-432`: `if d0 > 0.0 { push([a,b,c,d]) } else { push([d,c,b,a]) }` — and
  the raw finite-difference normal (`extract.rs:270-273`). Net result: a geometrically correct mesh
  with **every face wound backwards and every normal inverted**.

**Correct field sign: `iso − density`.** It is principled rather than a sign patch: the only writer
this path uses is `voxel_sculpt/ops.rs:1336` (dense) / `:1359` (sparse),
`new_density = (0.5 − d·inv_w).clamp(0,1)`, so `iso − density ≡ +d/W` exactly — the adapter
*recovers the true SDF* up to the positive scale `1/W`, while the plan's `density − iso ≡ −d/W` is
the **negated** SDF. Interpolation is clean either way (`t = f₀/(f₀−f₁)` is scale-invariant and
reduces exactly to `grid.rs:2182`'s `tt = (iso−da)/(db−da)`; an exhaustive probe over 8.39M
straddling pairs found `iso − density` bit-exact with 0 mismatches). `solve_qef` needs no
re-tuning — it consumes only `(position, unit normal)`.

**CORRECTION to the paragraph above (2026-08-09, second pass — the first pass overstated it).**
"Every face wound backwards *and* every normal inverted" is not right, and the truth is more
awkward than either: **neither adapter is sufficient on its own.**

* **Normals do not survive at all.** `HalfEdgeMesh::from_mesh` → `from_quads` passes only
  `vertices` + `quads` and **discards `mesh.normals`** (`half_edge/builders.rs:519-524`). And
  `solve_qef` builds `AᵀA` from `n⊗n` and `Aᵀb` from `n·(n·p)` — both quadratic in `n` — so a
  *global* normal flip leaves the QEF, and therefore every vertex position, bit-identical. The
  normal argument is moot for the remesh path.
* **Winding is the only thing that survives — and `iso − density` flips it.**
  `chunked_dc/extract.rs:425-428` and `voxel_sculpt/grid.rs:1859-1862` emit the character-for-
  character identical 4-cell tuple and differ **only in the selector**: `d0 > 0.0` versus
  `s0 = density >= iso`. Under a true SDF those are *opposite* (`d0 > 0` means AIR-first, `s0`
  means SOLID-first).

| adapter | field vs chunked_dc's convention | winding vs today's remesh | shading normals |
|---|---|---|---|
| `density − iso` (§4 original) | **inverted** (a negated SDF) | correct | inward (but discarded) |
| `iso − density` (correct sign) | correct | **flipped → inside-out** | correct |

**So if Gate 1 keeps `chunked_dc`, the boundary owes `iso − density` PLUS an explicit, named
winding reversal** at the `merge_to_mesh` → `extract_half_edge` seam (or a `reverse_winding: bool`
on `ChunkedMesh`) — one sign convention alive in `core_math`, one documented asymmetry in one
place. Do **not** "fix" it by editing `chunked_dc`'s `> 0.0` in place: `core_math/src/mesh.rs` holds
a bit-identical twin with 16 sign sites (`:191,193,268,276,290,298,312,320,398,399,465,469,479,483,
493,497`) pinned by `chunked_dc/tests.rs:143` on `quads.len()` **only**, so an in-place edit would
silently fork them.

This is the strongest remaining argument for **H6**: routing Gate 1 through the `VoxelGrid` regional
mesher never crosses the density/SDF boundary, so the whole sign-and-winding question disappears.

### H2 — corner-vs-cell off-by-one: construct with `res - 1`.

`VolumeAccessor::nverts() = grid_resolution + 1` (`chunked_dc/grid.rs:14-17,28`) — chunked_dc's
`grid_resolution` counts **cells** over a **corner-sample** field. `VoxelGrid`'s `res³` densities are
corner samples forming `(res-1)³` cells (`voxel_sculpt/grid.rs:2779` `cells: u32, // res - 1`;
`:1243` `voxel_size() = bounds*2/(res-1)`). So the call is `ChunkedMesh::new(res - 1, …)`, giving
`nverts = res` and `cell_size = 2·half/(res−1)` = exactly `VoxelGrid::voxel_size()`. Precedent
already in tree: `dc_baseline.rs:132`.

### H3 — §4 hazard 2 (chunk divisibility) is a NON-ISSUE, but it is the *common* case.

`ChunkedMesh::new`'s doc comment claims "The grid must be evenly divisible by chunk_size"
(`extract.rs:147`) — **the doc overstates the code**. `chunks_per_axis` ceiling-divides (`:156`),
`extract_chunk_from` clamps `x1 = (x0+cs).min(res)` (`:237-239`), and `dirty_chunk_coords` clamps to
`cpa-1` (`:183-185`). Partial trailing chunks are handled. Because of H2 the remesh always passes
`res−1` ∈ 7..=255, essentially never a multiple of 16/32 — so the partial-chunk path is the normal
path, and it is already exercised in production by `dc_baseline` (res 64 → 63 cells, chunk 32).
Action: no code change; fix the misleading doc comment.

### H4 — **seam vertices are duplicated and nothing welds them** (§4 does not mention this at all)

* `extract.rs:241-244` deliberately puts ghost cells in each chunk's own vertex list — "slight
  duplication across chunks is acceptable for crack-free boundary stitching".
* `stitch.rs:12-44` `merge_to_mesh` **concatenates with no dedup** (it only sorts for determinism).
* `HalfEdgeMesh::from_mesh` → `from_quads` → `from_quads_with_materials`
  (`half_edge/builders.rs:519, 392, 411-413`) allocates **one vertex per input position, no weld**.

So `merge_to_mesh` → `from_mesh` yields a half-edge mesh **split along every chunk seam** — open
boundary edges everywhere instead of a closed manifold. Every existing chunked_dc consumer is immune
because none needs manifold topology: `app_poll_gpu` rasterises it and `dc_baseline` hashes it. The
remesh would be the **first caller to need a manifold half-edge out of chunked_dc**. The duplicates
are bit-identical (same 8 corner samples, same global `cell_size`/`cell_center`), so an exact-position
weld is sound and needs no epsilon — but it must be written, and it is not free.

### H5 — Gate 1's "bit-identical" acceptance is UNACHIEVABLE through `chunked_dc`

§6 demands a full eval through the chunked extractor be "**bit-identical** … vertex count, face
count, and every position". The two are different implementations, not one implementation behind two
iteration orders:

| | `VoxelGrid::cell_surface_vertex_qef_d` (`grid.rs:2163`) | `chunked_dc::extract_chunk_from` (`extract.rs:214`) |
|---|---|---|
| crossing normal | `gradient_normal(world, vs)` — resampled in **world space**, 1-voxel stencil | finite difference on the **sample lattice** (`grid_normal` / `sdf_gradient_normal`) |
| clamp | explicit `v.clamp(origin, origin+vs)` after the solve | none after `solve_qef` |
| degenerate crossings | dropped (`n.length_squared() > 1e-12`) | all kept |
| empty-crossing fallback | `cell_surface_vertex_d` (centroid) | `continue` — no vertex |

Different normals ⇒ different QEF tangent planes ⇒ **different vertex positions**. Add differing
vertex order (chunk-sorted vs grid-major) and the seam duplicates of H4, and bit-identity cannot
hold. This collides with the plan's own **red line #4** ("No parity claim without a bit-identical
test"), so it must be resolved rather than waived.

### H6 — therefore: re-route Gate 1 to the `VoxelGrid` regional mesher (recommended)

`VoxelGrid::mesh_region_with_cells(a0, a1)` (`voxel_sculpt/grid.rs:2332`) is already a **regional**
mesher on the same type, taking cell ranges, and its quad loop is commented "same cell arrays +
winding as surface_nets" — written to match the extractor the remesh already uses. Gaps, all
additive, none requiring surgery on `chunked_dc`:

* it calls `cell_surface_vertex_d` (**centroid**), so it needs the `CellPlacement` parameter
  `surface_nets_placed` already takes — precisely CLAUDE.md's "extend an existing function with a
  parameter" over duplicating;
* it emits **triangles** (6 indices per quad) while the remesh needs **quads**;
* it has no retained per-chunk cache — that would live in `RemeshIncrCache`.

Taking this route makes H1, H2, H4 and H5 all evaporate, because the density/SDF boundary is never
crossed: parity holds **by construction** (same placement function, same winding, same iso
convention, one vertex per cell, no seam duplication), so Gate 1's bit-identical gate becomes
satisfiable instead of impossible. `object_display_mesh_chunked` (measured at §12: 14.9 ms vs
37.8 ms at res 256) shows the parallel-chunked win is already available on this side.

**Open decision for whoever implements Gate 1:** adopt H6, or keep `chunked_dc` and pay H1+H2+H4 plus
a downgraded (geometric-tolerance) parity gate. This plan now recommends **H6**.

---

## 4b. Gate premises that must be rewritten before their gate opens

*(Second-pass recon, 2026-08-09. Every item verified against live source. §4a covers Gate 1; these
kill or rewrite premises in Gates 2 and 3.)*

### G2-A — Gate 2 step 2 is unimplementable, and red line #3 forbids the only workaround

§7 step 2 says to prune octree nodes "with the existing `provably_interior_region` /
`provably_exterior_region` predicates rather than new ones". **`BrickClassifier` cannot be
instantiated in the remesh path.** Those predicates (`chunked_dc/grid.rs:628`, `:698`) take **zero
field samples**; their entire state derives from `&[SdfPrimitive]` — influence spheres from
`bounding_radius()` (`:446-448`), donors from `inscribed_radius()` (`:453-459`), carvers from
subtract/seam/extrude (`:470-473`). The remesh has **no `SdfPrimitive`s** at all: its field comes
from `io_bridge::mesh_to_sdf::region_mesh_to_sdf_band_stats` (`remesh.rs:193`). So red line #3
("no new octree classifier") as written contradicts its own gate.

**Rewrite step 2 to name the field-driven analogue that does exist:** `surface_chunk_mask`
(`voxel_sculpt/grid.rs:2575`) and its per-chunk min/max predicates at `:2586-2588`
(`has = cmin < iso && cmax >= iso`, `solid_present = cmax >= iso`), backed by
`chunk_minmax_sparse` / `chunk_minmax_dense` (`:2615`). That is density-native and is the correct
reuse target. Its dilation is **6-connected** (face neighbours only, `:2590-2610`) — an octree
prune must account for that.

### G2-B — the proposed `Qef` struct cannot compute the residual it is specified around

§7 step 1 proposes `Qef { ata: [f32;6], atb: [f32;3], mass: Vec3, n: u32 }` with a `residual` "that
drives collapse". The residual is `E(v) = vᵀ(AᵀA)v − 2vᵀ(Aᵀb) + bᵀb`, and **`bᵀb = Σ(nᵢ·pᵢ)²` is
accumulated nowhere** — `solve_qef` computes `np = n.dot(p)` (`mesh.rs:520`) and immediately folds
it into `atb`. **The struct needs a fifth field `btb: f32`** or step 3's collapse criterion cannot
be written. Three further mechanics gate the "threshold 0 ⇒ bit-identical" acceptance:
* **`mass` is not the Tikhonov target today.** `solve_qef` regularises toward the caller's
  `fallback`, which is the **cell centre** (`grid.rs:2199`) — wiring `mass` as the target changes
  output. `mass` is still needed as the raw sum of crossing positions, because both degenerate
  exits return the centroid `Σp/n` (`mesh.rs:545-546`, `:566-567`).
* **`cell_size` and `fallback` must stay solve-time parameters**, not stored state — callers pass
  different things (`grid.rs:2201` passes `voxel_size`; `mesh.rs:223`/`:429` and `extract.rs:344`
  pass `max_cell`).
* `reg = 0.01 · crossings.len()` is applied **once at solve time over the total count**, so `add()`
  must apply no per-crossing regularisation; and f32 summation is order-dependent, so `add()` must
  be called in the same `EDGE`-table order (`grid.rs:2719-2721`) or bit-identity fails for reasons
  unrelated to the octree. `op_impls.rs:1265`'s `Quadric` is a **layout** precedent only — it
  re-normalises its plane normal (`:1282-1287`) where `solve_qef` does not, and it is private and
  module-local. `solve_qef` is `pub(crate)`, so the wrapper refactor stays inside `core_math`.

### G3-A — Gate 3 step 3's premise is FALSE, and it is a live bug today

§3 and §8 both say new fields "ride the `FL4PMODF`/`FL4PSMTH` tail like `remesh_project`".
**No remesh field rides any tail.** `remesh_resolution` / `_smooth` / `_padding` / `_project` are
`#[serde(skip)]` (`node.rs:577-599`) with **no carrier at all**: `PolyModFlagRecord`
(`project/mod.rs:194-198`) is exactly `{ node_id, mod_index, bevel_consume }`, and
`grep -rn "remesh_" app/src/project/ app/src/project_save.rs app/src/project_load.rs` returns zero
hits. The doc comment at `node.rs:571-576` asserting otherwise is aspirational. Only
`smooth_falloff` actually rides a tail (`FL4PSMTH`).

**Consequence beyond this plan: every authored Remesh param silently resets to default on
save→load, today.** Filed as `REMESHPARAMSAVE` in `COMPLETE_BUG_TRACKER.md` (priority ladder #3).

**✅ RESOLVED 2026-08-09 — the carrier now exists.** A new `FL4PRMSH` tail section
(`POLY_REMESH_MAGIC`) carries `PolyRemeshRecord { node_id, mod_index, resolution, smooth, padding,
project }`, written LAST after `FL4PSMTH`, emitted only for non-default params so unused scenes
stay byte-identical, reattached by (node_id, stack index) with the usual forward-tolerance. It
deliberately has **no `geo_channel.is_all()` gate** — right for `FL4PSMTH`'s mask params, wrong for
a resolution. The live bug (`REMESHPARAMSAVE`) is fixed and the false doc comment at
`node.rs:571-576` corrected. Pinned by `poly_remesh_params_round_trip` (round-trip + no-magic-when-
unused + `FL4PSMTH` still decodes after it) and `collect_poly_remesh_emits_only_authored_params`.

**What Gate 3 still owes:** add `remesh_detail_levels` to `PolyRemeshRecord` — but **only while
this branch is unreleased.** Widening `FL4PRMSH` is safe exactly as long as no shipped build has
written it; once one has, widening becomes the same trap that forbids widening `FL4PMODF`
(`load_binary` propagates a section decode error with `?` and fails the ENTIRE load, `io.rs:310-317`
— documented in-tree at `io.rs:101-104` as why `FL4PSMTH` exists). If this ships first, Gate 3
needs its own section.

The rest of Gate 3 verified sound: `vertex_weights_auth` (`geometry_channel.rs:476-481`) is exactly
as described, `GeoChannelKind::Polygroup`/`EdgeWeight` exist and are append-only, and Masked
Smooth's consumption (`mesh_sync.rs:2076-2090`) is a clean template.

### G1-A — three concrete Gate 1 must-dos in the `chunked_dc` route

1. **Dirty-region off-by-one (would ship visible cracks).** A changed *sample* `s` dirties cells
   `{s−1, s}`, so a dirty sample window `[smin, smax]` maps to cells `[smin−1, smax]`. Passing
   `remesh.rs:364`'s `region` verbatim to `dirty_chunk_coords` **misses cell `smin−1`**, which lands
   in the previous chunk exactly when `smin % chunk_size == 0` → stale chunk → crack. Fix is
   caller-side: `min.saturating_sub(1)`. **This is already latent in the shipped SDF bake** —
   `app_poll_gpu.rs:1385` feeds a half-open voxel window straight in as inclusive cell coords.
2. **`chunk_size = 16`, not the SDF path's 32** — 16 is what `SN_CHUNK` (`grid.rs:1754`) and the
   churn bench (`remesh.rs:652`) use, so §12's churn table only predicts the Gate 1 result at 16.
3. **OOB sentinel.** `DensityStorage::at_i` returns **0.0** out of bounds (`grid.rs:54-63`), which
   under `iso − density` is **+0.5** — that does not have the "exceeds any real |distance|" property
   `BRICK_FAR = 100.0` exists to guarantee (`chunked_dc/grid.rs:53-56`), so a same-sign neighbour
   could form a false crossing. Use `get_idx` in-bounds and a `BRICK_FAR`-class sentinel outside,
   and assert `nverts()³ == density.len()` in the adapter.

Also note for scoping: `extract_half_edge` (`remesh.rs:81-133`) is **shared** by the stateless and
incremental paths (called at `:208` and `:409`), so Gate 1 must either parameterise it with an
optional cache/region or split the two call sites. And `region` **does not exist on every
incremental eval** — it is scoped inside `if any_changed {` (`:356`); a signature-only re-eval (a
`project` or `smooth` change) falls through to a whole-grid extract at `:409` with no region at all,
which any chunk cache must handle explicitly. Sharp rule for Gate 3: **a new field joins the
`break 'incr` guard only if it changes the LATTICE** — `remesh_detail_levels` does, so it must;
a purely post-extract field must stay out or every scrub drops to the full stateless path (which is
why `project` is correctly absent from the guard today).

Finally, red line #6's `MAX_*_PER_FRAME` cap is still unnamed by any gate.

## 12. Measurement log

### Gate 0b — the two existing benches, run 2026-08-09 in this worktree

*(§5 called these "apparently never run". Not quite: earlier numbers for the remesh live-edit work
exist in `COMPLETE_BUG_TRACKER.md` (the `REMESHLIVE` entry). What follows is a fresh same-machine,
same-build baseline so Gate 1's before/after is comparable. §5 also has a package error: the stage
harness is a **`core_math` integration test** — `cargo test -p app … bench_remesh_stages` matches
nothing and **exits 0**, which reads as a pass. Correct form:
`cargo test -p core_math --release --test bench_remesh_stages -- --ignored --nocapture`.)*

Worktree `.claude/worktrees/remesh-octree` (branch `feat/remesh-adaptive-octree`, snapshot `f66dfcc`
= master `5705bcc` + the uncommitted working tree). `cargo check -p app` = exit 0, 0 errors, so
**Gate 0a is closed here too**. Both benches `-p app --release`, both exit 0.

**`measure_drag_topology_churn` — the Gate 1 go/no-go.** Its own criterion: "payoff scales with how
FEW 16³ chunks contain a flipped cell. A handful of dirty chunks out of thousands ⇒ large win."

| res | drag speed (cell/frame) | topology-stable frames | avg flipped cells | avg dirty chunks |
|-----|------|-------|--------|------------------|
| 128 | 0.1  | 3/12  | 1      | **0.8 / 512 (0.15%)** |
| 128 | 0.25 | 0/12  | 2      | 1.1 / 512 (0.21%) |
| 128 | 1.0  | 0/12  | 11     | 1.2 / 512 (0.23%) |
| 128 | 3.0  | 0/12  | 478    | 2.9 / 512 (0.57%) |
| 256 | 0.1  | 4/12  | 1      | **0.7 / 4096 (0.02%)** |
| 256 | 0.25 | 1/12  | 196    | 4.4 / 4096 (0.11%) |
| 256 | 1.0  | 0/12  | 573    | 8.2 / 4096 (0.20%) |
| 256 | 3.0  | 0/12  | 418    | 6.1 / 4096 (0.15%) |

**Verdict: GO for Gate 1, by a wide margin.** A drag frame dirties 0.02%–0.57% of chunks — never
more than 8.2 chunks out of 4096. Regional extract is working on ~1/500th of the surface.
Secondary result: at 0.1 cell/frame, 3–4 of 12 frames flip ZERO cells, so **Tier 2.5** (skip extract
entirely when topology is stable) is real but is a slow-drag-only win — it collapses to 0/12 at
≥0.25 cell/frame and cannot be the primary mechanism.

**`bench_remesh_incremental_drag` — what V1 already bought, and what is left.**

| res | full stateless eval | V1 incremental drag frame | V1 speedup |
|-----|--------------------|---------------------------|-----------|
| 128 | 27.3 ms | 14.8 ms | 1.85× |
| 256 | 172.4 ms | 83.3 ms | 2.07× |

V1's regional *resample* only halves the frame. The residue — 14.8 ms at 128 and **83.3 ms at 256
(12 fps: a frozen drag)** — is the whole-mesh downstream S1–S4, which is exactly what Gate 1
targets. With 0.2% of chunks dirty, almost all of that 83.3 ms is avoidable work.

### Gate 0c — stage breakdown

An untracked harness for this already existed: `core_math/tests/bench_remesh_stages.rs`, with a
recorded 2026-08-08 baseline (`res=128 density=5.4 SN_rough=37.6 SN_smooth=42.4 chunked_par=4.6
from_mesh=21.0 to_raster=10.7`, ms). Two gaps found and one closed here:
* It timed `surface_nets(true)` — the **centroid** path — while the remesh calls
  `surface_nets_placed(smooth, Qef)` (`remesh.rs:93`). A `SN_QEF` row was added so S2 is measured as
  the remesh actually runs it.
* It does **not** cover S3 (`project_points_to_mesh`); that lives in `io_bridge` (`mesh_sdf_perf.rs`).

**Measured 2026-08-09, this machine, release** (`cargo test -p core_math --release --test
bench_remesh_stages -- --ignored --nocapture`, exit 0). Analytic sphere, so this is meshing cost
with the mesh→SDF stage excluded. All ms:

| res | density | SN_rough | SN_smooth | **SN_QEF (S2)** | chunked_par | **from_mesh (S4)** | to_raster | **seal (S1)** |
|-----|---------|----------|-----------|-------------|-------------|----------------|-----------|-----------|
| 32  | 0.6 | 1.0  | 0.9  | 0.8  | 0.5  | 0.5  | 0.4  | 0.2 (filled 17) |
| 64  | 0.2 | 1.3  | 1.6  | 1.7  | 1.1  | 1.3  | 1.7  | 1.0 (filled 105) |
| 128 | 0.6 | 5.7  | 6.6  | 7.9  | 4.1  | 5.8  | 6.1  | 6.7 (filled 664) |
| 256 | 3.2 | 31.4 | 34.5 | **37.8** | 14.9 | **21.0** | 24.3 | **45.9** (filled 3310) |

`SN_QEF` output is identical in size to `SN_smooth` (150264 quads / 150266 verts at 256) — the QEF
placement costs ~10% more than centroid and emits the same topology, as expected.

#### The scenario that actually matters: a mid-drag incremental frame, per stage

The table above is a *cold eval on a sphere*. The scenario Gate 1 optimises is a mid-drag
incremental frame, so a third probe was added — `measure_drag_frame_stages`
(`app/src/poly/remesh.rs`, `#[ignore]`, beside the other two). It drives the real incremental path
to a mid-drag state, then times the exact calls that path makes (`remesh.rs` 403 / 93 / 116 / 128)
on the exact grid it makes them on. Run:
`cargo test -p app --release measure_drag_frame_stages -- --ignored --nocapture` (exit 0).

**Median of 5** (the `ms5` house pattern — `bench_poly_smooth.rs:51` warns one-shot Windows timings
"are noisy enough to invert an ordering", and this gate turns on an ordering, so it was re-run):

| res | frame | **S1 seal (repeat)** | S2 extract | S3 reproject | S4 from_mesh | **S2+S4** | accounted |
|-----|-------|---------|------------|--------------|--------------|-------|-----------|
| 128 | 14.7 ms | 5.8 (39%) | 4.5 | 0.5 | 2.6 | **7.1** | 13.4 / 14.7 = 91% |
| 256 | 81.3 ms | **40.3** (50%) | 25.5 | 1.5 | 10.7 | 36.2 | 78.0 / 81.3 = 96% |

The decomposition accounts for 91–96% of the frame, so these four stages *are* the drag frame —
there is no hidden fifth cost. `S1 repeat` is the number to use: a drag frame always re-seals an
already-sealed field, so the first call overstates the steady state. (One-shot run for comparison,
same fixture: 128 → 5.6 / 4.8 / 0.6 / 2.6; 256 → 40.1 / 24.9 / 1.7 / 9.7. Medians moved every stage
by <1 ms and changed no conclusion.)

**The ordering INVERTS with resolution — this is the precise finding:**

* **res 128: S2+S4 = 7.1 ms > S1 = 5.8 ms.** The plan's precondition **holds**. S1 is still the
  largest *single* stage, but the regionalisable pair beats it, and the whole frame is 14.7 ms —
  already interactive.
* **res 256: S1 = 40.3 ms > S2+S4 = 36.2 ms.** The precondition **fails**. S1 alone is half the
  frame.

So this is specifically a **high-resolution** re-scope, which is exactly where it matters:
`REMESH_RES_MAX` is 256 and the owner raised the cap to 256 to work up there.

**Third scenario — episode-commit reconcile — needs no new harness.** §5 0c asks for it separately,
but it is by construction one **full stateless eval**: mid-episode the cache holds a deliberately
looser lattice (`lattice_headroom = 1.25`, `remesh.rs:152`/`:178`/`:422`), is flagged `diverged`
(`:435`), and commit reconciles with a single stateless pass. So its cost is exactly the `full=`
column above — **27.3 ms @128, 172.4 ms @256** — paid once per drag, not per frame. Recorded here
rather than built as a third bench.

**The headline: `filled 0/0`.** On this fixture the seal fills **nothing** — and still costs 40.1 ms
at res 256 (41.0 ms on a repeat call, so it is steady-state, not first-touch). It is a whole-field
span flood over 16.7M cells that runs every frame to discover there is nothing to do. The cost is
the *flood*, not the *fill*. That is the most actionable number Gate 0 produced.

It also sharpens §3's enclosure constraint: because the fill is one-way and any fill trips the
`had_enclosed_void` lockout, the **common case is precisely the case that pays full price for zero
work** — a field that never fills stays on the incremental path forever, re-flooding every frame.

**⚠ THIS PARTLY FALSIFIES THE GATE 0 PRECONDITION — Gate 1 must be re-scoped.**

The plan's §5 acceptance says: "If S2+S4 are not dominant, **stop and re-plan**." On the drag frame
at res 256, S2+S4 = **36.2 ms (45%)** while S1 alone = **40.3 ms (50%)**. So on the scenario that
matters, at the resolution that matters, **S2+S4 are not dominant — S1 is.** The literal reading of
the acceptance test says stop and re-plan; this section is that re-plan.

Gates 1–2 are *not* dead — 34.6 ms of regionalisable work per frame at 256 is a real win and the
churn data says it is nearly all avoidable. But S1 is not the minor item §6 treats it as ("Options:
(a) skip the seal … (b) keep it global but make it cheap"), and the arithmetic is unforgiving:

> **A perfect Gate 1 — regional extract driving S2 and S4 to ~0 — leaves 40.3 + 1.5 ≈ 42 ms per drag
> frame at res 256, i.e. ~24 fps. Still a frozen drag.**
> Conversely, fixing S1 alone takes the frame from 81.3 ms to ~41 ms (24 fps) — and doing **both**
> is what reaches an interactive drag at 256.

This is structural, not incidental: `seal_enclosed_air` is a whole-field span flood run on **every**
incremental frame (`remesh.rs:403`, inside `if any_changed`), and the Gate 0c harness already
carries the comment "whole-field span flood, so it does **not** shrink with the dirty region. This
is the floor V1 cannot go below." The measurement now puts a number on that floor — and `filled 0/0`
shows the floor is paid even when the seal has nothing to do.

**Consequence for sequencing:** S1 moves from an optional sub-problem of Gate 1 to a **required part
of it**. Gate 1 is not allowed to claim a drag-frame win at 256 while S1 is untouched. Red line #1
still binds — the `had_enclosed_void` lockout must not be weakened to buy the speed; the seal must
get *cheaper or conditional*, never *less correct*.

**Cheapest correct lever, now with evidence:** §6's option (a) — skip the seal when the dirty region
provably introduced no new air/solid boundary transition — is the right one, because `filled 0/0`
says the fill is almost always empty while the flood is never free. Fail closed: when in doubt,
seal and set `had_enclosed_void`. A cheap conservative precondition (did this splice create any new
air cell adjacent to the region border?) converts a 40 ms unconditional flood into a rare one. Do
**not** instead try to make the flood itself regional — enclosure is genuinely global (§3), and
`project_seal_enclosed_air_false_seals` records what happens when that is fudged.

At res 128 the plan's original ordering still holds (S2+S4 = 7.1 ms > S1 = 5.8 ms) and the frame is
already interactive at 14.7 ms — so Gate 1b alone would be the right call there. The re-scope is
driven entirely by res 256, where the ordering inverts.

Note also `chunked_par = 14.9 ms` vs `SN_QEF = 37.8 ms` at 256 — a chunked *parallel* mesher over the
same `VoxelGrid` (`object_display_mesh_chunked`) already runs ~2.5× faster than the serial extract
(~9× on the older 128 baseline), which is the headroom Gate 1 is claiming for S2.

## 12a. Gate 1a — conditional-vs-cheap seal: MEASURED, then made cheap

**Decision: §6 option (b) "keep it global but make it cheap", NOT option (a) "skip the seal".**
Recorded here with the reasoning so option (a) is not re-proposed without new information.

**Why not (a).** A skip needs a *provable* precondition, and the honest analysis does not yield a
useful one. The invariant on entry to any incremental frame is "all air is border-connected"
(the previous seal established it; the full path's sampler establishes it with `enclosed == 0`).
From there:
* If **no cell went air→solid**, the air set only grew, so every previously-border-connected cell
  stays connected and only the *newly-air* cells inside the dirty region need a local check —
  exact, and O(dirty region). Genuinely cheap.
* If **any cell went air→solid**, a pinch can disconnect an air region of unbounded size that
  extends far outside the dirty region. Deciding that exactly needs the global flood. Fail closed.

The second case is the common one: a dragged vertex sweeps the surface, so the leading edge turns
air→solid on essentially every frame. A correct conditional skip would therefore almost never fire.
Option (a) survives only as a cheap Tier-0 guard (skip when *zero* cells flipped — the churn table
says 3–4 of 12 frames at 0.1 cell/frame, and 0 of 12 at ≥0.25), which is not where the time is.

**Where the time actually was.** Profiling the three phases at 256³ (temporary instrumentation,
since removed):

| phase | ms | share |
|---|---|---|
| build air mask | 8.5 | 20% |
| **span flood** | **29.9** | **70%** |
| final scan | 4.5 | 10% |

So the flood, not the allocation, was the cost — which rules out the "reuse the buffer" fix that
the `project_poly_live_feel_bake_dominates` precedent would have suggested.

**The fix: the mask became a BITSET.** At 256³ a `Vec<bool>` mask is 16.7 MB and the span flood hops
between four neighbour rows `n` and `n²` cells apart, so every hop missed cache and went to DRAM.
One bit per cell is 2.1 MB, which fits in L3. Three changes, all bit-identical by construction:
1. **Bitset mask**, built in parallel per 64-cell word (no two threads share a word).
2. **Word-skipping `visit_row`** — the neighbour-row span being scanned is usually already visited,
   so masking 64 cells into one `u64` and testing `== 0` skips a fully-visited stretch in one
   compare. *This is the change the bitset exists to enable; the bitset alone was worth only 10%.*
3. **Word-accelerated run expansion** — every air cell is cleared exactly once across the flood
   (~n³ times total), now up to 64 per mask-and-store instead of one bit at a time.
4. Final scan iterates words, so "nothing enclosed" (the overwhelmingly common result) is a linear
   scan over 2.1 MB of zero words.

**Measured, `seal_enclosed_air` alone (analytic sphere, release):**

| res | before | bitset only | + word-skip scan | **+ word-run expand** |
|-----|--------|-------------|------------------|--------------------|
| 128 | 6.7 ms | — | 6.0 | **4.4 ms** |
| 256 | 44.0 ms | 39.6 | 32.5 | **24.0 ms** (−45%) |

**Measured on the real drag path** (`measure_drag_frame_stages`, median of 5, `filled 0/0`):

| res | S1 seal before | S1 seal after | frame before | frame after |
|-----|------|------|------|------|
| 128 | 5.8 ms | **4.2 ms** (−28%) | 14.7 ms | **12.6 ms** |
| 256 | 40.3 ms | **24.1 ms** (−40%) | 81.3 ms | **61.7 ms** (−24%) |

**The ordering flipped back, which is the important structural outcome.** At res 256 the split is
now S1 = 24.1 ms vs **S2+S4 = 36.1 ms** — S2+S4 are dominant again, so §5's stop condition is
satisfied and **the plan's original premise is restored**. Gate 1b (regional extract) is once more
the correct next step, and it is now the single biggest remaining item: driving S2+S4 → ~0 would
take the frame to ≈26 ms (≈38 fps) at 256, which is an interactive drag.

**Parity gate — new, and it did not exist before.** `seal_enclosed_air` had **no test of any kind**
despite guarding the one-way `had_enclosed_void` lockout (red line #1).
`core_math/tests/seal_enclosed_air_parity.rs` transcribes the pre-bitset body verbatim as an oracle
and asserts **bit-exact** density equality (`to_bits()`) plus equal fill counts over 8 fixtures ×
5 resolutions — including 17³ and 33³, which leave a *partial* final `u64` word (4913 % 64 = 49,
35937 % 64 = 33), the packing's off-by-one risk. Fixtures include a hollow shell (real enclosed
cavity), nested shells (two cavities), a vented shell (cavity NOT enclosed), all-air, all-solid,
and two pseudo-random fields. A second test pins the behavioural properties directly, including
**idempotence** — a drag frame re-seals an already-sealed field every frame, and a non-zero second
call would wrongly latch `had_enclosed_void` and banish the body to the full path forever.

## 12b. Gate 1b step 1 — the extract's own 66 MB scratch (done before any regional work)

Gate 1b is "re-extract only dirty chunks". Before building that, the same phase-profiling that
found the seal's real cost was pointed at `surface_nets_placed`, and found that **more than half of
it was not extraction at all**. At 256³ (ms):

| phase | ms | what it is |
|---|---|---|
| surface-chunk mask | 2.2 | |
| Phase A (cell vertices, parallel) | 5.8 | the actual QEF work |
| **`cell_vert` allocate + memset** | **13.7** | `vec![u32::MAX; cells³]` = **66 MB** |
| stitch | 1.4 | |
| Phase B (quads, parallel) | 2.9 | |

The cell→vertex table was a dense `cells³` array — 66 MB at 256³ — allocated and memset on **every
call**, to hold ~74k live entries. It is now stored **per live 16³ chunk**: a chunk the surface mask
marks dead provably emits no vertex (precisely the surface-chunk skip's existing invariant, since
Phase A never visits it), so dead chunks need no storage and read as `u32::MAX` — the same value the
dense table held there. Output unchanged; pinned by the existing
`surface_nets_parallel_matches_serial_reference` gate, which is the test that pins vertex numbering.

**Measured:**

| | S2 extract @256 | drag frame @256 |
|---|---|---|
| after Gate 1a | 25.7 ms | 61.7 ms |
| **after 12b** | **11.4 ms** (−56%) | **49.9 ms** |

Cumulative from the Gate 0 baseline: **81.3 → 49.9 ms** (−39%) with zero behaviour change.
At res 128 the frame is now 11.4 ms.

**Current split @256:** S1 seal 24.1 · S2 extract 11.4 · S3 reproject 1.5 · S4 `from_mesh` 10.8.
S1 is the largest stage again, and S2 is now smaller than S4. What remains of Gate 1b (true
regional re-extraction of only dirty chunks) is worth at most the 11.4 ms S2 still costs, so the
next-largest levers are S1 (24.1, needs the conditional-skip analysis in §12a revisited with a
sharper invariant) and S4 (10.8, the half-edge splice the plan calls "a gate of its own").

## 12c. S1 again — run-based components, and a FALSE-SEAL BUG in the shipped flood

Going after the seal's remaining 24 ms turned up a correctness bug that matters more than the speed.

### The bug: `seal_enclosed_air` was filling air that is reachable

The span flood aliased **"is air"** with **"not yet visited"** in one array. `visit_row` pushed only
the FIRST cell of a run it found, then cleared the rest of that run **without pushing it**, and
bounded the clear to the parent span's `x1`. On popping the pushed cell, its right-expansion
immediately saw the next cell already cleared and stopped. So:

* the cleared tail never seeded any neighbour-row scan, and
* whatever lay beyond `x1` in that run was never visited at all — reported unreachable, then
  **filled solid**.

Impact was not cosmetic. On the analytic sphere fixture — a solid ball with **no cavity whatsoever**
— the old code filled **3310 cells**. And because the incremental path does
`if c.grid.seal_enclosed_air() > 0 { c.had_enclosed_void = true; }` (`remesh.rs:403`), a false seal
**permanently banished an ordinary closed mesh to the full stateless path** for the rest of its
cache lifetime. The one-way lockout was being tripped by a bug.

`project_seal_enclosed_air_false_seals` records this same aliased-mask class biting once before
elsewhere, which is why it is worth naming rather than just fixing.

### Consequence for this document's method

The Gate 1a parity gate compared against a verbatim copy of the shipped flood — a **buggy oracle**.
That was still the right gate for Gate 1a (a pure `Vec<bool>`→bitset swap *should* be bit-identical,
and faithfully preserved the bug), but it cannot validate an algorithm change. The oracle is now a
plain 6-connected **BFS with a separate `seen` array**, written to be obviously correct rather than
fast — aliasing the two is exactly what broke the original. Red line #4 still holds; what changed is
*what* it is parity against.

### The replacement: connected components over RUNS, not cells

Air is overwhelmingly run-structured — the exterior of a remesh lattice is one or two contiguous
spans per row — so the grid is ~131k runs rather than 16.7M cells. Three passes: extract air runs
per row (parallel, one linear read of the density); union-find over runs, where two runs in y±1/z±1
neighbour rows are 6-connected iff their x ranges **overlap** (a two-pointer merge, since runs are
x-sorted); then fill any run not joined to the virtual OUTSIDE node. Border seeding matches the old
seed loop exactly: a whole row is border when its y or z is, otherwise only its x=0 / x=n−1 ends.

**Measured:**

| | S1 seal @256 | drag frame @256 | `filled` on the sphere |
|---|---|---|---|
| after 12b | 24.1 ms | 49.9 ms | 3310 (all false) |
| **after 12c** | **3.7 ms** (−85%) | **29.3 ms** | **0** (correct) |

### Where the drag frame now stands

**Cumulative: 81.3 → 29.3 ms at res 256 — 2.8×, ≈34 fps, an interactive drag.** At 128, 14.7 → 8.7 ms.

Split @256 is now **S1 3.7 · S2 extract 11.7 · S3 1.4 · S4 `from_mesh` 10.8**. S2 and S4 dominate —
which is the plan's ORIGINAL premise, restored for the second time. Gate 1b's regional
re-extraction is finally the correct next lever (worth up to 11.7 ms), with the half-edge splice
(10.8 ms) behind it as its own gate.

## 12d. Gate 1b — measured ceiling, spec, and one negative result

### The phase table that decides the gate (measured on the DRAG fixture, res 256, `smooth=true`)

The §12b table was taken on the analytic sphere and had no `relax` row. Re-measured on the actual
drag path, S2 = 11.7 ms splits:

| phase | ms | share | regional? |
|---|---|---|---|
| `surface_chunk_mask` | 2.2 | 19% | **yes** (per-chunk min/max reduction) |
| **Phase A — cell vertices (the QEF work)** | **4.1** | **35%** | **yes** |
| stitch (serial, assigns global vertex indices) | 2.2 | 19% | no |
| Phase B — quads | 1.4 | 12% | no |
| `relax_mesh` (smooth=true) | 1.5 | 13% | no — global Laplacian |

**Only 6.3 ms of the 11.7 is regional.** The other 5.4 ms is coupled through **global vertex
numbering**, and the churn table says topology flips on 11–12 of 12 drag frames at ≥0.25
cell/frame — so renumbering is not an edge case, it is every frame. Those phases must run
whole-grid or red line #4 is dead.

**Ceiling: S2 11.7 → ~5.5–6 ms; frame 29.3 → ~23.5 ms (−20%, 34 → 43 fps).** S2 can never go below
~5.5 ms. **After Gate 1b, S4 `from_mesh` (10.8 ms) is the largest stage by nearly 2×.**

### Verdict: REDUCED-SCOPE GO — but cache per-cz-SLAB, not per-chunk-per-cell

The obvious design (per-chunk, per-cell vertex cache) is the **wrong unit**. Phase A already emits
`Vec<Vec<(u32, Vec3, Vec3)>>` indexed by `cz` (`grid.rs:1805`). Caching *that* deletes most of the
adversarial surface: emission order is preserved by construction, the merge loop is untouched,
stitch/Phase B/relax need no dilation because the cache ends before them, and `smooth` stops being
a cache key because Phase A never sees it. ~150 lines instead of ~600, for ~1 ms more recompute.

**NO-GO:** chunk-granular per-cell caching; regionalising stitch / quads / relax.

### The two things that will silently corrupt output if got wrong

1. **Dilation is NOT the resample's dilation.** `cell_surface_vertex_qef_d` calls
   `gradient_normal(world, vs)` at every crossing (`grid.rs:2312`) with `eps` = one full voxel,
   whose trilinear fetch (`grid.rs:1284`) reaches grid points `[cx−1, cx+3]`. `cx+3` is attained at
   `t == 1.0`, which is reachable — `ops.rs:1335` writes `density = 0.5 − d/w`, so `d == 0` gives
   exactly `iso`. Inverting: a dirty grid point `g` dirties cells `[g−3, g+1]`. Add ±1 for the f32
   round-trip through `world_to_grid ∘ cell_world_pos` (it cancels `±half` and is not exact).
   ⇒ **`cell_lo = g_lo.saturating_sub(4)`, `cell_hi = (g_hi + 2).min(cells−1)`**, then snapped out
   to chunk boundaries **±1 chunk**, because `surface_chunk_mask`'s `need` loop reads the 6
   neighbour chunks. Only the `cz` span matters, since slabs are the cache unit.
2. **`seal_enclosed_air() > 0` must force full recompute in the SAME frame.** It writes arbitrary
   flat indices and returns only a count; the `had_enclosed_void` lockout guards the *next* frame,
   not this one. Post-12c it returns 0 on ordinary closed meshes, so this is rare — which is
   exactly why it would ship a phantom shell unnoticed.

Also: the retained key must be **`(chunk index, chunk-local cell)`, never the compacted slot** —
`chunk_slot` renumbers whenever the live set changes, so a retained slot silently addresses a
different chunk 16 cells away. Invalidation otherwise comes free: every `break 'incr` arm already
routes to `voxel_remesh_full`, which replaces `RemeshIncrCache` wholesale — so the cache must live
in that struct and nowhere else. A `primed: bool` covers the first incremental frame after a full
eval. Red line #6's cap: bail to full recompute above a `MAX_RECOMPUTED_SLABS_PER_FRAME`.
No-region case (`any_changed == false`): the grid is provably byte-identical, so reuse *every* slab
— declare the dirty state outside the `if any_changed` block, defaulting to `None`.

### Negative result — do not retry this

The serial stitch (2.2 ms) re-derives each vertex's slot with 3 divisions + 3 modulos, while
Phase A already knows `(cx,cy,cz)` and is parallel. Moving that arithmetic into Phase A **made it
worse**: S2 11.7 → 12.6 ms. So the stitch's cost is *not* the arithmetic — it is the random-access
writes into `cell_vert` plus vertex-array growth. Reverted. Anyone optimising the stitch should
target allocation and access pattern, not the index math.

## 12e. Gate 1b — SHIPPED (regional extract)

`VoxelGrid::surface_nets_placed_cached(smooth, placement, &mut SurfaceNetsCache, DirtyCells)`
retains Phase A output **per cz slab** and recomputes only the slabs the dirty region can reach.
`surface_nets_placed` delegates to the same impl with `None`, so the stateless path and the sculpt
display are byte-for-byte unchanged.

**Cache unit = the cz slab**, because that is exactly what Phase A already emits — so reuse cannot
perturb emission order, and therefore cannot perturb vertex numbering. The retained key is the
PLANE-LOCAL cell index (resolution-only), never the compacted chunk slot, which renumbers whenever
the live chunk set changes.

**Dilation, derived not guessed:** a dirty grid POINT `g` can change cells `[g−3, g+1]` (the
crossing-normal `gradient_normal` samples one full voxel, whose trilinear fetch reaches
`[cx−1, cx+3]`, and `cx+3` is reachable because `0.5 − d/w` hits `iso` exactly at `d == 0`), plus
±1 for the f32 round-trip ⇒ `[g−4, g+2]`, then snapped out ±1 **chunk** because a chunk can flip
live/dead from a change one chunk away (`surface_chunk_mask`'s `need` reads 6 neighbours).

**Invalidation is structural:** every `break 'incr` arm already falls through to
`voxel_remesh_full`, which replaces `RemeshIncrCache` wholesale — so res / smooth / padding / body /
topology / sealed-void changes drop the cache without needing their own checks. That is why the
cache lives in that struct and nowhere else. Explicit cases: `primed` covers the first incremental
frame after a full eval; `seal_enclosed_air() > 0` forces `DirtyCells::All` **in the same frame**
(it writes arbitrary flat indices and returns only a count); `any_changed == false` ⇒
`DirtyCells::None`, reuse everything.

**Measured @256:** drag frame **29.3 → 26.4 ms**, with **80 of 255 slabs recomputed** (31%) on the
real path — the probe prints that count so the win cannot be taken on faith. The dilation is the
limiter and it is necessary, not slack.

**Gates:** `regional_extract_parity.rs` — cached vs full extract compared bit-for-bit (`to_bits()`)
on vertices, normals and quads across res 32/64/128 × {Qef, Centroid} × {smooth, rough}, plus an
anti-vacuity assertion that a local edit does NOT recompute every slab (a cache that silently
recomputes everything would otherwise pass), plus zero-recompute on an unchanged grid, a cold cache
given a local hint, and placement-change invalidation. App side:
`incremental_interior_nudge_is_bit_identical` still passes with the cache live — the incremental
path still equals a stateless eval exactly.

**Cumulative: 81.3 → 26.4 ms @256 (3.1×, ~38 fps).** Split now S1 3.8 · S2 ~9 · S3 1.4 ·
**S4 `from_mesh` 11.0** — S4 is now the largest stage and the next lever.

## 12f. S4 `from_mesh` — measured, and the first slice taken

`HalfEdgeMesh::from_mesh` → `from_quads_with_materials` at res 256 (73748 verts / 73746 quads),
measured 9.8 ms:

| phase | ms | share |
|---|---|---|
| vertex alloc | 0.75 | 8% |
| **twin-linking quad loop** | **6.0** | **61%** |
| `recompute_normals` | 3.0 | 31% |

**Taken now:** `weighted_face_normals_and_areas` was a serial loop over every face while the
per-VERTEX pass beside it was already `par_iter`. Each face reads only its own ring, so it fans out
with no cross-face accumulation and the result is bit-identical; `for_each_init` keeps the
`ring`/`pts` scratch reuse the serial version had, now one pair per worker.
**Normals 3.0 → 1.9 ms; `from_mesh` 11.0 → 9.9 ms; drag frame 26.4 → 24.9 ms.**

**Left on the table — the 6.0 ms twin-linking loop.** 295k half-edges each doing a hash
remove-or-insert against a map peaking near 147k entries; `EdgeMap` already uses a single-`write_u64`
hasher, so the cost is cache misses, not hashing. The replacement is direct-addressed rather than
hashed: count outgoing edges per vertex, prefix-sum to CSR, then find `dst→src` by a short linear
scan of `bucket[dst]` (valence ~4). **It must reproduce the existing pairing semantics exactly**,
including the non-manifold case where 3+ half-edges share an edge and the current code pairs the
first two encountered and re-stores the third — so the replacement has to walk in the same quad
order and consume entries. `from_quads` is used by every mesh constructor in the codebase, so the
blast radius is the whole poly kernel and `cargo test -p core_math` is the gate. Deliberately NOT
started with limited budget left; it is the largest single remaining item.

**Cumulative: 81.3 → 24.9 ms @256 (3.3×, ~40 fps).** Split: S1 3.6 · S2 11.5 · S3 1.2 · S4 9.9.

## 13. Session log

- **2026-08-09 — GATE 0 CLOSED** in worktree `.claude/worktrees/remesh-octree`
  (branch `feat/remesh-adaptive-octree`, base snapshot `f66dfcc` = master `5705bcc` + the
  uncommitted working tree; `cargo check -p app` exit 0). Ran the two `#[ignore]` benches for the
  first time and added a third (`measure_drag_frame_stages`) because neither existing bench
  decomposed the scenario Gate 1 optimises.
  * **Gate 1 go/no-go: GO** — a drag frame dirties 0.02–0.57% of chunks (≤8.2 of 4096 @256).
  * **Re-scope:** on a drag frame at 256, S1 seal = 40.1 ms (50%) vs S2+S4 = 34.6 ms (43%), and the
    seal fills **nothing** (`filled 0/0`) while still flooding 16.7M cells every frame. §5's stop
    condition ("if S2+S4 are not dominant, stop and re-plan") fired; §10 now splits Gate 1 into
    1a (conditional seal) and 1b (regional extract).
  * **§4a added** — four §4 integration claims corrected against live source. The prescribed
    adapter `density - iso` is INVERTED (`iso - density` is correct, and equals `sdf/W`); it fails
    *silently*, passing a position-only parity test while flipping every quad winding and normal.
    Also: construct with `res - 1`; nothing welds chunked_dc's seam duplicates; and Gate 1's
    bit-identical acceptance is unachievable through `chunked_dc`, so §4a-H6 recommends re-routing
    to `VoxelGrid::mesh_region_with_cells` where parity holds by construction.
  * No production code touched — measurement probes only.
- **2026-08-09 — second-pass recon** (8 parallel readers over every §3/§4/§7/§8 anchor + the two
  named hazards). Added **§4b**: three gate premises are dead or false and must be rewritten before
  their gate opens.
  * **Gate 2 step 2 is unimplementable** — `BrickClassifier`'s prune predicates take zero field
    samples and derive entirely from `&[SdfPrimitive]`, which the remesh path does not have. Red
    line #3 therefore contradicted its own gate; the density-native replacement is
    `surface_chunk_mask`'s min/max predicates.
  * **Gate 2 step 1's `Qef` cannot compute its own residual** — `bᵀb` is accumulated nowhere; the
    struct needs a fifth field `btb`.
  * **Gate 3 step 3's premise is false** — no remesh field rides any tail; the carrier was never
    written. Surfaced a **live save-load bug** (`REMESHPARAMSAVE`, filed in the bug tracker): every
    authored Remesh param resets to default on load, today.
  * **§4a H1 corrected** — my first pass said the wrong sign flips "winding *and* normals". Normals
    are discarded by `from_mesh` and the QEF is quadratic in `n`, so only **winding** survives, and
    the honest sign (`iso − density`) is the one that flips it. Neither adapter alone suffices.
  * Hazard 2 confirmed a **non-issue** (exhaustive check over 5495 `(res, chunk_size)` pairs, 0
    failures) but concealed a real adjacent bug: the dirty *sample* window must be widened by
    `min.saturating_sub(1)` before it is used as a *cell* window — already latent in the shipped
    SDF bake at `app_poll_gpu.rs:1385`.
  * Measurement hardening: `measure_drag_frame_stages` now reports **median of 5** (the house
    `ms5` pattern from `bench_poly_smooth.rs:51`, whose comment warns that "one-shot timings on
    Windows are noisy enough to invert an ordering" — and this gate turns on a ~16% ordering).

- **2026-08-09** — Gate 0a closed: `cargo check -p app` green (exit 0, 0 errors) on master + working
  tree, after busting stale cargo fingerprints that had produced 50 phantom "no field" errors. Gate
  0b/0c (the two benches + stage breakdown) still open — nothing measured yet.
- **2026-08-09** — Plan authored. Code read, not run. Key finding: `chunked_dc` already provides
  dirty-chunk re-extraction with crack-free ghost padding, and `BrickClassifier` already provides the
  octree prune predicates — the poly remesh simply calls the whole-grid `surface_nets_placed` instead.
  Two integration hazards recorded (sign convention, chunk divisibility). Masked-decimation approach
  considered and demoted to §2 with reasons.
