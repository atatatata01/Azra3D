"""Clay Strips oracle — dump Blender's sculpt brush PARAMETERS that define the clay-buildup feel.

Clay Strips is an OPERATOR, not a 1D curve, so its "ground truth" is (a) its parameter set + defaults
and (b) its algorithm (area-averaged plane + offset + converging deposit + square/cube tip). This
script captures (a) as concrete Blender data; (b) is documented from Blender's sculpt design in the
gap analysis. We dump Clay Strips alongside Clay / Draw / Flatten so the differences are explicit.

Run:
  blender.exe --background --factory-startup --python claystrips_ref.py -- <out.json>
"""
import bpy, sys, json

argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "claystrips_ref.json"

# Feel-relevant sculpt brush properties (guarded — not all exist on every build/tool).
PROPS = [
    "sculpt_tool", "strength", "hardness", "auto_smooth_factor", "normal_weight",
    "plane_offset", "plane_trim", "use_plane_trim",
    "area_radius_factor", "normal_radius_factor",     # <- area-plane sampling (the Clay Strips signature)
    "tip_roundness", "tip_scale_x",                   # <- square/box tip (the "strips" look)
    "crease_pinch_factor", "direction", "sculpt_plane",
    "use_original_normal", "use_original_plane", "use_frontface",
    "curve_preset", "use_accumulate", "flow", "density", "use_persistent", "height",
    "spacing", "use_space",
]

def dump_brush(tool):
    try:
        b = bpy.data.brushes.new(f"oracle_{tool}", mode='SCULPT')
    except Exception as e:
        return {"_error_new": str(e)}
    try:
        b.sculpt_tool = tool
    except Exception as e:
        return {"_error_tool": str(e)}
    d = {}
    for p in PROPS:
        try:
            v = getattr(b, p)
            d[p] = v if isinstance(v, (int, float, bool, str)) else str(v)
        except Exception:
            d[p] = None  # property not present for this tool/build
    return d

TOOLS = ["CLAY_STRIPS", "CLAY", "DRAW", "FLATTEN"]
data = {
    "blender_version": bpy.app.version_string,
    "note": "Clay Strips = area-averaged plane + offset + converging deposit + square/cube tip. "
            "Params below are Blender's; the area-plane algorithm is the load-bearing feel.",
    "brushes": {t: dump_brush(t) for t in TOOLS},
}
try:
    probe = bpy.data.brushes.new("enum_probe", mode='SCULPT')
    data["sculpt_tools_available"] = [it.identifier for it in probe.bl_rna.properties['sculpt_tool'].enum_items]
except Exception as e:
    data["sculpt_tools_available"] = str(e)

json.dump(data, open(out_path, "w"), indent=1)
cs = data["brushes"].get("CLAY_STRIPS", {})
print("[CLAY-ORACLE] blender=", data["blender_version"])
for k in ("area_radius_factor", "normal_radius_factor", "plane_offset", "tip_roundness", "hardness", "auto_smooth_factor"):
    print(f"[CLAY-ORACLE] CLAY_STRIPS.{k} = {cs.get(k)}")
print("[CLAY-ORACLE] wrote", out_path)
