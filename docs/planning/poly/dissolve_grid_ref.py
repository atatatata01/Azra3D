# dissolve_grid_ref.py — Blender oracle for Dissolve Faces with a true INTERIOR vertex (verification #6).
# 3x3-vertex grid (4 quads, shared center vertex 4). Dissolve ALL 4 faces -> ONE 8-gon; the interior
# center vertex is REMOVED by Blender (AZRA leaks it as a loose orphan -> nv diverges).
#   blender.exe --background --python dissolve_grid_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]
verts = [(x, y, 0) for y in (-1.0, 0.0, 1.0) for x in (-1.0, 0.0, 1.0)]  # idx = y*3 + x, center = 4
faces = [(0,1,4,3), (1,2,5,4), (3,4,7,6), (4,5,8,7)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("g"); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("g", mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='FACE')
bpy.ops.mesh.select_all(action='SELECT')
bpy.ops.mesh.dissolve_faces()
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
has_center = any(abs(v[0]) < 1e-4 and abs(v[1]) < 1e-4 and abs(v[2]) < 1e-4 for v in V)
json.dump({"nv": len(V), "nf": len(F), "nverts_per_face": [len(f) for f in F], "has_center": has_center},
          open(out, "w"), indent=1)
print("DISSOLVE_GRID: nv=%d nf=%d faces=%s center_kept=%s" % (len(V), len(F), [len(f) for f in F], has_center))
