# extrude_ref.py — Blender oracle for the poly EXTRUDE tool (POLY_POLISH_PLAN.md §2/§4 Phase 2a).
# Build a cube matching AZRA new_box(Vec3::ONE) (±1, same verts/faces), select the bottom (-Y) face,
# run the user-facing Extrude Region + Move (0,0.5,0), dump {verts,faces} to JSON for a vertex diff.
#   "C:\Program Files\Blender Foundation\Blender 4.5\blender.exe" --background --python extrude_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

# match new_box(1,1,1) EXACTLY (same 8 verts + 6 quad faces as half_edge.rs::new_box)
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("m")
mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("o", mesh)
bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bpy.ops.object.mode_set(mode='EDIT')
bm = bmesh.from_edit_mesh(mesh)
bm.faces.ensure_lookup_table()
# select the bottom face (centroid y == -1) — the AZRA face 0
for f in bm.faces:
    f.select = abs(f.calc_center_median().y + 1.0) < 1e-4
bmesh.update_edit_mesh(mesh)

# the user-facing tool: Extrude Region + Move
bpy.ops.mesh.extrude_region_move(TRANSFORM_OT_translate={"value": (0.0, 0.5, 0.0)})
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]
F = [list(p.vertices) for p in mesh.polygons]
json.dump({"verts": V, "faces": F, "nv": len(V), "nf": len(F)}, open(out, "w"), indent=1)
print("EXTRUDE ORACLE: nv=%d nf=%d -> %s" % (len(V), len(F), out))
