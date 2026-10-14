"""Sculpt falloff oracle — dump Blender's brush falloff presets as w(e) LUTs.

Ground truth for AZRA sculpt-feel Phase F1 (falloff parity). Blender is the oracle.

Convention: w(e), e = 1 - dist/radius  (e=1 centre, e=0 rim). This matches BOTH
AZRA's `falloff_weight(e, hardness)` argument AND Blender's internal `p` in
`BKE_brush_curve_strength` (blenkernel/intern/brush.cc: `p = 1 - distance/radius`),
so no remapping is needed to compare.

For each falloff preset we emit TWO curves, cross-checked:
  * curvemap_w : Blender's live CurveMapping evaluated at x = 1 - e  (what the UI shows,
                 and what BKE uses at runtime for the CUSTOM preset).
  * analytic_w : the exact per-preset formula transcribed from BKE_brush_curve_strength
                 (what BKE uses at runtime for the non-CUSTOM presets). e == BKE's `p`.

Run:
  blender.exe --background --factory-startup --python falloff_ref.py -- <out.json>
"""
import bpy, sys, json, math

argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "falloff_presets_ref.json"

N = 65  # samples across e in [0,1] inclusive (65 => step 1/64, clean binary fractions)
es = [i / (N - 1) for i in range(N)]

# Analytic presets, transcribed verbatim from Blender source
# source/blender/blenkernel/intern/brush.cc :: BKE_brush_curve_strength()
# with e == p == (1 - distance/radius).  These are the runtime formulas for
# every non-CUSTOM preset.
def analytic(name, e):
    if name == "SHARP":      return e * e
    if name == "SMOOTH":     return 3.0 * e * e - 2.0 * e * e * e          # smoothstep — Blender sculpt DEFAULT
    if name == "SMOOTHER":   return e * e * e * (e * (e * 6.0 - 15.0) + 10.0)  # smootherstep (quintic)
    if name == "ROOT":       return math.sqrt(e)
    if name == "LIN":        return e
    if name == "CONSTANT":   return 1.0
    if name == "SPHERE":     return math.sqrt(max(0.0, 2.0 * e - e * e))
    if name == "POW4":       return e * e * e * e                          # UI label "Sharper"
    if name == "INVSQUARE":  return e * (2.0 - e)
    return None

# Make a real sculpt brush so we get a genuine CurveMapping + curve_preset enum.
brush = bpy.data.brushes.new("azra_oracle", mode='SCULPT')

# Discover the actual enum identifiers this Blender build exposes (robust to renames).
preset_items = [it.identifier for it in
                brush.bl_rna.properties['curve_preset'].enum_items]

# What preset does a factory sculpt brush default to? (Confirms Blender's out-of-box feel.)
default_preset = brush.curve_preset

presets = {}
for name in preset_items:
    entry = {"analytic_w": None, "curvemap_w": None, "note": ""}
    # analytic (source formula)
    a = None
    try:
        if analytic(name, 0.5) is not None:
            a = [round(analytic(name, e), 8) for e in es]
    except Exception as ex:
        entry["note"] += f"analytic_err={ex};"
    entry["analytic_w"] = a

    # live CurveMapping evaluation
    try:
        brush.curve_preset = name
        cm = brush.curve
        cm.update()
        c = cm.curves[0]
        # brush falloff curve maps x=distance-ratio (0 centre .. 1 edge) -> strength.
        # our e = 1 - distance-ratio, so evaluate at x = 1 - e.
        # API: CurveMapping.evaluate(curveMap, position)  (the CurveMap itself has no .evaluate)
        cw = [round(float(cm.evaluate(c, 1.0 - e)), 8) for e in es]
        entry["curvemap_w"] = cw
    except Exception as ex:
        entry["note"] += f"curvemap_err={ex};"

    presets[name] = entry

out = {
    "blender_version": bpy.app.version_string,
    "source_formula": "BKE_brush_curve_strength (blenkernel/intern/brush.cc)",
    "convention": "w(e), e = 1 - dist/radius; e=1 centre, e=0 rim; e == BKE 'p'",
    "default_sculpt_curve_preset": default_preset,
    "n": N,
    "e": [round(e, 8) for e in es],
    "presets": presets,
}
with open(out_path, "w") as fh:
    json.dump(out, fh, indent=1)

# stdout summary for eyeballing / the log
print(f"[ORACLE] blender={out['blender_version']} default_preset={default_preset} "
      f"presets={list(presets.keys())}")
sm = presets.get("SMOOTH", {}).get("analytic_w")
lin = presets.get("LIN", {}).get("analytic_w")
if sm and lin:
    # sample at e=0.25,0.5,0.75 to show the SMOOTH vs LIN (== AZRA default) gap
    for e_probe in (0.25, 0.5, 0.75):
        i = round(e_probe * (N - 1))
        print(f"[ORACLE] e={es[i]:.3f}  SMOOTH={sm[i]:.4f}  LIN/AZRA-h0={lin[i]:.4f}")
print(f"[ORACLE] wrote {out_path}")
