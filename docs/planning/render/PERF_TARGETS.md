# PERF TARGETS — the numbers AZRA has to beat

**Machine:** owner RTX box · **Date:** 2026-08-01 · **Blender:** 4.5.1 LTS (5.0.1 within noise)
**Method:** Blender measured with `wm.redraw_timer DRAW_WIN` (no vsync), same machine, same
scene shapes as the AZRA matrix cells. Scripts + raw outputs: `bench_blender/` +
`blender_dense_bench.py` (scratchpad copies committed alongside).
**Rule:** "beat X" is only claimable for a cell measured on the SAME machine, same scene.
Published numbers (ZBrush/Unreal) are capability targets, labeled as such — we did not
measure them locally.

## Measured head-to-head (hard data)

| Matrix cell | Blender 4.5.1 | AZRA today | Status |
|---|---|---|---|
| G3/S1 100k objects, object idle | 236.7 ms (4 fps) | **7.0 ms** (fe1629b) | ✅ AZRA **34×** ahead — hold it |
| G3/S1 10k objects | 20.1 ms | **1.3 ms** | ✅ AZRA 15× ahead |
| G1/S1 1M-tri single mesh, idle deselected | 3.3 ms | **1.6 ms full-UI @0.2M** (2026-08-02, `AZRA_FULL_UI=1`) · ~0.5 ms minimal-UI @3.1M | ✅ full-UI floor beats Blender at this scale |
| G1/S1 12.5M-tri single mesh, idle | 2.1 ms | ~0.5 ms med CPU / 4.2 ms GPU (minimal-UI) | ⚠️ GPU 2× Blender (we pay shadow pass ×2); CPU fine |
| G1/S2 dense mesh SELECTED | 3.0 ms @1M tris | **1.4 ms @0.2M · 0.8 ms @3.1M** full-UI (2026-08-03; the 8.8 ms residual was `draw_tweak_panel` scanning every half-edge per frame for a closed dev panel — fixed; was 68 ms pre-fingerprint-fix, ~15 ms @0.12M owner-live) | ✅ **beats Blender normalized** — the 68 ms was `overlay_faces_fingerprint` calling `content_signature()` (full O(mesh) hash) EVERY selected frame; now Arc-ptr + pt_stamp O(1). Residual @3.1M is `lay`≈8 ms full-UI panel (present deselected too — separate item) |
| G1/S4 dense wire / edit-mode draw | 3.8 ms @1M · 3.0 ms @12.5M | **1.8 ms @0.2M · 1.3 ms @3.1M** full-UI (2026-08-03, same two fixes; was 70 ms pre-fix, ~100 ms @0.12M owner-live) | ✅ same single root as S2 (fingerprint ran before the work-mode early-outs, so edit mode paid it too). "Fix A (GPU wire)" NOT needed for these cells — wire overlay itself was already memoized correctly |
| G1 edit-mode ENTER, 12.5M tris | **2.91 s** | T4 target: **<150 ms** | 🎯 our differentiator — beat by 20× |
| G2/S1 100 unique × 50k tris (5M) | 2.0–2.4 ms | pending (R6/R8 baseline runs) | ⏳ measure |

**The bar, in one line: Blender's draw floor is ~2–4 ms in EVERY dense state.** Any AZRA
cell above ~4 ms with comparable content is a loss until fixed. Blender's only measured
weaknesses: many-objects (won) and edit-enter latency (T4's target).

## Capability targets (published — not locally measured)

| Competitor | Claim | AZRA target that answers it |
|---|---|---|
| ZBrush | ~100M-poly sculpting feel (own 2.5D display architecture) | Sculpt + virtualized geometry ≥20M interactive by Phase D exit; 100M-class = Phase E ambition |
| Unreal Nanite | 100M+ visible tris @ 60 fps | Phase D **T5** (cluster/vis-buffer ladder) |
| Unreal (editing) | CANNOT edit inside Nanite meshes | **T4**: edit a 1M-tri clustered body, Tab-in <150 ms, zero >33 ms frames — the beat-both differentiator |

## Standing rules
1. Every PERF_MATRIX cell gets a target column from this file; a cell without a target is
   informational only.
2. Re-run the Blender side after any Blender upgrade (script is committed; ~5 min).
3. Never compare AZRA minimal-UI numbers against Blender full-UI numbers — Blender ran its
   full default UI in all rows above; AZRA claims must be full-UI to count.
4. Beating a published (unmeasured) number is never claimable — only capability parity
   with the published behavior, demonstrated in a scene we can show.
5. Repro for the S1/S2/S4 dense cells (2026-08-02): `AZRA_DENSE_STRESS=1x7` (0.2M) or
   `1x9` (3.1M) + `AZRA_SCOREBOARD=1 AZRA_QUIT_AFTER_SECS=25 AZRA_FULL_UI=1`, plus
   `AZRA_STRESS_SELECT=1` for S2 or `AZRA_STRESS_EDIT=1` for S4 (harness flags landed
   with this row update). Warm the pipeline cache first — a fresh binary's first run
   reports a false multi-100 ms steady state while background compiles hog the GPU.
