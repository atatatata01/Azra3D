"""Rounded-cube oracle: ±1 cube, bevel ALL 12 edges, segments=2, to study how Blender's M_ADJ places the
corner grid so the edge fillets stay PARALLEL to their edge (the user's 14/16 alignment issue).
Run: blender.exe --background --python rounded_cube_ref.py -- <out.json>
"""
import bpy, bmesh, sys, json
argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "rounded_cube_ref.json"

# ±1 cube (matches AZRA new_box(Vec3::ONE))
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]
bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces: bm.faces.new([bmv[i] for i in f])
bm.normal_update()

bmesh.ops.bevel(bm, geom=list(bm.edges), offset=0.3, offset_type='OFFSET', segments=2,
                profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True,
                miter_outer='SHARP', miter_inner='SHARP')
bm.verts.index_update(); bm.faces.index_update()
out = {"nv": len(bm.verts), "nf": len(bm.faces),
       "verts": [[round(v.co.x,5),round(v.co.y,5),round(v.co.z,5)] for v in bm.verts],
       "faces": [[v.index for v in f.verts] for f in bm.faces]}
json.dump(out, open(out_path,"w"))
print(f"[ORACLE] nv={out['nv']} nf={out['nf']}")
# study the +++ corner region (x>0.5,y>0.5,z>0.5): its verts, and whether edge-fillet verts share a coord
corner=[(i,v) for i,v in enumerate(out['verts']) if v[0]>0.55 and v[1]>0.55 and v[2]>0.55]
print("+++ corner-region verts:")
for i,v in corner: print(f"  {i}: {v}")
