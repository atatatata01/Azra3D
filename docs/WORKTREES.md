# AZRA Worktrees — Ownership Map

**Read this before opening a Claude/agent session.** Canonical rule text: `CLAUDE.md` § *Branch Roles*.

Last verified **2026-08-10** against `git worktree list`, after the landing + cleanup pass.

---

## Live worktrees

| Domain | Branch | Path | Launcher | Owns |
|--------|--------|------|----------|------|
| **Release trunk** | `master` | `F:\PROJECT\FourthLAyerAi` (repo ROOT) | `Dev_Azra.bat`, `Run_Fast.bat` | Everything by default — integration, product bugs, **`ui2/` + theme + docks**, poly/SDF/render landings after review |
| **UI polish (v2)** | `feat/ui-polish` | `…\.claude\worktrees\ui-polish` | `Run_UIv2.bat`, `Dev_UI.bat` | *Deprecated name, kept so the launchers work.* 0 ahead of `master`. UI work no longer has to live here |
| **Poly modifier integration** | `feat/poly-modifier-integration` | `…\.claude\worktrees\poly-mod-integration` | — | Stack gap closure: harden → Extrude/EdgeSplit dual-path → prefix-cache gate → generate/deform mods. Plan: `docs/planning/poly/POLY_MODIFIER_INTEGRATION_PLAN.md`. **Holds ~18 files of UNCOMMITTED work** (`ops_deform.rs`, `ops_wireframe.rs`) — do not delete |
| **Terrain / SDF displace** | `work/grok-1` | `…\.claude\worktrees\grok-1` | — | 0 ahead (terrain image-displace already on `master`), but **holds ~75 files of UNCOMMITTED work**: Dice tool (`ops_dice.rs`), `mesh_curvature.rs`, `POLY_CURVATURE_PLAN.md`, `PT_STRIPE_AND_PERF_PLAN.md` — do not delete |
| **Voxel remesh perf** | `feat/remesh-gpu` | `…\.claude\worktrees\remesh-gpu` | — | Remesh gates 0/1a/1b, `TwinLinker`, GPU-propagation verdict. **Merged to `master` 2026-08-10 at `b80ac9a`** (0 ahead). Active session — new commits here need a follow-up merge |
| **Voxel remesh (octree base)** | `feat/remesh-adaptive-octree` | `…\.claude\worktrees\remesh-octree` | — | Base of `feat/remesh-gpu`; fully contained in the 2026-08-10 merge (0 ahead). Retire once the octree Gate 2 work is scheduled |
| **Voxel remesh live-incr** | `feat/remesh-live-incr` | `…\.claude\worktrees\remesh-live-incr` | `Run_RemeshLive.bat` | Regional S3 + dirty-tri clip on the poly Remesh modifier. Live cage-edit; do not merge to master without review |
| **Poly perf** | `work/poly-perf` | `…\.claude\worktrees\poly-perf` | — | Smooth-modifier 21–24×, lazy base clone, QEF cell placement, `seal_enclosed_air`. **Content fully landed 2026-08-10** — 2 commits cherry-picked, the other 7 were already absorbed by `master`/`feat/remesh-gpu`. Still shows 9 unique SHAs; do not re-merge the branch wholesale |
| **Mesh-map baker** | `feat/mesh-map-baker` | `…\.claude\worktrees\mesh-map-baker` | — | Substance-class baker. G0–G7 (16-bit, BentNormal, cage, UDIM, GPU rays). Still parked: per-map res (D8) |

**Not** "0 ahead everywhere" any more — check before assuming. `work/poly-perf` still carries 9
SHAs whose *content* is on `master`; merging that branch again would drag the older `seal_enclosed_air`
span-flood fix back over the newer run-CCL one. Land by cherry-pick, verified per commit.

Build note (was on the deleted viewport-perf row): no cmake on this machine — prebuilt Manifold
static libs are preserved at `dev/manifold-prebuilt/`; set `MANIFOLD_CSG_LIB_DIR` to it plus
`MANIFOLD_CSG_LIB_KIND=static`.

List live checkouts / open the menu:

```bat
git worktree list
Worktrees.bat
```

---

## Branch-role rule (REDEFINED 2026-08-02, amended 2026-08-08 — owner-directed)

1. **`master` is the RELEASE TRUNK**, checked out at the repo ROOT. It ships to customers.
2. **`sandbox` (the former master) is the DEV sandbox** for experiments and feasibility spikes (e.g. `op_bench`).
3. **`feat/ui-polish` and `feat/material-render` are deprecated names**, fast-forwarded into `master`. Their worktrees still function; new work branches off `master`.
4. **The old "UI is authored only on `feat/ui-polish`" gate is RETIRED (2026-08-08).** `ui2/`, theme, dock and shell work lands on `master` like anything else — the branches are the same commit, so the gate only forked history. `Run_UIv2.bat` (sets `AZRA_UI=v2`) still builds the ui-polish worktree if you want it.
5. **Feature work branches off `master` and merges back** — `git merge` / cherry-pick only, **never hand-copied**.
6. **Never merge into `master` without review.** Release-quality bar: no debug logging, no dead experiments, no ungated perf hazards.
7. **Save-format lockstep:** changes to bincode-serialized shapes / enum variant order / `.fl4` format must land identically on every live branch in the same session.

---

## Recreate a worktree if missing

```bat
cd /d F:\PROJECT\FourthLAyerAi
git worktree add .claude\worktrees\<dir> <branch>
```

---

## Stale / removed — do not recreate casually

**Removed 2026-08-10** (all were 0 commits ahead of `master` with clean trees, so nothing was lost):
`grok-2`, `grok-3`, `grok-4`, `ui-floor-fix`, `poly-params` (`feat/poly-parametric-prims`),
`viewport-perf`, `verify-d4`, and the `F:\tmp\integrate` landing dry-run
(`integ/landing-dryrun` — every one of its 14 commits is on `master` by content; it was a REBASE
of the same work, so merging it would have re-conflicted each patch against its own twin).
Branches kept; only the checkouts are gone. Rescued first: `dev/manifold-prebuilt/` (23 MB of
Manifold `.lib`s that cannot be rebuilt without cmake) and
`dev/rescued/POLY_MODIFIER_INTEGRATION_PLAN.grok-4.md` (grok-4's copy diverged from the live one).

These were registered worktrees once; their directories and launchers are **gone** (launchers deleted 2026-08-08):

`poly-foundation`, `sculpt-tool`, `cache-perf`, `assembly-viewport`, `ui`, `lab`, `sdf-tools`,
`sculpt-feel-perf` (`Dev_SculptFeel.bat`), `procedural-tex` (`Dev_ProceduralTex.bat`),
`ui-polish-grok` (`Dev_UI_Grok.bat`), `nurbs-modeling` (`OPEN_NURBS.bat` — merged into `master`
2026-08-02; only an empty `ai_backend` leftover remains on disk),
`material-render` (merged into `master` 2026-08-02).

Archived copies of the old per-worktree launchers live under `dev/pathtrace/`; the dead ones were
deleted 2026-08-08.

Agent scratch folders under `.claude/worktrees/agent-*` and `wf_*` are **not** product worktrees.

---

## CONSOLIDATION 2026-08-02

**master = release trunk** (repo ROOT) — consolidated from `feat/ui-polish` + `feat/material-render` +
`feat/nurbs-modeling` + old master's audit work (merges 28ec343 / 35e2e94 / ce3236d; tests green:
core_math 768/0, graphics 70/0/1).

Materials/render reference kept from the retired `feat/material-render` entry:
`docs/planning/render/PLAN_FOR_GROK_MATERIALS.md` (M0–M6) and
`docs/planning/render/MATERIAL_RENDER_ARCHITECTURE.md` (rules M1–M3).
Hard rules unchanged: `shader_nodes/node_type.rs` serde enum is APPEND-ONLY; `AZRA_VISDIFF` green at
every gate; CPU bake = oracle, GPU bake = product.
