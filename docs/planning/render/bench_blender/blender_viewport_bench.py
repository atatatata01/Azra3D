# Blender viewport benchmark — mirror of AZRA_INSTANCE_STRESS (stress_test.rs
# spawn_shared_poly_instances): N objects sharing ONE cube mesh (half-extent 0.4),
# grid spacing 1.2, side ceil(sqrt(N)), camera target origin at distance
# min(grid_r*1.6, 200), nothing selected, Solid shading.
#
# Run:  blender --factory-startup --python blender_viewport_bench.py -- <count> <out.txt>
# Measures wm.redraw_timer DRAW_WIN (no swap -> no vsync floor; comparable to
# AZRA's CPU-frame median) and DRAW_WIN_SWAP (with present) over ITERS redraws.
import bpy, sys, time, math

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
COUNT = int(argv[0]) if len(argv) > 0 else 10000
OUT = argv[1] if len(argv) > 1 else "blender_bench_result.txt"
ITERS = 100


def build_scene():
    # Wipe default scene
    for ob in list(bpy.data.objects):
        bpy.data.objects.remove(ob, do_unlink=True)

    # One shared cube mesh, half-extent 0.4 (AZRA stress adj radius=0.4)
    mesh = bpy.data.meshes.new("InstBoxMesh")
    h = 0.4
    verts = [(x, y, z) for x in (-h, h) for y in (-h, h) for z in (-h, h)]
    faces = [(0, 1, 3, 2), (4, 6, 7, 5), (0, 4, 5, 1),
             (2, 3, 7, 6), (0, 2, 6, 4), (1, 5, 7, 3)]
    mesh.from_pydata(verts, [], faces)
    mesh.update()

    coll = bpy.context.scene.collection
    side = math.ceil(math.sqrt(COUNT))
    spacing = 1.2
    t0 = time.perf_counter()
    for i in range(COUNT):
        ix = i % side
        iz = i // side
        ob = bpy.data.objects.new("InstBox.%03d" % i, mesh)  # linked dup: shared mesh
        # AZRA grid is XZ (Y-up); Blender is Z-up -> use XY plane.
        ob.location = ((ix - side * 0.5) * spacing, (iz - side * 0.5) * spacing, 0.0)
        coll.objects.link(ob)
    bpy.context.view_layer.update()
    build_s = time.perf_counter() - t0

    # Deselect all (AZRA: deselect_all before measuring)
    for ob in bpy.context.selected_objects:
        ob.select_set(False)
    bpy.context.view_layer.objects.active = None

    grid_r = max(side * spacing * 0.5, 2.0)
    dist = min(max(grid_r * 1.6, 4.0), 200.0)
    return build_s, side, dist


def find_v3d():
    for win in bpy.context.window_manager.windows:
        for area in win.screen.areas:
            if area.type == "VIEW_3D":
                region = next(r for r in area.regions if r.type == "WINDOW")
                return win, area, region
    return None, None, None


def run_bench():
    build_s, side, dist = build_scene()
    win, area, region = find_v3d()
    lines = [
        "blender=%s" % bpy.app.version_string,
        "count=%d side=%d shared_mesh=1 tris=%d" % (COUNT, side, COUNT * 12),
        "build_s=%.1f" % build_s,
    ]
    if area is None:
        lines.append("FAIL: no VIEW_3D area")
    else:
        space = area.spaces.active
        space.shading.type = "SOLID"
        r3d = space.region_3d
        r3d.view_location = (0.0, 0.0, 0.0)
        r3d.view_distance = dist
        lines.append("view_distance=%.1f" % dist)
        with bpy.context.temp_override(window=win, area=area, region=region):
            # warmup
            bpy.ops.wm.redraw_timer(type="DRAW_WIN", iterations=10)
            for kind in ("DRAW_WIN", "DRAW_WIN_SWAP"):
                t0 = time.perf_counter()
                bpy.ops.wm.redraw_timer(type=kind, iterations=ITERS)
                dt = (time.perf_counter() - t0) / ITERS * 1000.0
                lines.append("%s avg_ms=%.2f (%d iters)" % (kind, dt, ITERS))
    with open(OUT, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))
    bpy.ops.wm.quit_blender()
    return None


# Defer until the window/UI is fully initialized.
bpy.app.timers.register(run_bench, first_interval=1.0)
