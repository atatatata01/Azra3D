# Contrast / Sat / Gamma sliders take 1–2 s to update — diagnosis + fix

**Date:** 2026-07-30
**Owner:** *"when I adjust contrast saturation etc it takes like 1 to 2 seconds before updating, I feel
we could improve that."*
**Status:** **F1 + F2 shipped** (2026-07-30). F3/F4 deferred — F1 alone targets ~15×; F2 adds
per-channel prune. Live→Final settle verified for image graphs (flush Final when no episode;
auto-upgrade when `work_res < target`).

---

## The measurement

CPU bake cost for an **image-bearing** graph (ImageTexture → ColorCorrect → Albedo), one channel,
debug build:

| bake res | time |
|---|---|
| 128² | 8.9 ms |
| 256² | 26.3 ms |
| 512² | 95.7 ms |
| **1024²** | **386.2 ms** |

The owner's panel reads **"Bake 1024²"** and the material drives **4 channels** (Base color,
Roughness, Metalness, Normal). So a single contrast tick costs roughly **4 × 386 ms ≈ 1.5 s** —
matching the reported 1–2 s exactly.

---

## Root cause — three things stack up

### 1. Image graphs are EXCLUDED from the interactive resolution cap

`node_graph_bake.rs:374`:

```rust
if interacting && !graph_uses_image_texture(graph) && full > BAKE_INTERACTIVE_RES {
    BAKE_INTERACTIVE_RES   // 256
} else {
    full                   // 1024 for this material
}
```

Procedural graphs drop to 256² while you scrub. **Image graphs do not** — they bake at full
resolution on every slider tick.

The exclusion is deliberate and the reasoning is written down (`:365-369`): *"downrez + upsample is
exactly what destroys photo-map sharpness … a blurry photo reads as a BROKEN texture rather than a
preview."* **That reasoning is sound for the SETTLED result and wrong for the LIVE scrub** — see the
fix.

### 2. Image graphs are also excluded from the GPU bake

`node_graph_bake.rs:2271` — `ImageTexture` / `TextureSample` are on the DELIBERATELY EXCLUDED list
because they compile to a white placeholder in WGSL. So this material cannot take the ~8–15 ms GPU
path at all; it is CPU-only by construction.

Combined with (1): **the slowest path, at the highest resolution, every tick.**

### 3. Every tick is a full re-bake of all four channels

The sliders (`layer_ui.rs:1324-1358`) set `out.changed` on every `DragValue` change. Nothing narrows
the work to the channel that actually changed, and an adjust edit is a **pure per-pixel colour
transform** — it does not need the image re-sampled or the noise re-evaluated.

---

## The fix, in order of value

### F1 ★★★ — Let image graphs use the interactive cap *while scrubbing* (small, safe)

The current comment defends against a **blurry settled result**. But the live scrub is already
followed by a Final re-bake on release (the existing Live → Final settle path). So capping *during*
the drag costs nothing permanent.

```rust
// 386 ms -> 26 ms per channel while dragging; the settle re-bake restores full res.
if interacting && full > BAKE_INTERACTIVE_RES {
    BAKE_INTERACTIVE_RES
} else {
    full
}
```

**Expected: ~1.5 s → ~100 ms.** That alone turns it from "wait" into "responsive".

**Gate:** assert `bake_work_resolution_for(image_graph, /*interacting*/ true) == 256` and
`(…, false) == full`. Plus a settle test: after the drag ends, the installed map is full-res
(this is what protects the sharpness the comment cares about).

**Watch out:** confirm the Live→Final upgrade actually fires for image graphs. If it does not, F1
would leave a permanently soft map — that would be worse than the lag. Verify before shipping.

### F2 ★★ — Only re-bake the channel that changed

An adjust edit on `Base color` should not re-bake Roughness, Metalness and Normal. `BakeChannelMask`
already exists and is already threaded through `bake_node_graph_maps_res`; the panel just passes
"everything" instead of the edited channel.

**Expected: another ~4× on top of F1** for this material (4 channels → 1).

### F3 ★★ — Cache the pre-adjust layer result

Contrast / Saturation / Gamma / Invert are a **pure per-pixel function of the layer's composited
output**. They do not change sampling, projection, or noise. So the expensive part — sampling the
source image at `res²` — can be computed once and reused for every subsequent adjust tick.

This is the same shape as `mesh_mod_prefix_cache`, which was **measured at 28–42×** on the modifier
stack (`ba81862` / `342e791`). Invalidate on anything upstream of the adjust (image id, projection,
tiling, offset); keep on adjust-only edits.

**Expected: adjust ticks become a colour transform over cached pixels — near-instant.**

### F4 ★ — Debounce the scrub

If F1–F3 land, this may be unnecessary. If any lag remains, coalesce ticks so a fast drag issues one
bake per settle window rather than one per pixel of mouse movement. **Do this last** — debouncing to
hide cost is a worse fix than removing the cost, and this project has a standing rule against timing
tricks that dodge a problem instead of solving it.

---

## Recommended order

1. **F1** — one condition, ~15× on the dominant term, immediately felt. *Verify the settle re-bake
   fires for image graphs first.*
2. **F2** — reuse an existing mask; ~4× more on this material.
3. **F3** — the structural fix; makes adjust edits cheap regardless of resolution.
4. **F4** — only if still needed.

---

## Notes for whoever implements this

* **Do not "fix" this by lowering `Bake 1024²`.** The owner chose that target; the bug is that a
  *live scrub* pays the *final* cost.
* **Measure after each step.** The numbers above are debug-build, single-channel — release will be
  faster, and the ratio is what matters, not the absolute.
* The existing `[bake] res=… N ms` log line already reports this; watch it while dragging to confirm
  each step lands.
