# Stress test report for Claude — complex SDF + materials (2026-07-29)

**Branch:** `feat/ui-polish`  
**Commits:** `1af1c54` (stress soak steps), prior perf findings `f7d1d13` / `PERF_FINDINGS.md`  
**Run dir:** `core_engine/soak_out_stress/` (`soak.log` + `console.txt`)  
**Machine:** NVIDIA RTX 4090, Vulkan, debug `app`  
**Harness:** AZRA_SOAK 51 steps, `AZRA_PERF_SPAM=1`, `AZRA_BAKE_PROF=1`, `AZRA_UI=v2`

```
[SOAK] complete — OK=35 NO-OP=11 SLOW=5 FAIL=0 UNWIRED=0
```

No harness FAIL. Several **product** perf cliffs and one harness gap (perf counters cleared before snapshot).

---

## Scene under test (what stress built)

| Phase | What |
|-------|------|
| Baseline | box + sphere subtract, blend, mods depth 1–8, materials/projections, poly, lights, shade |
| **stress.sdf_complex** | **+12 prims → 14 total**: boxes, 4 cylinders, 2 spheres (soft subtract), torus, capsule, cone (subtract), tower box; mixed Union/Subtract with `k` blends |
| **stress.sdf_move_burst** | Nudged ~7 prims + rebake |
| **stress.mat_noise_stack** | New mat `soak_stress_mat`: BaseColor **5 layers** — Constant clay + **Vertex curvature (auto r)** + **FBM noise Mult** + **Cellular Overlay** + **Cavity Mult** |
| **stress.mat_multi_channel** | Roughness (const + Perlin) + Metallic (Ridged noise) |
| **stress.mat_noise_scale_sweep** | BaseColor noise scales ×2.5 |
| **stress.modstack_depth12** | 12 mesh modifiers fold |
| **save_load.roundtrip** | Binary save/load of **14-prim** scene |

Material noise/generators **did produce real pixel deltas** (px 7–14%) — stacks are live, not vacuous.

---

## Critical performance findings (send these to Claude)

### 1. ★★★ POST-LOAD BAKE STALL ≈ **3 SECONDS** (TDR risk)

After complex save/load with 14 prims:

```
[BakeReadback] body=NodeId(1) grid=2097152  total=2958.9ms   // 128³
[SLOWPOLL] total=2962ms | primary=2962
[STALL] frame=2979ms | unaccounted=2963.2
[GPU] WARNING: Previous frame 2979ms — deferring bake dispatch to avoid TDR
```

Also on the cascade path:

```
[BakeReadback] … 64³ total=385.8ms
[BakeReadback] … 128³ total=2958.9ms
```

**Implication:** loading a moderately complex SDF body blocks the main thread for multi-second bake readback processing. App already has TDR mitigation (Z-slice split + defer after long frames) but **UX is still frozen**.

**Where to look:**
- `app_poll_bake_poll.rs` / `poll_primary_bake`
- `body_volume_cache/bake_dispatch.rs` cascade 64→128 after load
- Field-direct / FAST path vs SLOW 64MB copy (`AZRA_BAKE_PROF` showed 7/10 SLOW mid-session)
- Make post-load settle fully async; never `Maintain`/process multi-GB work on the frame that just loaded

**Target:** post-load first visible frame &lt; 50 ms; 128³ settle in background.

---

### 2. ★★★ IDLE OVERLAY TAX ≈ **14 ms every frame**

Steady-state (idle and under load):

```
[SPIKE] update(): 16.x ms (… dispatch=0.7 overlays=14.3 …)
```

| Metric (stress run) | Value |
|---------------------|------:|
| SPIKE update samples | 1444 |
| Frame spike avg / **max** | 20.0 / **2979** ms |
| Overlays avg / max | **14.3** / 75.7 ms |

**Implication:** UI/overlay CPU alone caps ~60 FPS even when GPU dispatch &lt;1 ms.

**Where:** `app_update` → overlays bucket (viewport overlays + ui2). Skip rebuild when dirty flags empty; profile with Perf HUD off.

---

### 3. ★★ MODIFIER STACK FOLD — **152 ms at depth 12**

| Depth | Sync fold ms |
|------:|-------------:|
| 1 | 3.7 |
| 2 | 11.3 |
| 4 | 21.1 |
| 8 | 39.5 |
| **12** | **152.5** |

Non-linear jump 8→12. Prefix caching (planned, not landed) is the intended fix.

**Where:** `sync_body_mesh_mod_display` / mesh_presentation fold.

---

### 4. ★★ SAVE/LOAD SYNC **303 ms** on 14-prim scene

```
[SOAK] 049 save_load.roundtrip  302.8ms  prims=14 … SLOW (counts match)
```

Counts match (good — no wire FAIL). Cost is high for “save then reload” as one blocking call (includes serialize + load apply; then async bake storm follows — see #1).

**Where:** `ProjectFile::from_app`, `save_binary`, `load_scene_from_path` / `apply_to_app` (load alone was ~12 ms earlier; 303 ms is broader).

---

### 5. ★ VERTEX CURVATURE still dispatches **2,048,384 verts**

```
[GpuCurvature] vertex rung: body=NodeId(1) 2048384 verts — dispatch submitted
```

Even on 14-prim complex body. Capacity ≠ live surface. Phase-3 proved buffer varies; still massive over-dispatch.

**Where:** `poll_vertex_curvature` + `encode_curvature_vertex_dispatch` use `vert_cap`.

---

### 6. ★ BAKE CASCADE always 64³ then 128³ + TDR slice

```
[VolumeUpload] full (res=64³)
[VolumeUpload] full (res=128³)
[PrimaryBake] Split [128,128,128] into 2 Z-slices to avoid TDR
```

Mid-session many `SKIPPED — direct-to-texture bake` (good FAST path). After load / stress rebuild, full cascade returns with multi-second stalls.

---

### 7. Sync API vs settle cost (important for interpreting soak)

| Step | Sync `ms` | Real cost lives in |
|------|----------:|--------------------|
| stress.sdf_complex | **0.8** | settle-frame bake (enqueue only) |
| stress.sdf_move_burst | **0.2** | same |
| stress.mat_* | **0.4–0.8** | graph lower + GPU bake/curvature |
| stress.modstack_depth12 | **152** | **sync fold** (real) |
| save_load | **303** | **sync** IO + apply |

Do not treat 0.8 ms complex SDF as “fast” — watch `[BakeReadback]` / `[STALL]` in settle.

---

## Bugs / product issues (not just slow)

| ID | Severity | Finding |
|----|----------|---------|
| B1 | High | **Post-load multi-second main-thread bake** (frame 2979 ms + TDR defer) |
| B2 | High | **Idle 14 ms overlays** every frame |
| B3 | Med | Modstack fold **152 ms @ depth 12** (feels broken in UI) |
| B4 | Med | Curvature **2M vert** dispatch (VRAM + GPU time) |
| B5 | Low | Soak `stress.perf_snapshot` reads **zeros** — `perf` counters reset before soak_tick samples them (harness bug; use SPIKE lines instead) |
| B6 | Known | Vertex curvature **px may still not show** κ (phase-3: kernel ok, downstream); stress noise stacks **do** show px — noise path works |
| B7 | Known | poly.boolean often early-out; many catalog tools still `tw` stubs |

---

## Material stack evidence (worked)

```
043 stress.mat_noise_stack     0.6ms  px=14.3%  OK  layers=5 (const+curv+fbm+cell+cavity)
044 stress.mat_multi_channel   0.7ms  px=12.4%  OK  rough+metal noise
045 stress.mat_noise_scale_sweep 0.8ms px=7.8%  OK  scale*2.5 on 2 noise layers
```

So complex **noise/generator panel stacks** lower and change the viewport. Curvature-only still needs the phase-3 downstream fix separately.

---

## Suggested fix order for Claude

1. **Never block `update()` on full 128³ BakeReadback after load** — async settle, progressive 64³ display, no 3 s STALL  
2. **Idle overlay skip/cache** — reclaim ~14 ms/frame  
3. **Modstack prefix cache** — depth 8–12  
4. **Curvature live-vert dispatch** (or tighter cap from content)  
5. **Profile save path** when prim count ≥ 10  

---

## How to reproduce

```powershell
cd core_engine
$env:AZRA_SOAK = "$PWD\soak_out_stress"
$env:AZRA_UI = "v2"
$env:AZRA_PERF_SPAM = "1"
$env:AZRA_BAKE_PROF = "1"
.\target\debug\app.exe 2>&1 | Tee-Object soak_out_stress\console.txt
```

Grep console for: `STALL`, `BakeReadback`, `SLOWPOLL`, `TDR`, `GpuCurvature`, `SOAK] 04`.  
Grep log for: `stress.`, `SLOW`, `complete`.

---

## Related docs

- `core_engine/PERF_FINDINGS.md` — earlier idle/overlay + bake profile  
- `docs/planning/LIVE_MOVE_PERF.md` — partial bake / escape full  
- Phase-3 curvature (a)/(b)/(c): kernel OK, loss downstream of buffer  
