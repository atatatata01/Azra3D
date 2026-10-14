# solidify_ref.py — Blender reference for the SOLIDIFY thicken op (POLY_POLISH_PLAN §5).
# Two cases, thickness 0.2, offset -1 (Blender default = inward):
#   A: closed cube +-1        -> hollow shell (inner + outer), no rim (already closed)
#   B: open single quad (z=0) -> a thin slab (inner + outer + 4 rim quads)
# BSP-free op; we record the topology INVARIANTS our robust solidify must also satisfy (Blender's exact
# vertex-normal averaging differs), so gate on nv/nf + closed + manifold + thickness, not vertex-for-vertex.
#   blender.exe --background --python solidify_ref.py -- out.json
import bpy, json, sys
from collections import Counter

out = sys.argv[sys.argv.index("--") + 1:][0]

def build(name, verts, faces):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    mesh = bpy.data.meshes.new(name); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new(name, mesh); bpy.context.collection.objects.link(obj)
    bpy.context.view_layer.objects.active = obj
    return obj, mesh

def solidify(obj, mesh, thickness):
    m = obj.modifiers.new("sol", 'SOLIDIFY'); m.thickness = thickness; m.offset = -1.0
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.modifier_apply(modifier="sol")
    V = [list(v.co) for v in mesh.vertices]; F = [list(p.vertices) for p in mesh.polygons]
    ec = Counter()
    for p in mesh.polygons:
        vs = list(p.vertices)
        for i in range(len(vs)):
            a, b = vs[i], vs[(i+1) % len(vs)]
            ec[(min(a,b), max(a,b))] += 1
    nonman = sum(1 for c in ec.values() if c != 2)
    return {"nv": len(V), "nf": len(F), "non_manifold_edges": nonman,
            "bbox_min": [min(v[i] for v in V) for i in range(3)],
            "bbox_max": [max(v[i] for v in V) for i in range(3)]}

cube_v = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
cube_f = [(0,1,2,3),(7,6,5,4),(4,5,1,0),(6,7,3,2),(7,4,0,3),(5,6,2,1)]
oc, mc = build("cube", cube_v, cube_f)
A = solidify(oc, mc, 0.2)

quad_v = [(-1,-1,0),(1,-1,0),(1,1,0),(-1,1,0)]
quad_f = [(0,1,2,3)]
oq, mq = build("quad", quad_v, quad_f)
B = solidify(oq, mq, 0.2)

json.dump({"cube": A, "plane": B}, open(out, "w"), indent=1)
print("SOLIDIFY cube: nv=%d nf=%d nonman=%d | plane: nv=%d nf=%d nonman=%d"
      % (A["nv"], A["nf"], A["non_manifold_edges"], B["nv"], B["nf"], B["non_manifold_edges"]))
