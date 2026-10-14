"""spike_counter_ref.py — Blender COUNTER-TEST for the surface-sculpt spike hunt (2026-07-23).

Runs the 3 owner-reported artifact-prone sequences in REAL Blender sculpt (the correctness bar)
and exports the final mesh (verts + triangulated faces) so `spike_metric.py` can apply the SAME
isolated-peak metric the AZRA fuzz harness / AZRA_SCULPT_VALIDATE use:
    d_i   = |v_i - centroid(1-ring)|          (per vertex)
    abs   = d_i / (K * median_edge)           K = 8
    ctr   = d_i / (max ring d_j + 0.25*med)   / C, C = 3
    score = min(abs, ctr)   -> spike iff score > 1
Blender must produce NO spikes under this metric; its worst scores are the correctness bar the
AZRA numbers are compared against.

Scenarios (all on a 64x32 UV sphere, brush size 40px, SPACE stroke spacing 10):
  CLAY_CARVE : 4 repeated INVERTED Clay strokes along the same top-cap arc + 1 crossing carve
  DRAW_UNDO  : Draw stroke -> ed.undo -> Draw stroke crossing the undone area
  HOOK_DRAW  : Snake Hook pull (+Z drag) then a Draw stroke across the tube base

GOTCHAS (README): live window, NOT --background (sculpt stroke hard-crashes headless);
Blender 5.0: `sculpt_brush_type`, not `sculpt_tool`.

  blender.exe --factory-startup --python spike_counter_ref.py -- CLAY_CARVE clay_carve_spike_ref.json
"""
import bpy, sys, json
from mathutils import Matrix

argv = sys.argv[sys.argv.index("--")+1:]
SCEN = argv[0] if len(argv) > 0 else "CLAY_CARVE"
OUT  = argv[1] if len(argv) > 1 else f"{SCEN.lower()}_spike_ref.json"

def top_cap_arc(region, n, x0, x1, y=0.0, swap=False):
    """Stroke samples sweeping the sphere's top cap along X (or Y when swap)."""
    st = []
    for i in range(n):
        t = i/(n-1); x = x0 + (x1-x0)*t
        sx = x*0.5
        loc = (sx, y, (max(0.0, 1.0 - sx*sx - y*y))**0.5)
        if swap:
            loc = (y, sx, (max(0.0, 1.0 - sx*sx - y*y))**0.5)
        mx = region.width*0.5 + (loc[1] if swap else loc[0])*240.0
        my = region.height*0.5 + (loc[0] if swap else loc[1])*240.0
        st.append({"name": "s", "mouse": (mx, my), "mouse_event": (mx, my),
                   "is_start": (i == 0), "location": loc, "size": 40.0,
                   "pressure": 1.0, "time": float(i), "x_tilt": 0.0, "y_tilt": 0.0})
    return st

def hook_stroke(region, n, x0, x1, zlift):
    st = []
    for i in range(n):
        t = i/(n-1); x = x0 + (x1-x0)*t
        sx = x*0.5
        base_z = (max(0.0, 1.0 - sx*sx))**0.5
        loc = (sx, 0.0, base_z + zlift*t)
        mx = region.width*0.5 + loc[0]*240.0
        my = region.height*0.5
        st.append({"name": "s", "mouse": (mx, my), "mouse_event": (mx, my),
                   "is_start": (i == 0), "location": loc, "size": 40.0,
                   "pressure": 1.0, "time": float(i), "x_tilt": 0.0, "y_tilt": 0.0})
    return st

def run():
    res = {"scenario": SCEN, "blender_version": bpy.app.version_string}
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    bpy.ops.mesh.primitive_uv_sphere_add(segments=64, ring_count=32, radius=1.0)
    obj = bpy.context.active_object
    for pl in obj.data.polygons: pl.use_smooth = True

    win = bpy.context.window_manager.windows[0]; screen = win.screen
    area = next(a for a in screen.areas if a.type == 'VIEW_3D')
    region = next(r for r in area.regions if r.type == 'WINDOW')
    space = area.spaces.active; rv3d = space.region_3d
    rv3d.view_perspective = 'ORTHO'
    rv3d.view_matrix = Matrix.Translation((0, 0, -3.0)); rv3d.view_distance = 3.0

    bpy.ops.object.mode_set(mode='SCULPT')
    ts = bpy.context.tool_settings.sculpt
    b = ts.brush
    b.use_frontface = False
    b.stroke_method = 'SPACE'; b.spacing = 10

    def set_brush(tool, strength):
        try: b.sculpt_brush_type = tool
        except Exception as e: res.setdefault("tool_err", []).append(repr(e))
        b.strength = strength

    ov = dict(window=win, area=area, region=region,
              active_object=obj, space_data=space, region_data=rv3d)
    def stroke(st, mode='NORMAL'):
        with bpy.context.temp_override(**ov):
            bpy.ops.sculpt.brush_stroke(stroke=st, mode=mode)

    try:
        if SCEN == "CLAY_CARVE":
            set_brush("CLAY", 1.0)
            for _ in range(8):
                stroke(top_cap_arc(region, 24, -0.8, 0.8), mode='INVERT')  # deep repeated carve
            stroke(top_cap_arc(region, 24, -0.8, 0.8, swap=True), mode='INVERT')  # crossing carve
        elif SCEN == "DRAW_UNDO":
            set_brush("DRAW", 0.6)
            rest_snap = [tuple(v.co) for v in obj.data.vertices]
            stroke(top_cap_arc(region, 24, -0.8, 0.8))
            # Real Ctrl+Z first; ed.undo's poll rejects every context an app timer can build in
            # 5.0.1, so fall back to restoring the pre-stroke coords directly — byte-for-byte what
            # sculpt undo does to the mesh (restores positions), which is the part this
            # counter-test needs (a stroke crossing RESTORED geometry).
            try:
                with bpy.context.temp_override(window=win, screen=screen, area=area, region=region):
                    bpy.ops.ed.undo()
                res["undo"] = "ok"
            except Exception:
                for v, co in zip(obj.data.vertices, rest_snap):
                    v.co = co
                obj.data.update()
                res["undo"] = "manual-restore (ed.undo poll rejected in timer context)"
            stroke(top_cap_arc(region, 24, -0.8, 0.8, swap=True))  # draw across the undone area
        elif SCEN == "INFLATE_CARVE":
            # Repeated INVERTED Inflate along overlapping arcs (fuzz seed 5364's regime):
            # vertex-normal-driven displacement on folding geometry — does Blender's identical
            # normal feedback also produce isolated filament verts?
            set_brush("INFLATE", 1.0)
            for k in range(6):
                y = -0.10 + 0.04 * k
                stroke(top_cap_arc(region, 24, -0.6, 0.6, y=y), mode='INVERT')
        elif SCEN == "HOOK_DRAW":
            set_brush("SNAKE_HOOK", 1.0)
            try: b.crease_pinch_factor = 0.6
            except Exception: pass
            stroke(hook_stroke(region, 24, -0.2, 0.6, 0.5))
            set_brush("DRAW", 0.6)
            stroke(top_cap_arc(region, 24, -0.8, 0.8, swap=True))  # draw across the tube base
        res["stroke"] = "ok"
    except Exception as e:
        res["stroke"] = repr(e)

    obj = bpy.context.active_object
    me = obj.data
    me.calc_loop_triangles()
    # deformation sanity: max |v| deviation from the unit sphere (all scenarios start at r=1)
    import math
    res["max_radial_dev"] = round(max(abs(math.sqrt(sum(c*c for c in v.co)) - 1.0)
                                      for v in me.vertices), 6)
    res["nverts"] = len(me.vertices)
    res["verts"] = [[round(c, 6) for c in v.co] for v in me.vertices]
    res["tris"] = [list(t.vertices) for t in me.loop_triangles]
    json.dump(res, open(OUT, "w"))
    print(f"[SPIKE-ORACLE] {SCEN}: verts={res['nverts']} tris={len(res['tris'])} stroke={res['stroke']} -> {OUT}")
    bpy.ops.wm.quit_blender()

bpy.app.timers.register(run, first_interval=1.0)
