# Blender oracle driven by the OWNER'S captured mesh (AZRA_BEVEL_DUMP=1).
# Usage: blender.exe --background --python repro_ref.py -- <repro.json> <out.json> [width] [segments]
import bpy, bmesh, json, sys, mathutils

argv = sys.argv[sys.argv.index("--") + 1:]
src, out = argv[0], argv[1]
WIDTH = float(argv[2]) if len(argv) > 2 else 0.15
SEGMENTS = int(argv[3]) if len(argv) > 3 else 3

rep = json.load(open(src))
P = rep["verts"]
F = rep["faces"]
PAIRS = [tuple(p) for p in rep["selected_edge_vert_pairs"]]

bpy.ops.object.select_all(action="SELECT")
bpy.ops.object.delete(use_global=False)

me = bpy.data.meshes.new("repro")
me.from_pydata([mathutils.Vector(v) for v in P], [], [list(f) for f in F])
me.validate(); me.update()
ob = bpy.data.objects.new("repro", me)
bpy.context.collection.objects.link(ob)

bm = bmesh.new(); bm.from_mesh(me)
bm.verts.ensure_lookup_table(); bm.edges.ensure_lookup_table(); bm.faces.ensure_lookup_table()

want = set(frozenset(p) for p in PAIRS)
target = [e for e in bm.edges if frozenset((e.verts[0].index, e.verts[1].index)) in want]
print("[ORACLE] base nv=", len(bm.verts), "nf=", len(bm.faces), "target=", len(target))
for p in PAIRS:
    for vi in p:
        v = bm.verts[vi]
        print("[ORACLE] endpoint v%d %s degree(faces)=%d degree(edges)=%d"
              % (vi, [round(c, 4) for c in v.co], len(v.link_faces), len(v.link_edges)))

res = bmesh.ops.bevel(bm, geom=target, offset=WIDTH, offset_type="OFFSET",
                      segments=SEGMENTS, profile=0.5, affect="EDGES",
                      clamp_overlap=True, loop_slide=True,
                      miter_outer="SHARP", miter_inner="SHARP")

bm.verts.ensure_lookup_table(); bm.faces.ensure_lookup_table(); bm.normal_update()
new_faces = set(f.index for f in res.get("faces", []))
data = {
 "width": WIDTH, "segments": SEGMENTS,
 "n_verts": len(bm.verts), "n_faces": len(bm.faces),
 "open_edges": sum(1 for e in bm.edges if len(e.link_faces) != 2),
 "verts": [[round(c, 6) for c in v.co] for v in bm.verts],
 "faces": [[v.index for v in f.verts] for f in bm.faces],
 "new_faces": [{"idx": f.index, "n": len(f.verts),
                "verts": [[round(c, 6) for c in v.co] for v in f.verts]}
               for f in bm.faces if f.index in new_faces],
}
json.dump(data, open(out, "w"), indent=1)
print("[ORACLE] result nv=", data["n_verts"], "nf=", data["n_faces"],
      "new=", len(new_faces), "open=", data["open_edges"])
bm.free()
