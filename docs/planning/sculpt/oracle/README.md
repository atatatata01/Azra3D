# Sculpt Blender Oracle

Ground-truth capture for the sculpt-feel effort. Blender is the oracle; we match its **behavior**
(falloff curves, displacement-vs-distance fields, spacing scalars), NOT vertex positions — see
`../SCULPT_FEEL_LOG.md` §C for why voxel sculpt can't be vertex-parity-tested against Blender.

## Convention (shared by every ref file)
`w(e)` = brush radial strength, argument `e = 1 - dist/radius`.
So **e = 1 at the brush centre, e = 0 at the rim**. This is exactly AZRA's `falloff_weight(e, …)`
argument AND Blender's internal `p` in `BKE_brush_curve_strength` (`p = 1 - distance/radius`), so
the two are directly comparable with no remapping.

## Run (headless, on this machine)
```
"/c/Program Files/Blender Foundation/Blender 4.5/blender.exe" --background --factory-startup \
  --python falloff_ref.py -- falloff_presets_ref.json
```
`--factory-startup` = reproducible (ignores user prefs). Output JSON is checked in next to the
script, like the poly `*_ref.json` files.

## Files
- `falloff_ref.py` — dumps Blender's sculpt falloff presets as `w(e)` LUTs (both the live
  CurveMapping evaluation AND the analytic source formula, cross-checked).
- `falloff_presets_ref.json` — captured reference (the ground truth our `falloff_weight` matches).

## GEOMETRIC oracle — `sculpt_ref.py` (real Blender brush strokes)
`falloff_ref.py`/`claystrips_ref.py` only dump PARAMETERS. `sculpt_ref.py` applies a real
`bpy.ops.sculpt.brush_stroke` and exports the resulting deformed geometry + a displacement
profile signature — the geometric ground truth for "matches Blender".

### CRITICAL: must run in a REAL window (NOT `--background`)
Blender 5.0.1's sculpt stroke needs a live GPU context (PBVH raycast + depth). Under
`--background` it HARD-CRASHES (`EXCEPTION_ACCESS_VIOLATION`). So `sculpt_ref.py` registers an
app-timer, runs inside a live (but immediately-quitting) Blender window, and calls
`bpy.ops.wm.quit_blender()` when done. A window flashes for ~2s per run; that is expected.

Also note Blender 5.0 renamed the brush-type property `sculpt_tool` → **`sculpt_brush_type`**,
and ships only the "Draw" brush by default — the script sets `sculpt_brush_type` on the active
brush (setting `sculpt_tool`, as pre-4.3 scripts did, silently no-ops and every brush comes out
as Draw).

```
# one brush:
"/c/Program Files/Blender Foundation/Blender 5.0/blender.exe" --factory-startup \
  --python sculpt_ref.py -- GRAB grab_ref.json
# all: loop over DRAW GRAB PINCH CLAY_STRIPS CLAY SNAKE_HOOK
```
Test mesh: 32×32 grid (Draw/Pinch/Grab/SnakeHook) or a 64×32 UV sphere (Clay-family — a flat
plane nets zero Clay deposit because the area-plane coincides with the surface). Straight +X
stroke, size 40px, strength per `PARAMS`. Output `profile`: `dz`(along stroke), `cross_dy`
(across — the Pinch squeeze signature), `dr_radial`/`dlat_radial` (footprint-radial — the
Grab/Clay signature), plus peak/max scalars.

### Diff harness — `sculpt_diff.py`
`python sculpt_diff.py <blender_ref.json> <azra_profile.json> --key dr_radial` — normalizes both
profiles to their peak and reports raw max/mean deviation (world units), shape correlation,
normalized RMS, and peak-bin offset. Voxel≠mesh means we gate on SHAPE (corr>0.9, peak Δ≤1),
not absolute vertex match. AZRA exports the same JSON shape from a `core_math` test helper.

### Captured references (checked in)
`grab_ref.json`, `pinch_ref.json`, `clay_strips_ref.json`, `clay_ref.json`,
`snake_hook_ref.json`, `draw_ref.json` — Blender 5.0.1 ground truth.

## Later phases add
- `spacing_ref.py` / `spacing_ref.json` — dab spacing + accumulate behavior.
