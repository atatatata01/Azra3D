# Third-party notices

## Bundled

| Component | Where | License |
|---|---|---|
| Geist and Geist Mono fonts, Copyright 2024 The Geist Project Authors | `core_engine/app/assets/fonts/` | SIL Open Font License 1.1 (`core_engine/app/assets/fonts/OFL.txt`) |

## Generated content (no third-party license)

The matcaps in `assets/Matcaps/` and the environments in `assets/HDR_Labs/HDR_Gradient/` are produced by `tools/gen_default_content.py` from mathematical formulas. They contain no third-party images and are covered by the repository's license.

## Free content sources checked (not bundled)

On 2026-10-16 the published terms of [ambientCG](https://docs.ambientcg.com/license/) (CC0 1.0) and [Poly Haven](https://polyhaven.com/license) (CC0) allowed redistribution of their assets inside software, with no attribution required. Those terms can change: check them again before bundling any file, and keep a record of the exact asset and download date.

## Code provenance: behaviour studied from other projects

Some modeling and sculpting code was written while studying the behaviour (and in places the source) of other open-source tools: **Blender** (GPL-2.0-or-later: bevel, brush stroke overlap, edge-loop selection, dyntopo), **LoopTools** (GPL: circle fitting and the inset circle region) and **Open CASCADE** (LGPL-2.1 with exception: fillet corner states). A small number of functions were close ports of that code and are being replaced by independent implementations written from published mathematics (see `docs/legal/CLEAN_ROOM_LOG.md` for each item, its method and its status). As of 2026-10-14 the stroke-overlap factor, circle fitting, inset circle region, edge-loop walker and fillet corner classification have been replaced. **All of `core_math/src/bevel_op/` is still derived from GPL code**: its replacement is deferred until the bevel tool is feature-complete. Code that remains derived is distributed here under the AGPL-3.0-or-later, which is compatible with the GPL-2.0-or-later and GPL-3.0 originals, and **cannot be offered under proprietary terms until it is replaced.** This is why commercial licensing is not yet offered.

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

Cargo dependencies keep their own licenses; see `core_engine/Cargo.lock`. An audit of the 539 resolved packages (2026-10-14, from `cargo metadata`) found only permissive licenses (MIT, Apache-2.0, BSD, Zlib, ISC, Unicode-3.0, CDLA-Permissive-2.0, BSL-1.0, Unlicense, 0BSD). `r-efi` is offered as `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; AZRA uses it under MIT/Apache-2.0. `epaint_default_fonts` (via egui) bundles fonts under OFL-1.1 and the Ubuntu Font License. Run `cargo install cargo-about && cargo about generate` to produce the full attribution text before a binary release.
