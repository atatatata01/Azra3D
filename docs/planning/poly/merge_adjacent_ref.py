# merge_adjacent_ref.py — Blender oracle for merging two ADJACENT verts (verification #8).
# new_box(±1) verts 0=(-1,-1,-1) and 1=(1,-1,-1) share the bottom-back edge. Merge At Center collapses
# that edge: the 2 quads using it become triangles. AZRA currently makes a self-loop (rejected).
#   blender.exe --background --python merge_adjacent_ref.py -- out.json
import bpy, bmesh, json, sys
from mathutils import Vector

out = sys.argv[sys.argv.index("--") + 1:][0]
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("m", mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='VERT')
bpy.ops.mesh.select_all(action='DESELECT')
bm = bmesh.from_edit_mesh(mesh); bm.verts.ensure_lookup_table()
for v in bm.verts:
    if (v.co - Vector((-1,-1,-1))).length < 1e-4 or (v.co - Vector((1,-1,-1))).length < 1e-4:
        v.select = True
bmesh.update_edit_mesh(mesh)
bpy.ops.mesh.merge(type='CENTER')
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
nonman = 0
from collections import Counter
ec = Counter()
for p in mesh.polygons:
    vs = list(p.vertices)
    for i in range(len(vs)):
        a,b = vs[i], vs[(i+1)%len(vs)]; ec[(min(a,b),max(a,b))] += 1
nonman = sum(1 for c in ec.values() if c != 2)
json.dump({"nv": len(V), "nf": len(F), "nverts_per_face": sorted(len(f) for f in F),
           "nonman": nonman, "merged_at": [0.0,-1.0,-1.0]}, open(out, "w"), indent=1)
print("MERGE_ADJACENT: nv=%d nf=%d faces=%s nonman=%d" % (len(V), len(F), sorted(len(f) for f in F), nonman))
