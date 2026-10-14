"""sculpt_ref.py — GEOMETRIC Blender sculpt oracle (re-derived invocation, no GPL code).

Blender 5.0.1. `bpy.ops.sculpt.brush_stroke` needs a LIVE GPU context (PBVH raycast +
depth). It HARD-CRASHES (EXCEPTION_ACCESS_VIOLATION) under `--background`, so this oracle
runs in a REAL Blender window driven by an app timer, applies one scripted straight stroke
of a chosen brush to a fixed subdivided grid, dumps the resulting vertex positions +
a compact displacement profile to JSON, then quits.

  blender.exe --factory-startup --python sculpt_ref.py -- <brush> <out.json>
  <brush> in: DRAW GRAB SNAKE_HOOK PINCH CLAY_STRIPS CLAY

The grid is a 2x2 plane, 32x32 subdiv (1089 verts) in the XY plane at z=0, viewed top-down
ortho along -Z. Stroke: straight line along +X from x=-0.8..0.8 at y=0, brush size 40px
(≈0.33 world), strength/params from PARAMS[brush]. GRAB/SNAKE_HOOK also get a +Z drag
component so the elastic pull is measurable.

Output JSON:
  brush, blender_version, params, nverts,
  verts:  [[x,y,z], ...]        (full deformed positions)
  rest:   [[x,y,z], ...]        (undeformed, for delta)
  profile: {                     (SHAPE SIGNATURE for voxel≠mesh comparison)
     "x": [...],                 sample x positions along stroke centerline (y≈0 band)
     "dz": [...],                mean vertical displacement at each x (deposit brushes)
     "peak_dz": float, "peak_x": float,
     "cross_x": [...], "cross_dz": [...]  cross-section at stroke midpoint (x≈0 band)
  }
"""
import bpy, sys, json
from mathutils import Matrix

argv = sys.argv[sys.argv.index("--")+1:]
BRUSH = argv[0] if len(argv) > 0 else "DRAW"
OUT   = argv[1] if len(argv) > 1 else f"{BRUSH.lower()}_ref.json"

# Per-brush params: sculpt_tool + property overrides. Values chosen to match our defaults.
# strength 0.5, brush size 40px, straight stroke. Grab-family gets a Z drag so pull shows.
PARAMS = {
    "DRAW":        {"tool": "DRAW",        "strength": 0.5, "z_drag": 0.0},
    "GRAB":        {"tool": "GRAB",        "strength": 1.0, "z_drag": 0.4},
    "SNAKE_HOOK":  {"tool": "SNAKE_HOOK",  "strength": 1.0, "z_drag": 0.4,
                    "crease_pinch_factor": 0.5},
    "PINCH":       {"tool": "PINCH",       "strength": 0.5, "z_drag": 0.0},
    "CLAY_STRIPS": {"tool": "CLAY_STRIPS", "strength": 0.5, "z_drag": 0.0,
                    "tip_roundness": 0.18, "normal_radius_factor": 1.35},
    "CLAY":        {"tool": "CLAY",        "strength": 0.5, "z_drag": 0.0},
}

def run():
    p = PARAMS.get(BRUSH, PARAMS["DRAW"])
    res = {"brush": BRUSH, "blender_version": bpy.app.version_string, "params": p}
    # scene — plane for Draw/Pinch/Grab/SnakeHook; curved sphere for Clay-family (a flat
    # plane's area-plane coincides with the surface, so Clay Strips nets zero deposit — it
    # needs curvature to show the proud area-plane offset). Sphere is subdivided for density.
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    if BRUSH in ("CLAY_STRIPS", "CLAY"):
        bpy.ops.mesh.primitive_uv_sphere_add(segments=64, ring_count=32, radius=1.0)
        obj = bpy.context.active_object
        # shade smooth so the area normal is well-defined
        for pl in obj.data.polygons: pl.use_smooth = True
        res["mesh"] = "uv_sphere"
    else:
        bpy.ops.mesh.primitive_grid_add(x_subdivisions=32, y_subdivisions=32, size=2.0)
        obj = bpy.context.active_object
        res["mesh"] = "grid"
    rest = [tuple(v.co) for v in obj.data.vertices]
    res["nverts"] = len(rest)

    win = bpy.context.window_manager.windows[0]; screen = win.screen
    area = next(a for a in screen.areas if a.type == 'VIEW_3D')
    region = next(r for r in area.regions if r.type == 'WINDOW')
    space = area.spaces.active; rv3d = space.region_3d
    rv3d.view_perspective = 'ORTHO'
    rv3d.view_matrix = Matrix.Translation((0, 0, -3.0)); rv3d.view_distance = 3.0

    bpy.ops.object.mode_set(mode='SCULPT')
    # configure the active brush
    ts = bpy.context.tool_settings.sculpt
    b = ts.brush
    # Blender 5.0: brush type is `sculpt_brush_type` (was `sculpt_tool` <=4.2).
    try: b.sculpt_brush_type = p["tool"]
    except Exception as e: res["tool_err"] = repr(e)
    res["active_type"] = getattr(b, "sculpt_brush_type", None)
    b.strength = p.get("strength", 0.5)
    for k in ("crease_pinch_factor", "tip_roundness", "normal_radius_factor"):
        if k in p:
            try: setattr(b, k, p[k])
            except Exception: pass
    b.use_frontface = False
    # single dab spacing so our comparison is one continuous stroke
    b.stroke_method = 'SPACE'; b.spacing = 10

    zdrag = p.get("z_drag", 0.0)
    sphere = BRUSH in ("CLAY_STRIPS", "CLAY")
    def build():
        st = []; N = 24
        for i in range(N):
            t = i/(N-1); x = -0.8 + 1.6*t
            mx = region.width*0.5 + x*120.0; my = region.height*0.5
            if sphere:
                # sweep across the TOP cap of the sphere (z up toward the camera)
                sx = x*0.5
                loc = (sx, 0.0, (max(0.0, 1.0 - sx*sx))**0.5)
            else:
                # flat plane; grab-family lifts along +Z as the stroke advances
                loc = (x, 0.0, zdrag*t)
            st.append({"name": "s", "mouse": (mx, my), "mouse_event": (mx, my),
                       "is_start": (i == 0), "location": loc, "size": 40.0,
                       "pressure": 1.0, "time": float(i), "x_tilt": 0.0, "y_tilt": 0.0})
        return st
    try:
        with bpy.context.temp_override(window=win, area=area, region=region,
                                       active_object=obj, space_data=space, region_data=rv3d):
            bpy.ops.sculpt.brush_stroke(stroke=build(), mode='NORMAL')
        res["stroke"] = "ok"
    except Exception as e:
        res["stroke"] = repr(e)

    obj = bpy.context.active_object
    verts = [tuple(v.co) for v in obj.data.vertices]
    res["verts"] = [[round(c, 6) for c in v] for v in verts]
    res["rest"]  = [[round(c, 6) for c in v] for v in rest]

    # ---- profile signature (bins along the stroke) ----
    import math
    dz = [verts[i][2] - rest[i][2] for i in range(len(verts))]
    # lateral (in-plane) displacement magnitude and signed toward-axis component
    dlat = [math.hypot(verts[i][0]-rest[i][0], verts[i][1]-rest[i][1]) for i in range(len(verts))]
    # signed y-motion (Pinch squeezes verts toward the stroke line y=0 -> dy has sign -sign(y))
    dy = [verts[i][1] - rest[i][1] for i in range(len(verts))]
    # centerline: verts with |y|<0.05, binned by x
    NB = 21
    xs = [-1.0 + 2.0*(k/(NB-1)) for k in range(NB)]
    def binned(mask_axis, keep_lo, keep_hi, group_axis):
        acc = {k: [] for k in range(NB)}
        for i, v in enumerate(rest):
            if keep_lo <= v[mask_axis] <= keep_hi:
                gx = v[group_axis]
                k = int(round((gx + 1.0)/2.0*(NB-1)))
                if 0 <= k < NB: acc[k].append(dz[i])
        return [ (sum(a)/len(a) if a else 0.0) for k, a in sorted(acc.items()) ]
    def binned_arr(arr, mask_axis, lo, hi, group_axis):
        acc = {k: [] for k in range(NB)}
        for i, v in enumerate(rest):
            if lo <= v[mask_axis] <= hi:
                k = int(round((v[group_axis]+1.0)/2.0*(NB-1)))
                if 0 <= k < NB: acc[k].append(arr[i])
        return [ (sum(a)/len(a) if a else 0.0) for k, a in sorted(acc.items()) ]
    center_dz = binned_arr(dz, 1, -0.05, 0.05, 0)   # along X at y~0
    cross_dz  = binned_arr(dz, 0, -0.05, 0.05, 1)   # along Y at x~0
    cross_dy  = binned_arr(dy, 0, -0.05, 0.05, 1)   # signed y-motion across the stroke (Pinch)
    cross_lat = binned_arr(dlat, 0, -0.05, 0.05, 1) # lateral pull magnitude across stroke
    # radial displacement (for the sphere clay: deposit is along the surface normal = radial)
    dr = [math.sqrt(sum(c*c for c in verts[i])) - math.sqrt(sum(c*c for c in rest[i]))
          for i in range(len(verts))]
    # grab/clay: profile displacement vs distance from stroke MIDPOINT (footprint radial)
    mid = rest[0]  # first stroke sample area
    NR = 16
    def radial(arr, center3):
        acc = {k: [] for k in range(NR)}
        for i, v in enumerate(rest):
            d = math.sqrt(sum((v[j]-center3[j])**2 for j in range(3)))
            k = int(d/0.06)  # 0.06 world per bin
            if 0 <= k < NR: acc[k].append(arr[i])
        return [ (sum(a)/len(a) if a else 0.0) for k, a in sorted(acc.items()) ]
    # center of footprint ~ world origin sweep midpoint
    mid3 = (0.0, 0.0, 1.0) if sphere else (0.0, 0.0, 0.0)
    peak_i = max(range(NB), key=lambda k: abs(center_dz[k]))
    res["profile"] = {
        "dr_radial": [round(v, 6) for v in radial(dr, mid3)],
        "dlat_radial": [round(v, 6) for v in radial(dlat, mid3)],
        "max_dr": round(max(dr), 6), "min_dr": round(min(dr), 6),
        "x": [round(v, 4) for v in xs],
        "dz": [round(v, 6) for v in center_dz],
        "cross_x": [round(v, 4) for v in xs],
        "cross_dz": [round(v, 6) for v in cross_dz],
        "cross_dy": [round(v, 6) for v in cross_dy],
        "cross_lat": [round(v, 6) for v in cross_lat],
        "peak_dz": round(center_dz[peak_i], 6),
        "peak_x": round(xs[peak_i], 4),
        "max_dz": round(max(dz), 6), "min_dz": round(min(dz), 6),
        "max_lat": round(max(dlat), 6),
    }
    json.dump(res, open(OUT, "w"))
    print(f"[SCULPT-ORACLE] {BRUSH} peak_dz={res['profile']['peak_dz']} "
          f"max_dz={res['profile']['max_dz']} min_dz={res['profile']['min_dz']} -> {OUT}")
    bpy.ops.wm.quit_blender()

bpy.app.timers.register(run, first_interval=1.0)
