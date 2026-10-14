# Blender oracle: notch solid whose concave edge is SPLIT into two collinear halves
# (what an extrude leaves), beveling ONLY the first half. AZRA silently no-ops on this.
# Usage: blender.exe --background --python partial_run_ref.py -- out.json
import bpy, bmesh, json, sys, mathutils

out = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else "partial_run_ref.json"
bpy.ops.object.select_all(action="SELECT")
bpy.ops.object.delete(use_global=False)

P = [(0,0,0),(0,0,2),(0,2,0),(2,0,0),(1.5,1,1),(2,0,2),(2,2,0),(2,2,2),
     (0,1,2),(0,1,1),(0,2,1),(1.5,2,2),(1.5,2,1),(1.5,1,2),(0.75,1,1)]
F = [[2,0,1,8,9,10],[7,6,2,10,12,11],[6,7,5,3],[3,5,1,0],[2,6,3,0],
     [8,1,5,7,11,13],[12,4,13,11],[4,14,9,8,13],[10,9,14,4,12]]

me = bpy.data.meshes.new("notch")
me.from_pydata([mathutils.Vector(v) for v in P], [], F)
me.validate(); me.update()
ob = bpy.data.objects.new("notch", me)
bpy.context.collection.objects.link(ob)

bm = bmesh.new(); bm.from_mesh(me)
bm.verts.ensure_lookup_table(); bm.edges.ensure_lookup_table()

def key(v): return (round(v.co.x,4), round(v.co.y,4), round(v.co.z,4))
target = [e for e in bm.edges
          if {key(e.verts[0]), key(e.verts[1])} == {(0.0,1.0,1.0), (0.75,1.0,1.0)}]
print("[ORACLE] target edges:", len(target))

res = bmesh.ops.bevel(bm, geom=target, offset=0.3, offset_type="OFFSET", segments=5,
                      profile=0.5, affect="EDGES", clamp_overlap=True, loop_slide=True,
                      miter_outer="SHARP", miter_inner="SHARP")
bm.verts.ensure_lookup_table(); bm.faces.ensure_lookup_table()
new_faces = set(f.index for f in res.get("faces", []))
data = {
 "n_verts": len(bm.verts), "n_faces": len(bm.faces),
 "open_edges": sum(1 for e in bm.edges if len(e.link_faces) != 2),
 "verts": [[round(c,6) for c in v.co] for v in bm.verts],
 "faces": [[v.index for v in f.verts] for f in bm.faces],
 "new_faces": [{"idx": f.index, "n": len(f.verts),
                "verts": [[round(c,6) for c in v.co] for v in f.verts]}
               for f in bm.faces if f.index in new_faces],
}
json.dump(data, open(out,"w"), indent=1)
print("[ORACLE] partial-run nv=", data["n_verts"], "nf=", data["n_faces"],
      "new=", len(new_faces), "open=", data["open_edges"])
bm.free()
