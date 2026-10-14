# Plan for Grok — node-graph correctness + codegen optimization

**Date:** 2026-07-29
**Source of findings:** `NODE_AUDIT_AND_OPT_2026-07-29.md` (measured, not read),
`LOG_TRIAGE_2026-07-29.md` (BUG 4), `CURVATURE_BOTH_MODES.md` §8 Phase 3.
**Status of the previous plan:** `OPTIMIZATION_PLAN_FOR_GROK.md` — O1, O3, T1, T2, T3 are **DONE**.
This document covers what that plan did not know about.

---

## Read this first — the headline

**9 of 42 node types emit WGSL that does not compile.** Wire any of them into a material and the GPU
bake dies and silently falls back to CPU. Measured this session: **7.6× slower at 256², 67× at 512²**.

That reframes the whole optimization effort. Every perf number the project has collected on the
material bake was measured on a path that fails for 21 % of node types. **Correctness before
optimization here is not a principle, it is arithmetic**: fixing G1 below buys more than every
remaining optimization in this document combined.

**Verified still open as of `b8eb675`:** all 7 int-cast sites present (`graph.rs` 1216, 1236, 1261,
1284, 1310, 1337, 1649); `fbm_wgsl` is **not** in the material-bake preamble (the 3 hits in
`shader_build.rs` are the *volume* bake and its tests — do not be fooled by the grep).

---

## Gate discipline (applies to every step)

Two rules earned from this session's failures. Both are non-negotiable:

1. **VALIDATE, never just parse.** Every one of these 9 bugs parses fine in isolation and dies in
   `naga::valid::Validator`. A parse-only test is a test that cannot fail here.
2. **A test that passes on broken output is not a test.** `p2_vertex_splat_grid_differs_from_planar…`
   asserted the vertex grid *differs* from planar — a grid of 77 % zeros differs enormously, so the
   gate passed *because* the feature was broken. Assert the **property you want** (coverage, validity),
   never "not identical".

Per step: `cargo build -p app`, then the named gate, then `cargo test -p graphics` if a shader changed.

---

## G1 ★★★ — Fix the 7 integer-cast sites (7 node types unblocked) ✅ — **DONE**

**Landed:** `input_var_name_u32` + 7 call sites; Octaves comment corrected. Gate: `g1_integer_defaults_emit_concrete_u_literals` + G2.

**The bug.** Socket defaults format as `{:.4}` → `"0.0000"`, then get spliced into `u32(...)`. WGSL
refuses to auto-convert an abstract float literal to an integer.

```
PARSE error: abstract floating-point values cannot be automatically converted to integers
```

**Unblocks:** `CylindricalMapping`, `PlanarMapping`, `TextureSample`, `GradientTexture`,
`WaveTexture`, `MusgraveTexture` (also needs G3), `SdfPosition`.

**The 7 sites** — all in `app/src/shader_nodes/graph.rs`:

| Line | Expression | Node |
|---|---|---|
| 1216 | `let ax = u32({axis});` | CylindricalMapping |
| 1236 | `let ax = u32({axis});` | (sibling mapping arm) |
| 1261 | `let pi = u32({pat});` | TextureSample |
| 1284 | `let mi = u32({mode});` | GradientTexture |
| 1310 | `let pi = u32({prof});` | WaveTexture |
| 1337 | `for (var oi = 0u; oi < u32({detail}); oi++)` | MusgraveTexture |
| 1649 | `let ax = u32({axis});` | SdfPosition |

**Do the structural fix, not the 7-instance fix.** Add an integer accessor beside the existing ones:

```rust
/// Integer-typed input. Formats an unconnected default as a concrete `Nu` literal and wraps a
/// connected (f32) socket in `u32(...)`. Never emit `u32({:.4})` — an abstract float literal
/// cannot be auto-converted to an integer (see the 7 sites this replaced).
fn input_var_name_u32(&self, node_id: NodeId, input_idx: usize, default: u32) -> String
```

This is the same move that fixed the vec3 class: **make the footgun stop existing** rather than
avoiding it at 7 call sites. `input_var_name_vec3` collapsing into `_coerced` is the precedent, and it
is why no new vec3 site can regress.

**Why not just `u32(f32(x))`** — I verified that works (18 → 6 failures, all 7 types fixed), and it is
correct for both connected and default sockets. But it leaves the raw `input_var_name` reachable from
integer contexts, so site #8 will have this bug again. Use it only if the accessor turns out to fight
the socket-default plumbing; if you do, say so in the commit.

**Historical note worth reading before you start.** `graph.rs:1015-1020` documents this *exact* defect
being fixed once already, for Noise's Octaves, and its comment concludes: *"Every sibling default on
this node already uses `{:.4}`; Octaves was the one outlier."* That conclusion is what caused these 7.
The correct rule is: **`{:.4}` for float sockets, integer formatting for integer-consumed sockets** —
and it is worth correcting that comment in the same commit so the next reader is not misled a third
time.

**Gate:** G2 below covers it — land them together.

---

## G2 ★★★ — The exhaustive per-NodeType validate gate ✅ — **DONE**

**Landed:** `every_nodetype_generates_valid_wgsl` (validate, not parse); non-whitelist types must
fail `graph_is_gpu_bakeable`. Do this **with G1**, not after.

```rust
/// GATE — every NodeType must emit WGSL that VALIDATES, wired to a Vec3 slot and a Float slot.
/// Nine node types shipped broken because the existing tests compile only hand-picked graphs.
/// Exhaustive by construction: a newly added NodeType cannot ship without passing this.
#[test]
fn every_nodetype_generates_valid_wgsl() { … }
```

Shape (this is the probe I ran — it works, ~0.05 s for 84 graphs):

* for each `NodeType`, build a fresh `NodeGraph`, add the node, `connect` it to `MO_ALBEDO`, and
  separately to `MO_ROUGHNESS`
* `generate_wgsl()` → `graphics::build_material_bake_wgsl(&node_wgsl)`
* `naga::front::wgsl::parse_str` **then** `naga::valid::Validator::validate`
* collect **all** failures and report them together — do not `panic!` on the first, or you will fix
  these one slow cycle at a time

**Enumerate the types explicitly** in the test array (43 variants, listed in
`NODE_AUDIT_AND_OPT_2026-07-29.md`). An explicit list means adding a variant to `NodeType` makes the
test fail to compile until someone consciously handles it — better than a runtime skip.

**The `GPU_INELIGIBLE` list.** Types that genuinely cannot bake on GPU (see G4) go in:

```rust
const GPU_INELIGIBLE: &[NodeType] = &[NodeType::AmbientOcclusion, NodeType::SdfThickness];
```

and the same test asserts they are **rejected by `graph_is_gpu_bakeable` before compiling**. That turns
"unsupported" into a stated decision with a test behind it, instead of a runtime shader crash. An
ineligible type must never reach `create_shader_module`.

---

## G3 ★★ — `fbm_wgsl` into the material-bake preamble (Musgrave) ✅ — **DONE**

**Landed:** material bake injects `wgsl_hash` / `noise_3d_wgsl` / `fbm_wgsl` (volume-bake forms)
beside `hash31`. Musgrave passes G2. **Careful:** volume bake already greps for `fbm_wgsl` — that
is not the material path.

`build_material_bake_wgsl` previously injected only `hash31`, so
`MusgraveTexture` failed with `no definition in scope for identifier: 'fbm_wgsl'`.

`fbm_wgsl` is a pure function with no scene dependency, and CLAUDE.md explicitly protects it: *"`fn
fbm_wgsl` stays in WGSL — used directly by surface noise modifiers, not part of the exponential
chain."* So injecting it is safe and in line with the existing rule.

**Careful — do not grep-and-assume.** `shader_build.rs` already mentions `fbm_wgsl` three times, but
those are the **volume** bake and its tests. The *material* bake preamble does not have it. I verified
this by compiling, not grepping.

**Parity requirement.** Verify the WGSL `fbm_wgsl` matches the CPU oracle numerically, not just by
name. Precedent from `9007b8b`: a first pass reused `sdf_prim_eval.wgsl`'s similarly-named hash, which
**validated but diverged numerically**. Same trap, same file.

**Gate:** `MusgraveTexture` passes G2, plus a CPU/GPU value-parity assertion on one Musgrave graph.

---

## G4 ★★ — AO and SdfThickness: fail closed ✅ — **DONE**

**Landed:** `GPU_INELIGIBLE` + gate `g4_ao_and_thickness_are_gpu_ineligible`. No bake-context
`calc_ao`/`scene_sdf` port (No Hacks).

`AmbientOcclusion` needs `calc_ao`; `SdfThickness` needs `scene_sdf`. Neither exists in the bake
context — these need scene traversal, so this is architectural, not a formatting slip.

**Do the cheap honest thing now:** add both to `GPU_INELIGIBLE` (G2) so `graph_is_gpu_bakeable` rejects
them *before* compiling. Result: identical CPU output, no wasted shader compile, one clear log line
instead of a validation crash. That is strictly better than today on every axis.

`AmbientOcclusion` is already special-cased out of the Vertex rung (`layer_ui.rs:1150`), so pinning it
to the CPU path is consistent with existing behaviour rather than a new exception.

**Do NOT port `calc_ao`/`scene_sdf` into the bake shader as part of this plan.** That is a real feature
with a real design question (which scene? bounded how?), and CLAUDE.md's "No Hacks" rule means it gets
done properly or fails closed — not approximated. Failing closed is the documented temporary guard the
rule permits.

---

## G5 ★ — Triplanar: stop evaluating every noise 3× — **STEP 1 DONE; STEP 2 SKIPPED (measured)**

`graph.rs` (was `:1027-1034`) emitted **three** full multi-octave fbm samples and blended them by
normal-derived weights. **Triplanar is the default projection**, so that was the common path.

**Step 1 landed** (`02b459e`): weight-guard `if (twN.axis > 0.01)` for Noise / Voronoi / Gradient /
Wave / Musgrave / Brick / TextureSample. Gate `g5_triplanar_generators_emit_weight_guards`.

**Measure (post-guard, same harness as audit baseline: full `bake_material_maps_gpu_blocking`, GPU
256², n=27, one warm; clean second run without driver spikes):**

| Path | mean | p50 | min |
|---|---|---|---|
| Triplanar **guarded** | 12.4 ms | **12.4 ms** | 12.1 ms |
| PlanarXY (single-plane floor) | 12.1 ms | **12.1 ms** | 11.8 ms |
| Audit baseline (pre-guard) | **14.7 ms** | — | — |

- **p50 vs baseline: −16 %** — real but modest wall-time win (not the hoped ~3×).
- **Room to floor (p50): ~2 %** — guarded triplanar already matches planar; **step 2 would buy almost nothing**.
- **Pixel gate:** triplanar-guarded ↔ PlanarXY at 64²: mean|Δ| = **0**/255 (bit-identical after sRGB8).

**Why not ~3×:** each blocking bake rebuilds shader module + pipeline + targets + `Maintain::Wait`
readback. FBM is a minority of that wall; step 1 already removed the 3× FBM waste (tri≈planar). The
remaining lead is module cache (see Unmeasured lead below), not more codegen.

**Step 2 (codegen planar collapse): SKIPPED** — floor gap is noise. Correctness + modest wall win kept.

**Gate:** `g5_measure_gpu_bake_triplanar_noise_256` prints the table + asserts pixel identity.

---

## G6 ★ — Curvature Vertex rung: Phase 3 ✅ — **DONE**

**Landed:** volume-stratified surface seeds (not AABB-face only) + planar multi-seed hole-fill for
UV coverage; gate `g6_vertex_splat_coverage_matches_planar_on_carved_box` (coverage ≥ 0.8× planar
AND max_delta > 0.02); disabled "Texture" option with "needs UVs" hover when `!has_uvs`.

**Owner, twice: "the curvature set in Vertex still does not work."** They are right, and Phase 2 is the
reason. Measured on `owner_repro_body` at res=64:

| Path | Texels with curvature |
|---|---|
| Planar (works) | **81.8 %** |
| Vertex (Phase 2) | **22.8 %** |

**77 % of the Vertex map is exactly 0.0** = "no curvature" = invisible.

**Density is not the fix — I tested it.** Sweeping seeds per face axis: 24 → 17.0 %, 64 → 26.1 %,
192 → 32.3 %, 384 → 34.9 %. **16× the seeds asymptotes near ~35 %**, never approaching 82 %. More seeds
buys a slower wrong answer, which is the cost the owner already objected to.

**Root cause is structural.** `build_vertex_splat_curv_grid` fires rays inward from six AABB faces, so
it can only find surface **visible from outside the box**. Concave pockets, boolean-seam interiors and
overhangs get no seed at all — exactly the geometry curvature masks exist to shade, and exactly what
the owner tests on.

**Phase 3 = use the real mesh vertices** (`SEED_POSITIONS`). Per `CURVATURE_BOTH_MODES.md` §3 the
machinery is already present: `SEED_POSITIONS` mode, `encode_curvature_vertex_dispatch`,
`poll_vertex_curvature`. DC vertices lie *on* the surface including inside concavities, so coverage
becomes a function of mesh density rather than AABB visibility — no blind spot by construction. Your
own O1 indirect dispatch (`4d663eb`) removed the 10–137× over-dispatch that made this expensive.

**Gate — replace the difference-only test:**

```
vertex_coverage >= 0.8 * planar_coverage        // fills the surface planar fills
AND still differs from planar by > 0.02          // the sampling domain really changed
```

Coverage is the property that was missing. Keep the difference check as a *second* assertion, never the
only one.

**Also worth a small UI fix while here:** on a no-UV body the "Texture" option is not rendered at all
(`layer_ui.rs:1115`) and the rung is force-set to Vertex every frame (`:1161`), so the combo offers only
"Vertex" and toggling it does nothing. Correct per the no-UV rule, but indistinguishable from a dead
control — which is part of why the owner reported it twice. A **disabled** "Texture" entry with a "needs
UVs" hover costs nothing and stops the confusion.

---

## G7 — BUG 4: the 124 ms idle poll (diagnostic, not a fix)

From the owner's second log: `poll_background_tasks: 124.5ms` with `tool=Idle`, repeating dozens of
times, while **`SLOWPOLL` accounts for none of it** (every sub-timer zero, all of it in `rest(top)`).
124.5/124.3/124.4 across dozens of samples is too *stable* to be computation — that is a blocking
wait's signature. ~8 fps of headroom gone before the app does anything.

**Leads already eliminated — do not redo:** `app_poll_gpu.rs:568`'s `Maintain::Wait` is behind an unset
`AZRA_PROBE`; there are no `recv_timeout`s or hardcoded blocking delays in the poll path (the three
`from_millis(100)` hits are `request_repaint_after`, non-blocking); every other `Maintain::Wait` is in a
documented test/soak entry point.

**Task:** instrument the un-instrumented remainder of `poll_background_tasks` (the `rest(top)` bucket)
and report. **Hint:** the three plateaus — ~32 ms, ~124 ms, ~465 ms — are roughly 1× / 4× / 15× a ~31 ms
unit, which smells like *N* iterations of one ~31 ms GPU round-trip in a loop rather than three separate
waits. Counting iterations per poll would confirm or kill that in a single run.

Report before fixing. This is likely the same root cause as the `input=151.1 ms` outlier from the first
log.

---

## Order, and why

| # | Item | Why here |
|---|---|---|
| 1 | **G1 + G2 together** | 7 node types onto a 7.6–67× faster path, permanently locked |
| 2 | **G3** | one injection, unblocks Musgrave |
| 3 | **G4** | small, honest, removes 2 shader crashes from the log |
| 4 | **G6** | owner-blocking, reported twice, root cause known |
| 5 | **G5** | step 1 landed; wall-time dead end vs 14.7 ms → **no step 2** |
| 6 | **G7** | instrumented (`c16e25d`); needs live SLOWPOLL with ptao/mats/warm |

G1+G2 first is not negotiable: until the bake compiles for every node type, any perf measurement on it
is measuring the wrong path — which is how the earlier 512 ms figures ended up describing CPU fallback
rather than the GPU bake.

**One item per commit.** Each commit states what was measured, not what was intended — and if something
measures as a dead end, commit that finding rather than discarding it.

---

## Module cache lead — **MEASURED + LANDED**

`bake_material_maps_gpu_blocking` used to `create_shader_module` + `create_compute_pipeline` on every
call. Now keyed by `(device.global_id, wgsl_hash)` with cap 48 entries.

**Measure** (`measure_material_bake_pipeline_cache_hit`, GPU 256², same device):

| | ms |
|---|---|
| cold (compile miss) | **11.6** |
| warm p50 (cache hit, n=12) | **9.3** |
| cold/warm | **1.25×** |

Not a 3× win — texture alloc + `Maintain::Wait` still dominate — but free on scrub re-bakes with an
unchanged graph. Gate asserts warm ≤ cold×1.05.
