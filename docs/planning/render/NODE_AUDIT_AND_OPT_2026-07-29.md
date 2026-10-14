# Node-graph audit + optimizations for Grok

**Date:** 2026-07-29
**Asked:** review the code for optimizations, and make all node work go through the shader node graph.
**Method:** compiled every `NodeType` through the real codegen and asked naga to parse+validate it.
Measured, not read. All probes were injected, run, then reverted — `graph.rs` and
`gpu_material_bake.rs` are byte-identical to HEAD.

---

## PART 1 — CORRECTNESS FIRST: 9 of 42 node types cannot compile at all

Every `NodeType` was wired to `MaterialOutput.Albedo` and to `.Roughness` (84 graphs), generated
through `generate_wgsl()` → `build_material_bake_wgsl()`, then parsed and validated.

**Result: 18 failures across 9 distinct node types.** Every one of these silently kills the GPU
material bake and drops the user onto the CPU path — which the same-session measurement puts at
**7.6× slower at 256² and 67× at 512²**. So this is not cosmetic: it is the same class of defect as
the `InvalidStoreTypes` bug, and it is nine more instances of it.

### Group A — float literal spliced into an integer conversion (7 types, all one-line fixes)

```
CylindricalMapping   PlanarMapping   TextureSample   GradientTexture
WaveTexture          MusgraveTexture SdfPosition
```

```
PARSE error: abstract floating-point values cannot be automatically converted to integers
```

**Cause.** The socket default is formatted with `{:.4}`, producing `0.0000`, and is then spliced
into `u32(...)`:

```rust
let axis = self.input_var_name(id, 1, &format!("{:.4}", node.input_defaults[1].value_f32));
lines.push(format!("    {{ let ax = u32({axis}); let s = {scale};"));   // → u32(0.0000)
```

WGSL refuses to auto-convert an abstract float literal to an integer.

**This is a recurrence, not a new bug.** `graph.rs:1015-1020` carries a comment describing the exact
same defect being fixed once before for Noise's Octaves (`{:.0}` emitted a bare `4` which broke
`clamp`). The comment even says *"Every sibling default on this node already uses `{:.4}`; Octaves was
the one outlier."* The lesson generalised in the wrong direction: `{:.4}` is right for float sockets
and wrong for every socket consumed as an integer.

**All 7 sites** (`graph.rs`):

| Line | Expression |
|---|---|
| 1217 | `let ax = u32({axis});` — CylindricalMapping |
| 1237 | `let ax = u32({axis});` — (sibling mapping arm) |
| 1650 | `let ax = u32({axis});` — SdfPosition |
| 1262 | `let pi = u32({pat});` |
| 1285 | `let mi = u32({mode});` |
| 1311 | `let pi = u32({prof});` |
| 1338 | `for (var oi = 0u; oi < u32({detail}); oi++)` |

**Verified fix.** Wrapping each as `u32(f32({expr}))` took failures from **18 → 6** and fixed all 7
types. `f32(...)` is a no-op when the input is already a runtime `f32` (a connected socket) and makes
the abstract literal concrete when it is a default — so it is correct for both cases, which matters
because these sockets can be either.

**Preferred fix (Grok's call).** `u32(f32(x))` works but treats the symptom. The cleaner version is a
dedicated accessor — `input_var_name_int()` — that formats defaults as `0u`/`1u` and wraps connected
float sockets in `u32(...)`. That removes the class rather than the 7 instances, exactly as collapsing
`input_var_name_vec3` into `_coerced` did for the vec3 case. Either way the gate matters more than the
mechanism.

### Group B — the generated code calls functions the bake shader does not have (3 types)

| Node | Missing symbol | Nature |
|---|---|---|
| `AmbientOcclusion` | `calc_ao` | needs the live scene — architectural |
| `SdfThickness` | `scene_sdf` | needs the live scene — architectural |
| `MusgraveTexture` | `fbm_wgsl` | **just a missing injection** |

`build_material_bake_wgsl` (`graphics/src/shader_build.rs:354`) injects only `hash31`. So:

* **`MusgraveTexture` is a cheap win** — `fbm_wgsl` is a pure function with no scene dependency, and
  CLAUDE.md explicitly keeps it in WGSL ("`fn fbm_wgsl` stays in WGSL — used directly by surface noise
  modifiers"). Inject it beside `hash31` and Musgrave works. Verify numerical parity with the CPU
  oracle, per the `hash31` precedent where a same-named-but-different hash validated yet diverged.
* **`AmbientOcclusion` / `SdfThickness` are genuinely harder.** They need scene traversal that does not
  exist in the bake context. Two honest options: port a bake-context implementation (they already have
  CPU equivalents that the CPU bake path uses), or **fail closed** — mark them GPU-ineligible so the
  bake gate rejects them *before* compiling, routing to CPU deliberately instead of via a shader crash.
  Failing closed is strictly better than today: same result, no wasted compile, and a clear log line.

Note `AmbientOcclusion` also cannot take the Vertex rung (`layer_ui.rs:1150`) — it is already a
partially-special node, so pinning it to CPU is consistent rather than a new exception.

### The gate this needs

The existing `gpu_material_bake` tests compile a handful of hand-picked graphs. **Nine broken node
types shipped anyway.** The structural gate is the probe I ran:

> for every `NodeType`, wire it to a Vec3 slot and a Float slot, generate, and assert the WGSL
> **validates** (not just parses).

42 types × 2 slots is ~0.05 s and it is exhaustive by construction, so a newly added node type cannot
ship broken. Types that are legitimately CPU-only should be listed in an explicit
`GPU_INELIGIBLE: &[NodeType]` allow-list that the same test asserts is *rejected by the bake gate* —
so "not supported" is a stated decision with a test behind it, not a runtime shader failure.

---

## PART 2 — OPTIMIZATION: Triplanar evaluates every noise 3×, and it is the default

`graph.rs:1027-1034`:

```rust
if tri {
    self.emit_noise_sample_wgsl(id, "x", …);   // full fbm
    self.emit_noise_sample_wgsl(id, "y", …);   // full fbm
    self.emit_noise_sample_wgsl(id, "z", …);   // full fbm
    lines.push(format!("let n{id}_raw = n{id}_valx*twN.x + n{id}_valy*twN.y + n{id}_valz*twN.z;"));
} else {
    self.emit_noise_sample_wgsl(id, "r", …);   // ONE fbm
    lines.push(format!("let n{id}_raw = n{id}_valr;"));
}
```

**Triplanar is the default projection** (`node_data.rs:59`: *"Built-in projection for Noise / Image /
Gradient / etc. Default Triplanar."*), so the 3× path is the **common** path, not an edge case. Each
`emit_noise_sample_wgsl` is a full multi-octave fbm — with the default 3 octaves that is 9 noise
evaluations per Noise node where 3 would do.

The same 3-eval pattern repeats for the texture nodes: `:1294` (Gradient), `:1319` (Wave), `:1346`
(Musgrave).

### Why much of that work is provably discarded

The blend weights `twN` are derived from the surface normal. **On the planar bake domain the normal is
a constant** — `node_graph_bake.rs:568-570` states it outright: *"on the planar bake domain the
evaluator's `n` argument is the CONSTANT `[0,0,1]` (every pixel of the flat bake quad)"*. With a
constant normal, two of the three weights are constant-near-zero for the entire bake, so **two of every
three fbm evaluations are multiplied by ~0 and thrown away**.

### Three fixes, cheapest first

1. **Weight-threshold skip (safe, local).** Emit the y/z samples inside `if (twN.y > 0.01)` guards.
   Wave-uniform on the bake domain (all lanes take the same branch), so no divergence cost. Saves
   ~2/3 of noise cost on the common path.
2. **Specialise the bake variant (bigger win).** The bake already generates its own shader per graph —
   so when the domain is known planar, emit the single-plane path directly and skip the blend entirely.
   Free at runtime; the decision moves to codegen time where it belongs.
3. **Hoist the octave loop bound.** `for (var oi = 0u; oi < u32({detail}); oi++)` re-converts a
   loop-invariant every iteration. Naga may hoist it; measure before bothering. Fixing the Group A bug
   at line 1338 touches this line anyway.

**Measure before/after.** The same-session numbers make the target concrete: a working GPU 256² bake is
**14.7 ms mean**. If two-thirds of noise cost is waste, that should move visibly — and if it does not,
that is worth knowing before investing in (2).

### Not an optimization — a caching note

`bake_material_maps_gpu_blocking` calls `create_shader_module` on every invocation
(`gpu_material_bake.rs:194`) with no module cache. Per-graph shader compilation is real cost
(startup logs show 25-43 s cold for the sdf set; a small material shader is far cheaper but not free).
**Worth measuring, not assuming** — if the graph is unchanged between bakes the module could be keyed by
a hash of the generated WGSL. I did not measure this one, so treat it as a lead rather than a finding.

---

## Suggested order

1. **Group A — 7 one-line fixes.** Unblocks 7 node types onto the 7.6-67× faster path. Highest
   value-to-risk in this document.
2. **The exhaustive per-NodeType validate gate.** Do this *with* (1) so the fix is locked in and the
   next new node cannot regress it.
3. **`fbm_wgsl` injection** → Musgrave works (verify CPU parity).
4. **AO / SdfThickness: fail closed** in the eligibility gate. Small, honest, removes two shader
   crashes from the log.
5. **Triplanar weight-skip** (opt 1), measured against the 14.7 ms baseline.
6. Bake-variant specialisation (opt 2) only if (5) shows the win is real.
