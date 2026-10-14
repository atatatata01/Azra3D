# Performance findings — AZRA soak + console SPIKE (2026-07-29)

**Method:** same process as SOAK phase 2/3 — drive production APIs via `AZRA_SOAK`, capture
viewport shots, then enable `AZRA_PERF_SPAM=1` + `AZRA_BAKE_PROF=1` and read `[SPIKE]` /
`[STALL]` / `[BakeReadback]` / `[VolumeUpload]` from console.

**Run:** `soak_out_perf/` on RTX 4090 (Vulkan), ui2, debug build.  
**Harness:** 43 scripted steps (SDF / modstack / materials / poly / lights / shade / save).

> Note: with PERF_SPAM on, console I/O floods the frame thread and many screenshot captures
> hit `shot timeout` (px NO-OP). **SPIKE timings remain valid**; pixel verdicts from this run
> are **not** — use `soak_out_p3` for correctness. Performance numbers below come from SPIKE
> lines and step `ms` (sync call time).

---

## Top problems (priority order)

### P0 — Steady-state frame: **~14 ms spent in overlays every frame**

Even **idle** frames (no edit, no bake) report:

```
[SPIKE] update(): 15–17ms (layout=0.3 input=0.0 mesh=0.0 dispatch=0.5 overlays=13.5–14.5 dirty=0.0)
```

| Metric | Value |
|--------|------:|
| SPIKE `update()` samples | **3446** |
| Frame spike avg / max | **17.0 ms / 685.6 ms** |
| Overlay component avg / max | **13.7 ms / 54.3 ms** |

**Impact:** floor is ~60 FPS locked by CPU overlay/UI work, not GPU draw (dispatch often <1 ms).  
**Where:** `app_update` wall breakdown → `overlays` bucket (viewport overlays + ui2 chrome).  
**Opportunity:** skip/cache overlay rebuild when nothing dirty; throttle egui full-tree when idle;
PERF HUD itself may cost — measure with HUD off.

---

### P0 — Save/load (and cold body) **bake cascade stalls the UI thread**

After `save_load.roundtrip`:

```
[BakeReadback] body=… grid=262144   total=94.8ms    // 64³
[BakeReadback] body=… grid=2097152  total=667.7ms   // 128³
[SLOWPOLL] total=668ms | primary=668
[STALL] frame=686ms | unaccounted=668.8
```

Typical create/edit path also always does **two full uploads**:

```
[VolumeUpload] full (res=64³)
[VolumeUpload] full (res=128³)
```

Sometimes mid-session bakes are fast (`0.7–4.4 ms` readback — GPU field-direct). After load /
`NeverBaked`, cost explodes to **hundreds of ms on the poll path**.

**Impact:** hitch after open/save and on first bake of a body.  
**Opportunity:** keep cascade async and never block `poll_background_tasks` on full 128³ readback;
prefer already-landed field-direct path after load; see `docs/planning/LIVE_MOVE_PERF.md`.

---

### P1 — Modifier stack fold cost grows linearly (real per-edit)

| Step | Sync fold ms |
|------|-------------:|
| modstack.depth1 | 3.8 |
| modstack.depth2 | 11.3 |
| modstack.depth4 | 21.0 |
| modstack.depth8 | **38.7** |

**Impact:** every stack edit re-folds all layers. Prefix caching measured elsewhere (118→43 ms on
depth 8, commit b979035 plan) **not built yet**. Do not paper over inside soak — report only.

---

### P1 — Vertex curvature dispatches **capacity (~2.0M verts)**, not live surface

```
[GpuCurvature] vertex rung: body=NodeId(1) 2048384 verts — dispatch submitted
```

**Impact:** multi-ms GPU + huge transient buffers every radius/key change (phase-3 readback
proved values vary, but work is ~1000× live surface). Cap clamp avoids device loss; still waste.

---

### P2 — Cold pipeline compile (first session / first mode)

| Event | Time |
|-------|-----:|
| TOTAL STARTUP | 55 ms |
| All 14 background WGSL | **312 ms** |
| Pathtrace-tri | 273 ms |
| PT denoise / SSAO | ~274–279 ms each |
| SDF DC mesher (bg) | 176 ms |
| First frame STALL | **359 ms** (mostly unaccounted — cache/hot-swap) |

**Impact:** first open / first PT / first select can hitch. Pipeline disk cache helps repeat
starts but first-use modes still compile lazy.

---

### P2 — Bake profile summary (BAKEPROF)

```
[BAKEPROF] last 10 edits: 3 FAST (GPU, no copy) | 7 SLOW (64MB CPU copy, avg 1.3ms, worst 2.1ms)
```

CPU copy of large grids is still on the slow path for most edits in this session (avg ~1–2 ms
for the copy alone when SLOW; the **667 ms** stall is wait/process of a full 128³, not just
`to_vec`).

---

## What SOAK actually exercised (tool coverage)

### Covered (production APIs, 43 steps)

| Area | Steps |
|------|--------|
| SDF | add box/sphere, boolean subtract, blend_k, move/rotate body, undos |
| Modifiers | depth 1/2/4/8 fold |
| Materials | add mat, curvature base+auto radius, image map, 4 projections |
| Poly | create, extrude, bevel, loop cut, delete, dissolve, merge, boolean |
| Lights | point/spot/sun, range, RT toggle |
| Shading | matcap, pbr, pathtrace, hybrid |
| IO | undo, save/load roundtrip |

### Catalog tools **not** in soak (many still `tw` = dimmed / not ready)

From `ui2/tools/catalog.rs` — large set of tools marked not fully wired (`tw(...)`):

- **SDF:** Emboss, Twist, Taper, Shear, Displace, Smooth, Spherize, Shell, Clip Curve, Smart Mirror, Elongate, Push/Pull, many convert cmds  
- **Poly:** Knife, slides, spin, poke, most select modes, array/mirror duplicate  
- **Sculpt:** almost all brushes (only Draw is ready)  
- **NURBS:** essentially all surface/CV tools  

Honest tool palette: **most sculpt/NURBS catalog entries are UI stubs** — soak cannot “test every tool” until those call production ops. Poly/SDF ready set is much larger and is what soak + keybindings hit.

---

## Recommended improvement backlog

| Pri | Item | Expected win | Evidence |
|-----|------|--------------|----------|
| 1 | Cut idle **overlay/UI** rebuild (~14 ms) | 60→120 FPS idle headroom | SPIKE overlays every frame |
| 2 | Make **post-load bake** non-blocking / avoid 128³ hitch | Kill 300–700 ms stalls | 667 ms BakeReadback after save_load |
| 3 | **Modifier prefix cache** (depth 8) | ~2× stack edit | 38.7 ms fold depth8 |
| 4 | Curvature dispatch on **live span**, not vert_cap | GPU time + VRAM | 2.0M verts log |
| 5 | Lazy PT/SSAO only when mode entered | Faster cold Hybrid start | 270 ms+ compiles |
| 6 | Live prim-move partial bake (existing plan) | Drag feel | LIVE_MOVE_PERF.md |

---

## How to re-run

```powershell
cd core_engine
$env:AZRA_SOAK = "$PWD\soak_out_perf"
$env:AZRA_UI = "v2"
$env:AZRA_PERF_SPAM = "1"    # [SPIKE]/[PollDetail]
$env:AZRA_BAKE_PROF = "1"    # [BAKEPROF] every 10 edits
.\target\debug\app.exe 2>&1 | Tee-Object soak_out_perf\console.txt
```

Correctness px run: **omit** `AZRA_PERF_SPAM` (console flood breaks screenshots).

---

## Harness note

`AZRA_PERF_SPAM=1` is now an **env gate** (`perf_spam_on()`), so profiling does not require flipping a const and rebuilding.
