# bool_seq_oracle.py — Blender ground truth for the SEQUENTIAL double-subtraction repro (§12 XL).
# Mirrors the app workflow: apply cut1 to A (bake), then apply cut2 to the RESULT.
# A  = cube size2 @ origin            -> [-1,1]^3
# c1 = cube size2 @ (1,1,1)           -> [0,2]^3   (removes +++ corner cube [0,1]^3)
# c2 = cube size2 @ (-1,1,1)          -> [-2,0]x[0,2]x[0,2] (removes -++ corner [-1,0]x[0,1]x[0,1])
# c1's -X face and c2's +X face are BOTH the plane x=0 -> coplanar seam = the csg.js failure trigger.
#   blender.exe --background --python bool_seq_oracle.py -- out.json
import bpy, json, sys
from collections import Counter

out = sys.argv[sys.argv.index("--") + 1:][0]

def census(obj):
    mesh = obj.data
    V = [list(v.co) for v in mesh.vertices]
    F = [list(p.vertices) for p in mesh.polygons]
    ec = Counter()
    for p in mesh.polygons:
        vs = list(p.vertices)
        for i in range(len(vs)):
            a, b = vs[i], vs[(i + 1) % len(vs)]
            ec[(min(a, b), max(a, b))] += 1
    nonman = sum(1 for c in ec.values() if c != 2)
    bmin = [min(v[i] for v in V) for i in range(3)]
    bmax = [max(v[i] for v in V) for i in range(3)]
    sizes = {}
    for f in F:
        sizes[len(f)] = sizes.get(len(f), 0) + 1
    return {"nv": len(V), "nf": len(F), "non_manifold_edges": nonman,
            "bbox_min": bmin, "bbox_max": bmax, "face_sizes": sizes}

bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0)); A = bpy.context.active_object
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(1, 1, 1)); C1 = bpy.context.active_object
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(-1, 1, 1)); C2 = bpy.context.active_object

bpy.context.view_layer.objects.active = A
m = A.modifiers.new("b1", 'BOOLEAN'); m.operation = 'DIFFERENCE'; m.object = C1; m.solver = 'EXACT'
bpy.ops.object.modifier_apply(modifier="b1")
after1 = census(A)

m = A.modifiers.new("b2", 'BOOLEAN'); m.operation = 'DIFFERENCE'; m.object = C2; m.solver = 'EXACT'
bpy.ops.object.modifier_apply(modifier="b2")
after2 = census(A)

json.dump({"after_cut1": after1, "after_cut2": after2}, open(out, "w"), indent=1)
print("SEQ after_cut1:", after1)
print("SEQ after_cut2:", after2)
