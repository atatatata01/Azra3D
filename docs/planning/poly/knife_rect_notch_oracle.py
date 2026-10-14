# knife_rect_notch_oracle.py — Blender reference for the owner's "KNIFE FAIL" rect slice
# (2026-08-07). The gesture: a RECTANGLE drawn over a cube whose TOP side leaves the
# silhouette, so the rect is a NOTCH open through the top face, and the slice runs THROUGH
# (front + back sheets). AZRA produced two dangling slits + a coincident duplicate vertex.
#
# Reconstructed here as a rectangular-prism DIFFERENCE (solver='EXACT'), which is what a
# cookie-cutter notch must converge to topologically. Vertex-diffing our BSP output against
# Blender's EXACT solver is meaningless (different tessellation), so we record the
# algorithm-INDEPENDENT invariants the owner stated as the pass/fail gate:
#   1. no pinched face (a ring with a repeated vertex),
#   2. no coincident duplicate vertices (Blender merges by distance),
#   3. no boundary (unpaired) edge on a closed solid,
# plus the edge-face census, face-arity histogram and volume.
#
#   blender.exe --background --python knife_rect_notch_oracle.py -- out.json
import bpy, json, sys
from collections import Counter

out = sys.argv[sys.argv.index("--") + 1:][0]
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()

# A = cube +-0.5 (matches HalfEdgeMesh::new_box(Vec3::splat(0.5)) in the Rust repro test).
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(0, 0, 0))
A = bpy.context.active_object

# B = the rect cutter prism. Screen rect x in [-0.2, 0.2], y from -0.1 UP THROUGH the top
# (y = 1.0 > 0.5, so it exits the silhouette). Extruded along the view axis (z) far enough
# to pass through both sheets — the "prism along the captured view" our knife builds.
bpy.ops.mesh.primitive_cube_add(size=1.0, location=(0, 0, 0))
B = bpy.context.active_object
B.scale = (0.4 / 1.0, 1.1 / 1.0, 4.0)          # x: 0.4 wide, y: 1.1 tall, z: through
B.location = (0.0, -0.1 + 1.1 / 2.0, 0.0)      # y spans [-0.1, 1.0]

m = A.modifiers.new("bool", 'BOOLEAN'); m.operation = 'DIFFERENCE'; m.object = B; m.solver = 'EXACT'
bpy.context.view_layer.objects.active = A
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(B)

mesh = A.data
V = [list(v.co) for v in mesh.vertices]
F = [list(p.vertices) for p in mesh.polygons]

# (3) edge-face census: a closed solid has every edge shared by exactly 2 faces.
ec = Counter()
for p in mesh.polygons:
    vs = list(p.vertices)
    for i in range(len(vs)):
        a, b = vs[i], vs[(i + 1) % len(vs)]
        ec[(min(a, b), max(a, b))] += 1
boundary_edges = sum(1 for c in ec.values() if c == 1)
non_manifold_edges = sum(1 for c in ec.values() if c > 2)

# (1) pinched faces: any polygon whose ring repeats a vertex.
pinched = sum(1 for f in F if len(set(f)) < len(f))

# (2) coincident vertices, quantised to 1e-4 of the bbox diagonal (same rule as our diag).
bmin = [min(v[i] for v in V) for i in range(3)]
bmax = [max(v[i] for v in V) for i in range(3)]
diag = max(1e-6, sum((bmax[i] - bmin[i]) ** 2 for i in range(3)) ** 0.5)
q = max(1e-6, diag * 1e-4)
buckets = Counter(tuple(round(c / q) for c in v) for v in V)
coincident_clusters = sum(1 for c in buckets.values() if c > 1)

arity = dict(sorted(Counter(len(f) for f in F).items()))

vol = 0.0
for p in mesh.polygons:
    vs = [mesh.vertices[i].co for i in p.vertices]
    for i in range(1, len(vs) - 1):
        a, b, c = vs[0], vs[i], vs[i + 1]
        vol += a.dot(b.cross(c)) / 6.0

res = {
    "nv": len(V), "nf": len(F),
    "pinched_faces": pinched,
    "coincident_clusters": coincident_clusters,
    "boundary_edges": boundary_edges,
    "non_manifold_edges": non_manifold_edges,
    "face_arity": {str(k): v for k, v in arity.items()},
    "volume": abs(vol),
    "bbox_min": bmin, "bbox_max": bmax,
}
json.dump(res, open(out, "w"), indent=1)
print("KNIFE_RECT_NOTCH: nv=%d nf=%d pinched=%d coincident=%d boundary=%d nonmanifold=%d arity=%s vol=%.5f"
      % (len(V), len(F), pinched, coincident_clusters, boundary_edges, non_manifold_edges, arity, abs(vol)))
