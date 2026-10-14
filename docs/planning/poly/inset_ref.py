# inset_ref.py — Blender oracle for INSET FACES, even offset (POLY_POLISH_PLAN §4 Phase 2e / audit #15).
#
# WHY THIS INPUT: a NON-SQUARE face. AZRA's current inset_face moves each vertex toward the face CENTROID
# by `amount`, which only coincides with Blender on a REGULAR polygon — a square top would falsely pass
# (POLY_POLISH_PLAN §0.3: gate at the real case, not the trivial one). Here the top face is 4 x 2, so the
# two methods diverge measurably:
#   Blender even offset (thickness=0.3): every edge keeps a UNIFORM 0.3 gap -> inner rect x=+-1.7, z=+-0.7
#   naive move-toward-centroid:          vertex (2,1,1) slides 0.3 along (-2,0,-1)/sqrt(5) -> (1.732,1,0.866)
#                                        i.e. edge gaps of 0.268 (x) and 0.134 (z) — UNEVEN and wrong.
#
# Blender settings pinned: bpy.ops.mesh.inset(thickness=0.3, depth=0.0). Defaults kept:
#   use_even_offset=True (the UI default), use_individual=False (region), use_relative_offset=False,
#   use_outset=False, use_boundary=True.
#
#   blender.exe --background --python inset_ref.py -- out.json
import bpy, bmesh, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]

# Box with half-extents (2,1,1) — matches AZRA `HalfEdgeMesh::new_box(Vec3::new(2.0,1.0,1.0))`.
verts = [(-2,-1,-1),(2,-1,-1),(2,-1,1),(-2,-1,1),(-2,1,-1),(2,1,-1),(2,1,1),(-2,1,1)]
faces = [(0,1,2,3),(4,5,6,7),(0,1,5,4),(1,2,6,5),(2,3,7,6),(3,0,4,7)]

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj

bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_all(action='SELECT')
bpy.ops.mesh.normals_make_consistent(inside=False)   # guarantee outward winding
bpy.ops.mesh.select_all(action='DESELECT')
bpy.ops.mesh.select_mode(type='FACE')
bm = bmesh.from_edit_mesh(mesh); bm.faces.ensure_lookup_table()
for f in bm.faces:
    if abs(f.calc_center_median().y - 1.0) < 1e-4:   # the +Y top face (4 x 2 — NON-square)
        f.select = True
bmesh.update_edit_mesh(mesh)

bpy.ops.mesh.inset(thickness=0.3, depth=0.0)          # even offset ON (default)
bpy.ops.object.mode_set(mode='OBJECT')

V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
# The 4 NEW inset verts are the y=+1 ones that are NOT original corners.
inner = sorted([[round(c, 6) for c in v] for v in V
                if abs(v[1] - 1.0) < 1e-4 and not (abs(abs(v[0]) - 2.0) < 1e-4 and abs(abs(v[2]) - 1.0) < 1e-4)])
json.dump({"verts": V, "faces": F, "nv": len(V), "nf": len(F), "inner_ring": inner},
          open(out, "w"), indent=1)
print("INSET: nv=%d nf=%d" % (len(V), len(F)))
print("INNER RING (expect x=+-1.7, z=+-0.7 for EVEN offset; +-1.732/+-0.866 = naive centroid bug):")
for p in inner:
    print("   ", p)
