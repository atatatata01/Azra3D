# Blender oracle: L-step prism, bevel the single CONCAVE (reflex) edge, seg=5.
# Mirrors core_math bevel_op::tests::l_step_prism exactly:
#   profile CCW in XY: (0,0)(2,0)(2,1)(1,1)(1,2)(0,2), extruded z = -1..+1
#   concave edge = the vertical edge at profile vertex (1,1)
# Settings per BEVEL.md 7: Width Type=Offset, profile 0.5, Grid Fill, Miter Sharp, loop slide on.
#
# Usage: blender.exe --background --python lstep_concave_ref.py -- out.json
import bpy, bmesh, json, sys, mathutils

out = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else "lstep_concave_ref.json"

# Clean scene
bpy.ops.object.select_all(action="SELECT")
bpy.ops.object.delete(use_global=False)

prof = [(0, 0), (2, 0), (2, 1), (1, 1), (1, 2), (0, 2)]
verts = [(x, y, -1.0) for (x, y) in prof] + [(x, y, 1.0) for (x, y) in prof]
faces = [tuple(reversed(range(6))), tuple(range(6, 12))]
for i in range(6):
    j = (i + 1) % 6
    faces.append((i, j, 6 + j, 6 + i))

mesh = bpy.data.meshes.new("lstep")
mesh.from_pydata([mathutils.Vector(v) for v in verts], [], [list(f) for f in faces])
mesh.validate()
mesh.update()
obj = bpy.data.objects.new("lstep", mesh)
bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj
obj.select_set(True)

WIDTH = 0.3
SEGMENTS = 5

bm = bmesh.new()
bm.from_mesh(mesh)
bm.verts.ensure_lookup_table()
bm.edges.ensure_lookup_table()

# The concave edge: both endpoints at (1,1), differing in z.
def is_concave_edge(e):
    a, b = e.verts[0].co, e.verts[1].co
    return (abs(a.x - 1) < 1e-6 and abs(a.y - 1) < 1e-6
            and abs(b.x - 1) < 1e-6 and abs(b.y - 1) < 1e-6)

target = [e for e in bm.edges if is_concave_edge(e)]
print("[ORACLE] concave edges selected:", len(target))

res = bmesh.ops.bevel(
    bm,
    geom=target,
    offset=WIDTH,
    offset_type="OFFSET",
    segments=SEGMENTS,
    profile=0.5,
    affect="EDGES",
    clamp_overlap=True,
    loop_slide=True,
    miter_outer="SHARP",
    miter_inner="SHARP",
)

bm.verts.ensure_lookup_table()
bm.faces.ensure_lookup_table()
bm.normal_update()

data = {
    "width": WIDTH,
    "segments": SEGMENTS,
    "verts": [[round(c, 6) for c in v.co] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
    "n_verts": len(bm.verts),
    "n_faces": len(bm.faces),
    "new_faces": len(res.get("faces", [])),
    # Faces created at the fillet ENDS (any face touching the z=+-1 planes AND a new vert)
    "end_plane_faces": [
        [v.index for v in f.verts]
        for f in bm.faces
        if all(abs(abs(v.co.z) - 1.0) < 1e-5 for v in f.verts)
    ],
}
with open(out, "w") as fh:
    json.dump(data, fh, indent=1)
print("[ORACLE] wrote", out, "nv=", data["n_verts"], "nf=", data["n_faces"])
bm.free()
