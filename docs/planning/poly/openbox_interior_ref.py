import bpy, bmesh, json, sys
def clear():
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
def build(verts, faces):
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj); return obj
def do(tag, verts, faces, vpair, seg, out):
    clear(); obj = build(verts, faces)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.mode_set(mode='EDIT')
    bm = bmesh.from_edit_mesh(obj.data); bm.edges.ensure_lookup_table()
    for e in bm.edges: e.select=False
    for v in bm.verts: v.select=False
    for f in bm.faces: f.select=False
    for e in bm.edges:
        if set(v.index for v in e.verts)==set(vpair): e.select=True
    bmesh.update_edit_mesh(obj.data)
    bpy.ops.mesh.bevel(offset=0.2, segments=seg, affect='EDGES', offset_type='OFFSET',
                       profile=0.5, clamp_overlap=False, loop_slide=True)
    bpy.ops.object.mode_set(mode='OBJECT')
    me=obj.data
    vout=[[round(v.co.x,6),round(v.co.y,6),round(v.co.z,6)] for v in me.vertices]
    fout=[[v for v in p.vertices] for p in me.polygons]
    d={"verts":vout,"faces":fout,"nv":len(vout),"nf":len(fout)}
    json.dump(d, open(out,"w"), indent=1)
    print("WROTE",tag,"nv",len(vout),"nf",len(fout))
    print(json.dumps(d))
OUT=sys.argv[-1] if len(sys.argv)>1 else "F:/rust-temp"
bv=[(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
bf=[(0,1,2,3),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]  # top (+Y) open
# interior vertical edge 0-4 (shared by -Z and -X faces), one endpoint (4) on the open rim
do("openbox_interior_04_seg1", bv, bf, (0,4), 1, OUT+"/openbox_int_seg1.json")
do("openbox_interior_04_seg3", bv, bf, (0,4), 3, OUT+"/openbox_int_seg3.json")
# a bottom interior edge fully away from the hole: edge 0-1 (shared by -Y bottom and -Z)
do("openbox_interior_01_seg1", bv, bf, (0,1), 1, OUT+"/openbox_int01_seg1.json")
