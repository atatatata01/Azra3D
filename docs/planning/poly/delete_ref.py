# delete_ref.py — Blender oracle for poly DELETE ops (POLY_POLISH_PLAN.md §4 Phase 2b).
# Cube matching new_box(Vec3::ONE). Case A: Delete Vertices on corner (1,1,1). Case B: Delete Edges on
# edge (1,1,-1)-(1,1,1). Dumps {verts,faces} per case for a vertex diff.
#   blender.exe --background --python delete_ref.py -- out_vert.json out_edge.json
import bpy, bmesh, json, sys
from mathutils import Vector

args = sys.argv[sys.argv.index("--") + 1:]
out_v, out_e = args[0], args[1]

verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]

def build():
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return mesh

def dump(mesh, path, label):
    bpy.ops.object.mode_set(mode='OBJECT')
    V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
    json.dump({"verts": V, "faces": F, "nv": len(V), "nf": len(F)}, open(path, "w"), indent=1)
    print("%s: nv=%d nf=%d -> %s" % (label, len(V), len(F), path))

# Case A — Delete Vertices on corner (1,1,1)
mesh = build()
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='VERT')
bpy.ops.mesh.select_all(action='DESELECT')
bm = bmesh.from_edit_mesh(mesh); bm.verts.ensure_lookup_table()
for v in bm.verts:
    if (v.co - Vector((1,1,1))).length < 1e-4: v.select = True
bmesh.update_edit_mesh(mesh)
bpy.ops.mesh.delete(type='VERT')
dump(mesh, out_v, "DELETE_VERT")

# Case B — Delete Edges on edge (1,1,-1)-(1,1,1)
mesh = build()
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='EDGE')
bpy.ops.mesh.select_all(action='DESELECT')
bm = bmesh.from_edit_mesh(mesh); bm.edges.ensure_lookup_table()
for e in bm.edges:
    cs = [tuple(round(c, 3) for c in v.co) for v in e.verts]
    if set(cs) == {(1.0,1.0,-1.0),(1.0,1.0,1.0)}: e.select = True
bmesh.update_edit_mesh(mesh)
bpy.ops.mesh.delete(type='EDGE')
dump(mesh, out_e, "DELETE_EDGE")
