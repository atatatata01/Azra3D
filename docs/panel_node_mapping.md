# Material panel → node mapping (MATERIAL_PANEL_SPEC §4-P2, C1)

Deterministic mapping: **every panel widget → exactly one named node / node property.**
A widget with no node equivalent is a spec violation and is surfaced here as such —
never invented (C1). Built alongside the P2 implementation
(`app/src/material_panel/{model,lower,edit,view}.rs`).

Terms: **Master** = the material graph's `MaterialOutput` node (`NodeGraph::output_node()`);
`MO_*` = its input socket indices (`shader_nodes/graph.rs:176`). Value-channel codegen is the
Master node's **socket default** (`InputDefault`), written through the one-material sync
`NodeGraph::pull_from_pbr` (called by `sync_pbr_into_material_graph` on every panel commit).
Engine mirror = the `PbrMaterial` field the same edit lowers onto (`lower_to_material`).

## Header

| Widget | Node | Engine mirror | Status |
|---|---|---|---|
| Material name / preview / cost readout | — (read-only display, no edit) | — | display-only, allowed (§2) |
| "Open shader tree" button | navigates to the Master node | — | navigation, allowed |

## Shader model

| Widget | Node | Engine mirror | Status |
|---|---|---|---|
| ShaderType segmented (PBR/SSS/Glass/VFX/Adv) | Master node's shader model (`PbrMaterial.shader_type`, the node IS the material — §9: one shared struct, lossless by construction) | `shader_type` | LIVE (P2) |

## Texture space (material-level; per-layer override = P3)

| Widget | Node | Engine mirror | Status |
|---|---|---|---|
| Projection (Triplanar/UV) | fast path: `TexMap.projection`; subgraph path: `ImageTexture.projection` (Box→`Triplanar`, UV→`PlanarXY`) | `TexMap.projection` per inheriting map | LIVE (P2/P3) |
| Tiling (uniform) | fast path: `TexMap.uv_scale`; subgraph path: `UVTransform` Scale X/Y | `TexMap.uv_scale` per inheriting map | LIVE (P2/P3) — material-level control stays uniform (`TexMap` has ONE `uv_scale`, bincode-frozen — see P3 verdict below) |
| Offset (material-level) | **NO engine binding on the fast path** — `TexMap` has no offset field and CANNOT get one (P3 verdict: `albedo_map: TexMap` is bincode-serialized inside `PbrMaterial` in the .fl4 blob; adding a field shifts the layout of EVERY existing save — the confirmed field trap). | — | read-only + tooltip. Per-LAYER offset works — it rides the `UVTransform` node in the layer subgraph (below) |

## Channels — Value state (live widgets)

Each channel row's widget writes the doc, lowers to the `PbrMaterial` field, and the codegen
writes the Master socket **default** listed (unconnected socket = the value; C1).

| Channel widget | Master socket (node property) | Engine mirror |
|---|---|---|
| Base color (color) | `MO_ALBEDO` default | `albedo` |
| Roughness (drag 0–1) | `MO_ROUGHNESS` default | `roughness` |
| Metalness (drag 0–1) | `MO_METALLIC` default | `metallic` |
| Opacity (drag 0–1) | `MO_OPACITY` default | `opacity` |
| Emission (color) | `MO_EMISSION` default | `emission_color` |
| Normal (drag 0–4) | `MO_NORMAL_STRENGTH` default | `normal_strength` |
| Bump | **map-only** (no scalar field; `MO_HEIGHT` is map-driven) | — ("map-only" caption, no widget; image layer = P3) |
| Displacement | **map-only** (`MO_DISPLACEMENT` map-driven) | — (same) |
| Clearcoat (drag 0–1) | `MO_CLEARCOAT` default | `clearcoat` |
| Clearcoat roughness (drag 0–1) | `MO_CLEARCOAT_ROUGH` default | `clearcoat_roughness` |
| Sheen (drag 0–1) | `MO_SHEEN` default | `sheen` |
| Sheen tint (drag 0–1) | `MO_SHEEN_TINT` default | `sheen_tint` |
| Anisotropy (drag −1–1) | `MO_ANISOTROPY` default | `anisotropy` |
| Anisotropy rotation (drag 0–1) | `MO_ANISOTROPY_ROT` default | `anisotropy_rotation` |
| IOR (drag 1–3) | `MO_IOR` default | `ior` |
| SSS radius (drag 0–10) | `MO_SSS_RADIUS` default | `sss_radius` |
| SSS color (color) | `MO_SSS_COLOR` default | `sss_color` |

> **2026-08-20 (SSSCOLORMAP):** SSS color is now ALSO a real map channel — `MapKind::SssColor` (FL4MATX2 wire tag 7) → `PbrMaterial.sss_color_map` (`#[serde(skip)]`); panel image/noise/generator layers lower + bake into it (CPU bake only; GPU plan falls to CPU), sampled by `gbuffer.wgsl`'s SSS wrap approx (replace-by-strength). Triangle PT: layout lane only, no SSS transport yet.
| Glass tint (color) | `MO_GLASS_TINT` default | `glass_tint` |

Note: there is **no Specular channel** — no engine field, no Master socket (model.rs header;
excluded per C1 rather than invented).

## Channel structure widgets

| Widget | Node | Status |
|---|---|---|
| "+ Add channel" | appends the doc channel; node effect = the channel's Master socket default becomes panel-authored | LIVE (P2, transaction) |
| Row context "Remove channel" | Value: doc-only. Layered/GraphDriven: confirmation, then clears the `TexMap` slot and (GraphDriven) prunes the channel's subgraph (`NodeGraph::prune_master_input` on `kind_sockets`) | LIVE (P2) |
| Row context "Revert to panel control" | disconnects the channel's Master sockets + deletes nodes reachable ONLY through them; state → Value | LIVE (P2, confirmed, transaction) |
| Connection dot / "graph" badge | click-through to the Master node in the shader tree | LIVE (navigation) |
| "conflict" badge | surfaced when a graph bake was gated off a panel-owned slot (`gate_mask_by_panel_ownership`) | LIVE (P2) |
| Expanded layer rows — **image layers LIVE (P3, below)** | ImageTexture / ColorCorrect / MixRgb / UVTransform | LIVE (P3); generator + mask rows read-only (**P6**) |
| "show only driven channels" filter | — (view state only, no material meaning) | allowed (§2 display) |

## P3 — Image layers (expanded Layered channel)

**Lowering decision tree** (`material_panel/lower.rs::channel_lowering`, applied per channel at
every `mp_commit_doc_edit`):

1. **FAST** — exactly one visible, unmasked Image layer, default adjust, `Normal` blend,
   opacity 1 → the channel's direct `TexMap` slot (the P0/P2 path, byte-identical behavior).
   Uniform tiling + projection ride the TexMap whether inherited or overridden.
2. **SUBGRAPH** — anything richer (≥2 layers, adjust ≠ default, blend ≠ Normal, opacity ≠ 1,
   Constant layers, per-layer offset, non-uniform tiling) → a deterministic **panel-generated
   node subgraph** (`regen_channel_subgraph`), and the existing **auto-apply bake** composites it
   into the channel's ONE `TexMap` slot (`enqueue_material_node_graph_bake`). This is how
   multi-layer works on a one-TexMap-per-channel engine: the graph is the compositor, the bake
   install is the writer, the slot holds the flattened result.
3. **STUB** — masked layers / Generator content → not lowered (fail-soft), P6 scope.

**Panel-generated vs hand-authored tagging:** every node the panel emits carries
`Node.label = "mp:{ChannelKind as u32}:{role}:{layer}[:inh|ovr]"` (label is a free-form
serialized String — FL4NODES JSON — so NO enum/struct shape change was needed).
`panel_authored_socket` walks the full upstream closure of the channel's master wire: **all**
tagged ⇒ panel-owned (derive re-parses layers, reconcile never promotes, the bake-install gate
lets the composite through); **any** untagged node ⇒ hand-authored ⇒ `GraphDriven` exactly as P2.
Roles: `img` ImageTexture · `nse` Noise · `str` VectorMath Scale (strength) ·
`kf`/`kc` ConstantColor (scalar/color) · `tc` TexCoord · `uvx` UVTransform ·
`adj` ColorCorrect · `inv` Invert · `op` ConstantValue (opacity) · `mfac` MathBinary
(mask×opacity) · `mix` MixRgb (blend mode) · `base` ConstantColor (channel base value) ·
`gen`/`cav`/`dir` generators · `msk*` mask content.

**Feedback safety:** the composite installs into the same map slot an ImageTexture would
legacy-sample, so panel `img` nodes pin their SOURCE via the new `Node.image_tex_id`
(`#[serde(default)]`, JSON-only persistence) and the bake snapshot carries
`ImageMapSources.by_tex` — re-bakes never read back their own composite.

| Widget (per layer) | Node / node property | Status |
|---|---|---|
| Load… / thumbnail / filename | `ImageTexture.image_tex_id` (pinned texture-array slot; shared `request_map_file_dialog` → `mp_pending_layer_load` → `mp_apply_layer_load`) | LIVE |
| Strength (drag 0–1) | **fast:** `TexMap.strength`; **subgraph:** dedicated `mp:k:str:i` `VectorMath` **Scale** after content (Image/Noise Mask stays 1.0 — strength is its own node) | LIVE |
| Visibility eye | node presence — hidden layers emit NO nodes (regen skips them; not recoverable by derive-from-graph, doc-store keeps them) | LIVE |
| Opacity (drag 0–1) | dedicated `mp:k:op:i` **ConstantValue** → MixRgb Factor (or `mfac` B when masked) | LIVE |
| Blend combo (Normal / Multiply / Screen / Overlay / Add) | `mp:k:mix:i` **MixRgb** with `mix_blend` only (opacity is a separate `op` node) | LIVE |
| Drag-reorder (⠿ handle) | layer order = blend-chain wiring order (collect-then-apply on release, target from last-frame block centers) | LIVE |
| Remove layer (handle context menu) | strips the layer's nodes on regen | LIVE |
| "+ Add layer" / row context "Add image layer" | appends an `img` node group on regen | LIVE |
| Adjust · Contrast (−1–1) | `ColorCorrect` input **6** default | LIVE |
| Adjust · Saturation (0–2) | `ColorCorrect` input **2** default | LIVE |
| Adjust · Gamma (0.1–4) | `ColorCorrect` input **4** default | LIVE |
| Adjust · Invert | `Invert` node after the ColorCorrect | LIVE |
| Projection · inherited/Override toggle (owner §8, ADVANCED) | `Layer.placement.space` None/Some; `inh`/`ovr` flag in the `img` tag round-trips it | LIVE |
| Override · projection (Tri/UV) | `ImageTexture.projection` (`Triplanar`/`PlanarXY`) | LIVE |
| Override · tiling U/V (non-uniform OK) | `UVTransform` **Scale X/Y** input defaults | LIVE |
| Override · offset U/V | `UVTransform` **Offset X/Y** input defaults — the offset's engine home (TexMap could not be extended, see verdict above) | LIVE |

**P3 tiling/offset verdict (investigated):** `TexMap` persists TWO ways — `albedo_map` is a
plain bincode field inside `PbrMaterial` (which lives in the `.fl4` blob via
`NodePayload::Material`), the other six ride the `FL4MATX2` side-channel
(`MaterialChannelRecord`). Because of the albedo path, adding `offset`/`tiling_v` to `TexMap`
changes the bincode layout of every existing save → **NOT provably safe, not done** (the
project's confirmed bincode-field-trap). Uniform tiling stays the TexMap contract; non-uniform
tiling + offset are expressed ONLY through the subgraph path's `UVTransform`, which needs no
format change. Material-level offset remains read-only with a tooltip.

## P6 — generator layers (spec §4-P6, gated per §10 backend gaps)

| Panel widget | Node equivalent | Status |
|---|---|---|
| "+ Add layer" → Generator (SDF bodies only; disabled with tooltip otherwise) | appends a `gen`/`cav` node group on regen | LIVE |
| Source combo · Curvature | `SdfCurvature` node, `sdf_prim_uid = 0` (whole-body field). Tag `mp:k:gen:i:{sg|cx|cc}` | LIVE |
| Source combo · Cavity | `Cavity` node (concave-only). Tag `mp:k:cav:i` | LIVE |
| Source combo · Ambient occlusion / Slope | **disabled entries** — no bake backend (AO eval is the synthetic demo field; Slope sees a constant normal on the planar bake domain). Honest tooltips, spec §10 | GATED |
| Radius (world units, C4) | generator node **Epsilon** input default | LIVE |
| `min radius ≈ X` caption | `curvature_radius_floor` = 1% of body AABB diagonal — the SAME clamp `sdf_curvature_masks` applies (`e = eps.max(diag*0.01)`), bounds via `SdfFieldSources::body_aabb` | LIVE |
| Output ± / Cx / Cc (Curvature only; spec "ship all three") | `SdfCurvature` output socket 1/2/3; Signed additionally through a `gnorm` `Remap` (−1..1 → 0..1, 0.5 = flat) | LIVE |
| Remap min/max | `grm` `Remap` (from min..max → 0..1, clamped; ALWAYS emitted so the layer tail exposes output 0) | LIVE |
| Invert | `ginv` `Invert` node (distinct from adjust's `inv`) | LIVE |
| Blur | **omitted** — no blur node exists (C1: no control without a node) | — |
| Projection / tiling / Adjust on generators | **not offered** (spec §2 rule; scalar masks have no color-correct) | — |

## Ownership / choke points (C1)

- Promotion: `commit_material_node_graph_edit` (win_node_editor.rs — every hand edit) and
  `enqueue_material_node_graph_bake_quality` (Apply / presets / live bakes) call
  `mp_reconcile_graph_ownership` → wired primary socket ⇒ doc channel `GraphDriven{Master}`.
- Bake install gate: `poll_graph_bakes` → `gate_mask_by_panel_ownership` — a panel-`Layered`
  channel's `TexMap` slot is never installed into or cleared by HAND-authored graph bakes.
  P3: channels in `panel_generated_mask(graph)` pass the gate (the install IS the panel's
  own layer composite), no conflict badge.
- Hand-load origin: `poll_file_dialogs` → `mp_mark_hand_loaded_map` records panel ownership
  (skipped when `mp_pending_layer_load` targets the channel — the per-layer route owns it).
- P3 codegen choke point: `mp_commit_doc_edit` → `mp_lower_rich_channels` (regen/strip panel
  subgraphs + enqueue the composite bake). Per-layer image loads land via `apply_map_slot` →
  `mp_apply_layer_load`.

## Owner UX pass (2026-07-26) — inline editors, blend modes, per-layer masks

### #1 — plain (Value-state) channel inline editors
| Panel widget | Node / engine equivalent | Status |
|---|---|---|
| Scalar channel (Roughness / Metalness / Opacity / …) — compact **Slider** (track + value) | `write_value` → `PbrMaterial` scalar field; engine clamp range per `kind_scalar_range` | LIVE (was a bare `DragValue`; now `egui::Slider`, ~ROW_H tall, transactional via `mp_commit`) |
| Color channel (Base color / Emission / SSS color / Glass tint) — clickable **color swatch** picker | `write_value` → `PbrMaterial` Vec3 (linear↔srgb via `material_editor` helpers) | LIVE (28×16 `color_edit_button_srgba`) |
| New material | `default_for_new` seeds Base color / Roughness / Metalness / Normal / Displacement channels in `Value` state | LIVE |

### #2 — layer blend modes (all real MixRgb nodes; C1)
`MixRgb` gained a `mix_blend: MixBlend` field (`Mix`/`Multiply`/`Screen`/`Overlay`/`Add`,
append-only, `#[serde(default)] = Mix`). Blender "Mix Color" semantics:
`result = mix(below, blend_op(below, layer), Factor)`. Implemented identically in the WGSL
codegen (`shader_nodes/graph.rs`) and the CPU bake (`node_graph_bake.rs`) — parity is required.
Editor exposes a Blend combo on any MixRgb node (node/panel duality #3).

| Panel BlendMode | MixRgb.mix_blend | Formula (per channel, a=below b=layer) |
|---|---|---|
| Normal | Mix | `b` (straight lerp) |
| Multiply | Multiply | `a*b` |
| Screen | Screen | `1-(1-a)(1-b)` |
| Overlay | Overlay | `a<0.5 ? 2ab : 1-2(1-a)(1-b)` |
| Add | Add | `a+b` |

**One feature → one node (image-editor base):** blend mode is MixRgb only (`mix_blend`);
opacity is always a separate `op` ConstantValue wired into Factor (or into `mfac` with the
mask). Strength is always a separate `str` VectorMath Scale after Image/Noise. Adjust stays
ColorCorrect; invert stays Invert. `parse_panel_channel` reads blend from
`role("mix").mix_blend`, opacity from `role("op").const_float`, strength from
`role("str")` Scale input (legacy Mask fallback for older graphs).

### #2/#3 — per-layer masks (Photoshop-style)
`Layer.mask: Option<LayerContent>` is now authored in the UI (was a read-only caption) and
lowered to a real subgraph that multiplies the layer's mix Factor.

| Panel widget | Node equivalent (tag) | Status |
|---|---|---|
| "+ Mask" → Image | `ImageTexture` `mp:k:mskimg:i` (strength → Mask input default) | LIVE |
| "+ Mask" → Constant | `ConstantColor` `mp:k:mskk:i` (scalar) | LIVE |
| "+ Mask" → Curvature (SDF only) | `SdfCurvature` `mp:k:mskgen:i` (concave-only default) | LIVE |
| Mask Invert | `Invert` `mp:k:mskinv:i` | LIVE |
| non-zero curvature output socket | identity `Remap` `mp:k:mskrm:i` so mask tail exposes output 0 | LIVE |
| mask → mix factor | `MathBinary(Multiply)` `mp:k:mfac:i` (A = mask, B = opacity) → MixRgb Factor (input 0) | LIVE |
| mask image "Load…" | shared file dialog; `mp_pending_layer_load` tuple gained an `is_mask` flag routed to `layer.mask` in `mp_apply_layer_load` | LIVE |

Masked layers force a base (`mp:k:base:i`) and go `Subgraph` (`channel_lowering`: `mask.is_some()`
is "rich"). A mask whose generator source has **no bake backend** (AO/Slope/…) still stubs the
whole channel fail-soft. Round-trip covered by `p3_1_mask_and_overlay_round_trip`.
