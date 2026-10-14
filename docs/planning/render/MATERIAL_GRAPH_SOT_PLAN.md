# Material Architecture — NodeGraph as Source of Truth (2026-08-15, owner-directed)

**Owner call:** *"the source of truth needs to be the node graph, much more powerful."* That is the
Substance Designer / Blender model — the graph IS the material; every panel is a view onto it.
This plan makes that true in the tree, by DELETION rather than addition.

**Why now:** measured today, the material subsystem is ~16,000 lines with **four** representations
of one material and **no declared authority** between them:

| representation | role today |
|---|---|
| `MaterialPanelDoc` | what the UI edits (12k lines of panel around it) |
| `PbrMaterial` | what the renderer reads |
| `NodeGraph` | what the bake evaluates |
| baked texture slots | what the GPU samples |

Seven conversion functions (`lower_to_material`, `lower_to_graph`, `derive_doc_from_material`,
`refresh_doc_from_material`, `reconcile_doc_with_graph`, `pull_from_pbr`,
`sync_pbr_into_material_graph`) called from **56 sites**, and the question "is this channel baked,
live or graph-driven" is re-derived in **8 files** (19 sites in `win_node_editor.rs`, 18 in
`lower/mod.rs`, 15 in `bake_maps.rs`). With no authority, no conversion has a *direction*, and two
paths can disagree with neither being wrong.

**This is not theoretical.** 2026-08-15's curvature bug was two source builders reaching opposite
conclusions about the same body; the roughness-map report is a 3x3 matrix (`Value`/`Layered`/
`GraphDriven` x `Fast`/`Subgraph`/`Stub`) where only some cells write the material and the rest
defer to a bake. "It depends which of nine cells you are in" IS the overcomplication.

---

## 0. The good news, verified in-tree

The graph can already express the panel — this is a **re-wiring, not a re-build**:

- `regen_channel_subgraph(g, kind, ch, ts, mat)` (lower/maps.rs:140) — panel channel -> graph nodes.
- `parse_panel_channel(g, kind)` (:982) — graph nodes -> panel layers.
- One label-tag grammar with ONE parser: `mp:<kind>:<role>:<layer_id>` (`panel_tag` /
  `parse_panel_tag`), already reused by the GPU curvature dispatch.
- Round-trip already pinned by `p2_round_trip_panel_graph_panel` (values, shader type, texture
  space, single-image layers) and `panel_blendmode_round_trips_mixblend`.
- The graph ALREADY persists per material (`collect_node_graphs` -> `MaterialGraphEnvelope`
  {azra_owner:"material", material_id, graph} -> `NodeGraphRecord`).

So the target state is reachable by pointing existing machinery in one direction and deleting the
other.

## 1. Target architecture

```
            NodeGraph  (SOURCE OF TRUTH, persisted)
                 |
     +-----------+------------------+
     |           |                  |
  PbrMaterial  panel view       baked slots
  (derived     (derived via     (evaluated
   render      parse_panel_      outputs)
   cache)      channel)
```

- **Write path (one direction only):** UI edit -> mutate the GRAPH (`regen_channel_subgraph`) ->
  derive `PbrMaterial` + queue bakes. The panel never writes `PbrMaterial` directly.
- **Read path:** the panel renders `parse_panel_channel(graph)`. `MaterialPanelDoc` degrades from
  authored state to a **frame-local projection** — a cache, not a document.
- **`PbrMaterial` becomes a derived render cache**, regenerated from the graph. Its `TexMap` slots
  are outputs, never authored state.
- **The 3x3 matrix collapses:** a channel is exactly what its subgraph says. `Value` = a constant
  node. `Layered` = a composite chain. `GraphDriven` = the general case. One representation, so
  `ChannelState` disappears as a stored discriminant.

## 2. Risks named up front (each gets a gate)

- **R1 Fidelity.** `parse_panel_channel` must recover EVERY panel construct the graph can hold, or
  making the graph authoritative silently loses authored intent on load. Today's round-trip test
  covers the "P2 subset" only. **Gate G1 is fidelity, before any deletion.**
- **R2 Persistence.** `FL4MPNLS` (panel doc) currently carries authored state. Once the graph is
  SoT it becomes redundant — but it must keep LOADING forever (old files) while no longer being
  written as authority. Migration = read doc -> build graph -> from then on graph wins. Tail
  discipline unchanged (never in-blob).
- **R3 Perf.** The panel currently reads a struct; it would read a parsed graph per frame.
  `parse_panel_channel` walks all nodes per channel. Needs a generation-keyed memo — and NOT one
  keyed on a mutation counter (that trap cost a full day on 2026-08-13; see the perf doc).
- **R4 Round-trip drift on labels.** The tag grammar is a string label; a renamed node breaks the
  association. Gate G1 must include a "node relabelled by hand" negative test.
- **R5 Non-panel graphs — and this risk GROWS with the editor work.** Users author arbitrary graphs
  in the node editor. Panel projection must degrade gracefully ("graph-driven — edit in node
  editor") rather than mangle them. **G8 (Reroute/Frame/Group) and G9 (node breadth) each widen the
  set of graphs the panel cannot project**, so R5's fallback is not a nicety to add later: it is a
  precondition for shipping either. G1 therefore pins the DEGRADE path, not only the round-trip.
- **R6 (new) — the editor is the UI after G3.** Flipping the write path while the editor is
  single-select would take away the panel's authority and hand users a worse tool. Selection-set +
  keymap is a PREREQUISITE of the flip, not a follow-up. This reorders the plan (see §4).

## 3. Gates

### G1 — Fidelity proof (no production change)

**RUN 2026-08-15 — gate output.** Four tests landed in `lower/mod.rs::g1_fidelity`. The two
safety-critical properties PASS: a hand-relabelled node degrades instead of mis-associating (R4),
and hand-authored nodes survive a panel regen byte-unchanged (R5). Three gaps found, each now a
failing test that stays red until fixed:

1. **Invisible layers are DELETED by the graph representation** (`regen_channel_subgraph` skips
   `!visible`; a 3-layer stack round-trips as 2). Under SoT, hiding a layer then saving would
   permanently destroy it. Fix: emit hidden layers fully (content + mask + mix carrying
   blend/opacity) tagged with an `h` flag in the grammar's 4th field, wired OUT of the composite
   chain; parser reads the flag → `visible:false`. Evaluation is unchanged (dead branch).
2. **Mask generators lose `output`/`remap`/`sharpness`** — a Curvature mask even comes back
   `ConcaveOnly` instead of `Signed`.
3. **AO/Position content generators lose `radius`/`sharpness`.**

Root cause of 2+3 (found, not guessed): `emit_mask_content` is a HAND-COPIED, degraded duplicate
of the content-path generator emitter — no Remap chain, sharpness only for Direction, no AO
radius.

**CLOSED 2026-08-15 (same session, three verified batches; gate + 64 material + 66 bake + 12
curvature + 107 render tests green):**
- **A (mask fidelity):** mask emitter now emits sharpness for Curvature(4)/Cavity(2)/Thickness(3),
  mirrors `sdf_curv_region` from the output selector, and the mask Remap carries the AUTHORED
  [min..max] ALWAYS (it used to emit identity, sometimes — the authored knob was silently ignored
  in mask evaluation, the owner-reported "mask does not use the same curvature as the generator"
  class). Mask parser reads output from the tag flag (was hardcoded ConcaveOnly), remap from
  `mskrm`, sharpness from the sockets.
- **B (inexpressible params):** AO/Position radius+sharpness have no sockets on their nodes — the
  authored values now RIDE THE TAG FLAG (comma-packed `r…,s…`), lossless round-trip while backends
  remain free to ignore them; Position's axis parse made comma-safe (it silently fell back to the
  wrong axis on the packed flag).
- **C (hidden layers):** hidden layers EMIT their full node set with an `h` flag on the mix and
  are excluded from the composite chain — evaluation byte-identical to the old skip, but the graph
  is no longer lossy. Parser recovers `visible:false` from the flag.
- NOTE: the full shared-emitter extraction (one function for both paths) was NOT done — the two
  emitters now have identical FIDELITY but remain two functions. Folded into G2's scope, where the
  one-decision-function work already touches these files.

**G1 IS CLOSED.** The graph can now hold everything the panel can author, hand-authored nodes are
safe, and the failure mode of the flip (silent loss of authored intent) has a green gate in front
of it.
Extend the round-trip to the FULL panel expressiveness: multi-layer stacks, masks, generators,
blend modes, opacity, per-layer placement/space overrides, 16-bit channels, every `ChannelKind`.
Property test: random doc -> `regen_channel_subgraph` -> `parse_panel_channel` -> identical.
Plus R4's relabel negative test, and **R5's degrade test**: a channel whose subgraph contains a
hand-authored node (and, once G8 lands, a Group or Reroute) must project as "graph-driven" and be
left BYTE-UNCHANGED by a panel round-trip — never silently rewritten. **Any gap found here is fixed
in G1** — it is the whole risk.

### G2 — One decision function
Replace the 8-file "baked / live / graph" derivation with ONE query on the graph
(`channel_source(graph, kind)`), called by the panel, the bake dispatcher, the curvature
generator, the mask stack and the renderer. Delete the rest. No behaviour change intended;
`p2_round_trip` + bake tests are the guard.

**CLOSED 2026-08-15 (two batches, all guards green, zero behaviour change).**
- **Batch A — the one query + six reroutes.** `ChannelSource { Unwired, PanelStack, HandWired }`
  + `channel_source(g, kind)` live in `lower/maps.rs` beside the tag grammar it reads. Rerouted:
  `parse_panel_channel`'s gate; `panel_generated_mask`; `reconcile_doc_with_graph`'s promotion
  predicate (was the inline `master_socket_driven && !panel_authored_socket` combinator);
  `derive_doc_from_material`'s `driven` closure (now CHANNEL-keyed — consumers stop carrying the
  kind→socket mapping). Deleted the two duplicate mask builders: `bake_dispatch`'s inline
  `BakeChannelMask{…}` was a BYTE-DUPLICATE of `channels_from_graph` (the classic two-builder
  drift trap), and `win_node_editor`'s map-clear block re-derived the same socket→slot OR-folds
  INVERTED — both now call `channels_from_graph`, plus `needs_bake` (nine-socket OR) collapsed to
  `channels_from_graph(g).any()`. Still-legitimate primitive uses: `channel_source`'s own body,
  `strip_channel_subgraph`'s wire guard, and bake-internal socket queries (bump-only test)
  NEXT TO the one mask builder in bake_maps.rs.
- **Batch B — the one generator emitter (deferred here from G1).** The six-arm generator node
  emission (Curvature/Cavity/Direction/AO/Thickness/Position) was copied between the content
  path and the mask path — the copy pair whose drift WAS the owner's "mask does not use the same
  curvature as the generator" bug. Now ONE `emit_generator_node(g, kind, lid, pos, for_mask, …)`:
  role prefix (`msk`), node position, optional radius drive (content-only today) and Position's
  mask-side Scalar-Y tap are parameters; emission order is preserved so deterministic codegen and
  every round-trip stay bit-identical.
- **Guards at close:** material_panel + node_graph_bake + poly_curvature 144/144 · render 107.

### G3 — Flip the write path
UI edits mutate the graph; `PbrMaterial` is derived from the graph
(`derive_pbr_from_graph`), not from the doc. `lower_to_material` becomes graph->material.
Delete `refresh_doc_from_material` and `reconcile_doc_with_graph` (the reverse syncs that can
fight the forward path). **Verify:** every material test green; a knob scrub still updates the
viewport same-frame (the roughness report's actual symptom).

**CLOSED 2026-08-15 — with two owner-visible amendments to the gate text above.**
- **What shipped.** (1) `NodeGraph.shader_type` (`#[serde(default)]`, JSON-safe) — the one
  authored material property the graph could not express; `pull_from_pbr`/`push_to_pbr` carry it
  both ways. (2) `mp_commit_doc_edit` now DERIVES the material back from the graph after every
  panel commit (`push_to_pbr` last): the graph is the last writer of shader_type and every
  unwired scalar, and **a wired Master socket wins structurally** (`is_input_connected` skip in
  `push_to_pbr`) instead of by reconcile bookkeeping. (3) `refresh_doc_from_material` is
  DELETED. Its three call sites became forward reads: `refresh_doc_values_from_graph` (panel
  Values follow node-editor Master-default scrubs; graph→doc projection) and
  `heal_doc_fast_texids` (the async-upload hole: `mp_mark_hand_loaded_map` authors `tex_id: 0`
  until the decode lands — the heal ADOPTS the landed slot id only when the doc's is still 0 and
  only on Fast channels, so it can never overwrite authored state nor adopt a Subgraph bake
  composite). (4) Undo's restore-side re-sync loop (refresh docs from materials + pull materials
  into graphs) is deleted outright — docs, graphs and materials are snapshotted in the SAME
  frame, so the restored triple is consistent by construction; the capture-side heal still runs
  before serialize.
- **Amendment 1 — `reconcile_doc_with_graph` stays (deletion moves to G5).** Since G2 rerouted
  it onto `channel_source`, reconcile is a FORWARD projection: it reads the graph and writes doc
  state badges, never the material or the graph. It cannot fight the write path. It dies with
  `ChannelState`-as-stored-state in G5's reap. Deleting it in G3 would leave stale GraphDriven
  badges after hand edits with no replacement until G4.
- **Amendment 2 — Fast single-image TexMap install stays doc-side until G4.**
  `regen_channel_subgraph` emits SUBGRAPH shapes only; a Fast channel has no graph nodes today,
  so a graph-derived TexMap install has nothing to read. Widening regen to emit Fast channels
  changes bake gating (`channels_from_graph` would see those sockets wired) — a behaviour change
  G3 must not make. **G4 prerequisite: regen emits Fast channels as img-node chains** (G4's
  doc-from-graph projection needs them regardless); the install derives from the graph then.
- **Pins:** `g3_flip` (4 tests): shader_type mat→graph→mat; wired-socket-wins + unwire
  fallback; forward Value refresh reads Master defaults; heal adopts ONLY zero tex_ids.
- **Suites at close:** full app run 901 passed / 7 failed — the failure set is EXACTLY the
  known pre-existing reds (dc_baseline legacy .fl4, 2× poly_knife owner bevel-WIP, 4× intrude/
  bevel-segment fixtures). Zero new. Same-frame scrub preserved: the flip adds only synchronous
  CPU value copies inside the commit, before dirty propagation.

### G4 — Doc becomes a projection
`MaterialPanelDoc` built on demand from the graph with an R3 memo. `FL4MPNLS` read-for-migration
only. **Verify:** old file with a panel doc loads, projects identically, and re-saves with the
graph as authority; panel interaction has no per-frame allocation.

**SUBSTANTIALLY CLOSED 2026-08-15 — the projection is LIVE.** `mp_current_doc` now RETURNS the
graph projection (`derive_doc_from_material` over parse + `channel_meta` + graph-level fields)
for every material that has a graph, memoized on `material_generation` — the trusted
invalidation signal bumped by every material/graph commit and explicitly on undo, never a UI
mutation counter (the 2026-08-13 trap). Stored docs remain as (a) the source for materials
WITHOUT graphs and (b) edit-input state. Fidelity work the flip forced, each pinned:
- **Graph homes (batch A):** `NodeGraph.texture_space` + `generator_mode` +
  `channel_meta: BTreeMap<kind, {bit16, live, disabled}>` (BTreeMap so graph JSON stays
  deterministic for the regen no-op compare). Doc-level fields ride the graph on every regen.
- **The eye toggle, evolved (MPEYEOFFREBAKE final form):** a disabled channel EMITS its chain
  but leaves the master socket UNWIRED — inert for every bake decider BY CONSTRUCTION (no
  decider consults the eye), while the graph keeps the layers the projection needs. Re-enable
  restores the wire. All-hidden stacks (Stub/NoVisibleLayers) codegen the same way.
- **Recovery gate:** `channel_meta` presence = explicit "panel authored this" marker
  (regen writes, strip removes, hand graphs never have it) — recovery applies ONLY to
  deliberately UNWIRED sockets. A WIRED-but-not-PanelStack socket (hand node spliced in)
  still degrades: recovering "the layers without their edit" would let regen destroy the
  user's node — the R5 line, pinned by the G8 reroute-splice test.
- **Projection fidelity gaps closed:** parse recovered `id: 0` (every projected doc would
  renumber) → now the tag lid; authored DRIVES were dropped (`drives: Vec::new()`) → recovered
  from the connection into the generator's radius socket via ONE shared
  `generator_drive_socket` mapping (emit + parse); and `panel_authored_socket`'s upstream walk
  no longer traverses DRIVE edges — a driven channel stayed classified HandWired before, which
  the stored doc masked and the projection would have surfaced as a lost layer view.
- **Suites:** full app sweep 913 passed / 7 failed — the failure set EXACTLY the known
  pre-existing reds; g4_projection + evolved eye pin + all material suites green.
**CLOSED 2026-08-16 (G4-final: the load migration).** `mp_sync_doc_to_graph` (the sync half of
`mp_lower_rich_channels`, split so migration CANNOT enqueue bakes — N bakes at load was the
measured freeze risk this load path already documents) runs at `project_load` for every
material with a stored doc: old files (Fast-only materials with no graph, pre-G4 graphs
without `channel_meta`/`texture_space`) become projection-backed the moment they load — R2's
"read doc → build graph → from then on graph wins", literally. The projection memo clears with
the stores (a loaded scene's `material_generation` could coincidentally match a stale entry).
Verified by composition (every part pinned) + the full sweep's load-path tests: 914 passed /
7 failed, the failure set exactly the pre-existing reds.
**Explicitly deferred INTO G5's reap:** retiring the FL4MPNLS write (docs are redundant on
disk for graph-backed materials post-migration; stopping the write is deletion work and
belongs with the reap); the R3 zero-alloc bar (the memo clones per read — PARITY with the old
stored-doc clone, not a regression; true zero-alloc needs the panel to render from `&doc` and
clone only on edit — a UI refactor that belongs beside the reap). Noted edge: a
generator_mode edit on a scalar-only doc that already has a graph skips regen and reverts on
projection (narrow — generator_mode only matters with generator layers).

**PREP DONE 2026-08-15 (the G3-amendment prerequisite): regen emits FAST channels.**
`regen_channel_subgraph` now accepts `ChannelLowering::Fast` (single plain image) — the graph
holds every panel construct, which is what "doc as projection" requires. The install and bake
behavior is UNCHANGED by construction: the TexMap install stays `lower_to_material`'s direct
Fast arm (native-res tint-Mult), and every bake DECISION now goes through
`bake_channels_from_graph(graph, doc)` = wired mask MINUS `panel_fast_channel(g, kind, ts)`
(PanelStack whose projection lowers Fast — composes the G2 query + the ONE lowering classifier,
so it cannot disagree with what lower will do). Three deciders rerouted: `mp_lower_rich_channels`
(`needs_codegen` widened to Fast, `needs_map_bake` subtracted), the bake dispatch `full_ch`, and
`win_node_editor`'s `needs_bake`. `channels_from_graph` stays the RAW wired mask for
ownership/map-clear logic — a Fast install must keep its slot. Side benefit: the async-upload
regen now writes the tex_id into the graph's img node, so graph state stays fresh through
uploads. Pins: `g4_prep` 2/2 (Fast regens + projects back with tex_id/strength; bake mask
subtracts Fast while the Subgraph channel beside it keeps baking; wired mask keeps both). Full
app sweep: 910 passed / 7 failed — the failure set is EXACTLY the pre-existing reds, zero new.

### G5 — Reap
Delete the now-unreachable lowering branches and `ChannelState` as stored state. Success metric is
a LINE COUNT DROP in `material_panel/` with all tests green — if the subsystem does not shrink,
the collapse did not happen and the plan failed.

## 3b. Node EDITOR parity with Blender (owner-directed 2026-08-15)

Making the graph the source of truth **promotes the node editor to the primary material UI**.
Its quality stops being optional the moment G3 lands, so parity gates belong in this plan, not a
separate one.

### Measured state (2026-08-15)

Editor is `shader_nodes/editor.rs` (2113 lines) + `win_node_editor.rs` (552, window shell).

**The structural root of nearly every gap: selection is `selected_node: Option<u32>` — ONE node.**
Box select, multi-move, multi-duplicate, multi-delete, framing and grouping are all *selection-set*
operations; none can exist until selection is a set. Fix that first or every parity item re-fights
it.

| Blender verb | in tree (updated 2026-08-15 after G6+G7) |
|---|---|
| box / rubber-band select | ✅ G6 |
| `G` move | ✅ G6 (`R`/`S` on nodes still ❌ — low value, deferred) |
| `Shift+D` duplicate | ✅ G6 set-based (`Ctrl+D` alias kept) |
| `X`/`Del` delete | ✅ G6 set-based, MaterialOutput protected |
| `M` mute node | ✅ G6 — evaluator-level bypass |
| `Ctrl+RMB` drag = cut links | ✅ G7 |
| `F` connect selected | ✅ G7 (2 selected, left→right, first free compatible input) |
| drop node ONTO a link to insert | ✅ G7 |
| link-drag-search (drag from socket → search) | ✅ — ALREADY EXISTED (`LinkDropMenu` with search + auto-wire); this table was stale |
| Reroute node | ❌ (G8) |
| Frame node (visual grouping) | ❌ (G8) |
| Node Group (`Ctrl+G`) + group I/O | ❌ (G8) |
| `H` collapse | ✅ G6 — param rows hidden, sockets stay live |
| snapping / auto-offset | ❌ |
| pan / zoom | ✅ |
| Add-menu search | ✅ |

**Node set: 43 types.** Strong on AZRA-specific (Curvature, Cavity, AO, Slope, SDF family) and on
procedurals (Noise, Voronoi, Musgrave, Wave, Brick, Gradient). Missing Blender staples, all absent
from `node_type.rs`: Checker, Magic, White Noise, Fresnel, Layer Weight, Geometry, Hue/Saturation,
Gamma, Map Range (we have `Remap` — decide alias vs port), Float/RGB/Vector Curve, Bump, Normal
Map, Displacement, Separate/Combine RGB, Vector Rotate, plus the structural Reroute / Frame /
Group.

### Gates (run AFTER G1 fidelity; they change the graph the fidelity test pins)

**G6 — Selection set + Blender keymap.** `selected: IndexSet<NodeId>` replacing the single slot;
box select, `Shift`-click extend, `G` move, `Shift+D`, `X`, `M` mute (bypass = pass first input
through, Blender semantics), `H` collapse. **Verify:** headless keymap test per verb; a
multi-select move preserves relative offsets; mute passes through and is honoured by the evaluator
(a muted node must not just be invisible — the WGSL/bake path has to skip it).

**CLOSED 2026-08-15 (three batches, same session as G1).**
- **Batch 1 — ops + evaluator (headless).** `Node.muted` / `Node.collapsed` (`#[serde(default)]`
  — graph JSON is name-tagged serde, additive-safe, never bincode). `NodeGraph::remove_nodes`
  (MaterialOutput protected), `duplicate_nodes` (relative offsets kept; links BETWEEN duplicates
  re-created, external wires NOT stolen), `toggle_mute` (uniform set toggle, clears `wgsl_cache`),
  `toggle_collapse`. Mute is an EVALUATOR property: `eval_node` bypass arm writes input-0 through
  to all outputs; `extract_live_gpu_noise` bails when any node is muted (both-paths rule — the GPU
  fast path has no mute concept, so it must decline the graph, not ignore the flag). Pinned by
  `node_graph_bake::g6_ops` (4 tests: eval passthrough + un-mute restore, dup offsets/links,
  X protects output, muted graph declines the live-GPU path).
- **Batch 2 — selection set + keymap.** `NodeDragState.selected: HashSet<NodeId>` with
  `selected_node` redefined as ACTIVE (Blender's active+selected model). Shift-click toggles
  membership; plain click replaces only when the node is unselected (so a multi-selection can be
  dragged); dragging any selected header moves the whole set by one shared delta. Set-based
  `X`/`Del`, `Shift+D` (duplicate becomes the selection), `M`, `H`, `G` (free move, LMB confirms,
  Esc restores captured origins). Legacy single-node arms kept for the empty-set case.
- **Batch 3 — canvas verbs + honest visuals.** LMB box select on empty canvas (pan is MMB/Alt+LMB
  so LMB was free; press-on-node disarms; 4px threshold so a click never reads as a box; Shift
  extends; marquee suppresses the click-deselect). Empty-canvas click now clears the SET, not just
  the active slot. Muted = dim overlay + `M` badge (indication only — the bypass lives in the
  evaluator). Collapse = param rows hidden (`param_rows = 0` is the existing single gate for the
  whole param-widget block), socket rows stay drawn and connectable; wire endpoints
  (`node_input_pos`/`node_output_pos`) depend only on socket-row index so no noodle can detach —
  deliberately NOT Blender's header-stacked sockets, which would fork the socket-position math.
- **Suites at close:** g6_ops 4/4 · g1_fidelity 4/4 · material_panel 64 · node_graph_bake 70 ·
  render 107 · shader_nodes 5. (`cargo test -p app --bins` — the committed example
  `bevel_fl4_repro.rs` references `app::` as a lib the package doesn't have and never compiled;
  pre-existing, owner's `d17369a`.)

**G7 — Link ergonomics.** `Ctrl+RMB` drag cut, `F` connect, drag-node-onto-link insert,
link-drag-search from a socket. **Verify:** cut removes exactly the crossed links; insert rewires
in→node→out preserving socket types; every op is ONE undo entry.

**CLOSED 2026-08-15.**
- **Link-drag-search already existed** (`LinkDropMenu`: drop a pending wire on empty canvas →
  searchable compatible-node menu that auto-wires). The parity table above was stale — corrected,
  nothing built.
- **Per-op undo (the gate's third clause, and it covers G6's ops too).**
  `NodeEditorOutcome.discrete = changed && !field_changed` — a NEW `field_changed` flag marks
  frames where a node field's VALUE scrubbed (narrower than `field_interact`: hover must not
  suppress an undo entry). `win_node_editor` calls `transaction_snapshot_only("Node graph edit")`
  BEFORE `commit_material_node_graph_edit` inserts the working copy — the store still holds the
  pre-gesture graph at that point, so the snapshot captures exactly what Ctrl+Z must restore, with
  zero pre-frame cloning. Scrubs skip the entry (as they always have). **Known gap:** prim
  override graphs (`ui.node_graphs`) are not captured by `snapshot_material_ui` — editor undo is
  material-target only; prim-graph undo needs its own snapshot channel (queued).
- **Ctrl+RMB cut.** Freehand stroke (4px-deduped polyline, red); on release each connection's
  noodle is sampled with `draw_noodle`'s OWN control points (`noodle_crosses_stroke` — kept in
  lockstep) and segment-segment tested: exactly the crossed set is removed. Bare-RMB socket
  disconnect kept — the cut owns the button only while Ctrl is held.
- **`F` connect.** Exactly 2 selected: left→right by x, first output into the first FREE
  compatible input — occupied inputs are skipped, never stolen.
- **Drop-on-link insert.** Requires a real drag (`drag_moved` — a header click can never splice),
  single-node drops only, the node must not already be an endpoint. Both splice ends are
  type-checked BEFORE any mutation (no half-splice); `connect()` replacing the target input's
  wire makes old-link removal atomic.
- **Pins:** `g7_geom` 3/3 (cross/miss/collinear; a stroke cuts the crossed noodle and NOT the
  parallel one; drop-rect uses the same curve). Suites: editor+material family 145/145.

**G8 — Structure nodes.** Reroute, Frame, Node Group + group I/O. These are graph *shape*, so they
must round-trip through `parse_panel_channel` untouched (R4 class) and through `.fl4`.
**Verify:** a grouped subgraph evaluates identically to its inlined form (numeric compare) —
grouping must be a pure refactor of the graph, never a semantic change.

**PARTIALLY CLOSED 2026-08-15 — Reroute + Frame shipped and pinned; Node Group DEFERRED.**
- **Reroute.** Appended (append-only tail: `… SdfPosition, Reroute, Frame`). Float sockets —
  `SocketType::compatible` is already total, so everything connects — and the CPU evaluator
  forwards the RAW upstream `Val`, so colours survive (pinned). **The GPU bake path DECLINES
  graphs containing one** (not in `node_type_gpu_bakeable`): the WGSL arm's float coercion would
  collapse RGB — a CPU/GPU parity break — so it fails CLOSED to the correct CPU bake (same
  precedent as AO/SdfThickness; typed WGSL passthrough is a queued optimization). Usable today
  via Add menu ("Layout" category) + G7's drop-on-link splice.
- **Frame.** Zero sockets (can never enter an eval chain — pinned inert), `Node.frame_size`
  (`#[serde(default)]`, JSON round-trip pinned). Draws FIRST (beneath nodes, translucent),
  header-drag carries every node whose center its box covers (live graph-space containment),
  bottom-right grip resizes. A frame-BODY press starts a marquee over its children (only the
  header claims the pointer), and frames stay out of `box_hits` so a marquee never selects the
  box itself. Layout changes follow the node-move contract: no `changed`, no rebake.
- **R4/R5 pinned:** a Reroute spliced into a panel chain flips `channel_source` to `HandWired`
  and `parse_panel_channel` returns None — the projection DEGRADES, never guesses through it.
- **Node Group: deferred to its own session, deliberately.** It is the one structure node that
  touches persistence shape (a nested `NodeGraph` on the node), evaluator recursion, group-I/O
  sockets, and enter/exit editing UX — landing it at the tail of a five-gate session is how
  half-shipped features happen. Design sketch for that session: `Node.group: Option<Box<
  NodeGraph>>` (`serde(default)`, JSON-safe), `GroupInput`/`GroupOutput` boundary node types
  (append-only), Ctrl+G moves the selection into the nested graph and maps crossing wires to
  group sockets, `eval_node` recurses, GPU path declines (whitelist), and the gate's OWN verify
  (grouped == inlined, numeric) plus a `.fl4` round-trip test are the exit bar.
- **Pins:** `g8_structure` 4/4. Suites: editor+material family 160/160 · render 107.

**G9 — Blender node port set.** Add the missing staples above, each with: WGSL parity to the CPU
evaluator (the "feature must work in BOTH paths" rule), a value test against Blender's documented
formula where one exists, and an Add-menu category matching Blender's so muscle memory transfers.
Map Range vs `Remap`: keep ONE, alias the other in the menu.

**STARTED 2026-08-15 — Checker + Gamma shipped (the continuous gate's first batch).**
Both appended at the enum tail, both GPU-whitelisted with WGSL mirroring the CPU arm (Checker's
parity uses integer floor sums — two's-complement `& 1` matches Rust for NEGATIVE cells on both
sides, which is why it never uses `fract()`; Gamma is `pow(max(c,0), max(g,1e-4))` per channel,
the gamma floor keeping `pow` defined identically on both). Checker follows the house
generator pattern (`generator_sample_pos` + `sample_projected` / `emit_builtin_projection`, per-
side triplanar like Musgrave/Brick). Value pins in `g9_nodes` (adjacent cells alternate incl.
across zero; Gamma exact); the **exhaustive `every_nodetype_generates_valid_wgsl` gate** now
covers all four new types across 8 projection configs × 2 sockets (Reroute asserts GPU-decline;
Frame exempted with reason — zero sockets, whitelisted so a decorative box never forces a CPU
bake). Palette: Checker under Texture, Gamma under Color. Remaining staples queued: White Noise,
Fresnel/Layer Weight (need a view vector in the eval context — check first), Hue/Saturation,
Map-Range-vs-Remap menu alias, Curves, Bump/NormalMap/Displacement, Sep/Comb RGB, VectorRotate.

**Ordering note.** G6 is the unlock and — per R6 — moves ahead of the write-path flip. G8 must
land before G5's reap, since Group/Reroute change what "the graph" contains and therefore what the
projection must tolerate. G9 is breadth: incremental, one node at a time, blocking nothing.

## 4. Order and discipline

Revised 2026-08-15 after the editor review: the original G1->G5 line assumed the editor was a
side-quest. R6 says otherwise — the flip hands users the editor, so the editor must be usable
BEFORE the flip.

**Phase A — prove and unlock (nothing deleted)**
1. **G1 fidelity proof** — round-trip + relabel + degrade tests. The gate that protects users'
   authored materials.
2. **G6 selection set + keymap** — the editor unlock; independent of G1, may run in parallel.

**Phase B — collapse the ambiguity**
3. **G2 one `channel_source()`** — kills the 8-file re-derivation.
4. **G3 flip the write path** — graph authoritative, reverse syncs deleted. Ships only with A done.

**Phase C — make the primary UI good**
5. **G7 link ergonomics** · 6. **G8 structure nodes** (before any reap).

**Phase D — finish**
7. **G4 doc becomes a projection** · 8. **G5 reap** (success = line-count drop).

**Continuous:** G9 node breadth, one node per change, each with CPU+WGSL parity.

One gate per session, each with headless verification and a negative test, tracker entry per gate.
G1 may find the graph CANNOT express some panel construct; if so the gate's output is that list and
the plan pauses for an owner decision (extend the graph, or drop the construct). **No deletion
before G1 passes** — removing the doc's authority while the graph cannot hold its content would
lose users' authored materials.
