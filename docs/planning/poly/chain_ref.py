"""Oracles for the rounded EDGE-CHAIN / CAP-LOOP bevel (k=2 at degree-3 vertices — Blender's weld case).
(a) ±1 cube, TWO adjacent top edges sharing one degree-3 corner, seg=3, offset 0.3.
(b) 12-gon prism (r=1, h=2), the full top rim loop, seg=3, offset 0.2.
Run: blender.exe --background --python chain_ref.py -- <outdir>
"""
import bpy, bmesh, sys, json, math
argv = sys.argv
outdir = argv[argv.index("--") + 1] if "--" in argv else "."

def dump(bm, path, label):
    bm.verts.index_update(); bm.faces.index_update()
    out = {"nv": len(bm.verts), "nf": len(bm.faces),
           "verts": [[round(v.co.x, 5), round(v.co.y, 5), round(v.co.z, 5)] for v in bm.verts],
           "faces": [[v.index for v in f.verts] for f in bm.faces]}
    json.dump(out, open(path, "w"))
    print(f"[ORACLE {label}] nv={out['nv']} nf={out['nf']}")

# ---------- (a) cube 2-edge chain ----------
verts = [(-1,-1,-1),(1,-1,-1),(1,-1,1),(-1,-1,1),(-1,1,-1),(1,1,-1),(1,1,1),(-1,1,1)]
faces = [(0,1,2,3),(7,6,5,4),(0,4,5,1),(1,5,6,2),(2,6,7,3),(3,7,4,0)]
bm = bmesh.new()
bmv = [bm.verts.new(v) for v in verts]
bm.verts.index_update()
for f in faces: bm.faces.new([bmv[i] for i in f])
bm.normal_update()
# two adjacent TOP edges sharing corner (1,-1,1)->? : edges (2,3) [top, y=-1] is v2=(1,-1,1) v3=(-1,-1,1);
# and (2,6): v2=(1,-1,1) v6=(1,1,1). They share v2, each endpoint degree-3.
sel = [e for e in bm.edges if {e.verts[0].index, e.verts[1].index} in ({2,3}, {2,6})]
assert len(sel) == 2
bmesh.ops.bevel(bm, geom=sel, offset=0.3, offset_type='OFFSET', segments=3, profile=0.5,
                affect='EDGES', clamp_overlap=True, loop_slide=True,
                miter_outer='SHARP', miter_inner='SHARP')
dump(bm, f"{outdir}/chain2_cube_ref.json", "chain2_cube")
bm.free()

# ---------- (b) 12-gon prism cap loop ----------
bm = bmesh.new()
n = 12
top = [bm.verts.new((math.cos(2*math.pi*i/n), math.sin(2*math.pi*i/n), 1.0)) for i in range(n)]
bot = [bm.verts.new((math.cos(2*math.pi*i/n), math.sin(2*math.pi*i/n), -1.0)) for i in range(n)]
bm.verts.index_update()
bm.faces.new(top)
bm.faces.new(list(reversed(bot)))
for i in range(n):
    j = (i+1) % n
    bm.faces.new([top[i], bot[i], bot[j], top[j]])
bm.normal_update()
sel = [e for e in bm.edges if e.verts[0].co.z > 0.5 and e.verts[1].co.z > 0.5]
assert len(sel) == n
bmesh.ops.bevel(bm, geom=sel, offset=0.2, offset_type='OFFSET', segments=3, profile=0.5,
                affect='EDGES', clamp_overlap=True, loop_slide=True,
                miter_outer='SHARP', miter_inner='SHARP')
dump(bm, f"{outdir}/caploop_cyl_ref.json", "caploop_cyl")
bm.free()
