# SDF-Target Generator — Plan

**Owner 2026-07-28:** *"sdf let make a button to have live or bake resume after"* — ship it
bake-accurate now, with an explicit user-facing switch, and add the live path later.

---

## What already works (verified, not assumed)

| Piece | State |
|---|---|
| `NodeType::PrimSdfMask` | exists |
| **CPU evaluation** | **REAL** — `node_graph_bake.rs:2325`: `sdf.sdf_at(pos, uid)` → `sdf_to_mask(d, grow, soft, contrast, strength, invert)` |
| **Blend K** | **already there** — the `soft` socket IS the requested smooth-blend parameter |
| Target selection | `Node.sdf_prim_uid`; `SdfFieldSources.by_uid` indexes every prim, so cross-body targets work |
| **WGSL evaluation** | **PLACEHOLDER** — `graph.rs:1478`: *"Deferred WGSL has no scene prim SDF — approximate with height field"*. Evaluates `sn_field_h`, an analytic sine field with **no relation to the chosen primitive** |

So the generator is one wiring job away from being correct **in the bake**, and a shader/BGL
project away from being correct **live**.

---

## Why a toggle, not a silent choice

Shipping it live-enabled would show a wobble unrelated to the target prim — a per-source
divergence, the exact failure class already documented twice in this codebase (`ImageTexture`
white placeholder, synthetic-field curvature). Shipping it bake-only *without saying so* would
look broken ("I added it and nothing happened in the viewport").

An explicit switch makes the trade **visible and the user's**, which is what the owner asked for.

---

## Design

**Reuse `GeneratorRung`** (`model.rs:506`) — it is already the "which path evaluates this layer"
selector, is `#[serde(default)]`, and is documented **APPEND only**. Add one variant:

```
GeneratorRung::BakeOnly     // appended last — never inserted (serde tag order)
```

| Rung | Meaning | Live viewport | Baked map |
|---|---|---|---|
| `Auto` | today's behaviour | shader preview | baked |
| `VertexCache` | per-vertex GPU carry | G-buffer | n/a |
| **`BakeOnly`** | **new** | **nothing drawn** | **correct** |

`BakeOnly` must render **nothing** live rather than the placeholder. Showing a wrong preview is
worse than showing none — that is the whole point of the switch.

### UI

The existing rung combo already sits on the generator row (`layer_ui.rs`). `SdfTarget` layers get
`BakeOnly` as their **default and only** option until the live path exists, with hover text stating
plainly: *"Baked only — no live preview yet. The map is correct after a bake."*

Nothing else changes shape: one more entry in a combo that is already there.

### Target picker

`sdf_prim_uid` is currently only settable from `win_node_editor.rs`. The panel needs its own
picker — a dropdown of the active body's prims by name. **This is the only genuinely new UI.**

---

## Phases

- **S1 — `GeneratorSource::SdfTarget`** wired to the existing `PrimSdfMask` node, `BakeOnly` rung,
  panel prim picker, blend-K (`soft`) exposed. `generator_source_supported` → true.
  **Gate:** round-trip test (source + target uid + soft + grow); a bake produces a mask that
  follows the chosen prim; selecting a different prim changes the result.
- **S2 — "Resume live" (later).** Bind the prim SDF into the deferred/material shader, make the
  WGSL arm real, then allow `Auto` for this source. Same class of shader + BGL work as the AO
  Phase B that is already deferred — **its own session**.

---

## Rules carried in

- **`GeneratorRung::BakeOnly` is APPENDED**, never inserted (serde tag order — save-format red line).
- **The placeholder WGSL is never shown for this source.** `BakeOnly` draws nothing live; when S2
  lands it draws the real field. There is no intermediate state where a wrong preview is displayed.
- **`graph_is_gpu_bakeable` must keep excluding `PrimSdfMask`** — the GPU *map* bake runs the same
  placeholder WGSL, so it would disagree with the CPU bake. Bake stays on the CPU path for this
  source (already true; do not "optimize" it later without making the WGSL real first).
