import bpy, bmesh, sys, json, math
argv = sys.argv
outdir = argv[argv.index("--") + 1] if "--" in argv else "."
bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
bm = bmesh.new()
# pentagonal bipyramid: apex top (deg-5), apex bottom, 5 equatorial
top = bm.verts.new((0,0,1)); bot = bm.verts.new((0,0,-1))
eq = [bm.verts.new((math.cos(2*math.pi*i/5), math.sin(2*math.pi*i/5), 0.0)) for i in range(5)]
bm.verts.index_update()
for i in range(5):
    j = (i+1)%5
    bm.faces.new([top, eq[i], eq[j]])
    bm.faces.new([bot, eq[j], eq[i]])
bm.normal_update()
# bevel 3 CONSECUTIVE top-apex edges: top->eq0, top->eq1, top->eq2  (fan pattern T,T,T,F,F)
sel = [e for e in bm.edges if top in e.verts and (eq[0] in e.verts or eq[1] in e.verts or eq[2] in e.verts)]
assert len(sel) == 3, len(sel)
me = bpy.data.meshes.new("m"); bm.to_mesh(me); bm.free()
obj = bpy.data.objects.new("o", me); bpy.context.collection.objects.link(obj)
bpy.context.view_layer.objects.active = obj
bm2 = bmesh.new(); bm2.from_mesh(me)
selset = set()
for e in bm2.edges:
    vs = set(v.index for v in e.verts)
    if 0 in vs and (2 in vs or 3 in vs or 4 in vs):  # top=0, eq = 2,3,4,5,6
        selset.add(e)
bmesh.ops.bevel(bm2, geom=list(selset), offset=0.25, offset_type='OFFSET', segments=3, profile=0.5,
                affect='EDGES', clamp_overlap=True, loop_slide=True, miter_outer='SHARP', miter_inner='SHARP')
bm2.verts.index_update(); bm2.faces.index_update()
sizes={}
for f in bm2.faces: sizes[len(f.verts)]=sizes.get(len(f.verts),0)+1
out={"nv":len(bm2.verts),"ne":len(bm2.edges),"nf":len(bm2.faces),"sizes":sizes,
     "verts":[[round(v.co.x,4),round(v.co.y,4),round(v.co.z,4)] for v in bm2.verts]}
json.dump(out, open(f"{outdir}/consec_ref.json","w"))
print(f"[CONSEC blender] nv={out['nv']} ne={out['ne']} nf={out['nf']} sizes={sizes}")
apx=[v for v in out['verts'] if v[2]>0.55]
print("apex-region verts (z>0.55):", len(apx))
for v in sorted(apx, key=lambda q:-q[2]): print("  ", v)
