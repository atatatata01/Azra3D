import bpy,bmesh,sys,json
sys.path.insert(0,sys.argv[-1])
from miter_inner_ref import build_base, rim_edges, fresh, W, SPREAD
from mathutils import Vector
res={}
for seg in (1,2,3,4,5,6):
    fresh(); obj=build_base()
    bm=bmesh.new(); bm.from_mesh(obj.data); bm.normal_update()
    bmesh.ops.bevel(bm,geom=rim_edges(bm),offset=W,segments=seg,profile=0.5,
        affect='EDGES',offset_type='OFFSET',clamp_overlap=True,miter_inner='ARC',spread=SPREAD)
    bm.normal_update()
    # arc verts of the +X/+Y corner: z == 1+W, near (0.6,0.6)
    arc=[v for v in bm.verts if abs(v.co.z-(1.0+W))<1e-4 and v.co.x>0.35 and v.co.y>0.35]
    # order them along the sweep: by angle around C=(0.5,0.5)
    import math
    arc.sort(key=lambda v: math.atan2(v.co.y-0.5, v.co.x-0.5))
    # the wall faces = faces containing a boss-top vert (z==2.0)
    walls=[f for f in bm.faces if any(abs(v.co.z-2.0)<1e-5 for v in f.verts)
           and any(v in arc for v in f.verts)]
    shared=[i for i,v in enumerate(arc) if sum(1 for f in walls if v in f.verts)>=2]
    res[seg]={"n_arc":len(arc),"shared_idx":shared,
              "arc":[[round(c,5) for c in v.co] for v in arc]}
    bm.free()
print("RESULT "+json.dumps(res))
