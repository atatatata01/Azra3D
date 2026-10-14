# Blender oracle for AZRA's "Circle Region" op (POLY_POLISH_PLAN §2 method).
#
#   blender.exe --background --python circle_ref.py -- circle_ref.json
#
# Reference op: LoopTools "Circle" (bundled extension bl_ext.blender_org.looptools),
# `bpy.ops.mesh.looptools_circle` at its UI DEFAULTS:
#     fit='best'  custom_radius=False  radius=1.0  flatten=True  regular=True
#     angle=0.0   influence=100.0      lock_x/y/z=False
#
# INPUT PARITY (§2 "author the SAME vertex list in both"): every case builds its mesh with
# from_pydata() from the SAME generation formula as the Rust test's HalfEdgeMesh::new_plane(2,2,4,4)
# — a 4x4 quad grid in the XZ plane at y=0, x,z in {-1,-0.5,0,0.5,1} (25 verts, 16 quads):
#     vert index iz*5 + ix  ->  (-1 + ix*0.5, 0, -1 + iz*0.5)
#     quad       [a, a+5, a+6, a+1]   with a = iz*5 + ix
# No axis mapping, no primitive-parity risk: identical coordinates on both sides.
#
# WHY THE "asym" CASE IS THE VERTEX-DIFF GATE (§0.3 "gate at the non-trivial case"):
# LoopTools anchors its REGULAR distribution on the loop vertex CLOSEST TO the loop centroid
# (circle_shift_loop). On a symmetric grid EIGHT boundary verts tie for closest, and Blender breaks
# the tie by its own bmesh loop order — unreproducible from outside, and Blender's own answer there is
# mesh-order dependent. So "asym" moves ONE boundary vertex (-1,0,0) -> (-0.6,0,0), which
#   (a) makes the closest-to-centroid vertex UNIQUE (0.625 vs 0.975 for the runner-up) => the anchor,
#       and therefore every output position, is well-defined; and
#   (b) breaks the symmetry that makes centroid+mean_radius coincide with the true least-squares
#       best-fit circle => a lazy "centroid + mean distance" fit CANNOT pass this case.
# "sym" is dumped too, but only for the properties that survive the tie (radius/planarity/interior).

import bpy, bmesh, addon_utils, json, sys, math

addon_utils.enable("bl_ext.blender_org.looptools", default_set=False, persistent=False)

SUB = 4          # quads per side
SIZE = 2.0       # full width/depth
EXTENT = SIZE * 0.5


def grid_verts_faces():
    """The exact generation formula of HalfEdgeMesh::new_plane(2.0, 2.0, 4, 4)."""
    verts = []
    for iz in range(SUB + 1):
        for ix in range(SUB + 1):
            x = -EXTENT + (ix / SUB) * SIZE
            z = -EXTENT + (iz / SUB) * SIZE
            verts.append((x, 0.0, z))
    faces = []
    for iz in range(SUB):
        for ix in range(SUB):
            a = iz * (SUB + 1) + ix
            faces.append((a, a + SUB + 1, a + SUB + 2, a + 1))
    return verts, faces


def clear():
    bpy.ops.object.select_all(action='SELECT')
    bpy.ops.object.delete()


def build(verts, faces, name):
    me = bpy.data.meshes.new(name)
    me.from_pydata([list(v) for v in verts], [], [list(f) for f in faces])
    me.update()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    bpy.context.view_layer.objects.active = ob
    ob.select_set(True)
    return ob


def run_circle(ob, **kw):
    bpy.ops.object.mode_set(mode='EDIT')
    bpy.ops.mesh.select_all(action='SELECT')
    res = bpy.ops.mesh.looptools_circle(**kw)
    bpy.ops.object.mode_set(mode='OBJECT')
    return res


def case(name, displace=None, **kw):
    clear()
    verts, faces = grid_verts_faces()
    if displace:
        for idx, pos in displace.items():
            verts[idx] = pos
    ob = build(verts, faces, name)
    before = [list(v.co) for v in ob.data.vertices]
    run_circle(ob, **kw)
    after = [[round(c, 6) for c in v.co] for v in ob.data.vertices]
    moved = [i for i in range(len(after))
             if (mathutils_len(after[i], before[i])) > 1e-6]
    return {
        "settings": {k: v for k, v in kw.items()},
        "input": [[round(c, 6) for c in v] for v in before],
        "verts": after,
        "nv": len(after),
        "nf": len(ob.data.polygons),
        "moved_indices": moved,
    }


def mathutils_len(a, b):
    return math.sqrt(sum((a[i] - b[i]) ** 2 for i in range(3)))


out = {}

# THE vertex-diff gate: unique anchor + asymmetric => exercises the real least-squares fit.
out["asym"] = case("asym", displace={10: (-0.6, 0.0, 0.0)})

# regular=False = "each vertex keeps its own angle" (the task's variant), same asymmetric input.
out["asym_nonregular"] = case("asym_nonregular", displace={10: (-0.6, 0.0, 0.0)}, regular=False)

# Custom radius (best-fit CENTRE, radius overridden).
out["asym_r1"] = case("asym_r1", displace={10: (-0.6, 0.0, 0.0)}, custom_radius=True, radius=0.75)

# Symmetric grid — anchor TIES 8 ways; properties only, not a vertex-diff gate.
out["sym"] = case("sym")

path = sys.argv[-1] if sys.argv[-1].endswith(".json") else "circle_ref.json"
with open(path, "w") as f:
    json.dump(out, f, indent=1)
print("WROTE", path)
for k, v in out.items():
    print(k, "nv", v["nv"], "nf", v["nf"], "moved", len(v["moved_indices"]))
