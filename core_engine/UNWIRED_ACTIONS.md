# UNWIRED_ACTIONS — AZRA_SOAK

Actions requested for soak that had **no production API** at the time of the run
(logged as `UNWIRED:<action>` and continued). Empty sections mean every requested
step had a callable path.

## Phase 3 run (2026-07-29) — `soak_out_p3` / commit `4b87e7e`

```
OK=27 NO-OP=11 SLOW=5 FAIL=0 UNWIRED=0
```

### STEP 1 — curvature (a)/(b)/(c) readback — THE finding

Probe after `tex.curv_base_color` (hairline r=0.02) and `tex.curv_auto_radius` (r=0.1424):

| field | hairline (015) | auto (016) |
|-------|----------------|------------|
| **diag** | **ok_varies** | **ok_varies** |
| base_vertex | 0 | 0 |
| cap | 2048384 | 2048384 |
| arrayLen (f32) | 8193536 | 8193536 |
| maxVsIdx | 8193535 | 8193535 |
| sample | 512 verts at base | 512 |
| ch1 signed [min,max] | [0.000, **0.986**] | [0.000, **0.939**] |
| ch1 signed mean | **0.06884** | **0.27004** |
| ch0 mask mean | 0.070 | 0.363 |
| px verdict | 2.1% (albedo) | **0.0% NO-OP** |
| flag | armed, blend=1 (Multiply) | armed, blend=1 |

**Verdict on the three candidates:**

| candidate | result |
|-----------|--------|
| **(a)** kernel writes ~constant | **RULED OUT** — signed varies [0, ~0.98]; radius change moves mean 0.069 → 0.270 |
| **(b)** base_vertex index mismatch | **RULED OUT** — `base=0` (unpooled / zero vert_off); no prefix to mismatch |
| **(c)** arrayLength guard always fails | **RULED OUT** — `maxVsIdx = arrayLen-1` exactly; full capacity is addressable |

**So the loss is NOT in the kernel write path.** Dispatch submits, buffer holds
radius-sensitive non-constant data, flag is armed with Multiply — yet auto-radius
produces **zero pixel delta**. The failure is **downstream of the storage write**
(VS bind/sample, FS composite not applied, Hybrid early-exit/lighting path
ignoring raster albedo, or drawn verts not the sampled domain). **No speculative
product fix landed** — diagnosis only.

Dispatch width remains `vert_cap` (~2.0M), not live count (GPU-only contract). Sample
is first 512 slots of that padded domain.

### STEP 2 — weak steps re-checked

| step | phase-2 | phase-3 | note |
|------|---------|---------|------|
| `sdf.blend_k` | px=0% (k=0.12) | **px=0.4% OK** (k=0.55) | weak value, not a product bug |
| `sdf.undo_blend_k` | 0% | **0.4% OK** | |
| poly create/extrude | ~0% | **39% / 3.6%** after `frame_selected` | was camera framing |
| poly bevel/delete/dissolve/merge | 0% | still often 0% | vcap moves; small local ops |
| poly.loop_cut | 0% | **0.3% OK** | |
| poly.boolean | early-out | still early-out (v=21, cutter kept) | plan/kernel |
| light.point | ok | **100%** (env off + dark albedo) | |
| light.spot | — | 0% | may not dominate over point |
| light.sun | 0% | **0.1% OK** | |
| light.point_radius | 0% | **0.2% OK** (range 3→12) | |
| light.rt_toggle | 0% | still 0% | RT flag may be visual no-op in Hybrid |

### STEP 3 — SLOW (informational)

| step | ms | triage |
|------|-----|--------|
| modstack.depth4 | 27.6 | real per-edit (prefix cache not yet built) |
| modstack.depth8 | 52.2 | real per-edit |
| tex.bind_image_map | 29.9 | one-shot user action |
| save_load.roundtrip | 73.8 | one-shot; counts match |

### Harness changes (phase 3)

- `curv.radius_sweep` folded into `tex.curv_auto_radius`
- `curv.readback_abc` step + `[SOAK] curv_readback` lines
- `COPY_SRC` on vertex/mesh curv out buffers for diagnostic readback
- Multiply default on soak generator layer (match f6e13ae)
- blend_k / poly frame / light scene setup strengthened

## Phase 2 notes (historical)

See git history / `soak_out_p2final`. Device-loss fixed by 88502a8; vcap not frozen at 15608.
No UNWIRED production APIs remain for the phase-2/3 step list.
