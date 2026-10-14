# Material Layer UX — Plan

**Owner directive 2026-07-28**, six requests from live testing. Verified against the code
before scoping — three are *bugs*, three are *features*, and separating them changes the order
they should be done in.

---

## The six requests, triaged

| # | Request | Verdict | Where |
|---|---|---|---|
| 1 | Projection needs Box / Sphere / Cylinder / Planar | **Feature — cheap.** All four already exist as node types | `node_type.rs:29-33` |
| 2 | Triplanar needs a blend control | **Feature — cheap.** Blend sharpness is hardcoded | `gbuffer.wgsl:121` |
| 3 | Changing Tiling shows no scale change | **BUG.** Tiling *is* wired to the sampler | `gbuffer.wgsl:123` |
| 4 | Changing Offset shows no movement | **BUG (known).** Offset never reaches the shader | panel admits it, `view.rs:473` |
| 5 | Tiling U/V link should default ON, unlink to edit separately | **Bug-ish.** Currently an action button, not a state toggle | `view.rs` |
| 6 | Adjustments too limited — want a curve with colour input | **Feature — large.** | new |
| 7 | Masks should work like Photoshop (any layer as mask, thumbnail slot, drag to assign) | **Feature — large, architectural** | `model.rs` `Layer.mask` |
| 8 | Layer **opacity** should appear in the settings section | **Trivial — do it in M1.** Exists, but only as the compact `DragValue` in the tree row (`layer_ui.rs:258`); the docked inspector never shows it | `layer_ui.rs` |

---

## Findings that shape the work

**Projection is NOT bincode-frozen.** `MapProjection` (`pbr_material.rs:185`) has only `Box`
and `Uv`, but it derives no `Serialize`; it persists as a `u32` code
(`material_panel/model.rs:453`, `MapProjection::as_u32()`). Adding variants is therefore
**append-safe** — new codes 2/3/4 with existing files reading 0/1 unchanged. This was the main
risk and it is clear.

**The projection maths already exists.** `CylindricalMapping`, `SphericalMapping`,
`PlanarMapping`, `BoxMapping` and `Triplanar` are all real node types with codegen. Exposing
them in the panel is wiring, not new maths.

**Triplanar blend is a hardcoded constant:**
```wgsl
w = pow(w, vec3<f32>(4.0));   // sharpen blend (reduces stretch seams)
```
Making the `4.0` a per-layer parameter is the whole of request #2.

**Tiling vs Offset are NOT the same bug.** Tiling *is* passed to the sampler
(`p.yz * tiling`), so #3 is a wiring/plumbing failure somewhere between the panel value and
that uniform. Offset genuinely has no path at all — the panel's own hint text says
*"material offset: no TexMap field (bincode-frozen)"*. Different fixes, and #3 should be
diagnosed before it is "fixed", because the value may be reaching the shader and being lost
elsewhere (e.g. only applied on re-bake).

---

## Phases

Ordered by (owner-visible value) ÷ (risk). Bugs first — a broken control is worse than a
missing one, because it teaches the user the feature does not work.

### M1 — Fix the controls that lie (bugs #3, #4, #5)
- **#3 Tiling:** trace panel value → `TexMap.tiling` → uniform → sampler. Find where it is
  dropped. Do NOT add a second path; the sampler already consumes it.
- **#4 Offset:** needs a real path to the shader. `TexMap` is bincode-frozen, so the offset
  must ride the same side-channel pattern used elsewhere (`#[serde(default)]` in-blob field, or
  the per-layer subgraph the hint text already mentions). **Do not add a field to `TexMap`.**
- **#5 Link toggle:** make U/V link a persisted per-layer *state* (default ON), not an action
  button. Unlinking reveals independent U and V.

- **#8 Opacity in the inspector:** the value already exists and is already editable — but only
  as the cramped `DragValue` in the tree row. The docked settings pane should carry it as a
  proper slider (matching the reference layout: `Blend [mode]  Opacity [====o] 1.00`). Pure UI;
  no model, no shader, no save-format involvement. Cheapest item in this file.

**Gate:** owner sees scale change on Tiling and movement on Offset. No test can prove this —
it must be visually confirmed.

### M2 — Projection modes (#1) + triplanar blend (#2)
- Extend `MapProjection` with `Sphere`, `Cylinder`, `Planar` (codes 2/3/4 — **append only**).
- Panel dropdown lists all of them; the fragment shader gains the matching sample paths, reusing
  the node codegen's existing maths rather than re-deriving it.
- Add a per-layer `blend` (triplanar sharpness) replacing the hardcoded `4.0`, exposed only when
  the projection is Box/triplanar.

**Risk:** every new projection needs a shader path AND a bake path or the two diverge — the
exact class of bug the GPU-bake work has been fighting. One projection at a time, each verified,
before adding the next.

### M3 — Adjustment curve with colour input (#6)
A per-layer curve editor (RGB + per-channel), applied after the layer's content and before its
blend. Needs: a curve data type that persists (in-blob, not `TexMap`), a curve widget, and
evaluation in **both** the CPU bake and the WGSL codegen — parity or it is a divergence source.

### M4 — Photoshop-style masks (#7)
The largest and most architectural. Today `Layer.mask: Option<LayerContent>` — a mask is a
*second content* embedded in the layer. The owner wants: **any layer usable as a mask**, a mask
slot shown next to the thumbnail, drag-any-layer-to-assign, and create-new-from-a-list.

**OWNER DECISION 2026-07-28: match Photoshop exactly — "everyone knows the software".**
That settles the open design question, and it makes M4 much smaller than first scoped:

**The data model is ALREADY Photoshop's.** `Layer.mask: Option<LayerContent>`, and the model's
own doc says *"The mask is a slot ON the layer, never a sibling stack entry."* That IS the
Photoshop layer-mask model — a mask belongs to one layer, is not itself a stack entry, and is
not drawn as a layer. So the earlier worry ("a shift from embedded content to a layer
reference, touching model + lowering + save format") **does not apply**. No model change, no
save-format change.

What is missing is entirely **UI affordance**:
1. **Mask thumbnail beside the layer thumbnail** in the tree row (Photoshop's two-slot row),
   not the current text-only `+ Add mask` buried in the settings pane.
2. **Click the mask slot to target it** — subsequent edits apply to the mask, with a clear
   focus ring, exactly as Photoshop switches between layer and mask editing.
3. **Create-new-from-a-list** on the mask slot (Noise / Image / Curvature / Fill …) — the same
   content menu the layer itself offers, since a mask is a `LayerContent`.
4. **Drag any layer onto the mask slot** to use it as that layer's mask. NOTE: because a mask
   is a `LayerContent` (a value, not a reference), dropping a layer COPIES its content into the
   mask slot. That matches Photoshop far better than a live reference would — Photoshop masks
   are independent once created — but it must be stated in the UI (the dragged layer is not
   consumed and does not stay linked).

**Deliberately NOT doing** (Photoshop parity is the goal, not Photoshop's whole feature set):
clipping groups, mask feather/density, vector masks. Each is a separate ask if wanted.

Still sequenced after M1–M3 to avoid colliding with the placement/adjust work in the same
file, but the risk is now "UI work" rather than "architectural change".

---

## Rules carried into every phase

- **Any new authored state persists via the in-blob `#[serde(default)]` side-channel**, never by
  adding a field to `TexMap`/`PbrMaterial` (bincode-frozen — CLAUDE.md save-format red line).
- **CPU bake and WGSL codegen must gain the feature together.** A feature in one and not the
  other is a silent divergence; that is exactly what produced the `ImageTexture` white-placeholder
  and synthetic-curvature gaps already documented in `GPU_MATERIAL_BAKE_PLAN.md`.
- **Every projection/blend change needs owner visual confirmation** — no automated test can tell
  "the texture moved correctly" from "the texture moved wrongly".
