# Third-party notices

## Bundled

| Component | Where | License |
|---|---|---|
| Geist and Geist Mono fonts, Copyright 2024 The Geist Project Authors | `core_engine/app/assets/fonts/` | SIL Open Font License 1.1 (`core_engine/app/assets/fonts/OFL.txt`) |

## Code provenance: behaviour studied from other projects

Some modeling and sculpting code was written while studying the behaviour (and in places the source) of other open-source tools: **Blender** (GPL-2.0-or-later: bevel, brush stroke overlap, edge-loop selection, dyntopo), **LoopTools** (GPL: circle fitting and the inset circle region) and **Open CASCADE** (LGPL-2.1 with exception: fillet corner states). A small number of functions (mainly in `core_math/src/bevel_op`, `core_math/src/voxel_sculpt/brush.rs` and `core_math/src/half_edge/geom.rs`) are close ports and are therefore treated as derived from GPL code. They are distributed here under the AGPL-3.0-or-later, which is compatible with the GPL-2.0-or-later and GPL-3.0 originals, and **cannot be offered under proprietary terms until they are replaced by independent implementations.** This is why commercial licensing is not yet offered.

Blender's own output was also used as test reference data (`*_ref.json`); that data is program output, not Blender source.

## Not bundled (obtain separately)

| Component | Notes |
|---|---|
| NVIDIA OptiX SDK headers | Proprietary NVIDIA license; install the SDK yourself and set `OPTIX_PATH`. |
| ambientCG PBR materials | CC0 1.0; downloadable from https://ambientcg.com. Not included. |
| HDR environments, matcaps | Not included; use assets whose license allows your use. |
| ComfyUI / FLUX models used by `ai_backend` | Referenced by path only. Each model has its own license (for example FLUX.1 Kontext [dev] is non-commercial); check before use. |

AZRA does not distribute any content library. Assets you import keep their own licenses.

Geist source: https://github.com/vercel/geist-font. The fonts are not sold standalone.

## Rust dependencies

Cargo dependencies keep their own licenses; see `core_engine/Cargo.lock`. Run `cargo install cargo-about && cargo about generate` to produce the full list before a binary release.
