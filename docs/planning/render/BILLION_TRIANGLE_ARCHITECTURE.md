# Billions of triangles — forests, cityscapes, terrain

**Date:** 2026-07-30
**Owner:** *"I want to be able to display billions of triangles in the scene — large forest, epic
cityscape, complex terrain."*

**This is achievable, and mostly NOT with Nanite.** The goal decomposes into three different problems
with three different solutions, and the current renderer blocks all three for one shared reason.

---

## 1. The reframe that matters

"A billion triangles" is not one problem. Count **unique** geometry versus **drawn** geometry:

| Scene | Drawn triangles | UNIQUE triangles | Real technique |
|---|---|---|---|
| Forest, 100k trees | ~2 B | ~2 M (20 tree models) | **Instancing** + LOD + culling |
| City, 50k buildings | ~1 B | ~20 M (200 kit pieces) | **Instancing** + LOD + occlusion |
| Terrain, 16k² heightfield | ~500 M | — (procedural) | **Clipmap / quadtree** |
| Scanned statue | ~1 B | ~1 B | **Nanite-class virtualised geometry** |

**Only the last row needs Nanite.** A forest is not a billion unique triangles — it is twenty tree
models drawn a hundred thousand times. The winning technique is drawing the *same* geometry many times
cheaply, which is instancing, not virtualisation.

This matters because Nanite is a multi-year rewrite that replaces the hardware raster path, while
instancing + LOD + occlusion is well-understood work that reuses the existing pipeline. **For forests,
cities and terrain, instancing carries roughly 95 % of the win.**

---

## 2. The blocker, verified in code

### Duplicates share CPU memory but NOT the GPU

```rust
// core_math/src/scene_graph.rs:567
mesh: Arc<HalfEdgeMesh>,          // ✅ 100k trees share ONE mesh in RAM
mesh_cache_id: Option<MeshCacheId>,
```

```rust
// app/src/scene_actions.rs — every duplicate path
mesh_cache_id: None,              // ❌ fresh GPU upload per copy
```

So duplicating a tree 100 000 times costs **one** `HalfEdgeMesh` in RAM and **100 000 separate GPU
vertex/index buffers**. The CPU side already got this right; the GPU side never did.

### One draw call per object, one instance per draw

```rust
// app/src/raster_gbuffer.rs
for (nid, node) in self.scene.scene_graph.all_nodes() { …   // every node, every frame
    rpass.draw_indexed(s..(s + c), 0, 0..1);                // ← 0..1 = ONE instance
}
```

100k trees = **100k draw calls per frame**, plus a `HashMap` iteration over every node. Typical desktop
budgets are 10–50k draw calls/frame *total*. This ceiling is hit long before triangle throughput
matters.

### What is missing entirely

| Capability | Status |
|---|---|
| GPU instancing (raster path) | ❌ none — `0..1` |
| Occlusion culling | ❌ none (frustum culling ✅ `raster_gbuffer.rs:537`) |
| Poly LOD | ❌ none (SDF LOD ✅ `lod_policy.rs`, tested, generic) |
| Terrain / heightfield / clipmap | ❌ none — grep returns zero |
| Scene graph for 100k+ nodes | ⚠ `HashMap<NodeId, SceneNode>`, walked per frame |

---

## 3. The path, in dependency order

### Phase 1 — Instancing ★★★ (the single biggest win)

Duplicates that share an `Arc<HalfEdgeMesh>` must share a `MeshCacheId`, then draw as
`0..instance_count` with a per-instance transform buffer.

**100k draw calls → ~20.** That is the difference between impossible and trivial for a forest.

The plumbing partly exists: `draw_indexed_indirect` and `instance_count` are already used on the
pooled-voxel path, so the pattern is established in-tree.

*Gate:* 10k duplicates of one mesh must issue **1** draw call and upload **1** vertex buffer. Assert
both — draw-call count is the metric that actually moves.

### Phase 2 — Poly LOD ★★★

Covered in `PLAN_FOR_GROK_VIEWPORT_PERF.md` P3. `choose_lod_level` is already generic over projected
pixels; poly bodies just are not in the walk and have no coarse meshes. `DecimateParams` supplies the
coarse meshes.

Instancing + LOD compose: a distant tree instance draws a 200-triangle imposter, a near one draws 20k.

### Phase 3 — Occlusion culling ★★★

For a cityscape this beats everything else: most of the city is behind other buildings. GPU
depth-pyramid against last frame's depth; the depth buffer already exists.

**Phases 1–3 together are what deliver "epic cityscape."** None of them is Nanite.

### Phase 4 — Terrain as its own primitive ★★

A 16k² heightfield is not a poly mesh and should not become one. Clipmap or quadtree with GPU
displacement: fixed vertex count, detail follows the camera. This is a **new body type**, not an
extension of `PolygonalBody` — and per CLAUDE.md a new `NodePayload` variant must be **appended at the
end** (bincode tags positionally).

### Phase 5 — Cluster LOD, only if measurement demands it ★

If 1–4 land and a *single object* is still too dense (a photogrammetry scan), then per-cluster LOD:
split the mesh into ~128-triangle clusters, choose a cut per frame, draw with the **hardware**
rasteriser.

**Do NOT implement Nanite's software rasteriser.** It replaces the hardware raster path for opaque
geometry, and CLAUDE.md's Display Invariant forbids fusing two techniques over the same pixels — that
is a documented device-loss crash class here, not a style preference.

---

## 4. Two honest cautions

**This does not start today.** `poll_background_tasks` is measured at **19–27 ms on an idle frame with
one cube**, against a 12 ms budget. A renderer that cannot draw one cube inside budget will not draw a
forest, and — more practically — that CPU floor makes every instancing measurement meaningless until
it is gone. `PLAN_FOR_GROK_VIEWPORT_PERF.md` P1/P2 are the prerequisite, and they are small.

**The scene graph will need attention around Phase 1.** `HashMap<NodeId, SceneNode>` walked per frame
is fine for hundreds of objects and wrong for hundreds of thousands. Instancing partly hides this
(instances are transforms in a buffer, not nodes), which is another reason it comes first — a forest
should be *one* node with 100k transforms, not 100k nodes.

---

## 5. What to tell the owner, plainly

The goal is reachable, and the honest sequencing is:

1. **Fix the CPU floor** (small, measured, blocking everything)
2. **Instancing** — turns 100k draw calls into ~20; this alone unlocks forests
3. **Poly LOD** — reuses a chooser that already exists and is tested
4. **Occlusion culling** — unlocks cities
5. **Terrain primitive** — a new body type, not a giant mesh
6. **Cluster LOD** — only if a single object is still too dense

Steps 2–4 are the ones that deliver "forest + cityscape", and none of them requires the architecture
rewrite the word "Nanite" implies.
