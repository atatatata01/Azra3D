# bool_diff_oracle.py — Blender reference for a box-difference boolean (POLY_POLISH_PLAN §4 2c).
# A = cube +-1 at origin, B = cube +-0.7 at (1,1,1). Difference A-B carves A's +++ corner.
# BSP output can't be vertex-diffed vs Blender's EXACT solver (different tessellation), so we record the
# algorithm-INDEPENDENT invariants our result must also satisfy: closed, manifold, bbox == minuend (+-1).
#   blender.exe --background --python bool_diff_oracle.py -- out.json
import bpy, json, sys

out = sys.argv[sys.argv.index("--") + 1:][0]
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()

bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))     # +-1
A = bpy.context.active_object
bpy.ops.mesh.primitive_cube_add(size=1.4, location=(1, 1, 1))     # +-0.7 at (1,1,1)
B = bpy.context.active_object

m = A.modifiers.new("bool", 'BOOLEAN'); m.operation = 'DIFFERENCE'; m.object = B; m.solver = 'EXACT'
bpy.context.view_layer.objects.active = A
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(B)

mesh = A.data
V = [list(v.co) for v in mesh.vertices]
F = [list(p.vertices) for p in mesh.polygons]
# manifold / closed census: each edge must have exactly 2 faces
from collections import Counter
ec = Counter()
for p in mesh.polygons:
    vs = list(p.vertices)
    for i in range(len(vs)):
        a, b = vs[i], vs[(i + 1) % len(vs)]
        ec[(min(a, b), max(a, b))] += 1
non_manifold = sum(1 for c in ec.values() if c != 2)
bmin = [min(v[i] for v in V) for i in range(3)]
bmax = [max(v[i] for v in V) for i in range(3)]
json.dump({"nv": len(V), "nf": len(F), "bbox_min": bmin, "bbox_max": bmax,
           "non_manifold_edges": non_manifold}, open(out, "w"), indent=1)
print("BOOL_DIFF: nv=%d nf=%d bbox=%s..%s non_manifold_edges=%d"
      % (len(V), len(F), bmin, bmax, non_manifold))
