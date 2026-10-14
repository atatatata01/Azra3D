# connect_ref.py — Blender oracle for CONNECT VERTEX PATH (bpy.ops.mesh.vert_connect_path, key J).
#   blender.exe --background --python connect_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

def fresh(verts, faces):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh, obj

def connect_path(mesh, order):
    # vert_connect_path uses the ACTIVE-history order; select in `order` so history matches.
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='VERT')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(mesh); bm.verts.ensure_lookup_table()
    for i in order:
        bm.verts[i].select = True
        bm.select_history.add(bm.verts[i])
    bmesh.update_edit_mesh(mesh)
    bpy.ops.mesh.vert_connect_path()
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [[round(c,6) for c in v.co] for v in mesh.vertices]
    F = [list(p.vertices) for p in mesh.polygons]
    return {"verts": V, "nv": len(V), "nf": len(F),
            "face_sizes": sorted(len(p.vertices) for p in mesh.polygons)}

res = {}

# single quad, connect the two diagonal verts 0 and 2 -> 2 triangles
quad_v = [(-1,0,-1),(1,0,-1),(1,0,1),(-1,0,1)]
quad_f = [(0,1,2,3)]
m,_ = fresh(quad_v, quad_f); res["quad_diag"] = connect_path(m, [0,2])

# regular hexagon ngon, connect verts 0 and 3 (opposite) -> two quads(4-gons)
import math
hex_v = [(round(math.cos(2*math.pi*k/6),6), 0.0, round(math.sin(2*math.pi*k/6),6)) for k in range(6)]
hex_f = [tuple(range(6))]
m,_ = fresh(hex_v, hex_f); res["hex_0_3"] = connect_path(m, [0,3])

# 3-vert path across TWO faces: 1x2 grid (2 quads sharing an edge). Verts:
#   0(-1,0,-1) 1(0,0,-1) 2(1,0,-1) / 3(-1,0,1) 4(0,0,1) 5(1,0,1)
# faces: (0,3,4,1),(1,4,5,2). Path 0 -> 4 -> 2: 0-4 splits face A, 4-2 splits face B.
gv = [(-1,0,-1),(0,0,-1),(1,0,-1),(-1,0,1),(0,0,1),(1,0,1)]
gf = [(0,3,4,1),(1,4,5,2)]
m,_ = fresh(gv, gf); res["path_0_4_2"] = connect_path(m, [0,4,2])

json.dump(res, open(out, "w"), indent=1)
for k,v in res.items():
    print("CONNECT %s: nv=%d nf=%d sizes=%s" % (k, v["nv"], v["nf"], v["face_sizes"]))
