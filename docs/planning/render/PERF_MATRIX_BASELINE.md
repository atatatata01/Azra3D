# PERF MATRIX BASELINE — engine-cost floor (fe1629b)

| Field | Value |
|---|---|
| **Date** | 2026-08-01 |
| **Worktree** | `F:\PROJECT\FourthLAyerAi\.claude\worktrees\perfbase` (detached) |
| **Commit** | `fe1629b` — `docs(D1): re-measure 100k scoreboard after key-driven instance upload` |
| **Binary** | `core_engine\target\release\app.exe` (cold `cargo build -p app --release`) |
| **GPU** | NVIDIA GeForce RTX 4090 · Vulkan · NVIDIA driver |
| **Method** | `AZRA_SCOREBOARD=1` + `AZRA_QUIT_AFTER_SECS=30` · `Start-Process` + `RedirectStandardError` · exe direct (not `cargo run`) |
| **Agent role** | **Measurement only** — no engine/code fixes. ui-polish worktree not touched. Master root (`fc75e0e` op_bench sandbox) not cited. |
| **Raw logs** | `docs/planning/render/perf_matrix_logs/*.stderr.log` |

## Known limitation (STATE THIS)

These stress hooks force **MINIMAL UI** (shell skipped so `lay` does not mask GPU/CPU).  
**This table is the engine-cost floor only.**

Full-UI + selected/edit-mode cells come from the owner’s live sessions and the future `AZRA_PERF_MATRIX` harness:

| Owner live session (2026-08-01) | Approx frame |
|---|---|
| Wire / full UI | ~80 ms |
| Selected / edit-mode | ~15 ms |

Do **not** treat this matrix as “shipping viewport FPS.”

## How numbers were reduced

- Scoreboard emits every ~2 s; each run yields ~14–15 samples over 30 s after first frame.
- **Idle stats:** drop first 3 samples (cold GPU/UI init), then drop any sample with `frame > 50 ms` (startup mesh ensure, bake, `poll_background_tasks` SPIKE pollution of the single-sample `frame=` field).
- **p50 / p95** = percentiles of remaining idle `frame=` samples.
- **med** = engine ring-buffer median (`med=` on last idle sample) — preferred single “steady” number.
- **Top 3 CPU sections** = median of scoreboard `in/mesh/disp/sub/ov/lay/dty` over idle samples.
- **gpu_ms breakdown** = median of `gpu_ms / gpuSh / gpuGb / gpuDf / gpuTm` (GPU timestamps; `gpuTs=ok` on all successful runs).
- **FLAG** = idle **p95 > 16 ms** (60 fps budget). Those cells are the fix queue.

---

## Matrix table

| Scenario | Env | Tris (harness) | Bodies | p50 / p95 frame (ms) | med (ms) | Top 3 CPU sections (ms) | gpu_ms (Sh / Gb / Df / Tm) | draws | share | Notable [STALL]/[SPIKE] | FLAG |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **R1** | `AZRA_INSTANCE_STRESS=10000` | shared low-poly box (segs=16) × 10k | 10 000 | **1.54 / 1.81** | **1.53** | disp=1.3, ov=0.1, lay=0.0 | **0.20** (0.06 / 0.08 / — / 0.02) | **1** | 1 | STALL 596ms; SPIKE poll_bg 224ms; STALL 229ms (startup/bg) | |
| **R2** | `AZRA_INSTANCE_STRESS=100000` (T1) | shared low-poly × 100k | 100 000 | **14.59 / 15.89** | **15.05** | disp=11.9, mesh=1.2, ov=0.7 | **1.38** (0.57 / 0.72 / 0.06 / 0.02) | **1** | 1 | STALL 518 / 321ms; SPIKE poll_bg 57ms. `occ≈3.0–3.2` (Hi-Z CPU). instUp=0. | **near 16** |
| **R3** | `AZRA_DENSE_STRESS=1x9` | **~3.15 M** | 1 | **0.56 / 0.82** | **0.44** | disp=0.4, ov=0.1, lay=0.0 | **0.94** (0.44 / 0.45 / 0.02 / 0.02) | 1 | 0 | ensure_poly / poll_bg spikes during first upload (STALL 585ms class) | |
| **R4** | `AZRA_DENSE_STRESS=1x10` | **~12.6 M** | 1 | **0.65 / 0.83** | **0.73** | disp=0.5, ov=0.1, lay=0.0 | **4.05** (1.74 / 2.25 / 0.02 / 0.02) | 1 | 0 | ensure_poly 234ms; upload_pending 129ms; STALL 781ms (startup) | |
| **R5** | `AZRA_DENSE_STRESS=4x9` | **~12.6 M** (4 unique) | 4 | **0.78 / 0.83** | **0.67** | disp=0.6, ov=0.1, lay=0.0 | **4.16** (2.19 / 1.74 / 0.02 / 0.02) | **4** | 0 | ensure_poly 239/178ms; STALL 935 / 267ms (startup) | |
| **R6a** | `AZRA_DENSE_STRESS=8x9` | **~25.2 M** (8 unique) | 8 | **0.79 / 1.73** | **0.86** | disp=0.6, ov=0.1, lay=0.0 | **7.34** (3.39 / 3.68 / 0.02 / 0.02) | **8** | 0 | Heavy startup: ensure_poly up to **680ms**; STALL **2834 / 938 / 1970ms**; poll_bg up to 1.4s. Steady idle still sub-ms frame. | |
| **R6b** | `AZRA_DENSE_STRESS=16x8` | **~12.6 M** (16 unique) | 16 | **0.79 / 1.18** | **0.74** | disp=0.6, ov=0.1, lay=0.0 | **4.97** (2.41 / 2.36 / 0.02 / 0.02) | **16** | 0 | ensure_poly ~220–246ms; STALL 936 / 281ms (startup). First attempt aborted mid-GPU-init (5s, no scoreboard); **retry OK**. | |
| **R7** | `AZRA_MULTI_MESH_STRESS=5x2000` | 5 unique × 2000 inst | 10 000 | **1.70 / 2.18** | **1.52** | disp=1.1, mesh=0.3, ov=0.1 | **0.24** (0.06 / 0.10 / 0.04 / 0.02) | **257**† | 5 | C.6 pure gate sorted_draws=**5**; runtime draws≈257 (frustum/Hi-Z split). STALL 449ms class. | |
| **R8a** | `AZRA_MULTI_MESH_STRESS=20x500` | 20 unique × 500 | 10 000 | **1.71 / 1.92** | **1.73** | disp=1.2, mesh=0.3, ov=0.1 | **0.24** (0.06 / 0.10 / 0.04 / 0.02) | **334**† | 20 | C.6 sorted_draws=20; runtime draws≈334. STALL 437ms. | |
| **R8b** | `AZRA_MULTI_MESH_STRESS=50x200` | 50 unique × 200 | 10 000 | **2.03 / 2.38** | **2.04** | disp=1.5, mesh=0.3, ov=0.1 | **0.49** (0.13 / 0.29 / 0.04 / 0.02) | **57**† | 50 | C.6 sorted_draws=50; runtime draws≈57. STALL 427 / 548ms; SPIKE poll_bg 542ms. | |
| **R9** | R2 + `AZRA_NO_HIZ=1` | shared × 100k | 100 000 | **7.41 / 9.06** | **7.31** | disp=5.2, mesh=0.9, ov=0.5 | **1.43** (0.50 / 0.63 / 0.04 / 0.02) | **1** | 1 | `occ=0` (Hi-Z off). STALL 531 / 220ms. | |
| **R10** | R4 + `AZRA_NO_SHADOWS=1` | **~12.6 M** | 1 | **0.37 / 0.56** | **0.36** | disp=0.3, ov=0.1, lay=0.0 | **2.15** (0.00 / 2.10 / 0.02 / 0.02) | 1 | 0 | gpuSh=**0** confirms shadow path off. Startup ensure_poly/upload spikes same class as R4. | |

† Multi-mesh **C.6 pure gate** reports `sorted_draws=N` at spawn; scoreboard `draws=` at idle is higher when frustum/Hi-Z compact or split instance lists. Contract under test remains “share=N unique cids, not 1.”

---

## FLAG / fix queue (idle p95 > 16 ms)

| Scenario | Idle p95 | Verdict |
|---|---|---|
| **(none)** | — | **No cell exceeds 16 ms idle p95 on this machine/build.** |

### Watch list (not flagged, but closest to budget)

| Scenario | med | idle p95 | Notes |
|---|---|---|---|
| **R2** 100k + default Hi-Z | **15.05** | **15.89** | **~0.1 ms under 16.** CPU-bound: `disp≈12`, `occ≈3.2`, `mesh≈1.2`. GPU only ~1.4 ms. T1 target “≤16” is **met but tight**. |
| **R9** 100k + `AZRA_NO_HIZ=1` | **7.31** | **9.06** | **~2× faster** than R2 → Hi-Z CPU path is the dominant 100k tax on this baseline, not GPU fill. |

### Startup / SPIKE debt (not idle-frame flags, but real pain)

All dense and large-instance runs show multi-100ms–multi-second `[STALL]` / `[SPIKE]` during first-seconds mesh ensure, upload, and `poll_background_tasks`. Worst:

| Scenario | Peak notable |
|---|---|
| R6a 8×9 | STALL **2834 ms**; ensure_poly **680 ms**; poll_bg **1.4 s** |
| R4 / R5 / R6b | STALL 780–940 ms class during ensure_poly |
| All runs | late `poll_background_tasks` SPIKE 50–500+ ms can still land mid-session |

These are **load spikes**, not steady idle. Still part of “feels slow on open.”

---

## A/B findings (hooks only)

### Hi-Z (R2 vs R9) — 100k shared instances

| | R2 Hi-Z ON (default) | R9 `AZRA_NO_HIZ=1` | Δ |
|---|---|---|---|
| med | 15.05 ms | **7.31 ms** | **−7.7 ms (−51%)** |
| idle p95 | 15.89 | 9.06 | −6.8 ms |
| disp (med) | 11.9 | 5.2 | −6.7 |
| occ (scoreboard) | **~3.1–3.2** | **0** | Hi-Z CPU test cost |
| gpu_ms | 1.38 | 1.43 | ~flat |
| draws / share / instUp | 1 / 1 / 0 | 1 / 1 / 0 | same |

**Conclusion:** On idle 100k, **default Hi-Z is a net loss** (~8 ms CPU) with no GPU win at this camera. Fix-queue candidate for the *other* agent: reduce Hi-Z CPU cost or gate it when cull yield is low — **not done here**.

### Shadows (R4 vs R10) — 1×10 dense (~12.6 M tris)

| | R4 shadows ON | R10 `AZRA_NO_SHADOWS=1` | Δ |
|---|---|---|---|
| med | 0.73 ms | **0.36 ms** | −0.37 ms |
| gpu_ms | 4.05 | **2.15** | **−1.9 ms** |
| gpuSh | 1.74 | **0.00** | shadow pass removed |
| gpuGb | 2.25 | 2.10 | ~flat gbuffer |

**Conclusion:** Shadows are ~half of dense GPU time at 12.6 M tris, but absolute frame is still ≪16 ms (CPU idle ~0.5 ms). Shadow cost is real; not a 60 fps flag at this minimal-UI floor.

### Dense scaling bend (R3 → R6)

| | Bodies × CC | Tris | med | gpu_ms | draws |
|---|---|---|---|---|---|
| R3 | 1×9 | 3.1 M | 0.44 | 0.94 | 1 |
| R4 | 1×10 | 12.6 M | 0.73 | 4.05 | 1 |
| R5 | 4×9 | 12.6 M | 0.67 | 4.16 | 4 |
| R6b | 16×8 | 12.6 M | 0.74 | 4.97 | 16 |
| R6a | 8×9 | **25.2 M** | 0.86 | **7.34** | 8 |

- **GPU scales with tris** (0.9 → 4 → 7 ms) but absolute stays single-digit.
- **Same tri inventory, more unique bodies** (R4 vs R5 vs R6b): gpu_ms moves little (4.0–5.0); draws = body count.
- **Bend is not a 16 ms wall** at engine floor — still not “multi-10 ms GPU tri-bound” for Phase D entry (matches prior Phase D entry notes).
- **Where it hurts:** **startup** at 8×9 (R6a STALL multi-second), not steady idle.

### Multi-mesh unique-count (R7 / R8)

| | unique × inst | med | draws (runtime) | C.6 sorted |
|---|---|---|---|---|
| R7 | 5×2000 | 1.52 | 257 | 5 |
| R8a | 20×500 | 1.73 | 334 | 20 |
| R8b | 50×200 | 2.04 | 57 | 50 |

Steady cost rises slowly with unique mesh count; still ≪16 ms. Runtime `draws` ≠ pure-gate `sorted_draws` (frustum / Hi-Z / list packing).

---

## Coordination notes

- **No `sb_*` / prior baseline matrix files** were present in perfbase when measurement started (other agent’s `sb_*` live under **ui-polish** — left untouched).
- Master root `fc75e0e` (op_bench sandbox) was **not** used for this table.
- First R6b launch died mid-GPU init (~5 s, truncated stderr); **retry succeeded** — log on disk is the successful run.

---

## Fix queue handoff (report only — do not implement here)

1. **R2 / Hi-Z CPU at 100k** — largest *steady* engine-floor cost; R9 proves ~8 ms reclaim if Hi-Z is free or smarter.  
2. **Startup ensure_poly / upload / poll_bg SPIKE** — multi-second stalls on dense multi-body open (esp. R6a).  
3. **Full-UI / selection / wire** — **not measured here**; owner ~80 ms wire / ~15 ms selected remain the real product flags once engine floor is this low.  
4. **No idle p95 > 16 ms** cell in this minimal-UI matrix — product fix queue is above the floor.

---

## Reproduce

```powershell
cd F:\PROJECT\FourthLAyerAi\.claude\worktrees\perfbase\core_engine
# one-time: cargo build -p app --release

$exe = ".\target\release\app.exe"
$env:AZRA_SCOREBOARD = "1"
$env:AZRA_QUIT_AFTER_SECS = "30"
# then ONE of:
$env:AZRA_INSTANCE_STRESS = "100000"          # R2
# $env:AZRA_DENSE_STRESS = "1x10"             # R4
# $env:AZRA_MULTI_MESH_STRESS = "5x2000"      # R7
# $env:AZRA_NO_HIZ = "1"                      # R9 add-on
# $env:AZRA_NO_SHADOWS = "1"                  # R10 add-on

$err = "..\docs\planning\render\perf_matrix_logs\manual.stderr.log"
Start-Process -FilePath $exe -WorkingDirectory (Get-Location) `
  -RedirectStandardError $err -PassThru | Wait-Process
```

Parse: all lines matching `\[SCOREBOARD\]`, `\[STALL\]`, `\[SPIKE\]` in the stderr log.
