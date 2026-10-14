# Blender dense-mesh benchmark — competitor targets for AZRA PERF_MATRIX cells
# G1 (one dense mesh) states S1 idle / S2 selected / S4 edit+wire, and G2 (many unique).
# Run: blender --factory-startup --python blender_dense_bench.py -- dense <grid_n> <out.txt>
#      blender --factory-startup --python blender_dense_bench.py -- unique <count> <grid_n> <out.txt>
import bpy, sys, time

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
MODE = argv[0] if argv else "dense"
if MODE == "dense":
    GRID = int(argv[1]) if len(argv) > 1 else 707
    OUT = argv[2] if len(argv) > 2 else "blender_dense.txt"
    COUNT = 1
else:
    COUNT = int(argv[1]) if len(argv) > 1 else 100
    GRID = int(argv[2]) if len(argv) > 2 else 158
    OUT = argv[3] if len(argv) > 3 else "blender_unique.txt"
ITERS = 40


def v3d():
    for win in bpy.context.window_manager.windows:
        for area in win.screen.areas:
            if area.type == "VIEW_3D":
                region = next(r for r in area.regions if r.type == "WINDOW")
                return win, area, region
    return None, None, None


def measure(win, area, region, label, lines, iters=ITERS):
    with bpy.context.temp_override(window=win, area=area, region=region):
        bpy.ops.wm.redraw_timer(type="DRAW_WIN", iterations=8)  # warmup
        t0 = time.perf_counter()
        bpy.ops.wm.redraw_timer(type="DRAW_WIN", iterations=iters)
        dt = (time.perf_counter() - t0) / iters * 1000.0
    lines.append("%s avg_ms=%.2f (%d iters)" % (label, dt, iters))


def run():
    lines = ["blender=%s mode=%s" % (bpy.app.version_string, MODE)]
    for ob in list(bpy.data.objects):
        bpy.data.objects.remove(ob, do_unlink=True)

    t0 = time.perf_counter()
    if MODE == "dense":
        bpy.ops.mesh.primitive_grid_add(x_subdivisions=GRID, y_subdivisions=GRID, size=10)
        tris = GRID * GRID * 2
        lines.append("grid=%dx%d tris=%d build_s=%.1f" % (GRID, GRID, tris, time.perf_counter() - t0))
    else:
        side = int(COUNT ** 0.5) + 1
        for i in range(COUNT):
            bpy.ops.mesh.primitive_grid_add(
                x_subdivisions=GRID, y_subdivisions=GRID, size=8,
                location=((i % side) * 12.0, (i // side) * 12.0, 0.0))
        tris = COUNT * GRID * GRID * 2
        lines.append("count=%d grid=%dx%d total_tris=%d build_s=%.1f"
                     % (COUNT, GRID, GRID, tris, time.perf_counter() - t0))

    win, area, region = v3d()
    if area is None:
        lines.append("FAIL: no VIEW_3D")
    else:
        space = area.spaces.active
        space.shading.type = "SOLID"
        r3d = space.region_3d
        r3d.view_location = (0.0, 0.0, 0.0) if MODE == "dense" else (60.0, 60.0, 0.0)
        r3d.view_distance = 18.0 if MODE == "dense" else 160.0

        # S1: object mode, deselected, idle
        for ob in bpy.context.selected_objects:
            ob.select_set(False)
        bpy.context.view_layer.objects.active = None
        measure(win, area, region, "S1_object_idle_deselected", lines)

        # S2: selected (active last-created object)
        last = bpy.data.objects[-1]
        last.select_set(True)
        bpy.context.view_layer.objects.active = last
        measure(win, area, region, "S2_object_selected", lines)

        # S4-wire in object mode: wireframe overlay on all
        space.overlay.show_wireframes = True
        measure(win, area, region, "S4a_object_wire_overlay", lines)
        space.overlay.show_wireframes = False

        if MODE == "dense":
            # Edit mode: entry time (T4 comparison: AZRA target Tab-in <150ms) + draw cost
            t0 = time.perf_counter()
            bpy.ops.object.mode_set(mode="EDIT")
            lines.append("edit_mode_enter_s=%.2f" % (time.perf_counter() - t0))
            measure(win, area, region, "S4_edit_mode_wire", lines, iters=20)
            t0 = time.perf_counter()
            bpy.ops.object.mode_set(mode="OBJECT")
            lines.append("edit_mode_exit_s=%.2f" % (time.perf_counter() - t0))

    with open(OUT, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))
    bpy.ops.wm.quit_blender()
    return None


bpy.app.timers.register(run, first_interval=1.0)
