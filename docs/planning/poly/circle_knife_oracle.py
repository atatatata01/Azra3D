# circle_knife_oracle.py — Blender reference for the CIRCLE knife (KNIFE.md, owner
# "KNIFE CIRCLE FAIL TO DO, GOT SEGMENT AFTER CUT", 2026-08-07).
#
# Two references in one run, because the C-shape knife has two commit paths
# (poly_knife_shape_release -> free_slice_commit  |  free_boolean):
#
#   [A] SUBTRACT reference — a circle PRISM (cylinder) DIFFERENCE a box, solver='EXACT'.
#       This is what the cookie-cutter path must match.
#   [B] SLICE reference — bmesh face split by a closed interior loop (the knife's job:
#       a circle drawn wholly inside one face). Built with bmesh.ops so it runs headless
#       (bpy.ops.mesh.knife_project needs a 3D viewport region, unavailable in --background).
#       Blender's own knife routes this through BM_face_split_edgenet, which yields
#       inner disc + outer KEYHOLE ring — NOT a dangling slit spur.
#
# Tessellation differs from ours, so we record only ALGORITHM-INDEPENDENT invariants:
# closed/manifold census (every edge exactly 2 faces), boundary-edge count, loose/wire
# edge count, coincident-vertex count, face-arity histogram, volume.
#
#   blender.exe --background --python circle_knife_oracle.py -- out.json
import bpy, bmesh, json, sys, math
from collections import Counter
from mathutils import Vector

out = sys.argv[sys.argv.index("--") + 1:][0]


def census(mesh, label):
    """Algorithm-independent invariants of a finished mesh."""
    ec = Counter()
    for p in mesh.polygons:
        vs = list(p.vertices)
        for i in range(len(vs)):
            a, b = vs[i], vs[(i + 1) % len(vs)]
            ec[(min(a, b), max(a, b))] += 1
    non_manifold = sum(1 for c in ec.values() if c != 2)
    boundary = sum(1 for c in ec.values() if c == 1)
    # loose / wire edges: an edge belonging to zero faces
    face_edges = set(ec.keys())
    wire = sum(1 for e in mesh.edges
               if (min(e.vertices), max(e.vertices)) not in face_edges)
    # orphan verts: used by no face
    used = set()
    for p in mesh.polygons:
        used.update(p.vertices)
    orphan = len(mesh.vertices) - len(used)
    # coincident vertices (1e-5 quantization)
    q = {}
    dup = 0
    for v in mesh.vertices:
        k = tuple(round(c * 1e5) for c in v.co)
        if k in q:
            dup += 1
        q[k] = 1
    # repeated-vertex rings (pinch / keyhole)
    pinched = sum(1 for p in mesh.polygons
                  if len(set(p.vertices)) < len(p.vertices))
    arity = dict(Counter(len(p.vertices) for p in mesh.polygons))
    bm = bmesh.new()
    bm.from_mesh(mesh)
    bmesh.ops.triangulate(bm, faces=bm.faces)
    vol = bm.calc_volume(signed=True)
    bm.free()
    r = {"label": label, "nv": len(mesh.vertices), "nf": len(mesh.polygons),
         "ne": len(mesh.edges), "non_manifold_edges": non_manifold,
         "boundary_edges": boundary, "wire_edges": wire, "orphan_verts": orphan,
         "coincident_verts": dup, "pinched_faces": pinched,
         "face_arity": {str(k): v for k, v in sorted(arity.items())},
         "volume": vol}
    print("[%s] " % label + "  ".join("%s=%s" % (k, r[k]) for k in
          ("nv", "nf", "ne", "non_manifold_edges", "boundary_edges", "wire_edges",
           "orphan_verts", "coincident_verts", "pinched_faces", "volume")))
    print("[%s] face_arity=%s" % (label, r["face_arity"]))
    return r


def wipe():
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()


# ------------------------------------------------------------------ [A] SUBTRACT
wipe()
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))          # +-1
A = bpy.context.active_object
bpy.ops.mesh.primitive_cylinder_add(vertices=16, radius=0.35, depth=4.0,
                                    location=(0, 0, 0), rotation=(0, math.pi / 2, 0))
B = bpy.context.active_object                                          # prism through X
m = A.modifiers.new("bool", 'BOOLEAN')
m.operation = 'DIFFERENCE'
m.object = B
m.solver = 'EXACT'
bpy.context.view_layer.objects.active = A
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(B)
sub = census(A.data, "SUBTRACT cylinder-through-box")

# ------------------------------------------------------------------ [B] SLICE
# A closed 16-gon loop drawn WHOLLY INSIDE the -X face of a +-1 box. This is the exact
# owner gesture. Blender's knife = BM_face_split_edgenet; bmesh.ops.edgenet_fill on the
# island-connected net is the headless equivalent.
# `bpy.ops.mesh.knife_project` needs a 3D viewport region (unavailable in --background) and
# `bmesh.ops.face_split_edgenet` is not exposed to Python. The scriptable stand-in with the
# SAME kernel underneath is `bpy.ops.mesh.intersect` ("Knife Intersect", BM_mesh_intersect):
# join a coplanar circle n-gon onto the box's -X face and intersect. Blender then re-tessellates
# the parent face around the loop. The circle's own face is deleted afterwards so the census
# describes only what the knife did to the BOX.
wipe()
bpy.ops.mesh.primitive_cube_add(size=2.0, location=(0, 0, 0))
C = bpy.context.active_object
bpy.ops.mesh.primitive_circle_add(vertices=16, radius=0.35, fill_type='NGON',
                                  location=(-1.0, 0, 0), rotation=(0, math.pi / 2, 0))
D = bpy.context.active_object
D.select_set(True)
C.select_set(True)
bpy.context.view_layer.objects.active = C
bpy.ops.object.join()
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_all(action='SELECT')
try:
    bpy.ops.mesh.intersect(mode='SELECT_UNSELECT', separate_mode='NONE',
                           solver='EXACT')
except Exception as e:                                   # noqa: BLE001
    print("[SLICE] mesh.intersect failed:", e)
bpy.ops.object.mode_set(mode='OBJECT')
sli = census(C.data, "SLICE closed circle inside one face (Knife Intersect)")

json.dump({"subtract": sub, "slice": sli}, open(out, "w"), indent=1)
print("WROTE", out)
