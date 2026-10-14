# Material parameter panel — implementation spec

Target: FourthLayer AI (Rust / wgpu)
Status: design locked, implementation not started
Audience: implementing agent (Claude Code)

---

## 0. How to use this document

This spec defines **contracts and phases**, not APIs. The codebase already contains a node
system, a PBR shader, a path-tracing shader, and a DC-based SDF mesher.

**Before writing any code:**

1. Read the existing node system and report back its actual API surface — node creation,
   connection, evaluation, serialisation. Do not assume signatures from this document.
2. Read the existing PBR and path-tracing material definitions and report the actual
   channel/parameter names in use.
3. Read the DC extraction path and report where per-vertex attributes are currently written.

Do not begin Phase 1 until steps 1–3 are reported and the contracts in §1 are confirmed
compatible with what exists. If any contract conflicts with the existing architecture, stop
and raise it — do not silently adapt the contract.

**Anti-anchoring:** this document states intent. Where it names a type, field, or module, treat
that as a suggestion subordinate to existing project conventions. Where it states a *contract*
(§1), treat it as binding.

---

## 0.5 Escalation protocol

**This spec was written by someone who cannot see the codebase. Parts of it are probably
wrong. Finding that out is your job, and reporting it is more valuable than implementing
around it.**

Stop and report — do not work around, do not silently substitute — when any of the following
is true:

1. A requirement is **impossible** given the architecture, the GPU pipeline, or wgpu's
   constraints.
2. A requirement is **possible but disproportionately expensive** — it works, but costs a
   frame-time or memory budget out of line with what the feature is worth.
3. An acceptance criterion **cannot be met as stated**, even though the feature works. (Most
   likely in §7: several tolerances in this document are guesses.)
4. A contract in §1 **conflicts with something that already exists** in the codebase.
5. Implementing the spec as written would require a panel control with **no corresponding node
   type** (violates C1).

**Report format:** what was attempted, why it fails, the measured or estimated cost, and two or
three concrete alternatives with their trade-offs. Do not pick one and proceed.

**Explicitly not escalation-worthy:** naming, file layout, module structure, and any type in §3.
Use project conventions and keep moving.

**Prebaking is an acceptable answer.** If live evaluation of a generator is infeasible or too
expensive, prebaking it Substance-style is a legitimate outcome, not a failure. Say so, state
the cost that drove the decision, and see §7.5 for the fallback ladder. What is *not* acceptable
is shipping something labelled live that is actually stale, or shipping something that silently
degrades without telling the user.

---

## 1. Binding contracts

These four decisions are settled. Everything else follows from them. Do not relitigate them
mid-implementation; if one appears wrong, stop and raise it.

### C1 — Authority: the panel is authored intent, the graph is its expansion

- Panel edits deterministically generate and update nodes in the material graph.
- Every panel widget maps to exactly one named node type. **If a control has no node
  equivalent, it does not belong in the panel.**
- Once a slot's subgraph is hand-edited in the graph, that slot becomes `GraphDriven`. The
  panel then displays a badge with click-through to the node — **never a slider showing a
  value it does not control.**
- There is no bidirectional sync. There is no silent overwrite. Promotion to `GraphDriven` is
  one-way and requires explicit user action to revert (which discards the custom subgraph,
  with confirmation).

### C2 — Presets are shader permutations, not UI filters

- A preset declares an active channel set. That set **is** the pipeline permutation key.
- A diffuse-only material compiles a pipeline with no metalness/specular/normal sampling, no
  unused BSDF lobes, fewer bound samplers.
- Named presets are the hot, pre-warmed paths. Arbitrary channel combinations are legal but
  compile lazily and async against a fallback pipeline. A permutation change must never stall
  the viewport and never raises a user-facing error.

### C3 — Bake is a cache, not a mode

- The live definition is always authoritative. Baking populates a cache; it never replaces or
  consumes the generator.
- Consequences that must hold: baking is non-destructive and needs no confirmation; unbaking
  is cache invalidation; undo does not model bake as an edit; a file opens identically with or
  without its cache; a stale cache falls back to live rather than erroring.
- **Parity is tiered, not absolute.** The three rungs cannot be pixel-identical: texture bake
  has UV seams, a resolution ceiling, mip filtering, and dilation; vertex cache interpolates
  across the face; live is exact. Required instead:
  - Same evaluation code path for all three. One writing to a target, one to a vertex
    attribute, one inline. **Divergence caused by different code paths is a P0 bug.**
  - Vertex cache vs live: within tolerance on smooth regions; deviation expected and
    acceptable where detail is finer than the vertex spacing.
  - Texture bake vs live: tolerance scales with resolution and UV quality. State the
    tolerance per resolution in the test suite (§7) rather than asserting a single number.
  - **The UI must be honest.** The lower rungs are approximations and are labelled as such.
    An artist choosing a cheaper rung should know they traded fidelity for speed. Silent
    degradation is the failure mode to avoid.
- Baking must never be the *only* way to get a correct result — if a generator is
  approximation-only in a given path, that is a §0.5 escalation, not a quiet substitution.
  (Known exception: AO in the raster path — see §7.5.)

### C4 — Curvature is one contract with multiple backends

- One node type, one parameter set, one output space. Backends: DC-extracted mesh, half-edge
  poly, dense sculpt mesh.
- The unifying parameter is **radius in world units**, not resolution or ring count. Every
  backend honours it. This makes results comparable across representations and stable under
  subdivision-level changes.
- Each backend has a different valid radius floor. The floor is **surfaced in the UI**, never
  silently clamped.

---

## 2. Layout (what the panel is)

Single scrolling column, channel-major. Top to bottom:

1. **Header** (pinned, does not scroll)
   - Material name, preview swatch
   - Cost readout, quiet: `standard PBR · 4 samplers`
   - `⌗ Shader tree` button — icon + label

2. **Shader model** — segmented control: `PBR` / `Path tracing`

3. **Texture space** — material-level, collapsible
   - Projection (UV set / triplanar / box / camera)
   - Tiling (u, v), Offset (u, v)
   - All maps inherit this by default

4. **Channels** — one row per channel, collapsed by default
   - Collapsed row: `[chevron] [swatch] name ......... [slider | badge] [value]`
   - Badges: `map` (has texture), `graph` (GraphDriven), none (plain value)
   - Expanded row reveals the layer stack for that channel

5. **Add channel** — appends to active set, triggers permutation change per C2

6. **Filter** — `show only driven channels` toggle

### Rules

- **Maximum two levels of disclosure.** channel → layer. Image-adjust controls sit flat inside
  the layer, not in a third nested section.
- **One term per concept.** `Base color` is the channel; `amount` is the scalar inside it.
  There is no separate "diffuse", "diffuse colour", and "diffuse amount".
- Every channel row has a context menu: *edit in shader tree*, *connect to node…*,
  *break connection*, *copy channel*, *paste channel*, *reset*.
- Connection dot on the right edge of each row: filled = GraphDriven, empty = panel-driven,
  clickable = frame that node in the graph.

### Layer model (inside an expanded channel)

A layer is: **content + mask + blend mode + opacity**.

Mask is a *slot on the layer*, not a sibling entry in the stack. Never two kinds of row in one
stack — that makes ordering semantics ambiguous.

Content is one of:

| Content type | Controls |
|---|---|
| Image | file, projection override, tiling override, offset override, adjust (opacity, contrast, saturation, gamma, invert) |
| Generator | source (curvature / AO / thickness / position / normal direction), radius, range remap, blur, invert |
| Constant | value or colour |
| Procedural | (deferred — out of scope for v1) |

**Generators have no projection or tiling controls.** They are mesh-derived and have no meaningful
tiling. Do not ship dead controls on generator layers.

---

## 3. Data model

Sketch only — conform to existing project conventions.

```rust
struct Material {
    name: String,
    model: ShaderModel,              // Pbr | PathTracing
    texture_space: TextureSpace,     // inherited default
    channels: Vec<Channel>,          // ordered, active set only
    preset: Option<PresetId>,
}

struct Channel {
    kind: ChannelKind,               // BaseColor, Roughness, Metalness, ...
    state: ChannelState,
    layers: Vec<Layer>,              // empty = plain value
}

enum ChannelState {
    Value(f32),                      // or Color, per channel kind
    Layered,                         // driven by `layers`
    GraphDriven { node: NodeId },    // C1 — panel shows badge, not slider
}

struct Layer {
    content: LayerContent,
    mask: Option<LayerContent>,
    blend: BlendMode,
    opacity: f32,
    visible: bool,
}

struct TextureSpace {
    projection: Projection,
    tiling: [f32; 2],
    offset: [f32; 2],
}

struct Placement {
    space: Option<TextureSpace>,     // None = inherit from material
}
```

**Permutation key** (C2): derived, never stored as truth.

```rust
fn permutation_key(m: &Material) -> PermutationKey  // model + active channel bitmask
```

---

## 4. Phases

Each phase is independently shippable and independently testable. Do not start a phase before
the previous one's acceptance criteria pass.

### P0 — Contracts and data model

**Goal:** types exist, serialise, round-trip. No UI.

Deliverables: types from §3; `permutation_key`; serde round-trip; migration path from whatever
material representation exists today.

Acceptance: existing materials load into the new model and render identically to before.
Byte-identical round-trip on save/load. No UI changes visible.

Out of scope: any panel, any codegen.

---

### P1 — Read-only panel

**Goal:** the panel renders the material correctly. Nothing is editable.

Deliverables: header, shader model display, texture space group, channel list with collapse,
badges (`map` / `graph`), filter toggle, cost readout.

Acceptance: a material with 8 channels, 3 of them mapped, renders correctly. Collapsed state
persists per material across selection changes. Panel height with all channels collapsed fits
in a 900px viewport without scrolling.

Out of scope: editing, codegen, layers.

---

### P2 — Panel → graph codegen (C1)

**Goal:** panel edits generate nodes. This is the load-bearing phase.

Deliverables:
- Deterministic mapping: each panel widget → one node type. Document the mapping table in
  `docs/panel_node_mapping.md` as you build it.
- Editing a channel value updates the corresponding node.
- Hand-editing a slot's subgraph in the graph promotes it to `GraphDriven`.
- Panel shows badge for `GraphDriven` slots, with click-through.
- Reverting to panel control discards the custom subgraph, with confirmation.

Acceptance:
- Round-trip determinism: panel → graph → panel produces an identical panel state.
- No panel edit ever destroys a hand-authored subgraph without explicit confirmation.
- Every widget in the panel has an entry in the mapping table. **A widget with no node is a
  spec violation — report it, do not invent a node for it.**

---

### P3 — Texture space and image layers

**Goal:** images work end to end.

Deliverables: texture space group with inheritance; per-layer override with clear
inherited/override indication; image slot (load, thumbnail, path); adjust controls generating
colour-correct nodes; blend mode; opacity; layer reorder; multiple layers per channel.

Acceptance: adding an image to roughness produces the correct node graph and correct render.
Changing material tiling updates every inheriting map. Overriding one map's tiling does not
affect the others. Adjust controls map 1:1 to colour-correct node parameters.

---

### P4 — Shader model swap (additive)

**Goal:** PBR ↔ path tracing without data loss.

Deliverables: shared channels retain values and maps across the swap; path tracing *reveals*
additional channels (transmission, IOR, dispersion, SSS radius — confirm actual set against
the existing path tracer); channels unique to the inactive model are retained in data, hidden
in UI.

Acceptance: **PBR → PT → PBR is lossless.** Nothing is dropped, nothing is reset. This is the
single acceptance criterion that matters; if a map can be lost by toggling a control, artists
will never touch that control again.

---

### P5 — Presets as permutations (C2)

**Goal:** channel count actually costs GPU time, and presets actually save it.

Deliverables:
- Named presets: diffuse only, standard PBR, full PBR, glass, skin.
- Permutation key drives shader codegen — unused channels produce no texture fetch and no
  BSDF lobe.
- Pipeline cache keyed on permutation; disk-persisted.
- Named presets pre-warmed at startup.
- Custom permutations compile async against a defined fallback pipeline. **No error dialog,
  ever.** "No viewport hitch" is the goal but not an absolute — wgpu pipeline creation stalls
  on some backends regardless of threading. Define the fallback pipeline explicitly, measure
  the worst case per backend, and report it. If the worst case is user-visible, escalate per
  §0.5 with options (broader pre-warming, coarser permutation granularity, on-disk cache
  warming at load).
- Draw-call sorting by pipeline.
- Cost readout in the header reflects the live permutation.

Acceptance: a diffuse-only material measurably outperforms a full-PBR material on the same
geometry — record the numbers in the sprint log. Switching preset never stalls the viewport
longer than one frame. 40 diffuse-only props batch into one pipeline state.

---

### P6 — Masks and generators (C4)

**Goal:** curvature and AO masks, live.

Deliverables:
- Generator layer content type (§2).
- Radius in world units, honoured by all backends.
- Per-backend radius floor surfaced in UI: `min radius 0.8mm at current resolution`.
- Outputs: signed curvature, convex-only, concave-only. Ship all three — do not make the user
  build convex/concave from a remap.
- Normalised output: 0.5 = flat, above = convex, below = concave. Remap scale-aware via radius.

**Curvature backends:**

| Backend | Method |
|---|---|
| DC-extracted | Stamp normal-covariance / QEF eigenvalue spread to a vertex attribute **during extraction**. Do not differentiate the SDF twice. Normalise by cell size. |
| Half-edge poly | Analytic from topology — dihedral angle per edge, discrete mean/Gaussian curvature per vertex, on the **limit surface**. Radius → geodesic n-ring or cotangent-weighted neighbourhood. |
| Dense sculpt | Geodesic-ball gather, local quadric or normal-variation fit. Expensive. Vertex-rate only. |

**Known hazards — handle explicitly, do not discover these in QA:**
- QEF eigenvalue spread measures *feature-ness*, not scale-normalised curvature. Sharp edges
  saturate; broad curves barely register. Acceptable for edge-wear and cavity masks. Do not
  label the output "mean curvature".
- Adaptive octree ⇒ spatially non-uniform radius floor. Gathering across neighbouring cells is
  required; reading a single cell's QEF is not sufficient. Local cell size is the floor, not a
  global constant.
- Non-manifold vertices and QEF solutions escaping their cell produce undefined or misleading
  curvature. State and implement a clamping policy or these appear as bright speckles at thin
  features.
- Incremental extraction: a world-space gather straddling a dirty-cell boundary reads a mix of
  fresh and stale neighbours, producing a seam along the edit region. **Mitigation: stamp a
  small fixed-scale value per vertex at extraction time; widen to the requested radius later,
  at shading time, from the mesh — not from the grid.**

**AO — prebaked, Substance-style, in the raster path.** This is a deliberate exception to the
live-first ladder, not an oversight:

- True AO requires ray casts. In the **path tracer** it is close to free — it is already
  tracing, so evaluate it live there.
- In the **PBR raster viewport** there is no live option. Screen-space AO is a different
  quantity that varies with camera and therefore cannot drive a texture-space mask. So AO in
  the raster path is prebaked, with an explicit bake step and an explicit stale state, exactly
  as Substance does it.
- Mesh-level, not material-level — a mesh's AO is identical regardless of which material
  samples it. Per-material 4K bakes will exhaust VRAM.
- The UI must show that AO is baked and when it went stale. Do not present a stale AO bake as
  if it were live.

**Representation stacking:** when a mesh has multiple sources (sculpted poly base, meshed SDF),
default to evaluating against the **shading surface**. Allow the slot to pin a source
explicitly; sculpted detail on a poly base is the common case where base-cage curvature is
wanted instead.

Acceptance (revised down to what is actually achievable — the stronger versions of these were
not):

- Curvature on a DC-extracted mesh shows **no visible popping at rest** after re-extraction.
  Exact stability is not achievable: re-extraction moves vertices and can change cell
  subdivision, so feature-ness shifts slightly. During an active sculpt stroke this reads as
  shimmer. Mitigation: freeze generator evaluation for the duration of a stroke, update on
  release. If shimmer persists at rest, escalate per §0.5.
- Across backends, masks must be **rank-comparable**, not pixel-comparable. Edges read as
  edges, cavities as cavities, at the same radius on the same shape. Pixel comparability is
  not achievable — QEF feature-ness saturates on sharp edges and discrete mean curvature does
  not; they are different quantities.
- Changing subdivision level does not change the mask. This one is a hard requirement — the
  world-space radius contract exists to guarantee it. Failure here is a C4 violation.

---

### P7 — Bake tiers (C3)

**Goal:** three rungs — live → vertex cache → texture bake.

Deliverables:
- Per-generator setting, not per-material.
- **Vertex cache** is the middle rung and the default escape hatch: no UVs required, no
  resolution choice, no seams. Store in the existing sparse columnar vertex side-table.
- **Texture bake** requires a UV set and a resolution — export-oriented, not the primary path.
  Meshes are frequently unwrapped badly or not at all during modelling, so a bake button that
  requires clean UVs fails exactly when it is most needed.
- Staleness: mesh edit dirties the cache. If live evaluation is affordable, silently fall back
  to live and re-cache on idle. If not, show a stale badge with one-click rebake. **Never
  display stale data without saying so.**
- AO bakes are stored **on the mesh, not on the material** — a mesh's AO is identical
  regardless of which material samples it. Per-material 4K bakes will exhaust VRAM.
- DC-stamped curvature needs no bake and no invalidation: it is regenerated whenever the mesh
  it describes is regenerated.

Acceptance:

- Vertex cache vs live matches within the stated tolerance on smooth regions; deviation below
  vertex spacing is expected and documented, not a bug.
- Texture bake vs live matches within a resolution-scaled tolerance. Record the actual
  tolerance per resolution — do not assert a single number.
- Baking is undoable as a cache operation, not as a document edit.
- Deleting the cache changes nothing except performance and, where applicable, fidelity — and
  where fidelity changes, the UI said so beforehand.

---

### P8 — Shader tree window integration

**Goal:** the panel and the graph are one system.

Deliverables:
- Header button opens the graph framed on the material output node.
- Channel context menu → *edit in shader tree* opens the graph framed on **that channel's
  subgraph**, node selected.
- If the window is already open: focus and re-frame. Never spawn a second window. Label
  reflects state — `Open shader tree` / `Show in shader tree`.
- Connection dots on channel rows are a second route into the graph.

Acceptance: from any channel row, two clicks reach the relevant node with it selected and
framed. No unframed graph views. No duplicate windows.

---

## 5. Open decisions — answer before the phase that needs them

Do not invent answers to these. Raise them and wait.

| # | Question | Blocks | Notes |
|---|---|---|---|
| D1 | Does a new material open with the full PBR channel set at zero, or empty with channels added on demand? | P1 | Full set ⇒ panel is a stable scannable form, `add channel` is an edge case. Empty ⇒ `add channel` is the primary flow. Hard to retrofit either way. |
| D2 | Is any generator evaluated live at **pixel** rate, or is live always **vertex** rate? | P6 | Geodesic-ball curvature on a dense sculpt cannot be pixel-rate. If backends disagree on evaluation rate, the C4 contract leaks and "live" stops meaning one thing. |
| D3 | Exact path-tracing-only channel set. | P4 | Read from the existing path tracer; do not guess. |
| D4 | Is DC extraction incremental (dirty-cell only) or full? | P6 | Determines whether the boundary-seam mitigation is required or merely prudent. |
| D5 | Vertex attribute budget in the sparse side-table. | P7 | How many cached generators can coexist per mesh. |
| D6 | Is AO wanted in the raster viewport at all, or only in the path tracer? | P6 | If raster-only-when-baked is acceptable, the feature is straightforward. If artists expect AO to update as they model in the raster view, that expectation cannot be met and the UI must set it correctly. |
| D7 | Acceptable staleness window for prebaked generators during active modelling. | P6/P7 | Determines whether re-bake is on-idle, on-release, or explicit. |

---

## 6. Out of scope for v1

Named so they are not built by accident:

- Procedural texture layer content (noise, gradient, tile generators)
- Material layering / smart materials
- Per-face material assignment
- Texture painting
- Material library browser
- Substance `.sbsar` import
- Displacement adaptive tessellation control (channel exists; tessellation settings do not)

---

## 6.5 Prebake fallback ladder

When live evaluation of a generator is infeasible or too expensive, walk down this ladder and
report which rung was reached and why. Reaching a lower rung is an acceptable outcome; reaching
one silently is not.

| Rung | Behaviour | Use when |
|---|---|---|
| 1. Live, pixel rate | Evaluated in the shader per sample | Cheap and camera-independent. DC curvature, SDF field queries. |
| 2. Live, vertex rate | Evaluated per vertex, interpolated | Too expensive per pixel but cheap per vertex. Poly curvature, most masks. |
| 3. Cached to vertex attribute | Computed on edit, stored, invalidated on topology change | Expensive to compute, cheap to store, no UVs needed. Default escape hatch. |
| 4. Prebaked to texture | Explicit bake step, explicit stale state, Substance-style | Requires ray casts or global information. **AO in the raster path lands here by necessity.** |

Rules that apply at every rung:

- The rung is **visible in the UI**, per generator. An artist can always see whether what they
  are looking at is live or baked.
- Stale state is **never hidden**. A stale bake shows a badge and a one-click rebake.
- Dropping a rung to hit performance is a decision the user makes or is told about — never one
  the system makes quietly.
- A generator that can only ever be rung 4 (AO in raster) is documented as such rather than
  presented as a degraded version of a live feature.

---

## 7. QA specification

Written alongside implementation, not after. One suite per phase.

**Determinism**
- Panel → graph → panel round-trip produces identical panel state, for every channel type and
  every layer configuration.
- Serialisation round-trip is byte-identical.

**Non-destruction**
- No panel edit destroys a hand-authored subgraph without confirmation. Test every widget.
- PBR → PT → PBR is lossless. Test with every channel populated, mapped, and layered.
- Cache deletion produces no visual change.

**Bake parity (C3)**
- Automated image diff, live vs vertex-cached vs texture-baked, across the test-asset set.
- **Tolerances are per-rung and per-resolution, established by measurement.** The numbers are
  not in this document because the author could not measure them. Establish them, record them
  in the suite, and treat regressions against the recorded baseline as P0.
- Divergence traceable to *different code paths* between rungs is P0 regardless of magnitude.
  Divergence traceable to resolution, UV quality, or vertex spacing is expected and documented.

**Permutation (C2)**
- Every named preset compiles.
- Arbitrary channel combinations compile async without a viewport stall exceeding one frame.
- Unused channels produce no texture fetch — verify by inspecting generated shader source, not
  by inference.
- Recorded performance delta, diffuse-only vs full PBR, logged per sprint.

**Curvature (C4)**
- Same radius across backends on equivalent geometry yields **rank-comparable** masks — edges
  read as edges, cavities as cavities. Not pixel-comparable.
- Subdivision level change does not alter the mask. Hard requirement.
- No visible popping at rest after re-extraction. Shimmer during an active stroke is mitigated
  by freezing evaluation for the stroke duration.
- Radius below the backend floor surfaces a UI message and does not emit noise.
- Non-manifold vertices and escaped QEF solutions produce no speckles.
- Incremental re-extraction produces no seam at the dirty-region boundary.

**UI**
- All channels collapsed fits in 900px without scrolling.
- Maximum disclosure depth is two levels. Automated check on the widget tree.
- Every panel widget appears in `docs/panel_node_mapping.md`.
- No generator layer exposes a projection or tiling control.

---

## 8. Owner clarifications (2026-07-25, post-mockup review)

- **Per-map projection override is required, surfaced as an ADVANCED option per image layer**
  (not always-visible). Material Texture Space stays the inherited default; each image layer's
  expanded view carries a "Projection · inherited" row that becomes an independent
  projection/tiling/offset override when toggled. The collapsed caption shows the effective
  state (`UV map 1 · 2×2 · inherited` vs `Triplanar · 4×4 · override`). Generators still get
  no projection controls (§2 rule unchanged). This confirms and strengthens the §2/P3
  per-layer override — it is not optional scope.

## 9. Verification outcomes + owner decisions (2026-07-25) — BINDING for implementation

Codebase verification (full report in session log) — revised contracts:
- **C2 REVISED (owner-ratified by decision flow):** no per-material pipeline permutations —
  contrary to the one-ubershader + material-buffer architecture (materials are data, not
  pipelines; unused channels already cost ~0). V1 = honest COST READOUT in the header
  (maps bound / lobes active) + presets as data. Permutations only if profiling ever demands.
- **P4 REVISED:** there is no PT-only channel set — raster and PT share one material struct
  (per-tri PT materials). The mockup's "PBR / Path tracing" segmented control becomes a
  **ShaderType selector** (Standard PBR / Glass / Subsurface / Advanced); switching is
  inherently lossless (all fields always exist). P4's swap machinery is dropped.
- **P7 REVISED:** no vertex side-table exists. V1 vertex cache = the dead uv slot on SDF/DC
  vertices (2 scalars, zero format change). Real side-buffer = later, BGL-audited addition.
- **C1 confirmed the load-bearing build:** today graph→panel (bake overwrites map slots) is the
  INVERSE of the contract; C1 needs per-channel GraphDriven state + panel→node codegen + no
  silent overwrite (current silent-overwrite is the forbidden state, to be fixed in P2).
- D2: live pixel-rate only for SDF-analytic signals (calc_curvature); mesh generators are
  vertex-rate/baked. D3: answered (shared struct). D4: extraction IS incremental — seam
  mitigation REQUIRED. D5: budget = 2 floats (uv slot) for v1.

Owner decisions:
- **D1:** new material opens with the MAIN channel set (user deletes what they don't need).
  Plus: preset picker (dropdown) in the assign/set-material popup — includes user-saved
  presets (e.g. an "empty" preset). Presets are data (revised C2).
- **D6:** AO in raster = Substance-style PREBAKED with explicit bake + stale badge. No live
  raster AO pretense.
- **D7:** re-bake on EXPLICIT BUTTON (Substance-style). Baking rules apply to the RASTER path
  only — the path tracer evaluates AO live in-shader (already tracing; no bake needed there).
  The UI must reflect this split: AO channel shows "baked (raster) / live (path traced)".

## 10. P6 backend gaps (2026-07-25, written during P6 implementation — §0.5 report)

What the generator substrate REALLY is today: `node_graph_bake.rs` evaluates the material
graph on the CPU over a **planar UV domain** (`p = (u*2−1, v*2−1, 0)`, normal fixed
`[0,0,1]`) and installs the result into the channel's map slot. Generator nodes that read
the **body SDF field** (`SdfFieldSources.body_prims`, snapshotted from the ACTIVE body's
prims) project UV seeds onto the 3D isosurface, so they produce true surface signals — but
ONLY for SDF bodies. Everything below follows from that.

**Shipped in P6** (all bake-rate, honest v1 "live-ish": edits auto-rebake):
- **Curvature** → `SdfCurvature` node (uid 0 = whole body): real signed/convex/concave from
  the SDF field. Radius (world units) → `Epsilon`; floor = 1% of body AABB diagonal
  (`e = eps.max(diag*0.01)` in `sdf_curvature_masks`), surfaced as a caption.
- **Cavity** → `Cavity` node: concave-only sugar over the same field.
- Range remap (`Remap`), invert (`Invert`), three outputs (native SdfCurvature sockets +
  a normalising Remap for signed, 0.5 = flat). **Blur: omitted — no blur node exists (C1).**

**Gaps — NOT built, gated honestly in the UI:**

1. **AO has no real backend.** `NodeType::AmbientOcclusion`'s eval reads `sn_field_h` — a
   *synthetic sine demo field*, not the body. Shipping it would bake a decorative pattern
   labelled "occlusion" (forbidden: fake-live). Combo entry is DISABLED with tooltip; the
   D6/D7 Substance-style prebaked mesh-AO (ray-cast bake, stored on the mesh, stale badge +
   explicit Rebake) is follow-on work: a real AO baker over the DC/poly mesh + a mesh-level
   cache keyed on `BodyContentId`/bake seq. PT needs none of it — it integrates occlusion
   live in-shader. The stale-badge machinery was NOT built (nothing real to badge yet).
2. **Slope is constant on the bake domain.** The planar bake passes normal `[0,0,1]`, so
   `NodeType::Slope` bakes a constant. Disabled entry + tooltip. Follow-on: evaluate
   generators against mesh normals (needs a mesh-domain bake, see gap 4).
3. **Poly / voxel bodies get garbage, gated.** With no SDF prims, `SdfFieldSources
   .has_field()` is false and Curvature/Cavity fall back to the same synthetic demo field
   (verified in `eval_node`: `sdf.has_field()` branch). The panel therefore disables the
   Generator layer type and both sources whenever the active body has no SDF prims
   ("SDF bodies only" tooltip), and an existing generator layer on such a target shows a
   warning caption. Follow-on: half-edge dihedral curvature backend (C4 poly rung).
4. **No mesh-domain generator rung.** DC-extraction curvature stamping (the dead uv slot,
   §9/P7), half-edge dihedral curvature, and dense-sculpt geodesic gather all remain
   unbuilt — P6 ships only the SDF-analytic rung (consistent with D2: mesh generators are
   vertex-rate/BAKED, and that bake infrastructure is P7 scope).
5. **Active-body snapshot scope.** `snapshot_sdf_field_for_material` reads the ACTIVE
   body's prims, not the body the material is assigned to — a material edited while a
   different body is active bakes against that other body's field (pre-existing P3-era
   behavior; the P6 UI gate reads the same source so gate and bake always agree). Follow-on:
   resolve the owning body from the material assignment.

6. **P7 VertexCache rung — §0.5 escalation (2026-07-25, written during P7 implementation).
   The vertex rung was NOT built; P7 ships the per-generator rung selector with the texture
   rung only and the vertex entry disabled with an honest tooltip.**

   *What was attempted:* trace a route for CPU-evaluated `sdf_curvature_masks` values
   (the ONE eval fn, `node_graph_bake.rs`) to reach the deferred shade point via the dead
   uv slot (§9/D5). The READ half of the route is clean and proven by the voxel-paint
   precedent: `RasterVertex.uv` flows vertex→fragment in `render/src/gbuffer.wgsl`
   (`out.uv = in.uv` at ~:301; the paint path bit-packs a colour word into `uv.x` +
   flag `uv.y`, carried FLAT, gated per instance by `INSTANCE_FLAG_VERTEX_COLOR`,
   `mesh_gpu.rs:332`, consumed at the fragment material eval ~:463-495). A parallel
   instance-flag bit + a sentinel map-slot read would compose without conflicting with
   voxel paint (an SDF DC mesh is never a painted voxel mesh).

   *Why it fails:* the WRITE half has no clean route. The production SDF display mesh is
   GPU-RESIDENT end to end: `graphics/src/sdf_mesh.wgsl` writes the 8-float vertices
   (uv hardwired `0.0` at ~:399/:540) into `SdfBodyMeshSlot.verts`
   (`app/src/sdf_gpu_mesh/resident.rs:1596`, usage STORAGE|VERTEX|COPY_SRC — **no
   COPY_DST**), drawn via `draw_indexed_indirect` (`raster_gbuffer.rs` G2 path); vertex
   count lives only in GPU-side atomic counters. Vertices never reach the CPU in the
   display path (readback is test/oracle/fold-only). Stamping CPU-evaluated values would
   require, per re-extraction (which happens continuously during gizmo drags — partial
   rebakes, the exact readback stalls the resident-mesh work removed): read back counters
   + all vertex positions, evaluate on CPU, and scatter-write uv back into a buffer with
   no write path. The only frame-budget-compatible alternative is porting
   `sdf_curvature_masks` to WGSL inside the mesher — a SECOND evaluation code path, i.e.
   this spec's own C3 P0 bug class by definition. Not built, per No Hacks.

   *Estimated cost of doing it properly:* (a) readback route ≈ per-rebake async map of
   verts (MBs at 512³) + CPU field eval per vertex + a new COPY_DST/compute-scatter write
   path + generation-counter invalidation — reintroduces the hitching class; (b) WGSL port
   ≈ small code, permanent dual-maintenance parity liability (forbidden).

   *Alternatives for a future P7b:* (1) evaluate ONCE in WGSL and make it the single
   source for BOTH rungs — move the texture bake's curvature eval to the same GPU kernel
   the mesher calls, restoring one-code-path at a higher level (large: GPU material bake);
   (2) piggyback the existing lazy readback: stamp only when/where a CPU mesh copy already
   exists (fold/modifier/detail-lo paths) and label the rung "available after readback" —
   honest but partial; (3) a dedicated post-mesh compute pass writing a separate small
   per-vertex storage buffer (BGL-audited addition, §9's "real side-buffer = later"),
   fed by a GPU eval — same dual-path problem unless (1) lands first.

   *What shipped instead:* `GeneratorRung { Auto, VertexCache }` on the Generator layer
   (serde-default, doc-JSON safe; round-trips through the node-label flag so
   panel→graph→panel stays identical); rung selector on the generator row with captions
   "Texture (baked) — updates on edit" / "Vertex — follows the mesh, approximate below
   vertex spacing" (disabled, tooltip pointing here); collapsed caption shows the rung
   (§6.5 visibility). C3 held by test: rung switch changes lowering by exactly the label
   flag (structural one-code-path assertion), and the serialized doc differs by exactly
   the rung token — no cache payload in FL4MPNLS.
