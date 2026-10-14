# Resize-Bevel / Unbevel via a procedural position cache

**Date:** 2026-07-30
**Owner:** *"we need to work on a resize bevel, and an unbevel tool, ideally they are part of the same
tool. I thought a while ago about a cache system that remembers point positions before an action —
this could help a lot for this tool. What do you think if we do a procedural cache system?"*

**Short answer: yes — and the codebase is already most of the way there.** But the cache must store
**topology provenance**, not just positions. Positions alone cannot unbevel. Details below.

---

## 0. "Is this the Blender way?" — asked 2026-07-30, answered honestly

**Resize: yes, and AZRA already does it this way — I under-researched §3–4 and reinvented an existing
system. Unbevel: no. Blender has no unbevel, and that is a deliberate design position, not an
oversight.**

### Blender has THREE mechanisms here; the plan conflated them

| Blender mechanism | Lifetime | Blender's bevel support |
|---|---|---|
| **Adjust Last Operation** (F9 redo panel) | until the next operator | ✅ full — width/segments/shape/miter |
| **Modifier stack** (Bevel modifier) | forever, re-evaluated | ✅ full, non-destructive |
| **Unbevel** | — | ❌ **does not exist** |

Blender's answer to "I want to change this bevel later" is **the modifier**. Its answer to "undo this
bevel" is **Ctrl+Z**, or *"you should have used the modifier."* There is no operator that finds an
existing bevel in finished geometry and removes it. The nearest things are manual: dissolve the bevel
edge loops (`Ctrl+X` on the loops) or `Limited Dissolve`. Third-party add-ons exist precisely because
vanilla lacks it.

### The correction that matters: AZRA already implements Blender's redo pattern

`PolyInsetAdjust` (`app/src/types/types_ui.rs:495`) is **exactly** the design §4 proposes, already
shipped for Inset:

```rust
pub(crate) struct PolyInsetAdjust {
    pub(crate) faces: Vec<u32>,
    /// The mesh BEFORE the inset. Every re-run starts from this, so tweaking never stacks insets.
    pub(crate) before: Arc<HalfEdgeMesh>,
    pub(crate) thickness: f32, pub(crate) depth: f32, pub(crate) individual: bool,
    pub(crate) needs_settle: bool,
}
```

Note the doc comment: *"Every re-run starts from this, so tweaking never stacks insets"* — that is
precisely the "resize must not compound" property §4 sets as a goal, already solved. `PolyCreateAdjust`
and a Circle variant do the same. The doc comment even records the owner asking for this behaviour:
*"the tool property goes away when I click to confirm; in Blender it stays there."*

**So Resize-Bevel is not a new subsystem. It is `PolyBevelAdjust`, copying an established local
pattern.** That is much smaller than §4 implies, and it should be built that way.

### Where storing `before` beats storing a mapping

`PolyInsetAdjust` stores the **whole pre-op mesh** (an `Arc`, so cheap to hold). That is strictly
simpler than §2's `created_from` element mapping, and it is *more* robust — restoring a snapshot cannot
desync, whereas a mapping can. §2's argument that "positions alone cannot unbevel" is still correct,
but it aimed at the wrong target: **the snapshot is not a position table, it is the entire prior mesh**,
which trivially supports both resize and un-bevel *for the most recent operation*.

### So what is actually left to design?

Only the part Blender genuinely does not have: **unbevelling geometry that is no longer the last
operation** — a bevel from ten edits ago, or from an imported mesh.

That is where §2's provenance mapping earns its place, and it is worth being clear that it is a
**deliberate step beyond Blender**, not parity work. It also inherits Blender's reason for not doing
it: after arbitrary intervening edits, "which loops are the bevel" stops having one correct answer.

### Revised recommendation

1. **`PolyBevelAdjust` first** — clone `PolyInsetAdjust`. Gives resize immediately, plus segments/shape
   once the kernel honours them (§7 step 1). Genuine Blender parity, small, low-risk.
2. **Bevel modifier arm** (`mesh_sync.rs:273`) — the fully non-destructive route. Also Blender parity.
3. **Unbevel-from-provenance** — only if wanted *after* 1 and 2 land. This is the beyond-Blender part;
   judge it on its own merits rather than as a gap being closed.

The sections below still describe the right end state, but read §4's `BevelRecord` as **step 3**, and
treat steps 1–2 as the near-term work.

---

## 1. Why this is needed — what bevel does today

`bevel_edges_ex` (`half_edge.rs:5013`) is **destructive and records nothing**:

```rust
pub fn bevel_edges_ex(&mut self, selected: &HashSet<u32>, width: f32, _segments: u32, _shape: f32)
```

Three facts follow immediately:

* **`_segments` and `_shape` are ignored** (leading underscore). Today's bevel is single-segment only,
  regardless of what the UI shows.
* **No provenance.** After the op, nothing records which new vertices came from which original edge,
  so there is no way to identify "the bevel" later.
* **Resize = re-bevel a bevel.** Running it again on the new geometry compounds — it does not adjust.

There is also a **Bevel modifier** (`PolyModifierType::Bevel`, `scene_graph.rs:308`) with
`bevel_width` / `bevel_segments` fields — and its apply arm is an explicit no-op
(`mesh_sync.rs:273`). To the team's credit that is *honest*: the comment says the controls are
disabled in `properties_poly.rs` "so the UI does not lie". But it means the procedural route is
currently unimplemented in both directions.

---

## 2. The trap: positions alone are NOT enough

The instinct — "remember the point positions before the action" — is right in spirit, and it is
exactly how you get **resize** working. But it cannot give you **unbevel**, and it is worth being
precise about why before building.

A bevel does not move vertices. It **changes topology**: one edge becomes two edges plus a new face
(or 2·N faces at N segments), and one vertex of degree *k* becomes *k* new vertices. Reversing that
means *deleting* elements and *re-merging* the survivors — which a table of old positions cannot
describe, because the elements to delete **did not exist** when the snapshot was taken.

So the cache has to remember the **mapping**, not the coordinates:

| Store | Enables |
|---|---|
| pre-op vertex positions | ✅ resize (re-run from the original shape) |
| pre-op → post-op element mapping | ✅ **unbevel** (know what to collapse back) |
| the op's parameters (width, segments, shape) | ✅ non-destructive re-evaluation |

**Positions get you half the tool. Provenance gets you both halves.**

---

## 3. The good news: the architecture for this already exists

This is not a new subsystem. `POLY_FOUNDATION_PLAN.md`'s chosen design already put the right
container in place — **`PolyMesh`** (`core_math/src/poly_mesh.rs:553`):

```rust
pub struct PolyMesh {
    pub topo: HalfEdgeMesh,   // the SERIALIZED substrate
    pub cd: CustomData,       // §4.2 runtime SoA attribute sidecar
    pub topo_index: TopoIndex,// §4.3 derived cycles
    pub topo_gen: u64,        // bumps on every topology change
}
```

Three properties make it the correct home:

1. **It is NOT serialized.** CLAUDE.md's one hard red line for poly is *"do NOT change
   `HalfEdgeMesh`'s serialized shape"* — any bincode field change corrupts every existing `.fl4`.
   A cache on `PolyMesh` sidesteps that completely.
2. **`CustomData` is already the per-element attribute sidecar** — exactly the shape a per-vertex
   provenance column needs.
3. **`topo_gen` already exists** to invalidate derived data on topology change. A stale-cache guard
   is therefore a comparison, not a new invention.

So the work is **populating an existing structure**, not designing a new one.

---

## 4. Proposed design — one tool, three verbs

### The op record

```rust
/// Runtime-only (PolyMesh sidecar). Never serialized — see CLAUDE.md's HalfEdgeMesh red line.
struct BevelRecord {
    /// Which ORIGINAL edges were beveled (stable edge UIDs, not Vec indices).
    source_edges: Vec<u32>,
    /// Pre-op positions of every vertex the op touched, keyed by stable vertex UID.
    prior_positions: HashMap<u32, Vec3>,
    /// New elements this op created → the source element they came from.
    /// THIS is what makes unbevel possible.
    created_from: HashMap<u32, u32>,
    params: BevelParams,   // width, segments, shape, width_type
    topo_gen_at_apply: u64,
}
```

### The three verbs, from one record

* **Bevel** — run the op, write the record.
* **Resize** — restore `prior_positions`, delete elements in `created_from`, re-run with a new width.
  *Idempotent by construction*: every resize starts from the original geometry, so dragging the
  handle back and forth does not compound or drift.
* **Unbevel** — restore `prior_positions`, delete `created_from`, **stop**. The mesh returns to its
  pre-bevel state.

Unbevel is literally resize with `width = 0` and no re-run. That is why the owner's instinct to make
them one tool is correct: they are the same operation with a different terminal step, and building
them separately would duplicate the hard part (the mapping) twice.

### Stable IDs, not Vec indices

CLAUDE.md is explicit: *"Vec indices go stale after insert/delete/reorder."* The record must key on
stable UIDs, resolved to indices only at execution time. This matters more than usual here because
the whole point is to survive intervening edits.

---

## 5. The hard question: what happens when the user edits *after* beveling?

This is where procedural systems usually break, and it should be decided **before** writing code.

If the user bevels, then extrudes a face that touches the bevel, then asks to unbevel — the record's
`created_from` may name elements that no longer exist, or that now carry geometry the user authored.

Three honest options:

* **(a) Invalidate on any topology change.** `topo_gen != topo_gen_at_apply` ⇒ drop the record; the
  bevel becomes permanent. Simple, never wrong, and matches how "adjust last operation" works in
  most DCCs. **Recommended for v1.**
* **(b) Invalidate only if the affected elements changed.** More permissive, needs the record to
  verify its own elements are intact. Good v2.
* **(c) Full procedural history (modifier stack).** The real answer long-term — see §6 — but it is a
  different, much larger project.

**Do (a) first.** It is honest, it is small, and per CLAUDE.md's fail-closed rule an invalidated
record should *disable the resize handle*, never silently produce a wrong result.

---

## 6. How this relates to the Bevel MODIFIER

Worth being clear, because these look like the same feature and are not:

| | Edit-mode Resize/Unbevel (this doc) | Bevel modifier |
|---|---|---|
| Scope | one specific beveled edge selection | whole mesh / a vertex group |
| Lifetime | until the next topology edit (option a) | forever, re-evaluated every frame |
| Where | `PolyMesh` sidecar | `PolyModifierType::Bevel`, `mesh_sync.rs` |
| Status | not built | **explicit no-op at `mesh_sync.rs:273`** |

They share the kernel op and nothing else. The modifier is the fully-procedural answer; this tool is
the interactive one. **Blender ships both**, for the same reason.

A note on sequencing: implementing the modifier arm requires the kernel bevel to honour `segments`
and `shape` — which it currently ignores. That is arguably the **prerequisite for both**, since a
single-segment-only bevel limits the interactive tool too.

---

## 7. Recommended order

1. **Make the kernel bevel honour `segments` and `shape`.** Currently `_segments` / `_shape` are
   ignored, so both features are capped at one segment. **Read `docs/planning/poly/BEVEL.md` §6
   first** — it is a graveyard of ~22 failed approaches, and multi-segment profile work is exactly
   where past attempts died. Do not re-derive a dead end.
2. **Add `BevelRecord` to `PolyMesh`** + write it in the app-layer bevel path. No behaviour change
   yet — just record. Gate: after a bevel, the record's `created_from` names every new element.
3. **Resize** — restore, delete, re-run. Gate: bevel@0.1 → resize to 0.3 must be **bit-identical** to
   a fresh bevel@0.3 on the original mesh. That single assertion is what proves the cache is honest,
   and it is the one that catches drift.
4. **Unbevel** — same path, terminal. Gate: bevel → unbevel restores the **exact** pre-bevel mesh
   (vertex count, face count, and positions).
5. **Invalidation (option a)** + a disabled handle when stale.

Steps 3 and 4 are ~one function apart, which is the payoff for storing the mapping once.

---

## 8. Verdict on the owner's question

**Yes, build the procedural cache — with one correction.** Store the *element mapping*, not just
positions; positions alone give resize but cannot give unbevel, and unbevel is the half that has no
workaround today.

And it should be **one tool**, exactly as proposed: unbevel is resize-to-zero-without-re-run. Building
them separately would implement the provenance mapping twice.

The strongest argument for doing it: the container (`PolyMesh`), the invalidation signal (`topo_gen`),
and the attribute sidecar (`CustomData`) **already exist and are already the plan of record**. This
fills them in rather than adding a subsystem — and it stays entirely on the runtime side of the
save-format red line.
