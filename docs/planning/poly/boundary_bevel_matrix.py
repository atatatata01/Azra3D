import bpy, bmesh, json, sys

def clear():
    bpy.ops.object.select_all(action='SELECT'); bpy.ops.object.delete()
def build(verts, faces):
    mesh = bpy.data.meshes.new("m"); mesh.from_pydata(verts, [], faces); mesh.update()
    obj = bpy.data.objects.new("o", mesh); bpy.context.collection.objects.link(obj); return obj

def do(tag, verts, faces, sel_pairs, offset_type):
    clear(); obj = build(verts, faces)
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.mode_set(mode='EDIT')
    bm = bmesh.from_edit_mesh(obj.data); bm.edges.ensure_lookup_table()
    for e in bm.edges: e.select=False
    for v in bm.verts: v.select=False
    for f in bm.faces: f.select=False
    sset = [set(p) for p in sel_pairs]
    nsel=0
    for e in bm.edges:
        vs = set(v.index for v in e.verts)
        if vs in sset:
            e.select=True; nsel+=1
    bmesh.update_edit_mesh(obj.data)
    nv0 = len(obj.data.vertices)
    try:
        res = bpy.ops.mesh.bevel(offset=0.2, segments=1, affect='EDGES',
                offset_type=offset_type, profile=0.5, clamp_overlap=False, loop_slide=True)
    except Exception as ex:
        res=[f"EXC:{ex}"]
    bpy.ops.object.mode_set(mode='OBJECT')
    nv1 = len(obj.data.vertices); nf1=len(obj.data.polygons)
    changed = nv1 != nv0
    print(f"{tag:40s} sel={nsel} type={offset_type:8s} -> nv {nv0}->{nv1} nf={nf1} CHANGED={changed} op={list(res)}")

# Open box, top removed
bv = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
bf = [(0,1,2,3),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]
for ot in ['OFFSET','WIDTH','DEPTH','PERCENT']:
    do("openbox_rim_edge_45", bv, bf, [(4,5)], ot)
do("openbox_rim_loop", bv, bf, [(4,5),(5,6),(6,7),(7,4)], 'OFFSET')

# Flat quad
qv=[(0,0,0),(1,0,0),(1,1,0),(0,1,0)]; qf=[(0,1,2,3)]
do("flatquad_edge_01", qv, qf, [(0,1)], 'OFFSET')

# Disc-like: a hexagon fan (center + 6) with ONE outer boundary edge beveled.
import math
hv=[(0,0,0)]
for i in range(6):
    a=i*math.pi/3; hv.append((math.cos(a),math.sin(a),0))
hf=[(0,i+1,(i+1)%6+1) for i in range(6)]
do("hexfan_outer_edge", hv, hf, [(1,2)], 'OFFSET')

# Interior edge on open box (control: should CHANGE)
do("openbox_INTERIOR_edge_04", bv, bf, [(0,4)], 'OFFSET')
