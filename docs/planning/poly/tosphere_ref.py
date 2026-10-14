# tosphere_ref.py — Blender oracle for TO SPHERE (bpy.ops.transform.tosphere).
#   blender.exe --background --python tosphere_ref.py -- out.json
import bpy, bmesh, json, sys, math

out = sys.argv[sys.argv.index("--") + 1:][0]

def fresh(verts, faces):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh, obj

def tosphere(mesh, factor, sel_idx):
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='VERT')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(mesh); bm.verts.ensure_lookup_table()
    for i in sel_idx: bm.verts[i].select = True
    bmesh.update_edit_mesh(mesh)
    bpy.ops.transform.tosphere(value=factor)
    bpy.ops.object.mode_set(mode='OBJECT')
    return [[round(c, 6) for c in v.co] for v in mesh.vertices]

res = {}

# 3x3 flat grid in XZ, y=0 (like new_plane(2,2,2,2)) -> 9 verts, all selected
gv, gf = [], []
for iz in range(3):
    for ix in range(3):
        gv.append((-1 + ix, 0.0, -1 + iz))
for iz in range(2):
    for ix in range(2):
        a = iz*3+ix; gf.append((a, a+3, a+4, a+1))
m,_ = fresh(gv, gf); res["grid_f1"] = tosphere(m, 1.0, list(range(9)))
m,_ = fresh(gv, gf); res["grid_f05"] = tosphere(m, 0.5, list(range(9)))

# cube, select the +Y (top) 4 verts (indices 4,5,6,7)
cube_v = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
cube_f = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]
m,_ = fresh(cube_v, cube_f); res["cube_top_f1"] = tosphere(m, 1.0, [4,5,6,7])

# expose the center/radius Blender would use for the grid (mean pos, mean dist)
import statistics
pts = [(-1+ix,0.0,-1+iz) for iz in range(3) for ix in range(3)]
cx = sum(p[0] for p in pts)/9; cy = sum(p[1] for p in pts)/9; cz = sum(p[2] for p in pts)/9
meandist = sum(math.dist(p,(cx,cy,cz)) for p in pts)/9
res["grid_center"] = [cx,cy,cz]; res["grid_meandist"] = meandist

json.dump(res, open(out, "w"), indent=1)
for k,v in res.items():
    if isinstance(v, list) and v and isinstance(v[0], list):
        print("TOSPHERE %s: nv=%d" % (k, len(v)))
print("grid_center=%s meandist=%.6f" % (res["grid_center"], res["grid_meandist"]))
