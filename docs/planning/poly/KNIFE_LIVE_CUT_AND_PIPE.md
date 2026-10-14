# Knife: Live Cut + Procedural Pipe — owner-ratified design (2026-08-04)

Two features requested by the owner in one session. **The four architecture questions below
were answered by the owner directly — do NOT re-litigate them.** Implementation is staged;
each stage must build, test, and be owner-testable on its own.

---

## Feature A — Procedural Pipe (Knife `P`)

**Gesture.** While the knife has an active polyline, `P` converts it into a tube.
Dragging changes the radius; scrolling changes the segment count (same live-modal feel as
extrude/bevel: drag = continuous param, scroll = discrete count).

**Output target — user choice in Tool Parameters:**
- **Own object** — a new polygonal body with its own modifier stack; ends up SELECTED as the
  active object.
- **Inside the current mesh** — merged into the active body as a new poly island; ends up
  SELECTED as that island (face selection = the island's faces).

**Editability — OWNER-RATIFIED: procedural, forever.**
The polyline is retained as the tube's SOURCE, so radius and segment count stay editable in
the modifier stack across sessions — not just in the F9 redo panel. Implies:
- New `PolyModifierType::Pipe` — **appended at the END of the enum** (bincode tag order is
  append-only; see CLAUDE.md).
- The source polyline is authored state: it rides an **in-blob `#[serde(default)]` field**
  (the `voxel_grids`/`VoxelNodeRecord` precedent). NEVER a second trailing magic after
  `FL4ORDER`, never a `HalfEdgeMesh` field.
- Params: `radius: f32`, `segments: u32`, `caps: bool`, plus the polyline.

---

## Feature B — Live Cut (knife SUBTRACT mode)

**The model — OWNER-RATIFIED: exactly ONE live cut at a time, alive until the next real cut.**

| Gesture | Meaning |
|---|---|
| **Normal knife cut** | BAKE the currently-live cut (apply it down), then this new cut becomes the live one |
| **Shift + knife cut** | Cut the CUTTER instead of the object — carves the cutter away, REVEALING material that had disappeared |
| **Alt + click** | Restore the live cutter to its as-created state (undo all shift-edits to it) — the "reveal back the original cutter" escape hatch |

The owner's words: *"the goal is to remain live until the next real cut — so I first cut
(subtract, part of the mesh disappears), use shift to reveal back, and I need a way to
reveal back the original cutter, maybe alt-click."* He noted this feels slightly complicated
but judged it the simplest workable scheme. **One level only — no nesting.** Cutting a
cutter edits it in place and shows instantly.

**Cutter storage — OWNER-RATIFIED: hidden children of the target.**
Each cutter is a real body parented to the cut object and hidden from the outliner by
default, with a reveal toggle. It travels with the object on move / duplicate / save. The
live cut itself is a **`PolyModifierType::Boolean` modifier** on the target referencing that
cutter — the existing modifier type, not a new parallel system. "Live" = the modifier is
present and unapplied. "Bake" = apply it down and drop the cutter.

Alt+click restore requires the cutter's **as-created** geometry, so the cutter body keeps a
pristine copy alongside its edited one (in-blob serde(default), same rule as above).

**Collect Cut Parts — OWNER-RATIFIED: only cuts still LIVE.**
A tool action that gathers the live cutter(s) into a new object (the removed material,
recoverable as geometry). Baking discards the cutter permanently — deliberately lean files;
the owner rejected permanent cut history and rolling-window history.

---

## Staging (each stage ships independently)

**A1** `PolyModifierType::Pipe` appended + evaluator (polyline + radius + segments -> tube
mesh) + core_math tests. No UI.
**A2** Knife `P` gesture: drag radius / scroll segments, live preview.
**A3** Tool Parameters target choice (own object vs island) + post-create selection handoff.

**B1** Knife subtract emits cutter-body + Boolean modifier instead of a destructive
boolean; one live cut tracked per object; normal cut bakes the previous one.
**B2** Shift+cut routes the cut at the live cutter (the reveal).
**B3** Alt+click restores the pristine cutter; hidden-child outliner treatment + reveal toggle.
**B4** "Collect Cut Parts" action over the live cutters.

## Invariants that must not be broken on the way

- Save format: new authored state uses in-blob `#[serde(default)]` fields ONLY; enum
  variants append at the END. A `.fl4` written before these features must still load.
- `mesh_boolean` results still go through `result_installable` (watertight, edge-manifold,
  fold-free) — a live cut that cannot be evaluated FAILS CLOSED (shows the un-cut mesh plus
  a toast), never a broken display mesh.
- Every user-visible parameter gets undo (`transaction()` / episode).
- The knife stays armed after a cut (shipped 4cd3ae4) — live-cut work must preserve that.
