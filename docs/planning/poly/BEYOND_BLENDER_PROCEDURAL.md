# Can the procedural system be BETTER than Blender — because we are in Rust?

**Date:** 2026-07-30
**Owner:** *"can we make the procedural system better than Blender, since we are in Rust?"*

**Short answer: yes — but the wins come from Blender's *architectural* debts, not from Rust being
fast.** Rust's contribution is mostly that it makes the correct architecture cheap to hold onto.
Below: what actually beats Blender, what merely *sounds* like it would, and one honest warning.

---

## 1. First, resist the obvious answer

"Rust is fast, C is fast, therefore we win on speed" is not a real advantage. Blender's mesh core is
mature, heavily optimised C, and it has had twenty years of profiling. **Assume rough parity on raw
per-op throughput and look elsewhere.** Where AZRA can genuinely be better is in things Blender
*cannot easily change now* because of decisions made long ago.

---

## 2. Blender's four real architectural debts

These are the openings. Each is a known, publicly-discussed limitation — not speculation.

### (a) The modifier stack is LINEAR

Blender modifiers are a list: each takes one mesh in, gives one mesh out. You cannot branch, merge two
branches, or reuse an intermediate result. Geometry Nodes was built *because* of this, and it is a
separate system that does not compose with the classic stack.

**AZRA's opening:** make the poly modifier stack a **DAG** from day one. Same evaluation model, but a
node can have two inputs. Bevel-then-branch → one branch solidified, one mirrored, both merged. That
is not a performance win, it is an *expressiveness* win, and it is the single biggest one available.

### (b) Re-evaluation is ALL-OR-NOTHING

Change any modifier parameter and Blender re-evaluates the whole stack from the start. On a heavy
stack (Subsurf → Bevel → Solidify → Array) tweaking the last modifier's slider still pays for every
earlier one.

**AZRA's opening — and it is already half-built.** The material system solved exactly this problem
this week: `mesh_mod_prefix_cache` (`cache_state.rs`) caches the unchanged *prefix* of an op chain, and
was **measured at 28–42× on edits at depth 4–12** (commits `ba81862` / `342e791`). The same idea
applied to the poly modifier stack means editing the last modifier costs only the last modifier.

This is the most concrete "better than Blender" item in this document, because the technique is
already proven in this codebase on a sibling problem.

### (c) No per-op provenance

Blender's BMesh does not record "this loop came from that edge". That is precisely *why* it has no
unbevel, and why "adjust last operation" is limited to the last operation. It is not a missing feature
so much as an absent data model.

**AZRA's opening:** `CustomData` on `PolyMesh` already exists as a per-element attribute sidecar with
`topo_gen` for invalidation. A provenance layer is a *column*, not a new subsystem. This is what makes
unbevel-at-distance possible at all (see `PROCEDURAL_BEVEL_CACHE.md` §0 step 3).

### (d) Single-threaded modifier evaluation

Most Blender modifiers evaluate on one thread. The dependency graph parallelises across *objects*, not
within one object's stack.

**AZRA's opening:** `rayon` is already a workspace dependency and already used in `bvh.rs`,
`chunked_dc.rs`, `mesh.rs`, `sdf_volume.rs`, `voxel_sculpt.rs`. A DAG (item a) makes independent
branches *structurally* parallel — you get concurrency from the shape of the graph, not from
hand-threading each modifier.

---

## 3. What Rust ACTUALLY contributes

Being precise here, because the usual claims are overstated:

| Claim | Verdict |
|---|---|
| "Rust is faster than C" | ❌ Not meaningfully. Assume parity. |
| "Fearless concurrency lets us parallelise evaluation" | ✅ **Real.** `Send`/`Sync` make a parallel DAG evaluator reviewable instead of terrifying. Blender's C core would need audited locking. |
| "The type system prevents whole bug classes" | ✅ **Real, and this is the big one** — see below. |
| "Enums + exhaustive `match` model op graphs well" | ✅ Real. Adding a node type breaks compilation until every consumer handles it. |
| "No GC pauses" | ➖ Irrelevant; Blender has no GC either. |
| "Ownership makes caching safe" | ✅ Real. `Arc<HalfEdgeMesh>` snapshots are cheap and provably immutable — `PolyInsetAdjust` already relies on this. |

### The type-system point is not abstract — this session proved it three times

* **Voronoi WGSL bug** — after the fix, swapping back to the blocking bake entry **failed to compile**
  because return types diverged. The type system became the first line of defence.
* **Mirror modifier** (found today) — `mirror_axis: u8` carries *two incompatible encodings* with no
  translation layer, so ticking "X" mirrors Y. A `MirrorAxis` enum or `bitflags` type makes that
  **unrepresentable**.
* **`PolyEdit`'s exhaustive match** already forces every new op to declare its undo tier.

So the honest formulation is: **Rust does not make the procedural system faster than Blender's — it
makes a more ambitious architecture safe to maintain.** A parallel, branching, incrementally-cached
evaluator is exactly the kind of thing that is hard to keep correct in C, and that is the actual
advantage.

---

## 4. The ranked opportunity list

| # | Opportunity | Beats Blender because | Effort | Confidence |
|---|---|---|---|---|
| 1 | **Prefix-cached stack re-eval** | Blender re-runs the whole stack on any change | Medium | **High** — 28–42× already measured on the sibling material path |
| 2 | **DAG instead of linear stack** | Blender's stack cannot branch or merge at all | Large | High (design), Medium (fit) |
| 3 | **Per-element provenance** | BMesh has none; enables unbevel-at-distance | Medium | Medium |
| 4 | **Parallel branch evaluation** | Blender is single-threaded per object | Medium | Medium — needs (2) first |
| 5 | **Typed parameters** (`MirrorAxis`, `BevelWidthType`) | Blender uses ints/enums loosely; today's bug is exactly this | Small | **High** — fixes a live P0 |

**Start with 1 and 5.** Item 1 has a measured precedent in this repo and a clear payoff. Item 5 is
small and fixes a bug that is currently corrupting geometry. Items 2–4 are the ambitious part and
should follow, not lead.

---

## 5. The honest warning

**Blender's stack is linear and eager for good reasons too.** It is predictable, trivially explainable
to users, and easy to debug. A DAG with incremental caching and parallel branches introduces three
hard classes of problem Blender does not have:

* **Cache invalidation correctness.** A stale prefix silently produces wrong geometry — the exact
  failure mode this project has hit repeatedly (the `p2_vertex_splat` gate that passed *because* the
  output was broken; the "differs from planar" test that proved nothing).
* **Non-determinism.** Parallel evaluation must produce bit-identical results to serial, every time.
  That needs a gate, not an intention.
* **User-facing complexity.** A DAG that users cannot reason about is worse than a stack they can.

**The mitigation is the one this codebase already uses:** every incremental path must be gated on
being **bit-identical** to the naive full re-evaluation. That is precisely the gate written for the
`SceneEvaluator` memo (`evaluator_memo_is_bit_identical_to_rebuilding`) and for O4's tiled bake
(`o4_tiled_region_matches_full_bake_bit_identical`). The pattern is established; it just has to be
applied here too.

**Recommendation:** be better than Blender on **(1) incremental re-evaluation** and **(5) typed
parameters** first — both are low-risk with proven precedent. Treat the DAG as a considered bet, not a
default, and only after the poly toolset's confirmed P0s (the Mirror axis bug, vertex/edge extrude) are
closed. Being *better* than Blender at procedural editing matters much less than being *correct* at
mirroring.
