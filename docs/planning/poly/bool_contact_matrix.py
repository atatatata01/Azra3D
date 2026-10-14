# Blender exact-boolean oracle — CONTACT edge-case matrix (owner 2026-08-03:
# "make sure the boolean code is up to par with Blender with all edge cases").
# Cells not covered by the existing oracles: edge touch, vertex touch, identical
# solids (A-A / A+A / A*A), cutter fully inside (cavity), flush full-face cover.
import bpy, json, sys

def cube(name, loc, size=1.0):
    bpy.ops.mesh.primitive_cube_add(size=size, location=loc)
    ob = bpy.context.active_object
    ob.name = name
    return ob

def run(op, a_loc, b_loc, a_size=1.0, b_size=1.0):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    a = cube("A", a_loc, a_size)
    b = cube("B", b_loc, b_size)
    mod = a.modifiers.new("bool", "BOOLEAN")
    mod.operation = op
    mod.solver = "EXACT"
    mod.object = b
    bpy.context.view_layer.objects.active = a
    bpy.ops.object.modifier_apply(modifier="bool")
    me = a.data
    # volume via bmesh
    import bmesh
    bm = bmesh.new(); bm.from_mesh(me)
    vol = bm.calc_volume(signed=True)
    nonman = sum(1 for e in bm.edges if not e.is_manifold)
    bm.free()
    return {"verts": len(me.vertices), "edges": len(me.edges), "faces": len(me.polygons),
            "volume": round(vol, 6), "nonmanifold_edges": nonman}

out = {}
# 1. Edge touch: unit cubes side by side sharing exactly one edge (diagonal offset).
out["edge_touch_diff"]  = run("DIFFERENCE", (0,0,0), (1.0, 1.0, 0.0))
out["edge_touch_union"] = run("UNION",      (0,0,0), (1.0, 1.0, 0.0))
# 2. Vertex touch: corner-to-corner.
out["vertex_touch_diff"]  = run("DIFFERENCE", (0,0,0), (1.0, 1.0, 1.0))
out["vertex_touch_union"] = run("UNION",      (0,0,0), (1.0, 1.0, 1.0))
# 3. Identical solids.
out["identical_diff"]      = run("DIFFERENCE", (0,0,0), (0,0,0))
out["identical_union"]     = run("UNION",      (0,0,0), (0,0,0))
out["identical_intersect"] = run("INTERSECT",  (0,0,0), (0,0,0))
# 4. Cutter fully inside (cavity): small cube centered inside big cube.
out["contained_diff"]      = run("DIFFERENCE", (0,0,0), (0,0,0), 2.0, 1.0)
out["contained_intersect"] = run("INTERSECT",  (0,0,0), (0,0,0), 2.0, 1.0)
# 5. Flush full-face cover: same-footprint cube stacked exactly on top (shares the full face).
out["flush_stack_diff"]  = run("DIFFERENCE", (0,0,0), (0, 0, 1.0))
out["flush_stack_union"] = run("UNION",      (0,0,0), (0, 0, 1.0))
# 6. Half-embedded flush side: B shifted so one face plane coincides with A's face.
out["halfface_flush_diff"] = run("DIFFERENCE", (0,0,0), (0.5, 0.0, 0.5))

path = sys.argv[sys.argv.index("--") + 1]
with open(path, "w") as f:
    json.dump(out, f, indent=1)
print("ORACLE_OK")
