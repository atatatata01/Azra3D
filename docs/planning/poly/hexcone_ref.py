import bpy, bmesh, sys, json, math
argv = sys.argv
out_path = argv[argv.index("--") + 1] if "--" in argv else "hexcone.json"
bm = bmesh.new()
n = 6
base = [bm.verts.new((math.cos(2*math.pi*i/n), -1.0, math.sin(2*math.pi*i/n))) for i in range(n)]
apex = bm.verts.new((0.0, 1.0, 0.0))
bm.verts.index_update()
bm.faces.new(list(reversed(base)))
for i in range(n):
    bm.faces.new([apex, base[i], base[(i+1)%n]])
bm.normal_update()
sel = [e for e in bm.edges if apex in e.verts]
assert len(sel) == n
bmesh.ops.bevel(bm, geom=sel, offset=0.25, offset_type='OFFSET', segments=3, profile=0.5,
                affect='EDGES', clamp_overlap=True, loop_slide=True,
                miter_outer='SHARP', miter_inner='SHARP')
bm.verts.index_update(); bm.faces.index_update()
out = {"nv": len(bm.verts), "nf": len(bm.faces),
       "verts": [[round(v.co.x,5),round(v.co.y,5),round(v.co.z,5)] for v in bm.verts],
       "faces": [[v.index for v in f.verts] for f in bm.faces]}
json.dump(out, open(out_path, "w"))
print(f"[ORACLE] nv={out['nv']} nf={out['nf']}")
apx = [v for v in out['verts'] if v[1] > 0.2]
print("apex-region verts:", len(apx))
for v in sorted(apx, key=lambda q: -q[1])[:6]: print("  ", v)
