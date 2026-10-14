# RENDERER_ROADMAP.md — AZRA viewport toward Unreal-class PBR

**Owner-approved direction 2026-07-18.** The path tracer (WGSL, `pt_*`) is the
ground-truth beauty renderer; the deferred viewport's job is **great real-time
lookdev**, so we skip game-engine machinery that doesn't serve that (Nanite —
the DC-mesh campaign already solved geometry scaling; virtual shadow maps —
overkill) and spend effort on the items below.

## Shipped this session (the foundation)
Deferred PBR · metallic/roughness materials with albedo/normal/roughness/metallic
maps + triplanar box projection + library drag-drop + Modo shader-tree ·
cascaded shadows (4 readback-proven bug fixes + resolution/softness controls) ·
**SSAO → GTAO + bent normals** (self-shading under IBL) · split-sum IBL from
loadable HDRIs (prefiltered specular + SH9 irradiance + BRDF LUT) · SSR (rebuilt:
DDA + refinement + energy-conserving composite) · tonemapper selector
(Classic/AgX/Filmic/Reinhard/Linear) · per-project environment persistence.

## The gap to Unreal-class, ranked by impact-per-effort

### Tier 1 — closes most of the visible gap
1. **GTAO + bent normals** — ✅ DONE (b28ef5b). Directional IBL occlusion; the flat-cube fix.
2. **TAA (temporal anti-aliasing)** — NEXT. FXAA is a blur; TAA gives clean stable
   edges, kills shimmer/crawl, and unlocks everything temporal (temporal AO/SSR,
   upscaling). Needs motion vectors + history buffer. ~2-3 days. Biggest single
   "looks professional" jump.
3. **Reflection fallback (probes / captured cubemaps)** — SSR reflects only
   on-screen; probes fill the off-screen gap so metals always reflect something
   plausible at screen edges. Medium.
4. **Auto-exposure (eye adaptation)** — consistent professional brightness across
   scenes regardless of HDRI intensity. Small, high polish-value.

### Tier 2 — production polish
Color grading (lift/gamma/gain + grading LUT beyond the tonemap curve) · bloom
quality pass · depth of field for lookdev shots.

### The AZRA power move
5. **SDF-traced GI + soft shadows + AO** — cone-trace the *existing baked SDF
   field* for one-bounce indirect light, soft area shadows, accurate off-screen
   AO. This is Lumen-lite, and AZRA is uniquely positioned: its native
   representation is the thing Lumen has to fake. ~1-2 weeks. The distinctive path.

## Discipline (every slice)
Readback-proven (the shadow-saga standard: real G-buffer probe, numeric asserts,
determinism) · runtime-only settings (save-format red line; env path persists
via FL4ENVIR) · OFF-toggleable / default-safe so nothing changes look silently ·
gated (graphics Naga + render + app + goldens/pick) · one WGSL edit per file.
