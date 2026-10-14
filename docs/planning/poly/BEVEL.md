# BEVEL.md — Bevel Tool Knowledge Base (READ THIS FIRST, EVERY BEVEL TASK)

> **This is a multi-month, high-difficulty task.** Matching Blender's bevel exactly is one of the
> hardest modeling operations there is. This file is the **accumulator**: every session appends what it
> learned so we never re-derive a dead-end. If you are about to work on the bevel tool, read §0 → §6
> (the graveyard) before touching a line. When you finish, append to §6 and §11.

**Scope:** the **polygon** edge bevel (`core_math/src/bevel_op.rs`, driven by `app/src/poly_drag_tools.rs`).
The SDF/parametric per-edge bevel (`sdf_prim_eval.wgsl`) is a *different* system — its one relevant
lesson is in the graveyard (§6, "SDF per-edge max()"). Do not confuse the two.

---

## §0. READ-FIRST PROTOCOL (how to not go in circles)

1. **Blender is the oracle. Never guess geometry.** For any scenario, generate the Blender ground truth
   (§7) and diff vertex-for-vertex. Topology gates (watertight/manifold) are **necessary but not
   sufficient** — they passed spikes, folds, twists, and flipped normals. Every one of those was caught
   only by comparing against Blender or by a *geometric* gate we had to add after the fact.
2. **Check the graveyard (§6) first.** 22 approaches have already failed. If your idea is in there, it
   does not work — read *why* and the lesson that replaced it.
3. **The architecture is REBUILD, not mutate.** `bevel_op.rs` reads the mesh through the public API and
   rebuilds the whole thing via `HalfEdgeMesh::from_polygons`. `half_edge.rs` is **frozen**. You are
   producing `(positions, faces)` for `from_polygons`, nothing else. (§1, §2)
4. **Two hard invariants or it tears:** (a) every point that must be one physical vertex goes through
   `WeldMap::push` (weld by position → one index); (b) every shared edge is wound in **opposite**
   directions by its two faces (`from_polygons` twins by reverse direction). Break either → boundary
   edges → non-watertight. (§5)
5. **Gate at the app default (segments=3), not the easy seg=1 case.** The seg=1 test stayed green for
   months while the app tore on every rounded corner.
6. **No bandaids** (CLAUDE.md). A watertight-but-wrong result is still wrong. If validate() passes but it
   looks wrong, the missing gate is *geometric* (spike/simple/outward-normal), add it.
7. **When you finish, WRITE IT DOWN** in §6 (if you hit a dead-end) and §11 (session log). This file only
   pays off if it accumulates.

---

## §1. What the bevel is, and the architecture

A bevel replaces each selected **edge** with a **chamfer strip** (flat, or an arc for segments>1), and
each affected **corner** (vertex where beveled edges meet, or where a beveled edge lands) with a corner
fill. In our engine:

- **`bevel_edges_offset(&mesh, &selected_he, &BevelParams) -> HalfEdgeMesh`** is the entire operation.
- It **never mutates** the input. It reads via the public half-edge API and calls
  `HalfEdgeMesh::from_polygons(&positions, &faces)` once at the end. `half_edge.rs` stays frozen
  (CLAUDE.md poly rule). This is why correctness is verified purely on the *output*, not by trusting any
  incremental Euler operation.
- **App glue:** `poly_activate_bevel` arms the drag (§8 miter toast lives here); `poly_update_bevel_drag`
  / `poly_bevel_scroll_segments` set width/shape/segments; `poly_bevel_regenerate` calls the core and
  **guards the install** (§8). One producer for drag + scroll (no duplicate paths).

**`BevelParams`:** `width` (perpendicular in-plane offset — the value v1 fully consumes), `segments`
(1=flat chamfer, >1 rounds), `shape` (0=flat / **0.5=circular fillet = Blender default** / 1=convex),
`clamp` (limit width so an oversized bevel can't invert a face). Default `{0.1, 1, 0.5, true}`.

---

## §2. Half-edge primer (our kernel's invariants)

Our `HalfEdgeMesh` is a standard half-edge structure (the Guibas–Stolfi / OpenMesh / CGAL model; see the
Jerry Yin and Berkeley CS184 references below). Each undirected edge = **two directed half-edge twins**
pointing opposite ways; each half-edge stores `twin`, `next`, `prev`, `vertex`, `face`; a vertex stores
one outgoing half-edge; a face stores one of its half-edges.

**Kernel gotchas that bevel code depends on:**
- **`he.vertex` = DESTINATION**, not source. Source is `mesh.he_source(he)`. Confusing them flips the
  whole corner substitution.
- **`vertex_fan(v)`** = incident half-edges in cyclic order (used to walk a corner). Its `.len()` is the
  vertex **degree** — a degree>3 vertex was itself created by an earlier bevel.
- **HashMap/HashSet iteration is random** — sort keys wherever output IDs must be stable (§5 determinism).
- **`compute_face_normal` uses Newell's method** (area-weighted) — sign-correct on non-convex faces. The
  old first-3-vertex cross product flipped non-convex beveled faces (graveyard §6).

**Does the half-edge literature help?** For *traversal and invariants* (this section) — yes, it's the
canonical model. For the **hard part (the miter)** — no. Our bevel doesn't *edit* the half-edge structure
(EdgeFlip-style ops); it rebuilds via `from_polygons`. The miter is **computational geometry**
(offset-line intersection, superellipse profile) + **Blender parity**, which no generic half-edge tutorial
covers. Use the article for the kernel model; use Blender's `bmesh_bevel.c` (§3) for the algorithm.

*References:* [Jerry Yin — Half-Edge](https://jerryyin.info/geometry-processing-algorithms/half-edge/) ·
[CS184 Intro to Half-Edge](https://cs184.eecs.berkeley.edu/sp24/docs/half-edge-intro) ·
[CGAL Halfedge DS](http://graphics.stanford.edu/courses/cs368-00-spring/TA/manuals/CGAL/ref-manual2/Halfedge_DS/Chapter_hds.html)

---

## §3. Blender's target sequence — the algorithm to MIRROR (`bmesh_bevel.c`)

> **STUDY-ONLY.** Blender is GPLv2+/GPL3. We study the algorithm/sequence and reimplement cleanly — we
> **never copy source**. (Same rule as `bmesh-rs` STUDY-ONLY in CLAUDE.md.)
>
> *Detailed function-by-function map + `offset_meet` math + superellipse profile + miter table in
> §3.1–§3.6 below (clean-room study of `bmesh_bevel.c` + Howard Trickey's dev notes, cited in §3.6).
> Blender bevels IN PLACE on a live BMesh; AZRA rebuilds via `from_polygons` — the STAGES still map 1:1.*

Blender's edge bevel runs this pipeline (the sequence the owner wants us to mirror):

1. **BevVert per beveled vertex** (`bevel_vert_construct`). Gather the ring of edges around the vertex in
   CCW order as **`EdgeHalf`** records, each marked beveled-or-not with a left/right offset.
2. **`build_boundary`** — build the **BoundVert** cycle (the corner's boundary loop). For each pair of
   consecutive beveled edges, place a BoundVert at **`offset_meet`** = the **intersection of the two
   edges' offset lines** (with a fallback when they're near-parallel). **This is the stage we do not
   have** — it is exactly the miter point our code gets wrong (§9).
3. **Miter handling** inside build_boundary (`miter_outer`/`miter_inner`, SHARP / PATCH / ARC) — adds the
   extra BoundVerts that make a clean miter. Flat SHARP miter of 2 edges ⇒ **2 BoundVerts + 1 shared
   edge** (verified against Blender, §7).
4. **`build_vmesh`** — fill the boundary loop with the **VMesh**: a single **n-gon** (`M_POLY`) for the
   simple flat case, or an **adjacency grid** (`M_ADJ`, `bevel_build_rings`) subdivided for segments>1
   (Blender's "Grid Fill"). This is what a rounded multi-edge corner needs and our single center-fan cap
   cannot produce (graveyard §6, "k≥2 center-fan").
5. **`build_edge_polygons`** — the quad **strips** along each beveled edge, connecting adjacent BevVerts'
   BoundVerts.
6. **Cleanup** — reattach wires, merge UVs/normals.

**Profile:** Blender parameterizes the bevel profile as a **superellipse**; `profile=0.5` ⇒ a true circle
(what we match with `circular_arc` SLERP, §4/§6). Sample points along the profile scale with `segments`.

**The key architectural takeaway:** our offset "rails" ≈ Blender's per-edge offsets, and our strips ≈
`build_edge_polygons`. But we **approximate the corner** with ad-hoc re-trace cases + a cap, whereas
Blender computes an **explicit boundary** (`build_boundary`/`offset_meet`) first and then fills it. Adding
that missing boundary stage is the path to matching Blender (§9).

### §3.1 Blender data structures (what the sequence operates on)
- **`BevelParams`** — the operator's whole parameter block: `offset` + `offset_type`
  (OFFSET/WIDTH/DEPTH/PERCENT/ABSOLUTE), `profile` (superellipse `r`) + `profile_type`, `seg`,
  `miter_outer`/`miter_inner` + `spread`, `vmesh_method` (ADJ/CUTOFF), `ProfileSpacing` caches.
- **`BevVert`** — one beveled vertex: the original `BMVert`, `edgecount`/`selcount`, the CCW **`EdgeHalf`
  array**, wire list, and its **`VMesh`**.
- **`EdgeHalf`** — one END of an edge seen from a BevVert: `next`/`prev` (CCW), the `BMEdge`, adjacent
  faces `fprev`/`fnext`, `is_bev`, `offset_l`/`offset_r` (final) + `_spec` (requested), and `leftv`/
  `rightv` → the two BoundVerts bordering it. A beveled EdgeHalf attaches to **two** BoundVerts.
- **`BoundVert`** — a vertex on the corner boundary loop: `next`/`prev` (circular CCW **`nv` cycle**),
  `efirst`/`elast`, `ebev` (the beveled EdgeHalf on its left), `eon` ("on edge between" for non-beveled
  placement), its `Profile`, miter flags. **This is the object AZRA has no equivalent of.**
- **`VMesh`** — the corner fill: a `NewVert` grid addressed by `(i,j,k)` = (boundvert, ring, segment),
  plus `mesh_kind` = `M_NONE`/`M_POLY`/`M_ADJ`/`M_TRI_FAN`/`M_CUTOFF`.
- **`Profile` / `ProfileSpacing`** — the superellipse cross-section (control points + projection plane +
  sampled `prof_co`) and the cached even-spacing tables (`xvals`/`yvals` for `seg`; `xvals_2`/`yvals_2`
  for the power-of-two count `M_ADJ` needs; `fullness` biases interior bulge).

### §3.2 `offset_meet` — THE miter math (what we're missing as a stage)
Given two edges meeting at vertex `v`, each offset inward **on its own face by its own amount**, find the
single 3D point where the two offset lines cross. That point is the corner's BoundVert.
```
offset_meet(v, e1, e2, f):
    d1 = dir(v -> far end of e1);  d2 = dir(v -> far end of e2)
    if angle(d1,d2) ~ 0 or ~ 180:                 # parallel/collinear
        return v + offset * inplane_perp(f, avg(d1,d2))      # fallback
    n  = face_normal(f)                            # or avg of the two faces
    m1 = inplane_unit_perp(n, d1) toward interior of f
    m2 = inplane_unit_perp(n, d2) toward interior of f
    P  = intersect_lines_in_plane( line(v+off1*m1, d1), line(v+off2*m2, d2), n )  # 2x2 solve
    if edges_between:  P = project_onto_edge(P, edge_between)   # "on edge between"
    return P
```
Non-coplanar faces → use the closest-points-of-two-3D-lines midpoint (same intent, handles skew).
**AZRA already has this exact primitive: `solve_corner_lines()` — same in-plane 2×2 solve + same
parallel fallback (`v + 0.5*(pe_dir+ce_dir)*w`).** The problem is not the math; it's that Blender computes
the meet point **once** in `build_boundary` and every later stage reads that BoundVert, whereas AZRA
computes it per-rail and then the STEP-3 `(T,T)` case **throws it away** and emits one edge's rail (§6 #16).

### §3.3 Superellipse profile (a real parity detail to log)
Blender's cross-section is a quarter **superellipse** `|x/a|^r + |y/b|^r = 1`; the `profile` slider maps to
the exponent `r`:
- **profile 0.5 → r=2 → circular** (round fillet) — **agrees with our `shape=0.5`** ✅
- profile > 0.5 → r>2 → convex/bulged
- **profile 0.25 → flat chamfer**; profile < 0.25 → **concave**
- extremes → square-in / square-out corners

Blender samples at **even arc-length** (cached `ProfileSpacing`), not even angle; `segments=N` → `N+1`
samples/boundvert. **⚠ PARITY GAP TO REMEMBER:** our `shape` is a *blend* param (0=flat chord, 0.5=circle,
1=convex) — it matches Blender at 0.5 (circle) but **not** elsewhere: our flat is `shape=0` vs Blender's
`profile=0.25`, and **we have no concave** (`profile<0.25`). Full profile parity = adopt Blender's
`profile→r` superellipse + even-arc sampling later (not v1). Do NOT assume our slider == Blender's slider
except at 0.5.

### §3.4 Miter cases (2 beveled edges at a vertex) — counts to match
| Case | Mode | seg | Corner output |
|---|---|---|---|
| Flat sharp miter | SHARP | 1 | **2 BoundVerts + 1 shared edge** (one `offset_meet` corner, flat `M_POLY` cap) — the classic case, our current gap |
| Rounded | SHARP | >1 | 2 BoundVerts, interior filled by an **`M_ADJ`** seg×seg grid (center vert if seg odd, center quad if even) |
| Outer miter | PATCH | any | corner → **3 BoundVerts** + small patch |
| Outer miter | ARC | any | corner → **2 BoundVerts** joined by an arc; **Spread** offsets them |
| Inner miter | SHARP/ARC | any | inner-miter pattern shaped by the inner profile (default 0.5) |

**3+ beveled edges** (N-way): one BoundVert per CCW-consecutive edge pair (degree-3 → 3, degree-4 → 4),
filled by `M_ADJ`. Outer = corner angle > 180° (reflex/convex), inner = < 180°.

### §3.5 Blender → AZRA stage map (what exists / what's MISSING)
| Blender stage (fn) | AZRA equivalent | Status |
|---|---|---|
| `bevel_vert_construct` → BevVert + CCW EdgeHalf ring | `beveled_he` + per-face `Rail{dir,edge_dir}` (STEP 1) | Partial (no ring struct) |
| **`build_boundary` → BoundVert cycle** | **— none —** corners computed ad hoc in `rail_endpoint_pos` | **MISSING (core gap)** |
| **`offset_meet`** (place each BoundVert) | `solve_corner_lines()` math EXISTS but is per-rail + discarded by STEP-3 `(T,T)` | **Math present, not owned** |
| miter `miter_outer/inner` (SHARP/PATCH/ARC) | — none — shared corners force flat clamp | MISSING |
| `Profile` superellipse samples | `circular_arc` SLERP (exact at 0.5) + `shape` blend | Approximate (see §3.3) |
| `build_vmesh` (`M_POLY`/`M_ADJ`/…) | STEP 5 cap: single n-gon = `M_POLY` only; **no `M_ADJ` grid** | `M_POLY` only |
| `bevel_build_edge_polygons` (strips) | STEP 4 strips | **Present & correct** |
| cleanup / wires / normals | WeldMap + `from_polygons` + Newell | Present (different mechanism) |

### §3.6 The precise defect + the minimal fix (Blender-confirmed)
The multi-edge-miter overshoot is one architectural miss: **no `build_boundary` stage owning a BoundVert
cycle.** At a shared corner the `(T,T)` re-trace emits one edge's rail endpoint instead of the `offset_meet`
intersection, then the cap closes over it → overshoot. **Minimal change:** insert one explicit boundary
stage between STEP 2 and STEP 3 that, per beveled vertex, walks incident faces/edges CCW and calls
`solve_corner_lines` (wrapped as `offset_meet`) **once per consecutive beveled-edge pair** to produce a
`BoundVert{pos, ebev, eon}` cycle; then STEP 3/4/5 **read that cycle** instead of recomputing — `(T,T)`
pushes the shared BoundVert index, strips bridge BoundVert→BoundVert, the cap rings the cycle. This removes
the overshoot structurally AND unblocks rounded shared corners (each BoundVert carries its profile). Miter
modes (SHARP/PATCH/ARC) and a real `M_ADJ` grid are later extensions of the *same* cycle. This is §9.

*Sources: [bmesh_bevel.c](https://github.com/dfelinto/blender/blob/master/source/blender/bmesh/tools/bmesh_bevel.c) ·
[Howard Trickey — User:Howardt/Bevel](https://wiki.blender.org/wiki/User:Howardt/Bevel) ·
[Dev:Source/Modeling/Bevel (archived)](https://archive.blender.org/wiki/2015/index.php/Dev:Source/Modeling/Bevel/) ·
[BMesh kernel — DeepWiki](https://deepwiki.com/blender/blender/11.2-bmesh-editing-kernel) ·
[Bevel manual](https://docs.blender.org/manual/en/latest/modeling/meshes/editing/edge/bevel.html) ·
[2.80 miter Sharp/Patch/Arc](https://developer.blender.org/docs/release_notes/2.80/modeling/) ·
[superellipse profiles — devtalk](https://devtalk.blender.org/t/improving-non-circular-superellipse-bevel-profiles/18570)*

### §3.7 Rounded corner fill — `M_ADJ` VMesh (clean-room study of `bmesh_bevel.cc`, 2026-07-02)
The rounded shared-corner grid (§9 point 2). Full extraction (line refs are Blender `main`):

- **`mesh_vert(vm,i,j,k)` index** (`:854`): `i`=boundvert/sector, `j`=**ring** `[0,ns2]` (`ns2=seg/2`,
  `nj=ns2+1`), `k`=**segment** `[0,ns]` (`nk=seg+1`). Alloc = `count·(ns2+1)·(ns+1)`. **Grid only goes out to
  the HALF segment count in `j`** — the corner is covered `count` times (once per sector), sectors share
  boundaries.
- **Canonical/equiv verts** (`:4410`–`4459`): adjacent sectors OVERLAP; only a canonical subset is computed,
  the rest are copies (`is_canon`/`mesh_vert_canon`/`vmesh_copy_equiv_verts`). Sector `i`'s high-`k` column ==
  sector `i+1`'s high-`j` row = the **same NewVert** → seam-free, no weld. This is the stitch AZRA would need
  (our equivalent = WeldMap by position, which we already have).
- **Dispatch** (`bevel_build_rings :5888`, `build_vmesh :6452`): `count==2` → **M_NONE weld** (NOT a grid —
  see below); `seg==1` → `M_POLY`; else `M_ADJ`. **M_ADJ requires `count≥3` AND `seg>1`.**
- **ODD vs EVEN seg** (`:5888`–`6110`): quads emitted per sector for `j∈[0,ns2)`, `k∈[0,ns2+odd)`. EVEN →
  a single SHARED center vertex `(0,ns2,ns2)` (grids meet at one point, no center face). ODD → one extra
  column + a central **`count`-gon** built from the `count` innermost verts (`build_center_ngon`).
- **`adj_vmesh`** (the generator, `:5082`): (1) seg-2 control cage = boundvert corners + each arc's midpoint
  (`get_profile_point`) + ONE center control vertex placed by **`fullness`** (`center = boundverts_centroid +
  (orig_vertex − centroid)·fullness`; fullness>0 bulges toward the sharp vertex, <0 concave; circle+small-seg
  uses a `circle_fullness[]` table, else a linear regression). (2) `cubic_subdiv` (Catmull-Clark doubling with
  cubic boundary smoothing + **Sabin center weight** `sabin_gamma(count)` for the valence-`count`
  extraordinary point) repeated until `seg` reached; (3) `interp_vmesh` bilinear-resamples if `seg` isn't a
  power of two. This is an **N-sided-hole Catmull-Clark fill** (Levin 1999) seeded by the profile arcs + one
  fullness center. `n_bndv==3` cube-corner has an analytic shortcut (`tri_corner_adj_vmesh`).
- **Boundary ring `(i,0,k)` == the edge strip's end profile** — the corner reuses the strip's boundary verts
  (no seam). `(i,0,0)`=boundvert, `(i,0,ns)`=next boundvert.
- **⚠ KEY CORRECTION for AZRA:** a *rounded 2-edge* corner is **M_NONE (weld)**, NOT M_ADJ. The two profile
  arcs are co-planarized (`move_weld_profile_planes`) and bridged directly by per-`k` shared midpoints —
  `ns−1` shared interior verts, **zero corner grid**. Our current 2-edge miter (STEP 2b, flat) is the seg=1
  case of exactly this; the rounded version = sample each side's arc and midpoint-bridge them (our WeldMap
  gives the sharing for free). The **grid** (`bevel_build_rings`) is ONLY for `count≥3` corners with `seg>1`.

*Source: `source/blender/bmesh/tools/bmesh_bevel.cc` (branch `main`) + Levin 1999 "Filling an N-sided hole
using combined subdivision schemes" (cited in `cubic_subdiv`'s own comment `:4663`).*

**DECODED TARGET (oracle `nway_pyramid_seg3_ref.json`, 2026-07-02): pyramid apex 4-way corner, seg=3.**
The corner fill is a **2-ring grid** (nv=32/nf=26 total; the corner region = 16 verts + 9 quads):
- **ring j=0 outer** (the 4 BoundVerts) + **ring j=0 arc-interior** (8 verts = ns−1=2 per beveled arc) =
  the **12-vert boundary loop**, SHARED vertex-for-vertex with the edge strips (this is exactly AZRA's
  `loop_pts` in STEP 5 when the corner is un-clamped — already computed).
- **ring j=1 inner** (4 verts, y=0.596) = the converged inner ring.
- **9 quad faces**: 8 grid quads stitching outer→inner (e.g. `[bv0, arc0a, inner0, arc3b]`,
  `[arc0a, arc0b, inner1, inner0]`, …) + **1 center quad** over the 4 inner verts `[6,9,12,15]`.
**THE PARALLELISM CONSTRAINT (oracle `rounded_cube_ref.json`, 2026-07-02): Blender's corner grid verts
PRESERVE each edge fillet's cross-section.** Rounded ±1 cube, all edges, seg=2, offset 0.3 → the +++ corner:
`42[0.7,0.7,1.0]` `43[1,0.7,0.7]` `44[0.7,1,0.7]` (fillet ends at the offset 0.7) · `45[0.912,0.7,0.912]`
`47[0.912,0.912,0.7]` `48[0.7,0.912,0.912]` (each KEEPS an edge fillet's arc-interior cross-section — e.g. 45
holds the +X+Z fillet's `(x,z)=0.912` and only drops `y` to the corner boundary 0.7) · `46[0.873³]` (center).
This is WHY Blender edges stay parallel: the corner-adjacent grid verts hold the fillet's perpendicular
offset, they do NOT get pulled onto a sphere. A sphere/SLERP dome (the current v1 approximation) BREAKS this
(user-reported 14/16 non-parallel). The exact fix = the M_ADJ `(i,j,k)` grid where ring `j=0` IS the arcs
and each `(i,j,k)` boundary column preserves edge `i`'s cross-section. So the corner interior is NOT a
free-form sphere — it is constrained by the incident edge fillets. This is the load-bearing detail for the
vertex-exact grid.

So the vertex-exact job is: (1) route the N-way corner through a rounded boundary loop (NOT the STEP-2b
flat BoundVert collapse — that's why degree>3 miters currently can't round: they collapse to flat); (2)
place the interior ring(s) via `cubic_subdiv` Catmull-Clark (fullness center + `sabin_gamma(count)`); (3)
tessellate the `(i,j,k)` grid + center n-gon and weld the boundary loop to the strips. The v1 dome (§11
2026-07-02 PM3) already does (1)-lite + a single-fan interior for **degree-3** corners; the exact grid
generalizes the interior to N rings AND unblocks degree>3 by replacing the STEP-2b collapse with this
rounded boundary. Oracle scripts: `nway_pyramid_seg3_ref.{py,json}`.

---

### §3.8 Arc INNER miter — clean-room study from the oracle (2026-08-11)

Measured, not inferred: `miter_inner_ref.py` dumps the same base under each miter mode and the corner
region is extracted around one inner (concave) corner at `(0.6, 0.6, 1.0)`. Params w=0.1382, seg=3,
profile 0.5, **spread 0.1**. See §11 2026-08-11 for the counts table.

**SHARP inner (what we already produce, vertex-exact) — 4 verts, all ON the bisector diagonal `x==y`:**
`(0.73820,0.73820,1.0)` → `(0.66910,0.66910,1.01852)` → `(0.61852,0.61852,1.06910)` →
`(0.60000,0.60000,1.13820)`. One profile arc lying in the bisector plane; the two edges' rails meet at a
single point per profile level. This is the whole sharp miter.

**ARC inner — 12 verts.** The structure, level by level (z ascending):

| z | SHARP | ARC | what changed |
|---|---|---|---|
| 1.00000 (outer end) | 1 pt `(0.7382,0.7382)` | **1 pt, IDENTICAL** | the outer end is NOT spread |
| 1.02032 | — | 2 pts, mirrored across the diagonal | |
| 1.03565 | 1 pt (1.01852) | 1 pt on diagonal | |
| 1.07064 / 1.07273 | 1 pt (1.06910) | 2 + 2 pts, mirrored pairs | |
| 1.13820 (= 1 + w, inner end) | **1 pt** `(0.6,0.6)` | **4 pts**: `(0.5,0.6)`, `(0.54908,0.58516)`, `(0.58516,0.54908)`, `(0.6,0.5)` | the sharp meet point becomes an ARC |

🔑 **The mechanism: the single innermost meet point is replaced by an arc swept around the corner, and
`spread` is what sizes it.** `0.6 − 0.5 = 0.1 = spread` exactly — the arc's two ends sit back along each
beveled edge by `spread` from the corner, and the intermediate points bow across. The OUTER end of the
profile is untouched, so the two modes share that vertex.

⚠ **`spread` is a SECOND parameter, not a free-standing toggle.** Blender exposes Spread only when a
miter is set to Arc. A Sharp/Arc dropdown alone cannot reproduce this table — shipping one without
Spread would produce a fixed, wrong arc size.
⚠ **It is NOT "one extra ring".** The intermediate levels are mirrored PAIRS with a lone diagonal point
between them (2/1/2/2), and the corner's face arity goes `{4:10}` → `{4:14, 3:1, 7:2}`. That is a VMesh
grid with the miter arc as one boundary, i.e. the same machinery family as §3.7's `M_ADJ` — **not a
variation on `miter_weld_arc`, which bridges exactly two BoundVerts with one profile arc.** Sizing an
implementation off `miter_weld_arc` will under-estimate it; the honest sibling is the §3.7 grid, which
§9 already lists as the standing next step.

**FULL EXTRACTED PATCH (the implementation target — do not re-derive, it is measured).** One inner
corner, 12 verts / 7 faces = **6 quads + 1 triangle**, plus the arc absorbed into the adjoining wall
face (that is where the `7:2` arity comes from). Local names: `O` = outer point (shared with Sharp,
unchanged), `S0/S1` = the two spread points, `A0/A1` = the top-arc interior, `D` = the diagonal
mid-point, and mirrored pairs `P*`:

| name | position | role |
|---|---|---|
| `O`  | `(0.73820, 0.73820, 1.00000)` | outer end — IDENTICAL to Sharp, shared |
| `S0` | `(0.50000, 0.60000, 1.13820)` | inner end, pulled back along edge 0 by `spread` |
| `S1` | `(0.60000, 0.50000, 1.13820)` | inner end, pulled back along edge 1 by `spread` |
| `A0` | `(0.54908, 0.58516, 1.13820)` | top-arc interior |
| `A1` | `(0.58516, 0.54908, 1.13820)` | top-arc interior |
| `P0a`/`P0b` | `(0.57574,0.62033,1.07064)` / `(0.62033,0.57574,1.07064)` | ring 1 pair |
| `P1a`/`P1b` | `(0.59014,0.60485,1.07273)` / `(0.60485,0.59014,1.07273)` | ring 2 pair |
| `P2a`/`P2b` | `(0.66246,0.67064,1.02032)` / `(0.67064,0.66246,1.02032)` | ring 3 pair (nearest `O`) |
| `D`  | `(0.63001, 0.63001, 1.03565)` | on the bisector diagonal `x==y` |

Faces (oracle f27–f33): `[S0,A0,P1a,P0a]`, `[A0,A1,P1b,P1a]`, `[S1,P0b,P1b,A1]`, `[P0b,P2b,D,P1b]`,
`[O,P2a,D,P2b]`, `[P2a,P0a,P1a,D]`, and the tri `[P1a,P1b,D]`.

**✅ THE TOP ARC IS A CIRCLE — CLOSED FORM, DERIVED FROM A SEGMENT SWEEP (`miter_arc_sweep.py`).**
*(Corrected same session: an earlier line here said "NOT a circle". That measured from the CORNER
`(0.6,0.6)`, which is the wrong centre — `A0`/`A1` sit at 0.053 from the corner, so a corner-centred
circle is indeed wrong. The right centre makes it exact.)*

With `M` = the SHARP inner meet point, and `dA`/`dB` the two beveled edges' unit directions at `V`:

```
S0 = M + spread·dA          S1 = M + spread·dB
C  = M + spread·(dA + dB)   → |S0 − C| = |S1 − C| = spread
arc = circle(centre C, radius spread) from S0 to S1
```

Check against the oracle: `M=(0.6,0.6)`, `dA=(-1,0,0)`, `dB=(0,-1,0)`, `spread=0.1` ⇒ `S0=(0.5,0.6)`,
`S1=(0.6,0.5)`, `C=(0.5,0.5)`, r=0.1 — all three exact. Radius holds for non-perpendicular edges too
(`|S0−C| = spread·|dB| = spread`).

🔑 **Blender is exact ONLY at power-of-two segment counts; its own sampling deviates elsewhere.**
Interior-point radius ÷ spread, swept:

| seg | 1 | 2 | 4 | 3 | 5 | 6 |
|---|---|---|---|---|---|---|
| factor | (no interior) | **1.00000** | **1.00000** ×3 | 0.98293 | 0.99538 / 0.99692 | 0.99572 (45° pt **1.00000**) |

`ns ∈ {2,4}` land on the circle to 5 decimals; `ns ∈ {3,5,6}` are pulled IN by up to 1.7%, and at
`ns=6` the 45° midpoint is still exact while its neighbours are not — the signature of a subdivision
resample, not a different intended shape. **So: implement the exact circle.** Gate vertex-exact at
`ns ∈ {1,2,4}` and allow ≤2e-2·spread at other counts, with a comment naming this table — do NOT
chase Blender's resampling artifact, and do NOT let a failing `ns=3` gate send the next session
looking for a different curve.

⚠ **STILL OPEN: the patch INTERIOR.** The closed form above gives the top-arc BOUNDARY only. The six
interior verts (`P0a/b`, `P1a/b`, `P2a/b`, `D`) and the 7-face fill are still the §3.7 `M_ADJ` VMesh
solve. The boundary being closed-form removes the largest unknown, but it does not remove that work.

#### §3.8a ✅ CLOSED 2026-08-12 — how the top arc attaches to the WALL faces (`miter_inner_ref.py`)

§3.8 flagged the `7:2` arity in passing and left the attachment undetermined, which is the one thing
that decides whether an implementation is watertight. **Measured, not inferred** — re-run
`docs/planning/poly/miter_inner_ref.py` (it reproduces §3.8's table exactly: 12 verts,
`{4:13, 7:2, 3:1}`) and it dumps the whole corner neighbourhood, walls included:

🔑 **The two 7-gons are the two BOSS WALL faces, and they ABSORB the top arc, splitting it at ONE arc
vertex.** The unbeveled vertical edge's foot moves from the sharp meet `M` onto that arc vertex.
At `ns=3` the walls come out as

```
wall +Y : [ S0', A0', A1', (-0.6,0.6,2), (0.6,0.6,2), A0, S0 ]      ← takes S0..A0
wall +X : [ S1, A1, A0, (0.6,0.6,2), (0.6,-0.6,2), A1'', S1'' ]     ← takes A0..S1
```

so `A0` is shared by both walls and is the new foot. **The walls therefore become NON-PLANAR** (`A0`
is off both wall planes) — that is Blender's own output, not an error to correct.

**The split index is deterministic (`miter_inner_wall_split.py`, ns 1→6).** Indexing the arc from the
`S1 = M + spread·dB` end:

| ns | 1 | 2 | 3 | 4 | 5 | 6 |
|---|---|---|---|---|---|---|
| arc pts | 2 | 3 | 4 | 5 | 6 | 7 |
| split idx | 1 | 1 | 2 | 2 | 3 | 3 |

i.e. **`ceil(ns/2)`** — the exact middle for even `ns`, biased one step toward the `S0`/`dA` end for
odd `ns`. `ns=1` degenerates to the endpoint `S0` (the arc is a single chord and one wall gets all of
it). The odd-`ns` bias is a tie-break from Blender's internal edge order, so a port only has to be
DETERMINISTIC here, not identical — but it must split at an arc VERTEX, because splitting mid-segment
has no vertex to be the foot and tears.

⚠ **The side chains are NOT a linear spread ramp.** Blender's strip-A boundary at `ns=3` is
`O → P2a(0.66246,0.67064,1.02033) → P0a → S0`, whose displacement off the SHARP chain point
`(0.6691,0.6691,1.01852)` is only 0.0066 — a linear `spread·i/ns` ramp would put it at 0.0333, 5×
further. The ramp is concentrated toward the inner end because it falls out of the `M_ADJ` solve.
**So a port that uses the closed-form arc + its own ramp is vertex-exact at `O`, `S0`, `S1` and the
top arc, and approximate on the two side chains** (≤0.03 on a 0.14 bevel). Say so rather than gating
a test that cannot pass without `M_ADJ`.

---

## §4. Our current pipeline (`bevel_edges_offset`, STEP 0→REBUILD)

| Step | Name | Purpose | Anchor |
|---|---|---|---|
| **STEP 0** | Canonicalize selection | Sort selected half-edge IDs (determinism); reject boundary/NO_FACE + twin-boundary edges; pick ONE primary directed half-edge per undirected edge. Populates `beveled_he`, `primary_edges`. Early-return `clone()` if width≤0 / empty / no manifold edge. Caches per-face centroid+normal (`face_data`). | preamble ~:86–143 |
| **STEP 1** | Offset rails | Per directed beveled half-edge: constant inward in-plane perpendicular `dir = normal × edge_dir`, sign-fixed toward the face centroid. Applies **Clamp Overlap** — Blender's `geometry_collide_offset` (collapse + slide walks), accumulated into a **per-edge capacity map `cap`**. With NO `BevelWidthPolicy` the global min of `cap` throttles the whole selection (original behaviour); WITH one, each edge takes its own ceiling plus `opposing_pair_caps` (`clamp.rs`) — so a tight pair pins at its meet point, where the `WeldMap` merges the two rails (Auto Merge Vertices). `Rail{src_idx,dst_idx,src_base,dir,edge_dir,w}`. | `// STEP 1` :145 |
| **STEP 2** | Weld rail endpoints | Compute welded index of each rail endpoint. If the adjacent in-face half-edge is also beveled, `solve_corner_lines` **intersects the two coplanar offset lines** → ONE welded point; else `V + dir·w`. Read-phase (compute) then write-phase (weld+assign) to satisfy the borrow checker. | `// STEP 2` :206 |
| **STEP 2.5** | Segment arcs (profile) | Per edge, arc of `n_seg+1` welded points face-L→face-R. `n_seg` clamped [1,32]; **forced to 1 when a shared multi-edge corner exists** (rounded multi-edge = v2). Profile blend: 0=flat chord, 0.5=`circular_arc` (Blender superellipse 0.5), 1=convex. Stores `arc_at[(eid,v)]`, `edge_arcs[eid]`. | `// STEP 2.5` :283 |
| **CAP-NEEDED** | Corner classification | `k = legs/2` (beveled edges at v). Cap iff `k≥2` OR (`k==1` & `n_seg≥2` & no `same_edge` seal). For `k==1` cap corners compute `partial_center` (arc centroid recessed 0.3 toward V — Blender deletes V, adds one recessed center). Materialize `center_idx`. **Coupled** with STEP 3's re-add-V. | ~:353–432 |
| **STEP 3** | Re-trace faces (4 cases) | Untouched faces copy verbatim. Affected faces re-trace each corner by `(pe_beveled, ce_beveled)`: **(T,T)** push `rails[ce].src` (shared welded corner); **(F,T)** `rails[ce].src`; **(T,F)** `rails[pe].dst`; **(F,F)** per-side rails (`ce.twin.prev`/`ce.twin.next`) — with cap→[pe-rail, center-or-V, ce-rail]; same_edge→whole arc (deterministic reverse); else→that side's endpoint. | `// STEP 3` :434 |
| **STEP 4** | Chamfer strips | Per edge, `n_seg` quads bridging L/R arcs, wound `dst[i]→src[i]→src[i+1]→dst[i+1]` (OPPOSITE the bridged face so `from_polygons` twins them → watertight). | `// STEP 4` :554 |
| **STEP 5** | Corner cap | Build rim by walking the vertex fan (NEVER angle-sort); chain arcs by shared endpoint (`append_chained`). Orient once against the twin sense (`cap_forward = !v_is_src`, Newell fallback). Emit: partial→open-arc fan to recessed center C (deletes V); simple k≥2→one n-gon; non-planar→centroid fan. | `// STEP 5` :574 |
| **STEP 6** | Residual-hole seal | Only if input was closed. Any directed edge with no reverse twin bounds a hole → chain into loops → cap each with its REVERSED loop (twins every boundary edge). Safety net; no-op on clean cubes. | `// STEP 6` :674 |
| **REBUILD** | `from_polygons` | Hand `(weld.positions, faces_out)` to `HalfEdgeMesh::from_polygons`. Sole topology mutation. | ~:716 |

**Key data structures:** `WeldMap` (position→index, 1e-5 grid — the anti-tear identity), `Rail`,
`beveled_he`/`primary_edges`/`primary_set`, `rails`, `arc_at`/`edge_arcs`, `cap_needed`/`partial_center`/
`center_idx`, `beveled_vertices`/`affected_faces`, `faces_out`.
**Helpers:** `circular_arc` (SLERP profile), `solve_corner_lines` (2×2 in-plane offset-line intersection —
this is our *partial* offset_meet, only wired for the STEP-2 adjacent case), `append_chained`,
`newell_idx`, `is_simple_loop`, `dedup_ring`, `weld_orig`, `rail_endpoint_pos`.

---

## §5. The load-bearing invariants (break these → it tears)

1. **`from_polygons` twins by REVERSE direction / vertex INDEX** (never coordinate). Two copies of an
   undirected edge twin only if one runs a→b and the other b→a. Forces (2) and (3).
2. **Weld everything through `WeldMap::push`** (1e-5 grid). A shared corner must be ONE index or the mesh
   seams. Never hand-assign indices.
3. **Strip winding `dst→src→src→dst`** (opposite the bridged face). Same direction on both copies → no
   twin → boundary edge = the classic non-watertight chamfer.
4. **Corner cases:** (T,T) share one welded corner; (F,F) is per-side (NOT whole-arc) at degree>3; the
   `same_edge` whole-arc seal is oriented by *which face arc[0] lies on* (`pe_face==fl?`), NEVER by
   angle-sort.
5. **cap_needed ⇄ re-add-V coupling:** the (F,F) re-trace re-adds V (or center C) iff `cap_needed(v)` — V
   supplies the twin partner for the cap's radial edges. Decouple them and the cap orphans.
6. **Cap rim in fan order, never angle-sorted;** orient once against the twin sense.
7. **Determinism:** sort selection / rail_keys / pc_keys / cap_verts — output IDs must be reproducible.
8. **`dir` points INTO the face** (sign-fixed by centroid), constant per half-edge → rails parallel to
   the edge for any dihedral.
9. **Clamp width to each edge's CAPACITY** — an oversized offset slides a rail past a neighbour and
   inverts the face. *(Corrected 2026-08-10. This used to read "½ the shortest incident edge", which
   has been wrong since the `geometry_collide_offset` port: that blanket rule is exactly what the
   port REPLACED, because it cut a cylinder cap loop at 0.26·r where Blender allows 0.97·r — see the
   STEP-1 comment at `mod.rs:402-404`. Re-deriving the old text would reintroduce that bug.)* The
   capacity is the collapse limit `len(B)/denom` plus the slide walks, per edge. With no
   [`BevelWidthPolicy`] the GLOBAL minimum of that set applies; with one, each edge takes its own
   ceiling and must additionally bound facing NON-ADJACENT edges inside one face
   (`opposing_pair_caps`) — the global minimum was covering that collision by accident, and going
   per-edge removes the accident. **Pinning at a per-edge ceiling is also the MERGE**: two rails that
   stop at the same meet point are welded into one vertex by `WeldMap`, which is why Auto Merge
   Vertices needed no merge code of its own.

---

## §6. THE GRAVEYARD — approaches that FAILED (never retry)

> The single most important section. Each entry: **tried → failed because → lesson**. If your idea is
> here, it does not work.

**FINDING A BEVEL STRIP'S CORNERS BY TURN ANGLE — DEAD (removed from `unbevel.rs` 2026-08-11).**
`split_loop_into_rails` took the four LARGEST turn angles in the strip's boundary loop as its four
corners. **Tried → failed because** that is a property of a bevel of ONE STRAIGHT edge, not of a bevel
strip: as soon as the beveled run bends, the rail's bend is as sharp as an end cap and outranks it, so
corners land mid-rail and the loop splits into unequal "rails" (2/6, 7/2, 2/7). A strip's two rails are
the two sides of a grid and are EQUAL by construction, so any split that does not produce equal
opposite chains is wrong by inspection. **Lesson:** a strip's corners are TOPOLOGICAL — a boundary
vertex touched by exactly one strip face — and its boundary is a grid, so opposite chains have equal
length. Use those two facts; geometry only breaks ties. Same family as the centroid-winding entry
below: a geometric proxy standing in for a topological fact, correct on the one example it was
written against.

**MEAN-PAIRWISE DISTANCE AS "WHICH CHAINS ARE CLOSEST" — DEAD (`unbevel.rs`, 2026-08-11).** Rails were
told from end caps by averaging distances between ALL pairs of vertices of two chains. **Tried →
failed because** for two parallel rails of run-length L a bevel-width w apart, most of those pairs are
far apart ALONG the strip, so the average returns O(L), not w — it only stays below the end caps' own
separation while the run is short, and inverts on any strip of a few columns (measured on the owner's
5-column strip: caps read as closer than the rails, so the sew ran lengthwise and tore it open).
**Lesson:** "how far apart are these two chains" means mean NEAREST-vertex distance. Mean-pairwise
answers "how big is this pair of chains", which is a different question.

**A KNIFE-RECT CORNER ROUND IS NOT A BEVEL — "cut sharp, then vertex-bevel the corners" is DEAD
(tried 2026-08-08, reverted same day, owner-reported regression).** The rectangular knife's
scroll-to-round-corners (`knife_shape_corner_bevel`, `poly_knife_shape_rebuild`) lays ARC points into
the cut polyline, so the round is a property of the cut PATH. Ctrl+Alt+B could not reverse it (there
is no bevel strip and no history entry), and the attempted fix rewrote the COMMIT instead of adding
history: `poly_knife_shape_release` / `poly_knife_slice_through_now` overwrote `knife_points` with the
sharp 4-corner ring, cut that, then ran `bevel_op::bevel_vertices_offset` on the corner verts and
pushed a `PolyBevelHistory`. **Four independent reasons it cannot work:**

1. **On the SLICE path the corner verts are COPLANAR.** A vertex bevel there inserts a coplanar
   facet — geometrically present, visually nothing. The round simply vanished.
2. **On the SUBTRACT path a vertex bevel only touches the rim it matched.** The hole's corner is a
   vertical prism EDGE spanning the depth; rounding one endpoint chamfers the front rim and leaves
   the hole square all the way through. (If a post-pass ever were wanted here, the edge bevel of the
   4 vertical corner edges — not `bevel_vertices_offset` — is the only relationship that is actually
   true. It is still false for the slice, so it does not unify the two paths.)
3. **Corner matching was nearest-vert against `radius * 4.0`, in DRAWING-PLANE space.** The cut
   corners live on the mesh surface, the intended corners on the drawing plane; when the press misses
   a face the plane sits at the body's bounds centre, so the gap is half the body depth and nothing
   matched. Five silent `return`s (no match / non-manifold / extra boundary loop / …) each fell back
   to a plain sharp cut with **no toast** — the tool looked like it had lost the feature.
4. **It forked one operation into two paths.** RMB/Enter (`poly_knife_free_confirm`) kept cutting the
   arc ring, release/S cut sharp+bevel. Owner: *"the bevel point cut works on the right click, on the
   release of drag should be same thing."* Measured on a 2-unit cube, ±0.5 rect, 6 segs, 50% round:
   release v=184/f=138 vs RMB v=64/f=36.

**Lesson:** when Unbevel cannot reverse an operation, add the history the reverse needs — do not
change what the operation PRODUCES so a reverser recognises it. The gate is
`rect_release_cuts_the_same_rounded_corners_as_rmb_confirm` (`poly_knife.rs`), which asserts release
and RMB land the same signature and that the round adds geometry over a sharp cut. **Still open:**
Ctrl+Alt+B on a knife-rect corner round — **now CLOSED, see §11 2026-08-08l: neither of the two
options was needed, the reverse math already did the right thing and was being vetoed by a
winding heuristic.**

**RE-DERIVING WINDING FROM THE MESH CENTROID — DEAD (removed from `unbevel.rs` 2026-08-08).**
The unbevel rebuild ended with "flip any face whose normal points away from the mesh centroid",
then handed the result to `from_polygons`. That is only valid on a STAR-SHAPED solid. On anything
concave it inverts, and a knife rect cut is the canonical counter-example: the body centroid lands
**inside the hole**, so every wall of the hole has its normal pointing back toward the centroid and
the entire hole got reversed. `is_manifold()` then reported *"directed edge a->b used by 2 faces
(same winding)"* and the gate turned that into a flat "Unbevel: reverse math refused" — on a
collapse whose vertex and face counts were already **exactly right** (48v/28f → 40v/24f: the ten
arc verts merged to two, the four arc quads gone). **Lesson: winding is INHERITED, never
re-derived.** Every ring in the rebuild is copied straight off the input mesh by `face_vertices`,
so it already carries that mesh's consistent orientation, and merging vertices cannot reverse a
loop. There was nothing to fix and the guess only destroyed correct data. Same family as §11
2026-07-18: the centroid-based inward sign-fix is WRONG on concave faces.

**INSET + EXTRUDE TAGGING — SHIPPED, AND THE TWO RULES ARE DELIBERATELY DIFFERENT (2026-08-05d).**
Owner: *"inset: tag all geo created by inset should be tagged not to bevel. extrude: we should use
angle based to tag or not, maybe all 20 degree under are tagged out of bevel."* Both write into the
same `CustomData::op_created` sidecar the boolean and the destructive bevel already use. Shared
rules now live in `app/src/poly/op_tags.rs` (the bevel's `bevel_carry_custom` MOVED there as
`carry_op_tags` — one carry policy for all three ops).

*Three dead ends this task ruled out, each already implied by an entry below:*

1. **A POSITION DIFF cannot see what an inset created.** The interactive inset builds its topology
   at thickness 0, where every new ring vertex is EXACTLY coincident with the original it was split
   from. `remap_by_position`'s `(src pos, dst pos)` key is then AMBIGUOUS — the new inner edge and
   the old boundary edge hash the same — so a diff either sees nothing or copies the boundary's tags
   onto the new ring. Same family as the "position diff tags 384 of 384" entry below. **The op must
   report what it created at emission:** `apply_inset` now returns the allocated vertex ids
   (`InsetApplied::new_verts`) and the created set is "every live edge incident to one of them".
2. **"ALL geo created by inset" taken as "every edge of every face the inset built" is WRONG, and
   it is the bevel end-arc mistake again.** The rim quads are new faces, but their OUTER boundary
   edge — where the band meets the untouched surface — is the SAME edge, at the SAME position, that
   existed before the inset, and on a cube it is a 90° crease the user obviously still wants
   beveled. Measured: `inset_leaves_the_regions_outer_boundary_beveled` shows all 4 of those edges
   at `cos ≈ 0` (90°) and 0 of 4 tagged. The vertex-incidence rule excludes them by construction —
   an edge with two ORIGINAL endpoints was not created. Reported to the owner as the one place the
   literal reading was not followed.
3. **A blanket tag on the extrude is wrong for the opposite reason** — an ordinary boss's walls meet
   the cap and the surrounding surface at ~90°, so tagging them would kill the most common bevel
   there is. Hence the owner's angle rule: only seams under **20°** of normal deviation
   (`EXTRUDE_TAG_TANGENT_DEG`, next to the modifier's 30° `SMART_SELECT_TANGENT_DEG` in
   `poly/mesh_sync.rs`). **That band deliberately includes exactly-coplanar edges even though the
   modifier's geometry rule exempts them** (a knife cut is coplanar and must keep beveling) —
   provenance is the ONLY thing that can separate a flush extrude seam from a knife cut. Do not
   "fix" the write side to match the read side.

*Two more things worth not re-deriving:* `extrude_region` never calls `delete_face`, so
"live now, not live before" is an EXACT created-face set (`op_tags::faces_created`) and works at
zero offset where a position diff cannot — but for the same reason the extrude must be stamped
**by id with NO remap**: remapping by position would drop the tags on the whole cap it just moved.
And the extrude stamp must run when the offset is FINAL (confirm), not at drag start: at zero offset
every wall is degenerate and `edge_dihedral_cos` has no meaningful angle to judge.

**NUMBERS (all from the pinned tests, cube 2³, bevel width 0.05 / 1 segment):**
inset (top face, thickness 0.4, depth 0) → 16 created half-edges = 8 undirected keys, 0 of the 4
outer boundary edges tagged, live faces **30 tagged vs 42 untagged**. Flush extrude (whole top face
+0.5Y, wall runs flush with the cube side) → 4 created faces, 16 half-edges, **4 tagged**, live
faces **34 vs 42**. Ordinary extrude (inset 0.5 first, then +0.6Y) → 4 created faces, 16 half-edges,
**0 tagged**, min deviation 90°. Pinned by `inset_created_edges_are_not_beveled_by_smart_select`,
`inset_leaves_the_regions_outer_boundary_beveled`, `flush_extrude_seams_are_tagged_and_not_beveled`,
`a_steep_extrude_tags_nothing` (`app/src/poly/mesh_sync.rs` tests) +
`op_tags::tests::near_tangent_{rejects_a_ninety_degree_crease,keeps_a_coplanar_seam}`.

**STILL NOT PERSISTED.** These tags are runtime-only, exactly like the boolean's and the bevel's —
see `SIDECAR_SURVIVES_BOOLEAN.md` "OPEN — the tags DO NOT PERSIST YET". Save/reload still loses
them, and that still needs an owner decision, not another attempt.

**SLIVER THROTTLE — RESOLVED, NOT A DEAD END (2026-08-05). The 0.00997-class edges named as
"the honest target" by the Smart Clamp entry below were real, and collapsing them at the
BOOLEAN raises the bevel ceiling. Measured, shipped, pinned.**

*What the slivers actually are — measured, not assumed.* Diagnostic
`cargo test -p core_math diag_boolean_sliver_sweep -- --ignored --nocapture` slides a 32-side
cylinder cutter across a cube in 0.02 steps. The result's shortest edge sweeps CONTINUOUSLY
from the full tessellation chord (0.098) down to 0.0026 and back, once per chord — because the
exact cut curve crosses the barrel at an arbitrary phase and routinely drops a new vertex a hair
from an existing one. **The raw kernel output and the final polished mesh had the SAME shortest
edge in all 24 samples.** So the answer to "(a) weld survivors / (b) created after the weld /
(c) in the kernel output itself" is **(c)**, and it is INTRINSIC — there is no weld epsilon
anywhere in the chain that could have prevented it, and raising one would have been the wrong
fix. Slivers must be collapsed after the fact.

*Why every earlier collapse attempt failed — a KERNEL bug, not a gating problem.* The note on
`collapse_epsilon_debris` ("cross-rim welds break closedness because `merge_vertices_to` cannot
re-twin the doubled edges") was diagnosing this exactly. Collapsing edge `a–b` of a triangle
`abc` degenerates that face to a 2-gon `a→c`,`c→a`; `collapse_self_loops` handed it to
`delete_face`, which sets EVERY neighbour's `twin = NO_TWIN` — so the two outer neighbours, which
are each other's true twin once the wedge is gone, became boundary edges and the gate correctly
rejected the torn result. **Fix: `HalfEdgeMesh::remove_degenerate_face`
(`half_edge/ops_dissolve.rs`) re-twins the survivors** (verified opposite-direction first) and
falls back to `delete_face` when they are not re-twinnable. Serialized shape untouched. With
that one step, collapses start passing the gates that were already there.

*The pass:* `collapse_sliver_edges` (`mesh_boolean_manifold.rs`), called at the end of
`manifold_boolean`. An edge is a sliver when `len < 0.35 × its shortest NEIGHBOURING edge` —
**scale-relative on purpose**: an absolute epsilon cannot tell "the cut nicked a corner" from
"the user modelled a small feature", but a 3:1 length step against its own neighbours can. On
`test_010` that keeps the cylinder chords (ratio ~1.0) and the honest 0.08019 support edge
(ratio 3.8 vs its 0.02094 neighbour) while catching 0.00997 and 0.02094. Operand vertices are
never moved (positions quantised at 1e-5, same mechanism as PRESERVE): one preserved endpoint →
the sliver snaps ONTO it, both preserved → refused outright. Each collapse is tried on a scratch
clone and kept only if the mesh stays valid + edge-manifold + closed + fold-free + volume-equal
(1e-3 rel) and nothing got shorter; the whole pass is then discarded unless the global minimum
strictly rose. Fail-closed throughout.

⚠ **Per-collapse acceptance must be NON-DECREASING, not strictly increasing.** First version
gated each collapse on the global min rising and accepted nothing: a symmetric cut leaves the
same sliver 4× (cylinder through a cube), so no single collapse can move the global minimum.
Gate the PASS on the strict increase, the individual collapse on "nothing got shorter".

**NUMBERS (owner `test_010`, cube + two cylinder booleans):** min edge `0.009969 → 0.090663`,
`bevel_op::last_bevel_effective_width()` for any request ≥0.05 `0.004846 → 0.016278` (3.4×),
volume `1.843165 → 1.843182` (9e-6 relative), result and beveled result both valid, edge-manifold
and fold-free. Pinned by `boolean_leaves_no_sliver_edges_so_the_bevel_can_open_up` +
`sliver_cleanup_never_moves_an_operand_vertex`; census diagnostic
`AZRA_DIAG_FL4=<path> cargo test -p app diag_sliver_census -- --ignored --nocapture`.

**⚠ OPEN, AND THE NEXT PERSON'S STARTING POINT: the minimum edge length is NO LONGER the binding
constraint, and the width is still not what the user wants.** After cleanup the shortest edge is
0.0907 (half = 0.0453) but the effective width lands at 0.0163 — so something else in
`bevel_edges_offset`'s clamp block (`bevel_op/mod.rs` ~:380-475, the `denom`/`lb` term and the
`slide_walk` accumulation, both ANGLE-driven) is now the ceiling. Do NOT chase shorter edges
further; instrument that block with `AZRA_BEVEL_CLAMP_DEBUG=1` and find which term binds.

**SUPPORT-EDGE PLACEMENT FIXED AT CREATION, NOT BY RELOCATION (2026-08-05c).** Owner
`test_014` (fresh cube−cylinder, v=57 f=27): support edge39 (len 0.7048) terminated on
VALENCE-3 vertex v48 — a T-pin splitting a rim chord mid-segment ("random vertex created on
the circle segment"), at an arbitrary steep angle. **Creation-site finding:** the bridges are
NOT an explicit step — they are whichever internal diagonals SURVIVE
`greedy_merge_region_impl` (`mesh_boolean/rebuild.rs`) when an annulus region hits its merge
fixed point, and survival was an accident of BTreeMap vertex-index dissolve order. A bridge
ending on a T-pin keeps that pin at degree 3 so `dissolve_collinear` can never remove it.
**Fix (no new geometry, no relocation):** steer the DISSOLVE ORDER — (1) diagonals holding a
collinear non-preserved endpoint alive dissolve FIRST (survivors then terminate on real
junction vertices and the freed pin is dissolved by the existing `dissolve_collinear`);
(2) then shorter diagonals first — MAXIMIN survivors, honouring the radial-reroute refutation
below (never shorten a bridge, never lower the mesh min edge length); (3) tie-break on the
(a,b) index pair — deterministic, no hash order. Positionless `greedy_merge_region` keeps the
old order. Measured on the interior-hole sweep (24 positions, 32-side cylinder through a
±1 cube): all support edges now land val≥4/val≥4, lengths rose (e.g. dx=0.24:
0.8001/0.8001/0.9419/1.1302 → 1.1786/1.1786/1.4313/1.4313), min edge stayed the full chord
0.09802 at every position. Pinned by `merge_coplanar_support_edges_avoid_midsegment_pin_vertices`
(mesh_boolean/tests.rs — FAILS on the old order) and
`support_edges_land_on_junction_vertices_not_midsegment_pins` (mesh_boolean_manifold.rs);
diagnostics `diag_support_edge_sweep` (core_math) + `AZRA_DIAG_FL4=<path> cargo test -p app
diag_support_edges -- --ignored --nocapture`. NOTE: the saved `test_014.fl4` mesh itself is
fixed data — the owner must re-execute the cut in a new build to see the fix; the sweep
covers the same class. `collapse_sliver_edges`' guarantees untouched (runs later, all its
pinned tests green).

**SMART CLAMP — "re-route boolean support edges RADIALLY to un-throttle the bevel width" —
BUILT, SHIPPED OFF-BY-DEFAULT, AND MEASURED TO A NO-OP ON THE OWNER'S OWN FILE (2026-08-05).**
The design (owner's, chosen over a destructive post-boolean pass): a Bevel-modifier toggle
that relocates badly-placed flat boolean support edges — the bridges from a hole rim out to
the face's outer boundary — inside the modifier's WORKING COPY, re-routing each radially from
the hole centre, snapping to an outer vertex within 5° when that is not worse than splitting.
The machinery is implemented and correct (`core_math/src/support_edge.rs`): region walk,
boundary-loop classification, radial ray/segment intersection, `dissolve_edge` +
`split_edge` + `split_face_between`, per-candidate scratch clone gated on
valid + manifold + fold-free + volume-preserving + min-edge-length-non-decreasing.

**TWO MEASUREMENTS KILL THE PREMISE — read these before anyone re-derives this idea.**

1. **The radial route is the NEAREST exit from a hole, so it is SHORTER than the bridge the
   boolean already placed — always.** Not an implementation bug: geometry. On the owner's
   `test_010` the four classifiable support edges would go
   `0.4332→0.1045` (50.5° off-radial), `0.4644→0.0617` (80.8°), `0.5680→0.3918` (15.8°),
   `0.3821→0.1946` (23.9°). Same result on 20+ real boolean fixtures (plain / off-centre /
   multi-cut / L-shaped concave cut faces, 8..32 cylinder segments): every single candidate
   shortens. Radial re-routing therefore TIGHTENS Clamp Overlap; it cannot loosen it. The
   improvement gate refuses all of them, which is why the shipped toggle is a no-op.
   The fifth edge — the `0.1792` one the whole task was aimed at — is not even a rim→outer
   bridge: its coplanar region has ONE boundary loop, so there is no hole for the rule to
   route away from.
2. **The `0.1792` support edge is NOT what throttles the bevel on `test_010`.** Measured on
   the loaded file: the mesh's shortest edge is **`0.00997`**, and **196 directed edges are
   shorter than `0.1792`**. `bevel_edges_offset`'s clamp is a SINGLE GLOBAL width
   (`mod.rs:471`, `w = w.min(limit)` where `limit` is the minimum over every beveled edge),
   and it measures **`0.00485` ≈ 0.00997/2** — set by that 0.00997 edge, not by any support
   edge. Relocating support edges cannot move that number.

**LESSON (the general one):** before designing around "edge X caps the width", print the
mesh's global minimum edge length and `bevel_op::last_bevel_effective_width()`. The clamp is
global, so the throttle is the single worst edge ANYWHERE, and a plausible-looking local
culprit can be an order of magnitude away from it. The diagnostic is
`cargo test -p app diag_smart_clamp -- --ignored --nocapture` with `AZRA_DIAG_FL4=<path>`.

**What was kept:** the toggle + kernel ship OFF by default with fail-closed gates and pinned
tests (`support_edge::tests`), including
`radial_reroute_is_shorter_than_the_existing_bridge_so_nothing_is_relocated`, which will
fail loudly if someone changes the rule so it starts moving things. If the real goal
(un-throttling the width) is picked up again, the honest target is the 0.00997-class edges —
a merge-by-distance / degenerate-edge cleanup — NOT support-edge placement, and the choice
rule would have to be maximin (pick the outer target that MAXIMIZES the bridge length),
which is a different rule from the owner's radial one and needs his ruling first.

**TAGGING A BEVEL'S OWN OUTPUT — BOTH EXISTING SIGNALS FAIL; `bevel_op` MUST RECORD IT
(2026-08-04).** Two attempts, both reverted, both tagging **384 of 384 edges**:
1. **Position diff** before/after (`edges_created_since`, the technique the boolean uses) —
   a bevel MOVES every original vertex onto its rails, so every edge looks new.
2. **`last_bevel_new_face_range()`** (`bevel_op/mod.rs:80`) — using it to index the INSTALLED
   mesh tagged everything. CORRECTION after reading the code: the range IS recorded
   (`mod.rs:1659-1662`), but it indexes `faces_out` — the local rebuild buffer — and
   `install_bevel_region` (`:1742`) then writes those faces into the mesh under DIFFERENT
   ids. So the range is meaningful, just not in the coordinate system I used it in. Either
   have `install_bevel_region` report the installed ids of the faces it wrote, or record the
   emitted half-edges directly (below). Do NOT simply index `result.faces` with it.

**DESIGN REDIRECT (2026-08-04) — the tags must live ON THE MESH, not be threaded through one
stack evaluation.** In the owner's screenshot the stack holds ONE Bevel modifier, and the
rounded corner is already in the BASE MESH (he beveled destructively in edit mode earlier).
Threading a tag set between modifier slots therefore cannot help him: there is no earlier
Bevel *modifier* to publish it.

Worse, in-stack threading is close to meaningless anyway: a Bevel modifier bevels ALL
manifold edges, so after it runs essentially every edge is bevel-created and a following
Bevel with Smart Select correctly has nothing left to do (which is exactly what "384 of 384"
was reporting).

**So Smart Select needs PERSISTED tags** — an edge tagged "created by a bevel" or "created by
a boolean" when the OPERATION runs, stored beside the mesh and surviving into later sessions.
That is the `PolyMesh` sidecar (`poly_mesh.rs:646` `CustomData`, keyed by `FaceId`/
`HalfEdgeId`) — the same store the boolean currently WIPES on install (`BOOL-STALECD`,
`app/src/poly/boolean.rs:549`). This unifies the whole feature: the sidecar remap that would
stop UV/seam/crease loss is the SAME work as persisting Smart Select's tags, and the boolean
half of the data already exists (`BoolProvenance`, 0753564).

Sequence it that way: (1) make the sidecar survive a boolean via provenance remap, (2) have
the destructive bevel tool and the boolean write edge tags into it, (3) Smart Select reads
them. Do NOT invest further in per-evaluation threading.

**MY TEST WAS WRONG, AND "384 OF 384" MAY HAVE BEEN CORRECT (2026-08-04 — read this before
touching the tagging again).** The double-bevel test beveled **every edge** of a cube first
(width .15, 3 segments), then asserted a second bevel still had sharp edges to work on. But
after beveling all 12 edges, essentially every remaining edge IS bevel-created — the strips
and caps are ~92 of the 98 faces. So "skipped 384 of 384" was very likely the tagging
telling the truth, and the assertion `sm > r` ("must still bevel the remaining sharp edges")
was asking for something that does not exist in that scene.

The owner's actual case is different and is the one to test: bevel **some** edges, leaving
others sharp, then a second bevel with Smart Select must skip the first bevel's segments and
bevel the sharp remainder. Build the fixture that way — e.g. select only the 4 top-face edges
for the first bevel — before concluding anything about the plumbing.

Also note for the coordinate-system question: the FULL-REBUILD install (`mod.rs:1735`)
uses `HalfEdgeMesh::from_polygons(&weld.positions, &faces_out)`, which preserves face order,
so `LAST_BEVEL_NEW_FACE_START..` DOES index the installed mesh correctly on that path. Only
the `use_local` path (face_count >= 8192, `mod.rs:1241`) renumbers and needs the
`install_face_rings_local` return value. That inverts my earlier note.

**The older note below — "the fix must cover both install paths" — is still true but was
written believing the numbers proved a bug. Re-derive from a correct fixture first.**
`install_face_rings_local` (`half_edge/euler.rs:420`) ALREADY RETURNS `Vec<FaceId>` in ring
order, and `rings` is 1:1 with `faces_out` — so the installed ids of the created faces are
`installed[new_face_start..]`. Capturing that into a `LAST_BEVEL_NEW_FACE_IDS` thread-local
compiles and is the right shape.

**But it reported `0 of 98` on cube + bevel(0.15, 3):** that capture was added only inside
`install_bevel_region` (the `use_local` branch, `mod.rs:1742`), and an all-edges bevel does
NOT take it — it goes through the full-rebuild install. Reverted rather than shipped
half-covering. **Add the same capture to the full-rebuild install path too**, then the
existing verification test (a MINORITY of faces / edges, every id live in the result) passes
or fails honestly. That test is worth re-adding: its minority bound is what caught both
coordinate-system errors immediately.

*(Superseded framing: "no usable existing signal" — there is one, it needed the right
coordinate system and both code paths.)* `bevel_op` must EXPLICITLY record the half-edges
it creates — the strip/cap/seal edges it already knows it is emitting — into a thread-local
or an out-param, alongside `LAST_BEVEL_EFFECTIVE_SEG`. Add it where the strip and cap faces
are appended, not by inferring afterwards; every after-the-fact inference tried has failed
for the same reason (the bevel rewrites the mesh wholesale).

Sanity check for whoever does it: on cube + bevel(width .15, segments 3), a correct tag set
is a MINORITY of the 384 edges — the earlier angle-based experiment excluded 336 and left a
watertight result, so the true segment-edge count is in that region, never all of them.

*(Superseded: the note below recommended `last_bevel_new_face_range`, before it was measured.)*
**Old note — use `last_bevel_new_face_range()`, NOT a position diff.** The owner's other case is a box already rounded by a bevel: a second Bevel
must skip that round's segment edges (his screenshot, blue). Tried first and REVERTED:
tagging by comparing endpoint positions before/after (`edges_created_since`, the technique
`BoolProvenance::edge_is_new` uses). It tagged **384 of 384 edges** — because a bevel MOVES
every original vertex onto its rails, so position-keyed comparison sees the entire mesh as
new. That technique is only valid for ops that leave untouched geometry in place, which is
why it works for the boolean and cannot work here.

The right signal already exists: `bevel_op::last_bevel_new_face_range() -> Option<(u32,u32)>`
(`bevel_op/mod.rs:80`) — the half-open FACE index range of geometry the last bevel created
(strips / caps / seals, explicitly "not re-traced originals"). Collect the half-edges of
those faces as the tag set. Note it is a thread-local recorded per rebuild, so read it
immediately after the `bevel_edges_offset` call that produced it.

Verified safe to exclude: an earlier experiment that skipped 336 of 384 segment edges on
this exact double-bevel produced a watertight, fold-free result — so unlike the boolean
seam case, this half of Smart Select is NOT blocked by the corner defect.

**OWNER CORRECTION (2026-08-04) — EXCLUDE FROM THE SELECTION. NEVER DISSOLVE.** Earlier
notes in this file proposed dissolving the flat boolean support edges. The owner rejected
that outright: *"when I say remove I don't want the dissolve for them, I want them removed
for the SELECTION that chooses which edge to bevel in the bevel modifier."* The mesh keeps
every edge; Smart Select only decides which edges the bevel TOUCHES. Ignore any
dissolve-based wording below — it is superseded.

**The tag set the owner wants** (his screenshot, edges highlighted blue): every edge created
by a BEVEL (its segment edges) and every edge created by a BOOLEAN with a cylinder (the
barrel seams), plus the flat support edges he marked red-X earlier. All excluded from the
bevel selection; none removed from the mesh.

**Consequence: the corner-fill defect below is now THE critical path.** Exclusion is the only
sanctioned handling, and exclusion is what produces the folds — so the fix is not optional
and cannot be routed around. Cylinder barrel seams already work this way today (shipped
66bfe0d: 32 edges excluded, watertight); it is the FLAT seams that hit the corner bug,
because they run out to a cube corner.

**⛔ SUPERSEDED AND REVERTED 2026-08-15 — this "fix" was a BANDAID and it shipped the owner's
broken round.** Skipping the cap did not remove a redundant face; it handed the corner to the
end-of-pipeline SEAL, which closes any residual boundary with ONE FLAT n-gon. At **seg=1** — the
only profile this was ever measured at — a flat n-gon over that rim is coincidentally the right
shape, which is exactly why "watertight either way" read as proof the cap was redundant. At
**seg>1** the same skip throws away the M_ADJ dome, and the user sees a flat facet with hard edges
cutting his rounded corner. The cap was DUPLICATING the seal because the RIM was broken (two rails
flanking the unbeveled edge slid onto it independently → two points, not one). Fixed at the rim;
guard deleted. Full account + the oracle table: §11 2026-08-15. **Do not reintroduce a cap skip
keyed on the unbeveled sector being flat/tangent** — Blender builds the identical dome either way.
*(Original entry kept below for the trail.)*

**FIXED (2026-08-04) — no cap when EVERY unbeveled fan sector is FLAT.** A cap closes the
gap between beveled edges at a corner. When the fan's unbeveled edges are real creases the
cap is required, and Blender agrees (5 oracle fixtures depend on it). But when an unbeveled
edge is FLAT — no plane change across it, which is exactly what a boolean leaves when its
seam crosses a face — that sector contributes no angular span, the re-traced faces already
meet at the persisting V, and the cap duplicates one of them with opposite winding: a
zero-thickness fold plus a non-manifold edge.

`bevel_skipping_an_annulus_seam_must_not_fold` now PASSES and is no longer `#[ignore]`d:
`v=126 f=121 manifold=true folds=0` (was `v=127 f=131 manifold=false folds=2`). All 5
partial-corner oracle tests still pass — the predicate keys on the unbeveled sector being
FLAT, which is what separates a boolean seam from a crease. core_math 841 green.

Why the blanket version was wrong, kept as the guard rail: skipping the cap for ANY
partially-beveled fan also fixed the repro but failed `k1_rounded_partial`,
`mixed_k1_deg4_rounded`, `mixed_k2_adjacent_deg4`, `mixed_k3_deg4`,
`span_corner_consecutive_unbeveled_rounds`. Those five define the behaviour any future
change here must preserve.

*(Investigation trail below.)*
**CONFIRMED BY EXPERIMENT + THE FIX IS NARROW (2026-08-04).** Skipping the cap for ANY
partially-beveled fan (`k_bev > 0 && k_bev < fan.len()`, at `mod.rs:1461`) makes the repro
CLEAN: `v=127 f=131 manifold=false folds=2` becomes `v=126 f=121 manifold=true folds=0`.
That is the numbers-changing confirmation the cap builder is the producer — and note the
result is watertight WITHOUT the cap, i.e. the cap there was a pure duplicate.

**But a blanket skip is WRONG: it fails 5 Blender-oracle tests** — `k1_rounded_partial`,
`mixed_k1_deg4_rounded`, `mixed_k2_adjacent_deg4`, `mixed_k3_deg4`,
`span_corner_consecutive_unbeveled_rounds`. Partial fans DO need caps in general; those five
define the correct behaviour and must keep passing. Reverted.

**So the fix is a narrower predicate**, not "partial fan". Those 5 oracle fixtures are the
discriminator: find what the repro's corner has that they do not. The repro's vertex is
`vert24 (2,-2,-2)`, fan 4 / 3 beveled, and its 4th edge is a BOOLEAN SEAM — collinear with
the surface, dihedral ~0 — whereas the oracle fixtures' unbeveled fan edges are real
creases. A flat/collinear unbeveled sector is the prime suspect: the cap rim walk chains its
persisting V twice because that sector contributes no angular span. Test that predicate
against all 6 cases before shipping.

*(Original measurement, still valid:)*
**PRODUCER NAMED (2026-08-04) — the fold is CORNER-FILL geometry, from STEP 5 (the cap
builder), not the strips and not `build_boundary_flat`.** Measured with the strip/cap split
(`last_bevel_strip_range` vs `last_bevel_new_face_range`) on the pinned repro:

```
strip range=Some((24, 84))   new range=Some((24, 131))
fold producer: FaceId(111)=CORNER-FILL / FaceId(112)=CORNER-FILL
```

Both folded faces sit in 84..131 — the CAP range emitted by STEP 5 (`mod.rs:1424`, the
rounded-corner cap / centroid fan at `:1548-1557`). They are the same triangle wound both
ways, at a cube corner whose fan has 4 edges with 3 selected. So the cap builder emits a
duplicated, oppositely-wound face when a vertex fan is PARTIALLY beveled.

**Start at `mod.rs:1424-1560`** — the cap loop walk and its convex-n-gon / centroid-fan
emission. Likely cause: the rim loop is walked assuming every fan sector contributes, so an
un-beveled sector's persisting V is added twice (once as the sector's own V, once as the
chained arc endpoint), closing a degenerate 3-gon. Confirm by making an edit there that
CHANGES THE NUMBERS before reasoning about correctness.

*(Superseded: `build_boundary_flat` was correctly eliminated — it is the boundary/rail
builder, not the cap.)*
**ELIMINATED (2026-08-04) — `build_boundary_flat` is NOT the producer.** TWO independent edits to it produced BYTE-IDENTICAL output
(`v=127 f=131 manifold=false folds=2` every time): (1) dropping the wrap span for partial
fans, (2) skipping any span whose two beveled edges are not adjacent in the fan. Neither
moved a single number, so this corner's geometry does not come from here. Both reverted.

**MEASURED FACT to build on:** the culprit vertex is `vert24 (2.000,-2.000,-2.000)`, and its
fan is **4 edges with 3 selected** — a valence-4 corner (the 4th edge is the boolean seam
arriving at the cube corner). So it is NOT the "fewer than 2 beveled" early-return either;
the corner is genuinely mixed-selection and something OTHER than `build_boundary_flat` emits
its geometry. Look at `caps.rs`, `rails.rs`, and the weld — and confirm the producer by
making an edit there that CHANGES THE NUMBERS before reasoning about correctness. The
instrumentation is in the repro test (`[SEAM] vert..`, `[SEAM] fold ..`).

*(Superseded: the fan-wrap reasoning below was a good story that measured to nothing.)*
**Old note — it is NOT the fan wrap in `build_boundary_flat`.** The obvious
candidate: `corners.rs:117` iterates consecutive beveled fan edges with
`bev_idx[(a + 1) % bev_idx.len()]`, so with exactly TWO beveled edges it emits the same span
twice in opposite directions — which matches the observed duplicated, oppositely-wound
sliver exactly. Guarding the wrap for partial fans (`spans = len - 1` when
`bev_idx.len() < n`) changed NOTHING: still `v=127 f=131 manifold=false folds=2`. Reverted.
So either the fold is emitted elsewhere (try `caps.rs` / `rails.rs` / the weld), or that
corner never reaches `build_boundary_flat` (it early-returns when `bev_idx.len() < 2` —
check whether the culprit vertex has fewer than 2 beveled fan edges and is therefore
skipped entirely, leaving a gap something else fills).

**Instrument the specific vertex.** The fold triangle is
`(1.980,-2.000,-1.976) (1.989,-1.988,-1.983) (2.000,-1.980,-1.980)` — find which original
vertex it came from, then log that vertex's fan: how many incident edges, how many selected,
and which code path emits its corner geometry. Three hypotheses have now died from reasoning
about this bug from the outside; measure inside the corner build before changing anything.

**LOCATED (2026-08-04) — the defect is CORNER FILL on a partially-selected fan, not the
rails.** Repro pinned as `mesh_boolean::tests::bevel_skipping_an_annulus_seam_must_not_fold`
(`#[ignore]`d, un-ignores into the fix). Skipping the 4 annulus seams gives v=127 f=131
manifold=FALSE folds=2 — and the fold is NOT near the hole:

```
fold FaceId(111)[(1.980,-2.000,-1.976) (1.989,-1.988,-1.983) (2.000,-1.980,-1.980)]
   / FaceId(112)[(2.000,-1.980,-1.980) (1.989,-1.988,-1.983) (1.980,-2.000,-1.976)]
manifold error: non-manifold: directed edge 3->21 used by 2 faces (same winding)
```

Both faces are the SAME triangle with opposite winding, sitting at a CUBE CORNER
(2,-2,-2) — the far end of a seam edge, which runs across the flat face from the hole out
to the corner (visible in the owner's screenshot). The hole itself is at radius 0.5 about
Y, nowhere near it. So the rails are fine; what breaks is the **corner fill where a vertex
fan has SOME edges beveled and some not** — it emits a duplicated, oppositely-wound sliver.

Start in `bevel_op/corners.rs` with that vertex's fan, not in `rails.rs`. Note §6's existing
corner entries: never bolt a 3D corner sphere on with `max()`, and partial-corner cases
already matched Blender for single-edge bevels — this is the mixed-selection corner, which
is new.

**ANSWERED (2026-08-04) — the red-X edges CANNOT be removed; the fix belongs in `bevel_op`.**
They are STRUCTURAL. The diagnostic data proves it: edges 16 and 18 BOTH have `face=4,
twin_face=5`, i.e. faces 4 and 5 share TWO edges. Together they are an ANNULUS — the flat
face pierced by the cylinder — split into two simple faces by two seams. Merging across
either seam leaves a face with a hole, which a half-edge mesh cannot represent, so
`dissolve_faces_region_gated` refuses and `dissolve_edges_region` returns false. It is not a
bug: it is the kernel correctly declining an unrepresentable merge. This is the same
constraint as a hole bridge, implemented as a 2-face split instead of a bridge edge — which
is why hypothesis 1 ("bridges") looked wrong on the same-face test yet was right in spirit.

**Therefore neither prior handling can work:** they cannot be dissolved (unrepresentable),
and excluding them from the bevel folds the mesh. **The real fix is in `bevel_op`**: it must
correctly bevel a selection that SKIPS an edge lying between two selected regions — here the
annulus seam, with beveled edges on both sides of it. That is a bevel robustness gap, not a
Smart Select tagging gap; the tagging side is finished and green (`new_flat_edges`, 6ce04ac).
Reproduce it directly: take cube-minus-cylinder, select all edges EXCEPT the 4 seams, call
`bevel_edges_offset`, and inspect the 2 folds it produces around the seam.

*(Superseded investigation notes — kept for the trail.)*
**OPEN — the red-X edges resist BOTH handlings (2026-08-04).**
The owner's boolean support edges are now IDENTIFIED exactly: `BoolProvenance::new_flat_edges`
returns 4 of them on cube-minus-cylinder (created by the cut AND flat, never an input edge) —
that part is landed and green (commit 6ce04ac). What is NOT solved is what to do with them:

- **Excluding them from the bevel selection FOLDS the mesh** — the §6 table below
  ("minus coplanar 120 edges -> manifold=FALSE, folds=2"). Proven twice.
- **Dissolving them FAILS SILENTLY** — `dissolve_edges_region` removed 0 of 4, both as a set
  and one-at-a-time with per-edge gates (valid + manifold + fold-free + volume-preserving).
  An unproven dissolve was written and REVERTED rather than shipped returning 0.

Two hypotheses were tested and BOTH are wrong, so do not re-try them:
1. *Hole bridges* — they are not: each separates two DIFFERENT faces (4/5 and 6/7), a bridge
   would have the same face on both sides.
2. *Two seams per pierced face, only one removable* — one-at-a-time still removed none.

**Start by finding out why `dissolve_edges_region` refuses these specific edges** (does it
return false, or does the gate reject?) — instrument inside it rather than inferring from the
outside, which is what burned both hypotheses above. Diagnostic already in place:
`mesh_boolean::tests` prints `[REDX] edge N: face=.. twin_face=.. same_face=..`.

**OWNER DIRECTION (2026-08-04) — SMART SELECT IS TAG-BASED, NOT ANGLE-BASED. NEVER retry
the angle limit.** An angle-limit implementation (Blender's bevel Limit Method: Angle, 30°
default) was built and worked on both test cases, and the owner REJECTED the approach on
sight: *"those tags should be set when I model, this is not angle based like Blender, it
needs to be smart."* It was reverted unmerged. The rule is:

> **An operation TAGS the edges it creates, at the moment it creates them.** The bevel
> modifier then skips tagged edges. No geometric inference, ever.

Why the owner is right, beyond preference: an angle limit cannot distinguish a coarse
tessellated cylinder from a low-poly cone the user deliberately wants beveled, and it
silently changes which edges are beveled when a *different* modifier alters curvature
upstream. A tag records intent; an angle re-guesses it every evaluation.

What must be tagged (owner's two cases, both from his own models):
1. **Bevel output** — every segment edge a bevel creates ("I need all segments created from
   a bevel to be skipped"). The bevel already knows these; it must record them.
2. **Boolean support edges** — the flat cross-face edges a boolean leaves behind (his
   screenshot marks two with a red X: they run across a flat face from a cylindrical hole to
   the corner). These are COPLANAR artifacts, and §6 already proves EXCLUDING a coplanar edge
   from a bevel produces folds/non-manifold — so the correct handling for this class is
   **DISSOLVE**, not exclude. Tag them at boolean time, dissolve them before the bevel.

The provenance layer (`mesh_boolean_with_provenance`, commit 0753564) is the boolean half of
this and is landed + green; it already distinguishes cutter faces from target faces. The
remaining work is (a) persisting tags through install, (b) making bevel_op record its own
output edges, (c) dissolving tagged coplanar boolean edges instead of excluding them.

**RESOLVED SAME DAY (2026-08-04) — it is RULE 3, not partial selection.** Two diagnostics
(kept as passing tests: `bevel_op::tests::diag_partial_edge_set_on_a_plain_cube`,
`mesh_boolean::tests::diag_partial_bevel_on_boolean_output`) isolate it:

| selection on cube-minus-cylinder | edges | result |
|---|---|---|
| ALL | 128 | valid, manifold, folds=0 |
| minus COPLANAR (rule 3) | 120 | **manifold=false, folds=2** |
| minus CUTTER-INTERIOR (rule 2) | 96 | valid, manifold, folds=0 |
| minus both | 88 | **manifold=false, folds=4** |

Partial selection is NOT the problem — a plain cube bevels correctly with one edge, a face
ring, or half its edges (including excluded edges neighbouring selected ones). **Rule 2 is
sound and shippable as-is.** Rule 3 is what breaks it: a coplanar edge on boolean output is
typically a SPLIT/TRIANGULATION ARTIFACT sitting inside a flat region. Beveling the edges
around it while leaving it in place drives the new rails into it, producing folds and
non-manifold output. **Lesson: a coplanar artifact edge must be DISSOLVED, not merely
excluded from the selection** — "no plane change" means the edge should not be there at all,
not that it should survive un-beveled. Re-implement rule 3 as a dissolve pass before the
bevel (`merge_coplanar` / `dissolve_coplanar_edges_iter` already exist), never as an
exclusion.

**Superseded open finding (kept for the trail) — PARTIAL EDGE SETS.** Bevel Smart Select
(owner feature: auto-exclude the tessellation of a subtracted cylinder + coplanar edges)
selected 88 of 128 edges on a cube-minus-cylinder and the result FAILED
`bevel_result_installable`, so the modifier fell back and the user saw no bevel at all. The
rule logic was correct — provenance excluded exactly the barrel seams, `edge_is_flat`
excluded the coplanar ones (`[BEVEL] smart select: 40 of 128 edges ignored`). What is
unproven is `bevel_op` on a PARTIAL edge selection over boolean output: every existing
modifier-path test bevels ALL manifold edges (`apply_bevel_matches_bevel_op_all_edges`).
**Investigate before building on it** — is the failure (a) partial selection generally, (b)
partial selection specifically where the omitted edges neighbour selected ones (open rails
at the boundary of the selected set), or (c) this mesh's post-boolean topology? Reproduce
with `bevel_op` directly on a hand-built partial set before touching the modifier again.
Provenance itself is landed and green (`mesh_boolean_with_provenance`, commit 0753564) and
is NOT implicated.

**Formulation / architecture**
1. **Endpoint-slide chamfer** (`half_edge::bevel_edges_ex`, frozen): slid the shared endpoint along the
   neighbour edge, welded by VertexId. → Can't round (`segments` unused), no corner fills, rails
   non-parallel, corners **tear** (weld by index not position). **Lesson:** constant in-plane
   perpendicular OFFSET (rails parallel by construction) + `WeldMap` keyed by quantized position. Never
   weld by VertexId. *(f939f38)*
2. **`bevel_edges_offset` left as a `clone()` stub** while the app still ran the old broken path. →
   Rendered but did nothing (delegated to the broken algorithm). **Lesson:** implement the replacement
   *before* pointing the caller at it — a slider that renders but no-ops is a bandaid. *(f939f38)*

**Watertightness / winding**
3. **Strip quads wound SAME direction as bridged faces.** → Orphan boundary edges, holes. **Lesson:**
   reverse the strip winding vs each bridged face. *(f939f38)*
4. **`compute_face_normal` via first-3-vertex cross product.** → Wrong SIGN on reflex/non-convex corners
   → (F,F) faces shaded INVERTED. Topology gates missed it (validate() checks twin symmetry not
   direction). **Lesson:** Newell's method; ADD an outward-normal gate. *(94d4da6, gate
   `bevel_output_faces_all_point_outward`)*

**Corner ordering (angle-sort is cursed)**
5. **`order_arc_about_vertex` sorted (F,F) cut points by angle around an EXTERNAL normal** (tried vertex
   normal, then face normal). → Handedness flips the order → endpoint face SELF-INTERSECTS ("edges slide
   along the wrong face"). Neither normal fixed it. **Lesson:** derive the sweep axis from the corner
   itself `(pe.src−V)×(ce.dst−V)`; add a `bevel_output_faces_are_simple` geometric gate. *(0cae0e9)*
6. **Re-sorting the (F,F) re-add-V tokens `[pe,V,ce]` with the angle-sort** on degree-4 corners. → Flipped
   `[pe,V]→[V,pe]` → PRONGED non-manifold boundary. **Lesson:** don't re-sort tokens already correct by
   construction; insert deterministically (pe-side→ce-side). Angle-sort is unsafe wherever
   handedness/planarity is ambiguous. *(bf6b1a7 → 17/17 green)*

**Profile shape**
7. **Quadratic Bézier arc** for the round profile. → Not Blender's circle; ~0.015–0.04 vertex residual vs
   oracle. **Lesson:** true circular arc via SLERP = Blender superellipse 0.5 → single edges match
   **0.0000**. *(2c8e013 → 036d55a)*
8. **Apex-SCALING the circle** to get sub-0.5 (flatter) profiles. → Mis-centred circle, not a flatten.
   **Lesson:** blend flat↔circular↔convex, don't scale one circle. *(6b0b8e8)*
9. **Default shape = 1.0 (convex).** → Blender's default is 0.5; out-of-box mismatch. **Lesson:** match
   the reference tool's DEFAULTS, not just its capabilities. *(63324aa)*

**Bevel-on-beveled corner (the long saga)**
10. **(F,F) insert the WHOLE arc into both adjacent strips.** → Right only for degree-3; at degree>3
    over-inserts → triangular hole → tear. **Lesson:** per-side rail insertion (degree-3 whole-arc,
    degree>3 just the bordering side). One rule, any degree. *(5111ac3)*
11. **Trust validate()/watertight gates + a seg=1-only test** for bevel-on-beveled. → Stayed green while
    the app tore on EVERY corner at the default seg=3 (17/17). A validate()-passing bevel of a CLOSED
    mesh can still OPEN holes (validate doesn't count boundary edges). **Lesson:** gate at seg=3; add a
    boundary-count install guard. *(21f294b, 6b0b8e8)*
12. **Corner cap = apex-FAN with the apex pushed OUT by the rim radius.** → A cone = visible SPIKE (grew
    the bbox); watertight so gates passed. **Lesson:** don't push the apex out; add a **bbox-growth spike
    gate** (width-0.08 bevel must not grow bbox >0.2). *(43d7c44)*
13. **Fan to the rim CENTROID keeping the original sharp V** (flat cap). → Didn't match Blender's
    structure = a FOLD. **Lesson:** DELETE V, place ONE recessed center C shared with the re-traced
    neighbours. Replaced both the keep-V fold and the apex-out spike. *(5608116)*
14. **Classify cap-needed by vertex DEGREE.** → The degree-3 same_edge seal only spans a *clean* convex
    corner, not a re-beveled one → caps the wrong corners. **Lesson:** classify cap_needed DIRECTLY
    (k≥2, or k==1 rounded with no same_edge seal detected). *(fcf11a9)*
15. **Fix the round tear with the general STEP-6 residual-hole seal.** → At degree>3 the patch produces
    BOTH holes AND inconsistent winding; hole-capping is symptom-mopping. **Lesson:** explicit local
    vertex-mesh corner (fan-walked rim + strip-matched winding), not a hole band-aid. *(fcf11a9)*

**Multi-edge miter (RESOLVED 2026-07-01 for the 2-edge case — see §11)**
16. ✅ **RESOLVED. STEP 3 (T,T) emitted ONE edge's rail at the shared corner + STEP 5 capped it.** → rails
    are NOT welded-equal at a degree>3 shared corner; emitting one rail (not the meet) is wrong and the cap
    OVERSHOOTS (boundary vert z=−1.041). Repro: pair (9,29) sharing v3=(1,−0.75,−1) opened flat_boundary=4.
    **Fix shipped:** a miter is NOT a cap — added Blender's `build_boundary` as **STEP 2b**
    (`build_boundary_flat`): V deleted → **2 BoundVerts + 1 shared edge** via a **3D rail meet**
    (`isect_lines_3d`; old `solve_corner_lines` only met COPLANAR rails, so degree>3 never met). Strips end
    at the miter edge, no cap (M_NONE). Locked by `multi_edge_pairs_watertight` (all pairs, flat+round).
17. ✅ **CONTEXT for #16. The k≥2 center-fan cap was the wrong tool** (regressed boundary 4→5). The lesson
    that held: a miter is not any kind of cap — it needs the `offset_meet` BoundVert stage (#16 fix), which
    a cap can never produce. Rounded multi-edge corners (Blender M_ADJ grid) are STILL flat-clamped — that
    grid, and the N-way (3+ edge) miter, are the next extensions. *(45b29e1)*

**Rounded 2-edge miter — the profile-arc derivation (RESOLVED 2026-07-02, see §11)**
23. ⚠ **Do NOT reverse-engineer the rounded-miter arc as a "circle through A,B sampled at even angle."**
    The arc IS a circle through the two flat BoundVerts, BUT: (a) its radius/arc-angle is GEOMETRY-DEPENDENT
    (std pyramid → 120° arc, tall pyramid → 161°; it is NOT a constant, NOT `180−angle(A,orig,B)`, NOT the
    orig-legs inscribed circle — those give ~5–24e-3 error); (b) Blender does NOT sample it at even angle —
    even-t/even-angle overshoots the interior verts by ~6e-3 (fails the 2e-3 gate). Chasing a closed-form
    circle+spacing wasted a long session. **What actually works (the ONLY faithful route): reimplement
    Blender's real construction verbatim** — `set_profile_params` (`middle = project_to_edge(bevel_edge,
    A, B)`), the unit-square affine map `co = x(mid−A)+y(mid−B)+(A+B−mid)`, THEN the key step
    `calculate_profile_segments` PROJECTS `co` ALONG the beveled-edge direction onto the weld plane
    (`plane_no = (orig−A)×(orig−B)` from `move_weld_profile_planes`). That projection — not any circle fit —
    is what turns the parabola-like map point into the exact fillet point. Verified err **0.0** on std AND
    tall pyramids. Superellipse profile 0.5 samples = even-angle circle `(sin,cos)` (exact for the 90° single
    edge; the corner non-uniformity is a CONSEQUENCE of the edge-dir projection, not a spacing table).
    **Lesson: for any Blender profile geometry, port the C construction (source is authoritative), don't
    fit curves to the output.** *(`miter_weld_arc`)*
24. ⚠ **Do NOT average the two edges' raw (w-offset, orig-vertex-apex) profile arcs** to get the weld chain —
    those live at the wrong height (y≈0.91 vs the loop-slid BoundVerts at 0.73); midpoint/rigid-map averaging
    collapses to the axis or lands ~5–7e-3 off. The weld uses ONE profile per BoundVert anchored to the flat
    BoundVerts A,B (not the rails) + the edge-dir projection; both edges yield the SAME chain after
    co-planarization, so compute ONE canonical chain and let WeldMap share it. *(§11 2026-07-02)*

**Boundary / open-face edge bevel (RESOLVED 2026-07-17 as a DEAD-END + a real guard fix — see §11)**
25. ⛔ **Do NOT invent a "one-sided chamfer" for a BOUNDARY (open-face) edge — Blender does NOT bevel
    boundary edges at all.** The task premise ("Blender bevels boundary edges by sliding the two endpoints
    inward along the single face and adding a chamfer strip on the open side") is **FALSIFIED by the
    oracle.** Exhaustive Blender matrix (`boundary_bevel_matrix.py`, Blender **4.5 LTS AND 5.0**, both agree):
    a boundary-edge edge-bevel is **IDENTITY (CHANGED=False, op FINISHED)** for a flat quad, a 3D open-box
    rim edge, the full rim LOOP, and a hex-fan outer edge — across **every** offset type (OFFSET/WIDTH/
    DEPTH/PERCENT), seg 1 and seg 3. The raw kernel call `bmesh.ops.bevel` on a lone boundary edge
    **CRASHES Blender** (segfault). The interior-edge control on the SAME open box correctly changes
    (nv 8→10), proving the harness is right. So there is **NO Blender ground truth to match** — a
    hand-authored boundary chamfer would violate §0 rule 1 (never guess geometry) and feed a one-sided edge
    into the closed-mesh watertight machinery = the exact §5/§6 tear class. **Lesson: AZRA's STEP-0 boundary
    skip is already Blender-faithful; keep it.** Selecting only boundary edges now fires an honest toast
    (`poly_activate_bevel`) so the no-op isn't silent. *(oracle `bnd_*` JSON, `boundary_bevel_matrix.py`)*
26. ✅ **THE REAL "bevel doesn't work on open faces" bug was the INSTALL GUARD, not the core.** AZRA's
    `bevel_edges_offset` ALREADY bevels an INTERIOR edge of an OPEN mesh **vertex-for-vertex to Blender**
    (open box, interior edge with one endpoint on the rim: seg1 nv=10/nf=6, seg3 nv=14/nf=8, both **0.000000**
    vs `openbox_int_seg{1,3}.json`). But such a bevel legitimately RESHAPES the rim, so its boundary EDGE
    count GROWS (4→5 flat, 4→7 seg3) — and the old app guard `manifold && boundary_edges <= orig` **rejected
    every open-mesh bevel that touched the boundary** → "nothing happens on open faces." **Fix:** the guard
    is now `bevel_result_installable` = `is_manifold()` (accepts clean OPEN meshes; catches non-manifold/
    bowtie/degenerate tears — stronger than the old `validate()`) **AND boundary LOOP count not increased**
    (a spurious hole = a NEW loop; a faithful open bevel keeps the loop count, only the edge count moves).
    Closed meshes are unchanged (loops 0 ⇔ 0 boundary edges). Never gate an open-mesh bevel on boundary
    EDGE count. *(`boundary_loop_count` + `bevel_result_installable` in `bevel_op.rs`, guard in
    `poly_bevel_regenerate`)*
**SDF per-edge bevel — the mixed-radii C1 combine (2026-07-17 session; the shipped fix is §11)**
25. ⚠ **Do NOT fix the SDF tri-planar hard-max with a SMOOTH max (`smax`), however you gate it.** Three
    gatings were derived to failure before the shipped construction: (a) plain `smax(a,b,k)` — bumps the
    value at surface creases where both plane fields = 0 → shaves a k/4 fillet into EVERY bevel-strip/face
    junction (global surface change, bake regression); (b) `k = λ·relu(min(a,b))` (distance-gated) — above
    a FACE both plane fields tie at the exact face distance with ZERO slack → overestimates by λ·d/4 →
    march overshoot (forbidden); (c) `k = λ·relu(min(qx,qy,qz))` (octant-gated) — near-parallel ties still
    occur above the corner column (qx≈qz small, qy large) where slack ~qx²/2qy ≪ bump λ·qx/4 → overestimate
    again. **Lesson: a bump ADDED to a max can only be safe where provable slack exists; slack vanishes on
    faces and at true creases, so no scalar gate works. Don't blend the max — max in an additional EXACT
    lower-bound field that dominates where the ridges are (the §11 corner-term fix).**
26. ⚠ **A per-octant corner term is UNSOUND unless every 2D plane field is the EXACT prism SDF.** The
    roundbox-ρ corner term with per-octant ρ = min(edge radii) leaks a discontinuity across octant
    boundary planes (ρ jumps with the edge selection) — it is hidden ONLY because the shared-plane 2D
    field dominates it there, and that domination bound (2D round-ρ ≤ 2D field for ρ ≤ r) requires the 2D
    field to be exact. The stock 2D CHAMFER form `max(a,b,(a+b+r)/√2)` is NOT exact (underestimates near
    its two vertices) → domination fails → Lipschitz spike 1.34 caught by the gradient property gate.
    Also rejected: GLOBAL ρ = min over all 12 edges (continuous, but a remote radius sphere-shaves corners
    whose own edges are None — sharp corners must stay sharp). **Lesson: exact-ify the 2D chamfer (add its
    two vertex sectors) FIRST; then per-octant ρ is provably continuous + 1-Lipschitz.**

**Other systems / app**
18. **SDF per-edge bevel via hard `max()` of three rounded fields + a corner sphere via max().** → C0 only;
    mixed radii → non-differentiable NOTCH + normal flip; corner-sphere → STRIPE artifacts. **Lesson:**
    uniform radius → exact `sdRoundBox`; mixed → radius-aware smooth combine, never hard max. *(CLAUDE.md
    Per-Edge Bevel Corner Rule; reverted 2026-03-22)* — **different subsystem, same "don't hard-max"
    lesson.**
19. **Nondeterministic HashSet/HashMap iteration** when building corners. → Unstable vertex/face IDs.
    **Lesson:** sort every emission loop. *(6b0b8e8)*
20. **Full-body bake every frame during the SDF bevel drag** (live feedback). → Generation-counter race,
    reverted. **Lesson:** respect the debounce/generation contract. *(tracker)*
21. **Guard the bevel-drag START with only `(!is_gizmo() && !is_create())`.** → After FacePushPull was
    added, crossing a bevel edge during a Push/Pull drag fired `start_operation(Bevel)` mid-op → assert
    crash. **Lesson:** guard with `!is_busy()` (class-level), keep the assert. *(EBEVELCRASH, 2026-06-24)*
22. **Sliders update the value, re-mesh on the next input event.** → WYSIWYG lie (mesh disagreed with the
    slider until an unrelated event). **Lesson:** regen on `.changed()` via one producer
    (`poly_bevel_regenerate`) shared by drag + scroll. *(6b0b8e8)*

---

## §7. Blender oracle — generate ground truth (never guess)

Blender's edge bevel is compiled C, not a script — so you can't read a console sequence. Instead drive
Blender **headless** to emit reference meshes and diff them.

- **Binary:** `C:\Program Files\Blender Foundation\Blender 4.5\blender.exe` (5.0 also present).
- **Command:** `blender.exe --background --python <script>.py -- <out.json>`. Add-on `bl_info`
  tracebacks in background mode are harmless noise.
- **Coordinate mapping:** make the Blender cube `size=2.0` → **±1**, matching AZRA `new_box(Vec3::ONE)`.
  (Blender's default cube is ±1 already at size=2; the *old* ±0.5 default needs ×2.) Then diff verts 1:1.
- **Blender settings to match:** Width Type = **Offset**; `segments`; Profile **0.5** (Superellipse =
  circle); Intersection = **Grid Fill**; Miter Outer/Inner = **Sharp**; Loop Slide on.
- **Scripts (scratchpad, re-createable):** `bevel_ref.py` (single/loop scenarios), `miter_ref.py` (two
  edges sharing a vertex — the miter). Each: build cube, select edges by geometry, `bmesh.ops.bevel(...)`
  or `bpy.ops.mesh.bevel(...)`, dump `{verts, faces}` to JSON. Compare with a small diff script.

**Reference data captured (the targets):**
- **Single-edge bevel:** our output vs Blender vertex diff = **0.0000** (exact) after the SLERP circular
  profile.
- **Partial corner** (one beveled edge into a degree>3 vertex): Blender DELETES V, adds ONE recessed
  center (e.g. ±1 cube, w=0.15: center ≈ `(0.913,−0.913,−0.837)`), fans the arc to it, shares C with the
  neighbour faces. We match this.
- **Flat SHARP miter (`miter_ref.json`):** cube corner (1,1,1), 2 top edges beveled w=0.2 seg1 → shared
  corner DELETED → **2 miter verts + 1 shared edge**: vert `(0.8,0.8,1)` = intersection of the two edges'
  offset lines on the SHARED face; vert `(1,1,0.8)` = where the two NON-shared-face rails coincide; both
  strips share edge (0.8,0.8,1)–(1,1,0.8). **This is the target for §9.**

---

## §8. Status — what works / what's broken / how it's guarded / gates

**WORKS (matches Blender / passes):** single-edge bevel (degree-3 corner); parallel rails; edge-loop
bevel (k=2 corners, n-gon patch); bevel-all-12-edges (k=3) watertight; rounded profile on **isolated**
edges (shape 0/0.5/1); **bevel-on-beveled ONE edge at a time** (partial k=1 corner — delete-V +
recessed-center, Blender-faithful, green at seg=3); **✅ 2-EDGE MITER at a re-beveled (degree>3) vertex** —
`build_boundary`/`offset_meet` (STEP 2b), V deleted → 2 BoundVerts + 1 miter edge, watertight (the fixed
multi-edge bug), **+ loop slide** (`loop_slide_meet`) pins the miter verts onto the intervening chamfer
edge instead of floating (matches Blender within 0.002); all faces simple; all normals outward; zero-width /
empty / boundary-edge identity; WeldMap identity; high-poly stress; **✅ ROUNDED 2-EDGE MITER (seg>1) at a
degree>3 vertex** — the M_NONE weld arc (`miter_weld_arc`, STEP 2b/2.5) bridges the two BoundVerts with the
co-planarized circular profile; watertight AND matches Blender vertex-for-vertex (`rounded_2edge_ref.json`).

**✅ MIXED CORNER WITH A LONE UNBEVELED EDGE — DONE 2026-08-15.** `3 ≤ n_bev < degree` with an
isolated unbeveled fan edge (the shape a boolean support seam makes when it lands on a box corner,
and what Smart Select's exclusion leaves behind) now takes its BoundVerts from `build_boundary_flat`
like every other corner: ONE BoundVert per consecutive beveled pair, placed by the rail meet
**projected onto the in-between edge** (`project_meet_onto_edge`, §3.2's last line). Both bracketing
rails weld to it, so the rim is a clean chained arc ring and the M_ADJ dome fills it — vertex-exact
with Blender on an ASYMMETRIC fixture (§11 2026-08-15; a cube is a trap here, every placement rule
agrees on it). Gates `flat_unbeveled_edge_must_not_break_the_rounded_corner`,
`boolean_seam_corner_rounds_like_a_clean_corner`.

**REMAINING GAPS (guarded):**
- **✅ N-way miter (FLAT, seg=1) DONE 2026-07-02** — 3+ beveled edges at a degree>3 vertex now builds the
  BoundVert ring (STEP 2b un-gated to `n_bev>=2`) + an **M_POLY n-gon cap** (STEP 5b). Matches Blender
  vertex-for-vertex on the pyramid oracle.
- **✅ k=1 PARTIAL CORNER AT A degree>3 FAR-ENDPOINT DONE 2026-07-02 (PM2)** — the octahedron-base hole. A
  degree>3 vertex with exactly ONE beveled edge now DELETES V, places a recessed center slid off V by the
  offset along the unique "back" fan edge (`partial_back_edge_slide`, Blender `offset_on_edge_between`), and
  fans the flat chamfer to it. Un-gated the partial cap from `n_seg>=2` to also fire flat at degree>3. Octa
  bevel-4-apex is watertight + matches `nway_octa_ref.json` vertex-for-vertex (nv=17/nf=17). §11 (PM2).
- **✅ ROUNDED 2-EDGE MITER (seg>1) DONE 2026-07-02** — the M_NONE weld (`miter_weld_arc`): STEP 2.5's flat
  clamp is lifted for exactly-2-BoundVert miter vertices; the two edges' strips share the co-planarized
  profile arc (`ns−1` shared interior verts, zero corner grid). Vertex-for-vertex Blender match. **Guard:** a
  selection mixing a rounded 2-edge miter with a BLOCKING shared corner (edge-loop k=2 / N-way) still flat-
  clamps globally — the STEP-2b chain is then dropped (`n_seg != req_seg → miter_arc_pos.clear()`) so strip
  and arc stay length-consistent (watertight flat fallback).
- **Rounded N-way / shared k≥2 corner (seg>1)** — a corner with **3+** beveled edges (M_POLY N-way cap) or a
  degree-3 edge-loop **k=2** corner still flat-clamps. Blender's `M_ADJ` grid (studied §3.7) is the next
  piece — SEPARATE from the 2-edge weld (which is M_NONE, not a grid).
- **❌ MITER MODES ARE NOT IMPLEMENTED (Miter Inner / Miter Outer) — owner-reported 2026-08-11.** AZRA has
  no miter parameter at all; our corners are permanently Blender's **Sharp/Sharp**, and §7's references were
  all generated at Sharp, so no gate has ever covered Arc. **We are vertex-EXACT with Blender at Sharp
  (0.0000000 max deviation, oracle-diffed on the owner's params/geometry — §11 2026-08-11), so this is a
  MISSING FEATURE, never a regression.** Inner=Arc costs **+8 verts / +7 faces per inner corner**
  (28v/26f → 60v/54f on the 4-corner boss scenario). **Miter OUTER governs CONVEX corners and is a no-op on
  the concave corners this was reported against** — do not wire Outer and call it done. The Inner arc is a
  **VMesh grid in the §3.7 `M_ADJ` family** (corner geometry extracted in §3.8), and it needs a **`spread`
  parameter** alongside the mode. Not a `miter_weld_arc` variation — see the correction in §11.

**BOUNDARY / OPEN-FACE edges (2026-07-17):** a BOUNDARY edge (one incident face, other side open) is
**NOT beveled — and that MATCHES Blender**, which is itself a no-op on boundary edges (§6 #25, oracle-proven
on 4.5 LTS + 5.0; raw kernel crashes). The STEP-0 skip stays; a pure-boundary selection now toasts. The
owner's real "bevel doesn't work on open faces" report was the **install guard** rejecting valid INTERIOR-edge
bevels of OPEN meshes (§6 #26): AZRA already matches Blender vertex-for-vertex there (`interior_edge_on_open_mesh_matches_blender`),
the guard just wrongly counted boundary EDGES. Fixed (below).

**GUARD (two layers, correctness not bandaid):**
1. `poly_bevel_regenerate`: install the new mesh ONLY IF `core_math::bevel_op::bevel_result_installable(orig, new)`
   = `new.is_manifold().is_ok() && boundary_loop_count(new) <= boundary_loop_count(orig)`; else log
   `[BEVEL] REJECTED (kept previous frame)`. **The metric is boundary LOOP count, not boundary EDGE count**
   (adapted for OPEN meshes 2026-07-17, §6 #26): a faithful bevel of an interior edge on an open rim grows
   the edge count (Blender's does too) but never adds a hole (loop count stable); a TEAR opens a spurious
   hole (loop count grows) → rejected. `is_manifold()` accepts clean OPEN meshes and is stricter than the
   old `validate()` (catches non-manifold/bowtie tears). Closed meshes unchanged (loops 0 ⇔ 0 boundary
   edges — no regression). A closed mesh that opens holes, or any mesh that tears, never reaches the renderer.
2. `poly_activate_bevel`: on arm, if any selected-edge vertex has `incident≥3 && vertex_fan().len()>3`
   (an N-way miter), fire ONE toast so the (rare) rejection isn't a silent no-op. 2-edge miters no longer
   warn — they work. **If EVERY selected edge is a boundary edge**, fire ONE toast ("Boundary (open-face)
   edges can't be beveled — Blender skips them too") so the Blender-faithful no-op isn't a silent mystery.

**GATES (`cargo test -p core_math bevel`, all green):**
**`octahedron_nway_matches_oracle`** (octahedron, bevel 4 apex edges → watertight + nv=17/nf=17 matching
Blender `nway_octa_ref.json` vertex-for-vertex + 4 recessed partial-corner centers — the k=1-partial-at-degree>3 lock),
`weld_collapses_coincident_and_separates_distinct`, `zero_width_is_identity`, `cube_single_edge_bevel_golden`,
`parallel_rails`, `boundary_edge_skipped` (now confirmed Blender-faithful identity, §6 #25),
**`boundary_edge_bevel_matches_blender_identity`** (the 3 task cases — flat quad seg1/seg3 + 2×2 grid —
all Blender IDENTITY), **`interior_edge_on_open_mesh_matches_blender`** (open box interior edge seg1 nv=10/nf=6
+ seg3 nv=14/nf=8, vertex-for-vertex vs Blender + manifold + loop-count preserved — the OPEN-FACE FIX lock),
**`bevel_install_guard_open_and_closed`** (`bevel_result_installable` accepts closed + open-mesh bevels,
rejects synthetic tears on both), **`boundary_loop_count_counts_holes`** (0/1/2 loops),
`edge_loop_corner_patch_watertight`, `bevel_all_edges_manifold`,
`bevel_segments_produce_round_watertight`, `bevel_on_beveled_watertight` (seg1),
**`bevel_on_beveled_round_app_faithful`** (seg3 — the 17/17-tear gate), `bevel_output_faces_are_simple`,
`bevel_output_faces_all_point_outward`, **`build_boundary_cube_corner_miter`** (offset_meet vs oracle
(0.8,0.8,1)+(1,1,0.8)), **`multi_edge_pairs_watertight`** (all interior edge-pairs, flat+round, zero
boundary — the miter regression lock), **`miter_loop_slide_matches_oracle`** (two-stage bevel miter verts
land on the chamfer edge at (0.891,-0.75,-1)/(1,-0.75,-0.891), no overshoot), **`nway_pyramid_matches_oracle`**
(square pyramid, bevel 4 apex edges → watertight + nv=12/nf=10 matching Blender + M_POLY quad cap — the flat
N-way lock), **`rounded_2edge_miter_matches_oracle`** (square pyramid, bevel 2 OPPOSITE apex edges seg3 →
watertight + nv=14/nf=11 matching Blender `rounded_2edge_ref.json` + 2 shared interior arc verts = the
ROUNDED-MITER lock), **`rounded_miter_mixed_with_blocking_corner_watertight`** (rounded miter + blocking
corner → watertight flat fallback = the chain-drop guard lock), plus legacy `half_edge` tests and `mesh_ops`
wrapper/proptest.
> ⚠ **Pre-existing RED (NOT this task):** `mesh_ops::test_spine::bevel_apply_inverse_apply_is_identity`
> fails on a persisted regression seed (`KNOWN_bevel_inverse_regression_seeds.txt`) — the bevel *inverse/
> undo delta* isn't faithful for some random selections. Verified failing IDENTICALLY on the clean base
> (`f998f2d`), independent of the rounded-2-edge weld (which only adds `(positions, faces)`). Its own task.
> **UPDATE 2026-07-17:** this test is GREEN on the current base (`e84029f`) — the seed regression was fixed
> upstream since. Left the note for history; no longer a standing RED.

---

## §9. NEXT STEP — the rounded N-way grid (2-edge weld + flat N-way DONE)

The 2-edge miter (flat AND rounded) and the flat N-way are DONE (STEP 2b + STEP 2.5 weld + STEP 5b, §11).
Remaining:

1. **✅ DONE 2026-07-02 — N-way miter (flat).** Un-gated STEP 2b to `n_bev>=2`; added STEP 5b M_POLY n-gon
   cap over `miter_cap_rings` for `bvs.len()>2`, oriented by Newell-vs-vertex-normal. Matches Blender
   vertex-for-vertex (`nway_pyramid_matches_oracle`). METHODOLOGY: use a single-stage mesh with a natural
   degree>3 vertex (pyramid apex) for a byte-identical oracle — NOT a two-stage bevel (stage-1 diverges).
2. **✅ DONE 2026-07-02 (PM2) — k=1 partial corner at a degree>3 FAR-endpoint** (the octahedron-base hole).
   The flat partial cap was gated on `n_seg>=2`; un-gated it to also fire at degree>3 (no `same_edge` seal),
   and added `partial_back_edge_slide` (the merged center slides off V by the offset along the unique back fan
   edge — Blender `offset_on_edge_between`). Octahedron bevel-4-apex → watertight, matches `nway_octa_ref.json`
   vertex-for-vertex. Gate `octahedron_nway_matches_oracle`. §11 (PM2).
3. **✅ DONE 2026-07-02 — ROUNDED 2-edge miter (M_NONE weld).** `miter_weld_arc` reimplements Blender's
   `set_profile_params` + `move_weld_profile_planes` + `calculate_profile_segments` exactly (project_to_edge
   midco, unit-square map, edge-dir projection onto the weld plane). STEP 2.5 lifts the flat clamp for
   exactly-2-BoundVert miters; both edges' strips share the welded chain. Vertex-for-vertex Blender match
   (`rounded_2edge_miter_matches_oracle`). Same single-stage methodology (2 OPPOSITE pyramid apex edges).
4. **Rounded N-way / edge-loop k=2 (Blender `M_ADJ` grid, seg>1).** Replace the remaining shared-corner
   `n_seg→1` flat clamp (3+ beveled edges, or a degree-3 k=2 corner) with the adjacency grid (studied §3.7):
   seed a seg-2 cage (BoundVert corners + arc midpoints + one fullness center), Catmull-Clark subdivide
   (`sabin_gamma(count)`), resample. The bigger piece; keep it separate. NOTE: the M_ADJ grid is ONLY for
   `count≥3`; the 2-edge case is the M_NONE weld above (already done).

Keep the §8 guard as the safety net throughout.

---

## §10. Regression protocol (run before declaring a bevel task done)

1. `cargo test -p core_math bevel` — all green (25+ tests).
2. `cargo test -p core_math` — full suite green (nothing else regressed).
3. `cargo build -p app` (dev) **and** `--release`. If `app.exe` is locked, kill the running instance
   first (`taskkill /F /IM app.exe`), then rebuild — a stale binary is the #1 "nothing changed" cause.
4. Generate the Blender reference for the scenario you touched and diff (§7). Topology green ≠ correct.
5. Append to §6 (any dead-end) and §11 (session log). Update `COMPLETE_BUG_TRACKER.md` + the memory
   pointer `project_bevel_corner_reference`.

---

## §9.5 ✅ DONE 2026-08-02 — partial-corner cap: per-edge slide points, not a recessed centre

**SHIPPED.** `partial_slides` (bevel_op.rs) replaces the single recessed centre at a **degree ≥ 5**
partial corner: every non-beveled incident edge gets a point slid off V by the offset, the cap is ONE
n-gon over `[arc…, slides]`, and the re-traced neighbour faces meet the corner through **their own**
two edges' points instead of all being pulled to one vertex. Owner's mesh now matches Blender
**26v / 19f, watertight, and vertex-exact on all 6 hexagon points** (gate asserts each within 2e-3 plus
"exactly one hexagonal cap").

**SCOPE = degree ≥ 5, deliberately.** Two failed integrations proved the boundary (don't redo them):
1. *All degrees > 3 through the general path* → the owner case went exact BUT broke 7 oracle gates
   (`octahedron_nway`, `k1_rounded_partial`, `mixed_k1/k2/k3_deg4`, `span_corner`,
   `bevel_on_beveled_round`). At degree-4 exactly one edge remains and `partial_back_edge_slide`
   already places it oracle-exact; the general path perturbs those verified corners.
2. *Excluding the two FLANKING edges from the slide set* (the "degree−3 remaining" reading) → broke
   the owner gate as well. The flanking slide points must be EMITTED and left to `WeldMap` to collapse
   against the arc endpoints; measured net +2 verts at degree-5, which is what makes the hexagon.

Suite after: **803 passed / 0 failed / 8 ignored.**

*(Historical spec + oracle data below — kept because it is the target any future change must still hit.)*

### §9.5a The measurement that specified it

**The owner's real defect** (captured live with `AZRA_BEVEL_DUMP=1`, oracle `repro_ref.py` +
`repro_in.json`/`repro_out.json`). Do NOT re-derive the shape — the input and the target are committed.

*Input:* unit cube extruded +Z then +Y (the step). Beveled edge `v7(-0.5,0.5,0.5) → v6(0.5,0.5,0.5)`,
w=0.15 seg=3. **Both endpoints are degree-5** (the L's side is three COPLANAR quads, so V has 5 edges /
5 faces) — every earlier test shape had degree-3 endpoints, which is why they all passed.

| | verts | faces | open |
|---|---|---|---|
| Blender 4.5 | **26** | **19** | 0 |
| AZRA today | 24 | 23 | 0 |

*Blender's 5 new faces:* 3 strip quads + **2 HEXAGON end caps** (one per endpoint, in the x=±0.5 plane).
The hexagon at v6 is, in fan order:
```
(0.5,0.5,0.35) (0.5,0.52,0.425) (0.5,0.575,0.48) (0.5,0.65,0.5)   <- the 4 profile points (seg=3)
(0.5,0.5,0.65)                                                    <- slide 0.15 along edge v6→v11 (+Z)
(0.5,0.35,0.5)                                                    <- slide 0.15 along edge v6→v2  (−Y)
```
**THE RULE (this is the whole fix):** at a partial corner (exactly ONE beveled edge at V), Blender
DELETES V and places a point at the offset along **every OTHER incident edge**, then caps with ONE
n-gon = `[arc points…, those slide points in fan order]`. The arc's own two endpoints already land on
the two edges bounding the beveled edge's faces (`v6→v5`, `v6→v12` here), so a degree-d vertex
contributes `(seg+1) + (d−3)` cap verts. Vertex accounting checks out: 16 − 2 deleted + 8 arc + 4 slide
= 26.

**What AZRA does instead:** one recessed centre (`partial_back_edge_slide`, §8/PM2) fanned to the arc —
1 vert where Blender uses 3, hence 24v and 4 extra fan faces (the owner's "mess"). PM2 was validated on
the octahedron, where d=4 and the difference is invisible; d=5 exposes it.

Gate: `owner_step_bevel_vs_blender_oracle` — now asserts `== 26/19`, watertight, manifold, all six
hexagon vertices within 2e-3, and exactly one hexagonal cap.

---

**OWNER ACCEPTANCE SET for support-edge placement — `test_013.fl4` (2026-08-05).** Seven
bodies: `original` (a raw cylinder-boolean topology) plus six hand-corrected duplicates at
different cylinder positions, showing the support segments the owner builds by hand.
Measured (`diag_support_edges`): in EVERY corrected body, both endpoints of every support
edge are **valence-4 junction vertices** — never a mid-segment landing — while lengths vary
freely (0.17–0.81). So the owner's specification is *junction-to-junction landing above all;
length is secondary*. This matches the creation-order rule shipped 2026-08-05c (junction
landings first, then maximin length). Use this file as the acceptance oracle for any future
change to support-edge placement.

## §11. Session log (append newest at top — the accumulator)

- **2026-10-13 — ids only, no geometry change (POLY-STALE-IDS).** The LEGACY chamfer
  `HalfEdgeMesh::bevel_edges_ex` (no production caller) now rebuilds through
  `from_polygons_keeping_ids`, so untouched vertices/edges/faces keep their ids. No new dead-end.
  The PRODUCTION bevel (`bevel_op`, `from_polygons(&weld.positions, ..)`) still renumbers — tracked
  OPEN in `COMPLETE_BUG_TRACKER.md` "POLY-STALE-IDS: rebuilds outside the listed set".

- **2026-08-15 — THE FLAT BOOLEAN SEAM BROKE THE ROUNDED CORNER, and the 2026-08-04 "skip the cap"
  fix was a bandaid over a BROKEN RIM. Owner: "bevel bug, the edge from the boolean create a 4 eges
  that break rounded cap."**
  🔑 **ROOT CAUSE, one sentence:** at a corner where a lone unbeveled edge sits between two beveled
  ones, AZRA slid each of the two flanking rails onto that edge INDEPENDENTLY (`rail_endpoint_pos`'s
  terminal slide, `w/sin`), which lands on **two different points** unless the corner happens to be
  symmetric — so the corner rim was a 10-point ring instead of the 9-point chained arc ring the
  M_ADJ grid requires, the grid declined it, and the corner degraded.
  **Blender's rule, MEASURED not guessed** (`docs/planning/poly/flat_edge_corner_ref.py`, new): the
  BoundVert is the two edges' **rail meet PROJECTED onto the in-between edge** — exactly what §3.2's
  pseudo-code always said on its last line (`if edges_between: P = project_onto_edge(P, edge_between)`),
  and never what we implemented. Discriminated on a box stretched to sx ∈ {0.5,1,2,3} with a flat
  diagonal across its top face:

  | sx | Blender | project (shipped) | `loop_slide_meet2` 0.5·(w/s1+w/s2) |
  |---|---|---|---|
  | 0.5 | (0.44,−0.88,1) | **(0.4400,−0.8800)** ✅ | (0.4250,−0.8500) ✗ |
  | 1 | (0.9,−0.9,1) | **(0.9000,−0.9000)** ✅ | (0.9000,−0.9000) ✅ |
  | 2 | (1.88,−0.94,1) | **(1.8800,−0.9400)** ✅ | (1.8500,−0.9250) ✗ |
  | 3 | (2.88,−0.96,1) | **(2.8800,−0.9600)** ✅ | (2.8000,−0.9333) ✗ |

  ⚠ **sx=1 is a TRAP FIXTURE — every rule coincides there.** On a cube the offset_meet lands exactly
  ON the diagonal, so a symmetric fixture cannot tell the projection, the averaged slide, and two
  independent slides apart. Any future corner-placement work must use an ASYMMETRIC fixture.
  **Why nobody saw it:** the missing rim was closed by the end-of-pipeline **closed-input SEAL**
  (`mod.rs`, "residual boundary → one n-gon"), which plastered ONE FLAT n-gon over a rim Blender fills
  with the 7-face dome. Watertight, manifold, `bevel_result_installable` = true, zero folds, no log
  line anywhere — a textbook §0-rule-6 *watertight-but-wrong*. Measured on the minimal fixture:
  flat-seam corner `(9 verts, 1 face, [9])` vs the clean control `(12, 7, [3,4,4,4,4,4,4])` in the
  SAME mesh. The seal now `eprintln!`s `[BEVEL] SEAL:` at seg>1 so the next instance is visible.
  **The 2026-08-04 entry in §6 ("no cap when every unbeveled fan sector is FLAT") is SUPERSEDED and
  its guard is DELETED.** It was measured at **seg=1 only**, where a flat n-gon over the rim happens
  to be the right shape, so "watertight either way, the cap was redundant" read as a clean result. At
  seg>1 the same skip throws away the dome. The cap was never redundant — it was DUPLICATING the seal
  because the rim was broken.
  **Fix (3 parts, all in `bevel_op`):** (1) new `project_meet_onto_edge` (`offsets.rs`) and
  `build_boundary_flat`'s `between == 2` branch now uses it, `loop_slide_meet2` demoted to the
  degenerate fallback; (2) STEP 2b routes **every** mixed corner (`3 ≤ n_bev < degree`) through
  `build_boundary_flat`, no longer only those with a `(false,false)` span and no longer only at
  `req_seg > 1` — both bracketing rails then weld to ONE BoundVert; (3) the flat-sector cap skip is
  gone. **Do NOT reintroduce it:** an unbeveled edge being flat says nothing about whether the corner
  needs filling — Blender builds the identical dome with or without it.
  **Results.** Minimal fixture: vertex-exact with Blender at sx=1 AND sx=2 (nv=96/nf=99, corner
  12v/7f identical to the control). Owner's real class (cube − cylinder, Smart Select skipping the 4
  flat seams): every corner — seam or not — now the same 12v/7f dome at seg=3 and 3v/1f at seg=1.
  **A second, louder failure also fell out:** at w=0.1 that mesh was `manifold=false, boundary_loops=1`
  at BOTH seg=1 and seg=3 → the install guard rejected it → the user saw **no bevel at all**. Now
  watertight at every width tested.
  Gates: **`flat_unbeveled_edge_must_not_break_the_rounded_corner`** (bevel_op — the oracle lock, both
  sx, asserts the flat corner equals the clean control), **`boolean_seam_corner_rounds_like_a_clean_corner`**
  (mesh_boolean — the owner's real geometry, 4 width/segment combos, incl. `bevel_result_installable`).
  **151 bevel gates green, core_math lib 1183 passed.** `cargo build -p app` dev + release clean.
  (Pre-existing REDs in the tree, verified by reverting `mod.rs` and re-running — NOT this change:
  `edge_select::directed_loop_crosses_pole_straight`, `mesh_ops::{apply_in_place_refusal_leaves_mesh_untouched,
  clean_mesh_still_refuses_slit_creating_dissolve}`, `serde_golden::golden_scene_graph_deserializes_exactly`,
  and the flaky timing bench `bench_display_mesh_ops`. Also fixed one committed-broken call so the
  suite could build at all: `tests/measure_bevel.rs` still called `bevel_edges_offset_mut` with 3 args
  after it gained the `BevelWidthPolicy` param.)

- **2026-08-12 — INNER MITER ARC: the wall attachment is MEASURED and §3.8's open question is closed;
  the feature is still NOT shipped, and here is exactly where it stands.** Owner asked where the
  miter/arc settings are in the Bevel tool. Answer: nowhere — `grep MiterInner|BevelMiter` over the
  whole workspace returns ONLY their own declarations (`bevel_op/mod.rs:235-261`), and
  `inner_miter_arc` (`corners.rs:113`) has exactly one caller, its own tests. The arc math + 6 oracle
  gates are a verified ISLAND: `BevelParams` never carried a miter, `bevel_edges_offset` never took
  one, no UI ever exposed one. The `feat/bevel-arc-miter` worktree is DOCS ONLY (2 commits, 0 Rust)
  and its last commit is a negative result about Miter *Outer* — that branch never began integration.
  **What this session added:** `miter_inner_ref.py` + `miter_inner_wall_split.py` (both now in
  `docs/planning/poly/`), and §3.8a — the arc is absorbed by the two WALL faces, split at the arc
  vertex `ceil(ns/2)` from the `S1` end, which makes those walls non-planar and moves the unbeveled
  edge's foot onto the arc. That was the last unknown blocking a watertight port.
  **The integration point is narrow and now known.** The `boss-on-plate` concave corner is a
  degree-3, k=2 vertex, so it takes the **STEP 2c** ellipse chain (`mod.rs:1176`), NOT `miter_weld_arc`
  — its `O` is the shared-face rail MEET and its `M` is the SLIDE up the unbeveled edge (identify `M`
  as the group whose offset from `V` is parallel to the single unbeveled fan edge). Arc mode then
  replaces the ONE shared `miter_arc_pos[v]` chain with TWO per-edge side chains, so `build_at`
  (`mod.rs:1356`) must key on `(v, eid)` and not `v` alone, plus a corner patch and — the part §3.8a
  unlocked — an "extra points" injection into the two wall-face corners in STEP 3 (`mod.rs:1656`).
  ⚠ **Do NOT ship the Sharp/Arc dropdown before that kernel path exists.** A toggle whose geometry is
  unchanged is precisely the "workaround that pretends to be the feature" CLAUDE.md forbids; and do
  not ship Arc without **Spread**, per §3.8.

- **2026-08-11 (e) — EDGE WEIGHT IS NOW A PERCENTAGE OF WIDTH, ALWAYS LIVE, EVERYWHERE. Owner: "the
  edge weight feature should only have one weight slider, not part of variable [width]; the weight is
  a percentage of the width."** The (d) fix above repaired the display-sync gate and the owner still
  got nothing, because two more things gated the feature and neither was the sync:
  **(1) It was buried behind a mode.** Reading the weight required ticking *Variable Width* and then
  choosing *"Authored weight"* from a combo box inside it. `bevel_var_width` defaults to 0, so out of
  the box the column was simply never read. Worse, that driver made an UNPAINTED edge read weight
  0.0 → `Min Width`, which defaults to 0 — so the one path that did read weights erased the bevel
  from every edge not painted yet.
  **(2) The LIVE Ctrl+B bevel never read it at all.** `poly_bevel_regenerate` called
  `bevel_edges_offset` with no policy. Weights were wired only into the Bevel MODIFIER stack, and
  Ctrl+B is where most bevels are actually made — so even with the mode set correctly, the common
  workflow showed nothing.
  **Now:** the weight is a MULTIPLIER applied on top of whichever driver is active, not a driver of
  its own. `width_e = driver_width_e x weight_e`, **absent = 1.0 (full width)** so an unpainted mesh
  is bit-identical to before and painting only ever scales an edge DOWN. Live drag and modifier both
  route through `bevel_edges_offset_var`. "Authored weight" is gone from the combo; a `.fl4` holding
  the retired driver 2 still loads and behaves as Uniform with its weights applied.
  **Trap found while doing it, worth the entry on its own:** merely PASSING a policy switches the
  kernel from the ONE global clamp limit to per-edge ceilings — that substitution is what Variable
  Width and Auto Merge are for. A weights-only policy inheriting it made a 50% weight come out
  **wider** than the unweighted bevel (measured 0.15 vs 0.05) wherever the global limit was binding,
  i.e. the weight read as "turned some other option on". `BevelWidthPolicy::per_edge_limit` now says
  which regime the policy owns; weights-only sets it `false` and scales exactly the width the user
  already sees. Anything adding a new reason to build a policy must decide that field deliberately.
  Gates: `var_width_authored_weight_lerps_the_band` (1.0/0.5/0.25 of a 0.3 width → 0.3/0.15/0.075,
  unpainted → full width, legacy driver 2 → percentage),
  `bevel_variable_width_reaches_the_kernel_through_the_stack` (weight applies with
  `bevel_var_width = 0`), `bevel_variable_width_by_weight_counts_as_an_edge_weight_reader`.

- **2026-08-11 (d) — VARIABLE WIDTH BY AUTHORED WEIGHT (`W`) never updated the display. Owner: "the
  variable width with weight (W) does not work".** Nothing wrong with the kernel or the data: the
  `AuthoredWeight` driver grades correctly (`bevel_op/mod.rs`, gated by an existing test), `W` writes
  `CustomData.crease`, and `poly_authored_channels` feeds it to `bevel_var_width_spec`. The break was
  one gate in the app layer. **Authoring a weight only re-evaluates the display stack when something
  in the stack can observe the column** — a deliberate optimisation, because `content_signature` folds
  `crease`, so an unconditional sync is a full re-bake on every 1% step of a `W` drag, and the display
  bake dominates poly interactivity. That gate (`poly_write_edge_weights`, `poly_groups.rs`) asked
  only *"is some modifier masked on the `EdgeWeight` geometry channel?"*. **A Bevel driving Variable
  Width from authored weight reads the same column with its channel left at `All`**, so it was
  invisible to that test. The display cache KEY in `mesh_sync.rs` already knew about this consumer —
  its comment even says "the channel predicate alone would miss it and editing a weight would not
  refresh the display" — so authoring a weight invalidated a cache entry that nothing then asked to
  rebuild. Every authoring path (W drag, numeric entry, menu, Tool Parameters slider) funnels through
  the same write, so all four were dead.
  **Fix is structural, not a second special case:** the question "does this modifier read the authored
  edge-weight column?" is now ONE predicate, `PolyModifier::reads_edge_weight()`
  (`core_math/src/scene_graph/node.rs`), called by both the cache key and the sync gate. They had each
  spelled the rule out separately and drifted apart, which is the actual defect — a third consumer
  (Catmull-Clark creasing is the expected one) now extends one function and both sites follow.
  Gate: `bevel_variable_width_by_weight_counts_as_an_edge_weight_reader`.
  **Bankable shape:** when a cache KEY and the gate that decides whether to REBUILD are written as
  two independent predicates over the same question, the key gets updated for a new consumer and the
  rebuild gate does not. It fails silently and reads as "the feature does nothing", never as a stale
  cache — so it gets triaged in the feature's math instead of its plumbing.

- **2026-08-11 (c) — UNBEVEL: the four-corner split was a TURN-ANGLE GUESS; fixed topologically.
  Owner file `UNBEVEL TEST.fl4`, "the yellow strip is a bevel I made using bevel tool and Unbevel
  tool does not work".** Reverse-math unbevel worked on a bevel of ONE STRAIGHT EDGE and on closed
  rings at low segment counts, and refused essentially everything else — which is every strip a real
  model produces. Root cause was one line of `split_loop_into_rails`: the strip's four corners were
  taken as the four LARGEST TURN ANGLES in its boundary loop. That identifies the corners only while
  the beveled run is straight. The moment the run bends, the rail's own bend is as sharp as an end cap
  and outranks it, so two "corners" landed mid-rail; the loop split lopsided (measured: rails 2/6, 7/2,
  2/7, where a strip's two rails are equal by construction), `pair_rails_exclusive` then sewed only
  `min(len)` pairs and every unsewn rail vertex stayed behind — surfacing downstream as "opened holes",
  "lost >2/3 of faces" or a self-loop pinch, i.e. one flat "reverse math refused" for four different
  causes. **Corners are now topological: a strip corner is a boundary vertex touched by exactly ONE
  strip face.** A corner patch at a bend also qualifies, so that yields a SUPERSET; the four real ones
  are picked by the invariant a strip's boundary obeys — it is a grid, so OPPOSITE chains are equal
  length — via an exhaustive 4-subset search (candidate sets are tiny).
  **Four more defects fell out of the same sweep, each independently able to refuse a correct
  collapse:** (1) ring cleanup removed repeated NEIGHBOURS but not hairline FLAPS (`A B A`), so a
  9-gon with a zero-area flap emitted a duplicate vertex and `is_manifold()` killed the whole unbevel —
  cleanup now runs to a fixpoint, and a genuine pinch splits into its lobes instead of being dropped
  (dropping it opened a hole). (2) The "lost >2/3 of faces" gate scaled with the wrong quantity: a
  4-segment ring bevel legitimately IS 16 of a mesh's 22 faces, so removing it correctly tripped the
  rule every time — the rounder the bevel, the worse. Replaced by the wreck's own signature, the strip
  sewn to a single point rather than to an edge. (3) Rails vs end caps were chosen by MEAN PAIRWISE
  distance, which for two parallel rails of run-length L returns O(L), not the bevel width — so on a
  5-column strip the caps measured closer than the rails and the sew ran lengthwise and tore the strip
  open. Now mean NEAREST-vertex distance, which is the narrow-direction question actually being asked.
  (4) The boundary walk was fed by HashSet iteration order, so the same strip could unbevel in one
  process and refuse in the next; sorted now.
  **STILL OPEN — N-WAY CORNER STRIPS (this is the owner's yellow strip).** Several beveled edges
  meeting at one vertex produce one profile arc PER EDGE (three, on a box corner), joined by rails
  through junction vertices — six boundary corners, not four. The two-rail/two-arc model cannot express
  it. Forced through anyway it returns a mesh that is manifold, hole-free and the right bounding box
  while sewing four of six rail pairs across ~1.5 units when the bevel chord is 0.235 — a sheared lie
  that clears every downstream gate, which is exactly the class §6 exists to keep out. It now REFUSES
  (guard: the two end arcs are one profile measured twice, so their lengths must be within 4x).
  Per-edge unbevel from a RAIL edge still works and is the route meanwhile (6 of the 15 boundary edges
  on the owner's strip). The real fix is its own path: one sharp edge recovered per beveled edge, all
  meeting at one recovered corner point.
  Gates: `unbevel_restores_the_cube_for_every_strip_shape` (4 strip shapes x 5 segment counts, exact
  8v/6f cube back every time), `unbevel_is_deterministic`,
  `unbevel_refuses_an_n_way_corner_strip_instead_of_shearing_it`. core_math unbevel 10/10 pass.

- **2026-08-11 (b) — CORNER-SPAN WIDTH DRIVER SHIPPED (Bevel modifier). Owner: "for the variable edge
  width you use length of segment, this does not work in all scenarios… we should build an option based
  on length between corners, this length is always proportional to the detail."**
  The complaint is exact: `AutoSize` grades by each edge's own clamp CAPACITY, which is a property of
  ONE segment (`size_of` → `cap`, falling back to `half_len`). Cut a 2 m panel into ten segments and
  every segment reads as small detail, so an unchanged shape asks for a smaller bevel.
  **The construction is OWNER-SPECIFIED, and my first two readings of it were both wrong** — worth
  recording because the wrong ones look reasonable. (1) I assumed an angle-threshold walk along the
  edge run to find "corners"; owner: *"this has no link to the angle."* It is purely LOCAL to the
  vertex — no walking, no angle test. (2) I assumed sliding each neighbour to its own far end; that
  fails the owner's own counter-example. The correct rule, confirmed against his 2 m / 1 cm case:
  at each endpoint of the beveled edge take the two in-face neighbours, **slide BOTH by the same
  `s = min(len)`**, and measure the chord between the slid points; the edge's size is the **min over
  its two ends**. The shared, min-capped slide is the whole mechanism — it is what stops a long
  neighbour inflating a small detail (`corner_span_sizes`, `bevel_op/clamp.rs`).
  ⚠ **I OVER-CLAIMED SUBDIVISION-INVARIANCE when scoping this and it does NOT fully hold.** I told the
  owner the principle was "subdividing the mesh must not change the bevel width". True while the
  SHORTEST neighbour is untouched (cutting the 2 m side changes nothing — the 1 cm side still caps `s`),
  FALSE when the shortest neighbour is itself subdivided, which shrinks `s`. Same sensitivity the
  existing clamp capacity already has, so the two measures stay consistent — but **do not write a gate
  asserting invariance under arbitrary subdivision**. The gate that IS true and shipped is SCALE
  EQUIVARIANCE (scale the model by k ⇒ every span scales by k).
  **Wiring:** `BevelWidthDriver::CornerSpan` (appended; the enum is NOT serialized, so no format risk),
  computed ONLY for that driver so every other driver stays bit-identical; `bevel_var_width == 3` in
  `bevel_var_width_spec`; modifier UI is now a 3-way combo (*Auto by segment size* / *Auto by corner
  span* / *Authored weight*). **No save-format change was needed** — the existing `FL4PBVAR` record
  already carries `var_width: u8`, and an older build reading a `3` falls through its documented
  `_ => AutoSize` arm, so the file degrades gracefully instead of failing to decode.
  **Scope, stated:** modifier only. The interactive `B` drag and Adjust-Last-Bevel still run uniform
  width (unchanged since §11 2026-08-10(b)), so this option does not appear in the destructive panel.
  **Gates (4 new, `bevel_op/tests.rs`):** `corner_span_is_capped_by_the_shorter_neighbour` (the owner's
  2 m/1 cm case → span 0.02 exactly, plus an explicit assert against the far-end reading that would
  give 2.01), `…_does_not_care_which_side_is_short`, `…_grows_with_the_detail`,
  `…_scales_with_the_model`. Bevel gates **145 green**; core_math 1122 passed / the same 3 pre-existing
  red. `cargo build -p app` clean.

- **2026-08-11 — MITER INNER = ARC IS NOT IMPLEMENTED (it is a MISSING FEATURE, not a regression).
  Owner: "you just broke what was already working, the arc inner … we still don't have the same arc
  inner miter that Blender has."**
  🔑 **ORACLE VERDICT: our bevel is VERTEX-EXACT with Blender at Miter Inner = Sharp. Max vertex
  deviation 0.0000000 over 28/28 verts, 0 unmatched.** Nothing regressed; the Arc inner miter was
  never built. AZRA has **no miter parameter at all** (grep `miter_outer|miter_inner|bevel_miter`
  across the workspace = 0 hits) — our miter is permanently the equivalent of Sharp/Sharp, which is
  also what every reference in §7 was generated with, so no gate has ever exercised Arc.
  **The measurement** (`core_math/examples/miter_inner_diag.rs` + scratchpad `miter_inner_ref.py`,
  Blender 4.5): base = cube ±1 with a 0.6 boss extruded to z=1.8 (16v/14f) — the owner's geometry
  class, four CONCAVE edges at the boss base meeting at four INNER corners. Both kernels fed the
  *identical* base (Blender's own `_base` dump embedded in the Rust example, not a rebuilt
  approximation). Owner's exact tool params: w=0.1382, seg=3, shape 0.50, Clamp Overlap on.

  | Miter Inner | Miter Outer | nv | nf |
  |---|---|---|---|
  | SHARP | SHARP | 28 | 26 |
  | SHARP | ARC   | 28 | 26 |
  | ARC   | SHARP | 60 | 54 |
  | ARC   | ARC   | 60 | 54 |
  | **AZRA (no miter modes)** | — | **28** | **26** — all quads, manifold, validate ok |

  ⚠ **MITER OUTER IS IRRELEVANT TO THIS COMPLAINT AND THE OWNER'S SCREENSHOT CIRCLED IT.** Outer
  changes NOTHING here (28/26 either way) because it governs CONVEX corners and every beveled edge
  in this scenario is concave. Only **Inner** moves the result, and it moves it a lot: 28→60 verts,
  26→54 faces = **+8 verts and +7 faces per inner corner**. Do not "implement the arc miter" by
  wiring Outer — it will measure as a no-op on exactly the geometry that prompted the request.
  **Scale check before anyone estimates this:** Arc inner more than doubles the output. It is a real
  corner-construction stage, NOT a parameter tweak.
  ⚠ **CORRECTED SAME SESSION — my first read of this was wrong and is exactly the kind of cheap
  pointer §0 warns about.** I initially wrote "generalise `miter_weld_arc` (`corners.rs:28`)". Then I
  extracted the actual corner from the oracle (§3.8) and it is NOT a two-BoundVert bridge: the
  intermediate levels are mirrored PAIRS around a lone diagonal point (2/1/2/2) and the corner arity
  goes `{4:10}` → `{4:14, 3:1, 7:2}`. That is a VMesh grid in the §3.7 `M_ADJ` family. Sizing this off
  `miter_weld_arc` under-estimates it badly. **Also: `spread` is a required SECOND parameter** (the arc
  ends sit back along each edge by exactly `spread`; 0.6−0.5 = 0.1) — a Sharp/Arc dropdown without
  Spread cannot reproduce the reference at all.
  **Ruled out this session, with evidence, so nobody re-investigates:** the uncommitted
  variable-width / auto-merge work (2026-08-10 b+c) is NOT implicated — both toggles default off
  (`bevel_var_width: 0`, `bevel_auto_merge: false`), `bevel_var_width_spec` returns `None` unless one
  is set, and on the `var == None` path the clamp set is identical by construction (`beveled_he` is
  `{eid, twin}` per accepted primary; `und2prim` keys the same primaries both ways, so every `note()`
  lands in `cap` and `cap.values().min()` IS the old running `limit`). 135/135 bevel gates green.
  ⚠ **DIAGNOSTIC DEBT THAT COST A FULL ROUND: the unconditional `[BEVEL] edges=… -> verts A->B
  faces C->D boundary=…` log line NO LONGER EXISTS** (it survives only in `core_engine/lib_stall_v2.log`
  and as a comment at `poly_mesh.rs:2393`). The only `[BEVEL]` line left in the source is the
  CONDITIONAL flat-clamp one, so an owner console paste cannot answer "did the kernel even run" —
  absence of output is ambiguous between "no bevel" and "bevel, no flat-clamp". Restore it.
  ⚠ **FORENSIC TRAP (cost two rounds): the interactive bevel bakes into the BASE MESH and leaves
  `modifiers: 0`.** A saved `.fl4` with zero modifiers is NOT evidence that no bevel ran. Read the
  VERTEX POSITIONS instead: an arc shows up as a ladder like y = 1.0 / 1.0182207 / 1.0680000 /
  1.1359998 (= 4 points, seg=3, w=0.136). Two owner files that looked like "before and after" were
  two *different* models beveled at 0.136 and 0.1382. Diagnostic added:
  `AZRA_FL4=<path> cargo test -p app --bin app inspect_corner -- --ignored --nocapture`
  (`app/src/poly_curvature.rs`) — modifier stack, face-arity histogram, highest-aspect faces,
  highest-valence vertices, and `AZRA_OBJ_OUT=<dir>` to dump the mesh as OBJ for the oracle.
  **STARTED (owner GO, same day):** the closed-form arc BOUNDARY is derived (§3.8) and landed as
  `inner_miter_arc` (`bevel_op/corners.rs`) + `MiterInner` / `BevelMiter` (`bevel_op/mod.rs`), with 6
  gates: ends-sit-back-by-spread, radius==spread at 60°/90°/120° openings, **vertex-exact vs Blender at
  ns ∈ {2,4}**, the ns=3 resample gap pinned as bounded AND directional (ours ON the circle, Blender's
  INSIDE — so a flip reads as a stale table, not a bug), zero-spread ⇒ Sharp, collinear ⇒ chord/no-NaN.
  Bevel gates 135 → **141 green**; core_math 1118 passed / the same 3 pre-existing red
  (`edge_select::directed_loop_crosses_pole_straight`, `mesh_ops::apply_in_place_refusal_leaves_mesh_untouched`,
  `mesh_ops::clean_mesh_still_refuses_slit_creating_dissolve` — none bevel, and nothing calls the new
  code yet).
  **NOT wired on purpose — the bevel is byte-identical today.** Remaining: the patch INTERIOR (6 verts
  `P0a/b`,`P1a/b`,`P2a/b` + `D`, 7 faces = the §3.7 `M_ADJ` VMesh), then the pipeline hand-off in
  `build_boundary_flat`, then the two UIs (`poly/bevel.rs` destructive panel + the modifier section) and
  a NEW FL4 record. **No UI until the fill exists** — a Sharp/Arc control that leaves corners sharp is
  the exact failure the owner spent a session reporting.

- **2026-08-10 (c) — AUTO MERGE VERTICES SHIPPED (Bevel modifier). Owner ask: "auto merge vertex that
  touch each other — NOT a merge by distance, that breaks small [detail]; if 2 edges are beveled too
  near one another, past the clamp limit it will merge the vertex at the clamp position", plus "the
  merge will only affect geo created by bevel."**
  🔑 **MEASURE-FIRST PAID OFF: most of this was already in the kernel, and the "obvious" fix was a
  no-op.** The `WeldMap` (§5 invariant 2) merges any two rail points landing on the same spot, so at
  exactly the clamp limit the collision ALREADY resolves to one vertex, cleanly — thin plate, all
  edges: w=0.02 → 24v/26f; **w=0.05 (the limit) → 16v/18f, 0 degenerate faces, watertight, valid**;
  w=0.10/0.30 → identical to 0.05, frozen. So the merge was never the missing piece. The missing piece
  is that Clamp Overlap applies its ceiling as ONE GLOBAL MINIMUM, so a single tight spot freezes
  everything: cube + a 0.06-wide loop-cut band, request 0.2 → **the whole cube bevels at 0.030**;
  per-edge → **the roomy corners reach 0.200** while the band pins and welds at its own limit
  (32v/34f either way, 0 degenerate faces, verified at seg 1 AND 3).
  **Implementation is 2 lines of new kernel behaviour** — `BevelWidthDriver::Uniform` (t = 1, so every
  edge targets `params.width`) riding the per-edge ceiling + `opposing_pair_caps` that 2026-08-10(b)
  already built. `BevelVarWidth` was renamed **`BevelWidthPolicy`** because it now carries two
  independent things (per-edge CEILING, and optionally a graded TARGET); `is_graded()` gates
  `width_min` so the graded floor cannot leak into the single-width mode.
  ⚠ **TWO SABOTAGE RESULTS WORTH KEEPING — I was wrong twice about what is load-bearing.** (1)
  Guarding the global clamp with `&& var.is_none()` changes NO gate: the policy path derives targets
  from `width_req`, never from `w`, so clamping `w` could never reach it. Kept only as a dead-work
  guard + a trap-stopper for a future edit that starts reading `w`. (2) `lo` is irrelevant under
  `Uniform` because `t = 1` makes it cancel in `lo + (width − lo)·t`. The gate that IS live is
  "discard a `Uniform` policy" → the roomy corner drops 0.2 → 0.0299 → FAILED. **Sabotage the thing
  you claim is the mechanism, not the thing you just edited.**
  **Owner's two constraints, answered structurally rather than with a threshold:** *not a merge by
  distance* — there is no distance pass anywhere; the only vertices that can coincide are rail points
  this bevel computed, placed at `sep · w_a/(w_a+w_b)`, which IS "the clamp position". Nothing scans
  the mesh, so no small feature can be swallowed by proximity; per-edge ceilings make small features
  get *smaller* bevels, the opposite of breaking them. *Only bevel-created geo* — the weld joins new
  rail points; no original vertex is displaced (gated on a PARTIAL selection).
  **Stops AT the meet, never past it** (owner's words). Eating the strip to keep growing is Consume
  Geometry, which stays mutually exclusive with this (absorb decides from ONE width; the UI disables
  BOTH per-edge toggles under Consume and zeroes them, so no invisible state drives geometry).
  **Persistence:** `bevel_auto_merge` is `#[serde(skip)]` and rides the EXISTING `FL4PBVAR` record as
  a third field — allowed ONLY because that section was created hours earlier and is still
  uncommitted, so no `.fl4` carries a narrower version. Once it is committed the rule flips back to
  "new magic, written last"; the record's doc comment says so.
  **Gates:** 5 new kernel (`merge_per_edge_ceiling_lets_roomy_edges_reach_full_width`,
  `merge_uniform_driver_ignores_width_min`, `merge_welds_colliding_rails_into_one_vertex`,
  `merge_never_moves_an_unbeveled_original_vertex`, `merge_leaves_no_degenerate_faces_across_a_sweep`
  = 30 configurations, all 0 collapsed faces) + app round-trip / collect / stack-hash / an e2e gate
  through `apply_poly_modifiers_tagged` that also pins "needs Clamp Overlap" and "Variable Width
  implies it". 135 bevel gates green. **No degenerate-face cleanup was written**: every measured
  configuration returns 0, so `remove_degenerate_face` (`half_edge/ops_dissolve.rs`, private, 2-gon
  only) stays unwired and the sweep gate will say if that ever changes.

- **2026-08-10 (b) — VARIABLE WIDTH SHIPPED (Bevel modifier). Owner ask: "having the same width
  everywhere causes problems on smaller detail — set 2 widths, small and large, and let the bevel
  adjust in between depending on the size."** The complaint is real and it is STRUCTURAL, not a feel
  issue: STEP 1 computes a per-edge capacity and then **collapses it into ONE global minimum**
  (`limit = limit.min(…)` over every beveled half-edge, applied as `w = w.min(limit)`). So with
  Clamp Overlap ON the single smallest edge in a selection drags every other edge down to its own
  limit (measured: a big cube + a 0.05 cube in one mesh, request 0.3 → the BIG cube gets 0.05), and
  with it OFF the small detail folds. Blender has the same global limit; its answer is per-edge bevel
  WEIGHT, which is manual.
  **Feasibility, established before any code:** per-edge widths already exist and are oracle-gated —
  `adjust_offsets` produces them and they ride `Rail.w` through `solve_corner_lines2` /
  `loop_slide_meet2` / `build_boundary_flat(w_of)` / `partial_back_edge_slide` (§11 2026-07-23(d)).
  Unequal widths at a shared corner are therefore a SHIPPED condition, not a new regime — the feature
  only changes where those numbers come from, so **no new corner math was needed.**
  **Design:** new `BevelVarWidth { width_min, driver, weights }` passed beside `BevelParams` (which
  stays `Copy` and unchanged — it is built as a full literal at ~60 sites, and adding fields there
  would have churned every one). `bevel_edges_offset` keeps its exact signature and delegates with
  `None`; new `bevel_edges_offset_var` carries the spec. `w_e = lo + (width − lo)·t_e`, with
  `t_e = cap_e / cap_max` (driver AutoSize) or the authored 0..1 weight (AuthoredWeight), then
  `min(cap_e)`. Two properties make it safe and both are gated: **uniform geometry is bit-unchanged**
  (all caps equal → every t = 1 → the uniform result), and **the floor never wins over capacity**
  (a `width_min` 4× an edge's room degrades to the room, never a fold).
  `adjust_offsets` stays authoritative — its least-squares just fits per-edge targets now:
  `d = Σ(g_i·w_i)/Σg_i²`. The uniform branch keeps the ORIGINAL `w·sg/sg2` expression verbatim, not
  the algebraically-equal `Σ(g_i·w)`, because the two differ in the last f32 bits and every prior
  gate is pinned to the former.
  **NEW RULE FOUND THE HARD WAY (`opposing_pair_caps`, `bevel_op/clamp.rs`).** Going per-edge
  removes a protection the global minimum was providing BY ACCIDENT. On a 0.1-thick plate the long
  edges' local capacity is 1.0 (their in-face neighbours are 2 units away), so a 0.3 target sails
  clean past the opposite wall: sabotage-verified, the bottom edge's rail lands at **z = +0.25 on a
  plate whose surface is at 0.05**. No vertex-local term can see it — the two colliding edges are two
  steps apart in the face ring, never adjacent. Fix: for NON-ADJACENT beveled edges in one face whose
  inward perpendiculars oppose, `w_a + w_b ≤ separation`, resolved proportionally so the width ratio
  survives. The opposition threshold is **−0.95 (≈18° of antiparallel), and that number matters**:
  at −0.5 a 32-gon cylinder cap's edges 12 steps apart register as "facing" and cut a legitimate
  0.97·r rim bevel to ~0.82·r. Only a genuine slab shares a corridor; a convex polygon's offset is
  bounded by its corner solves, not by distance to nearby vertices.
  **Scoped to the variable path on purpose.** The uniform path has this same overlap bug TODAY
  (select only the 8 long edges of a thin plate: the slide walk allows 0.1 each into a 0.1 gap) but
  fixing it there would change existing bevels and risk the cylinder-cap case the STEP-1 comment
  warns about. Logged separately in `COMPLETE_BUG_TRACKER.md` as `BEVELOPPWALL`, not folded in here.
  **Consume Geometry is mutually exclusive** with variable width (UI disables it AND zeroes the
  field, so no invisible state drives visible geometry): absorb picks which rings to dissolve from
  ONE width, so a per-edge width would make that decision disagree with the geometry it produces.
  **Persistence:** `bevel_var_width: u8` + `bevel_width_min: f32` on `PolyModifier`, both
  `#[serde(skip)]` riding a NEW `FL4PBVAR` tail section written LAST — never an in-blob field (the
  positional-bincode trap) and never a widened existing record (a section decode error fails the
  ENTIRE load). Unused ⇒ no magic bytes ⇒ byte-identical to pre-feature saves.
  **Gates:** 6 new in `bevel_op/tests.rs` (`var_width_off_matches_uniform`,
  `…_uniform_mesh_matches_uniform_bevel`, `…_auto_gives_each_feature_its_own_width`,
  `…_floor_never_exceeds_capacity`, `…_opposing_walls_stay_uncrossed`,
  `…_authored_weight_lerps_the_band`) + 4 app-side: `poly_bevel_var_params_round_trip`,
  `collect_poly_bevel_var_emits_only_enabled_modifiers`,
  `bevel_variable_width_changes_stack_hash` (the display-cache key, without which the toggle looks
  inert until an unrelated rebuild), and
  **`bevel_variable_width_reaches_the_kernel_through_the_stack`** — the one that runs the real path
  (`PolyModifier` → `bevel_var_width_spec` → `bevel_edges_offset_var`) on a big-box + tiny-box mesh:
  OFF ⇒ the big box gets the tiny one's 0.05, ON ⇒ 0.3, authored-weight-1 ⇒ 0.3, unweighted ⇒
  `width_min`. The kernel gates prove the math; only that one proves the app wired it up.
  core_math 1085 passed / 3 pre-existing red, unchanged; `app --bins` 731 passed with the SAME 5
  pre-existing red (all main-blob `.fl4` decode, unrelated); serde_golden 3/3.
  **NOT done (stated, not dropped):** the interactive `B` drag tool and Adjust-Last-Bevel keep
  uniform width; no live "0.006–0.030 across 24 edges" readout in the panel, because the width
  telemetry is thread-local and the modifier evaluates off the display-sync path — it would read
  stale from the UI thread.

- **2026-08-10 — VERTEX BEVEL SEED WIPED ON FRAME 1 (owner: "i try and did not work").**
  Follow-up to the 2026-08-09 app-arm fix. Seed width + regenerate on arm was correct, but
  every frame `poly_update_bevel_drag` ran with `width = |dx| * scale`. On the first post-arm
  frame dx≈0 → **width written to 0** → regenerate restored the original mesh → seed flashed
  off or never stuck. Fix: `PolyBevelDrag.width_base` (seed / HUD re-anchor); drag formula is
  now `width = width_base + |dx| * scale` (edge arm keeps base 0 → same as before). HUD width
  set re-anchors base + start_mouse. No core math change. Rebuild the app binary before retest.

- **2026-08-09 — VERTEX BEVEL APP PATH (owner: "i can not bevel a vertex").** Core
  `bevel_vertices_offset` was already green (oracle cube + sweep). The failure was the **app
  arm**, not the math. **(0) ROOT UX:** Tools palette listed `tw("Bevel Vertices")` = `ready:false`
  → row is hover-only, never clickable, and `ui2_tool_activate` had no branch — dead tool.
  Promoted to `tk(..., "Ctrl+B")` and wired to switch Vertex mode + `bevel_activate`. **(1)**
  Vertex mode + empty selection fell through to the Modo all-edge path — Ctrl+B without a pick
  rewrote the whole mesh as edge bevel. Now refuses with a toast. **(2)**
  `poly_activate_vertex_bevel` armed at width=0 and core skips `d<3` with no feedback — open-rim /
  wire verts looked like a dead hotkey. Now filters to degree≥3, toasts when nothing is bevelable,
  seeds width = 15% shortest incident edge and regenerates on arm so geometry appears immediately.
  **(3)** Install guard rejects only `eprintln`'d — toast once per drag via new
  `warned_install_reject`. Confirm no-op message distinguishes verts vs edges. **(4)** Q-menu
  "Bevel Vertices" under Vertex Ops; experimental "Vertex bevel" checkbox removed (was a **dead
  gate** defaulting false — feature shipped 2026-07-03, flag now defaults true for JSON load only).
  No new math dead-end. Gate: `vertex_bevel_from_before_installs_cube_corner`
  (`app/src/poly/bevel.rs`) + existing `vertex_bevel_cube_matches_oracle`. Path: palette / Ctrl+B /
  menu while `PolyWorkMode::Vertex` + verts → `poly_activate_vertex_bevel` →
  `bevel_vertices_offset`.

- **2026-08-09 — UNBEVEL FROM A RAIL EDGE + SELECTION SCOPING (owner: "ctrl alt b in this
  situation should just affect the selected segment letting user resize and collapse the edge
  bevel", screenshot = ONE lengthwise edge of a rounded corner selected).** Two defects, one in
  each layer. **(1) app scoping:** `poly_activate_unbevel` preferred the `poly_bevel_history`
  entry whenever the signature matched, and that entry covers the WHOLE edge set the last bevel
  ran on — so a picked single edge resized/removed every bevel in that set. Now an explicit
  face/edge selection is treated as an explicit SCOPE and skips history for the reverse-math
  path; only an empty selection still gets the (exact) history restore. **(2) core seeding:** the
  §11 (2026-08-08l) KNOWN LIMIT — "selecting the arc's RIM CHORDS still refuses" — was the
  owner's exact case. A rail edge borders the strip and the flat wall equally, so seed 1
  (incident) is strip+wall by construction and seed 2 (crossed) spans nothing. New **seed 3
  `rail_strip`**: walk ACROSS the strip from the seed edge, starting on the NARROWER of its two
  faces and stopping at the first face >3x that width (a bevel's faces are narrow and similar;
  the surfaces it joins are wider). The walk is also the scoping — it collects only the strip
  column at the selected edge, so one segment of a long beveled edge unbevels alone.
  **Ordering matters and cost a test cycle:** seed 3 first tried LAST still failed — on a cube,
  seed 1 does not refuse a rail selection, it *succeeds wrongly*, collapsing the 2-face
  strip+wall component into a plausible 7-face mesh (one strip face orphaned). Seed 3 now runs
  FIRST, but only when it is unambiguously a rail selection (`saw_wall` — a side actually
  rejected as wall). Cross-edge and face selections see no asymmetry, get `None`, and keep every
  prior seed order and gate. Reused `edge_select::{he_on_face, opposite_on_face}` (made pub)
  rather than re-deriving the ring walk.
  **KNOWN LIMIT (documented, not hidden):** where the flanking faces are as narrow as the bevel
  itself the walk can overshoot into them; the reverse-math gate + the app's manifold gate + the
  undo episode are the only guards there. Select the strip faces on that kind of dense topology.
  Gate: `unbevel_from_one_rail_edge_restores_exact_cube` (seg=3 bevel, one long rail edge →
  exact 8v/6f ±1 cube, manifold). core_math 1037 pass / 3 pre-existing failures
  (`directed_loop_crosses_pole_straight`, two `mesh_ops` dissolve/refusal tests — verified
  identical with the change stashed). `cargo build -p app` green.

- **2026-08-09 — CONSUME SEGMENTS + ROUNDNESS (the WEDGE path was the whole gap); SHIFT now
  toggles Consume, shape moves to ALT (owner: "geometri consume does not support segment and
  roundness, pressing shift should toggle the option on when bevel is active").**
  **Diagnosis first — three of the four consume paths were never broken.** The consume chain is
  fuse → wedge → absorb+bevel → plain bevel, and `consume_fuse_bevel`, `absorb_for_bevel` +
  `bevel_edges_offset` and the plain bevel all already took `&BevelParams`. Only
  `consume_wedge_cut` took `width: f32` — segments and shape could not physically reach it, so
  every boolean-rim shape (the owner's EAT class: retraced n-gons, support edges, micro-clusters,
  where the fuse refuses) cut a flat 45° chord no matter what the HUD said. The 2026-08-08 code
  comment had already named the fix — *"Segments/rounding come later by giving the sweep an arc
  cross-section — same machinery"* — so this was a wiring gap, not new geometry.
  **What shipped:** (1) `consume_profile_point(a, b, rim, shape, t)` — ONE profile definition,
  extracted from the fuse's inline bezier (control = the rim vertex scaled by `shape*2`) and now
  called by BOTH paths, so the two consume routes cannot disagree about roundness (the user
  cannot tell which one ran). (2) `build_rim_wedge` sweeps rings X (apex), P_0=A … P_segs=B
  instead of X/A/B; the end caps became the whole cross-section polygon. **At segments=1 the
  emitted faces are byte-identical to the old triangular wedge** (verified: `ring(0)=ao`,
  `ring(1)=bo` reproduces all three bands + both caps) — the flat path is untouched.
  (3) `consume_wedge_cut(mesh, sel, &BevelParams)`; all 5 call sites updated (drag, modifier
  stack, 3 tests) and the drag's `record_external_bevel(width, 1)` → `(width, segments)`.
  **THE TRAP (banked): at shape=1 the profile passes exactly through its control point.** An
  unlifted rim control would make the cutter TANGENT to the original surface along the entire
  rim — precisely the boolean degeneracy the existing 2%-of-width a/b lift exists to prevent
  (the 2026-08-08 open-chain sweep that collapsed a mesh to a sliver). So `r_pts` carries
  `p[i] + outward*lift`, not `p[i]`. Do not "simplify" that lift away.
  **GATE, geometric not topological** (§0.6): `wedge_cut_rounds_with_segments_and_shape` measures
  the cut surface's closest approach to the original sharp rim locus (the circle r=0.45, y=1 of
  the box-minus-cylinder fixture). Flat may not beat the chord's perpendicular foot w/√2;
  segments=4 must come at least 20% nearer; shape=0 must retreat toward the chord and shape=1
  advance past 0.5. **Verified RED on the pre-fix tree** by forcing `segments=1` into the cutter:
  *"segments>1 must round the cut: closest approach 0.1470 vs flat 0.1470"* — bit-identical, i.e.
  segments had literally zero effect. Note the flat closest approach is ~w, not w/√2: a flat
  chamfer has no mid-chord VERTEX, only the a/b rails, so measure surface approach by vertices
  and expect the endpoints to win.
  **INPUT: SHIFT = Consume Geometry, ALT = shape.** SHIFT was `poly_update_bevel_drag`'s
  shape gate, so binding a SHIFT tap to consume on top of it would flip the mode on every shape
  tweak — a genuine collision, resolved by moving shape to ALT (also still on the HUD's Shape
  field, so nothing is lost). SHIFT is a MODIFIER, not an `egui::Key`, so it has no `ACTION_DEFS`
  chord and cannot go in the rebind editor; it uses a `shift_was_down` rising-edge latch on
  `PolyBevelDrag`, the same shape as the existing `ctrl_was_down` unbevel latch. The latch is
  seeded **`true`** at all four activation sites: `poly_bevel` is `Ctrl+B` today, but it is
  REBINDABLE, and a chord carrying SHIFT would otherwise read as a tap on frame 1 and arm the
  tool with consume already flipped. Suppressing an edge the user has not made yet costs nothing
  — that is why `ctrl_was_down` is seeded `true` on the Ctrl+Alt+B path too. Toasts + the Tool
  Parameters hint line updated in the same pass (a stale hint is how the next dev "discovers" a
  collision that isn't there).
  **Tests:** 122 bevel gates + 12 consume gates green (`consume_sequential`,
  `consume_corner_adversarial` 5/5, `consume_corner_placement`, `boolean_locality`); core_math
  1030 pass with 3 PRE-EXISTING failures (`edge_select::directed_loop_crosses_pole_straight`,
  `mesh_ops::apply_in_place_refusal_leaves_mesh_untouched`,
  `mesh_ops::clean_mesh_still_refuses_slit_creating_dissolve`) — confirmed identical on a
  pristine `bevel_absorb.rs`, unrelated to bevel. `cargo build -p app` green.
  **KNOWN LIMIT (documented, not hidden):** the wedge still relies on the boolean, so a rounded
  cutter is more tangency-prone than a flat one on degenerate rims. Every existing gate (cutter
  `validate`/`is_manifold`, boolean validity, `bevel_result_installable`, the >50% locality
  check) is unchanged and still fails CLOSED to absorb+bevel — a refused round wedge degrades to
  the ordinary rounded bevel, never to a broken surface.

- **2026-08-08l — UNBEVEL ON A KNIFE-RECT CORNER ROUND: SHIPPED. The §11k "still open" item is
  CLOSED, and neither proposed option was needed.** Owner spec: *"just rail vertex to one shared
  point, do that for each ring edge in the bevel, now you have perfect 90 degrees, and since you
  refind the starting edge you can reapply bevel to change segment and roundness."* That is
  **exactly what `unbevel.rs`'s reverse math already did** — rails, profile arcs,
  `solve_sharp_point`. It was being vetoed one line before the finish.
  **Method that found it in two steps (bank this):** (1) build the owner's shape headlessly
  through the REAL app path (`poly_knife_shape_release` on a box) instead of guessing at
  topology; (2) mechanically tag every `return None` in `unbevel.rs` with its line number and
  run. The bail was a single site — the `is_manifold()` gate — and printing the error string
  named the defect outright: *"directed edge 35->36 used by 2 faces (same winding)"*, with
  v/f counts already **exactly right** (48v/28f → 40v/24f). Not a geometry bug at all: the
  rebuild's centroid winding heuristic (§6) had flipped the hole. **Deleted it** — winding is
  inherited from the input rings, never re-derived.
  **Second defect, app path (`unbevel_from_edges`):** seeding "every face touching a selected
  edge" is wrong for an edge selection — the strip's flanking walls each share a rail, so the
  component grew sideways and refused. Added a second seed, tried when the first refuses:
  **`crossed`** = only faces the selection SPANS (≥2 selected edges that are non-adjacent in the
  face ring). Touching a face is not the same as being part of it. Selecting the strip's own
  cross edges now lands the identical result the face selection does.
  **KNOWN LIMIT (documented, not hidden):** selecting the arc's RIM CHORDS still refuses — those
  edges bound the flat face just as much as the strip, so they do not identify a strip at all.
  Select the strip faces or its cross edges.
  Gates: `knife_rect_corner_round_unbevels_to_a_sharp_90_degree_corner` (full round-trip — cut →
  rounded, no vertex at the sharp point → unbevel → −8v/−4f, corner recovered at the true
  (0.5,0.5), recovered edge measured at **90.0°** → re-bevel at 7 segments takes and rounds it
  away again), `unbevel_works_from_the_corner_strips_own_edge_selection`. core_math 1013 pass,
  app 652 pass (4 pre-existing unrelated failures, verified identical without the change).

- **2026-08-08k — KNIFE-RECT CORNER ROUND RESTORED; the "make it a real bevel" fix REVERTED
  (owner regression report).** The rectangular knife stopped rounding corners because the
  2026-08-08 Unbevel fix rewrote the COMMIT: release/S cut a sharp rect then vertex-beveled the
  corner verts, while RMB/Enter still cut the arc ring. Full autopsy in §6 (four reasons a vertex
  bevel cannot express this round). Release and S now cut `knife_points` untouched, exactly as
  `poly_knife_free_confirm` does — one path for one operation. Removed:
  `poly_knife_bake_rect_corner_bevels`, `knife_rect_pending_corner_bevel`,
  `knife_closed_rings_from_corners` (185 lines). New gate
  `rect_release_cuts_the_same_rounded_corners_as_rmb_confirm` — verified RED on the pre-fix tree
  (release v=184/f=138 vs RMB v=64/f=36), green after. **Also found: the app test binary did not
  COMPILE at HEAD** — the deleted `rect_corner_bevel_bake_is_unbevelable` formatted
  `app_types::ActiveTool` with `{:?}` and `ActiveTool` has no `Debug`, so `cargo test -p app` had
  been failing to build; that change was never test-run. Now 645/650 pass (5 pre-existing failures
  — `dc_gate0_committed_ref_fl4_loads_unchanged`, `diag_modifier_audit`, three intrude tests —
  confirmed identical on the pre-fix tree, unrelated to the knife).
  **STILL OPEN — Unbevel on a knife-rect corner round.** Two honest options, both untried:
  (a) *exact history*: dry-run the SHARP cut alongside the real arc cut and store that mesh as
  `PolyBevelHistory::original_mesh` with the arc result's `post_signature` — Ctrl+Alt+B then
  restores the sharp rect exactly, for subtract AND slice. Cost: one extra boolean per rounded
  rect cut, plus factoring the cutter-prism + `mesh_boolean` block out of `poly_knife_free_boolean`
  (the slice path needs the same in `poly_knife_free_slice_commit`). Caveat: the history
  mechanism's *live re-bevel* branch assumes `live = bevel(original, edges, params)`, which is
  false here — only the width→0 full-removal branch is truthful, so this wants an explicit
  "restore only" history kind rather than reusing the bevel drag.
  (b) *scope it out*: the round is a knife parameter, not a bevel strip; make Ctrl+Alt+B say so in
  a toast and re-edit the round by undo + re-cut (or via a future re-editable knife op). Cheap,
  honest, no geometry lie.

- **2026-08-08j — CORNER TURNING LANDED (built inline after three subagent attempts died on
  the 64k output cap — the §11i derivation implemented directly).** The fuse now turns
  corners: L-chain, U-chain, and the FULL top-perimeter ring all fuse with segments and
  correct placement. **What made it small:** (1) at lattice-aligned widths the miter point P
  IS the first kept column's rail-cut vertex — no new geometry kind, just a shared index;
  (2) the carve, with corner quads excluded from blocking
  (`is_selected(next/prev)` skip), naturally MERGES the whole corner region into one n-gon —
  no explicit topology surgery, the flood just removes it; (3) the FAN falls out of the
  band emitter: corner-influenced columns override their side-A boundary to P, `push_dedup`
  turns the k=0 band row into the radial triangles (Maya's spokes). New machinery:
  corner detection + loop orientation (shared side → A; loops sharing on BOTH sides or
  non-~90° or non-valence-3 corners refuse), corner-rail clearing (reuses clear_end_rail
  per flanking wall), corner flood (all-verts-strictly-inside-L, 256-face cap), per-he
  relaxation masks in classify_side, both-influenced residual skip, orphan compaction
  before from_polygons. **v1 SCOPE: lattice-aligned widths at corners** (P must land on an
  existing vertex; unaligned refuses → absorb+bevel fallback — sweep-safe). **TRAPS:**
  (a) the cap case (ring_column_box square rim sharing ONE top face) is corner-detection
  bait — every-he-shares-one-face must route to the Cap classifier, not corners;
  (b) the placement audit's "one boundary vert per column" expectation is WRONG under the
  fan — P is shared across the stretch; the honest gates are structural (each miter exactly
  once; no non-miter boundary vert inside an influence zone). Tests: 16 in-file (both
  corner tests un-ignored), placement 2/2 (P/Q exact, roundness bulge, zero strays),
  adversarial 5/5 (pins b/c flipped: aligned widths fuse, unaligned refuse), sequential,
  locality, jitter, 120 bevel, 53 boolean — all green; app builds. NEXT candidates:
  unaligned widths (split the rail instead of requiring an existing P vertex), the
  rounded/sphere-octant corner upgrade, EAT via clean-collar.

- **2026-08-08i — CORNER GEOMETRY DERIVED (bank this — two implementation agents died on
  output caps before using it; the math is settled).** For two rims at 90° meeting at v_c,
  both beveled at width w on a quad grid: the SHARP-MITER seam is the intersection of the
  two quarter-cylinders (axis₁ ∥ rim1 at depth w, axis₂ ∥ rim2), parameterized
  S(φ) = (1−w + w·sinφ, 1−w + w·cosφ, −1+w − w·sinφ) in the cube-corner frame — running
  from **P = the miter point at distance w from BOTH rim lines on the shared face**
  (φ=0, e.g. (1−w, 1, −1+w)) to **Q = the point at distance w down the PERPENDICULAR
  unbeveled edge** (φ=90°, e.g. (1, 1−w, −1)). Both P and Q are exact cylinder–cylinder
  intersection endpoints. **The standard consume profile column (bezier P→Q, control =
  v_c) approximates this seam to the same tolerance the straight-run profiles approximate
  circles** (mid-arc x: 1−0.293w exact vs 1−0.25w bezier) — so the corner column IS an
  ordinary column, SHARED by index between the two adjacent runs (their bands weld through
  it; no special corner patch for the sharp miter). Remaining structure: rim verts within
  w of v_c are consumed; corner-region faces collected by flood-from-corner-quad bounded
  by the L-band; the one shared-face cell containing P gets a two-sided clip retrace
  [cutRun1, P, cutRun2]; each wall gets one extra residual segment from the run's last cut
  to Q; the corner rail (perpendicular edge) is cleared with the clear_end_rail machinery.
  Fan/rounded corners (Maya radial spokes = sphere-octant look) are a LATER upgrade on the
  same skeleton — the sharp miter ships first.

- **2026-08-08h — CORNER-TURNING SESSION CLOSE-OUT: corner fuse NOT built (tests stay
  ignored), cap-miter machinery hardened by adversarial audit + review.** *(Labelled h —
  the g slot was already taken by the boolean-locality entry below.)* The intended design
  — the MITER CORNER AS A SHARED COLUMN between runs (each straight run lofts its bands
  as today; at a corner vertex the two runs share ONE profile column placed on the corner
  miter, exactly the cap-offset math generalized to a quad-ring side) — did not land;
  `ordered_loops` still refuses degree-3+ vertices and corner quads still refuse via
  `ring face owned twice`, so `full_top_perimeter_ring_fuse` and
  `l_chain_around_corner_eats_four_rows` remain `#[ignore]` and every corner scenario
  falls back to absorb+bevel (fail closed, mesh untouched — verified: no panic/unsound
  mesh anywhere in the adversarial sweep). **What DID ship — 5 confirmed fixes from the
  review of the fuse machinery the corner build will sit on:**
  (1) **Cap miter centroid flip REMOVED** — `n.cross(e)` is already inward at EVERY
  vertex of a winding-consistent loop (both n and e negate together on the B side); the
  centroid dot-test was a no-op on convex caps and flipped the miter OUT of the face on
  concave ones (L-plate cap, centroid outside an edge's inward half-plane → folded cap
  that still passes `bevel_result_installable`). Same trap the wedge already documented —
  inward offset = normal×edge_dir, never centroid tests. NEVER reintroduce.
  (2) **Needle-corner miter clamp → refusal** — `cos_half.clamp(0.2,1.0)` silently
  shortened the miter at spike corners (≲23°), breaking the fuse's even-distance
  invariant with no refusal; now cos_half < 0.2 refuses (falls back to absorb+bevel,
  which clamps properly). Cube caps (90°, cos_half≈0.707) unaffected.
  (3) **Determinism: 4 HashMap/HashSet-iteration sites sorted** — open-chain endpoints
  + closed-cycle seeds in `ordered_loops`, the end-face retrace loop, and `walk_loops`'
  equal-length seed ties. Rust HashMaps seed per-instance, so identical input produced
  different vertex/face id assignment per evaluation → per-frame GPU re-upload + broken
  id-keyed downstream data. `check_absorbable` already knew this rule; these missed it.
  (4) **`CURRENT_FUSE_WIDTH` thread-local DELETED** — width is now a parameter of
  `classify_side`. The hidden handoff was never reset; any second caller would have
  built cap offsets with a stale width (silent wrong result, worse than a crash).
  (5) **Loft bands take the NEAREST side's material** — bands in the A half get mat_a,
  B half mat_b (odd middle ties to A); previously the whole fillet took side A's,
  diverging visibly from the offset-bevel fallback on two-material loops.
  **TEST INVENTORY:** in-file: 14 green + 2 ignored corner pins (flip live when corner
  turning lands). NEW `core_math/tests/consume_corner_adversarial.rs` (5 green) PINS the
  refusal behavior — a: L-chain n=6 w=0.15/0.9; b: full perimeter ring n=8 w=0.3/1.0;
  c: U-chain 2 corners; d: two disjoint half-edge L-chains; e: w=1.9 > face size FUSES
  sane (placement pinned structurally: in-cube bound + exact boundary-loop columns +
  untouched-lattice count, because the width-sweep band check is VACUOUS at large widths
  — band radius covers the whole cube). The a–d `assert!(!fused)` pins fail DELIBERATELY
  when corner turning lands: delete the pin only after `check_sane` (which runs first)
  passes. Audit caveat for the implementer: no corner scenario currently exercises the
  wedge/absorb fallbacks through this entry — refusal happens before carving.

- **2026-08-08g — BOOLEAN LOCALITY FIXED IN THE KERNEL (the 2026-08-08c collapse class).**
  The 386v→10v wipe that §11c gated around inside `consume_wedge_cut` is fixed at its root:
  `post_boolean_polish`'s `limited_dissolve(…, None, None)` was preserve-blind (the admitted
  inconsistency at rebuild.rs) — its region merge + valence sweep ate untouched lattices and
  knife scribes whenever an edge-clipping cutter left every coplanar region a simple disk
  (mid-face notches survived only by the annulus-refusal accident). Now: polish passes a
  protect set from the thread-local preserve edges, per-step + manifold-wrap acceptance
  refuses any candidate that deleted a preserved input vertex, the coarse weld keeps the
  INPUT position in mixed clusters, and input verts ON the other operand's surface are
  seam-exempt so flush unions still dissolve clean (blender_exact coplanar_union 8v holds).
  `core_math/tests/boolean_locality.rs` pins it (wedge: 386v → 377v). §11c's in-tool gate
  stays as defense-in-depth; the wedge path no longer collapses when it fires.

- **2026-08-08f — CONSUME v5.3: the "test it yourself" regression + SHIP DISCIPLINE (owner:
  "your last code was 90% there, this one is not doing the job — each time you ship, check
  vertex count AND placement, it needs to make sense").** The regression was v5.2's own
  sequencing: `clear_open_chain_end_rails` ran BEFORE every carve round including round 0,
  so on a pristine mesh it ate LIVE lattice rows ahead of the carve (the "no-op on pristine"
  claim was never verified). Fix: clearing runs ONLY AFTER a dissolve round, and
  `clear_end_rail` got a STUB TEST — more than one non-ring extra edge = pristine vertex →
  STOP harmlessly (next round reaches it), never dissolve. **SHIP DISCIPLINE (standing owner
  directive): the width sweep now verifies PLACEMENT, not just counts** — every vertex must
  be either untouched lattice or inside the fillet band (stray = fail), and each fuse
  boundary loop must have EXACTLY one vertex per column at EXACTLY `width` from the edge.
  Run `width_sweep_never_destroys_the_mesh` before showing the owner anything. **Owner
  variations (all must eat ≥4 rows):** TWO SEPARATE vertical chains at w=1.0 — WORKS,
  pinned (`two_vertical_chains_eat_four_rows`: both consumed, exact boundary loops,
  watertight). L-CHAIN turning the cube corner — the corner-quad class again, pinned
  ignored (`l_chain_around_corner_eats_four_rows`) next to the full ring. **The Maya
  polySmartBevel screenshots are the quality bar for CONSUME ITSELF (directive 1)**, and
  corner turning is now unambiguously the #1 next build; the clean-collar retopo stays a
  SEPARATE toggle (directive 2).

- **2026-08-08e — OWNER DIRECTION for the next consume sessions (ratified):**
  1. **Consume/fuse: corner turning ("full edge ring").** The whole top-face perimeter
     selected refuses today: at each of the 4 corners the side-A corner quad borders TWO
     selected edges (`ring face owned twice`) and side B changes wall face — this is the
     classic bevel corner (§3 `build_boundary` / §9 grid corner) surfacing inside consume.
     Pinned by `full_top_perimeter_ring_fuse` (`#[ignore]`, asserts support — flip it live
     when the corner-turning fuse lands). Falls back to absorb+bevel meanwhile, mesh safe.
  2. **CLEAN-COLLAR RETOPO (Maya polySmartBevel class — owner screenshots confirm the
     target) ships under its OWN TOGGLE, separate from Consume Geometry.** New
     `#[serde(skip)]` PolyModifier/drag flag + FL4PMODF record field when built (same
     save-format pattern as `bevel_consume`) — do NOT overload the consume toggle.
     Spec additions from the Maya screenshots: the band's outer boundary MAY join messy
     surroundings with triangles (the slice seam absorbs irregularity — only the band
     itself must be clean quads); the CLEANED RIM is the vertex-count master (spokes
     continue the cylinder wall segments); Maya's Depth ≈ our shape param; "Weld
     Opposing Arcs" = later corner-meeting feature.

- **2026-08-08d — CONSUME v5.2: "KEEP REPEATING THE FIRST SET" — fuse unbounded (owner: "works
  for the first series but fails after 3 eaten segments").** Root cause chain, found via the
  width sweep: (1) carve round 2+ skips the ring's END segment (the end column is a pentagon,
  `face_vertices != 4` excludes it from blocking) → leftover row edge → fan-4 stub the old
  clearing refused; (2) fixed by generalized stub clearing (keep the straightest continuation,
  dissolve every other fan edge) — but the DISSOLVED edge's FAR endpoint drops to valence 2 on
  the neighbouring rail and re-pentagons column 1 — collapse it too; (3) one clearing pass
  after the carve still lets round 3 regrow pentagons — the actual fix is the owner's own
  phrase implemented literally: `clear_open_chain_end_rails` runs INSIDE the carve loop,
  BEFORE every round, restoring the uniform "first set" state each time. Result: the fuse now
  serves the ENTIRE sweep (w=0.1..1.4 on the cube chain, all `path=fuse`, valid + manifold +
  bulk-surviving at every step). 117 bevel gates green; EAT wedge regression intact.
  **NEXT (owner-designed, owner-ratified choices): CONSUME SOLUTION 2 — CLEAN-COLLAR RETOPO
  for messy boolean rims**, replacing the wedge's geometry-only cut with a topology repair:
  record original surface normals (boolean provenance) → offset the rim loop in each vertex's
  local tangent plane (NOT a flat camera-plane scale — tilted rims need even width) → project/
  SLICE the offset loop into the surface (knife-project machinery) → delete the messy collar
  between the loops → equalize both loops' vertex counts (densest ring wins, per owner) →
  `bridge_edge_loops` → clean quad collar → ordinary fuse. Auto per side (only sides that are
  not already clean quad rings), auto-staged inside Consume when the fuse gate refuses.

- **2026-08-08c — CONSUME v5.1: WEDGE LOCALITY GATE (owner: "push the width — breaks all geo,
  removes all edges; needs to be localised; the sliding auto-merge version was nice").**
  Reproduced with a WIDTH SWEEP the owner suggested trying myself
  (`width_sweep_never_destroys_the_mesh`, w=0.1..1.4 on the cube chain): at w≥0.6 the fuse
  refused (end-rail clear stops after ~2 rows) and the WEDGE took a clean subdivided mesh —
  the boolean's coplanar-merge collapsed 386v→10v. **Fix: locality gate inside
  `consume_wedge_cut`** — a chamfer only ADDS detail near the rim, so losing half the
  vertices means the kernel rewrote unrelated geometry: refuse, fall through to the absorb/
  slide path (strictly local, the owner's preferred wide-width feel). Resulting behavior by
  width on clean meshes: fuse (crisp strip + arc ends) while the end rails clear, then v2
  slide/auto-merge beyond; junk boolean rims still wedge (EAT diag re-verified OK). The
  sweep test is the permanent gate: every width must yield valid + manifold + ≥half the
  vertices, whatever path serves it. OPEN (minor): extend `clear_end_rail` past 2 dissolved
  rows (fan!=3 stop) to widen the fuse's range.

- **2026-08-08b — CONSUME v5: OPEN CHAINS fuse end-to-end (owner: subdivided cube, one full
  top edge — "try this more simple situation"; his red curve = the profile arc where the
  fillet dies into the end face).** The fuse now handles OPEN CHAINS: `ordered_loops` returns
  (loop, closed) including degree-1 chains; `classify_side` grows an extra boundary column at
  the far end; bands/residuals stop at cols−1; each chain END vertex may border exactly ONE
  unremoved face — the END FACE, retraced with the ARC COLUMN replacing the corner vertex
  (arc endpoints lie ON the face's rail-bordering edges, so the retraced ring stays simple
  and twins cleanly). **THE PENTAGON TRAP + FIX:** carving a ring on an open chain leaves a
  valence-3 stub on the end face's shared edge (the end-face lattice owns it) → the merged
  ring face is a PENTAGON → quad gate refused. Fix = `clear_end_rail`: while the end rail is
  shorter than width, dissolve the stub's end-face interior edge (two end-face faces merge —
  consume semantics extend into the end face) and `join_edge_kill_vert` it. **WEDGE LESSON
  (test caught it after an initial too-weak assert):** the boolean kernel MERGES COPLANAR
  faces — on the subdivided cube the wedge cut returned 10v/7f, the whole lattice collapsed
  into big n-gons. The wedge is therefore ONLY for junk boolean rims (curved, like EAT.fl4,
  where nothing is coplanar); clean meshes MUST fuse, which the chain order (fuse → wedge →
  absorb → plain) guarantees. Wedge gained open-chain sweeps anyway (sections extended
  1.2×width past both ends + triangle caps; a/b rings lifted 2% of width off the surface —
  suspected-coplanar-band hygiene). Always assert BULK SURVIVAL (`out.verts >
  in.verts/2`) on any boolean-consume test — manifold+folds=0 alone passed a collapsed mesh.
  Pinned: `open_chain_fuse_on_subdivided_cube_ends_with_arc` (narrow, carve-free),
  `open_chain_fuse_wide_clears_end_rails` (wide, ring crossed + end rails cleared), EAT diag
  still WEDGE OK. 12 consume gates, 116 bevel gates green.

- **2026-08-08 — CONSUME v4: WEDGE CUT — consume by BOOLEAN (owner EAT.fl4: "does not work at
  all, vertices do not get consumed — first step is just a 45 strip we can make large; I
  boolean a cylinder in a sphere").** Diag on the real file (`diag_fl4_consume_bevel`, new,
  with `AZRA_CONSUME_DEBUG=1` refusal tracing): the boolean rim's outer side is retraced
  n-gons (20 of them) and the rim itself is INTERRUPTED by four micro-edge zigzag clusters
  (0.008–0.045 edges at 25–171° dihedrals where the cut ran near tangency) — no clean loop
  exists, the fuse gate refuses, and the plain bevel's GLOBAL clamp is crushed to 2e-5 by
  the shortest junk edge. Those clusters ARE the owner's spikes. **No topology-walking carve
  survives this mesh class; the fix is the owner's own mental model: the rim came from a
  boolean, so the chamfer is cut by ANOTHER boolean.** `consume_wedge_cut`
  (`bevel_absorb.rs`): sweep a triangular wedge along the rim (apex 1.5w outside the corner,
  base corners at `width` along each side surface) → `mesh_boolean` Difference. Consumes
  every vertex/edge/face in its path whatever the topology; flat 45-class strip (seg=1);
  rounding later = arc cross-section on the same sweep. **Three traps pinned on the way:**
  (1) side-offset sign from FACE CENTROIDS is wrong at a hole — an annulus face's centroid
  sits across the void, flipping the offset INTO the hole so the cutter missed the mesh
  (boolean no-op). Correct rule is winding-local: interior is LEFT of a directed half-edge
  (`n × he_dir`), no centroid. (2) The rim polyline must be RESAMPLED (skip points closer
  than 0.25w to the last kept, skip degenerate frames) — the wedge is pure geometry and
  bridges over junk clusters, which the boolean then eats. (3) Ragged selections need a
  tolerant reader: `walk_loops` (longest-edge seed, straightest-continuation walk) when
  strict `ordered_loops` refuses. Chain is now fuse → wedge → v2 absorb → plain bevel, all
  fail-closed. Pinned: `wedge_cut_chamfers_a_boolean_rim` (box−cylinder fixture, manifold,
  0 folds, rim vertices consumed) + EAT.fl4 diag = WEDGE OK v578→591 f592→634 folded=0.

- **2026-08-07d — CONSUME v3: FUSE — the stripe IS the bevel (owner: "Offset Cut addon — does
  the strip and bevel after").** Research triangulated three references that all share ONE
  principle: **the strip rim is placed at an EVEN distance from the edge ALONG the surface (a
  cut through existing topology), and the round is lofted BETWEEN the two rim loops** — Maya
  polyBevel (width = Fraction along adjacent edges), Blender's built-in Offset Edge Loop Cut /
  Offset Edges addon (even-distance loop cut, "not proportional to the crossed perpendicular
  edges"), MESHmachine Chamfer→Fuse (flat strip of polygons → rounded transitional surface
  between its rails). **v3 (`consume_fuse_bevel` in `bevel_absorb.rs`):** (1) dissolve every
  ring the fillet covers (< width); (2) CUT the stripe boundary at exactly `width` along each
  rail — a new vertex ON the rail edge, nothing existing moves (v2's slide is now fallback
  only); (3) delete rim + stripe rows and loft `segments` profile rows between the two
  boundary loops (quadratic profile, control = the sharp edge, shape 0→flat/0.5→round/1→
  convex). The boundary loops ARE the rims: full width achieved, no clamp interaction, no
  leftover band, zero influence from consumed geometry. **Scope gate (fail closed to v2 →
  plain bevel):** closed corner-free loops; each side = clean quad ring OR one cap n-gon
  whose boundary is exactly the loop; face-conflict + rim-isolation gates; materials carried
  through `from_polygons` by parallel vec (bevel_op discipline); installable guard.
  **Pinned:** `fuse_seg1_matches_plain_bevel_when_unclamped` (seg=1 fuse == bevel_edges_offset
  EXACTLY when unclamped — the two constructions agree where their domains overlap),
  full-width-past-clamp, covered-rings-dissolved+mid-row-cut, adjacent-parallel-loops refusal.
  New `bevel_op::record_external_bevel()` resets the last-bevel telemetry so the drag HUD /
  Smart Select strip tagging never reads a stale `bevel_edges_offset` record after a fuse.

- **2026-08-07c — CONSUME v2: THE STRIPE RULE (owner: "does not work nicely… the bevel must not
  be influenced by the eaten segment — create a nice stripe polygon loop, like Maya").** v1's
  defect, diagnosed against Maya polyBevel docs: **Maya places the strip rim by sliding ALONG the
  adjacent edges (width = a *Fraction* of the edge, capped at the shortest edge) — the strip
  always lies ON the original surface.** Blender's default "Loop Slide" is the same idea. v1
  instead dissolved every blocking ring and let the bevel offset perpendicular across the merged
  chord — on curved walls the rim left the surface and the strip followed the eaten rings. **v2
  (`bevel_absorb.rs` rewritten): a ring is DISSOLVED only when its whole row fits inside the
  stripe (2×width); the first OVERSHOOTING ring is not eaten — each of its vertices SLIDES down
  its own rail edge so the stripe height is exactly 2×width.** The stripe boundary is then a
  clean uniform loop on the original surface; geometry beyond it has zero influence; the clamp
  binds exactly at width so the strip rim lands mid-stripe, bounded. Slide fraction hard-capped
  at 0.9 (row below can never invert); a vertex on a selected edge, a non-quad row below, or a
  boundary → skip (fail closed). **Oracle updated:** carve-then-bevel must equal
  `bevel_edges_offset` on a mesh AUTHORED with the stripe in place
  (`carve_then_bevel_matches_authored_stripe_mesh`, vertex-for-vertex). Maya's remaining
  vocabulary (Fraction as a 0–1 parameterization, Mitering/Miter-Along, Depth) noted as future
  reference — NOT implemented; the Fraction cap "never larger than the shortest edge" is
  exactly our clamp, which is why 2×width is the right stripe height.

- **2026-08-07b — CONSUME GEOMETRY v1 (owner: "bevel beyond the first ring, eating the geometry
  that sticks at clamp — like CAD").** New `core_math/src/bevel_absorb.rs`: a PRE-PASS on the Bevel
  MODIFIER's working copy — while the clamp would bind (`row_height/2 < width`), the next parallel
  edge loop is dissolved (`dissolve_edge` per loop edge + `join_edge_kill_vert` on the valence-2
  leftovers, both ID-preserving) and the freed rows merge, raising the clamp; then the UNCHANGED
  `bevel_edges_offset` runs. Gated by a `Consume Geometry` toggle (off by default) in the Bevel
  modifier panel; `bevel_edges_offset` itself and the destructive tool path are untouched. v1 =
  quad-grid rows only; a selected next loop, boundary, or non-quad side is a WALL → fail closed to
  the clamped bevel. **Oracle is NOT Blender** (Blender can't do this): absorb-then-bevel must equal
  bevel on a hand-dissolved mesh — pinned by `absorb_then_bevel_matches_hand_dissolved_mesh` (+4
  more gates in `bevel_absorb::tests`). **SAVE-FORMAT LESSON RE-CONFIRMED:** first attempt appended
  `#[serde(default)] bevel_consume` to `PolyModifier` — the serde_golden gate failed instantly
  (bincode is positional; POLY_PARAMS_MAGIC's doc comment already warned). Correct shape:
  `#[serde(skip)]` on the field + NEW `FL4PMODF` tail section (`PolyModFlagRecord` =
  node_id + mod_index + flag), written newest-last, reattached in `project_load.rs`. Also fixed:
  `bevel_limit_angle`/`bevel_angle_deg` were MISSING from the modifier stack hash (frozen-slider
  trap) — added alongside `bevel_consume` in `mesh_sync.rs`. Known v1 limits: absorption threshold
  is the clamp bound (eats a loop the rail may not fully reach — matches "un-stick the width", not
  "sweep only"); modifier-bevels-ALL-edges configurations never absorb (every next loop is
  selected = wall) — consume pairs with Limit-by-Angle / Smart Select / geo channel.
  **Same session, DESTRUCTIVE path too (owner: "can I test it destructively on the normal
  bevel"):** `PolyBevelDrag.consume` + "Consume Geometry" checkbox in the Bevel HUD panel +
  `poly_bevel_toggle_consume` (single-producer, mirrors `toggle_clamp`). In
  `poly_bevel_regenerate` the EDGE path absorbs from `original_mesh` each frame before the
  unchanged `bevel_edges_offset` — so during the drag it is still non-destructive (shrink the
  width and the rings come back); only confirm bakes. Vertex bevel and the Last·Bevel adjust
  panel / unbevel history deliberately do NOT consume (v1 scope).

- **2026-08-07b — RIM-VERTEX FIX CONFIRMED ON `BEVEL BUG.fl4`; owner's "still broken + snaps back"
  was the STALE-BINARY trap.** The second repro (gutter island: open-ENDED box, bottom-back edge
  beveled INTO the rim, seg5) showed the same folded cap (v132 fan, normals flipping) — but the
  headless gate on that exact geometry (`gutter_end_bevel_stays_open_and_installable`, seg 1/3/5,
  manifold + fold-free + loop-preserved + installable + exact nv) is GREEN on the fixed core and
  RED pre-fix (seg3 folded end cap). A pre-fix `app.exe` (PID 32088) was STILL RUNNING while the
  owner retested — the "snap back on confirm" = that old binary's cap output failing its own
  install guard on some edges (and installing the fold on others). Killed it, rebuilt dev+release.
  The "sometimes works multi-edge" cases were the closed-island M_ADJ corners, which were always
  fine. LESSON (again, now twice): after a kernel fix, VERIFY THE RUNNING PROCESS IS THE NEW BUILD
  before interpreting an in-app retest — §10.3's stale-binary rule applies to the OWNER's instance
  too, not just the dev's.
- **2026-08-07 — RIM-VERTEX CAP KILLED (owner `BEVEL EDGE CASE.fl4`: "broken geo when beveling an
  open surface").** Beveling an interior edge whose ENDPOINT is a boundary (open-rim) vertex fired
  the k=1 partial corner cap — recessed center + fan triangles forming a folded coplanar "lid" over
  the open rim (folded_coplanar_pairs=2 in the owner file; the guard couldn't reject it: manifold OK,
  loop count preserved). **Oracle (`rim_vertex_bevel_ref.py`, Blender 4.5): NO cap at a boundary
  vertex** — open-top cube, vertical corner edge, w=0.2: seg1 nv=10/nf=6, seg3 nv=14/nf=8, arc verts
  lie ON the rim, boundary loop just gets longer. **Fix: the cap-classification loop skips any
  `is_boundary_vertex(v)`** (`bevel_op/mod.rs` ~:1105) — the "corner hole" opens onto the mesh
  boundary and must stay open. **TRAP that hid this for months:** `vertex_fan` is PARTIAL at a
  boundary vertex (walks one way, stops at the gap), so whether the old cap fired depended on WHICH
  outgoing half-edge the vertex stored — `interior_edge_on_open_mesh_matches_blender` stayed green
  (fan missed the beveled leg → k=0) while the owner's mesh capped. New gate
  `rim_vertex_bevel_never_caps_the_open_corner` sweeps ALL FOUR open-box vertical edges (seg1+seg3,
  nv/nf/loop/fold asserts) so the storage lottery can't hide it; proven red pre-fix. 103 bevel gates
  green; 3 pre-existing core_math reds (edge_select pole loop + 2 mesh_ops dissolve-refusal) are the
  concurrent dissolve WIP, untouched files, not bevel.
- **2026-08-08 — LIVE BEVEL UI DOCK-ONLY (owner: merge floating Bevel window → Tool Parameters).**
  Removed the viewport `egui::Window("Bevel")` from `draw_bevel_hud` (was a second editable home
  for width/segments/shape/clamp while Tool Parameters already had Live · Bevel). Cursor orange
  label kept. Tool Parameters is the sole producer UI (`poly_bevel_set_*`); parity: unbounded
  width, shape flat|round|convex suffix, clamp hover, same hotkey help. Kernel/math untouched.
  Same class as create-adjust dock-only.

- **2026-08-05f — UNBEVEL IS NOW REVERSE MATH ON THE BAKED MESH (owner: "blender as it, modo
  add it").** The topology path's midpoint sew is replaced by plane recovery: each rail
  pair's sharp point = f64 least-squares intersection of the OUTSIDE surface planes at the
  pair (λ=1e-6 regularized toward the midpoint — 2 planes → their line pinned nearest the
  pair, 3 → the exact corner; spike guard falls back to midpoint past 10× rail separation).
  Open-strip END ARCS collapse onto the recovered end points (end caps re-trace through the
  original corner). Width is now MEASURED (rail→sharp distance = Blender's offset at any
  dihedral, replacing the 90°-only /√2 guess). Also fixed the RAIL-SELECTION inversion:
  "longer chains = rails" flipped on a multi-seg bevel of a short edge (profile arcs outgrow
  rails → paired the end arcs length-wise — the collapsed-pyramid family); rails are now the
  two chains with the SMALLEST separation (sew collapses across the strip width), length
  only breaks ties. New gate `unbevel_reverse_math_restores_exact_cube`: 1 cube edge, w=0.2
  seg=3 → unbevel from strip faces alone → EXACTLY 8 verts at ±1 / 6 quads / width measured
  0.2 (midpoint sew can never pass — it shaved corners by w/√2 and kept end arcs). App
  precedence flipped: fresh history (signature match) is only a fast path that preserves
  ids/tags; ANY stale case now flows silently into reverse math on the current mesh — this
  also explains "unbevel worked 2-3 days ago": history is session-local, so fresh sessions
  used the topology path (edits kept) while same-session unbevel hit the snapshot wipe.
  927 core_math green.

- **2026-08-05e — UNBEVEL/ADJUST DATA LOSS fixed: staleness gate via `content_signature()`.**
  Owner repro: bevel → keep modeling → Unbevel/rebevel wiped everything after the bevel.
  Root: `PolyBevelHistory` restores the pre-bevel snapshot with NO check that the live mesh
  is still the post-bevel result (its doc claimed invalidation that never existed — zero
  `.clear()`/`.retain()` sites); the redo panel (`PolyBevelAdjust`, re-runs from `before`)
  had the same hole; and the from-history stale-id bail swapped the mesh BEFORE validating,
  so even the bail destroyed the live mesh. Fix (class-level): both structs now carry
  `post_signature` (the post-bevel `content_signature()`, computed once at confirm); Unbevel
  refuses stale history with a toast (entries NOT dropped — Ctrl+Z back revalidates them);
  the adjust panel closes itself instead of applying, refreshes its signature after every
  re-apply, and updates the matching history top in lockstep (panel tweaks are the same
  bevel, must not falsely stale the history); id validation moved before any mutation.
  Signature checks are interaction-time only, never per-frame (O(mesh) hash — the
  overlay_lines rule). Recovery for the wipe was always Ctrl+Z (the swap is one episode).
  Gates: `cargo build -p app`, `poly::bevel` app tests, 99 core_math bevel tests green.
  (Pre-existing flaky, unrelated: `prefix_cache_actually_reuses_the_unchanged_prefix` —
  sub-ms timing assert in a debug build, fails ~2/3 runs with the app open.)

- **2026-08-05d — Smart Select tagging extended to INSET (all geometry incident to the vertices it
  allocated; the pre-existing outer boundary edge deliberately NOT tagged — measured 0 of 4, all at
  90°) and to EXTRUDE (angle-based, owner's 20° `EXTRUDE_TAG_TANGENT_DEG` beside the modifier's 30°).
  Shared rules extracted to `app/src/poly/op_tags.rs`; the bevel's carry moved there. Tagged vs
  untagged live faces: inset 30/42, flush extrude 34/42, ordinary extrude 0 tagged. §6 entry above.**

- **2026-08-05c — SUPPORT-EDGE PLACEMENT fixed at CREATION: `greedy_merge_region_impl`'s
  dissolve order now removes pin-holding diagonals first, then shortest first (maximin
  survivors, deterministic tie-break). Owner test_014's valence-3 mid-chord landing class is
  gone (sweep: all bridges val≥4, longer, min edge unchanged at full chord). No relocation,
  no new geometry — the refuted radial reroute stays refuted. §6 entry above.**

- **2026-08-05b — SLIVER THROTTLE FIXED at the boolean. `test_010` min edge 0.00997→0.09066,
  effective bevel width 0.00485→0.01628 (3.4×), volume-neutral, valid/manifold/fold-free.
  Root enabler was a KERNEL bug: an edge collapse's degenerate 2-gon was `delete_face`d, which
  orphaned its neighbours' twins and tore the mesh — `remove_degenerate_face`
  (`half_edge/ops_dissolve.rs`) now RE-TWINS them. That is the missing op every previous
  merge-by-distance attempt died on. §6 entry above.**

- **2026-08-05 — Smart Clamp (Bevel modifier `bevel_smart_clamp`, OFF by default) built per the
  owner's radial-relocation design; measured to a NO-OP on `test_010` because the radial route
  is always the shortest exit and the real throttle there is a 0.00997 edge (clamped width
  0.00485), not the 0.1792 support edge. Kernel `core_math/src/support_edge.rs`, §6 entry above.**

- **2026-08-04b — angle-limit Smart Select REJECTED by the owner; the design is TAG-based.**
  Built and tested (skipped 336/384 segment edges of a previous bevel, watertight; and
  32/128 on a subtracted cylinder), then reverted unmerged on the owner's direction: tags
  must be set BY THE OPERATION AT MODEL TIME, not inferred from dihedral angle. Also learned
  on the way: sweeping coplanar edges into ANY exclusion re-breaks manifoldness (the §6
  finding resurfaced the moment the angle rule caught them) — coplanar boolean artifacts must
  be DISSOLVED, never excluded. See the new head-of-§6 entry.

- **2026-08-04 — Smart Select foundations landed; partial-selection bevel is the blocker.**
  Shipped `mesh_boolean_with_provenance` + `BoolProvenance` (0753564: each result face tagged
  OperandA / OperandB / Cut by SURFACE MEMBERSHIP — vertex-identity was tried first and is
  wrong, a through-cut leaves no original cutter vertices on the barrel) and the shared
  `HalfEdgeMesh::edge_dihedral_cos` / `edge_is_flat` accessor (ab44e69, replacing a 4th copy
  of the same dihedral test). Wiring both into a `bevel_smart_select` modifier flag was
  REVERTED unmerged, then DIAGNOSED the same day: partial selection is fine (a plain cube
  bevels correctly at 1 edge / a face ring / half its edges). The breakage is RULE 3 —
  excluding a coplanar artifact edge instead of dissolving it. Rule 2 (provenance-based
  cylinder exclusion, the owner's actual ask) is sound: valid, manifold, folds=0. See §6.

- **2026-08-03 — split into module dir, pure relocation.** `bevel_op.rs` (~5.5k) →
  `bevel_op/{mod,offsets,weld,rails,corners,caps,clamp,tests}.rs` (GOD_FILES_SPLIT_PLAN §0).
  No logic change; public paths `core_math::bevel_op::*` stable. Clamp-overlap walk stays
  inline in STEP 1 (`clamp.rs` docs-only — cannot split a function body). Gates: `cargo build -p app`
  + `cargo test -p core_math bevel` (89 pass / 1 ignore) green per peel commit.

- **2026-08-02 (owner's captured mesh) — the REAL defect isolated: partial-corner cap at a DEGREE-5
  endpoint.** Three earlier oracle shapes (L-prism, notch, collinear run) all passed vertex-exact, which
  is why the fix kept missing: none of them had the owner's topology. Added `AZRA_BEVEL_DUMP=1`
  (`poly_drag_tools.rs`) which writes the exact pre-bevel mesh + selection to
  `%TEMP%/azra_bevel_repro.json`; fed that straight into `repro_ref.py`. Result: AZRA 24v/23f vs Blender
  26v/19f — spec + oracle data in **§9.5**. **Lesson (generalises beyond bevel): capture the user's
  actual input before building an oracle; a screenshot does not determine topology.**

- **2026-08-02 (owner: "check blender oracle you will see how blender fix this scenario") — FIXED:
  `geometry_collide_offset` clamped the width to ZERO whenever a neighbouring edge was COLLINEAR,
  so beveling one segment of a straight run silently produced nothing.** Oracle-first, per §7.
  - **Oracles generated** (scratchpad, re-creatable): `lstep_concave_ref.py` (L-step prism, single
    concave/reflex edge, w=0.3 seg=5 → Blender nv=22 nf=13) and `notch_corner_ref.py` / 
    `partial_run_ref.py` (box [0,2]³ minus a corner notch x≤1.5,y≥1,z≥1, so the concave edge
    TERMINATES MID-BODY at (1.5,1,1) instead of running out to a flat end face). Blender enum note:
    this version wants `miter_outer="SHARP"`, **not** `"MITER_SHARP"` (ValueError otherwise).
  - **Finding 1 — the terminating fillet was never broken.** On the notch base (14v/9f) AZRA already
    matched Blender **vertex-for-vertex and face-for-face: nv=24 nf=14, watertight** (gate
    `notch_corner_bevel_vs_blender_oracle`). Blender adds NO special cap here — it inserts the 6
    profile points into the terminating face and re-traces it, which is what we do. So do NOT go
    build an "M_ADJ corner patch" for this report; that would be re-deriving a non-problem.
  - **Finding 2 — THE BUG.** Split that concave edge into two collinear halves (exactly what an
    EXTRUDE leaves) and bevel only one half: Blender → nv=26 nf=19 watertight; AZRA → **complete
    no-op**. Instrumented (`AZRA_BEVEL_CLAMP_DEBUG=1`, kept, env-gated): θ1 = **180°** → the collapse
    closed form `(k + cosθ)/sinθ` divides by sin(π)≈−8.7e−8 → **denom 1.14e7 → limit 6.5e−8 → w
    clamped to 0**. A side only imposes a collapse constraint when its adjacent edge actually TURNS;
    the in-line case is Blender's SLIDE-WALK case (already implemented below it). Fix: `collapse_term`
    returns 0 when `|sinθ| < 1e-4` instead of dividing. Now clamp=ON gives full width and **nv=26
    (Blender-exact), watertight, manifold**.
  - **Residual (logged, not chased):** faces 20 vs Blender's 19 — one n-gon split differently at the
    termination; every VERTEX matches. Gate `partial_collinear_run_bevel_stays_watertight` asserts
    watertight + manifold and prints the face delta.
  - Suite: `cargo test -p core_math` **802 passed / 0 failed / 8 ignored** (the cylinder
    `clamp_allows_blender_width` gate still green — the fix only removes the degenerate branch).

- **2026-08-02 (owner repro: "bevel problem with 5 segments", "side panel geometry creating overlap")
  — MEASURED, two findings, no speculative fix landed.** Owner compared AZRA vs Blender on an L-step
  (cube + slab, one CONCAVE edge filleted, seg=5) and pointed at Blender's screenshot: **Blender closes
  the fillet's END with a dedicated terminating patch** (a curved quad spanning fillet-end → the corner's
  vertical edge — the highlighted face in the oracle shot). AZRA does not build it there, which is what
  reads as "overlapping side geometry".
  1. **The profile itself is NOT the defect — proven.** New gate
     `concave_edge_five_segment_profile_is_even_circle` (L-step prism, reflex vertical edge, w=0.3
     seg=5): exactly 6 profile points per cross-section, chord ratio **1.00** (even bands), every point
     on the true fillet circle within **1e-4**, manifold + watertight at both ends. So do NOT go
     hunting profile spacing/superellipse parity for this report — §6 #23 already says even-angle is
     correct for the circular profile, and the measurement confirms it.
  2. **NEW GAP — `geometry_collide_offset` UNDER-estimates on an all-edges selection over a reflex
     edge.** Gate `modifier_style_bevel_all_edges_clamped_manifold`: the Bevel MODIFIER path
     (`apply_poly_modifiers` → `all_live_half_edge_ids`, i.e. EVERY edge at once) on the L-step is
     clean at w=0.1/0.3 but at **w≥0.6 goes NON-MANIFOLD and LOSES vertices (324→294)** — folding into
     itself — although the shortest feature is 1.0 so Blender's clamp limit is 0.5. `bevel_result_installable`
     then rejects the result, so the side panel silently stops updating (reads as "the panel is broken").
     The gate asserts the green range (≤0.5 manifold, never grows, never inverts) and prints the gap.
  **NEXT (do it oracle-first, §7):** (a) generate the Blender reference for the L-step fillet END and
  port the terminating-patch construction verbatim — do NOT fit a cap by eye (§6 #23's lesson);
  (b) tighten the collide limit for all-edges selections containing a reflex edge. Both are separate
  tasks; neither was guessed at in this session.

- **2026-08-02 (kernel sweep, fix/kernel-sweep) — legacy `bevel_edges_ex` `_segments`/`_shape` params
  were DEAD, removed.** Grep across the workspace: only callers were the `bevel_edges` wrapper (passed
  defaults `1, 1.0`; itself has zero callers) and 9 half_edge tests that assert validity/perf, never
  segment count — no UI/app path ever reached them (the segments slider drives
  `bevel_op::bevel_edges_offset`, which honors segments/shape). Not a lying-UI bug; the signature was
  the lie: it promised multi-segment/shape control and silently produced a 1-segment chamfer. Params
  removed, doc comments rewritten to state the fn is a legacy single-segment chamfer superseded by
  `bevel_op`. No bevel math touched; 83 bevel-filtered tests green.

- **2026-07-23 (promotion) — master's 9 bevel commits cherry-picked onto `feat/ui-polish`** (ec771ed
  e18cfa6 7a78ee2 723516e 5835469 200e85a b3a38d9 08d5cca 52f4fbf — open-face no-ops + RELATIVE guard,
  knife-chain, slit-skip, concave rails, HUD/toast, boolean-splitter + k2-d3 round + directed loops +
  click-select cache key, adjust_offsets v1, weld-arc control point = V). `bevel_op.rs` is byte-identical
  to master; kernel deps ported into trunk's `half_edge.rs` (is_edge_live, content_signature, free_face/
  free_edge chokepoints, slit-refusing dissolve_edge, dead-slot-silent he_source, newell_normal,
  join_edge_kill_vert, dissolve_boundary_valence2_vert, dissolve_faces_region_gated, dissolve_edges_region,
  limited_dissolve). Trunk-only adaptations kept: 200e85a's Action-Center/precision-drag transform hunks and
  the GPU select buffer (`poly_select_gpu.rs`) NOT brought (absent subsystems) — the click-select fix runs on
  the CPU fallback path with a ported content-aware pick-cache key. **NEW FIX found by the promotion (also
  applies to master): STEP-6 hole-seal chaining was NONDETERMINISTIC** — the boundary next-map was built from
  HashSet iteration, so a degenerate hole with a double-outgoing vertex chained a different loop each run
  (±1 face/edge on identical input; probe showed 4 distinct outputs in 20 calls ON MASTER; caught by
  `bevel_apply_inverse_apply_is_identity`'s persisted seed). Fixed by sorting boundary edges + starts
  (§5 invariant 7 / §6 #19). 489 core_math + 42 graphics green.
  (A) Intermittent B double-press: the toggle guard used `active_prim_index_unified()` (`input_keyboard.rs:986`),
  which is `Some` only when `usel.active == Prim(uid)`; whenever `active` had fallen back to the body node
  (range/body select) a visibly-selected prim read as "no object" → B no-op → press twice. Fixed by adding
  `resolved_prim_index_unified()` (active OR `last_clicked` fallback, mirroring `properties.rs:20`) and gating
  the toggle on it. (B) Bevel/Extrude persisted across body switch: added `ToolState.mode_body_nid` (stamped
  in `set_mode`) + a per-frame `drop_mode_tool_on_body_switch()` reconciler (called in `app_update.rs` before
  `auto_commit_episodes`) that `set_mode(Select)` when `active_body_nid` moved off the armed body — covers all
  switch paths (outliner/viewport/Tab) without touching `input_select.rs`; same-body prim reselect keeps the
  tool (body nid unchanged). `cargo check -p app` green.
- **2026-07-23 (e) — WELD-ARC ANCHOR = V: owner scene now VERTEX-EXACT (both directions).** After
  adjust_offsets v1, 2 residual verts (0.039) remained at the flat→fillet bend and the owner still saw
  "segments too straight". Root cause in `miter_weld_arc`: the profile control point used
  `closest_point_line_line(beveled edge, chord)` (INFINITE lines) — at a gentle pass-through bend the
  closest point slides onto the a–b chord → zero bow. Inverting the unit-square map on BOTH oracles
  (owner_ring bend AND the pyramid rounded miter) recovers **M = orig (V) exactly** — the old construction
  only coincided with V on the pyramid. FIX: `middle = orig` + skip the weld-plane projection when
  proj_dir ∥ plane (in-plane bend, isect degenerate). Gate tightened to VERTEX-EXACT (<4e-3) in BOTH
  directions (reverse check added — the owner's report was reverse-only, a one-directional gate missed it:
  ALWAYS diff both ways). 76 bevel gates green. LESSON: when a KB line says "project_to_edge midco",
  verify the recovered CONTROL POINT numerically from the oracle before trusting a geometric paraphrase.

- **2026-07-23 (d) — `adjust_offsets` v1 SHIPPED (sinratio chain/cycle solve).** Model validated in
  Python against the owner_ring oracle BEFORE any Rust (§0 discipline): predicted per-edge offsets
  0.2324→0.2562 across the fillet vs Blender 0.2331→0.2549 (~1%). Implementation (bevel_op.rs, after
  the clamp): at every vertex with EXACTLY 2 beveled edges, constraint w_b = w_a·sin(b,x)/sin(a,x)
  (mean over cross fan edges, both sides); constraints form paths/cycles (edge degree ≤2); per
  component propagate g_i and least-squares d = w·Σg/Σg²; per-edge widths ride `Rail.w` and thread
  through `solve_corner_lines2` / `loop_slide_meet2` / `build_boundary_flat(w_of)` /
  `partial_back_edge_slide`. Symmetric corners give ratio 1 → ALL 75 prior gates unchanged-green.
  Owner-scene gate `owner_ring_adjust_offsets_matches_oracle`: 34 divergent verts → **2** (worst
  0.0388, both = the weld-arc PLANE detail at one gentle-bend vertex). **REMAINING (v2):** per-SIDE
  offsets (Blender offset_l/offset_r) + the bend weld-arc plane → drive those 2 verts to
  vertex-exact; then BEVEL-MITER-OPTIONS (owner ask).

- **2026-07-23 (c) — OWNER "weird straight edge / not an arc" = the DEFERRED `adjust_offsets`, now
  user-visible. NEXT BEVEL TASK (superseded by (d) — v1 shipped same day).** Owner scene reproduced exactly: base.fl4 (30v/28f — a block with a
  PRIOR rounded fillet) + bevel the full z=0.5 ring (10 edges, 6 of which cross the old fillet's arc),
  offset .242 seg 3. Oracle `owner_ring_ref.py` → `owner_ring.json`: Blender nv=60/nf=58 = SAME topology
  as AZRA — but 34/60 verts diverge 0.018–0.05, worst AT the crossing corners (the owner's red line).
  Pattern: Blender's `adjust_offsets` (bmesh_bevel offset harmonization across chained/crossing beveled
  edges) moves every band edge off the nominal width so seams stay smooth; AZRA's raw per-edge uniform
  offset kinks the seam across the curved fillet into straight chords. The 2026-07-01 "adjust_offsets /
  sinratio DEFERRABLE (cosmetic)" call is hereby REVOKED for chained/crossing selections — it is the
  owner-visible defect class. TODO(next bevel task): clean-room `adjust_offsets` (spec §3 pass; Blender
  solves per-BevVert edge-offset consistency, propagating around vertices with the sinratio relation,
  then a global pass over connected beveled chains). Gate = `owner_ring.json` vertex-for-vertex.
  Diagnostic infra added this session: env-gated `diag_dump` test (app/src/project.rs) dumps any .fl4's
  poly meshes to JSON for headless replay — keep it.

- **2026-07-23 (b) — ROUNDED k=2 AT DEGREE-3 SHIPPED (the owner's actual clamp, caught on a LIVE console).**
  Attached a live console to the running app; the owner's round-bevel replay printed the new
  `[BEVEL] FLAT-CLAMP by v…` diagnostic (added this session in `unroundable_blocking`): every blocker was
  `degree=3 k=2 bev=[t,t,f]` at ORIGINAL box corners — the canonical "bevel a face's edge loop" corner,
  NOT the chamfer-created degree>3 corners the morning's gates covered. Oracle `k2d3_corner_ref.py`
  (adj2 + loop4) decoded the target: Blender's degree-3 k=2 corner is **the SAME M_NONE weld as the 2-edge
  miter** — V deleted, one BoundVert on the shared face + one slid onto the unbeveled edge, ONE shared
  profile arc, NO grid. AZRA's STEP 2b had all of it but was gated `degree > 3`. FIX = routing only:
  `miter_2edge_d3 = n_bev==2 && degree==3 && req_seg>1` joins the STEP-2b condition (seg=1 keeps the
  proven legacy flat n-gon patch — edge_loop_corner_patch gate untouched). Gates:
  `k2_degree3_adjacent_pair_rounds_matching_oracle` (nv=17/nf=12) +
  `k2_degree3_face_loop_rounds_matching_oracle` (nv=20/nf=18) — both vertex-for-vertex. 75 bevel green.
  Also NEW permanent diagnostic: the FLAT-CLAMP eprintln names vertex/degree/k/bev-pattern/pos whenever
  the clamp fires — no more silent "round blocked". METHOD WIN: live console beat guessing; the morning's
  synthetic exoneration was correct but tested the WRONG corner class — always capture the user's actual
  [BEVEL] line first.

- **2026-07-23** — OWNER "cannot add round bevel on chamfered box" → KERNEL EXONERATED, 3 new oracle
  gates. New single-stage base: `chamfered_cube()` (24-vert explicit chamfered ±1 cube, Newell-outward
  windings, byte-identical in `k1_rounded_partial_ref.py` / `k2loop_rounded_ref.py`). Gates all GREEN on
  the CURRENT kernel with NO code change: (1) `k1_rounded_partial_matches_oracle` — single edge ending on
  degree-4 chamfer corners, seg=3: vertex-for-vertex Blender match (nv=32/nf=35; V deleted, recessed
  vertex slid the offset down the back fan edge (0.894,0.7,±0.806), ns-tri fan — the flat PM2 machinery
  rounds correctly); (2) `k2_loop_rounded_on_chamfered_cube_matches_oracle` — the 4-edge top loop, k=2
  ADJACENT pairs at degree-4: nv=36/nf=38 ✓ (STEP 2b n_bev==2 handles the adjacent pair; NOT flat-clamped);
  (3) `k2_loop_rounded_with_top_diagonal_stays_round_and_watertight` — same loop with a fan-triangulation
  diagonal on the top face (the owner's actual topology, degree-5 corners): round + watertight ✓.
  **⚠ §8 CORRECTION: "edge-loop k=2 corner still flat-clamps" is STALE** — the k=2-at-degree>3 adjacent
  pair rounds via STEP 2b + M_NONE weld (gate 2), and the k=1-at-degree>3 partial rounds (gate 1). The
  remaining §9.4 gap narrows to: k=2 corners STEP 2b cannot weld + mixed corners with a (false,false)
  sector (the `unroundable_blocking` closure — M_ADJ-with-vertex-profiles). The owner's report is
  therefore APP-LAYER or STALE-BINARY (they ran a pre-rebuild exe all day — the #1 cause, §10.3). If it
  reproduces on a fresh build: capture the [BEVEL] log + toast; suspect poly_activate_bevel arm gating or
  the HUD segments never reaching params.

- **2026-07-18 (e)** — 90° STAIRCASE-ON-STROKE non-manifold: owner's [BEVEL] log (edges=4 seg=3
  manifold=false, guard-rejected every frame). Root cause found via [BBF] sector instrumentation:
  **the centroid-based inward sign-fix is WRONG on CONCAVE faces.** A knife staircase creates
  L-shaped regions whose centroid sits on the far side of an edge → `inward_offset_line` (STEP 2b)
  and STEP-1 rails flipped a CORRECT inward → both sector BoundVerts collapsed to one point →
  palindromic miter chain [A,i,i,A] → strips double-covered one arc segment (4 faces on a directed
  edge). FIX (structural, not a tweak): drop the centroid flip everywhere — `normal × edge_dir`
  ALREADY points into the face (interior = LEFT of the directed ring boundary, convex or concave;
  Newell normal is winding-consistent). Also added chain row-alignment in the per-edge arc build
  (straight-vs-crossed endpoint pairing) for S-turn miter chains. ALL 568 tests + every Blender
  oracle stay vertex-exact. Gate: `bevel_staircase_on_knife_stroke_stays_manifold` (two strokes,
  2nd anchored on the 1st, seg1+seg3). NEVER reintroduce a centroid side-test for offset
  directions — winding is the invariant.

- **2026-07-18 (d)** — KNIFE-SIDE fix that removes the §11(c) slit-edge-breaks-bevel class AT SOURCE
  (not a bevel change — logged here because it's the upstream half of (c)). Owner: "cut in the middle
  of a poly — Blender connects the 2 endpoints to the nearest vertex to support n-gon creation." The
  knife's middle-of-face cut used to embed a dangling SLIT spur (one anchor, free tip inside the face),
  which is exactly the wire/slit edge (c)'s STEP-0 skip has to defend against. Now `knife.rs`
  connects EACH free chain endpoint to ITS OWN nearest ring vertex → a boundary-to-boundary chord that
  splits the face into a clean n-gon (`split_poly_through`, two vertex anchors). So a middle-of-face
  knife cut no longer PRODUCES a slit edge; the bevel consumes the result directly. The §11(c) STEP-0
  slit skip STAYS (defends genuinely dangling knife ends — a cut deliberately stopped mid-face). The
  N-way / rounded-shared-corner `M_ADJ` gap (§9 #4) is UNTOUCHED and remains the one guarded bevel gap.
  Gate `bevel_skips_slit_edges` still green; knife n-gon gates updated (see COMPLETE_BUG_TRACKER
  KNIFEINTERIOR). 566 core_math lib + all knife/bevel suites green; `cargo check -p app` clean.

- **2026-07-18 (c)** — owner corner repro (2-wide box, oblique bottom T + corner-edge T): the CLEAN
  topology already matched Blender vertex-for-vertex at seg1 AND seg3 (oracle
  `knife_corner_bevel_ref.py` → `knife_corner_seg{1,3}_ref.json`, nv18/nf13 + nv24/nf17; oblique
  slide w/sin ✓, corner-edge slide ✓ — gate `knife_corner_bevel_matches_oracle`). The LIVE breakage
  was a **SLIT edge in the selection**: a knife stroke that misses the edge snap ends mid-face →
  `split_poly_slit` doubles the edge with the SAME face on both sides; the rail machinery sign-fixes
  both rails toward one centroid → folded self-intersecting strip. Blender parity: dangling knife
  ends are WIRE edges and bmesh bevel skips them. Fix: STEP-0 skip for same-face (slit) edges
  (`bevel_op.rs` canonicalize); gate `bevel_skips_slit_edges` (identity). 570 green.

- **2026-07-18 (b)** — KNIFE-CHAIN bevel fixed, Blender vertex-for-vertex (owner live repro: cube + 3-seg
  knife cut, bevel the chain → non-manifold → guard-rejected → "bevel does nothing"). TWO defects:
  (1) a knife BEND vertex is **degree-2** (only its two chain edges); the k≥2 cap fan-walked the
  valence-2 fan and emitted DUPLICATE fan faces ("directed edge used by 2 faces same winding"). Fix:
  route `degree==2 && n_bev==2` through STEP 2b (2 BoundVerts, delete V, NO cap) — Blender's M_NONE
  miter. (2) the bend joins two COPLANAR faces (cut across a flat face, dihedral π); `miter_weld_arc`
  bowed the profile there (~0.01 off). Fix: FLAT-miter degenerate — V on the a–b line → straight lerp
  (Blender emits the exact linear thirds). T-terminals needed NOTHING: `rail_endpoint_pos`'s loop
  slide already lands V±w on the collinear edge and the same_edge seal suppresses the cap. Oracle:
  `knife_chain_bevel_ref.py` → `knife_chain_seg{1,3}_ref.json` (nv16/nf10, nv24/nf16); gate test
  `knife_chain_bevel_matches_oracle` (both segs, vertex-for-vertex). 558 green, no oracle regressions.

- **2026-07-18** — install guard made RELATIVE (owner ruling: "bevel need to work even if the mesh is
  open or broken"). `bevel_result_installable` required `is_manifold(new)` ABSOLUTELY, so one defect
  anywhere in the mesh (e.g. the unpaired-twin slits the pre-f2ab88e symmetrize left behind) silently
  disabled bevel EVERYWHERE — the recurring "can't bevel my knife cut" reports. Now the manifold check
  applies only when the INPUT was manifold (clean meshes keep full tear protection; broken meshes are
  held to "no new boundary loops" only) — Blender parity: BMesh bevel has no global manifold gate.
  Guard test +1 (T-junction fin input must install). Related: `repair_unpaired_twins()` (f2ab88e)
  fixed the slit source; project-load heal (7487451) fixes saved scenes.

- **2026-07-17 — BOUNDARY / OPEN-FACE bevel: dead-end confirmed + the REAL open-face bug fixed (the guard).**
  Owner: "bevel do not work when we have an open face." Task premise: "Blender bevels boundary edges (slide
  the two endpoints inward along the single face, chamfer the open side)." **The oracle FALSIFIED the
  premise.** Exhaustive Blender matrix (`boundary_bevel_matrix.py`, **4.5 LTS AND 5.0 agree**): a boundary-edge
  edge-bevel is **IDENTITY** (op FINISHED, CHANGED=False) on a flat quad, a 3D open-box rim edge, the whole
  rim loop, and a hex-fan — across **every** offset type, seg 1 and 3; the raw `bmesh.ops.bevel` on a lone
  boundary edge **CRASHES** Blender. The interior-edge control on the same open box correctly changes
  (nv 8→10) → harness verified. So there is **no ground truth for a boundary chamfer** — inventing one
  violates §0 rule 1 and tears (one-sided edge → closed-mesh watertight machinery). **AZRA's STEP-0 boundary
  skip is already Blender-faithful — kept it** (§6 #25). **The actual owner bug was the INSTALL GUARD, not
  the core** (§6 #26): AZRA's `bevel_edges_offset` ALREADY bevels an INTERIOR edge of an OPEN mesh
  **vertex-for-vertex to Blender** (open box, interior edge with one endpoint on the rim: seg1 nv=10/nf=6,
  seg3 nv=14/nf=8, both **0.000000** vs `openbox_int_seg{1,3}.json`). But that legitimately reshapes the rim
  → boundary EDGE count grows (4→5 / 4→7), and the old guard `manifold && boundary_edges <= orig` **rejected
  every open-mesh bevel touching the boundary** → "nothing happens." **Fix:** new pure, tested
  `bevel_result_installable(orig,new)` = `is_manifold()` (accepts clean OPEN meshes; catches non-manifold/
  bowtie/degenerate tears — stronger than the old `validate()`) **AND `boundary_loop_count` not increased**
  (a spurious hole = a NEW loop; a faithful open bevel keeps the loop count, only the edge count moves).
  Closed meshes unchanged (loops 0 ⇔ 0 boundary edges). `poly_bevel_regenerate` now calls it; a pure-boundary
  selection toasts (honest no-op). New gates: `boundary_edge_bevel_matches_blender_identity` (the 3 task
  cases = Blender identity), `interior_edge_on_open_mesh_matches_blender` (the open-face fix lock, vertex-exact
  + loop-preserved), `bevel_install_guard_open_and_closed` (accepts closed+open, rejects synthetic tears),
  `boundary_loop_count_counts_holes`. **58 bevel gates + 522 core_math lib GREEN, zero regression;
  `cargo build -p app` clean.** Oracles: `docs/planning/poly/boundary_bevel_matrix.py`,
  `openbox_interior_ref.py`, `bnd_*`/`openbox_int_*.json`. NOTE: `bevel_apply_inverse_apply_is_identity`
  (the old standing RED) is GREEN on this base. **Owner-facing takeaway: boundary edges intentionally don't
  bevel (Blender parity); open-FACE bodies now bevel their interior edges correctly (was guard-blocked).**
- **2026-07-17 (PM) — BEVEL-NOTCH FIXED (SDF per-edge bevel, worktree cache-perf, uncommitted).** The
  radius-aware combine that §6 #18 demanded, shipped to all 3 live eval paths in one task (CPU
  `sdf_shapes.rs` first as oracle, then `sdf_prim_eval.wgsl` + `volume_bake.wgsl`, line-parallel).
  Construction (after burning the smooth-max family — §6 #25/#26): keep the tri-planar max as surface
  authority and `max` in (1) an **EXACT uniform roundbox-ρ corner term** on the folded coords, ρ = min
  effective radius of the octant's 3 edges (None→0 ⇒ sharp corners stay sharp), and (2) an **EXACT 2D
  chamfer prism SDF** (two vertex sectors added) — required for the corner term's octant-boundary
  domination bound AND a stripe reducer itself. Proven/tested properties: continuous, 1-Lipschitz
  (pairwise property gate — never overestimates its own zero set, march-safe by construction);
  all-Round-equal radii reduces EXACTLY to sdRoundBox (1e-6 gate); new field ≥ old field everywhere
  (march steps only grow); corner-ridge march distance +24.4% (probe gate — the stripe mechanism);
  surface unchanged off corner blend neighborhoods (40k-sample sign-parity gate; inside them the old
  tri-cylinder corner bulge ≤0.226·ρ is shaved to the ρ-sphere, 0 flips actually sampled at repro radii).
  Gates: `bevel_notch_{a,b,c,d}` (core_math, permanent) + the 5 existing sparse beveled-subtract GPU
  gates green with the new CPU oracle + full app suite 205/205 + graphics 44/44. Follow-up tracked:
  `sdf_box_per_edge_profiled` still bare-max (BEVEL-NOTCH-PROFILED). Owner verify: mixed-chamfer
  beveled-cube subtract → stripes gone in full deferred shading (no Solid switch needed).

- **2026-07-17 — SDF beveled-SUBTRACT "stringy vertical sheets" DIAGNOSED (different subsystem: SDF
  per-edge bevel, worktree cache-perf).** Owner: subtracting a per-edge-beveled cube carves correctly but
  the carved surface shows thin vertical sheets/stripes. **The GEOMETRY pipeline is exonerated at every
  layer** — 5 new permanent gates (worktree `feat/sparse-3a`, `tests_sparse_bake.rs` /
  `tests_gpu_classify.rs`): full classify+bake vs CPU oracle (0 flips, bit-exact), windowed-reclassify
  carve gesture (bit-exact, no leaks), bevel-slider partial + incremental dirty-tile mesh (every DC vertex
  ≤0.24 voxel from the true iso-surface), and the previously-UNTESTED O1 workgroup coarse cull
  (bit-identical, incl. the beveled-subtract scene). CPU/WGSL bevel eval formulas verified line-identical
  (sdf_shapes.rs:174-204 ≡ volume_bake.wgsl:278-298 ≡ sdf_prim_eval.wgsl:418-446). **Root-cause verdict:
  the artifact is SHADING-level, not geometry** — deferred lighting marches `soft_shadow`/`calc_ao`/
  reflections against the ANALYTICAL `scene_sdf` (deferred_entry.wgsl:471-473,542,594-606), whose
  tri-planar hard-max mixed-radii bevel field (§6 #18, the open notch bug, sdf_prim_eval.wgsl:418) has C0
  ridge surfaces parallel to the box's vertical edges + a systematic distance UNDERESTIMATE in concave
  (carved) regions — shadow/AO marching turns those into thin vertical stripe bands exactly on carved
  surfaces. Discriminator for the owner: stripes vanish in Solid/clay shading (no analytical marching) ⇒
  shading; persist ⇒ send the .fl4 (geometry claim would contradict 5 green gates). The REAL fix remains
  §6 #18's lesson (radius-aware smooth combine, never hard max — port to BOTH sdf_prim_eval.wgsl and
  volume_bake.wgsl in one task); memory says "hard, do not rush" — diagnosis shipped, fix not rushed.

- **2026-07-03 (PM3) — FLAT-SURFACE / SPLIT-EDGE bevel VERIFIED WORKING (already correct).** User flagged
  "bevel on a flat surface (split edge)" as needed. Investigated: beveling a COPLANAR edge (between two
  same-plane faces) already works and is VERTEX-EXACT to Blender — a flat edge has no dihedral, so the
  profile arc degenerates to a straight chord (the ellipse degenerate-frame guard fires: apex is the
  midpoint of the two rails → anti-parallel legs → chord), producing a FLAT strip of evenly-spaced parallel
  lines. Oracle `flat_edge_ref.json` (box top split into 2 coplanar quads, bevel the split edge, seg3):
  Blender nv=16/nf=10, strip at x=±0.3/±0.1 in the y=1 plane — OURS IDENTICAL. The hard case (a flat spoke
  whose far endpoint is a degree-N flat centre with many coplanar edges — hexagonal coin) is also watertight
  at seg 1/3. **The earlier "holes" were a malformed TEST mesh (T-junctions: split verts not referenced by
  the side faces), NOT the bevel.** Gates: `flat_surface_edge_bevel_matches_blender` (vertex-exact) +
  `flat_spoke_ngon_center_watertight`. 40 bevel gates + 373 core_math GREEN. NOTE: if a user's flat-surface
  bevel produces nothing, the likely cause is a NON-MANIFOLD / T-junction input (the app install guard
  rejects the holey result → no-op); a repro would let us add input-hygiene guarding.


- **2026-07-03 (PM2) — VERTEX BEVEL (affect=VERTICES) shipped.** New entry point `bevel_vertices_offset`
  (a must-have). Blender `build_boundary_vertex_only` (bmesh_bevel.cc:3245): one BoundVert per incident
  edge = V slid along the edge by the offset; the ring is capped (seg=1 → flat n-gon = M_POLY; seg>1 →
  M_ADJ grid). Structurally it's a corner cap with NO edge strips (edges stay sharp — only the vertex is
  beveled). Our impl reuses `circular_arc` (corner arc bulging toward V, chord at seg=1) + `WeldMap` +
  `fill_corner_grid_madj` + the (perp²,height) pole fit: for each fan edge build the arc across its face
  from BV(prev-edge)→BV(this-edge), chain into the rim, cap it; then re-trace every face at V replacing the
  V-corner with that face's (welded, shared) arc → watertight. Oracles `vbevel_cube_seg{1,3}.json`: seg=1
  VERTEX-EXACT (nv=10/nf=7, 3 BoundVerts (1,0.6,1)/(1,1,0.6)/(0.6,1,1) exact, 1 tri cap, 3 pentagons); seg=3
  exact counts (nv=19/nf=13, grid cap, 3 heptagons), grid within the fullness tolerance. Gates:
  `vertex_bevel_cube_matches_oracle` + `vertex_bevel_sweep_watertight` (cube corners deg-3 incl. all-8
  multi-select, octahedron deg-4, bipyramid deg-5/6, seg 1/2/3 — all watertight). App: Ctrl+B in **Vertex
  work mode** with a vertex selection routes to `poly_activate_vertex_bevel` → the modal carries a `verts`
  set; `poly_bevel_regenerate` branches to `bevel_vertices_offset`. Same drag controls. 371 core_math GREEN.
  v1 scope: adjacent selected verts (shared edge) get a BoundVert from each end (clamp keeps them apart) —
  no special vertex-only edge weld yet.
- **2026-07-03 (PM) — SPAN CORNERS ROUND (last flat mixed case done).** The final flat-clamped case: a MIXED
  corner (2 < n_bev < degree, seg>1) with a run of **2+ CONSECUTIVE unbeveled edges** (a (false,false)
  sector). Oracle `consec_ref.{py,json}` = pentagonal bipyramid, deg-5 apex, 3 consecutive beveled edges
  (fan T,T,T,F,F): Blender ROUNDS it (nv=30 nf=35). Studied `build_boundary` (bmesh_bevel.cc:3521): for each
  beveled edge it walks to the NEXT beveled edge and places ONE BoundVert at the `offset_meet` across the
  intervening unbeveled edges (drop-onto-face) — so the corner is a CLEAN **count-n_bev BoundVert ring
  filled by the M_ADJ grid**, NOT a flat clamp; the unbeveled edges collapse to that BoundVert. KEY: the
  across-span BoundVert replaces the original vertex V, and the two BRACKETING beveled edges' rails weld to
  it → their arcs SHARE it → clean chain (we already compute this point via `build_boundary_flat` +
  `drop_onto_faces`). Implementation = ADDITIVE + tightly gated to the span signature (which previously
  ALWAYS flat-clamped → cannot regress any existing gate): new `span_round_verts` set — STEP 2b routes span
  corners through `build_boundary_flat` (apply `boundvert_override` + `corner_boundvert`); STEP 3 collapses
  each fully-unbeveled face's V-corner to the across-span BV (beveled faces keep their rail corner); STEP 5
  rim SKIPS the V-insertion at the span (arcs already share the BV); `unroundable_blocking`/`clean_apex`
  accept span corners. **Result:** across-span BoundVert VERTEX-EXACT (0.0000), counts exact (nv=30/nf=35,
  12 apex verts), boundary arc-mids ≤3e-3; interior ≤1.6e-2 (same adj_vmesh fullness-path class as the
  hex-cone / k=3 gates — NOT superellipsoid-snap-exact, a consistent documented limitation). Gates:
  `span_corner_consecutive_unbeveled_rounds` (oracle) + `span_corner_sweep_watertight` (18 configs: n=5/6/7
  bipyramids × contiguous k × seg 2/3/4, all watertight+rounded). 36 bevel gates + 369 core_math GREEN,
  zero regression. **The whole edge-bevel corner family now rounds Blender-faithfully** (cube corners, edge
  chains/cap loops, degree>4 apexes, and every mixed corner k=1/k=2/k=3/span). Remaining bevel-MATH work is
  feature-expansion: vertex bevel, flat-surface/split-edge bevel, superellipse profile (shape≠0.5), miters.
- **2026-07-03 (AM) — MIXED k=2 CORNERS VERTEX-EXACT (drop-onto-faces fixed, two winding traps).** The
  k=2-ADJACENT mixed corner (2 beveled + 2 unbeveled edges at a degree-4 vertex — octahedron oracle
  `mixed_k2d4_ref.json`) already had Blender's exact TOPOLOGY through STEP 2b + the M_NONE weld chain
  (nv=17/nf=20 — the "spine + ladder" structure IS count=2 M_NONE: BV_meet + BV_wrap bridged by the
  welded profile, strips laddering onto it). Only the WRAP BoundVert was wrong: the skew rail meet
  floated OFF-surface (|x|+|y|+|z| = 1.33) because `drop_onto_faces` never accepted a face — TWO
  winding traps stacked:
  1. The wedge FACE was resolved by the `he(fan[k+1]).face` shortcut, which assumes a fan winding that
     inverts on some meshes → tested the WRONG face. Fix: resolve the SHARED face of the two fan edges
     directly (checks both sides of each half-edge — winding-free).
  2. `point_between_edges`' signed wedge promotion (`cross·f_no`) depends on fan handedness (the same
     §11 reflex-guard trap from the 2-edge miter): a 60° face corner promoted to a 300° wedge that
     ACCEPTED the un-dropped float at the FIRST step. Fix: orient the wedge so it contains the FACE
     CENTROID (the face interior defines the wedge — winding-free by construction).
  With the drop fixed, the wrap BV lands at Blender's exact perpendicular projection
  ((−0.0544,−0.0544,0.8911), on-surface) and the EXISTING `miter_weld_arc` chain lands exactly on
  Blender's weld spine — ALL 4 oracle verts at 0.0000. Also verified: the k=1 ROUNDED partial at
  degree-4 (`mixed_k1d4_ref.json`, Blender M_TRI_FAN from the on-edge BV) matches our delete-V +
  recessed-center path vertex-for-vertex (nv=14/nf=17). Gates `mixed_k2_adjacent_deg4_matches_oracle`
  + `mixed_k1_deg4_rounded_matches_oracle`.
  **k=3-of-4 DONE same day (PM):** the 2<k<degree corner with ISOLATED unbeveled edges now rounds —
  no new geometry code needed: the flanking rails' terminal slides already produce the on-edge
  BoundVert ((0,−0.1633,0.8367) EXACT), the meets are exact, and the rim is a clean 3-arc chain the
  M_ADJ grid fills. Changes were pure RE-GATING: (1) `unroundable_blocking` allows k≥3 mixed corners
  with no two consecutive unbeveled fan edges (a (false,false) sector would insert V into the rim);
  (2) `clean_apex` extended with the same predicate; (3) `fill_corner_grid_madj`'s sphere-snap gate now
  requires the rim to actually LIE on the fitted sphere (mixed rims aren't spherical — Blender routes
  them through adj_vmesh/fullness; snapping would distort). Oracle `mixed_k3d4_ref.json`: nv=29/nf=33
  EXACT counts, BVs exact, interior ≤6.3e-3 (fullness-path class). Gate `mixed_k3_deg4_matches_oracle`.
  **REMAINING mixed tail (small):** corners with TWO+ consecutive unbeveled edges (k≥2 with a
  (false,false) sector) stay flat — need Blender's vertex profiles between on-edge BVs.
- **2026-07-02 (PM9) — ARC-RIM SPIKE FIXED (ellipse profile) + DEGREE>4 APEXES ROUND.**
  1. **The arc-rim re-bevel spike is FIXED at the root.** Diagnosis chain: the terminal rail legitimately
     loop-slides ~10× the width along a 174°-near-collinear neighbour (Blender does the same); the OLD
     `circular_arc` (slerp + radius-lerp) then swept OUTSIDE the asymmetric-leg parallelogram → verts 0.47
     off-surface + a self-intersecting face. **FIX: `circular_arc` is now the quarter ELLIPSE in the
     parallelogram frame (c = a_l + a_r − apex; p(θ) = c + cosθ·ua + sinθ·ub) — Blender's
     `make_unit_square_map` profile, the same form the chain-weld oracles decoded.** Bounded by
     construction; identical to the old circle for symmetric perpendicular legs → ALL vertex-exact
     oracles unchanged. Degenerate-frame guard: legs (anti)parallel within ~3° (apex on the a_l→a_r
     line — a rail slid past the vertex) → flat chord (the collapsed ellipse would sweep through its
     centre and back = self-crossing arc). Gate `bevel_on_rounded_bevel_arc_rim_spike` un-ignored +
     `bevel_on_beveled_round_app_faithful` upgraded to a ROUNDED (seg-3 shape-0.5) stage-1 base — every
     edge of a rounded bevel now re-bevels clean. **KNOWN LIMITATION (tracked):** composing shape-1.0
     bevels (convex ×2 overshoots the face planes) can yield ~2e-4-deep twisted strip quads at
     overshoot-inverted frames — watertight/manifold/bounded, sub-visual; circular profiles immune.
  2. **Degree>4 fully-beveled apexes now ROUND** (pentagon/hexagon cones, N-pole tips): relaxed the three
     valence gates (STEP 2b `round_apex` degree==4→>=4; STEP 2.5 `k==degree && degree<=4`→`k==degree`;
     STEP 5 `clean_apex` drop `d<=4`) — the M_ADJ grid + `sabin_gamma` were already valence-general.
     Hex-cone oracle `hexcone_ref.json`: nv=48/nf=38 EXACT count match; inner 6-ring within 7e-3 (the
     adj_vmesh fullness path — close, not snap-exact; the seg-3 centre n-gon = Blender's odd-seg centre
     HEXAGON, don't mistake it for a flat M_POLY cap). Gate `nway_degree6_apex_rounds`.
     31 bevel gates + 350 core_math GREEN.
- **2026-07-02 (PM8) — ROUNDED EDGE CHAINS / CAP LOOPS DONE, VERTEX-EXACT (STEP 2c).** User reports: "top
  cylinder doesn't round" + "multi-select edges don't round". Both are the SAME case: a degree-3 vertex
  with k==2 beveled edges (edge chain / cap loop) — Blender's 2-BoundVert WELD, which our STEP 2.5 clamp
  force-flattened (`k>=2 && k!=degree`). Fix = **STEP 2c** in `bevel_op.rs`: STEP 2 already produces both
  BoundVerts (shared-face rail MEET via `solve_corner_lines` + terminal SLIDE onto the unbeveled third
  edge); bridge them with the shared profile chain in `miter_arc_pos` (the SAME M_NONE infrastructure as
  the degree>3 rounded miter — `build_at` hands both edges the same welded chain → strips twin → watertight
  by construction), add the vertex to `rounded_2edge` (un-clamps + skips the STEP-5 cap, which would
  double-cover). **KEY PROFILE FACT (do not re-derive): the weld chain is the ELLIPSE of Blender's
  unit-square profile map — c = A + B − V, p(θ) = c + cosθ·(A−c) + sinθ·(B−c), θ∈[0,π/2] — NOT
  `circular_arc` (slerp + radius-lerp): the chain's legs are UNEQUAL (meet ≠ slide distance) and only the
  ellipse matches Blender** (cube chain (0.85,−0.85,0.95981) exact; cylinder (0.89647,0,0.97321) exact).
  Guard: fires only when the 4 rail endpoints at the vertex reduce to exactly 2 welded positions (skewed
  corners where the two slides disagree keep the flat clamp — Blender's `offset_on_edge_between`
  compromise slide is future work). Gates: `rounded_chain_cube_matches_oracle` (nv=17/nf=12 + 4 verts
  exact vs `chain2_cube_ref.json`) + `rounded_cap_loop_cylinder_matches_oracle` (12-gon prism rim loop,
  nv=60/nf=50 + 4 verts exact vs `caploop_cyl_ref.json`). 28 bevel gates + 347 core_math GREEN.
  **NEW BUG EXPOSED (pre-existing, tracked):** beveling an ARC-RIM edge of already-ROUNDED geometry spikes
  (bbox +0.41 at w=0.08, one face self-intersects; watertight+manifold so the app guard doesn't catch it).
  Reproduces with a single rounded stage-1 edge → NOT a chain regression; the old gate never saw it because
  its 2-edge stage-1 was flat-clamped until today. Repro test `bevel_on_rounded_bevel_arc_rim_spike`
  (#[ignore]); `bevel_on_beveled_round_app_faithful` stage-1 pinned to segments=1 (its original effective
  coverage, unchanged).
- **2026-07-02 (PM7) — THE M_ADJ CONVERGING QUAD GRID IS DONE — VERTEX-EXACT AT ANY SEG.** The full
  Blender corner algorithm is now implemented (`fill_corner_grid_madj` + `madj_canon` +
  `madj_cubic_subdiv` + `sabin_gamma` + `madj_fullness` + `arc_lerp_3pt` in `bevel_op.rs`), replacing
  the triangle fan for clean apexes. Method: studied `bmesh_bevel.cc` (clean-room, algorithm-level) +
  built a Python pilot (`madj_sim.py`, archived here) verified against cube oracles seg 3/4/5
  (`rc3/4/5.json`) to 5e-6 / 9e-6 / 4.5e-4 BEFORE porting. The pipeline (§3.7 now definitive):
  coarse control mesh (BVs + arc mids + seeded centre) → Levin cubic subdivision (NOT textbook CC:
  boundary −1/6 smoothing rides the profile curve; interior vert = E_avg − ¼F_avg + ¼old; centre =
  E_avg − γF_avg + γold, γ=sabin_gamma(n)) → arc-length-frac bilinear interp to exact seg
  (`interp_vmesh`) → snap to fillet sphere (3-sector circular corner only) → faces = n sectors ×
  ns2·(ns2+odd) quads + odd centre n-gon / even shared centre vert.
  **THE 3 CRACKED SECRETS (never re-derive):**
  1. **The coarse centre seed is 1.4×** the pole height for a 3-sector circular corner with seg>2
     (`make_cube_corner_adj_vmesh`; ≤2 → pole itself). Without it the inner ring lands 1.7° too low
     (t=0.746 vs 0.778) — this single constant was the whole "fan vs Blender" visual gap.
  2. **Path split:** n==3 & circular → tri-corner path (sphere seed, `while` ≥0 subdivs, SNAP);
     n≥4 or shape≠0.5 → `adj_vmesh` path (centre = BV-centroid + fullness(seg)·(orig−centroid),
     fullness = circle table [.559,.642,.551,.646,…], `do-while` ≥1 subdiv, NO snap).
  3. **Blender's seg-3 rim quirk:** its corner boundary is chord-lerped from the power-of-2 grid then
     sphere-snapped → 29.956° not 30.000° (2e-4). We deliberately use the exact arcs (the intended
     limit curve); gates tolerate the 2e-4.
  Gates: `rounded_cube_seg3_matches_oracle` (12 corner verts vertex-exact, 6 quads + centre tri,
  exactly 12 verts = no fan pole), `rounded_cube_multi_seg_watertight` (seg 2–6 watertight + manifold +
  interior on the fillet sphere). 26 bevel gates + 345 core_math GREEN. Fan kept only as fallback for
  malformed rims. Remaining (unchanged scope): re-beveled k<degree corners still flat-clamp; degree>4
  apexes not yet routed (gate `d<=4`).
- **2026-07-02 (PM6) — CORNER NOW VERTEX-EXACT (seg=2) + PARALLEL EDGES + EXACT POLE (all segs).** The user
  showed the AZRA corner (triangle-fan spider) vs Blender (clean quad grid) — "check blender for truth." Ran
  the oracle process on the ±1 cube (`rounded_cube_ref.json`, seg=2). Three vertex-for-vertex findings, all
  fixed:
  1. **Arc-mids bulged OUT of the fillet cross-section** → non-parallel edges (the user's 14/16 bug). Root:
     `circular_arc`'s apex was the sharp vertex `orig_pos[v]`; for a lone edge the rails share that vertex's
     edge-plane (no-op), but at a **fully-surrounded corner** the corner-solve trims the rails INWARD off that
     plane, so bulging toward the 3D vertex pulled the mid off-plane (mine (0.746,0.873,0.873) vs Blender
     (0.7,0.912,0.912)). Fix: **re-project the apex into the cross-section plane** (⊥ edge through the rail
     midpoint) — gated to vertices with **≥3 beveled edges** (`bev_count`), because simple/loop-slid miter
     vertices are NOT trimmed and must keep the un-projected apex (regressed `rounded_2edge_miter` until gated).
  2. **Center pole off** (0.870 rho-avg vs Blender 0.873). Root: the perp-radius average mixes BV and mid
     radii. Fix: the whole rim lies on the corner's fillet SPHERE whose centre is on the axis by symmetry →
     **fit the circle in the (perp², height) plane** (robust 2×2 LSQ; the 3D sphere fit is ill-conditioned for
     a shallow cap and overshot to 0.883). Pole = rc + up·(c_h + r) = **0.8732 exactly**.
  3. **Fan topology** (my corner) vs **quad grid** (Blender). For seg=2 the corner is `count` QUADS
     `[BV, mid, center, mid]` (verified vs faces f18/19/20) — implemented (parity by rim height picks the
     BoundVerts). New gate `rounded_cube_seg2_quad_grid` is VERTEX-EXACT (BVs, mids, pole, quad faces).
  **Result:** seg=2 corner matches Blender vertex-for-vertex; **all segs** now get parallel arc-mids + exact
  pole (the spike + non-parallel complaints are gone). 24 bevel gates + 343 core_math GREEN, zero regression.
  **REMAINING:** seg≥3 still fills with a FAN (parallel + domed + watertight, but not the quad grid). Mapped
  the seg=3 oracle (`rc3.json`): all 12 corner verts on the fillet sphere; structure = boundary(9) → inner
  ring(3, each on the **BV→pole great circle**, at slerp≈0.77 — Blender's Catmull-Clark ring weight, not yet
  reverse-engineered) → **center n-gon** (triangle), 6 quads + 1 n-gon, NO fan. Building the general
  converging grid (odd/even rings + exact ring weights) is the next step. See `rc3.py`/`rounded_cube_ref.py`.
- **2026-07-02 (PM4) — M_ADJ STEP 1: degree>3 clean apexes now ROUND (re-route off STEP 2b).** The v1 dome
  (PM3) rounded only degree-3 corners; degree>3 fully-beveled apexes (the pyramid apex) still flat-clamped
  because STEP 2b intercepts degree>3 vertices and COLLAPSES them to flat BoundVerts (so `n_seg>1` there
  opened a 9-edge hole). Fix: for a ROUNDABLE clean apex (`n_bev==degree && degree==4 && req_seg>1`), SKIP
  STEP 2b (`round_apex` guard) so it flows through STEP 2/3/5 like a degree-3 corner — its edges keep their
  arcs, STEP 5 builds the 12-vert rounded boundary loop, and the dome caps it. seg=1 (flat N-way) still uses
  the STEP-2b M_POLY collapse. Un-clamp widened to `k==degree && degree<=4`. **KEY FIX:** the dome gate was
  `rim_is_convex` (2D-projection convexity) which WRONGLY rejected the apex — its arc-interior verts bulge
  toward the apex so the projected rim is STAR-shaped (arcs at smaller radius than the BoundVerts). Replaced
  with the correct TOPOLOGICAL gate `clean_apex` (k==degree≤4, fully beveled) → the apex domes, bevel-on-
  beveled (k<degree) stays flat. Gate `rounded_nway_apex_domes` (pyramid 4-apex seg3 → watertight + domed,
  was boundary=9). 342 core_math GREEN, zero regression. `rim_is_convex` removed (dead). REMAINING: the dome
  interior is still a single bulged fan (v1), NOT the exact Catmull-Clark 2-ring grid (§3.7 decoded target);
  and re-beveled (k<degree) corners still flat. Next: the exact grid interior.

- **2026-07-02 (PM3) — ROUNDED MULTI-EDGE CORNER v1 (dome) — the user's "circular multi-edge round bevel."**
  Shared corners were GLOBALLY flat-clamped (STEP 2.5 `n_seg→1`) so rounding a corner where 3+ edges meet
  rendered flat. Shipped a **v1 dome** (NOT the full Catmull-Clark M_ADJ — that's still future): STEP 5's
  flat corner cap, when `n_seg>1` and the rim is CONVEX (`rim_is_convex`), fans the curved profile-arc rim
  to a center **bulged toward the original vertex** (`shape·(orig−centroid)`) → a rounded dome. Watertight
  (rim shared with the strips), gated simple/outward. **The hard part was the DISCRIMINATOR** (found
  empirically, test-driven): lift the flat clamp ONLY for a **clean degree-3 fully-beveled corner**
  (`k==degree==3` — a cube corner or a cap edge-loop vertex). Everything else stays flat: (a) a degree>3
  fully-beveled vertex is a STEP-2b MITER — it collapses to flat BoundVerts, so `n_seg>1` there opens holes
  (boundary=9/11); (b) a PARTIALLY-beveled corner (`k<degree` — bevel-on-beveled) has a NON-convex rim that
  self-intersects when rounded (verified 8/36 self_x + bbox-spike). Both need the real M_ADJ grid.
  **METHOD that cracked it:** disable-and-isolate — stashed to confirm the baseline passed, disabled the
  dome to prove the CLAMP (not the dome) broke `bevel_on_beveled_round_app_faithful`, then narrowed the
  clamp-lift to the one safe class. Gate `rounded_cube_corner_domes` (round bulges toward the corner more
  than flat + watertight). 341 core_math GREEN, ZERO regression (all flat gates + the degree>3 miter/
  bevel-on-beveled gates untouched). Oracle `nway_pyramid_seg3_ref.json` (nv=32/nf=26) captured the full
  M_ADJ grid structure for the vertex-exact future work. **REMAINING (§9):** vertex-exact Catmull-Clark
  M_ADJ for degree>3 miters + re-beveled + the exact grid (this v1 is visual, not Blender-vertex-identical).

- **2026-07-02 (PM2) — K=1 PARTIAL CORNER AT A degree>3 FAR-ENDPOINT FIXED (the octahedron-base hole).**
  Beveling all 4 +Y-apex edges of an octahedron capped the apex correctly (N-way M_POLY, untouched) but
  opened a **9-edge hole at the 4 BASE verts** — each degree-4 with exactly ONE beveled edge (a k=1 partial
  corner at a degree>3 far-endpoint). Before: nv=13/nf=14/boundary=9. **ROOT CAUSE:** the whole partial-corner
  machinery (cap_needed → recessed center → fan) was gated on `n_seg>=2`. At seg=1 the FLAT k=1 chamfer got
  NO cap, and the (false,false) back faces have no `same_edge` seal at degree>3 (the un-beveled back faces are
  separated from the beveled edge by other fan edges → nothing bounds the chamfer arc → hole). At degree-3
  the third face `same_edge`-seals the whole arc, so the seg=1 flat corner was always fine — the bug only bites
  degree>3. **FIX (3 surgical edits in `bevel_op.rs`, zero half_edge.rs):** (1) compute `has_same_edge_seal`
  for ALL k==1 (was `n_seg>=2` only) and cap iff `k==1 && !seal && (n_seg>=2 || degree>3)` — adds the FLAT
  degree>3 partial corner, leaves flat degree-3 (seal=true → no cap) and all round cases unchanged; (2) new
  `partial_back_edge_slide()` — Blender `offset_on_edge_between`: the deleted-vertex merged center slides off V
  by EXACTLY the offset `w` along the unique "back" fan edge (the non-beveled edge whose BOTH incident faces
  are un-beveled). At the degree-4 base this is X↔−Y → center `V + w·normalize(far−V)` = oracle
  (0.8232,−0.1768,0) EXACTLY. Returns `None` (→ prior 0.3 arc-centroid fallback) when no unique back edge
  (some round bevel-on-beveled corners); (3) STEP-5 rim gate `<3`→`<2` for a partial corner (a flat 2-point
  rim fans to C as one triangle). **RESULT:** octahedron bevel-4-apex nv=17/nf=17/boundary=0, matches Blender
  `nway_octa_ref.json` vertex-for-vertex (max dev <2e-3). Gate `octahedron_nway_matches_oracle` (watertight +
  17-vert oracle diff + 4 recessed centers). **337 core_math GREEN, zero regression** — the degree-3 partial
  gates (`bevel_on_beveled_watertight`, `bevel_on_beveled_round_app_faithful`), `cube_single_edge_bevel_golden`,
  and `nway_pyramid_matches_oracle` (N-way M_POLY cap untouched) all still green. The center-formula change is
  guarded to `None`-fallback so the round bevel-on-beveled corners keep their watertight arc-centroid center.
  **REMAINING:** rounded N-way / shared corner (seg>1) still flat-clamps — the M_ADJ grid (§3.7) is next.
- **2026-07-02 (PM2) — ROUNDED 2-EDGE MITER SHIPPED (M_NONE weld) — matches Blender vertex-for-vertex.**
  Lifted STEP 2.5's flat clamp for exactly-2-BoundVert miter vertices so a ROUNDED (seg>1) bevel of two
  edges meeting at a degree>3 vertex renders a rounded corner instead of a flat chord. New `miter_weld_arc`
  bridges the two flat BoundVerts (A,B) with the co-planarized circular profile arc (`ns−1` SHARED interior
  verts, ZERO corner grid — Blender's M_NONE weld, §3.7); both edges' strips reference the SAME welded chain
  (WeldMap dedups → watertight). **The derivation is the story (§6 #23/#24):** the arc is a circle through
  A,B but its radius/arc-angle is GEOMETRY-DEPENDENT and Blender does NOT sample even-angle — a long session
  fitting circles+spacing dead-ended at ~5–24e-3 (fails the 2e-3 gate). The fix was to **reimplement
  Blender's actual C construction verbatim** (fetched `bmesh_bevel.cc`): `set_profile_params`
  (`middle=project_to_edge(bevel_edge,A,B)`), the unit-square map `co=x(mid−A)+y(mid−B)+(A+B−mid)`, then the
  KEY step — `calculate_profile_segments` PROJECTS `co` ALONG the beveled-edge direction onto the weld plane
  (`plane_no=(orig−A)×(orig−B)`, `move_weld_profile_planes`). That projection (not any circle fit) yields
  err **0.0** on both a std AND a tall pyramid. **ORACLE METHODOLOGY (per the flat N-way lesson):** a
  single-stage **square pyramid**, bevel the 2 OPPOSITE apex edges (apex→v2, apex→v4) — both miter sectors
  are between==2 loop-slides whose FLAT BoundVerts AZRA already matches Blender EXACTLY, so the only new
  thing is the rounding. AZRA out nv=14 nf=11 == Blender `rounded_2edge_ref.json` EXACTLY (arc chain
  (±0.1369,0.7261,±0.1369) endpoints + (±0.0501,0.8264,±0.0501) shared interior). Gates:
  `rounded_2edge_matches_oracle` (watertight + 14-vert oracle diff + 2 interior arc verts) +
  `rounded_miter_mixed_with_blocking_corner_watertight` (a miter mixed with a BLOCKING corner flat-clamps
  globally → the STEP-2b chain is DROPPED, `n_seg!=req_seg → miter_arc_pos.clear()`, so strip/arc lengths
  stay consistent → watertight flat fallback). All bevel gates GREEN, zero regression (single-edge/flat-
  miter/N-way untouched: n_seg=1 or >2-BoundVert → no chain). **SCOPE:** ONLY the 2-edge weld (M_NONE); the
  count≥3 rounded M_ADJ grid and the degree-3 edge-loop k=2 grid stay flat-clamped (§9 point 4).
  **BRANCH NOTE:** this landed on `feat/bevel-rounded-2edge` off `feat/poly-foundation`@`f998f2d` (the flat
  N-way commit — what "just landed" at branch time); two later fixes (`d706fda`, `55743b4` — the octahedron
  k=1 far-endpoint hole) were added to `feat/poly-foundation` concurrently and are NOT on this branch.
  **PRE-EXISTING RED:** `bevel_apply_inverse_apply_is_identity` (a randomized inverse/undo proptest) fails on
  a persisted regression seed — verified failing IDENTICALLY on the clean base `f998f2d`, independent of this
  weld (see §8 note + `KNOWN_bevel_inverse_regression_seeds.txt`). Oracle: `rounded_2edge_ref.{py,json}`.
- **2026-07-02 (PM) — N-WAY MITER (flat) SHIPPED — matches Blender vertex-for-vertex.** Un-gated STEP 2b
  from `n_bev==2` → `n_bev>=2` (so `build_boundary_flat` builds the ordered BoundVert ring for k≥3 too) and
  added **STEP 5b** — an **M_POLY n-gon cap** over the ring for miter verts with >2 BoundVerts (Blender's
  `build_vmesh` M_POLY at seg=1). Each ring position welds to the SAME index the strips already reference
  (WeldMap dedups by position) → the cap twins the strips; oriented outward by Newell-vs-vertex-normal.
  **ORACLE METHODOLOGY FIX (important):** a *two-stage* bevel oracle is UNRELIABLE — AZRA's stage-1 already
  diverges from Blender at the miter, so stage-2's input mesh differs → no vertex-for-vertex compare. Used a
  **single-stage mesh with a natural degree>3 vertex** (identical in both tools): a **square pyramid** (apex
  degree-4, base corners degree-3 = the known-good k=1 partial), bevel all 4 apex edges. AZRA output nv=12
  nf=10 == Blender `nway_pyramid_ref.json` nv=12 nf=10 EXACTLY (apex BoundVerts (±0.2739,0.4523,0)/
  (0,0.4523,±0.2739), M_POLY quad cap, octagon base). Gate `nway_pyramid_matches_oracle` (watertight +
  12-vert oracle diff + quad-cap assert). 336 core_math GREEN, zero regression (2-edge miter untouched:
  n_bev==2 still hits `bvs.len()==2` → no cap, identical). **DIAGNOSTIC LESSON:** first tried an OCTAHEDRON
  (all verts degree-4) — the apex N-way cap was correct (f=[boundverts] present, none of its edges on the
  boundary) but a 9-edge hole remained at the BASE because beveling the apex edges makes all 4 base verts
  **k=1 partial corners at degree-4**, a SEPARATE unfixed interaction; the pyramid's degree-3 base corners
  isolate the pure N-way. **REMAINING:** (a) k=1 partial corner at a degree>3 far-endpoint (the octahedron
  base hole — bevel-on-beveled N-way may hit it); (b) ROUNDED N-way (seg>1) — STEP 5b is seg=1 M_POLY only;
  the M_ADJ grid (§3.7) is next. Oracles saved: `docs/planning/poly/nway_pyramid_ref.{py,json}` (+ octa).
- **2026-07-02 — ROUND-BEVEL `M_ADJ` STUDY persisted (§3.7); N-way + round-bevel set up, NOT yet coded.**
  Completed a full clean-room extraction of Blender's rounded-corner VMesh (`M_ADJ`, `bevel_build_rings` /
  `adj_vmesh` / `cubic_subdiv`) into **§3.7** — the `(i,j,k)` half-ring grid, canonical/equiv seam-free
  stitch, odd/even center, the fullness-seeded N-sided-hole Catmull-Clark fill (Levin 1999), and the
  **key correction**: a *rounded 2-edge* corner is **M_NONE weld** (midpoint-bridged arcs, no grid), the
  grid is ONLY for `count≥3` + `seg>1`. This unblocks §9 point 2. **Deliberately did NOT code the N-way or
  round-bevel this session:** both are oracle-gated (§0 rule 1) and the two-stage degree>3 Blender reference
  isn't generated yet — a guard-rejected un-oracled change is the §6 #2 "renders but no-ops" trap. The
  §9-point-1 plan (un-gate STEP 2b `n_bev==2`→`>=2`; `build_boundary_flat` already returns the ordered
  BoundVert ring + `corner_boundvert` for k≥3; add an **M_POLY n-gon over `boundverts`** in STEP 5 for
  `len>2`, orient by vn/Newell to twin the strips; STEP 5 currently `continue`s on miter verts at line ~423)
  is confirmed against the code and stays the next focused task. (Non-bevel work this session: split-cap
  edge-loop fix + poly create-param panel — see git log, not logged here.)
- **2026-07-01 (PM5) — DROP-ONTO-FACE + TERMINAL-EDGE SLIDE shipped; user's flat-bottom FIXED.** Implemented
  the general solver from the PM4 study. TWO changes: (1) `drop_onto_faces` + `point_between_edges` — for
  ≥2 between-edges the floating skew meet is re-pinned onto the first intermediate face whose wedge accepts
  it (miter now matches Blender). (2) **TERMINAL-EDGE loop slide** in `rail_endpoint_pos` — a k=1 endpoint
  whose adjacent in-face edge is non-beveled now SLIDES the rail onto that edge (`v + adj_dir·w/sin`) instead
  of the plain perpendicular `v+dir·w`. Made it UNIVERSAL (not degree-gated): sin(90°)=1 so orthogonal cube
  corners are unchanged (golden green), but tilted-face endpoints (a bevel landing on a prior chamfer) stay
  ON the surface. **KEY DIAGNOSIS:** the user's reported "verts 4,5 too high" was NOT the miter (which the
  drop fixed, matching Blender v10/v13) — it was the k=1 TERMINAL EDGE at the bottom, lifting to y=-0.423
  via the plain offset. The slide pins it to y=-0.5 (Blender v8/v9 (0.013,-0.5,0.232)/(-0.232,-0.5,-0.013);
  ours (0,-0.5,0.232)/(-0.232,-0.5,0), within 0.013) → the bottom pentagon is FLAT. Gate
  `terminal_edge_stays_on_bottom` + `miter_loop_slide_matches_oracle` + `multi_edge_pairs_watertight`.
  329 core_math GREEN, golden single-edge unchanged. LESSON: "the float" had TWO independent sources — the
  miter skew-meet AND the terminal-edge plain-offset — on separate code paths; fixing one didn't fix the
  other. This is why the unified boundary solver (every corner through the same offset/slide/drop) matters.
- **2026-07-01 (PM4) — Blender complete-solver study + design (ultracode workflow).** Extracted Blender's
  full corner solver. The DESIGN (build_boundary_flat = the complete corner solver, Blender's decision tree):
  per consecutive beveled pair (e1,e2) with the fan edges between them classified by `eh_on_plane`
  (`|dot(fprev.no,fnext.no)±1|<1e-4`):
  - **0 between-edges** → `offset_meet(edges_between=false)` = our coplanar rail meet. (adjacent shared face)
  - **exactly 1 not-in-plane crease** (+loop_slide) → `offset_on_edge_between` = our `loop_slide_meet`.
  - **≥2 between-edges** (or loop_slide off) → `offset_meet(edges_between=true)`: the floating 3D meet
    (skew → midpoint) **then DROP-ONTO-FACE** — project onto each intermediate face plane, accept the first
    where `point_between_edges` (signed angular wedge test vs f.no, 2° slack) passes. **This drop is the
    missing piece for the 2-between float** (`isect_kind==2` skew → Blender re-pins onto a real face).
  - only-in-plane between-edges → collapse to the 0-between meet.
  Thresholds: 2° (`BEVEL_EPSILON_ANG`, parallel/anti-parallel + wedge slack), 10° (`BEVEL_SMALL_ANG`,
  coplanar-face skip in the drop), 1e-4 (`eh_on_plane`). N-way (count>2): M_POLY n-gon cap over the ring.
  **`adjust_offsets`/sinratio DEFERRED** — cosmetic width-uniformity for loop-slide chains, not correctness
  (Blender's own doc). Fills our whole miter solver; oracle for the current case is `miter_2between_ref.json`.
- **2026-07-01 (PM3) — 3rd corner topology found: TWO-BETWEEN-EDGE miter (uncovered).** User reported the
  miter verts still lift off the bottom plane. Reproduced their exact case (±0.5 app cube, first bevel edges
  (2,3) w=0.268, second (15,29) w=0.2): the shared vertex v3=(-0.5,-0.232,0.5) has fan
  **[bev, bev, nonbev, nonbev]** — the two beveled edges are adjacent one way, but the OTHER sector has
  **2 non-beveled edges** between them. `loop_slide_meet` only fires for *exactly one* between-edge
  (`between==2`); this `between==3` sector falls through to the floating rail meet → verts float to y=-0.423.
  **Blender oracle** (`docs/planning/poly/miter_2between_ref.json`, r1 mesh `miter_2between_r1.obj`) keeps a
  FLAT 5-vert bottom (y=-0.5); AZRA has only 3 on the bottom. **User + Blender agree; AZRA is wrong.**
  LESSON: this is the 3rd distinct corner topology after shared-face + single-between — each targeted patch
  reveals the next gap. DECISION: stop patching corner-by-corner; complete Blender's `build_boundary` +
  general `offset_meet` (edges_between with the drop-onto-face for ANY between-count) as the proper boundary
  stage (Codex's plan, executed as an in-place migration — see §9). The oracle for this case is captured.
- **2026-07-01 (PM2) — LOOP SLIDE added.** The 2-edge miter worked but the miter verts FLOATED at the 3D
  rail-meet midpoint — overshooting *outside* the ±1 cube (z=-1.006) at the wrong height (y=-0.739). Blender
  pins them onto the intervening non-beveled edge (`offset_on_edge_between`, "loop slide", on by default).
  Implemented `loop_slide_meet` (clean-room): with exactly ONE between-edge `emid`, slide the BoundVert along
  `emid` by `0.5·(w/sin a1 + w/sin a2)` (a1,a2 = the two beveled edges' angles to emid). Now miter verts land
  at (0.889,-0.75,-1)/(1,-0.75,-0.889) vs Blender (0.891,-0.75,-1)/(1,-0.75,-0.891) — **within 0.002, on the
  cube faces**. Oracle `docs/planning/poly/miter_twostage_ref.json`; gate `miter_loop_slide_matches_oracle`
  (+ no-overshoot assert). 328 core_math GREEN. **GOTCHA (logged):** Blender's reflex guard
  `cross(d1,d2)·v_no<0` is tied to its *outside*-CCW disk order; our `vertex_fan` is *inside*-CCW so the sign
  inverts and wrongly rejected the convex miter — dropped it (parallel/straight/sin guards suffice; a reflex
  miter just falls back to the meet). Same handedness lesson as `offset_meet` (§11 PM).
- **2026-07-01 (PM) — MULTI-EDGE MITER FIXED (2-edge).** Implemented Blender's `build_boundary`/`offset_meet`
  as a new **STEP 2b** (`build_boundary_flat` + `inward_offset_line` + `isect_lines_3d` in `bevel_op.rs`).
  Test-first: `offset_meet` primitive verified against the oracle (BoundVerts (0.8,0.8,1)+(1,1,0.8)), then
  the ring walk verified on the real cube mesh, then integrated. The key was a **3D rail meet** (our
  `solve_corner_lines` was 2D-in-plane only, so it never met the two rails when they lived on different
  faces — the degree>3 miter). At a miter vertex V is deleted and replaced by the BoundVert ring; STEP 2
  welds each rail to its BoundVert, STEP 3 replaces the 4-case corner with one `corner_boundvert[ce]` lookup
  (sector BoundVert, covers the "between" faces), no cap (M_NONE). **Result:** pair (9,29) 17→14 verts,
  boundary **4→0**; new gate `multi_edge_pairs_watertight` (all interior pairs, flat+round) GREEN; 327
  core_math tests GREEN, zero regression. SCOPED to k==2 & degree>3 (see §6 #16/#17 RESOLVED). Remaining
  gap: **N-way miter** (3+ beveled edges at one re-beveled vertex → Blender M_ADJ/M_POLY over 3+ boundverts)
  and rounded shared corners (still flat-clamped) — toast updated to warn only for 3+.
- **2026-07-01 (AM)** — Diagnosed the multi-edge miter as a *shared-vertex* case (pair 9,29 share v3, degree>3),
  not a cap. Tried extending the k=1 center-fan to k≥2 → regressed boundary 4→5 (graveyard §6 #17),
  reverted. Root-caused to STEP 3 (T,T) + STEP 5 cap (§6 #16). Added the §8 miter toast; kept the boundary
  install guard. Captured Blender flat-miter reference (`miter_ref.json` = 2 verts + 1 edge). Built this
  knowledge base and folded a clean-room `bmesh_bevel.c` study into §3 — **confirmed our
  `solve_corner_lines` IS Blender's `offset_meet`**; the gap is a missing `build_boundary`/BoundVert-cycle
  stage, not the math (§3.5–§3.6). Logged a profile parity gap (§3.3: our `shape` slider ≠ Blender
  `profile` except at 0.5; we have no concave). **Next:** add the BoundVert-cycle `offset_meet` stage
  between STEP 2 and STEP 3, have STEP 3/4/5 read it (§9).
- **2026-06-30 (overnight)** — see `docs/planning/poly/OVERNIGHT_LOG.md` (40KB). Rewrote bevel-on-beveled:
  delete-V + recessed center (§6 #10–15), SLERP circular profile (#7–8), Newell normals (#4),
  corner-derived sweep axis (#5–6), 17/17 tear → green. All Codex P1–P3 fixed (#19,22 + clamp + no-op).
- **Earlier** — offset-rail + WeldMap replaced the endpoint-slide chamfer (#1–3). See `BEVEL_PLAN.md`,
  `BEVEL_QUALITY_PLAN.md`, and the git trail (`git log --oneline -- '*bevel*'`).

- **2026-07-30 (Grok P1-1 residual honesty)** — Interactive tool already drove `bevel_op` with
  width/segments/shape/clamp (NOT frozen `bevel_edges_ex`). Residual lies closed this session:
  (1) Bevel **modifier** hard-coded shape=0.5/clamp=true while only exposing width/segments → added
  `bevel_shape` + `bevel_clamp` on `PolyModifier` with `#[serde(default)]` (old .fl4 → 0.5/true);
  UI sliders live. (2) Console log always printed `clamp=on` regardless of the C toggle → prints
  actual on/off. Gate: `apply_bevel_shape_flat_differs_from_round`. No multi-seg re-derivation;
  graveyard approaches not revisited. Plan `PLAN_FOR_GROK_POLY_PROCEDURAL.md` P1-1 marked done.

## Round 2 (2026-08-02) — locality install

**Goal:** kill measurement-C O(mesh) bevel (XL ~2s) without re-breaking oracle geometry.

**What shipped:**
- Large meshes (`face_count >= 8192`): **local install** — delete affected faces only, install dirty soup via `install_face_rings_local` + region normals. Untouched face IDs stay put.
- Small meshes: still full `from_polygons` soup (clean faces + seal) so multi-stage oracle tests with hard-coded HE ids after the first bevel keep their renumbering contract.
- Geometry stages (rails / miter / strips / caps) **unchanged** — only the rebuild vehicle splits.

**Dead ends this session (do not retry without new evidence):**
1. **Soup-only STEP-6 hole seal with local install** — treats clean-neighbour edges as holes, emits seal faces that same-winding-collide with surviving clean mesh. Seal is full-rebuild only.
2. **Densify-after-local for oracle nv** — renumbers differently than historical faces_out order; breaks hard-coded HE ids in two-stage tests (`miter_loop_slide`, `terminal_edge_stays_on_bottom`).
3. **Registering dirty-dirty twins as "boundary"** during `delete_faces_region` — must skip twins whose face is also dying.

**Lesson:** local install is safe for measurement C on large meshes; oracle multi-stage HE-id tests require the historical full-soup renumbering. Threshold 8192 is the split.

**Measured (op_bench --c-only, mut path):** bevel XL **222 ms** (was **1969 ms**, ~9×). Still not ring-flat (GC/free walks residual); further win is O(ring) unused-vert GC without face scan.

- **2026-08-08 (Grok — knife rect corner bevel ≠ Unbevel)** — Owner: "bevel made by the rectangular
  knife slice can not be unbevel with the unbevel tool." Root cause: knife R-shape **scroll corner
  bevel** baked **arc points into the cut polyline** (preview == commit). That produces rounded cut
  topology, **not** a bevel strip, so reverse-math Unbevel and history had nothing to peel.
  **Proposed fix (NOT LANDED — DEFERRED 2026-08-08, owner call):** preview keeps arcs; **commit**
  becomes sharp cut + real `bevel_vertices_offset` on the four corner verts + a `PolyBevelHistory`
  push (same as `poly_confirm_bevel`), so Ctrl+Alt+B restores the sharp hole and re-enters live
  vertex bevel. Gate would be `rect_corner_bevel_bake_is_unbevelable`; files
  `app/src/poly_knife.rs` (`knife_rect_pending_corner_bevel`, `poly_knife_bake_rect_corner_bevels`).
  **Why deferred:** it breaks master's `poly_knife_shape_release` invariant that the rounded ring in
  `knife_points` IS the cut, byte-identical across release / RMB / Enter. Two designs, one feature —
  the owner chose to keep the single-path invariant and revisit unbevel later, so **the original
  complaint is still OPEN**: knife corner rounding remains un-unbevelable.
  Still true regardless of which design wins: do **not** try to reverse-math arc-baked knife
  polylines — wrong geometry class.
