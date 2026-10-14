# Limited Dissolve oracle v2 — via the mesh operator (object/edit mode), which is what
# users actually run. Cases: annulus (grid with hole), vertex pinch.
import bpy, json, sys

out = {}

def run_case(name, verts, faces):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    me = bpy.data.meshes.new(name)
    me.from_pydata(verts, [], faces)
    me.update()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    bpy.context.view_layer.objects.active = ob
    ob.select_set(True)
    bpy.ops.object.mode_set(mode="EDIT")
    bpy.ops.mesh.select_all(action="SELECT")
    bpy.ops.mesh.dissolve_limited(angle_limit=0.087)
    bpy.ops.object.mode_set(mode="OBJECT")
    rings = [list(p.vertices) for p in me.polygons]
    out[name] = {
        "faces": len(me.polygons),
        "edges": len(me.edges),
        "verts": len(me.vertices),
        "rings": rings,
        "any_repeated_vertex_in_ring": any(len(set(r)) != len(r) for r in rings),
    }

# Case A: 3x3 grid, center quad missing (annulus).
gv = [(x, y, 0.0) for y in range(4) for x in range(4)]
idx = lambda x, y: y * 4 + x
gf = [
    (idx(x, y), idx(x + 1, y), idx(x + 1, y + 1), idx(x, y + 1))
    for y in range(3) for x in range(3) if (x, y) != (1, 1)
]
run_case("annulus", gv, gf)

# Case B: two coplanar quads sharing exactly one vertex (pinch).
pv = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0), (2, 1, 0), (2, 2, 0), (1, 2, 0)]
pf = [(0, 1, 2, 3), (2, 4, 5, 6)]
run_case("pinch", pv, pf)

path = sys.argv[sys.argv.index("--") + 1]
with open(path, "w") as f:
    json.dump(out, f, indent=1)
print("ORACLE_OK")
