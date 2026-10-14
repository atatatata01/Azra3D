"""Seg=3 rounded-cube oracle to study Blender's M_ADJ converging quad grid at a 3-way corner."""
import bpy, bmesh, sys, json
argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "rc3.json"
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]
bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces: bm.faces.new([bmv[i] for i in f])
bm.normal_update()
bmesh.ops.bevel(bm, geom=list(bm.edges), offset=0.3, offset_type='OFFSET', segments=3,
                profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True,
                miter_outer='SHARP', miter_inner='SHARP')
bm.verts.index_update(); bm.faces.index_update()
out = {"nv": len(bm.verts), "nf": len(bm.faces),
       "verts": [[round(v.co.x,5),round(v.co.y,5),round(v.co.z,5)] for v in bm.verts],
       "faces": [[v.index for v in f.verts] for f in bm.faces]}
json.dump(out, open(out_path,"w"))
print(f"[ORACLE] nv={out['nv']} nf={out['nf']}")
