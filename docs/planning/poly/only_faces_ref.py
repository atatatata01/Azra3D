# only_faces_ref.py — Blender oracle for poly "Delete > Only Faces" (delete(type="ONLY_FACE")).
# Proves ONLY_FACE keeps every edge+vertex (loose ones become WIRE edges) and CONTRASTS with
# type="FACE" (which removes the now-loose edges). Dumps verts + undirected edge pairs + face count.
#   blender.exe --background --python only_faces_ref.py -- out.json
#
# Geometry matches the AZRA test inputs exactly:
#   (i)  isolated quad: a 1x1 plane in Z=0, corners (±0.5, ±0.5, 0). from_polygons 1 quad.
#   (ii) cube spanning ±1 = new_box(Vec3::ONE). Delete one face (any -> 5f/12e/8v).
#   (iii) 2x2 grid: 3x3 verts at x,y in {-1,0,1}, z=0, 4 quads. Delete the 2 LEFT faces (centroid x<0).
import bpy, bmesh, json, sys
from mathutils import Vector

out = sys.argv[sys.argv.index("--") + 1:][0]

def build(verts, faces):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh

def dump(mesh):
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [[round(c, 4) for c in v.co] for v in mesh.vertices]
    E = []
    for e in mesh.edges:
        a = tuple(round(c, 4) for c in mesh.vertices[e.vertices[0]].co)
        b = tuple(round(c, 4) for c in mesh.vertices[e.vertices[1]].co)
        E.append(sorted([list(a), list(b)]))
    return {"verts": sorted(V), "edges": sorted(E), "nv": len(V), "ne": len(E), "nf": len(mesh.polygons)}

def run(verts, faces, select_pred, del_type):
    mesh = build(verts, faces)
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_mode(type='FACE')
    bpy.ops.mesh.select_all(action='DESELECT')
    bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
    for f in bm.faces:
        if select_pred(f.calc_center_median()):
            f.select = True
    bmesh.update_edit_mesh(mesh)
    bpy.ops.mesh.delete(type=del_type)
    return dump(mesh)

results = {}

# (i) isolated quad
quad_v = [(-0.5,-0.5,0),(0.5,-0.5,0),(0.5,0.5,0),(-0.5,0.5,0)]
quad_f = [(0,1,2,3)]
results["quad_only_face"] = run(quad_v, quad_f, lambda c: True, 'ONLY_FACE')
results["quad_face"]      = run(quad_v, quad_f, lambda c: True, 'FACE')   # contrast

# (ii) cube ±1 = new_box(Vec3::ONE); delete one face (the +Y top)
cube_v = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
cube_f = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]
results["cube_one_only_face"] = run(cube_v, cube_f,
    lambda c: (c - Vector((0,1,0))).length < 1e-4, 'ONLY_FACE')

# (iii) 2x2 grid; delete the 2 LEFT faces (centroid x < 0)
grid_v, idx = [], {}
for j, y in enumerate((-1,0,1)):
    for i, x in enumerate((-1,0,1)):
        idx[(i,j)] = len(grid_v); grid_v.append((x, y, 0))
grid_f = []
for j in range(2):
    for i in range(2):
        grid_f.append((idx[(i,j)], idx[(i+1,j)], idx[(i+1,j+1)], idx[(i,j+1)]))
results["grid_left2_only_face"] = run(grid_v, grid_f, lambda c: c.x < -1e-4, 'ONLY_FACE')

json.dump(results, open(out, "w"), indent=1)
for k, r in results.items():
    print("%s: nv=%d ne=%d nf=%d" % (k, r["nv"], r["ne"], r["nf"]))
