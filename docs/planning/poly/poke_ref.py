# poke_ref.py — Blender oracle for POKE FACES (bpy.ops.mesh.poke).
# Emits, for several inputs, the vertex positions + face count after poke with the
# DEFAULT center (MEDIAN_WEIGHTED) and offset=0, plus an offset case and an irregular
# quad (where MEDIAN_WEIGHTED != plain centroid, to pin the weighting).
#   blender.exe --background --python poke_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

def fresh(verts, faces):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh, obj

def poke(mesh, offset=0.0, center='MEDIAN_WEIGHTED'):
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='FACE')
    bpy.ops.mesh.select_all(action='SELECT')
    bpy.ops.mesh.poke(offset=offset, use_relative_offset=False, center_mode=center)
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [[round(c, 6) for c in v.co] for v in mesh.vertices]
    F = [list(p.vertices) for p in mesh.polygons]
    return {"verts": V, "nv": len(V), "nf": len(F),
            "tri_only": all(len(p.vertices) == 3 for p in mesh.polygons)}

res = {}

# cube = new_box(Vec3::ONE)
cube_v = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
cube_f = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]
m,_ = fresh(cube_v, cube_f); res["cube"] = poke(m)
m,_ = fresh(cube_v, cube_f); res["cube_off"] = poke(m, offset=0.5)

# single quad (XZ plane, y=0), like new_plane(2,2,1,1): verts a,d,c,b winding [0,1,2,3] here
quad_v = [(-1,0,-1),(1,0,-1),(1,0,1),(-1,0,1)]
quad_f = [(0,1,2,3)]
m,_ = fresh(quad_v, quad_f); res["quad"] = poke(m)

# regular hexagon (ngon) in XZ, y=0, radius 1
import math
hex_v = [(math.cos(2*math.pi*k/6), 0.0, math.sin(2*math.pi*k/6)) for k in range(6)]
hex_f = [tuple(range(6))]
m,_ = fresh(hex_v, hex_f); res["hex"] = poke(m)

# single triangle
tri_v = [(-1,0,-1),(1,0,-1),(0,0,1)]
tri_f = [(0,1,2)]
m,_ = fresh(tri_v, tri_f); res["tri"] = poke(m)

# IRREGULAR quad — MEDIAN_WEIGHTED center differs from plain centroid.
irr_v = [(0,0,0),(4,0,0),(4.5,0,1),(0,0,1)]
irr_f = [(0,1,2,3)]
m,_ = fresh(irr_v, irr_f); res["irregular"] = poke(m)
# also plain centroid for reference
res["irregular_centroid"] = [sum(c)/4 for c in zip(*irr_v)]

json.dump(res, open(out, "w"), indent=1)
for k,v in res.items():
    if isinstance(v, dict):
        print("POKE %s: nv=%d nf=%d tri_only=%s" % (k, v["nv"], v["nf"], v["tri_only"]))
