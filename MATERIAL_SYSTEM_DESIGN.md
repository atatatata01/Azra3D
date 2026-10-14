# PBR Material System — texture maps for all object types (investigation + design)

Owner: control the PBR material *surface* of all object types (SDF / voxel / poly) — load basic texture
MAPS (albedo, roughness, metallic, normal, height) + tweak; target UX = a Substance-Painter layered material
(per-channel maps + masks + "Cube Mapping" triplanar). Full investigation in agent report af720b50.

## Current state — the owner is right: NO functioning texture-map shading
- **`PbrMaterial`** (`core_math/src/pbr_material.rs:55`) is **scalar + procedural only**: albedo/roughness/
  metallic/emission/opacity + in-shader noise/edge-wear/cavity/Material-DNA. **Zero texture-map handles.**
- Texture sampling EXISTS but is **DEAD in production**: the one albedo triplanar path
  (`sample_triplanar`, `SdfPrimitive.texture_id`) lives in the `#region LEGACY_TEXTURE` block that is
  **stripped from every shader build** (`shader_build.rs:273`) and belongs to the **disabled analytical SDF
  path**. The live paths — raster `gbuffer.wgsl` (voxel + poly + SDF-Object-Mode) and `volume_preview.wgsl`
  (SDF edit) — do **no material texture sampling at all**.
- **No `texture_2d` material textures, no array/bindless** — only packed-u32 storage buffers + hand-written
  bilinear. Poly has real UVs (passed, unused); SDF/voxel have none.
- **Material UI is duplicated 4×** (SDF rich; voxel/poly/NURBS trivial) — **no map-loading UI anywhere.**

## ⭐ The near-free win: AZRA already GENERATES the maps
The startup "Texture server" is an **AI Stable-Diffusion backend** (`ai_backend/server/texture_server.py`,
spawned `gpu_init.rs:609`) that already produces a **full 5-map PBR set** (albedo/normal/roughness/metalness/
height) per prompt. **But the generated maps are preview-only — they never reach shading** (only the albedo
thumbnail is uploaded, into the dead analytical path). So the highest unlock-per-effort is simply **wiring the
already-generated AI maps into a material that the live shaders sample.** Day-one "load maps + tweak" win.

## MVP — load 4–6 maps + triplanar, on ALL object types
1. **Data** (`pbr_material.rs`): add `TexMap { tex_id, strength, uv_scale, projection /*0=triplanar,1=UV*/ }`
   and 6 slots on `PbrMaterial` (albedo/roughness/metallic/normal/height/AO), all `#[serde(default)]` →
   v6-safe additive. Existing scalars become **tint** (`albedo_final = albedo × sample(albedo_map)`).
   `projection` = the screenshot's "Cube Mapping" (triplanar default for SDF/voxel, UV for poly).
2. **Textures** (new `app/src/material_textures.rs`): ONE `texture_2d_array` RGBA8 atlas (`MaterialTextureArray`),
   **generation-counted** (mirror `VolumeAtlas.texture_generation`), with `load_map(path)` decoding via the
   `image` crate and **deferring `write_texture` to `poll_background_tasks`** (Frame Ordering Contract).
   One new binding pair (array + sampler) → minimal BGL surface.
3. **Shaders (the real work — BOTH live paths):** `gbuffer.wgsl` (poly = `in.uv`; voxel + SDF-baked-tris =
   triplanar on world pos/normal) and `volume_preview.wgsl` (triplanar at the raymarch hit). Sample albedo/
   rough/metal/normal/AO into the G-buffer. Keep OUT of the stripped analytical/GLSL path (stays disabled).
4. **Unified editor** (new `app/src/material_editor.rs::draw_pbr_material`): extract the SDF editor into ONE
   shared widget, call it from SDF/voxel/poly/NURBS (kills the 4× dup — "Single Responsibility + Reuse").
   Per-channel row: `[Load Map…] [clear] [value slider] [Cube Mapping ▾]`. **Plus a one-click "Apply AI maps →
   material"** routing the existing `ai.ai_tex_maps_png` 5-map set into the slots.

**Why one editor serves all:** every body type holds a `PbrMaterial`; filling its map slots makes all object
types texture-mapped through the same two shaders.

## FULL — the Substance layer stack = THIRD sibling of the stack pattern
`LayeredMaterial { layers: SmallVec<[MaterialLayer; 8]>, cavity_modulator }`, each layer = 6 per-channel maps
+ values + a mask + blend + opacity + projection — the literal screenshot. Same **"ordered slots → CPU fold →
flat POD → ONE bounded GPU loop (≤8 layers)"** discipline as the brush + modifier stacks. The mask system
("Condition Mask" / "Edge Scattering") is **already built** — it's the Material-DNA curvature/cavity
conditions (`dna_eval_condition`). Additive `#[serde(default)]` serialization. No new render path (Display
Invariant safe — same two shaders, richer loop).

## Conflicts to resolve (CLAUDE.md)
1. **"Materials Are Referenced, Never Embedded" is VIOLATED today** — every body embeds `PbrMaterial` and
   `SdfPrimitive` has BOTH `material` + `material_id`. Mid-migration. Extend `PbrMaterial` (flows through both
   paths); write through the graph material node; **resolve embed-vs-reference before the layered tier** (don't
   duplicate a layered material per-prim).
2. **GPU serialization lockstep**: new map slots touch `RasterMaterial` + `MaterialData` + SDF prim cold
   floats — keep in sync; hold `FLOATS_PER_PRIM` parity (the `validate_glsl_floats_per_prim_sync` CI test).
3. **BGL 6-site chore**: one array + one sampler, added only to the 2 live layouts → one binding pair.
4. **Frame Ordering + generation counter** for texture create/replace (deferred flag in panel → upload in
   `poll_background_tasks`).

## Effort + sequencing
Multi-day → ~1.5 week for the MVP (the two shader edits + the D2Array texture system are the bulk; expect
shader-recompile iteration). **Biggest unlock-per-effort: wire the AI-generated maps** (they already exist).
Sequence: (1) `PbrMaterial` maps + D2Array + shared editor + AI-map wire-up (albedo first); (2) rough/metal/
normal/height/AO sampling in both live shaders; (3) the layered stack (third sibling). Shipping (1)–(2) closes
the "no material options" gap on all object types.
