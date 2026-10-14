# BRIDGE — multi-loop / multi-face bridging (Blender parity)

Status: **N-loop bridging SHIPPED 2026-08-07.** Owner report: "look at the bridge code — I feel it
does not work on multi select and it needs to."

---

## 1. The Blender model (oracle-established, not assumed)

Oracle: `docs/planning/poly/bridge_multi_ref.py`, Blender 4.5.1 LTS, `--factory-startup`.
Run: `blender.exe --background --factory-startup --python bridge_multi_ref.py -- out.json`

Recorded invariants (algorithm-independent — tessellation differs from ours):

| case | sel edges | nv | nf | boundary | non-manifold | arity |
|---|---|---|---|---|---|---|
| `two_wire_rings` (baseline) | 16 | 16 | 8 quads | 16 | 0 | 4 |
| `three_wire_rings` | 24 | 24 | 16 quads | 16 | 0 | 4 |
| `four_wire_rings` | 32 | 32 | 24 quads | 16 | 0 | 4 |
| `three_wire_unequal` (6/8/4) | 18 | 18 | 26 tris | 10 | 0 | 3 |
| `two_islands_wire` (2 islands x 2 rings) | 32 | 32 | 24 quads | 16 | 0 | 4 |

**Three conclusions, all load-bearing:**

1. **N loops → a CHAIN of N−1 bridges in ONE invocation.** 3 rings = 2 bridges (16 quads),
   4 rings = 3 bridges (24 quads). Blender never bridges only the first pair and never pairs
   loops off into ⌊N/2⌋ disjoint couples.
2. **Island membership does NOT constrain pairing.** `two_islands_wire` produces a census
   *byte-identical* to `four_ngon_rings` — Blender chains straight across the gap between two
   disconnected components. Ordering is pure proximity (`BM_mesh_edgeloops_calc_order`).
3. **Bridging adds NO vertices.** Unequal counts are absorbed by FANNING triangles, never by
   resampling/subdividing the shorter ring. A 6→8 bridge is `6+8 = 14` triangles; 8→4 is 12;
   the 6/8/4 chain is 26 — which is exactly the oracle's number.

The face-selection variant (`three_ngon_rings` etc.) confirms Blender **deletes the selected
faces first** and bridges the resulting boundary rings.

---

## 2. What was actually broken (the give-up points)

Call chain: keybinding / menu → `app/src/ops/poly/bridge.rs::run` → `AzraApp::poly_bridge`
(`app/src/poly/bridge.rs`) → `HalfEdgeMesh::bridge_edge_selection` / `bridge_faces`
(`core_math/src/half_edge/ops_misc.rs`).

Three independent `== 2` refusals, any one of which alone would have killed multi-select:

* `ops_misc.rs` `bridge_edge_selection`: `if rings.len() != 2 { return Vec::new(); }`.
  `edge_loop_rings` (`nav.rs:332`) already traced **all N** rings correctly — the wrapper threw
  N−2 of them away.
* `app/src/poly/bridge.rs:70`: the app's manifold-preview repeated `rings.len() != 2`, so fixing
  only the kernel would still have left the tool dead.
* `app/src/poly/bridge.rs:27`: `if sel_faces.len() == 2` gated the whole FACE path.

Selecting 3 loops therefore fell through to the legacy 1-vert/2-vert/2-edge fabrication, failed
that too, and toasted "need exactly two closed loops" — a silent no-op from the user's seat.

### Three latent bugs the multi-loop work exposed

* **BRIDGERAND — `edge_loop_rings` was NONDETERMINISTIC (the worst of the three).**
  `nav.rs::edge_loop_rings` iterated the `edges` **HashSet** directly into
  `geom.entry(key).or_insert(...)`, so which of a half-edge / its twin won — i.e. each ring's
  **orientation** — depended on hash iteration order. `adj` was then built from `geom.values()`
  (also HashMap-ordered) with unsorted neighbour lists, so the walk direction was random too.
  Ring orientation decides the winding `bridge_loops` picks, so **the same selection bridged
  differently from run to run.** The app previews the op on a clone, gates on `is_manifold()`,
  and then re-runs the op on the real mesh — two rolls of the same dice. Measured with
  `AZRA_BRIDGEDIAG=edges2`: **5 identical runs → 4 silent refusals and 1 committed NON-MANIFOLD
  mesh.** This was a live data-corruption path into the `.fl4`, and it was invisible to every
  unit test because a unit test runs the op once. Fixed by sorting the half-edge ids, building
  adjacency from sorted keys, and sorting each neighbour pair.
  Regression: `edge_loop_rings_is_deterministic_across_runs`.


* **BRIDGEFAN (overlapping faces).** `bridge_unequal_loops` built a quad SPANNING the whole arc
  (`[s0,s1,l_to,l_from]`, skipping interior long verts) *and then* fanned triangles over that same
  arc. Every span covering >1 long edge produced overlapping geometry. `validate()` passed, which
  is why it shipped; the existing test asserted only coverage and face arity, never manifoldness
  or boundary count. Measured on a 6/8/4 chain: **40** boundary half-edges vs Blender's **10**.
* **BRIDGECHAINWIND.** The reverse/rotate alignment search minimised distance only. In a chain,
  the second bridge's long ring already carries half-edges created by the first, and reversing the
  ring is precisely what flips their direction. The reverse choice must satisfy the twin
  constraint FIRST, and minimise distance only among the orientations that survive.

---

## 3. The implementation

All in `core_math/src/half_edge/ops_misc.rs`. **One** implementation per concern:

| fn | responsibility |
|---|---|
| `bridge_ring_pair(a, b)` | the ONLY place equal-strip vs unequal-fan is chosen |
| `order_loop_chain(rings)` | proximity chain over ring centroids, extendable at either end, seeded from the lowest vertex id (determinism), ties broken on vertex id |
| `bridge_ring_chain(rings)` | order + bridge consecutive pairs → N−1 bridges |
| `bridge_edge_selection(edges)` | `edge_loop_rings` → `bridge_ring_chain` |
| `bridge_faces_multi(faces)` | capture rings → delete all faces → `bridge_ring_chain`, with the delete-only fallback preserved |
| `bridge_faces(a, b)` | **now a two-face wrapper** over `bridge_faces_multi` |

Honesty contract unchanged: everything is previewed on a clone and the mesh is only mutated when
the result `validate()`s **and** `is_manifold()`s. `poly_bridge` still refuses before opening a
transaction, so a failed bridge leaves no undo entry.

**The app keeps its `is_manifold` gate (audit #3), and that is a deliberate divergence from
Blender.** Blender will happily produce a non-manifold result when the middle ring of a chain
still has a cap (its edges reach 3 faces). AZRA refuses instead — per CLAUDE.md we never install
non-manifold geometry that would persist to the `.fl4`. The manifold-clean N-loop case (rings that
are true boundaries) is accepted, which is the case users actually hit.

---

## 3b. EDGE mode vs FACE mode for N > 2 — read this before "fixing" it again

Measured through the real tool (`AZRA_BRIDGEDIAG`, alternating rim winding):

| mode | selection | result |
|---|---|---|
| `edges2` | 2 rims | nv=16 nf=10 **boundary=0** manifold ✓ — **exactly Blender's `two_ngon_rings`** |
| `faces3` | 3 caps | 8 walls, all caps deleted, manifold ✓ |
| `faces4` | 4 caps | 12 walls, manifold ✓ |
| `edges3` / `edges4` / `unequal` / `islands` | 3–4 rims | **refused** with an explicit toast |

**Why EDGE mode refuses N > 2, and why that is correct here:** chaining N rings that each already
bound a face gives every INTERIOR ring **3 faces per edge**. This is not an AZRA defect — the
Blender oracle records exactly that (`three_ngon_rings` → `nonmanifold_edges=8`,
`four_ngon_rings` → 16). Blender ships the non-manifold mesh; AZRA's `is_manifold()` gate refuses
it, because CLAUDE.md forbids installing non-manifold geometry that would persist to the `.fl4`.
A pure-wire N-loop chain IS manifold (`three_wire_rings`, nonman=0) and the kernel handles it —
but AZRA cannot select wire edges, so that configuration is unreachable from the UI.

**The manifold-clean way to bridge N loops in AZRA is therefore FACE mode**, which deletes the
caps first and is fully supported for any N. The refusal toast says so explicitly rather than
falling through to the legacy quad fabrication, which is what made the original failure silent.

**Do not "fix" this by dropping the manifold gate.** That trades a visible refusal for silent
save-file corruption. If N>2 edge-mode bridging is ever required, the correct design is to
delete the interior rings' adjacent faces (making it the face case), and that needs an owner
decision because Blender has no such behaviour to copy.

Also worth knowing: two rims must be wound to FACE each other. Two identically-wound caps cannot
close into a solid, and bridging them yields a correctly-twinned but CROSSED quad band. That is
geometry, not a bug — it cost one round of harness debugging, hence this note.

## 4. Verification

* **Kernel:** `bridge_ring_chain_three_loops_matches_blender_oracle`,
  `bridge_edge_selection_four_loops_via_selection_api`,
  `bridge_ring_chain_unequal_counts_three_loops`, `bridge_ring_chain_crosses_two_islands`,
  `bridge_faces_multi_three_caps_chains`, `bridge_faces_multi_refuses_below_two_and_never_mutates`
  (`core_math/src/half_edge/tests.rs`). Each asserts the oracle's boundary count and manifoldness,
  plus a chain-ORDER assertion (no face may span two rings' worth of Y — that would mean the chain
  skipped a loop).
* **App path** (a kernel test proves nothing about the user's experience — memory
  `feedback_verify_app_path_not_kernel`): `app/src/bridgediag.rs`, env-gated.
  `AZRA_BRIDGEDIAG=edges3|edges4|unequal|islands|faces3|faces4|cyl2 cargo run -p app` builds a real
  poly body, populates the real `poly_sel` selection, fires the real `poly_bridge()` entry point,
  and prints the resulting census.

---

## 5. Not done / next

* **Blender's bridge OPTIONS are still absent**: Number of Cuts, Interpolation, Smoothness,
  Profile Factor, Twist, Merge. Adding them needs a `BridgeParams` in `tool_params.rs` + UI in
  `properties_poly.rs`; both files are owned by other work streams, so they were left alone.
  `Twist` is the cheapest and most-missed (it is just a rotation offset applied after
  `order_loop_chain`'s alignment search).
* **Chain vs. per-island pairing is a deliberate Blender match, not necessarily the best UX.**
  If the owner ever reports "bridging two separate tubes welded them to each other", that is this
  behaviour, and it is what Blender does — change it only on an explicit decision.
