# Blender oracle: the OWNER'S scenario — a box with a corner NOTCH removed, so the concave
# edge TERMINATES MID-BODY against the notch end wall (not on a flat end face). This is the
# case where Blender builds a terminating patch and AZRA leaves a hole.
#
# Base: box [0,2]^3 minus notch x in [0,1.5], y in [1,2], z in [1,2].
# The concave edge under test runs along X at (y=1, z=1) from x=0 to x=1.5 and ENDS at x=1.5.
#
# Dumps BOTH the base mesh (so AZRA can rebuild it identically) and the beveled result.
# Usage: blender.exe --background --python notch_corner_ref.py -- out.json
import bpy, bmesh, json, sys, mathutils

out = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else "notch_corner_ref.json"

bpy.ops.object.select_all(action="SELECT")
bpy.ops.object.delete(use_global=False)

WIDTH = 0.3
SEGMENTS = 5


def add_box(name, lo, hi):
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=1.0)
    for v in bm.verts:
        v.co.x = lo[0] + (v.co.x + 0.5) * (hi[0] - lo[0])
        v.co.y = lo[1] + (v.co.y + 0.5) * (hi[1] - lo[1])
        v.co.z = lo[2] + (v.co.z + 0.5) * (hi[2] - lo[2])
    me = bpy.data.meshes.new(name)
    bm.to_mesh(me)
    bm.free()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    return ob


base = add_box("base", (0, 0, 0), (2, 2, 2))
cutter = add_box("cutter", (-0.1, 1, 1), (1.5, 2.1, 2.1))

bpy.context.view_layer.objects.active = base
m = base.modifiers.new("bool", "BOOLEAN")
m.operation = "DIFFERENCE"
m.object = cutter
m.solver = "EXACT"
bpy.ops.object.modifier_apply(modifier="bool")
bpy.data.objects.remove(cutter, do_unlink=True)

bm = bmesh.new()
bm.from_mesh(base.data)
bmesh.ops.dissolve_limit(bm, angle_limit=0.001, verts=bm.verts, edges=bm.edges)
bm.verts.ensure_lookup_table()
bm.edges.ensure_lookup_table()
bm.faces.ensure_lookup_table()

base_dump = {
    "verts": [[round(c, 6) for c in v.co] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
}

# The concave edge under test: runs along X at y=1, z=1.
def is_target(e):
    a, b = e.verts[0].co, e.verts[1].co
    return (abs(a.y - 1) < 1e-5 and abs(b.y - 1) < 1e-5
            and abs(a.z - 1) < 1e-5 and abs(b.z - 1) < 1e-5
            and abs(a.x - b.x) > 1e-5)

target = [e for e in bm.edges if is_target(e)]
print("[ORACLE] base nv=", len(bm.verts), "nf=", len(bm.faces), "target edges=", len(target))
for e in target:
    print("[ORACLE] target edge:", [list(v.co) for v in e.verts])

res = bmesh.ops.bevel(
    bm, geom=target, offset=WIDTH, offset_type="OFFSET", segments=SEGMENTS,
    profile=0.5, affect="EDGES", clamp_overlap=True, loop_slide=True,
    miter_outer="SHARP", miter_inner="SHARP",
)

bm.verts.ensure_lookup_table()
bm.faces.ensure_lookup_table()
bm.normal_update()

new_faces = set(f.index for f in res.get("faces", []))
data = {
    "width": WIDTH, "segments": SEGMENTS,
    "base": base_dump,
    "verts": [[round(c, 6) for c in v.co] for v in bm.verts],
    "faces": [[v.index for v in f.verts] for f in bm.faces],
    "n_verts": len(bm.verts), "n_faces": len(bm.faces),
    "new_face_indices": sorted(new_faces),
    "new_faces": [
        {"idx": f.index,
         "n": len(f.verts),
         "verts": [[round(c, 6) for c in v.co] for v in f.verts]}
        for f in bm.faces if f.index in new_faces
    ],
    "open_edges": sum(1 for e in bm.edges if len(e.link_faces) != 2),
}
with open(out, "w") as fh:
    json.dump(data, fh, indent=1)
print("[ORACLE] beveled nv=", data["n_verts"], "nf=", data["n_faces"],
      "new_faces=", len(new_faces), "open_edges=", data["open_edges"])
bm.free()
