# Material system → feature complete (Substance/Photoshop class): the build plan

**Date:** 2026-07-29
**Supersedes the sequencing in** `MATERIAL_SUBSTANCE_PARITY_PLAN.md` (which stays as the evidence
and review record; this is the executable plan).

## Goal, in the owner's terms

* Author **complex procedural textures that bake later** — Substance-class generators, full layer
  blending, masks, multi-material looks.
* **"Live" = live preview *feeling*** (scrub a noise scale, see it move). NOT per-frame evaluation
  in a game engine.
* **Shader tree and node system LINKED** — panel for basic users, nodes for advanced work, same
  material.
* **Everything must work in the path tracer.**

---

## The finding that shapes everything: PT makes the bake the natural owner

PT samples the material **texture array** (albedo / roughness / metallic — verified in
`pathtrace_tri_entry.wgsl`; it never perturbs the shading normal). The GPU bake writes *into* that
array via `NodeGraph::generate_wgsl()`.

**Therefore: anything that goes through the bake works in PT for free.** The things that do *not*
work in PT are exactly the things evaluated LIVE outside it — the vertex-rung curvature carry and
the G-buffer's inline layer composite — because PT never runs the G-buffer fragment path.

So "everything must work in PT" is not a porting project. It is an argument for **one compositing
owner, and that owner is the bake.** The G-buffer composite becomes a *preview* of the bake, not a
second implementation of it. That single decision resolves:

* PT parity (bake is the source of truth)
* the `blended = top` clobber class (one composite, not four)
* per-layer alpha (the compositor can carry it; a flattened map cannot)
* normal maps in PT (bake normal→height, or accept that PT needs geometry, not a normal perturb)

### Good news on blend modes

There is already a single evaluated definition: `MixBlend` (`ops.rs:292`) with Mix / Multiply /
Screen / Overlay / Add, evaluated in **two** places — the CPU bake (`node_graph_bake.rs:2159`) and
WGSL codegen (`graph.rs:1118`). The panel maps its own `BlendMode` onto it (`lower.rs:1290`).

So blend modes are **2 implementations + 1 mapping**, not 4 from scratch. Adding ~22 Photoshop modes
means extending one enum and two evaluators — real work, but bounded and mechanical. The 4-way
duplication I described earlier is specifically the G-buffer's *inline* composite, which this plan
deletes.

---

## Phase plan

Each phase is independently landable and leaves the app working. **Gate every phase on the soak
harness showing a real pixel delta** — a green unit test that moves no pixels has been wrong twice
this project.

### P0 — Land what exists (blocking; ~1 session)

1. Resolve the two regressions in Grok's uncommitted work:
   * `new_generator_layer`'s `if vertex { Normal }` reintroduces the clobber (no `blend == 0u` arm
     exists). Decide: keep Multiply, **or** add a real "over" arm.
   * `gpu_bake_matches_cpu_on_procedural_graph` — albedo mean |Δ| 35.5/255 vs tol 2. A *mean* shift
     is a real port bug; suspect the sRGB encode path the dual-plane work touched.
2. Commit. 1269 uncommitted lines in a two-agent tree is the standing risk.

**Exit:** `-p app` and `-p render` green, tree clean.

### P1 — Identity + merge semantics (the foundation; 2–3 sessions)

Nothing else can be built safely first — mask stacks and drivable params both need this.

1. **Stable IDs replace the parsed string.** `mp:<kind>:<role>:<layer>` cannot survive reorder,
   insert, or nesting. Give every layer and every parameter a stable id (the `prim_uid` pattern;
   "stable IDs, not indices" is already a CLAUDE.md rule).
2. **Regen becomes MERGE, not replace.** `regen_channel_subgraph` currently deletes every
   panel-tagged node and rebuilds (`lower.rs:854`). It must preserve user-added nodes and any socket
   marked driven, regenerating only what the panel still owns.
3. **Per-parameter ownership.** Every panel control becomes `Const(v)` | `Driven(node_output)`.
   `Driven` lowers to a socket **connection**. The sockets already exist — `SdfCurvature` declares
   Position / Epsilon / GrowShrink / Band / Contrast / Strength as real inputs — so this is lowering
   + UI, not shader work.

**ADR required before coding.** This changes how existing materials round-trip, and per-parameter
ownership is a contestable choice against the cheaper per-channel `GraphDriven` that already exists.

**Save format:** `Layer` is serialized. New shape rides the in-blob `#[serde(default)]`
side-channel — never an in-place retype, and note that appending to a *nested* struct also breaks
the wire (`REF_FL4_BACKCOMPAT_INVESTIGATION.md`).

**Exit:** hand-edit a node, then edit the panel — the hand edit survives. Gated by a test.

### P2 — One compositor, bake-owned (2–3 sessions)

1. Delete the G-buffer's inline layer composite. The G-buffer samples the baked result like PT does.
2. Extend `MixBlend` to the full Photoshop set (~27). Two evaluators to update, one enum.
3. **Per-layer alpha** through the compositor with real over-operators. This is what makes
   "masking removes" distinct from "blend toward black" — impossible today because the bake
   flattens.

**Exit:** PT and Hybrid agree on a multi-layer material (pixel-compared). Alpha masking visibly
removes.

### P3 — Expressiveness on the new foundation (2 sessions)

1. **Mask stacks:** `mask: Option<LayerContent>` → a recursive stack with its own blend/opacity.
   Unblocks "mask the curvature mask with noise."
2. Drivable params exposed in the UI (the model landed in P1).

**Exit:** the owner's two stated cases work: noise drives curvature radius; a mask has its own mask.

### P4 — Resolution and precision as parameters (1–2 sessions)

1. `MAP_RES = 1024` is hardcoded at **59 sites** → make the bake target a parameter; 2K/4K final.
2. Finish dual-plane 16-bit end to end (Grok's approach; sound).
3. **Live vs final is a QUALITY TIER of one compositor**, not a second code path. Live: capped res
   and octaves. Final: **uncapped, off the frame thread, progress reported** (`df44281` is the
   pattern — schedule expensive work honestly rather than shrinking it).

**Exit:** 4K 16-bit final bake; live scrub still feels immediate.

### P5 — Generator completeness (1–2 sessions, additive)

- **AO: done** — `sdf_ambient_occlusion` + live `calc_ao`. Gate: `p5_ao_open_surface_bright_cavity_dark`.
- **Thickness: done** — `NodeType::SdfThickness` (interior −normal march). Panel + mask.
- **Position: done** — `NodeType::SdfPosition` (AABB-normalized RGB / axis). Panel + mask.
- **Vertex→map splat: done** — `build_vertex_splat_curv_grid` for Vertex-tagged curvature.
  Gate: `p2_vertex_splat_grid_differs_from_planar_cpu_on_carved_box`.
- Residual: `NormalDirection` (legacy, use `Direction`).

**P2 exit (PT/Hybrid multi-layer pixel soak):** still open. Unit suites pass; a headless soak that
pixel-compares Hybrid vs PT on a multi-layer material has not been automated. Prefer the existing
soak harness (`_audit_soak.py` / owner soak scripts) over a new unit test that cannot move pixels.

### P6 — Multi-material (own ADR)

Sits above the compositor. Do not conflate with P1–P3.

---

## Rules carried from this session's failures

These are not generic advice; each cost real time today.

1. **Measure before optimising.** The convert stall was a blocking GPU readback, not the mesh
   builder I was about to optimise (109 ms of 1137 ms was CPU). The 14 ms "overlay tax" was
   mislabelled — it contained the library panel and thumbnail ticking.
2. **A green test that moves no pixels proves nothing.** The soak's `px` column caught a curvature
   cap that silently skipped every dispatch while all unit tests passed.
3. **Sabotage-check new tests.** A cache-equality test passed under deliberate sabotage because the
   cache barely engaged.
4. **Manifoldness under-specifies geometry.** The sphere fix passed all 25 manifold cases and was
   inside-out; only a normal-vs-centroid check caught it.
5. **Benchmarks lie in both directions.** A shared-cache benchmark reported 1.2× for work that was
   actually 28–42×.

---

## What I still need from the owner

Two answers change the plan; everything else I can proceed on.

1. **P1's ADR — per-parameter ownership vs per-channel.** Per-parameter is what the stated goal
   needs, but it is the larger build. Confirm before I design it.
2. **Normal maps in PT.** PT cannot perturb normals without a real shading-normal path. Options:
   (a) bake normal→height and drive displacement (geometry — works everywhere; conversion already
   landed in `35f5d09`), (b) add tangent-space normal perturbation to PT, (c) accept the gap. (a) is
   cheapest and matches "bake later"; (b) is the correct renderer fix. Which?

I do not need more information to start P0 or to write the P1 ADR.

---

## Amendments from Grok's review (2026-07-29)

Three corrections, all accepted. Recording them here rather than silently editing the phases above,
so the reasoning stays visible.

### 1. P4's 16-bit item is further along than stated — and the remaining gap is HONESTY

"Finish dual-plane" understates it: `upload_map_16` / `high_precision` / `reconstruct_u16_channel`
are in place, plus EXR/HDR/float-TIFF height decode. **The real remaining gap is telling the user
when 16-bit does nothing.**

Dual-plane lives in an **sRGB** array, so both planes pass through a non-linear hardware decode. The
lo plane's contribution is therefore not a clean 1/257 step, and precision near white is coarser
than near black. On a **flat colour** — no gradient — 8-bit and 16-bit can land on the same byte and
the user sees no difference at all. That is exactly the trap that produced the owner's "same
stepping in 16 bit or in 8 bit" report, and the tooltip fix (`99e997d`) only addressed the *viewport
is 8-bit* half of it.

**Action:** P4 gains an honesty requirement — the badge/tooltip must say 16-bit helps **gradients**,
not "is better". And a real decision to make explicitly: dual-plane-in-sRGB is a *workaround*; a
true linear `R16`/`Rgba16` array is the correct storage. Dual-plane was the right call to avoid
threading a binding through 26 WGSL references and 7 BGL sites, but if precision matters beyond
height maps, the array is the honest fix. **Not decided — flagging it as a choice, not an omission.**

### 2. Live/Bake is already an explicit user CONTROL, not just an internal tier

`ChannelEvalMode` (Live / Bake) exists and is user-chosen, not inferred. P4's "live vs final is a
quality tier" framing was written as though the tiering were still to be designed.

**Corrected intent, and it matters:** keep the explicit control. The rule P2 must enforce is
narrower and more useful than "tiering" —

> **Do not grow a second Live compositor.** Live may run at lower resolution / fewer octaves, but it
> must be the SAME compositor as the bake. A separate Live evaluation path is precisely how the
> G-buffer's inline composite came to disagree with the bake in the first place.

### 3. G-buffer `Rgba16Float` improves display precision — it does NOT finish P2

Correct, and worth stating plainly because the two are easy to conflate. R16F removes the 8-bit
banding floor *after* the map is sampled. It says nothing about **who composites the layers**.

P2 is unchanged: delete the G-buffer's inline layer composite, make the bake the single owner, and
have both the G-buffer and PT sample its result. R16F makes that result display better; it does not
make the bake the owner.
