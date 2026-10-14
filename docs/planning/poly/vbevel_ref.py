import bpy, bmesh, sys, json
argv=sys.argv; outdir=argv[argv.index("--")+1] if "--" in argv else "."
def cube_vbevel(seg, off):
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
    verts=[(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
    faces=[(0,1,2,3),(7,6,5,4),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]
    bm=bmesh.new(); bv=[bm.verts.new(v) for v in verts]; bm.verts.index_update()
    for f in faces: bm.faces.new([bv[i] for i in f])
    bm.normal_update()
    corner=[v for v in bm.verts if v.co.x>0.5 and v.co.y>0.5 and v.co.z>0.5]  # vertex (1,1,1)=idx6
    bmesh.ops.bevel(bm, geom=corner, offset=off, offset_type='OFFSET', segments=seg, profile=0.5,
                    affect='VERTICES', clamp_overlap=True)
    bm.verts.index_update(); bm.faces.index_update()
    sizes={}
    for f in bm.faces: sizes[len(f.verts)]=sizes.get(len(f.verts),0)+1
    out={"nv":len(bm.verts),"ne":len(bm.edges),"nf":len(bm.faces),"sizes":sizes,
         "verts":[[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm.verts],
         "faces":[[v.index for v in f.verts] for f in bm.faces]}
    bm.free(); return out
for seg in (1,3):
    o=cube_vbevel(seg, 0.4)
    json.dump(o, open(f"{outdir}/vbevel_cube_seg{seg}.json","w"))
    print(f"[VBEVEL seg{seg}] nv={o['nv']} nf={o['nf']} sizes={o['sizes']}")
    cor=[v for v in o['verts'] if v[0]>0.4 and v[1]>0.4 and v[2]>0.4]
    print(f"  corner-region verts ({len(cor)}):")
    for v in sorted(cor,key=lambda q:-(q[0]+q[1]+q[2]))[:8]: print("   ",v)
