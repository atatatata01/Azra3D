import bpy, bmesh, sys, json
argv = sys.argv
outdir = argv[argv.index("--") + 1] if "--" in argv else "."

def dump(bm, path, label):
    bm.verts.index_update(); bm.faces.index_update()
    out = {"nv": len(bm.verts), "nf": len(bm.faces),
           "verts": [[round(v.co.x,5),round(v.co.y,5),round(v.co.z,5)] for v in bm.verts],
           "faces": [[v.index for v in f.verts] for f in bm.faces]}
    json.dump(out, open(path, "w"))
    print(f"[ORACLE {label}] nv={out['nv']} nf={out['nf']}")
    return out

def octa():
    bm = bmesh.new()
    p = [(1,0,0),(-1,0,0),(0,1,0),(0,-1,0),(0,0,1),(0,0,-1)]
    vs = [bm.verts.new(c) for c in p]
    bm.verts.index_update()
    faces = [(0,2,4),(2,1,4),(1,3,4),(3,0,4),(2,0,5),(1,2,5),(3,1,5),(0,3,5)]
    for f in faces: bm.faces.new([vs[i] for i in f])
    bm.normal_update()
    return bm, vs

# (a) 2 ADJACENT beveled edges at vertex 4 (0,0,1): edges (4,0) and (4,2) — share face (0,2,4). k=2 < degree=4.
bm, vs = octa()
sel = [e for e in bm.edges if set(v.index for v in e.verts) in ({0,4},{2,4},{1,4})]
assert len(sel) == 3
bmesh.ops.bevel(bm, geom=sel, offset=0.2, offset_type='OFFSET', segments=3, profile=0.5,
                affect='EDGES', clamp_overlap=True, loop_slide=True, miter_outer='SHARP', miter_inner='SHARP')
d = dump(bm, f"{outdir}/mixed_k3d4_ref.json", "k3_deg4")
apx = [(i,v) for i,v in enumerate(d['verts']) if v[2] > 0.55]
print("verts near apex (z>0.55):", len(apx))
for i,v in apx: print("  ", i, v)
bm.free()

