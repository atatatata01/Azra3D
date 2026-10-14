# extrude_region_ref.py — Blender oracle for REGION extrude (POLY_POLISH_PLAN §4 2a remainder).
# Box +-1 with the +Y top split into 2 coplanar quads (shared edge at x=0). Select BOTH top faces, run
# Extrude Region + Move (0,0.5,0): the region lifts as ONE piece — the shared interior edge stays welded,
# walls only on the OUTER boundary (per-face extrude would duplicate the shared edge).
#   blender.exe --background --python extrude_region_ref.py -- out.json
import bpy, bmesh, json, sys
from mathutils import Vector

out = sys.argv[sys.argv.index("--") + 1:][0]
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1),(0,1,-1),(0,1,1)]
faces = [(0,1,2,3),(7,9,8,4),(9,6,5,8),(4,8,5,1,0),(6,9,7,3,2),(7,4,0,3),(5,6,2,1)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_mode(type='FACE')
bpy.ops.mesh.select_all(action='DESELECT')
bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
for f in bm.faces:
    if abs(f.calc_center_median().y - 1.0) < 1e-4:  # the two +Y top faces
        f.select = True
bmesh.update_edit_mesh(mesh)
bpy.ops.mesh.extrude_region_move(TRANSFORM_OT_translate={"value": (0.0, 0.5, 0.0)})
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
json.dump({"verts": V, "faces": F, "nv": len(V), "nf": len(F)}, open(out, "w"), indent=1)
print("EXTRUDE_REGION: nv=%d nf=%d" % (len(V), len(F)))
