import bpy, bmesh, sys, json, math
argv=sys.argv; outdir=argv[argv.index("--")+1] if "--" in argv else "."
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
bm=bmesh.new()
top=bm.verts.new((0,0,1)); bot=bm.verts.new((0,0,-1))
eq=[bm.verts.new((math.cos(2*math.pi*i/5),math.sin(2*math.pi*i/5),0.0)) for i in range(5)]
bm.verts.index_update()
for i in range(5):
    j=(i+1)%5
    bm.faces.new([top,eq[i],eq[j]]); bm.faces.new([bot,eq[j],eq[i]])
bm.normal_update()
me=bpy.data.meshes.new("m"); bm.to_mesh(me); bm.free()
obj=bpy.data.objects.new("o",me); bpy.context.collection.objects.link(obj); bpy.context.view_layer.objects.active=obj
bm2=bmesh.new(); bm2.from_mesh(me)
sel=[e for e in bm2.edges if 0 in set(v.index for v in e.verts) and any(k in set(v.index for v in e.verts) for k in (2,3,4))]
bmesh.ops.bevel(bm2,geom=sel,offset=0.25,offset_type='OFFSET',segments=3,profile=0.5,affect='EDGES',clamp_overlap=True,loop_slide=True,miter_outer='SHARP',miter_inner='SHARP')
bm2.verts.index_update(); bm2.faces.index_update()
vs=[[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm2.verts]
# apex-region = verts with z>0.5; print faces touching >=2 of them
apx=set(i for i,v in enumerate(vs) if v[2]>0.5)
print("apex verts:", len(apx))
for i in sorted(apx): print(f"  v{i} {vs[i]}")
print("apex faces:")
for f in bm2.faces:
    fi=[v.index for v in f.verts]
    if sum(1 for x in fi if x in apx)>=2:
        print(f"  n{len(fi)}: {fi}")
bm2.free()
