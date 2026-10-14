# Contributing to AZRA

Thanks for helping. The maintainer is Dimitri Neron; all changes are merged by the maintainer through pull requests.

## How to contribute

1. Open an issue first for anything larger than a small fix.
2. Fork, branch off `master`, make one focused change per pull request.
3. Run `cd core_engine && cargo build -p app` and `cargo test -p core_math` (and `cargo test -p graphics` after shader changes).
4. Read `CLAUDE.md` / `ARCHITECTURE.md` for the project's architecture rules.
5. Commit under your real name or a stable handle, with a `Signed-off-by:` line (`git commit -s`).
6. Open a pull request. `master` is protected: nothing merges without maintainer approval.

## Contributor License Agreement (required)

"Contribution" means any code, asset, documentation or other work you submit to this project. "Maintainer" means Dimitri Neron and his successors and assigns.

By submitting a Contribution you agree that:

1. **Ownership and rights.** You are the author of the Contribution, or you have the right to submit it under this agreement. You keep your copyright. If your employer or school has rights in your work, you have obtained written permission to submit it (or they have waived those rights). If any part was written by someone else, you identify the author and license in the pull request, and that license permits redistribution under the AGPL-3.0 and relicensing as described below.
2. **Copyright license.** You grant the Maintainer a perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable license to reproduce, prepare derivative works of, publicly display, publicly perform, distribute and sublicense your Contribution and derivative works of it, in source or binary form, under any license terms the Maintainer chooses, including the GNU AGPL-3.0 and proprietary or commercial licenses.
3. **Patent license.** You grant the Maintainer and every recipient of the project a perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable (except as stated here) patent license to make, have made, use, sell, offer to sell, import and otherwise transfer the Contribution, for the patent claims you can license that are necessarily infringed by the Contribution alone or combined with the project. If you start patent litigation alleging that the Contribution or the project infringes a patent, the patent licenses you granted to the party you sued end on the date you file.
4. **Public license.** Your Contribution is also licensed to everyone under the AGPL-3.0-or-later, like the rest of the project, and that license is irrevocable.
5. **No warranty.** Your Contribution is provided "AS IS", without warranty (except as stated in clause 1). The Maintainer has no obligation to use or merge it.
6. **Accuracy.** You will tell the Maintainer if you learn that any statement above has become untrue.

**How to agree:** when you open a pull request, the CLA bot comments on it. Reply with exactly this comment:

> I have read the CLA Document and I hereby sign the CLA

By signing you agree to the Contributor License Agreement above for this and all your future contributions to this project. The bot records your signature (your GitHub account and the date) in the `cla-signatures` branch. Pull requests without a recorded signature will not be merged. If you contribute on behalf of a company, the company must sign a separate corporate agreement; contact the Maintainer first.

## Assets and code from elsewhere

Do not add fonts, textures, HDRIs, models or code unless their license allows redistribution under the AGPL-3.0 **and** relicensing under the CLA above. Do not port code from GPL projects (for example Blender) verbatim: write from the mathematics instead. State sources and licenses in the pull request and in `THIRD_PARTY_NOTICES.md`.
