# Review: Grok's P1–P3 (ADR-001 implementation)

**Date:** 2026-07-29
**Reviewed:** `f0df0e4` (P1), `527dba8` (P2 compositor), `8394829` (P2 blend set), `d2f543b` (P3)
**Scope:** review only, no code changed.

---

## Verdict: this is good work and it follows the ADR

Three phases landed in one pass, and the parts I checked line up with what ADR-001 specified —
including the correction I had to issue mid-flight.

| Suite | Result |
|---|---|
| `-p render` | **63 passed, 0 failed** |
| `-p core_math --lib` | **664 passed, 0 failed** |
| `-p app` (material/curvature/bake, `--test-threads=1`) | **195 passed, 1 failed** — the one failure is an obsolete test of mine, see §3 |

---

## 1. What was verified, not assumed

### P2's central claim holds — the inline composite is actually gone

```
grep -c "blended = top" core_engine/render/src/gbuffer.wgsl   →  0
```

This is the load-bearing item. The ADR's correction said the defect was never that Normal
mis-blended a stack — it was that **the G-buffer had no stack**, so the fix had to be *deletion*,
not repair. Grok deleted it. The commit message confirms the intent explicitly ("remove
vertex-curvature inline blend (blended=top path)"), and VertexCache generators now wire into the
bake instead.

That also retires my own failed attempt from the record: I tried `blended = mix(base, top, mag)` and
reverted it when GPU tests caught that `mag` is a value, not a coverage. Deletion was the right
move, and it is done.

### P1's drives are real socket connections, not stored intent

I checked this specifically because "drives" could easily have been a field nobody reads:

```rust
// ADR Decision 2: Driven radius → Epsilon (input 1).
if let Some(d) = layer.drive("radius") {
    let _ = g.connect(d.node, d.output as usize, gen, 1);
}
```

`ParamDrive { param, node, output }` is consumed in `lower.rs` at four sites and produces a genuine
`g.connect(...)` into the socket the ADR named. `layer_ui.rs` reads the same list to show drive
badges. So the plumbing is end to end, and it uses the sockets that already existed rather than new
shader work — exactly the shape the ADR predicted.

### Stable IDs exist and are allocated centrally

`next_layer_id` + `alloc_layer_id()` / `ensure_layer_ids()` / `stamp_layer()` on the doc. Central
allocation is the right pattern — it is what makes reorder/insert/nesting survivable, and it matches
the `prim_uid` precedent CLAUDE.md already mandates.

---

## 2. Gap: regen is still DESTRUCTIVE, not merge

This is the one ADR requirement I could not verify as done.

`strip_channel_subgraph` (`lower.rs:844`) has this doc comment:

> *"Remove every panel-generated node for `kind` (and its master wire, if it was panel-authored).
> **Hand-authored wiring is never touched.**"*

But the filter is unchanged from before P1:

```rust
let doomed: Vec<u32> = g.nodes.iter()
    .filter(|n| is_panel_node_for(n, kind))   // every panel-tagged node
    .map(|n| n.id).collect();
for id in doomed { g.remove_node(id); }
```

The claim is *narrowly* true — a node the user added by hand carries no `mp:` tag, so it is not
matched and does survive. What does **not** survive is a **panel-tagged node the user then
hand-edited**, which is the case ADR-001 §Consequences item 2 required:

> *"Regen becomes merge. Preserve user-added nodes **and any socket marked `Driven`**; regenerate
> only panel-owned structure."*

Drives themselves are safe because they live in the **doc** (`layer.drives`) and are re-applied on
each regen — a sound design, arguably better than preserving graph state. So the practical exposure
is smaller than it first looks.

**Not a blocker; a scoping question for the owner.** Either:
* accept doc-as-truth (drives survive because the doc replays them) and **soften the ADR wording**
  plus that doc comment, which currently over-promises; or
* implement real merge, if the intent is that hand-tweaking a panel-generated node sticks.

The ADR's exit test for P1 was *"hand-edit a node, then edit the panel — the hand edit survives."*
Under the current implementation that passes for **untagged** nodes and fails for **tagged** ones.
Worth deciding deliberately rather than discovering later.

---

## 3. One failing test — obsolete, mine, not a regression

```
owner_curvature_vertex_rung_on_base_color_produces_keys ... FAILED
  "VertexCache-rung Curvature on Base color produced NO vertex keys"
  Node labels: ["Master Material", "mp:0:gen:1:sg", "mp:0:gnorm:1", "mp:0:grm:1"]
```

The label is `sg`, not `sgv`. `vertex_curv_keys_for_graph` requires the flag to end in `v`, which is
how the *vertex rung* was identified — and P2 deliberately removed that rung ("VertexCache
generators/masks wire into the bake via SdfCurvature"). So the test asserts an invariant that no
longer exists by design.

**Action: delete or rewrite that test.** I wrote it to pin the vertex-rung path; that path is gone
on purpose. Leaving it red trains people to ignore red — the same argument I made about the
parallel-test race. It is Grok's call whether to delete it or repoint it at the bake path, since they
own this area now.

---

## 4. Smaller observations

* **`channel_skip_planar_map_install` is now always false.** Consistent with P2 (pure-Vertex stacks
  install maps because there is no separate vertex rung), and it removes the predicate behind the
  418-bake loop. Worth a note that the *original* hazard — install/clear ping-pong — is now
  structurally impossible rather than merely guarded.
* **19 files still uncommitted.** Much smaller than the 65 earlier, and the tree compiles green.
* **The Photoshop MixBlend set** (`8394829`) landed as the ADR predicted: one enum, two evaluators.

---

## 5. What I did NOT verify

Stated plainly so nobody reads this as broader than it is:

* **No pixel verification.** The ADR gates every phase on the soak showing a real pixel delta, and
  this project has twice shipped features that passed every unit test and moved zero pixels. P2's
  exit test — *PT and Hybrid pixel-comparable on a multi-layer material* — has not been run.
* **P3 mask stacks** (`d2f543b`) reviewed only at commit level, not read line by line.
* **Save-format round-trip.** `Layer` gained `id` and `drives`; both are wire changes. I did not
  verify they ride the in-blob `serde(default)` side-channel correctly, and an old `.fl4` load test
  is the only thing that proves it.

Those three are the highest-value next checks, in that order.
