# Log triage — two real bugs, one of them silent

**Date:** 2026-07-29
**Source:** owner's console capture (reparent prim between bodies, "took a while to update")
**Status:** BUG 1 **fixed** (`5651ea2`) — `input_var_name_vec3` always coerces via `_coerced`
(`vec3(scalar)`). Gate: `noise_into_vector_math_scale_wgsl_validates` — reviewed, passes, and it
**validates** rather than parses. BUG 2 still diagnosis.
**BUG 3 — MIRROR of BUG 1: Color→Float — FIXED** (`input_var_name` luminance coerce;
gate `color_into_roughness_wgsl_validates`).

---

## BUG 1 (worse, and silent to the user) — the GPU material bake NEVER RUNS

Repeated ~20× in the log:

```
[gpu_material_bake] shader VALIDATE failed: InvalidStoreTypes { pointer: [134], value: [138] }
  in fn eval_node_material
Shader dumped to …\azra_material_bake_dispatch_fail.wgsl
[bake] res=256 80 ms          ← CPU fallback, every single time
```

**Root cause, read from the dumped shader.** Generated codegen, `eval_node_material`:

```wgsl
let n979_val = (n979_shaped) * (1.0);      // f32  — Noise output is SCALAR
var n980_rawv: vec3<f32> = vec3(0.0);      // declared VEC3
n980_rawv = n979_val * 1.0000;             // stores f32 into vec3  ← InvalidStoreTypes
```

`n979_valx/y/z` come from `sn_fbm_perlin` (scalar), get blended by triplanar weights into a scalar
`n979_raw`, and are then assigned straight into a `vec3<f32>` slot. naga's error names exactly this:
pointer is the vec3, value is the f32.

**Where in the source.** `graph.rs:1583`, `VectorMathOp::Scale`:

```rust
let a = self.input_var_name_vec3(id, 0, &Node::default_vec3(node, 0, [0.0; 3]));
```

There are **two** resolvers, and only one is safe:

| Function | Behaviour | Call sites |
|---|---|---|
| `input_var_name_vec3_coerced` | wraps scalars: `format!("vec3({expr})")` | **7** |
| `input_var_name_vec3` | returns the raw var — `NodeType::Noise => "n{id}_val"` (a scalar) at `:1745` | **19** |

So the coercion already exists and 19 call sites do not use it. Any graph that feeds a **scalar**
source (Noise, Voronoi scalar output, Curvature…) into a **vec3** input emits invalid WGSL.

**User-visible consequence.** The bake silently falls back to CPU — `[bake] res=256 80 ms` after every
failure. That is T1 in `OPTIMIZATION_PLAN_FOR_GROK.md` made concrete: the GPU bake is not merely
disabled for Final, it is *failing outright* for this graph shape. Note the contrast in the same log:
when validation passes, `[GpuBake] 256² Live — 8.0 ms (GPU)`.

**8 ms GPU vs 80 ms CPU — a 10× penalty, taken silently, on every edit.**

**Fix direction (Grok's call — this is the material codegen):** either route these sites through
`_coerced`, or make `input_var_name_vec3` splat by construction so the unsafe variant stops existing.
The second is stronger: a resolver that can emit a type error is a footgun with 19 loaded chambers.

**Gate:** a codegen test that builds Noise → a vec3 input and asserts the generated WGSL *validates*
(not just parses — this is precisely the "voxel_mesh lesson" case where parse-only passes and validate
catches it).

### Review of the fix (`5651ea2`) — correct, and correct for the right reason

Grok collapsed the unsafe resolver into the safe one rather than patching 19 call sites. That is the
stronger of the two options I offered: the footgun **stops existing** instead of being avoided, so a
future call site cannot reintroduce it. The doc comment even forbids re-adding a "raw" path. Verified
by running it: `noise_into_vector_math_scale_wgsl_validates ... ok`.

One note on the gate's second assertion — `node_wgsl.contains("vec3(")` is weak (nearly every
generated shader contains `vec3(` somewhere, so that half of the assert cannot fail). The `validate()`
call is doing the real work, and it is sufficient. Not worth changing.

---

## BUG 3 — the MIRROR of BUG 1, still live: `Color → Roughness`

`SocketType::compatible` (`types.rs:18`) permits **both** directions:

```rust
| (Self::Float, Self::Vec3) | (Self::Float, Self::Color)     // fixed by 5651ea2
| (Self::Vec3, Self::Float) | (Self::Color, Self::Float)     // STILL BROKEN
```

BUG 1 was scalar→vec3. The reverse — a **Color/Vec3 source into a Float slot** — has no coercion at
all. `input_var_name` (`graph.rs:1698`) returns the raw connected var, and it has **95 call sites**
versus the 18 that the fixed vec3 resolver has.

`graph.rs:670-673` flags this in a comment, but concludes it is *"not hit by any graph this task
builds or tests."* **That conclusion is wrong — I reproduced it.** Roughness / Metallic / Opacity /
Emission-Intensity are Float slots, the node editor accepts the connection because `compatible` says
yes, and the result is:

```wgsl
result.roughness = n1_col;      // vec3<f32> stored into an f32 field
```

```
VALIDATE failed: InvalidStoreTypes { pointer: [13], value: [7] }   in eval_node_material
```

Same error, same function, same consequence: **GPU bake dies → silent 80 ms CPU fallback on every
edit.** Reproduced with a 3-node graph — `ConstantColor → MaterialOutput.Roughness` — which is two
clicks in the UI, not a synthetic edge case. (Probe written, run, and removed; `gpu_material_bake.rs`
is untouched by me.)

**Fix:** the symmetric one. Add a `Float`-side resolver that reduces vec3 sources — and the choice of
reduction is a *product* decision, not a mechanical one:

* `luminance(v)` — matches the `compatible` comment, which literally says *"Float↔Color/Vec3 allowed
  (broadcast / **luminance**)"*, and matches Blender's Color→Float implicit conversion.
* `v.x` — cheaper, and what a "take the red channel" reading would give.

The comment already promises luminance, so **luminance is the honest choice** — anything else makes
that comment a second lying control. Whichever is picked, the CPU evaluator must use the *same*
reduction or CPU and GPU bakes diverge (the "Feature Must Work in Both Paths" rule).

**Gate:** the exact probe above — `ConstantColor → Roughness` must VALIDATE — plus a CPU/GPU parity
assertion on the chosen reduction. And with both directions coerced, the honest structural gate is a
loop over **every** pair in `compatible` asserting the generated WGSL validates; that is what stops a
third instance of this class.

**Priority:** same as BUG 1 was — it silently costs 10× on every edit, and it is now the *only*
remaining way to reach `InvalidStoreTypes` from a legal UI action.

### Confirmed from the owner's SECOND log — this was the live failure, on a real graph

The second capture dumps the shader from the owner's actual material. Grok's BUG 1 splat is visibly
working (`n1555_rawv = vec3(n1554_val) * 1.0000;` — correct), and the surviving failure is BUG 3:

```wgsl
let n1558_rawc = mix(n1557_col, (n1557_col * n1556_col), n1559_val);
let n1558_col  = (n1558_rawc) * (1.0);
result.metallic = n1558_col;      // vec3<f32> into an f32 field  ← InvalidStoreTypes
```

A **Color/Mix chain wired into Metallic.** Not a synthetic case — a hue/contrast/mix stack, i.e.
exactly the "complex procedural material" this pillar is for. My `ConstantColor → Roughness` probe was
the same defect on a neighbouring socket.

### Verification of the fix (post-`5aab556`)

`input_var_name` now reduces Vec3/Color via `dot(expr, vec3(0.2126, 0.7152, 0.0722))` — Rec.709
luminance, and the comment states it matches `Val::as_f` on the CPU path, so the two bake paths agree
(the "Feature Must Work in Both Paths" requirement). That is the option the `compatible()` comment
promised.

I ran the **full matrix** rather than the single gate — every `{ConstantColor, Noise}` source × every
`{Roughness, Metallic, Opacity, EmissionIntensity, Albedo, Emission}` slot, **plus the owner's exact
`ConstantColor → MixRgb → Metallic` chain**: **13/13 combinations parse and validate, 0 failures.**
`cargo test -p app -- gpu_material_bake node_graph_bake` = **44/44 green**. (Probe injected, run, and
the file restored byte-exact from a backup — `gpu_material_bake.rs` carries none of my edits.)

### MEASURED cost — I under-reported this by a lot

My earlier "10×" was from two log lines. The second capture has enough samples to measure properly:

| Path | n | mean | min | max |
|---|---|---|---|---|
| **GPU 256²** (validation passes) | 27 | **14.7 ms** | 10.1 | 29.1 |
| **CPU 256²** (fallback) | 45 | **111.1 ms** | 65 | 206 |
| **CPU 512²** (fallback) | 53 | **984.0 ms** | 369 | **2283** |

* 256²: **7.6× penalty**, ~96 ms lost per bake.
* 512²: **984 ms average, one sample at 2.28 s** — and 512² is only 4× the pixels of 256² but **8.9×
  the time**, so the CPU path scales *worse* than linearly. Extrapolating to the 2K/4K finals that P4
  is meant to enable, the CPU fallback is not viable at all.
* Against a working GPU 256² bake, a CPU 512² bake is **67×**.

So the honest framing is not "10× slower on edits" but: **this defect is what made high-resolution
baking feel impossible.** Every 512² number in the owner's own performance reports was measured on the
broken path.

---

## BUG 4 (new, from the second log) — a 124 ms idle poll, forever

The second capture is dominated by a metronome that has nothing to do with materials:

```
[SPIKE] poll_background_tasks: 124.5ms
[STALL] frame=128ms | layout=0.4 input=0.0 mesh=0.0 dispatch=1.9 overlays=0.2 | unaccounted=125.9 | tool=Idle
```

That pair repeats **dozens of times with `tool=Idle` and nothing being edited**, at a suspiciously
constant 124–125 ms. Two things make it a real bug rather than background work:

1. **`SLOWPOLL` accounts for none of it.** Every sub-timer reads zero —
   `dlo=0 dpoll=0 vox=0 ai=0 resident=0 bvc=0 primary=0 aitex=0 jobs=0 dc=0 pick=0 tail=0 dispatch=0`
   — and the whole 124 ms lands in `rest(top)`. So the cost is inside `poll_background_tasks` but
   outside every instrumented section.
2. **The value is too stable to be work.** Real work varies with scene size; 124.5/124.3/124.4/124.6
   across dozens of samples is the signature of a **fixed timeout or a blocking wait**, not
   computation. Candidates: a `device.poll(Maintain::Wait)`, a channel `recv_timeout`, or a
   fence/map wait with a hardcoded duration.

Also present and probably related: a repeating **`~32 ms`** poll and a repeating **`~465 ms`** poll,
both equally unaccounted. Three stable plateaus suggests a small number of blocking waits, not one.

**Why it matters:** 124 ms per frame at idle is ~8 fps of headroom gone before the app does anything.
This is very likely the same root cause as the `input=151.1ms` outlier I flagged from the first log —
a blocking wait that lands wherever the frame happens to be.

**Leads I already eliminated** (so the next person does not redo them):

* **`app_poll_gpu.rs:568` `Maintain::Wait`** — the only `Maintain::Wait` in the interactive poll path,
  but it is inside `if std::env::var_os("AZRA_PROBE").is_some()`, which was not set for this run.
  **Not it.**
* **A `recv_timeout` / hardcoded poll delay** — there are none in the poll path. The three
  `from_millis(100)` hits are `ctx.request_repaint_after`, which schedules a repaint and does not
  block. **Not it.**
* Every other `Maintain::Wait` in the tree is in a documented blocking/diagnostic entry point
  (`gpu_material_bake`'s test-only dispatcher, `gpu_curvature`'s soak path, `input_sculpt`) — none on
  the idle frame path.

So the wait is **not** an obvious hardcoded one, which makes the instrumentation step mandatory rather
than optional.

**Next step:** add a sub-timer for the un-instrumented remainder of `poll_background_tasks` (the
`rest(top)` bucket) and bisect. Three stable plateaus — ~32 ms, ~124 ms, ~465 ms — is a strong hint:
those ratios are close to 1×/4×/15× a ~31 ms unit, which smells like *N* iterations of one ~31 ms
blocking operation (a per-item GPU round-trip in a loop) rather than three separate waits. Counting
iterations per poll would confirm or kill that in one run.

---

## BUG 2 — the reparent delay the owner reported

```
[SPIKE] dispatch_pending_primary_bakes: 26.7ms
[SLOWPOLL] total=44ms | primary=44
[SPIKE] poll_background_tasks: 45.6ms
```

…repeating ~12 times in a row after the prim move. Then:

```
[BakeReadback] body=NodeId(4) grid=16777216  total=179.9ms
  to_vec (65536KB):  33.06ms
  nan_scan:           4.12ms
  dc_dispatch:        2.71ms
```

**What actually happened, in order:**

1. Reparent dirties **two** bodies (source + target) — correct per the Structural Mutation Checklist,
   and I verified all four required steps are present and ordered in `reparent_prim_to_body`.
2. `MAX_BAKE_READBACKS_PER_FRAME = 1`, so two bakes drain one per frame at best.
3. The body escalated to **256³** (`grid=16777216`), where a single readback is **180 ms** — of which
   **33 ms is the `to_vec` copy of 64 MB**.
4. My starvation guard (`b36fca2`) defers up to 8 frames before forcing each through.

Two bodies × (up to 8 deferred frames + one 180 ms readback) ≈ **0.5–1.5 s** of visible lag. That
matches "took a while to update" exactly, and it is *slow*, not *wrong* — the surface did land.

**The good news:** the starvation fix is working as intended. The repeated `SLOWPOLL primary=44`
followed by completion is deferral-then-force, not the permanent stall from before.

**Real improvements available, in order of value:**

* **The 33 ms `to_vec` is avoidable.** It copies the whole mapped 64 MB range into a fresh `Vec`
  before use. At 256³ that is pure memcpy the consumers may not need — several only scan it.
  Worth measuring a borrow-from-mapped-range path.
* **`MAX_BAKE_READBACKS_PER_FRAME = 1` doubles the wait** for the two-body case, which is the *common*
  case for reparent/group/dissolve. A time-budgeted drain (keep going while the frame has room)
  rather than a fixed count of 1 would halve it.
* **Both bodies re-bake at full 256³** even though only one prim moved. This is what O4 (dirty
  regions for the material bake) does for textures — the volume bake already has `dirty_regions`, so
  the question is why the reparent path is not producing one.

---

## Also visible in this log, unrelated to the report

* **`[Overlays] 10.5ms = chrome 0.0 + scene 10.4 + library 0.0 (thumbs 0.0)`** — my split (`96854b8`)
  paying off. It is **`ui2_draw_scene_overlays`**, not thumbnails, and not the library panel. My
  suspicion in the optimization plan (O2/T4: "library_tick_thumbs is the standout suspect") is
  **wrong** — thumbs are 0.0 ms. Correct O2 to target `scene`, and drop T4's priority.
* **`[STALL] frame=154ms | input=151.1`** — 151 ms in *input handling*, twice. Not investigated; it
  is a separate lead and nothing else in the log explains it.
* **`[Retighten] settle → 1 body(s) already tight, skipped redundant re-bake`** ×30+. The skip works,
  but it is being *asked* 30 times. Cheap to skip, but it suggests the settle path re-fires far more
  often than needed.
* **Cold pipelines total 349 ms**, PT-tri alone 302 ms — matches the earlier measurement, still worth
  lazy compilation.
