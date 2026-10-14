# bool_coplanar_oracle.py — Blender reference for a COPLANAR-overlap boolean (POLY_POLISH_PLAN §4 2c, #2).
# A = box x∈[-1,1], B = box x∈[1,3] (share the face x=1). Union A∪B should DISSOLVE the coincident
# coplanar faces into ONE 2×1×1 box (x∈[-1,3]); a naive BSP keeps both → a doubled internal face =
# non-manifold. Record the invariants our result must satisfy (closed, manifold, bbox=[-1..3], convex box).
#   blender.exe --background --python bool_coplanar_oracle.py -- out.json
import bpy, json, sys
from collections import Counter

out = sys.argv[sys.argv.index("--") + 1:][0]
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()

bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))   # x∈[-1,1]
A = bpy.context.active_object
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(2, 0, 0))   # x∈[1,3]  (shares face x=1)
B = bpy.context.active_object

m = A.modifiers.new("bool", 'BOOLEAN'); m.operation = 'UNION'; m.object = B; m.solver = 'EXACT'
bpy.context.view_layer.objects.active = A
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(B)

mesh = A.data
V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
ec = Counter()
for p in mesh.polygons:
    vs = list(p.vertices)
    for i in range(len(vs)):
        a, b = vs[i], vs[(i+1) % len(vs)]
        ec[(min(a,b), max(a,b))] += 1
nonman = sum(1 for c in ec.values() if c != 2)
bmin = [min(v[i] for v in V) for i in range(3)]; bmax = [max(v[i] for v in V) for i in range(3)]
json.dump({"nv": len(V), "nf": len(F), "non_manifold_edges": nonman, "bbox_min": bmin, "bbox_max": bmax},
          open(out, "w"), indent=1)
print("BOOL_COPLANAR union: nv=%d nf=%d nonman=%d bbox=%s..%s" % (len(V), len(F), nonman, bmin, bmax))
