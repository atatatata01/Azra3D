# dissolve_faces_ref.py — Blender oracle for poly DISSOLVE FACES region-merge (POLY_POLISH_PLAN §4 2b, #10).
# Cube = new_box(Vec3::ONE). Select the +Y (top) and +Z (front) faces, run Dissolve Faces -> ONE n-gon.
#   blender.exe --background --python dissolve_faces_ref.py -- out.json
import bpy, bmesh, json, sys
from mathutils import Vector

out = sys.argv[sys.argv.index("--") + 1:][0]
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='FACE')
bpy.ops.mesh.select_all(action='DESELECT')
bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
for f in bm.faces:
    c = f.calc_center_median()
    if (c - Vector((0,1,0))).length < 1e-4 or (c - Vector((0,0,1))).length < 1e-4:
        f.select = True  # top (+Y) and front (+Z)
bmesh.update_edit_mesh(mesh)
bpy.ops.mesh.dissolve_faces()
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
json.dump({"verts": V, "faces": F, "nv": len(V), "nf": len(F)}, open(out, "w"), indent=1)
print("DISSOLVE_FACES: nv=%d nf=%d -> %s" % (len(V), len(F), out))
