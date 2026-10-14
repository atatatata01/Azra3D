# AZRA

A 3D modeling, sculpting and rendering application written in Rust (wgpu): SDF, voxel and polygon modeling, a physically based viewport and a path tracer.

**Created and maintained by Dimitri Neron.**

> Status: early and moving fast. Windows is the primary platform.

## Build and run

```bash
cd core_engine
cargo run -p app --release
cargo test -p core_math
```

Requirements: a recent stable Rust toolchain and a GPU with Vulkan or DX12 support.

### Optional: OptiX path tracer

The `cuda_render` crate needs NVIDIA's OptiX headers, which are proprietary and **not included** in this repository. Download the OptiX SDK from NVIDIA and point `OPTIX_PATH` at the folder that contains `include/optix.h`.

### Content (materials, environments, matcaps)

The app ships with a neutral gradient environment only. Material and environment libraries are not bundled:

- PBR materials: free CC0 sets from [ambientCG](https://ambientcg.com), placed under `assets/PBR/<Name>/`.
- HDR environments: any `.hdr` file placed under `assets/HDR_Labs/<Name>/`.
- Matcaps: any `.png` placed under `assets/Matcaps/`.

## License

AZRA is licensed under the **GNU Affero General Public License v3.0 or later** (AGPL-3.0-or-later; see [LICENSE](LICENSE)).
Copyright (C) 2026 Dimitri Neron.

You are free to use, study, modify and distribute it under the AGPL's terms. If you distribute a modified version, or run one as a network service, you must publish its source under the same license and keep the copyright notices.

Commercial licensing (use without the AGPL's obligations) is planned but not yet available: parts of the code are still being rewritten to make that possible (see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

The names **AZRA** and the AZRA logo are trademarks of Dimitri Neron and are not covered by the license. See [TRADEMARK.md](TRADEMARK.md).

Third-party material is listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md): a short contributor agreement (CLA), signed through a bot comment on your first pull request, is required so the project can keep offering commercial licenses.

## Support the project

Sponsor development on GitHub Sponsors (link to be added by the author).
