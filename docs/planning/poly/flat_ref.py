import bpy, bmesh, sys, json
argv=sys.argv; outdir=argv[argv.index("--")+1] if "--" in argv else "."
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
p=[(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1),(0,1,-1),(0,1,1)]
faces=[(0,1,2,3),(7,9,8,4),(9,6,5,8),(4,8,5,1,0),(6,9,7,3,2),(7,4,0,3),(5,6,2,1)]
bm=bmesh.new(); bv=[bm.verts.new(v) for v in p]; bm.verts.index_update()
for f in faces: bm.faces.new([bv[i] for i in f])
bm.normal_update()
# bevel the flat edge 8-9
e=[e for e in bm.edges if {e.verts[0].index,e.verts[1].index}=={8,9}]
assert len(e)==1
bmesh.ops.bevel(bm, geom=e, offset=0.3, offset_type='OFFSET', segments=3, profile=0.5, affect='EDGES', clamp_overlap=True, loop_slide=True)
bm.verts.index_update(); bm.faces.index_update()
sizes={}
for f in bm.faces: sizes[len(f.verts)]=sizes.get(len(f.verts),0)+1
out={"nv":len(bm.verts),"nf":len(bm.faces),"sizes":sizes,
     "verts":[[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts]}
json.dump(out, open(f"{outdir}/flat_edge_ref.json","w"))
print(f"[FLAT blender] nv={out['nv']} nf={out['nf']} sizes={sizes}")
# strip verts (x in (-0.4,0.4), y~1)
strip=[v for v in out['verts'] if abs(v[0])<0.45 and v[1]>0.9]
print("strip verts:", len(strip))
for v in sorted(strip): print("  ",v)
