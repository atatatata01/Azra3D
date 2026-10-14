# Plan for Grok — poly P0 fixes + procedural bevel (resize / unbevel)

**Date:** 2026-07-30 (rev: P1-1 bevel path corrected; worktree = `feat/poly-modifiers`)  
**Branch/worktree:** `feat/poly-modifiers` @ `F:\PROJECT\FourthLAyerAi\.claude\worktrees\poly-modifiers`  
**Trunk:** `feat/ui-polish` (release trunk — **not** old `master`)  
**Sources:** `POLY_TOOL_AUDIT_2026-07-30.md` (45-agent audit, verified),
`PROCEDURAL_BEVEL_CACHE.md` (design + its §0 correction), `BEYOND_BLENDER_PROCEDURAL.md`.

---

## ⚠ Read before touching anything

**Do not implement this plan in the dirty `ui-polish` checkout.** That tree often has in-flight
keybind / drag / menu WIP (`key_dispatch.rs`, `poly_drag_tools.rs`, `keybindings.rs`, …).  
**All work lands on `feat/poly-modifiers`**, branched from current `feat/ui-polish`.

If you must pull trunk: `git merge --ff-only feat/ui-polish` (or rebase) on this worktree only after
confirming no conflicting dirty files on the trunk checkout.

### Standing rules for every step

1. **`cargo build -p app` after every change.** Never pile onto a broken state.
2. **`cargo test -p core_math` after ANY `half_edge.rs` change** — non-negotiable per CLAUDE.md.
3. **Never change `HalfEdgeMesh`'s serialized shape.** No field add/remove/reorder/retype on
   `HVertex` / `HFace` / `HalfEdge` / `HalfEdgeMesh`. It corrupts every existing `.fl4`. New runtime
   state goes on the **non-serialized `PolyMesh` wrapper** or the app layer.
4. **Gate on the PROPERTY, never on "it changed".** This repo has now shipped two features whose gate
   asserted "output differs" and passed *because the output was broken*
   (`p2_vertex_splat_grid_differs_from_planar…`, and the earlier sabotage-passing tests). Assert the
   thing you actually want: bit-identity, coverage, a specific axis.
5. **Sabotage-check each new gate.** Revert your fix, confirm the gate FAILS, restore. A gate that has
   never failed is not known to work.
6. **One item per commit**, stating what was *measured*, not what was intended.

---

## P0-1 ★★★ — Mirror modifier mirrors the WRONG axis

**Verified still broken at `properties_poly.rs` checkbox row as of plan write.**

`PolyModifier.mirror_axis: u8` carries **two incompatible encodings at once, with no translation
layer:**

```rust
// KERNEL reads an INDEX — half_edge.rs mirrored()
match axis { 0 => reflect X, 1 => reflect Y, _ => reflect Z }

// PANEL writes a BITMASK — properties_poly.rs
if ui.checkbox(&mut mx, "X").changed() { mods[mi].mirror_axis ^= 1; } // X=1
if ui.checkbox(&mut my, "Y").changed() { mods[mi].mirror_axis ^= 2; } // Y=2
if ui.checkbox(&mut mz, "Z").changed() { mods[mi].mirror_axis ^= 4; } // Z=4
```

`mesh_sync.rs` passes the value **raw** to `mirrored()`. Consequences:

| User ticks | value | kernel mirrors |
|---|---|---|
| X | 1 | **Y** ← wrong |
| Y | 2 | falls to `_` → **Z** ← wrong |
| Z | 4 | falls to `_` → Z (right *by accident*) |
| X + Y | 3 | `_` → **Z only** ← multi-axis silently impossible |

A third site, `symmetry.rs` (`poly_add_mirror_modifier`), correctly writes an **index** via
`axis.min(2)` — so the two writers also disagree with each other.

**This is the highest-value class of bug in this codebase: a control that looks right and silently
produces wrong geometry.**

### Fix — bitmask is the honest encoding; multi-axis is Blender parity

The panel offers three independent checkboxes, so **bitmask is the honest choice**. Kernel applies
each set bit in turn (X then Y then Z) so X+Y = 4-way mirror — Blender Mirror multi-axis behaviour.

**Make the encoding a type** so index vs bitmask cannot coexist silently:

```rust
// core_math — PolyModifier.mirror_axis stays u8 on disk (no format change).
// Convert at the boundary via MirrorAxes.
pub struct MirrorAxes(u8); // X=1, Y=2, Z=4
```

- Store `u8` on disk; convert to `MirrorAxes` at read/eval boundary.
- `HalfEdgeMesh::mirrored(axes: MirrorAxes, …)` applies set bits successively.
- Call sites that have an **axis index** (symmetry pie) use `MirrorAxes::from_axis_index(i)`.
- Default for new Mirror modifiers: **`1` (X)**, not `0` (empty = no-op under bitmask).

**Save-format note:** existing `.fl4` values 1/2/4 already *mean* X/Y/Z under the panel encoding.
Values 3/5/6/7 become real multi-axis. Value `0` (old default under *index* encoding = X) becomes
“no axes” under bitmask — mitigated by defaulting new modifiers to `1`, and treating empty bits as
identity (no silent wrong-axis).

**Gate:** `mirror_axis_x_mirrors_x` — asymmetric +X half-box, mirror X only → verts appear with
negative x; y/z extents unchanged in sign pattern. Repeat Y and Z. Then X+Y → 4-way vertex expansion
for a non-symmetric input. Sabotage: pass raw bits as index and confirm the X case fails.

**Status:** ✅ DONE on `feat/poly-modifiers` — `MirrorAxes` + multi-axis `mirrored()`, mesh_sync/symmetry
wired, property gates green (`mirror_axis_*`, `apply_mirror_bitmask_*`).

---

## P0-2 ★★★ — Extrude does nothing in Vertex or Edge mode

**Blocked on dirty `key_dispatch.rs` / `poly_drag_tools.rs` on trunk — land that WIP first, or
implement carefully on this worktree without merging half-finished trunk edits.**

There is no `extrude_vertices` / `extrude_edges` equivalent anywhere (4 name variants grepped, 0 hits).
`E` outside Face mode arms `poly_tool = ExtrudeFace` but the drag gate needs a non-empty **face**
selection, so nothing happens.

**Verifier precision worth keeping:** the arming *persists*. Switch to Face mode later and the
still-armed tool fires on the next viewport click. Latent, not merely inert.

Blender: `mesh.extrude_vertices_move` (verts → new edges) and `mesh.extrude_edges_move` (edges → new
faces). Drawing a profile edge-loop and pulling it into a surface is a **core box-modelling move**.

**Fix:** two new kernel ops + `PolyEdit` variants (**append at the END** of the enum — bincode tags by
position, inserting corrupts every save), routed from the same `E` key by work mode.

**Gate:** in Edge mode, select one edge of a plane, extrude, assert **one new face** and 2 new verts;
in Vertex mode assert **one new edge**. Plus: after `E` in Vertex mode with no valid selection, assert
the tool is **not** left armed.

**Status:** ✅ DONE on `feat/poly-modifiers` — `extrude_vertices` / `extrude_edges` kernel + PolyEdit
variants; E routes Face/Edge/Vertex; no latent tool arm on empty selection; gates
`extrude_vertices_makes_wire_and_new_vert` / `extrude_edges_plane_makes_face`.

---

## P1-1 ★★ — Bevel residual audit (NOT “segments ignored everywhere”)

### ⚠ Corrected premise (2026-07-30)

The earlier draft claimed “today’s bevel is single-segment only” based on:

```rust
// FROZEN path — half_edge.rs
pub fn bevel_edges_ex(..., _segments: u32, _shape: f32)
```

**That is not the interactive tool path.** The app drives:

```text
poly_drag_tools → core_math::bevel_op::bevel_edges_offset (multi-seg + shape exist)
```

`bevel_edges_ex` is frozen legacy. Multi-segment/profile work already lives in `bevel_op.rs` with
oracles/tests (see `BEVEL.md`, recent k2-d3 / adjust_offsets commits).

### What P1-1 actually is

1. **Audit** interactive bevel: which UI params still lie or degrade (segments / shape / clamp / miter)?
2. **Read `docs/planning/poly/BEVEL.md` §6** before any kernel change — graveyard of failed approaches.
3. **Fix only residual bugs** with geometric + Blender-oracle gates (property, not “output differs”).
4. Append outcome to BEVEL.md §6 + §11 session line when done.

**Do not** re-implement multi-seg on `bevel_edges_ex`. **Do not** wire the Bevel modifier to the
frozen path.

**Gate (residual-specific):** for each fixed residual, a property assertion + sabotage check. Baseline:
`segments=1` remains regression-safe vs current `bevel_op` defaults where claimed.

**Status:** ✅ DONE (audit + residual honesty) on `feat/poly-modifiers` —
- Interactive path confirmed already on `bevel_op` (segments/shape/clamp live).
- Bevel modifier gained `bevel_shape` + `bevel_clamp` (`#[serde(default)]`); no longer hard-codes them.
- Log `clamp=on` lie fixed.
- Gate: `apply_bevel_shape_flat_differs_from_round`. BEVEL.md §11 session entry added.

---

## B1 ★★ — Resize-Bevel (`PolyBevelAdjust`)

**This is NOT a new subsystem.** It is a clone of an existing, shipped pattern —
`PolyInsetAdjust` (`app/src/types/types_ui.rs`):

```rust
pub(crate) struct PolyInsetAdjust {
    pub(crate) faces: Vec<u32>,
    /// The mesh BEFORE the inset. Every re-run starts from this, so tweaking never stacks insets.
    pub(crate) before: Arc<HalfEdgeMesh>,
    pub(crate) thickness: f32, pub(crate) depth: f32, pub(crate) individual: bool,
    pub(crate) needs_settle: bool,
}
```

That doc comment — *"every re-run starts from this, so tweaking never stacks"* — **is** the
resize-must-not-compound property. Already solved for Inset; copy it.

Storing `before` as an `Arc` is **better than storing an element mapping**: a snapshot cannot desync,
a mapping can.

Follow the same lifecycle: live re-run on change, **ONE settled undo** on release, auto-dismiss when
the node goes away. `poly/inset.rs` is the reference implementation, including the dry-run scratch
clone so a refusing op does not eat the next Ctrl+Z.

**Depends on:** `bevel_op` quality good enough for re-run (P1-1 residuals closed or accepted).

**Gate:** bevel@0.1 then resize to 0.3 must be **bit-identical** to a fresh bevel@0.3 on the original
mesh. Sabotage: re-run from *current* mesh instead of `before` and confirm the gate fails.

**Status:** ✅ DONE on `feat/poly-modifiers` — `PolyBevelAdjust` + `apply_bevel_from_before` +
Adjust Bevel window (Width/Segments/Shape/Clamp/Unbevel); confirm seeds panel; gates
`bevel_resize_from_before_matches_fresh` / `bevel_unbevel_width_zero_restores_before`.

---

## B2 ★★ — Bevel modifier arm

`mesh_sync.rs` is an explicit, **honest** no-op (`Decimate | Bevel => {}`) with controls disabled
in `properties_poly.rs` so the UI does not lie. Good — keep that discipline.

**Implementation rule:** call **`bevel_op::bevel_edges_offset`** (or the same public entry the tool
uses) on **all edges** (or documented selection policy), **never** `half_edge::bevel_edges_ex`.

With P1-1 residuals acceptable, implement the arm. Re-enable the controls **in the same commit**
you implement it, not before.

**Gate:** a body with a Bevel modifier at width w renders the same mesh as the destructive bevel at
width w (same params); toggling `enabled` off restores the original exactly.

**Status:** ✅ DONE on `feat/poly-modifiers` — `apply_poly_modifiers` Bevel arm →
`bevel_op::bevel_edges_offset` (all live half-edges) + install guard; UI Add + width/segments live;
gates `apply_bevel_matches_bevel_op_all_edges` / `apply_bevel_disabled_is_identity`.

---

## B3 ★ — Unbevel (only after B1 + B2, and only if still wanted)

**Blender has no unbevel.** Its answer to "change this bevel" is the modifier; to "remove it", Ctrl+Z
or manual loop dissolve. So this is a deliberate step **beyond** Blender, not parity work — judge it on
its own merits.

For the **last** operation, B1 already gives it free: unbevel = resize to 0 without re-running.

**Status:** ✅ **Satisfied by B1 for last-op** — `Adjust Bevel` panel **Unbevel** button sets
width→0 and restores `before` via `apply_bevel_from_before` (gate
`bevel_unbevel_width_zero_restores_before`). Non-destructive Bevel **modifier** is the forever path
(toggle off / remove card).

**Not built (and not required unless product asks again):** provenance unbevel of a bevel that is
*no longer the last op* (`PROCEDURAL_BEVEL_CACHE.md` §4 `created_from` + `topo_gen` invalidation).
That is the beyond-Blender piece; leave parked.

---

## Suggested order

| # | Item | Why here | Blocked by |
|---|---|---|---|
| 1 | **P0-1 Mirror** | silently corrupting geometry *now* | — |
| 2 | **B2 Bevel modifier** | non-destructive stack; user goal #1 for modifiers | P1-1′ audit; wire `bevel_op` only |
| 3 | **P1-1′ Bevel residual audit** | only if tool/modifier quality still lies | read BEVEL.md §6 |
| 4 | **B1 resize-bevel** | clone `PolyInsetAdjust` | usable `bevel_op` |
| 5 | **P0-2 vertex/edge extrude** | core modelling move, currently dead | clean key_dispatch or careful isolation |
| 6 | **B3 unbevel** | beyond-Blender; optional | B1 + B2 |

**Note:** B2 can start once P0-1 is in and `bevel_op` is the chosen entry — full multi-seg “from zero”
is **not** a prerequisite.

---

## Explicitly NOT in this plan

* **The DAG modifier stack / parallel evaluation.** Real opportunities (see
  `BEYOND_BLENDER_PROCEDURAL.md`) but they are a considered bet, not near-term work. Being *better*
  than Blender at procedural editing matters far less than being *correct* at mirroring.
* **Prefix-cached stack re-evaluation.** Highest-confidence "beat Blender" item (28–42× measured on the
  sibling material path) — but it belongs after the P0s.
* **Weighted Normal modifier / Boolean-as-stack-card.** Separate product bets; Boolean already ships
  as an X-pie **tool**. Track outside this plan.
* **The other confirmed audit gaps** (Clean Up submenu, Beautify Faces, Rotate Edge, Select All by
  Trait…). All in `POLY_TOOL_AUDIT_2026-07-30.md`; none are P0 here.
